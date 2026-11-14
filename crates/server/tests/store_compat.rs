// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The Rust store against a database the Python server wrote (`fixtures/python_v2.sqlite3`, built
//! by `fixtures/make_python_db.py` from `app/store.py`): the migrations and the added indexes must
//! apply cleanly, and every read and write must work on rows the Rust code did not create.
use printpapi_server::store::{self, *};
use serde_json::{Value, json};

const BASE: f64 = 1_700_000_000.0;
const ORG: i64 = 2;

/// A private copy of the fixture, opened the way `main` opens a database. The `TempDir` must
/// outlive the `Db`.
fn open() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("python_v2.sqlite3");
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/python_v2.sqlite3"
    );
    std::fs::copy(fixture, &path).unwrap();
    let db = Db::open(path.to_str().unwrap()).unwrap();
    (dir, db)
}

#[test]
fn opening_a_python_database_applies_the_migrations_and_the_new_indexes_cleanly() {
    let (dir, db) = open();
    let indexes: Vec<String> = db.with(|c| {
        let mut stmt = c
            .prepare("SELECT name FROM sqlite_master WHERE type='index' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    });
    for wanted in [
        "jobs_claim",
        "jobs_org_created",
        "jobs_state_claimed",
        "jobs_hooks",
        "agents_org",
        "jobs_idem",
        "users_email",
    ] {
        assert!(
            indexes.iter().any(|i| i == wanted),
            "{wanted} missing from {indexes:?}"
        );
    }
    // reopening is a no-op, and the seeded default org/user were not duplicated
    drop(db);
    let db = Db::open(dir.path().join("python_v2.sqlite3").to_str().unwrap()).unwrap();
    let orgs = db.with(list_orgs).unwrap();
    let names: Vec<_> = orgs.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["default", "Acme GmbH"]);
}

#[test]
fn credentials_written_by_python_authenticate() {
    let (_dir, db) = open();
    db.with(|c| {
        assert_eq!(
            authenticate_client(c, "pk_known_test_key").unwrap(),
            Some(ClientKey { id: 1, org_id: ORG })
        );
        assert_eq!(authenticate_client(c, "pk_other").unwrap(), None);
        assert_eq!(authenticate_agent(c, "agent-key-1").unwrap(), Some(1));
        let s = authenticate_session(c, "sess_known", Some(BASE))
            .unwrap()
            .unwrap();
        assert_eq!(
            (s.user_id, s.org_id, s.email.as_deref()),
            (2, ORG, Some("boss@acme.test"))
        );
        assert_eq!(
            authenticate_session(c, "sess_known", Some(4102444801.0)).unwrap(),
            None
        );
        // the scrypt hash is the auth module's business; the store must hand it back untouched
        let u = get_user_by_email(c, "  BOSS@acme.test ").unwrap().unwrap();
        assert_eq!((u.id, u.org_id), (2, ORG));
        assert!(u.password_hash.unwrap().starts_with("scrypt$"));
    });
}

#[test]
fn printers_agents_and_orgs_read_back_including_integer_timestamps() {
    let (_dir, db) = open();
    db.with(|c| {
        let ps = list_printers(c, 60.0, Some(BASE + 120.0), Some(ORG)).unwrap();
        let names: Vec<_> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Zebra ZD420", "HP LaserJet"]);
        assert!(ps.iter().all(|p| p.online && p.agent_name == "win-1"));
        assert_eq!((ps[0].can_pdf, &ps[0].capabilities), (false, &None));
        assert!(ps[1].can_pdf);
        assert_eq!(
            ps[1].capabilities,
            Some(json!({"papers": ["A4", "Letter"], "bins": ["Tray 1"], "duplex": true, "color": false}))
        );
        assert!(list_printers(c, 60.0, Some(BASE + 120.0), Some(1)).unwrap().is_empty());
        assert!(!list_printers(c, 60.0, Some(BASE + 1000.0), Some(ORG)).unwrap()[0].online);

        let agents = list_agents(c, 60.0, Some(BASE + 120.0), Some(ORG)).unwrap();
        // created_at is an INTEGER column value in the fixture
        assert_eq!((agents.len(), agents[0].printers, agents[0].created_at), (1, 2, BASE));
        assert_eq!(get_org(c, ORG).unwrap().unwrap().created_at, BASE);
        assert_eq!(list_users(c, Some(ORG)).unwrap()[0].email.as_deref(), Some("boss@acme.test"));
        let keys = list_api_keys(c, Some(ORG)).unwrap();
        assert_eq!((keys[0].label.as_str(), keys[0].active), ("acme-key", 1));
    });
}

#[test]
fn history_status_and_metrics_read_python_rows() {
    let (_dir, db) = open();
    db.with(|c| {
        let rows = recent_jobs(c, 50, Some(ORG), None).unwrap();
        let seen: Vec<_> = rows.iter().map(|r| (r.id, r.state.as_str())).collect();
        assert_eq!(
            seen,
            [
                (5, "queued"),
                (4, "cancelled"),
                (3, "claimed"),
                (2, "failed"),
                (1, "done")
            ]
        );
        assert_eq!(rows[0].title.as_deref(), Some("waiting"));
        assert_eq!(
            (rows[0].printer_name.as_str(), rows[0].agent_name.as_str()),
            ("HP LaserJet", "win-1")
        );
        assert_eq!(rows[3].error.as_deref(), Some("printer offline"));
        assert_eq!(recent_jobs(c, 50, Some(1), None).unwrap().len(), 0);
        let some = recent_jobs(c, 50, None, Some(&[1, 3, 99])).unwrap();
        assert_eq!(some.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 1]);

        let j = get_job(c, 1, Some(ORG)).unwrap().unwrap();
        assert_eq!(
            (j.state.as_str(), j.printer_id, j.created_at, j.finished_at),
            ("done", 1, BASE, Some(BASE + 2.0))
        );
        assert!(get_job(c, 1, Some(1)).unwrap().is_none());
        assert_eq!(get_payload(c, 2, 1).unwrap().unwrap(), b"%PDF-failed");

        let m = metrics(c, 60.0, Some(BASE + 120.0), Some(ORG)).unwrap();
        assert_eq!(
            (m.agents_total, m.agents_online, m.printers_total),
            (1, 1, 2)
        );
        assert_eq!(m.jobs.values().sum::<i64>(), 5);
        assert_eq!(org_usage(c, ORG, Some(BASE - 1.0)).unwrap(), 5);

        let hooks = pending_webhooks(c, 5).unwrap();
        assert_eq!(hooks.len(), 1); // the queued job has a callback but is not terminal yet
        assert_eq!(
            (
                hooks[0].job_id,
                hooks[0].state.as_str(),
                hooks[0].error.as_deref()
            ),
            (2, "failed", Some("printer offline"))
        );
    });
}

#[test]
fn claiming_a_python_queued_job_roundtrips_its_options() {
    let (_dir, db) = open();
    db.with(|c| {
        let job = claim_job(c, 1, Some(BASE + 1000.0)).unwrap().unwrap();
        assert_eq!(
            (job.job_id, job.printer_id, job.mode.as_str(), job.copies),
            (5, 2, "pdf", 3)
        );
        assert_eq!(
            job.options,
            Some(json!({"duplex": "short-edge", "paper": "Letter", "bin": "Tray 1"}))
        );
        assert!(claim_job(c, 1, Some(BASE + 1000.0)).unwrap().is_none());
        // the in-flight job is the one Python had already claimed (older claimed_at)
        let held = claimed_job(c, 1).unwrap().unwrap();
        assert_eq!((held.job_id, held.copies), (3, 2));
        assert!(finish_job(c, 3, 1, true, None).unwrap());
        assert_eq!(get_job(c, 3, None).unwrap().unwrap().state, "done");
        assert_eq!(
            cancel_job(c, 1, Some(ORG)).unwrap(),
            CancelOutcome::NotCancellable
        );
    });
}

#[test]
fn enqueue_and_idempotency_work_against_python_rows() {
    let (_dir, db) = open();
    db.with(|c| {
        let hp = json!(2);
        let job = |key: Option<&str>| NewJob {
            printer_id: &hp,
            type_: "pdf_base64",
            mode: "pdf",
            payload: b"%PDF-new",
            user_id: 2,
            title: Some("new".into()),
            copies: 1,
            callback_url: None,
            options: Some(json!({"paper": "A4"}).as_object().unwrap().clone()),
            org_id: Some(ORG),
            idempotency_key: key.map(Into::into),
            expire_after: None,
        };
        // the Python job holding "order-100" is found, even though it is still queued
        let again = enqueue_job(c, &job(Some("order-100"))).unwrap();
        assert_eq!(
            again,
            Enqueued {
                job_id: 5,
                agent_id: 1,
                created: false
            }
        );
        let fresh = enqueue_job(c, &job(Some("order-101"))).unwrap();
        assert_eq!(
            fresh,
            Enqueued {
                job_id: 6,
                agent_id: 1,
                created: true
            }
        );
        // another org cannot reach the printer
        let foreign = NewJob {
            org_id: Some(1),
            ..job(None)
        };
        assert!(matches!(
            enqueue_job(c, &foreign),
            Err(StoreError::UnknownPrinter(_))
        ));
        // the fixture's 2023 rows do not count towards this month, only the one just written does
        assert_eq!(org_usage(c, ORG, None).unwrap(), 1);
        set_org_quota(c, ORG, Some(1)).unwrap();
        assert!(matches!(
            enqueue_job(c, &job(None)),
            Err(StoreError::QuotaExceeded(_))
        ));
        // an idempotent resubmit spends nothing, even with the quota used up
        assert!(!enqueue_job(c, &job(Some("order-100"))).unwrap().created);
    });
}

#[test]
fn registering_against_python_rows_keeps_ids_and_enforces_the_name_key_binding() {
    let (_dir, db) = open();
    db.with(|c| {
        let hp = PrinterDecl {
            name: "HP LaserJet".into(),
            can_pdf: true,
            capabilities: Some(json!({"duplex": false})),
        };
        let r = register_agent(c, "win-1", "agent-key-1", &[hp], ORG).unwrap();
        assert_eq!(
            (r.computer_id, r.printer_ids["HP LaserJet"].clone()),
            (1, Value::from(2))
        );
        let ps = list_printers(c, 60.0, None, Some(ORG)).unwrap();
        assert_eq!(ps.len(), 2); // Zebra stays although this registration did not list it
        assert_eq!(ps[1].capabilities, Some(json!({"duplex": false})));
        assert!(matches!(
            register_agent(c, "win-1", "stolen", &[], ORG),
            Err(StoreError::Auth(_))
        ));
        assert!(matches!(
            register_agent(c, "win-2", "agent-key-1", &[], ORG),
            Err(StoreError::Conflict)
        ));
    });
}

#[test]
fn expiring_and_reaping_python_rows() {
    let (_dir, db) = open();
    db.with(|c| {
        assert_eq!(expire_jobs(c, Some(BASE + 5.0)).unwrap(), 0);
        assert_eq!(expire_jobs(c, Some(4102444801.0)).unwrap(), 1);
        let j = get_job(c, 5, None).unwrap().unwrap();
        assert_eq!(
            (j.state.as_str(), j.error.as_deref()),
            ("failed", Some("expired"))
        );
        // job 3 was claimed long ago and is stale
        assert_eq!(
            requeue_stale(c, 60.0, 3, Some(BASE + 1000.0), false).unwrap(),
            1
        );
        assert_eq!(get_job(c, 3, None).unwrap().unwrap().state, "queued");
    });
}

#[test]
fn deleting_the_python_org_removes_its_rows() {
    let (_dir, db) = open();
    db.with(|c| {
        assert!(store::delete_org(c, ORG).unwrap());
        assert!(
            authenticate_session(c, "sess_known", Some(BASE))
                .unwrap()
                .is_none()
        );
        assert!(authenticate_agent(c, "agent-key-1").unwrap().is_none());
        assert!(
            authenticate_client(c, "pk_known_test_key")
                .unwrap()
                .is_none()
        );
        assert!(recent_jobs(c, 50, None, None).unwrap().is_empty());
    });
}

/// The other direction, run by hand: `PRINTPAPI_COMPAT_OUT=/some/new.sqlite3 cargo test -p
/// printpapi-server --test store_compat -- --ignored`, then open that file with the Python
/// `app/store.py` (agent, printers with capabilities, jobs with options and an idempotency key).
#[test]
#[ignore = "writes a database for the Python store to read; needs PRINTPAPI_COMPAT_OUT"]
fn write_a_database_for_python_to_read() {
    let out = std::env::var("PRINTPAPI_COMPAT_OUT").expect("set PRINTPAPI_COMPAT_OUT");
    let db = Db::open(&out).unwrap();
    db.with(|c| {
        let caps = json!({"papers": ["A4"], "duplex": true});
        let printers = [
            PrinterDecl {
                name: "HP".into(),
                can_pdf: true,
                capabilities: Some(caps),
            },
            PrinterDecl {
                name: "Zebra".into(),
                can_pdf: false,
                capabilities: None,
            },
        ];
        let reg = register_agent(c, "rust-1", "rust-key", &printers, DEFAULT_ORG).unwrap();
        let hp = reg.printer_ids["HP"].clone();
        let opts = json!({"duplex": "long-edge", "paper": "A4"})
            .as_object()
            .unwrap()
            .clone();
        let job = NewJob {
            printer_id: &hp,
            type_: "pdf_base64",
            mode: "pdf",
            payload: b"%PDF-rust",
            user_id: DEFAULT_USER,
            title: Some("from rust".into()),
            copies: 2,
            callback_url: Some("https://hooks.test/x".into()),
            options: Some(opts),
            org_id: None,
            idempotency_key: Some("rust-order-1".into()),
            expire_after: Some(3600),
        };
        enqueue_job(c, &job).unwrap();
        let dev = register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-mac1").unwrap();
        assert!(dev.agent_id > reg.computer_id);
        let uid = signup(c, "Rust Org", "Rust@Shop.test", "scrypt$stub", None, None)
            .unwrap()
            .user_id;
        create_session(c, uid, "sess_rust", 3600.0, None).unwrap();
        add_api_key(c, "rust-key", "pk_rust", DEFAULT_ORG).unwrap();
        // the new table is in the file Python opens, with a row in it
        let tx = begin_tx(c).unwrap();
        insert_audit(
            &tx,
            &AuditRow {
                action: "org.update",
                org_id: Some(ORG),
                ..AuditRow::default()
            },
        )
        .unwrap();
        tx.commit().unwrap();
    });
}

#[test]
fn a_python_database_gets_the_label_templates_table_at_startup() {
    let (dir, db) = open();
    let has_table = |db: &Db| -> bool {
        db.with(|c| {
            c.query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='label_templates'",
                [],
                |_| Ok(()),
            )
            .is_ok()
        })
    };
    assert!(has_table(&db));
    // usable on the old rows' org, and a second start leaves what was saved alone
    let spec = printpapi_server::label::parse_spec(
        json!({
            "name": "shipping", "content": "text", "label_mm": {"w": 50, "h": 30}, "dpi": 203,
            "rotate": 0, "crop_percent": {"t": 0, "r": 0, "b": 0, "l": 0},
            "place_mm": {"x": 0, "y": 0, "w": 50, "h": 30}
        })
        .as_object()
        .unwrap(),
    )
    .unwrap();
    db.with(|c| create_label_template(c, Some(ORG), &spec))
        .unwrap();
    drop(db);
    let db = Db::open(dir.path().join("python_v2.sqlite3").to_str().unwrap()).unwrap();
    assert!(has_table(&db));
    let list = db.with(|c| list_label_templates(c, Some(ORG))).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].spec.name, "shipping");
}

