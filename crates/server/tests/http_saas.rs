// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The hosted-service plumbing: self-signup, password reset by e-mail, removing an account,
//! reading/writing org settings from a session, and monthly job quotas — the port of
//! `tests/test_saas.py`.
mod common;

use common::*;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::json;

// --- self-signup ------------------------------------------------------------------------------

#[test]
fn signup_is_closed_unless_the_operator_opens_it() {
    let app = App::new();
    let (code, body) = app.post(
        "/signup",
        None,
        json!({"email": "new@shop.test", "password": PW}),
    );
    assert!(code == 403 && body["error"].as_str().unwrap().contains("disabled"));
    assert!(
        app.db
            .with(|c| store::get_user_by_email(c, "new@shop.test"))
            .unwrap()
            .is_none()
    );
    assert_eq!(app.get("/health", None).1["signup"], "closed");
}

#[test]
fn open_signup_creates_an_org_and_signs_the_new_owner_in() {
    let app = App::with(|c| c.signup_open = true);
    assert_eq!(app.get("/health", None).1["signup"], "open");
    let (code, body) = app.post(
        "/signup",
        None,
        json!({"email": "Owner@Shop.test", "password": PW, "org_name": "Shop GmbH"}),
    );
    assert!(code == 200 && body["email"] == "owner@shop.test");
    assert!(body["org_id"] != DEFAULT_ORG && body["token"].as_str().unwrap().starts_with("sess_"));
    let (code, me) = app.get("/me", body["token"].as_str());
    assert!(code == 200 && me["org_id"] == body["org_id"] && me["kind"] == "session");
    // The new org starts empty — it sees none of the default org's things.
    assert_eq!(
        app.get("/printers", body["token"].as_str()).1["printers"],
        json!([])
    );
    let org = app
        .db
        .with(|c| store::get_org(c, body["org_id"].as_i64().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(org.name, "Shop GmbH");
}

#[test]
fn signup_names_the_org_after_the_mail_domain_without_an_org_name() {
    let app = App::with(|c| c.signup_open = true);
    let (_, body) = app.post(
        "/signup",
        None,
        json!({"email": "a@acme.test", "password": PW}),
    );
    let org = app
        .db
        .with(|c| store::get_org(c, body["org_id"].as_i64().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(org.name, "acme.test");
}

#[test]
fn signup_rejects_a_taken_address_a_bad_address_and_a_short_password() {
    let app = App::with(|c| c.signup_open = true);
    assert_eq!(
        app.post(
            "/signup",
            None,
            json!({"email": "a@shop.test", "password": PW})
        )
        .0,
        200
    );
    let orgs = app.db.with(store::list_orgs).unwrap().len();
    assert_eq!(
        app.post(
            "/signup",
            None,
            json!({"email": "a@shop.test", "password": PW})
        )
        .0,
        409
    );
    assert_eq!(
        app.post("/signup", None, json!({"email": "nope", "password": PW}))
            .0,
        400
    );
    assert_eq!(
        app.post(
            "/signup",
            None,
            json!({"email": "b@shop.test", "password": "short"})
        )
        .0,
        400
    );
    assert_eq!(app.db.with(store::list_orgs).unwrap().len(), orgs); // no half-made orgs left behind
}

#[test]
fn a_bad_password_spends_no_signup_slot() {
    let app = App::with(|c| c.signup_open = true);
    for i in 0..10 {
        let (code, _) = app.post(
            "/signup",
            None,
            json!({"email": format!("s{i}@shop.test"), "password": "sixsix"}),
        );
        assert_eq!(code, 400, "attempt {i}");
    }
    let (code, _) = app.post(
        "/signup",
        None,
        json!({"email": "ok@shop.test", "password": PW}),
    );
    assert_eq!(code, 200);
}

#[test]
fn signup_is_throttled_per_client_so_one_host_cannot_spray_orgs() {
    let app = App::with(|c| {
        c.signup_open = true;
        c.max_login_fails = 2;
    });
    for n in 0..2 {
        assert_eq!(
            app.post(
                "/signup",
                None,
                json!({"email": format!("u{n}@shop.test"), "password": PW})
            )
            .0,
            200
        );
    }
    let (code, _) = app.post(
        "/signup",
        None,
        json!({"email": "u3@shop.test", "password": PW}),
    );
    assert_eq!(code, 429);
    assert!(
        app.db
            .with(|c| store::get_user_by_email(c, "u3@shop.test"))
            .unwrap()
            .is_none()
    );
    // another client address has its own slot
    let r = app
        .call("POST", "/signup")
        .peer("10.0.0.9")
        .json(&json!({"email": "u4@shop.test", "password": PW}))
        .send();
    assert_eq!(r.status, 200);
}

#[test]
fn concurrent_signups_from_one_address_cannot_exceed_the_window_cap() {
    let app = App::with(|c| {
        c.signup_open = true;
        c.max_login_fails = 3;
    });
    let orgs = app.db.with(store::list_orgs).unwrap().len();
    let ok = std::thread::scope(|s| {
        let hs: Vec<_> = (0..30)
            .map(|n| {
                let app = &app;
                s.spawn(move || {
                    app.call("POST", "/signup")
                        .json(&json!({"email": format!("c{n}@shop.test"), "password": PW}))
                        .send()
                        .status
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap())
            .filter(|c| *c == 200)
            .count()
    });
    assert_eq!(ok, 3);
    assert_eq!(app.db.with(store::list_orgs).unwrap().len(), orgs + 3);
}

#[test]
fn failed_logins_named_like_a_signup_slot_do_not_block_that_address() {
    let app = App::with(|c| {
        c.signup_open = true;
        c.max_login_fails = 2;
    });
    for _ in 0..2 {
        assert_eq!(app.login("signup:127.0.0.1", "wrong").0, 401);
    }
    let (code, _) = app.post(
        "/signup",
        None,
        json!({"email": "u1@shop.test", "password": PW}),
    );
    assert_eq!(code, 200);
}

// --- password reset ---------------------------------------------------------------------------

#[test]
fn a_reset_mails_a_token_that_sets_a_new_password_exactly_once() {
    let app = App::new();
    let session = app.default_account();
    assert_eq!(
        app.post("/password/reset", None, json!({"email": "ops@shop.test"}))
            .0,
        200
    );
    let sent = app.mail.all();
    assert_eq!(sent[0].to, "ops@shop.test");
    assert_eq!(sent[0].subject, "Reset your printpapi password");
    let token = sent[0]
        .body
        .split("token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let (code, _) = app.post(
        "/password/reset/confirm",
        None,
        json!({"token": token, "password": "brandnewpassword"}),
    );
    assert_eq!(code, 200);
    assert_eq!(app.get("/me", Some(&session)).0, 401); // old session is dead
    assert_eq!(app.login("ops@shop.test", PW).0, 401);
    assert_eq!(app.login("ops@shop.test", "brandnewpassword").0, 200);
    // Spent — a leaked mail cannot be replayed.
    assert_eq!(
        app.post(
            "/password/reset/confirm",
            None,
            json!({"token": token, "password": "yetanotherpassword"})
        )
        .0,
        400
    );
}

#[test]
fn a_reset_for_an_unknown_address_answers_the_same_and_mails_nothing() {
    let app = App::new();
    app.default_account();
    let known = app.post("/password/reset", None, json!({"email": "ops@shop.test"}));
    let unknown = app.post(
        "/password/reset",
        None,
        json!({"email": "nobody@shop.test"}),
    );
    assert_eq!(known, unknown); // no account enumeration
    assert_eq!(known, (200, json!({"ok": true})));
    assert_eq!(
        app.mail
            .all()
            .iter()
            .map(|m| m.to.clone())
            .collect::<Vec<_>>(),
        ["ops@shop.test"]
    );
    assert_eq!(
        app.post(
            "/password/reset/confirm",
            None,
            json!({"token": "sess_invented", "password": PW})
        )
        .0,
        400
    );
    let (code, body) = app.post(
        "/password/reset/confirm",
        None,
        json!({"token": "x", "password": "short"}),
    );
    assert_eq!(
        (code, body["error"].as_str().unwrap()),
        (400, "password must be at least 8 characters")
    );
}

#[test]
fn the_reset_mail_only_links_when_the_operator_declared_a_public_url() {
    let db = mem();
    let app = App::on(db.clone(), |_| {});
    app.default_account();
    app.post("/password/reset", None, json!({"email": "ops@shop.test"}));
    // No PUBLIC_URL: the Host header is attacker-controlled, so the mail carries the bare token
    // instead of a link built from it.
    let body = app.mail.all()[0].body.clone();
    assert!(!body.contains("http"));
    assert!(body.contains(
        "It is valid for 60 minutes and works once. If this was not you, ignore this message.\n"
    ));
    let linked = App::on(db, |c| c.public_url = "https://print.shop.test".into());
    linked.post("/password/reset", None, json!({"email": "ops@shop.test"}));
    let body = linked.mail.all().last().unwrap().body.clone();
    assert!(body.contains("\nOpen https://print.shop.test/?reset=sess_"));
    assert!(
        body.starts_with(
            "Someone asked to reset the printpapi password for ops@shop.test.\n\nOpen "
        )
    );
}

#[test]
fn reset_requests_are_throttled_per_address() {
    let app = App::with(|c| c.max_login_fails = 2);
    app.default_account();
    for _ in 0..2 {
        assert_eq!(
            app.post("/password/reset", None, json!({"email": "ops@shop.test"}))
                .0,
            200
        );
    }
    assert_eq!(
        app.post("/password/reset", None, json!({"email": "ops@shop.test"}))
            .0,
        429
    ); // no mail-bombing an inbox
    assert_eq!(app.mail.all().len(), 2);
}

#[test]
fn a_broken_mail_server_still_answers_200() {
    let app = App::new();
    app.default_account();
    *app.mail.fail.lock().unwrap() = true;
    assert_eq!(
        app.post("/password/reset", None, json!({"email": "ops@shop.test"})),
        (200, json!({"ok": true}))
    );
}

// --- removing an account ----------------------------------------------------------------------

#[test]
fn a_session_removes_a_colleague_but_not_itself_and_not_a_foreign_user() {
    let app = App::new();
    let other = create_org(&app.db, "other");
    let session = app.default_account();
    let (code, mate) = app.post(
        "/users",
        Some(&session),
        json!({"email": "mate@shop.test", "password": PW}),
    );
    assert_eq!(code, 200);
    let (code, stranger) = app.post(
        &format!("/orgs/{other}/users"),
        Some("t"),
        json!({"email": "x@other.test", "password": PW}),
    );
    assert_eq!(code, 200);
    let me = app.get("/me", Some(&session)).1["user_id"].clone();
    assert_eq!(
        app.delete(&format!("/users/{}", stranger["id"]), Some(&session))
            .0,
        404
    );
    assert_eq!(app.delete(&format!("/users/{me}"), Some(&session)).0, 400);
    assert_eq!(
        app.delete(&format!("/users/{}", mate["id"]), Some(&session))
            .0,
        200
    );
    let users = app.get("/users", Some(&session)).1;
    let emails: Vec<&str> = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["email"].as_str().unwrap())
        .collect();
    assert_eq!(emails, ["ops@shop.test"]);
    assert_eq!(app.login("mate@shop.test", PW).0, 401);
}

#[test]
fn the_last_account_of_an_org_cannot_be_removed_and_a_machine_key_may_not_remove_anyone() {
    let app = App::new();
    let session = app.default_account();
    let me = app.get("/me", Some(&session)).1["user_id"].clone();
    let (code, body) = app.delete(&format!("/users/{me}"), Some("t")); // root, no self-rule
    assert!(
        code == 400
            && body["error"]
                .as_str()
                .unwrap()
                .contains("at least one account")
    );
    let key = app.post("/apikeys", Some("t"), json!({"label": "shop"})).1["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(app.delete(&format!("/users/{me}"), Some(&key)).0, 401);
}

// --- org settings from a session --------------------------------------------------------------

#[test]
fn a_session_reads_and_writes_its_own_org_settings_but_no_other() {
    let app = App::new();
    let other = create_org(&app.db, "other");
    let session = app.default_account();
    let (code, _) = app.put(
        &format!("/orgs/{DEFAULT_ORG}"),
        Some(&session),
        json!({"event_url": "https://hooks.shop.test/agents", "shopify_secret": "shhh"}),
    );
    assert_eq!(code, 200);
    let (code, org) = app.get(&format!("/orgs/{DEFAULT_ORG}"), Some(&session));
    assert_eq!(code, 200);
    assert_eq!(org["event_url"], "https://hooks.shop.test/agents");
    assert!(org["shopify_secret_set"] == true && !org.to_string().contains("shhh"));
    assert!(org["job_quota"].is_null() && org["jobs_this_month"] == 0);
    let keys: Vec<&String> = org.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "id",
            "name",
            "event_url",
            "shopify_secret_set",
            "job_quota",
            "plan",
            "jobs_this_month",
            "created_at"
        ]
    );
    assert_eq!(app.get(&format!("/orgs/{other}"), Some(&session)).0, 404);
    assert_eq!(app.get(&format!("/orgs/{other}"), Some("t")).0, 200); // root sees every org
    assert_eq!(app.get("/orgs/9999", Some("t")).0, 404);
}

#[test]
fn org_updates_apply_field_by_field_and_report_what_stuck() {
    // Not transactional, like the Python: a later field's rejection leaves the earlier ones written.
    let app = App::new();
    let (code, body) = app.put(
        "/orgs/1",
        Some("t"),
        json!({"job_quota": 7, "event_url": "https://h.example/e"}),
    );
    assert_eq!(code, 200);
    // event_url is applied before job_quota, whatever the body's own order
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"ok":true,"event_url":"https://h.example/e","job_quota":7}"#
    );
    let (code, body) = app.put(
        "/orgs/1",
        Some("t"),
        json!({"event_url": "https://h.example/f", "shopify_secret": 5}),
    );
    assert_eq!(
        (code, body["error"].as_str().unwrap()),
        (400, "shopify_secret must be a string or null")
    );
    let org = app.get("/orgs/1", Some("t")).1;
    assert_eq!(org["event_url"], "https://h.example/f"); // the first field stuck
    assert_eq!(
        app.put("/orgs/1", Some("t"), json!({})),
        (400, json!({"error": "nothing to update"}))
    );
    assert_eq!(
        app.put("/orgs/77", Some("t"), json!({})),
        (404, json!({"error": "not found"}))
    );
    assert_eq!(
        app.put("/orgs/1", Some("t"), json!({"shopify_secret": "  "}))
            .0,
        400
    );
    assert_eq!(
        app.put("/orgs/1", Some("t"), json!({"job_quota": -1})).0,
        400
    );
    assert_eq!(
        app.put("/orgs/1", Some("t"), json!({"job_quota": true})).0,
        400
    );
    assert_eq!(
        app.put("/orgs/1", Some("t"), json!({"job_quota": 1.5})).0,
        400
    );
}

#[test]
fn only_the_operator_sets_a_quota() {
    let app = App::new();
    let session = app.default_account();
    let url = format!("/orgs/{DEFAULT_ORG}");
    let (code, body) = app.put(&url, Some(&session), json!({"job_quota": 100000}));
    assert!(code == 403 && body["error"].as_str().unwrap().contains("operator"));
    assert_eq!(
        app.put(&url, Some("t"), json!({"job_quota": "lots"})).0,
        400
    );
    assert_eq!(app.put(&url, Some("t"), json!({"job_quota": 5})).0, 200);
    assert_eq!(app.get(&url, Some(&session)).1["job_quota"], 5);
    assert_eq!(app.put(&url, Some("t"), json!({"job_quota": null})).0, 200);
    assert!(app.get(&url, Some(&session)).1["job_quota"].is_null());
}

// --- quotas -----------------------------------------------------------------------------------

#[test]
fn a_signup_quota_caps_the_new_org_but_not_root_created_ones() {
    let app = App::with(|c| {
        c.signup_open = true;
        c.signup_quota = Some(2);
    });
    let (_, s) = app.post(
        "/signup",
        None,
        json!({"email": "q@shop.test", "password": PW}),
    );
    let (org, session) = (s["org_id"].as_i64().unwrap(), s["token"].as_str().unwrap());
    assert_eq!(
        app.get(&format!("/orgs/{org}"), Some(session)).1["job_quota"],
        2
    );
    let pid = register(&app.db, "pc", "agentkey", &[decl("HP", true)], org).pid("HP");
    let job = json!({"printer_id": pid, "type": "raw_base64", "content": "eA=="});
    assert_eq!(app.post("/jobs", Some(session), job.clone()).0, 200);
    assert_eq!(app.post("/jobs", Some(session), job.clone()).0, 200);
    let (code, body) = app.post("/jobs", Some(session), job);
    assert!(code == 402 && body["error"].as_str().unwrap().contains("quota"));
    // an org the operator creates keeps the operator's choice: no quota
    let (_, o) = app.post("/orgs", Some("t"), json!({"name": "by root"}));
    let url = format!("/orgs/{}", o["id"]);
    assert!(app.get(&url, Some("t")).1["job_quota"].is_null());
}

#[test]
fn without_a_signup_quota_a_new_org_is_unlimited() {
    let app = App::with(|c| c.signup_open = true);
    let (_, s) = app.post(
        "/signup",
        None,
        json!({"email": "q@shop.test", "password": PW}),
    );
    let url = format!("/orgs/{}", s["org_id"]);
    assert!(app.get(&url, s["token"].as_str()).1["job_quota"].is_null());
}

#[test]
fn a_spent_quota_answers_402_on_every_submit_path() {
    let app = App::new();
    let pid = register(&app.db, "pc", "agentkey", &[decl("HP", true)], DEFAULT_ORG).pid("HP");
    let key = app.post("/apikeys", Some("t"), json!({"label": "shop"})).1["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        app.put(
            &format!("/orgs/{DEFAULT_ORG}"),
            Some("t"),
            json!({"job_quota": 1})
        )
        .0,
        200
    );
    let job = json!({"printer_id": pid, "type": "raw_base64", "content": "eA=="});
    assert_eq!(app.post("/jobs", Some(&key), job.clone()).0, 200);
    let (code, body) = app.post("/jobs", Some(&key), job);
    assert!(code == 402 && body["error"].as_str().unwrap().contains("quota"));
    let order = json!({"printer_id": pid, "order": {"number": "1001", "items": []}});
    assert_eq!(app.post("/orders", Some(&key), order).0, 402);
    let r = app
        .call("POST", "/printjobs")
        .basic(&key)
        .json(&json!({"printerId": pid, "contentType": "raw_base64", "content": "eA==", "source": "t"}))
        .send();
    assert_eq!(r.status, 402);
    assert_eq!(r.json()["code"], "QuotaExceeded");
    // The counter is per org: another org's budget is untouched.
    assert_eq!(
        app.get(&format!("/orgs/{DEFAULT_ORG}"), Some("t")).1["jobs_this_month"],
        1
    );
}

// --- job retention ----------------------------------------------------------------------------

#[test]
fn retention_runs_at_most_once_an_hour_and_scrubbed_jobs_still_list() {
    use printpapi_server::server::retention_pass;
    let day = 86_400.0;
    let now = store::month_start(printpapi_server::now()) + 25.0 * day;
    let app = App::with(|c| c.job_retention_days = Some(7));
    let pid = agent_z(&app.db).pid("Z");
    let old = enqueue(&app.db, pid, b"secret", |j| j.title = Some("T".into()));
    let age = |id: i64| {
        app.db.with(|c| {
            c.execute(
                "UPDATE jobs SET state='done', created_at=?1, finished_at=?1 WHERE id=?2",
                rusqlite::params![now - 10.0 * day, id],
            )
            .unwrap()
        })
    };
    age(old);
    assert_eq!(retention_pass(&app.state, now).unwrap().scrubbed, 1);
    // inside the hour nothing runs, even with fresh work waiting
    let second = enqueue(&app.db, pid, b"secret", |_| {});
    age(second);
    assert!(retention_pass(&app.state, now + 3599.0).is_none());
    assert_eq!(
        retention_pass(&app.state, now + 3600.0).unwrap().scrubbed,
        1
    );
    // the history and the PrintNode layer cope with a job that lost its title
    let (code, body) = app.get("/jobs", Some("t"));
    assert_eq!(code, 200);
    assert!(body.to_string().contains(&format!("\"id\":{old}")));
    assert_eq!(
        app.get(&format!("/jobs/{old}"), Some("t")).1["state"],
        "done"
    );
    let key = "pnkey";
    add_key(&app.db, "pn", key, DEFAULT_ORG);
    let (code, body) = app.pn("GET", &format!("/printjobs/{old}"), key, None);
    assert_eq!(code, 200, "{body}");
}

#[test]
fn retention_does_nothing_unless_configured() {
    use printpapi_server::server::retention_pass;
    let app = App::new();
    assert!(retention_pass(&app.state, 1e12).is_none());
}

#[test]
fn retention_retries_a_wal_truncate_a_reader_blocked_even_when_nothing_changed() {
    use printpapi_server::server::retention_pass;
    let day = 86_400.0;
    let now = store::month_start(printpapi_server::now()) + 25.0 * day;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.db");
    let path = path.to_str().unwrap();
    let app = App::on(store::Db::open(path).unwrap(), |c| {
        c.job_retention_days = Some(7)
    });
    let pid = agent_z(&app.db).pid("Z");
    let marker = b"RETENTION-WAL-MARKER-77;";
    let id = enqueue(&app.db, pid, &marker.repeat(1000), |_| {});
    app.db.with(|c| {
        c.execute(
            "UPDATE jobs SET state='done', created_at=?1, finished_at=?1 WHERE id=?2",
            rusqlite::params![now - 10.0 * day, id],
        )
        .unwrap()
    });
    // a reader pins the WAL while the pass runs
    let reader = store::connect(path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT count(*) FROM jobs;")
        .unwrap();
    let t = std::time::Instant::now();
    assert!(retention_pass(&app.state, now).unwrap().wal_busy);
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
    // the next pass changes no rows, but finishes the truncate once the reader is gone
    reader.execute_batch("COMMIT").unwrap();
    let p = retention_pass(&app.state, now + 3600.0).unwrap();
    assert_eq!(p.scrubbed + p.deleted + p.abandoned, 0);
    assert!(!p.wal_busy);
    let wal = std::fs::read(dir.path().join("r.db-wal")).unwrap_or_default();
    assert!(!wal.windows(marker.len()).any(|w| w == marker));
}
