// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Support access, the store side (spec A6, "PR 2 decisions"): the grant, its end, the one
//! definition of "active", the audited payload read and titled page, the customer's history.
use printpapi_server::store::*;
use rusqlite::Connection;
use serde_json::{Value, json};

fn db() -> Connection {
    let conn = connect(":memory:").unwrap();
    init_db(&conn).unwrap();
    conn
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn created(conn: &Connection, org: i64) -> Option<f64> {
    org_created_at(conn, org).unwrap()
}

/// An org with one printer; returns (org id, printer id).
fn org_with_printer(conn: &Connection, name: &str) -> (i64, i64) {
    let org = create_org(conn, name).unwrap();
    let reg = register_agent(
        conn,
        "pc",
        &format!("key-{name}"),
        &[PrinterDecl {
            name: "Z".into(),
            can_pdf: true,
            capabilities: None,
        }],
        org,
    )
    .unwrap();
    (org, reg.printer_ids["Z"].as_i64().unwrap())
}

fn job(conn: &Connection, printer: i64, mode: &str, payload: &[u8], title: Option<&str>) -> i64 {
    let pid = json!(printer);
    enqueue_job(
        conn,
        &NewJob {
            printer_id: &pid,
            type_: if mode == "pdf" {
                "pdf_base64"
            } else {
                "raw_base64"
            },
            mode,
            payload,
            user_id: DEFAULT_USER,
            title: title.map(str::to_string),
            copies: 1,
            callback_url: None,
            options: None,
            org_id: None,
            idempotency_key: None,
            expire_after: None,
        },
    )
    .unwrap()
    .job_id
}

fn grant(conn: &Connection, org: i64) -> SupportGrant {
    match operator_grant_support(conn, org, created(conn, org), "ticket 4711", 30).unwrap() {
        Outcome::Done(g) => g,
        other => panic!("grant: {other:?}"),
    }
}

fn audit(conn: &Connection, action: &str) -> Vec<AuditEntry> {
    list_audit(conn, None, Some(action), None, 100).unwrap()
}

fn reads(conn: &Connection, grant_id: i64) -> i64 {
    conn.query_row(
        "SELECT reads FROM support_grants WHERE id=?",
        [grant_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// A grant row written by hand (an old one, an expired one, one of an earlier generation).
fn raw_grant(
    conn: &Connection,
    org: i64,
    org_created: f64,
    expires: f64,
    ended: Option<f64>,
) -> i64 {
    conn.execute(
        "INSERT INTO support_grants(org_id, org_created_at, reason, granted_at, expires_at, ended_at) \
         VALUES(?,?,?,?,?,?)",
        rusqlite::params![org, org_created, "old ticket", expires - 1800.0, expires, ended],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn make_audit_fail(conn: &Connection) {
    conn.execute_batch(
        "CREATE TRIGGER audit_refuses BEFORE INSERT ON operator_audit \
         BEGIN SELECT RAISE(ABORT, 'audit down'); END;",
    )
    .unwrap();
}

#[test]
fn the_schema_has_the_grant_table_and_both_indexes() {
    let conn = db();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(support_grants)")
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        cols,
        [
            "id",
            "org_id",
            "org_created_at",
            "reason",
            "granted_at",
            "expires_at",
            "ended_at",
            "reads"
        ]
    );
    let idx = |name: &str| {
        count(
            &conn,
            &format!("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='{name}'"),
        )
    };
    assert_eq!(idx("support_grants_org"), 1);
    assert_eq!(idx("operator_audit_target"), 1);
}

#[test]
fn granting_checks_the_org_the_generation_and_writes_one_audit_row() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "acme");
    assert_eq!(
        operator_grant_support(&conn, 999, Some(1.0), "ticket 4711", 15).unwrap(),
        Outcome::NotFound
    );
    for seen in [None, Some(created(&conn, org).unwrap() + 1.0)] {
        assert_eq!(
            operator_grant_support(&conn, org, seen, "ticket 4711", 15).unwrap(),
            Outcome::Refused(Refusal::OrgChanged)
        );
    }
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM support_grants"), 0);
    assert!(audit(&conn, "support.grant").is_empty());

    let g = grant(&conn, org);
    assert_eq!(g.reason, "ticket 4711");
    assert_eq!(g.reads, 0);
    assert!((g.expires_at - g.granted_at - 30.0 * 60.0).abs() < 1e-6);
    let rows = audit(&conn, "support.grant");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].target.as_deref(),
        Some(format!("grant:{}", g.id).as_str())
    );
    assert_eq!(rows[0].org_id, Some(org));
    assert_eq!(rows[0].org_created_at, created(&conn, org));
    assert_eq!(rows[0].reason.as_deref(), Some("ticket 4711"));
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail, json!({"minutes": 30}));
    assert_eq!(active_support_grant(&conn, org).unwrap(), Some(g));
}

