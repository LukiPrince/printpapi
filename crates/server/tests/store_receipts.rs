// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Receipts in the store: the printer profile, the copy of a receipt job's document, and the
//! triggers that remove both (the database does the cleanup, so an older binary does it too).
mod common;

use common::*;
use printpapi_server::receipt::{self, Codepage, CutMode, Profile};
use printpapi_server::store::{self, DEFAULT_ORG, Db, NewJob};
use serde_json::{Value, json};

const DOC: &str = r#"{"receipt":{"content":[{"type":"text","text":"Grüße"}]},"profile":{"paper_mm":80,"width_dots":576,"codepage":"raster","codepage_number":16,"cut":"partial","output":"auto"}}"#;

fn new_job<'a>(
    pid: &'a Value,
    payload: &'a [u8],
    org: Option<i64>,
    key: Option<&str>,
) -> NewJob<'a> {
    NewJob {
        printer_id: pid,
        type_: "receipt",
        mode: "raw",
        payload,
        user_id: store::DEFAULT_USER,
        title: None,
        copies: 1,
        callback_url: None,
        options: None,
        org_id: org,
        idempotency_key: key.map(str::to_string),
        expire_after: None,
        api_key_id: None,
    }
}

fn enqueue_receipt(
    db: &Db,
    printer: i64,
    org: Option<i64>,
    key: Option<&str>,
    doc: Option<&str>,
) -> store::Enqueued {
    let pid = json!(printer);
    db.with(|c| store::enqueue_receipt_job(c, &new_job(&pid, b"\x1b@bytes", org, key), true, doc))
        .unwrap()
}

fn copies(db: &Db) -> i64 {
    db.with(|c| c.query_row("SELECT COUNT(*) FROM job_receipts", [], |r| r.get(0)))
        .unwrap()
}

fn has_copy(db: &Db, job: i64) -> bool {
    db.with(|c| {
        c.query_row(
            "SELECT COUNT(*) FROM job_receipts WHERE job_id=?",
            [job],
            |r| r.get::<_, i64>(0),
        )
    })
    .unwrap()
        == 1
}

fn profile(v: Value) -> Profile {
    receipt::parse_profile(v.as_object().unwrap()).unwrap()
}

// --- 26 profiles --------------------------------------------------------------------------------

#[test]
fn a_profile_is_created_replaced_and_deleted_per_printer() {
    let db = mem();
    let reg = register(
        &db,
        "pc",
        "ak",
        &[decl("A", false), decl("B", false)],
        DEFAULT_ORG,
    );
    let (a, b) = (reg.pid("A"), reg.pid("B"));
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, None)).unwrap(),
        None
    );
    let p58 = profile(json!({"paper_mm": 58, "codepage": "cp1252"}));
    assert!(
        db.with(|c| store::put_receipt_profile(c, a, None, &p58))
            .unwrap()
    );
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, None)).unwrap(),
        Some(p58.clone())
    );
    // the other printer is untouched
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, b, None)).unwrap(),
        None
    );
    // replace: the whole profile, no merge
    let full = profile(json!({"cut": "none"}));
    assert!(
        db.with(|c| store::put_receipt_profile(c, a, None, &full))
            .unwrap()
    );
    let got = db
        .with(|c| store::get_receipt_profile(c, a, None))
        .unwrap()
        .unwrap();
    assert_eq!(
        (got.paper_mm, got.cut, got.codepage),
        (80, CutMode::None, Codepage::Raster)
    );
    assert_eq!(
        db.with(
            |c| c.query_row("SELECT COUNT(*) FROM receipt_profiles", [], |r| r
                .get::<_, i64>(0))
        )
        .unwrap(),
        1
    );
    // delete: true for a printer without one too, false for an unknown printer
    assert!(
        db.with(|c| store::delete_receipt_profile(c, a, None))
            .unwrap()
    );
    assert!(
        db.with(|c| store::delete_receipt_profile(c, a, None))
            .unwrap()
    );
    assert!(
        !db.with(|c| store::delete_receipt_profile(c, 999, None))
            .unwrap()
    );
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, None)).unwrap(),
        None
    );
    assert!(
        !db.with(|c| store::put_receipt_profile(c, 999, None, &p58))
            .unwrap()
    );
}