#[test]
fn a_python_database_gets_the_operator_audit_table_and_the_python_tables_stay_as_they_were() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("python_v2.sqlite3");
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/python_v2.sqlite3"
    );
    std::fs::copy(fixture, &path).unwrap();
    let columns = |path: &std::path::Path, table: &str| -> Vec<String> {
        let c = rusqlite::Connection::open(path).unwrap();
        let mut st = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
        st.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let python_tables = [
        "orgs", "users", "sessions", "agents", "printers", "jobs", "api_keys",
    ];
    let before: Vec<_> = python_tables.iter().map(|t| columns(&path, t)).collect();
    assert!(columns(&path, "operator_audit").is_empty());

    let db = Db::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        columns(&path, "operator_audit"),
        [
            "id",
            "at",
            "actor",
            "action",
            "org_id",
            "org_created_at",
            "target",
            "reason",
            "detail"
        ]
    );
    assert_eq!(
        columns(&path, "org_locks"),
        ["org_id", "org_created_at", "reason", "locked_at"]
    );
    assert_eq!(
        columns(&path, "support_grants"),
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
    // the new table adds nothing to a table Python knows beyond the old migrations
    for (t, was) in python_tables.iter().zip(&before) {
        let now = columns(&path, t);
        assert!(
            was.iter().all(|c| now.contains(c)),
            "{t}: {was:?} -> {now:?}"
        );
    }
    // a row survives a second start
    db.with(|c| {
        c.execute(
            "INSERT INTO support_grants(org_id, org_created_at, reason, granted_at, expires_at)              VALUES(?,?,?,?,?)",
            rusqlite::params![ORG, BASE, "ticket 4711", BASE, BASE + 900.0],
        )
        .unwrap();
        let tx = begin_tx(c).unwrap();
        insert_audit(
            &tx,
            &AuditRow {
                action: "org.update",
                org_id: Some(ORG),
                ..AuditRow::default()
            },
        )
        .unwrap();
        tx.commit().unwrap();
    });
    drop(db);
    let db = Db::open(path.to_str().unwrap()).unwrap();
    let rows = db.with(|c| list_audit(c, None, None, None, 10)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "org.update");
    assert_eq!(rows[0].actor, "operator");
    // the grant is still there, and the Python-era tables are not what holds it
    let grants: i64 = db.with(|c| {
        c.query_row("SELECT COUNT(*) FROM support_grants", [], |r| r.get(0))
            .unwrap()
    });
    assert_eq!(grants, 1);
}