#[test]
fn a_second_grant_while_one_is_active_is_refused_and_leaves_no_trace() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "acme");
    grant(&conn, org);
    assert_eq!(
        operator_grant_support(&conn, org, created(&conn, org), "ticket 4712", 15).unwrap(),
        Outcome::Refused(Refusal::SupportGrantActive)
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM support_grants"), 1);
    assert_eq!(audit(&conn, "support.grant").len(), 1);
}

#[test]
fn an_expired_grant_that_was_never_ended_neither_blocks_nor_grants() {
    let conn = db();
    let (org, printer) = org_with_printer(&conn, "acme");
    let j = job(&conn, printer, "raw", b"data", Some("t"));
    let old = raw_grant(&conn, org, created(&conn, org).unwrap(), 1.0, None);
    assert_eq!(active_support_grant(&conn, org).unwrap(), None);
    assert_eq!(
        support_read_payload(&conn, j).unwrap(),
        PayloadRead::Refused(Refusal::NoSupportGrant)
    );
    assert_eq!(
        support_org_jobs(&conn, org, None, 10).unwrap(),
        Outcome::Refused(Refusal::NoSupportGrant)
    );
    assert_eq!(
        operator_end_support(&conn, old, None).unwrap(),
        Outcome::Refused(Refusal::GrantNotActive)
    );
    let g = grant(&conn, org);
    assert_ne!(g.id, old);
    assert_eq!(active_support_grant(&conn, org).unwrap().unwrap().id, g.id);
}

#[test]
fn ending_a_grant_by_id_audits_it_and_only_works_once() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "acme");
    assert_eq!(
        operator_end_support(&conn, 77, None).unwrap(),
        Outcome::NotFound
    );
    let g = grant(&conn, org);
    assert_eq!(
        operator_end_support(&conn, g.id, Some("done helping")).unwrap(),
        Outcome::Done(())
    );
    let ended: Option<f64> = conn
        .query_row(
            "SELECT ended_at FROM support_grants WHERE id=?",
            [g.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ended.is_some());
    let rows = audit(&conn, "support.end");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].target.as_deref(),
        Some(format!("grant:{}", g.id).as_str())
    );
    assert_eq!(rows[0].org_id, Some(org));
    assert_eq!(rows[0].org_created_at, created(&conn, org));
    assert_eq!(rows[0].reason.as_deref(), Some("done helping"));
    assert_eq!(active_support_grant(&conn, org).unwrap(), None);
    assert_eq!(
        operator_end_support(&conn, g.id, None).unwrap(),
        Outcome::Refused(Refusal::GrantNotActive)
    );
    assert_eq!(audit(&conn, "support.end").len(), 1);
    // an ended grant frees the org for the next one
    grant(&conn, org);
}