#[test]
fn a_foreign_org_sees_no_profile() {
    let db = mem();
    let other = create_org(&db, "acme");
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let a = reg.pid("A");
    let p = profile(json!({"paper_mm": 58}));
    assert!(
        db.with(|c| store::put_receipt_profile(c, a, Some(DEFAULT_ORG), &p))
            .unwrap()
    );
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, Some(other)))
            .unwrap(),
        None
    );
    assert!(
        !db.with(|c| store::put_receipt_profile(c, a, Some(other), &Profile::default()))
            .unwrap()
    );
    assert!(
        !db.with(|c| store::delete_receipt_profile(c, a, Some(other)))
            .unwrap()
    );
    // still there for its owner and for root
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, Some(DEFAULT_ORG)))
            .unwrap(),
        Some(p.clone())
    );
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, None)).unwrap(),
        Some(p)
    );
}

#[test]
fn a_profile_goes_with_its_printer_and_survives_a_re_register() {
    let db = mem();
    let reg = register(
        &db,
        "pc",
        "ak",
        &[decl("A", false), decl("B", true)],
        DEFAULT_ORG,
    );
    let (a, b) = (reg.pid("A"), reg.pid("B"));
    let p = profile(json!({"paper_mm": 58}));
    for id in [a, b] {
        assert!(
            db.with(|c| store::put_receipt_profile(c, id, None, &p))
                .unwrap()
        );
    }
    // the agent comes back with capabilities changed: the same printer rows, the profiles stay
    let again = register(
        &db,
        "pc",
        "ak",
        &[decl("A", true), decl("B", false)],
        DEFAULT_ORG,
    );
    assert_eq!((again.pid("A"), again.pid("B")), (a, b));
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, None)).unwrap(),
        Some(p.clone())
    );
    // the printer row is deleted (with its org): the trigger takes the profile
    db.with(|c| c.execute("DELETE FROM printers WHERE id=?", [a]))
        .unwrap();
    assert_eq!(
        db.with(|c| store::get_receipt_profile(c, a, None)).unwrap(),
        None
    );
    let n: i64 = db
        .with(|c| c.query_row("SELECT COUNT(*) FROM receipt_profiles", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 1);
}

// --- 27 documents -------------------------------------------------------------------------------

#[test]
fn the_job_and_its_document_are_written_together() {
    let db = mem();
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let a = reg.pid("A");
    let e = enqueue_receipt(&db, a, None, None, Some(DOC));
    assert!(e.created);
    assert!(has_copy(&db, e.job_id));
    let (doc, prof) = db
        .with(|c| store::get_job_receipt(c, e.job_id, None))
        .unwrap()
        .unwrap();
    assert_eq!(doc.content.len(), 1);
    assert_eq!(prof, Profile::default());
    // no document: no row
    let n = enqueue_receipt(&db, a, None, None, None);
    assert!(n.created && !has_copy(&db, n.job_id));
    assert!(
        db.with(|c| store::get_job_receipt(c, n.job_id, None))
            .unwrap()
            .is_none()
    );
    // the ordinary enqueue writes none either
    let plain = enqueue(&db, a, b"x", |_| {});
    assert!(!has_copy(&db, plain));
}

#[test]
fn a_resubmit_adds_no_second_document() {
    let db = mem();
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let a = reg.pid("A");
    let first = enqueue_receipt(&db, a, None, Some("bon-1"), Some(DOC));
    let again = enqueue_receipt(
        &db,
        a,
        None,
        Some("bon-1"),
        Some(&DOC.replace("Grüße", "Other")),
    );
    assert_eq!((again.job_id, again.created), (first.job_id, false));
    assert_eq!(copies(&db), 1);
    let (doc, _) = db
        .with(|c| store::get_job_receipt(c, first.job_id, None))
        .unwrap()
        .unwrap();
    match &doc.content[0] {
        receipt::Element::Text { text, .. } => assert_eq!(text, "Grüße"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refused_job_leaves_no_document() {
    let db = mem();
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let pid = json!(reg.pid("A") + 100);
    let r = db
        .with(|c| store::enqueue_receipt_job(c, &new_job(&pid, b"x", None, None), true, Some(DOC)));
    assert!(r.is_err());
    assert_eq!(copies(&db), 0);
}

#[test]
fn a_document_is_read_through_its_job() {
    let db = mem();
    let other = create_org(&db, "acme");
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let e = enqueue_receipt(&db, reg.pid("A"), Some(DEFAULT_ORG), None, Some(DOC));
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id, Some(DEFAULT_ORG)))
            .unwrap()
            .is_some()
    );
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id, Some(other)))
            .unwrap()
            .is_none()
    );
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id, None))
            .unwrap()
            .is_some()
    );
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id + 1, None))
            .unwrap()
            .is_none()
    );
}

