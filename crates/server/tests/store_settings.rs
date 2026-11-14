// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Live settings, the store side (spec A5): the override rows and the audited write.
use printpapi_server::settings::{Change, Parsed};
use printpapi_server::store::*;
use rusqlite::Connection;
use serde_json::{Value, json};

fn db() -> Connection {
    let conn = connect(":memory:").unwrap();
    init_db(&conn).unwrap();
    conn
}

fn rows(conn: &Connection) -> Vec<(String, String)> {
    settings_rows(conn).unwrap()
}

fn set(conn: &Connection, p: Vec<Parsed>, unset: Vec<&'static str>, reason: Option<&str>) {
    operator_update_settings(conn, &Change { set: p, unset }, reason).unwrap();
}

fn audit(conn: &Connection) -> Vec<AuditEntry> {
    list_audit(conn, None, Some("settings.update"), None, 100).unwrap()
}

#[test]
fn a_fresh_database_has_the_table_and_no_rows() {
    let conn = db();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(settings)")
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(cols, ["key", "value", "updated_at"]);
    assert!(rows(&conn).is_empty());
}

#[test]
fn a_write_stores_the_rows_and_one_audit_row() {
    let conn = db();
    set(
        &conn,
        vec![Parsed::Signup(true), Parsed::SignupQuota(25)],
        vec![],
        Some("opening the beta"),
    );
    assert_eq!(
        rows(&conn),
        [
            ("signup".to_string(), "open".to_string()),
            ("signup_quota".to_string(), "25".to_string())
        ]
    );
    let a = audit(&conn);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].actor, "operator");
    assert_eq!(a[0].reason.as_deref(), Some("opening the beta"));
    assert_eq!(a[0].org_id, None);
    let detail: Value = serde_json::from_str(a[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(
        detail,
        json!({"set": {"signup": "open", "signup_quota": 25}, "unset": []})
    );
    let at: f64 = conn
        .query_row(
            "SELECT updated_at FROM settings WHERE key='signup'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(at > 1.0e9, "updated_at is a wall-clock time: {at}");
}

#[test]
fn a_second_write_replaces_the_value_and_unset_removes_the_row() {
    let conn = db();
    set(&conn, vec![Parsed::SignupQuota(25)], vec![], None);
    set(&conn, vec![Parsed::SignupQuota(5)], vec![], None);
    assert_eq!(rows(&conn), [("signup_quota".to_string(), "5".to_string())]);
    set(&conn, vec![], vec!["signup_quota"], None);
    assert!(rows(&conn).is_empty());
    // unset of a key that has no row is fine, and still audited
    set(&conn, vec![], vec!["terms"], None);
    let a = audit(&conn);
    assert_eq!(a.len(), 4);
    let detail: Value = serde_json::from_str(a[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail, json!({"set": {}, "unset": ["terms"]}));
    assert_eq!(a[0].reason, None);
}

#[test]
fn set_and_unset_in_one_change_share_one_audit_row() {
    let conn = db();
    set(&conn, vec![Parsed::SignupQuota(25)], vec![], None);
    set(
        &conn,
        vec![Parsed::JobRetentionDays(30)],
        vec!["signup_quota"],
        Some("tightening"),
    );
    assert_eq!(
        rows(&conn),
        [("job_retention_days".to_string(), "30".to_string())]
    );
    assert_eq!(audit(&conn).len(), 2);
}

#[test]
fn only_the_four_live_keys_are_read() {
    let conn = db();
    conn.execute(
        "INSERT INTO settings(key, value, updated_at) VALUES('fetches_per_org', '99', 1.0)",
        [],
    )
    .unwrap();
    assert!(rows(&conn).is_empty());
    set(&conn, vec![Parsed::SignupQuota(1)], vec![], None);
    assert_eq!(rows(&conn).len(), 1);
}

#[test]
fn terms_are_stored_as_json_text() {
    let conn = db();
    let t = printpapi_server::server::Terms {
        version: "v2".into(),
        terms_url: "https://x.test/t".into(),
        dpa_url: None,
        privacy_url: Some("https://x.test/p".into()),
    };
    set(&conn, vec![Parsed::Terms(t.clone())], vec![], None);
    let r = rows(&conn);
    assert_eq!(r[0].0, "terms");
    assert_eq!(Parsed::decode("terms", &r[0].1), Ok(Parsed::Terms(t)));
}

#[test]
fn a_failing_audit_insert_leaves_no_settings_row() {
    let conn = db();
    set(&conn, vec![Parsed::SignupQuota(25)], vec![], None);
    conn.execute_batch(
        "CREATE TRIGGER audit_refuses BEFORE INSERT ON operator_audit \
         BEGIN SELECT RAISE(ABORT, 'audit down'); END;",
    )
    .unwrap();
    let r = operator_update_settings(
        &conn,
        &Change {
            set: vec![Parsed::Signup(true), Parsed::SignupQuota(1)],
            unset: vec!["terms"],
        },
        None,
    );
    assert!(r.is_err());
    // nothing of the change survived, the old row is untouched
    assert_eq!(
        rows(&conn),
        [("signup_quota".to_string(), "25".to_string())]
    );
    let r = operator_update_settings(
        &conn,
        &Change {
            set: vec![],
            unset: vec!["signup_quota"],
        },
        None,
    );
    assert!(r.is_err());
    assert_eq!(rows(&conn).len(), 1);
}