#[test]
fn a_grant_of_an_earlier_org_with_the_same_id_grants_nothing_and_is_not_listed() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "old");
    let old_created = created(&conn, org).unwrap();
    // the org is deleted and an org of the same id comes back later (SQLite reuses the rowid);
    // the grant row of the first one is still there
    raw_grant(&conn, org, old_created, f64::MAX / 2.0, None);
    conn.execute_batch(&format!(
        "DELETE FROM jobs; DELETE FROM printers; DELETE FROM agents; DELETE FROM orgs WHERE id={org};"
    ))
    .unwrap();
    conn.execute(
        "INSERT INTO orgs(id, name, created_at) VALUES(?, 'new', ?)",
        rusqlite::params![org, old_created + 100.0],
    )
    .unwrap();
    let reg = register_agent(
        &conn,
        "pc",
        "key-new",
        &[PrinterDecl {
            name: "Z".into(),
            can_pdf: false,
            capabilities: None,
        }],
        org,
    )
    .unwrap();
    let j = job(
        &conn,
        reg.printer_ids["Z"].as_i64().unwrap(),
        "raw",
        b"x",
        Some("t"),
    );
    assert_eq!(active_support_grant(&conn, org).unwrap(), None);
    assert_eq!(
        support_read_payload(&conn, j).unwrap(),
        PayloadRead::Refused(Refusal::NoSupportGrant)
    );
    assert_eq!(
        support_org_jobs(&conn, org, None, 10).unwrap(),
        Outcome::Refused(Refusal::NoSupportGrant)
    );
    assert!(support_grants_of_org(&conn, org).unwrap().is_empty());
    // and it does not block the new org's own grant
    grant(&conn, org);
    assert_eq!(support_grants_of_org(&conn, org).unwrap().len(), 1);
}

#[test]
fn reading_a_payload_needs_a_grant_of_the_jobs_own_org_and_is_audited() {
    let conn = db();
    let (org, printer) = org_with_printer(&conn, "acme");
    let (other, _) = org_with_printer(&conn, "other");
    let pdf = job(&conn, printer, "pdf", b"%PDF-1.4 hello", Some("invoice"));
    let raw = job(&conn, printer, "raw", b"^XA^XZ", None);

    assert_eq!(
        support_read_payload(&conn, 9999).unwrap(),
        PayloadRead::NotFound
    );
    assert_eq!(
        support_read_payload(&conn, pdf).unwrap(),
        PayloadRead::Refused(Refusal::NoSupportGrant)
    );
    // a grant of another org opens nothing here
    grant(&conn, other);
    assert_eq!(
        support_read_payload(&conn, pdf).unwrap(),
        PayloadRead::Refused(Refusal::NoSupportGrant)
    );
    assert!(audit(&conn, "support.read_job").is_empty());

    let g = grant(&conn, org);
    assert_eq!(
        support_read_payload(&conn, pdf).unwrap(),
        PayloadRead::Bytes {
            bytes: b"%PDF-1.4 hello".to_vec(),
            mode: "pdf".into()
        }
    );
    assert_eq!(
        support_read_payload(&conn, raw).unwrap(),
        PayloadRead::Bytes {
            bytes: b"^XA^XZ".to_vec(),
            mode: "raw".into()
        }
    );
    assert_eq!(reads(&conn, g.id), 2);
    let rows = audit(&conn, "support.read_job");
    assert_eq!(rows.len(), 2);
    // newest first
    assert_eq!(
        rows[1].target.as_deref(),
        Some(format!("grant:{}", g.id).as_str())
    );
    assert_eq!(rows[1].org_id, Some(org));
    assert_eq!(rows[1].org_created_at, created(&conn, org));
    let detail =
        |e: &AuditEntry| -> Value { serde_json::from_str(e.detail.as_deref().unwrap()).unwrap() };
    assert_eq!(detail(&rows[1]), json!({"job_ids": [pdf]}));
    assert_eq!(detail(&rows[0]), json!({"job_ids": [raw]}));
}