// --- 28 triggers --------------------------------------------------------------------------------

/// Five receipt jobs: the first three are finished (the 1st will be scrubbed, the 2nd deleted by
/// retention, the 3rd stays fresh), the 4th is claimed, the 5th still queued.
fn five_jobs(db: &Db) -> (Registered, [i64; 5]) {
    let reg = register(db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let a = reg.pid("A");
    let ids: Vec<i64> = (0..5)
        .map(|_| enqueue_receipt(db, a, None, None, Some(DOC)).job_id)
        .collect();
    stamp_in_order(db, &ids);
    let [scrub, delete, finished, claimed, queued] = [ids[0], ids[1], ids[2], ids[3], ids[4]];
    // the first two and the last are finished; the claimed one stays claimed
    let c1 = claim(db, reg.computer_id).unwrap().job_id;
    finish(db, c1, reg.computer_id, true, None);
    let c2 = claim(db, reg.computer_id).unwrap().job_id;
    finish(db, c2, reg.computer_id, true, None);
    let c3 = claim(db, reg.computer_id).unwrap().job_id;
    finish(db, c3, reg.computer_id, true, None);
    let c4 = claim(db, reg.computer_id).unwrap().job_id;
    assert_eq!([c1, c2, c3, c4], [scrub, delete, finished, claimed]);
    (reg, [scrub, delete, finished, claimed, queued])
}

/// The claim order is `created_at, id`, the wall clock: with the clock stepped back between two
/// enqueues the younger row is claimed first. That is the product's order and stays; a fixture
/// that needs insert order sets `created_at` itself (`stamp_in_order`).
#[test]
fn claim_follows_created_at_and_stamp_in_order_makes_a_fixture_independent_of_the_clock() {
    let db = mem();
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let a = reg.pid("A");
    let first = enqueue_receipt(&db, a, None, None, Some(DOC)).job_id;
    let second = enqueue_receipt(&db, a, None, None, Some(DOC)).job_id;
    // the clock stepped back between the two enqueues
    db.with(|c| {
        c.execute(
            "UPDATE jobs SET created_at=created_at+100 WHERE id=?",
            [first],
        )
        .unwrap();
    });
    assert_eq!(claim(&db, reg.computer_id).unwrap().job_id, second);
    finish(&db, second, reg.computer_id, true, None);
    // the fixture sets the order itself, whatever the clock did
    stamp_in_order(&db, &[first, second]);
    let created = |id: i64| -> f64 {
        db.with(|c| c.query_row("SELECT created_at FROM jobs WHERE id=?", [id], |r| r.get(0)))
            .unwrap()
    };
    assert!(created(first) < created(second));
}

#[test]
fn the_v3_6_0_purge_sql_removes_the_copies_through_the_triggers() {
    let db = mem();
    let (_, [scrub, delete, finished_fresh, claimed, queued]) = five_jobs(&db);
    let now = printpapi_server::now();
    db.with(|c| {
        // ages: scrub finished 3 days ago, delete finished 40 days ago in an earlier month
        c.execute(
            "UPDATE jobs SET finished_at=? WHERE id=?",
            rusqlite::params![now - 3.0 * 86400.0, scrub],
        )
        .unwrap();
        c.execute(
            "UPDATE jobs SET finished_at=?, created_at=? WHERE id=?",
            rusqlite::params![now - 40.0 * 86400.0, now - 60.0 * 86400.0, delete],
        )
        .unwrap();
        // the queued one has waited three days with no expiry: retention gives up on it
        c.execute(
            "UPDATE jobs SET created_at=? WHERE id=?",
            rusqlite::params![now - 3.0 * 86400.0, queued],
        )
        .unwrap();
    });
    // a state-only update keeps the copy
    assert!(has_copy(&db, scrub) && has_copy(&db, delete) && has_copy(&db, queued));
    // the literal statements of purge_jobs in v3.6.0, all three
    let (cutoff, delete_cutoff, month) = (now - 86400.0, now - 7.0 * 86400.0, now - 30.0 * 86400.0);
    db.with(|c| {
        let abandoned = c.execute(
            "UPDATE jobs SET state='failed', error='retention', finished_at=:now, payload=x'', title=NULL          WHERE state='queued' AND expires_at IS NULL AND created_at < :cutoff",
            rusqlite::named_params! {":now": now, ":cutoff": cutoff},
        ).unwrap();
        assert_eq!(abandoned, 1);
        assert!(!has_copy_in(c, queued));
        let scrubbed = c.execute(
            "UPDATE jobs SET payload=x'', title=NULL, callback_url=NULL          WHERE state IN ('done','failed','cancelled') AND COALESCE(finished_at, created_at) < :cutoff          AND (length(payload) > 0 OR title IS NOT NULL OR callback_url IS NOT NULL)",
            rusqlite::named_params! {":cutoff": cutoff},
        ).unwrap();
        assert_eq!(scrubbed, 2);
        // the rows count are the jobs', not the trigger's
        assert!(!has_copy_in(c, scrub) && !has_copy_in(c, delete));
        let deleted = c.execute(
            "DELETE FROM jobs          WHERE state IN ('done','failed','cancelled') AND COALESCE(finished_at, created_at) < :cutoff          AND created_at < :month",
            rusqlite::named_params! {":cutoff": delete_cutoff, ":month": month},
        ).unwrap();
        assert_eq!(deleted, 1);
    });
    for keep in [finished_fresh, claimed] {
        assert!(has_copy(&db, keep), "{keep}");
    }
    assert_eq!(copies(&db), 2);
}

fn has_copy_in(c: &rusqlite::Connection, job: i64) -> bool {
    c.query_row(
        "SELECT COUNT(*) FROM job_receipts WHERE job_id=?",
        [job],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

#[test]
fn purge_jobs_keeps_its_numbers() {
    let db = mem();
    let (_, [scrub, delete, _, _, queued]) = five_jobs(&db);
    let now = printpapi_server::now();
    db.with(|c| {
        c.execute(
            "UPDATE jobs SET finished_at=? WHERE id=?",
            rusqlite::params![now - 3.0 * 86400.0, scrub],
        )
        .unwrap();
        c.execute(
            "UPDATE jobs SET finished_at=?, created_at=? WHERE id=?",
            rusqlite::params![now - 40.0 * 86400.0, now - 60.0 * 86400.0, delete],
        )
        .unwrap();
        // the third finished job is fresh and stays; the queued one gave up waiting
        c.execute(
            "UPDATE jobs SET created_at=? WHERE id=?",
            rusqlite::params![now - 3.0 * 86400.0, queued],
        )
        .unwrap();
    });
    let purged = db.with(|c| store::purge_jobs(c, 1, Some(now))).unwrap();
    // the scrub pass runs first and also clears the row the delete pass then removes
    assert_eq!(
        (purged.abandoned, purged.scrubbed, purged.deleted),
        (1, 2, 1)
    );
    assert!(!has_copy(&db, scrub) && !has_copy(&db, delete) && !has_copy(&db, queued));
    assert_eq!(copies(&db), 2);
    // the scrubbed job's row is still there, empty, and its document is not served
    assert!(
        db.with(|c| store::get_job_receipt(c, scrub, None))
            .unwrap()
            .is_none()
    );
}

#[test]
fn deleting_an_org_removes_both_tables_rows() {
    let db = mem();
    let org = create_org(&db, "acme");
    let reg = register(&db, "pc", "ak2", &[decl("A", false)], org);
    let a = reg.pid("A");
    assert!(
        db.with(|c| store::put_receipt_profile(c, a, Some(org), &Profile::default()))
            .unwrap()
    );
    let e = enqueue_receipt(&db, a, Some(org), None, Some(DOC));
    let keep = register(&db, "pc2", "ak3", &[decl("K", false)], DEFAULT_ORG);
    let k = enqueue_receipt(&db, keep.pid("K"), None, None, Some(DOC));
    assert!(db.with(|c| store::delete_org(c, org)).unwrap());
    assert!(!has_copy(&db, e.job_id));
    assert!(has_copy(&db, k.job_id));
    let n: i64 = db
        .with(|c| c.query_row("SELECT COUNT(*) FROM receipt_profiles", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 0);
}

/// The statements of `delete_org_tx` in v3.6.0, which knows nothing of the receipt tables: the
/// triggers alone must take the document copies and the profiles with the org.
#[test]
fn the_v3_6_0_delete_org_statements_remove_both_tables_rows() {
    let db = mem();
    let org = create_org(&db, "acme");
    let reg = register(&db, "pc", "ak2", &[decl("A", false)], org);
    let a = reg.pid("A");
    assert!(
        db.with(|c| store::put_receipt_profile(c, a, Some(org), &Profile::default()))
            .unwrap()
    );
    let e = enqueue_receipt(&db, a, Some(org), None, Some(DOC));
    let keep = register(&db, "pc2", "ak3", &[decl("K", false)], DEFAULT_ORG);
    let k = enqueue_receipt(&db, keep.pid("K"), None, None, Some(DOC));
    db.with(|c| {
        let users = "SELECT id FROM users WHERE org_id=?";
        c.execute(
            &format!("DELETE FROM sessions WHERE user_id IN ({users})"),
            [org],
        )
        .unwrap();
        c.execute(
            &format!("DELETE FROM password_resets WHERE user_id IN ({users})"),
            [org],
        )
        .unwrap();
        for table in [
            "jobs",
            "printers",
            "agents",
            "api_keys",
            "users",
            "label_templates",
            "terms_acceptances",
            "org_locks",
            "support_grants",
        ] {
            c.execute(&format!("DELETE FROM {table} WHERE org_id=?"), [org])
                .unwrap();
        }
        c.execute("DELETE FROM orgs WHERE id=?", [org]).unwrap();
    });
    assert!(!has_copy(&db, e.job_id));
    assert!(has_copy(&db, k.job_id));
    let n: i64 = db
        .with(|c| c.query_row("SELECT COUNT(*) FROM receipt_profiles", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 0);
}

// --- 29 orphans ---------------------------------------------------------------------------------

#[test]
fn an_orphan_or_emptied_row_is_never_returned() {
    let db = mem();
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    // a row whose job never existed (an older binary without triggers could leave one)
    db.with(|c| {
        c.execute(
            "INSERT INTO job_receipts(job_id, doc) VALUES(9999, ?)",
            [DOC],
        )
    })
    .unwrap();
    assert!(
        db.with(|c| store::get_job_receipt(c, 9999, None))
            .unwrap()
            .is_none()
    );
    // a row whose job payload is empty, written behind the trigger's back
    let e = enqueue_receipt(&db, reg.pid("A"), None, None, Some(DOC));
    db.with(|c| {
        c.execute_batch("DROP TRIGGER job_receipts_scrub").unwrap();
        c.execute("UPDATE jobs SET payload=x'' WHERE id=?", [e.job_id])
            .unwrap();
    });
    assert!(has_copy(&db, e.job_id));
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id, None))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_reused_org_id_reads_no_foreign_document() {
    let db = mem();
    let org = create_org(&db, "gone");
    let reg = register(&db, "pc", "ak2", &[decl("A", false)], org);
    let e = enqueue_receipt(&db, reg.pid("A"), Some(org), None, Some(DOC));
    // an older binary deletes the org without the triggers: the copy stays behind
    db.with(|c| {
        c.execute_batch("DROP TRIGGER job_receipts_gone").unwrap();
        store::delete_org(c, org).unwrap();
    });
    assert!(has_copy(&db, e.job_id));
    // the id is free again (orgs.id is not AUTOINCREMENT) and the new org owns a job of its own
    let again = create_org(&db, "new");
    assert_eq!(again, org);
    let reg2 = register(&db, "pc3", "ak4", &[decl("B", false)], again);
    enqueue_receipt(&db, reg2.pid("B"), Some(again), None, Some(DOC));
    // the leftover copy has no job, so no org can read it
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id, Some(again)))
            .unwrap()
            .is_none()
    );
    assert!(
        db.with(|c| store::get_job_receipt(c, e.job_id, None))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_document_that_no_longer_parses_is_a_database_error() {
    let db = mem();
    let reg = register(&db, "pc", "ak", &[decl("A", false)], DEFAULT_ORG);
    let e = enqueue_receipt(
        &db,
        reg.pid("A"),
        None,
        None,
        Some(r#"{"receipt":{"content":[{"type":"zap"}]},"profile":{}}"#),
    );
    let r = db.with(|c| store::get_job_receipt(c, e.job_id, None));
    assert!(matches!(r, Err(store::StoreError::Db(_))), "{r:?}");
}