#[test]
fn a_payload_the_retention_pass_emptied_is_scrubbed_not_read() {
    let conn = db();
    let (org, printer) = org_with_printer(&conn, "acme");
    let j = job(&conn, printer, "raw", b"secret", Some("t"));
    conn.execute(
        "UPDATE jobs SET state='done', finished_at=?",
        [printpapi_server::now()],
    )
    .unwrap();
    let g = grant(&conn, org);
    purge_jobs(&conn, 1, Some(printpapi_server::now() + 3.0 * 86_400.0)).unwrap();
    assert_eq!(
        support_read_payload(&conn, j).unwrap(),
        PayloadRead::Scrubbed
    );
    assert_eq!(reads(&conn, g.id), 0);
    assert!(audit(&conn, "support.read_job").is_empty());
    // the grant check comes first: without one, a scrubbed job is just refused
    operator_end_support(&conn, g.id, None).unwrap();
    assert_eq!(
        support_read_payload(&conn, j).unwrap(),
        PayloadRead::Refused(Refusal::NoSupportGrant)
    );
}

#[test]
fn the_titled_page_rechecks_the_grant_lists_titles_and_audits_the_job_ids() {
    let conn = db();
    let (org, printer) = org_with_printer(&conn, "acme");
    assert_eq!(
        support_org_jobs(&conn, 999, None, 10).unwrap(),
        Outcome::NotFound
    );
    let a = job(&conn, printer, "raw", b"a", Some("order 1"));
    let b = job(&conn, printer, "raw", b"b", None);
    let c = job(&conn, printer, "raw", b"c", Some("order 3"));
    assert_eq!(
        support_org_jobs(&conn, org, None, 10).unwrap(),
        Outcome::Refused(Refusal::NoSupportGrant)
    );
    let g = grant(&conn, org);

    let Outcome::Done(page) = support_org_jobs(&conn, org, None, 2).unwrap() else {
        panic!("page")
    };
    let seen: Vec<_> = page.iter().map(|(j, t)| (j.id, t.clone())).collect();
    assert_eq!(seen, [(c, Some("order 3".into())), (b, None)]);
    assert_eq!(page[0].0.printer_name.as_deref(), Some("Z"));
    let Outcome::Done(page2) = support_org_jobs(&conn, org, Some(b), 2).unwrap() else {
        panic!("page2")
    };
    assert_eq!(page2.iter().map(|(j, _)| j.id).collect::<Vec<_>>(), [a]);
    assert_eq!(reads(&conn, g.id), 2);
    let rows = audit(&conn, "support.list_jobs");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[1].target.as_deref(),
        Some(format!("grant:{}", g.id).as_str())
    );
    assert_eq!(rows[1].org_id, Some(org));
    let ids: Value = serde_json::from_str(rows[1].detail.as_deref().unwrap()).unwrap();
    assert_eq!(ids, json!({"job_ids": [c, b]}));

    // an empty page: no audit row, but the access still counts
    let Outcome::Done(none) = support_org_jobs(&conn, org, Some(a), 10).unwrap() else {
        panic!("empty")
    };
    assert!(none.is_empty());
    assert_eq!(audit(&conn, "support.list_jobs").len(), 2);
    assert_eq!(reads(&conn, g.id), 3);
}

#[test]
fn a_failing_audit_insert_rolls_everything_back() {
    let conn = db();
    let (org, printer) = org_with_printer(&conn, "acme");
    let j = job(&conn, printer, "raw", b"secret", Some("t"));
    let g = grant(&conn, org);
    make_audit_fail(&conn);

    assert!(support_read_payload(&conn, j).is_err());
    assert!(support_org_jobs(&conn, org, None, 10).is_err());
    assert_eq!(reads(&conn, g.id), 0);
    assert!(operator_end_support(&conn, g.id, None).is_err());
    assert_eq!(active_support_grant(&conn, org).unwrap().unwrap().id, g.id);

    // a grant whose audit row fails is not kept either
    let (org2, _) = org_with_printer_after_trigger(&conn);
    assert!(operator_grant_support(&conn, org2, created(&conn, org2), "ticket 4711", 15).is_err());
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM support_grants"), 1);
    assert_eq!(active_support_grant(&conn, org2).unwrap(), None);
}

fn org_with_printer_after_trigger(conn: &Connection) -> (i64, i64) {
    org_with_printer(conn, "later")
}

#[test]
fn the_customer_sees_the_grants_of_this_generation_newest_first_with_the_job_ids() {
    let conn = db();
    let (org, printer) = org_with_printer(&conn, "acme");
    let (other, _) = org_with_printer(&conn, "other");
    grant(&conn, other);
    let a = job(&conn, printer, "raw", b"a", Some("one"));
    let b = job(&conn, printer, "raw", b"b", Some("two"));
    // a grant of an earlier generation stays out
    raw_grant(&conn, org, created(&conn, org).unwrap() - 500.0, 5.0, None);

    let first = grant(&conn, org);
    support_org_jobs(&conn, org, None, 10).unwrap();
    support_org_jobs(&conn, org, None, 1).unwrap(); // lists b again
    support_read_payload(&conn, b).unwrap();
    support_read_payload(&conn, a).unwrap();
    support_read_payload(&conn, b).unwrap();
    operator_end_support(&conn, first.id, None).unwrap();
    let second = grant(&conn, org);

    let rows = support_grants_of_org(&conn, org).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        [second.id, first.id]
    );
    assert!(rows[0].active && rows[0].ended_at.is_none());
    assert!(rows[0].listed_job_ids.is_empty() && rows[0].opened_job_ids.is_empty());
    assert!(!rows[1].active && rows[1].ended_at.is_some());
    assert_eq!(rows[1].reason, "ticket 4711");
    assert_eq!(rows[1].listed_job_ids, [a, b]);
    assert_eq!(rows[1].opened_job_ids, [a, b]);
    assert_eq!(rows[1].expires_at, first.expires_at);
    assert_eq!(support_grants_of_org(&conn, other).unwrap().len(), 1);
}

#[test]
fn the_customer_list_stops_at_50_and_an_expired_grant_is_not_active() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "acme");
    let c = created(&conn, org).unwrap();
    for i in 0..54 {
        raw_grant(&conn, org, c, 10.0 + i as f64, Some(5.0));
    }
    let rows = support_grants_of_org(&conn, org).unwrap();
    assert_eq!(rows.len(), 50);
    assert!(rows.windows(2).all(|w| w[0].id > w[1].id));
    assert!(rows.iter().all(|r| !r.active));
}

/// A grant that was used never drops out of the customer's view, however many unused ones follow:
/// otherwise 50 empty grants in a row would hide an access from the customer.
#[test]
fn a_used_grant_stays_in_the_customer_list_behind_50_unused_ones() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "acme");
    let c = created(&conn, org).unwrap();
    let used = raw_grant(&conn, org, c, 5.0, Some(4.0));
    conn.execute("UPDATE support_grants SET reads = 1 WHERE id = ?", [used])
        .unwrap();
    for i in 0..60 {
        raw_grant(&conn, org, c, 10.0 + i as f64, Some(5.0));
    }
    let rows = support_grants_of_org(&conn, org).unwrap();
    assert_eq!(
        rows.len(),
        51,
        "the 50 newest unused grants and the used one"
    );
    assert_eq!(rows.last().map(|r| r.id), Some(used));
    assert!(rows.windows(2).all(|w| w[0].id > w[1].id));
}

#[test]
fn deleting_an_org_removes_its_grants_and_keeps_the_audit_rows() {
    let conn = db();
    let (org, _) = org_with_printer(&conn, "acme");
    let (other, _) = org_with_printer(&conn, "other");
    grant(&conn, org);
    grant(&conn, other);
    assert!(delete_org(&conn, org).unwrap());
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM support_grants"), 1);
    assert_eq!(
        count(
            &conn,
            &format!("SELECT COUNT(*) FROM support_grants WHERE org_id={org}")
        ),
        0
    );
    assert_eq!(audit(&conn, "support.grant").len(), 2);
}

#[test]
fn the_customer_view_finds_a_grants_audit_rows_by_index() {
    let conn = db();
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT action, detail FROM operator_audit \
             WHERE target = 'grant:1' AND action IN ('support.list_jobs', 'support.read_job')",
        )
        .unwrap()
        .query_map([], |r| r.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter().any(|l| l.contains("operator_audit_target")),
        "{plan:?}"
    );
}
