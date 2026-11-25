// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Pairing over HTTP: who may make a code, the redeem call and its throttle, what the fetched key
//! may do, the takeover at the first poll, the late result of the old machine. The real routes over
//! an in-memory database; the long-poll wake over loopback with the real agent client.
mod common;

use common::*;
use printpapi_agent::client::{Client, Http};
use printpapi_agent::config::HttpSettings;
use printpapi_server::auth::Scope;
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::spawn_test_server;
use printpapi_server::mail::SmtpMailer;
use printpapi_server::server::{AppState, Config};
use printpapi_server::store::{self, DEFAULT_ORG, Db};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

fn make(app: &App, token: &str, body: Value) -> (u16, Value) {
    app.post("/pairing", Some(token), body)
}

/// A new code from root.
fn code(app: &App) -> String {
    let (st, b) = make(app, "t", json!({}));
    assert_eq!(st, 200, "{b}");
    b["code"].as_str().unwrap().to_string()
}

fn pair(app: &App, code: &str, name: &str) -> Reply {
    app.call("POST", "/agent/pair")
        .json(&json!({"code": code, "name": name}))
        .send()
}

/// "office-pc" with its own full key and printer Z, in the default org.
fn office(app: &App) -> Registered {
    add_key(&app.db, "office-pc", "old-key", DEFAULT_ORG);
    register(
        &app.db,
        "office-pc",
        "old-key",
        &[decl("Z", false)],
        DEFAULT_ORG,
    )
}

fn register_http(app: &App, key: &str, name: &str) -> Reply {
    app.agent(
        "POST",
        "/agent/register",
        key,
        Some(json!({"name": name, "printers": [{"name": "Z"}]})),
    )
}

fn org(app: &App, name: &str) -> i64 {
    app.post("/orgs", Some("t"), json!({ "name": name })).1["id"]
        .as_i64()
        .unwrap()
}

// --- who may make, list and cancel a code -------------------------------------------------------

#[test]
fn creating_a_code_needs_root_or_a_session_never_a_machine_key() {
    let app = App::with(|c| c.operator_token = Some("operator-token-123456".into()));
    add_key(&app.db, "n8n", "machine", DEFAULT_ORG);
    let session = app.default_account();
    for who in [
        None,
        Some("machine"),
        Some("operator-token-123456"),
        Some("junk"),
    ] {
        assert_eq!(make(&app, who.unwrap_or(""), json!({})).0, 401, "{who:?}");
        assert_eq!(app.get("/pairing", who).0, 401, "{who:?}");
        assert_eq!(app.delete("/pairing/1", who).0, 401, "{who:?}");
    }
    // a key of every scope is no manager either
    for scope in [Scope::Agent, Scope::Full, Scope::Read, Scope::Print] {
        let k = format!("k-{}", scope.as_str());
        app.db
            .with(|c| store::add_api_key_scoped(c, &k, &k, DEFAULT_ORG, scope))
            .unwrap();
        assert_eq!(make(&app, &k, json!({})).0, 401, "{k}");
    }
    for who in ["t", session.as_str()] {
        let (st, b) = make(&app, who, json!({}));
        assert_eq!(st, 200, "{b}");
        assert_eq!(b["code"].as_str().unwrap().len(), 14);
    }
}

#[test]
fn the_reply_of_a_new_code_is_no_store_and_names_the_server_only_from_public_url() {
    let app = App::new();
    let r = app
        .call("POST", "/pairing")
        .bearer("t")
        .json(&json!({}))
        .header("host", "evil.example")
        .send();
    assert_eq!(r.status, 200);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let b = r.json();
    assert_eq!(b["server_url"], Value::Null);
    assert_eq!(b["computer_id"], Value::Null);
    assert_eq!(b["computer_name"], Value::Null);
    assert_eq!(b["old_key"], Value::Null);
    assert!(b["expires_at"].as_f64().unwrap() > 0.0 && b["id"].as_i64().is_some());
    let fields: Vec<&String> = b.as_object().unwrap().keys().collect();
    assert_eq!(
        fields,
        [
            "id",
            "code",
            "expires_at",
            "server_url",
            "computer_id",
            "computer_name",
            "old_key"
        ]
    );

    let app = App::with(|c| c.public_url = "https://print.example.com".into());
    let r = app
        .call("POST", "/pairing")
        .bearer("t")
        .json(&json!({}))
        .header("host", "evil.example")
        .header("x-forwarded-host", "evil.example")
        .send();
    assert_eq!(r.json()["server_url"], "https://print.example.com");
}

#[test]
fn a_session_cannot_name_another_org_and_root_defaults_to_the_default_org() {
    let app = App::new();
    let other = org(&app, "Other");
    let session = app.default_account();
    let (st, b) = make(&app, &session, json!({"org_id": other}));
    assert_eq!((st, b["error"].as_str()), (400, Some("org_id not allowed")));
    assert_eq!(make(&app, &session, json!({"org_id": DEFAULT_ORG})).0, 200);
    assert_eq!(
        make(&app, "t", json!({"org_id": 999})).1["error"],
        "unknown org"
    );
    let (st, _) = make(&app, "t", json!({"org_id": other}));
    assert_eq!(st, 200);
    let (_, list) = app.get("/pairing", Some("t"));
    let orgs: Vec<i64> = list["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["org_id"].as_i64().unwrap())
        .collect();
    assert_eq!(orgs, [other, DEFAULT_ORG]);
}

#[test]
fn list_and_cancel_are_org_scoped_and_the_list_has_no_secret() {
    let app = App::new();
    let other = org(&app, "Other");
    let mine = app.default_account();
    let theirs = app.account(other, "boss@other.test");
    let (_, a) = make(&app, &mine, json!({}));
    let (_, b) = make(&app, &theirs, json!({}));
    let (_, list) = app.get("/pairing", Some(&mine));
    assert_eq!(list["codes"].as_array().unwrap().len(), 1);
    assert_eq!(list["codes"][0]["id"], a["id"]);
    assert_eq!(list["codes"][0]["state"], "open");
    let text = list.to_string();
    for needle in [
        a["code"].as_str().unwrap(),
        "created_by",
        "ops@shop.test",
        "hash",
    ] {
        assert!(!text.contains(needle), "{needle}");
    }
    assert_eq!(
        app.call("GET", "/pairing")
            .bearer(&mine)
            .send()
            .header("cache-control"),
        Some("no-store")
    );
    assert_eq!(
        app.get("/pairing", Some("t")).1["codes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let foreign = format!("/pairing/{}", b["id"]);
    assert_eq!(app.delete(&foreign, Some(&mine)).0, 404);
    assert_eq!(
        app.delete(&format!("/pairing/{}", a["id"]), Some(&mine)).0,
        200
    );
    assert_eq!(
        app.delete(&format!("/pairing/{}", a["id"]), Some(&mine)).0,
        404
    );
    assert_eq!(app.delete("/pairing/999", Some("t")).0, 404);
    // a cancelled code is dead
    assert_eq!(pair(&app, a["code"].as_str().unwrap(), "pc").status, 401);
    assert_eq!(pair(&app, b["code"].as_str().unwrap(), "pc").status, 200);
}

#[test]
fn a_device_code_checks_the_device_and_the_limits() {
    let app = App::new();
    let dev = office(&app);
    let (st, b) = make(&app, "t", json!({"computer_id": dev.computer_id}));
    assert_eq!(st, 200, "{b}");
    assert_eq!(
        (b["computer_id"].as_i64(), b["computer_name"].as_str()),
        (Some(dev.computer_id), Some("office-pc"))
    );
    assert_eq!(b["old_key"]["label"], "office-pc");
    assert_eq!(
        make(&app, "t", json!({"computer_id": "7"})).1["error"],
        "computer_id must be a number"
    );
    assert_eq!(make(&app, "t", json!({"computer_id": 1.5})).0, 400);
    assert_eq!(make(&app, "t", json!({"computer_id": 999})).0, 404);
    // another org's device is as good as missing, for its own session too
    let other = org(&app, "Other");
    let theirs = app.account(other, "boss@other.test");
    assert_eq!(
        make(&app, &theirs, json!({"computer_id": dev.computer_id})).0,
        404
    );
    assert_eq!(
        make(
            &app,
            "t",
            json!({"computer_id": dev.computer_id, "org_id": other})
        )
        .0,
        404
    );
    // a Star printer connects by itself
    let star = app
        .db
        .with(|c| store::register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-star"))
        .unwrap();
    let (st, b) = make(&app, "t", json!({"computer_id": star.agent_id}));
    assert_eq!(
        (st, b["error"].as_str()),
        (
            400,
            Some("this printer connects by itself (CloudPRNT), there is nothing to pair")
        )
    );
    // twenty open codes, the 21st is refused
    for _ in 0..19 {
        assert_eq!(make(&app, "t", json!({})).0, 200);
    }
    let (st, b) = make(&app, "t", json!({}));
    assert_eq!(
        (st, b["error"].as_str()),
        (429, Some("too many open pairing codes"))
    );
}

// --- redeeming ----------------------------------------------------------------------------------

#[test]
fn redeeming_needs_no_credential_and_the_key_is_an_agent_key_and_nothing_else() {
    let app = App::new();
    let session = app.default_account();
    let (_, made) = make(&app, &session, json!({}));
    let code = made["code"].as_str().unwrap();
    // an Authorization header is ignored, even a wrong one
    let r = app
        .call("POST", "/agent/pair")
        .bearer("garbage")
        .json(&json!({"code": code.to_lowercase(), "name": "  warehouse-pc "}))
        .send();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let b = r.json();
    assert_eq!(b["name"], "warehouse-pc");
    assert_eq!(b["org"], "default");
    assert_eq!(b["created_by"], "ops@shop.test");
    assert_eq!(b["replaces"], false);
    let key = b["key"].as_str().unwrap();
    assert!(key.len() >= 32);

    // it registers, polls, reads the computers of its org (an agent key does) ...
    let reg = register_http(&app, key, "warehouse-pc");
    assert_eq!(reg.status, 200, "{}", reg.text());
    assert_eq!(app.agent("GET", "/agent/jobs", key, None).status, 204);
    let (st, comp) = app.get("/computers", Some(key));
    assert_eq!(st, 200);
    assert_eq!(comp["computers"][0]["name"], "warehouse-pc");
    // ... and nothing else
    let pid = reg.json()["printer_ids"]["Z"].as_i64().unwrap();
    assert_eq!(app.post("/jobs", Some(key), job_body(pid)).0, 403);
    assert_eq!(app.get("/jobs", Some(key)).0, 403);
    assert_eq!(app.get("/printers", Some(key)).0, 403);
    assert_eq!(app.get("/pairing", Some(key)).0, 401);
    assert_eq!(app.get("/apikeys", Some(key)).0, 401);
    assert_eq!(app.get("/me", Some(key)).1["scope"], "agent");
    // the same code never works twice, the list says who used it
    assert_eq!(pair(&app, code, "again").status, 401);
    let (_, list) = app.get("/pairing", Some(&session));
    assert_eq!(list["codes"][0]["state"], "connected");
    assert_eq!(list["codes"][0]["device_name"], "warehouse-pc");
    let (_, keys) = app.get("/apikeys", Some(&session));
    assert!(keys.to_string().contains("Device warehouse-pc"));
}

fn job_body(printer: i64) -> Value {
    json!({"printer_id": printer, "type": "raw_base64", "content": "SGVsbG8=", "title": "t"})
}

#[test]
fn every_dead_code_gets_the_same_answer() {
    let app = App::new();
    let used = code(&app);
    assert_eq!(pair(&app, &used, "a").status, 200);
    let (_, cancelled) = make(&app, "t", json!({}));
    app.delete(&format!("/pairing/{}", cancelled["id"]), Some("t"));
    let (_, expiring) = make(&app, "t", json!({}));
    app.db
        .with(|c| {
            c.execute(
                "UPDATE pairing_codes SET expires_at=1 WHERE id=?",
                [expiring["id"].as_i64().unwrap()],
            )
        })
        .unwrap();
    let answers: Vec<(u16, String)> = [
        used.as_str(),
        cancelled["code"].as_str().unwrap(),
        expiring["code"].as_str().unwrap(),
        "ZZZZ-ZZZZ-ZZZZ",
    ]
    .iter()
    .map(|c| {
        let r = pair(&app, c, "b");
        (r.status, r.text())
    })
    .collect();
    assert!(answers.iter().all(|a| a == &answers[0]), "{answers:?}");
    assert_eq!(answers[0].0, 401);
    assert!(
        answers[0]
            .1
            .contains("pairing code is wrong, expired or already used")
    );
}

#[test]
fn a_malformed_request_is_400_and_costs_no_slot() {
    let app = App::new();
    for _ in 0..30 {
        for body in [
            json!({"name": "pc"}),
            json!({"code": "ABC", "name": "pc"}),
            json!({"code": 12, "name": "pc"}),
            json!({"code": "7K3M-Q2ZD-9XHT"}),
            json!({"code": "7K3M-Q2ZD-9XHT", "name": "   "}),
            json!({"code": "7K3M-Q2ZD-9XHT", "name": "x".repeat(201)}),
            json!({"code": "7K3M-Q2ZD-9XHT", "name": "bad\nname"}),
            json!({"code": "7K3M-Q2ZD-9XHT", "name": "cloudprnt-star"}),
        ] {
            let r = app.call("POST", "/agent/pair").json(&body).send();
            assert_eq!(r.status, 400, "{body}");
        }
    }
    assert_eq!(
        app.call("POST", "/agent/pair")
            .raw(b"{broken")
            .send()
            .status,
        400
    );
    assert_eq!(
        app.call("POST", "/agent/pair").raw(b"[]").send().status,
        400
    );
    // and the throttle is still full
    assert_eq!(pair(&app, "7K3M-Q2ZD-9XHT", "pc").status, 401);
}

#[test]
fn a_cloudprnt_name_is_refused_and_the_code_is_not_spent() {
    let app = App::new();
    let c = code(&app);
    let r = pair(&app, &c, "cloudprnt-front");
    assert_eq!(r.status, 400);
    assert_eq!(
        r.json()["error"],
        "names starting with cloudprnt- are reserved for Star printers"
    );
    assert_eq!(pair(&app, &c, "front").status, 200);
}

#[test]
fn the_eleventh_wrong_code_is_throttled_and_a_login_is_not_affected() {
    let app = App::new();
    app.default_account();
    for n in 0..10 {
        assert_eq!(pair(&app, "ZZZZ-ZZZZ-ZZZZ", "pc").status, 401, "{n}");
    }
    let r = pair(&app, "ZZZZ-ZZZZ-ZZZZ", "pc");
    assert_eq!(r.status, 429);
    assert_eq!(
        r.json()["error"],
        "too many pairing attempts, try again later"
    );
    // a valid code is refused too while the address is blocked (nothing is spent)
    assert_eq!(pair(&app, &code(&app), "pc").status, 429);
    // another address and the login are separate
    let r = app
        .call("POST", "/agent/pair")
        .peer("10.1.1.1")
        .json(&json!({"code": "ZZZZ-ZZZZ-ZZZZ", "name": "pc"}))
        .send();
    assert_eq!(r.status, 401);
    assert_eq!(app.login("ops@shop.test", PW).0, 200);
    assert_eq!(app.login("ops@shop.test", "wrong wrong").0, 401);
}

#[test]
fn ten_taken_names_end_in_429_and_a_success_resets_the_count() {
    let app = App::new();
    let _ = office(&app);
    let c = code(&app);
    for n in 0..10 {
        assert_eq!(pair(&app, &c, "office-pc").status, 409, "{n}");
    }
    assert_eq!(
        pair(&app, &c, "office-pc").status,
        429,
        "a valid code cannot reset the counter"
    );
    // success resets: nine wrong ones, then a good one, then ten more wrong ones are possible
    let app = App::new();
    for _ in 0..9 {
        assert_eq!(pair(&app, "ZZZZ-ZZZZ-ZZZZ", "pc").status, 401);
    }
    assert_eq!(pair(&app, &code(&app), "pc").status, 200);
    for n in 0..10 {
        assert_eq!(pair(&app, "ZZZZ-ZZZZ-ZZZZ", "pc").status, 401, "{n}");
    }
    assert_eq!(pair(&app, "ZZZZ-ZZZZ-ZZZZ", "pc").status, 429);
}

#[test]
fn a_taken_name_leaves_the_code_usable() {
    let app = App::new();
    let _ = office(&app);
    let c = code(&app);
    let r = pair(&app, &c, "office-pc");
    assert_eq!(
        (r.status, r.json()["error"].as_str().map(String::from)),
        (409, Some("a device with this name already exists".into()))
    );
    assert_eq!(pair(&app, &c, "other-pc").status, 200);
}

// --- reconnect ----------------------------------------------------------------------------------

/// Office-pc with a job in flight, a Reconnect code redeemed and the new key registered. Returns
/// the device, the new key, the claimed job.
fn reconnected(app: &App) -> (Registered, String, i64) {
    let dev = office(app);
    let job = enqueue(&app.db, dev.pid("Z"), b"x", |_| {});
    claim(&app.db, dev.computer_id).unwrap();
    let (_, made) = make(app, "t", json!({"computer_id": dev.computer_id}));
    let r = pair(app, made["code"].as_str().unwrap(), "something else");
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["replaces"], true);
    assert_eq!(r.json()["name"], "office-pc");
    let key = r.json()["key"].as_str().unwrap().to_string();
    let reg = register_http(app, &key, "office-pc");
    assert_eq!(reg.status, 200, "{}", reg.text());
    assert_eq!(reg.json()["computer_id"], dev.computer_id);
    assert_eq!(reg.json()["printer_ids"]["Z"], dev.pid("Z"));
    (dev, key, job)
}

#[test]
fn the_old_agent_keeps_printing_until_the_new_one_polls() {
    let app = App::new();
    let (dev, new_key, _) = reconnected(&app);
    // registered, not swapped: the old key polls, the new one is not the device yet
    let queued = enqueue(&app.db, dev.pid("Z"), b"y", |_| {});
    let r = app.agent("GET", "/agent/jobs", "old-key", None);
    assert_eq!((r.status, r.json()["job_id"].as_i64()), (200, Some(queued)));
    let (_, list) = app.get("/pairing", Some("t"));
    assert_eq!(list["codes"][0]["state"], "registered");
    assert_eq!(list["codes"][0]["old_key"]["label"], "office-pc");

    // the first poll of the new key takes the device over
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 204);
    assert_eq!(app.agent("GET", "/agent/jobs", "old-key", None).status, 401);
    assert_eq!(register_http(&app, "old-key", "office-pc").status, 401);
    assert_eq!(
        app.agent("GET", "/agent/jobs", &new_key, None).status,
        204,
        "second poll as ever"
    );
    let (_, list) = app.get("/pairing", Some("t"));
    assert_eq!(list["codes"][0]["state"], "connected");
    // the old key was a full key: still there for the API, and listed for revocation
    assert_eq!(app.get("/jobs", Some("old-key")).0, 200);
    let old = list["codes"][0]["old_key"]["id"].as_i64().unwrap();
    assert_eq!(app.delete(&format!("/apikeys/{old}"), Some("t")).0, 200);
    assert_eq!(
        app.get("/pairing", Some("t")).1["codes"][0]["old_key"],
        Value::Null
    );
}

#[test]
fn the_old_keys_result_after_the_swap_is_recorded_only_as_done() {
    let app = App::new();
    let (dev, new_key, job) = reconnected(&app);
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 204);

    // ok:false, poll, payload, register: refused as before
    let path = format!("/agent/jobs/{job}/result");
    assert_eq!(
        app.agent(
            "POST",
            &path,
            "old-key",
            Some(json!({"ok": false, "error": "x"}))
        )
        .status,
        401
    );
    assert_eq!(
        app.agent(
            "GET",
            &format!("/agent/jobs/{job}/payload"),
            "old-key",
            None
        )
        .status,
        401
    );
    assert_eq!(job_state(&app.db, job), "claimed");
    // ok:true is the printed job: recorded, not printed again by the new machine
    let r = app.agent("POST", &path, "old-key", Some(json!({"ok": true})));
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(job_state(&app.db, job), "done");
    // a job the new machine claims after the swap is not the old key's to finish
    let later = enqueue(&app.db, dev.pid("Z"), b"z", |_| {});
    assert_eq!(
        app.agent("GET", "/agent/jobs", &new_key, None).json()["job_id"],
        later
    );
    let r = app.agent(
        "POST",
        &format!("/agent/jobs/{later}/result"),
        "old-key",
        Some(json!({"ok": true})),
    );
    assert_eq!(r.status, 401);
    assert_eq!(job_state(&app.db, later), "claimed");
    // a stranger gets nothing, with or without ok
    assert_eq!(
        app.agent("POST", &path, "stranger", Some(json!({"ok": true})))
            .status,
        401
    );
}

#[test]
fn the_late_result_also_works_on_the_results_route_and_ends_after_300_seconds() {
    let app = App::new();
    let (_, new_key, job) = reconnected(&app);
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 204);
    let created: f64 = app
        .db
        .with(|c| {
            c.query_row("SELECT created_at FROM jobs WHERE id=?", [job], |r| {
                r.get(0)
            })
        })
        .unwrap();
    let job_uid = store::job_uid(job, created);
    let results = |ok: bool| {
        app.agent(
            "POST",
            "/agent/results",
            "old-key",
            Some(json!({"job_id": job, "job_uid": job_uid, "ok": ok})),
        )
    };
    assert_eq!(results(false).status, 401);
    // a wrong uid changes nothing
    let r = app.agent(
        "POST",
        "/agent/results",
        "old-key",
        Some(json!({"job_id": job, "job_uid": "0".repeat(32), "ok": true})),
    );
    assert_eq!(r.json()["state"], Value::Null);
    assert_eq!(job_state(&app.db, job), "claimed");
    // past the window the key is nothing
    app.db
        .with(|c| {
            c.execute(
                "UPDATE pairing_codes SET taken_over_at = taken_over_at - 400",
                [],
            )
        })
        .unwrap();
    assert_eq!(results(true).status, 401);
    app.db
        .with(|c| {
            c.execute(
                "UPDATE pairing_codes SET taken_over_at = taken_over_at + 400",
                [],
            )
        })
        .unwrap();
    let r = results(true);
    assert_eq!(
        (r.status, r.json()),
        (200, json!({"ok": true, "state": "done"}))
    );
}

#[test]
fn a_key_narrowed_before_its_first_poll_takes_nothing_over() {
    let app = App::new();
    let (_, new_key, _) = reconnected(&app);
    let kid = app.get("/apikeys", Some("t")).1["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["label"] == "Device office-pc")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        app.put(
            &format!("/apikeys/{kid}"),
            Some("t"),
            json!({"scope": "read"})
        )
        .0,
        200
    );
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 401);
    assert_eq!(
        app.agent("GET", "/agent/jobs", "old-key", None).status,
        204,
        "the device is still the old one's"
    );
    assert_eq!(
        app.get("/pairing", Some("t")).1["codes"][0]["state"],
        "registered"
    );
    // widened again, it may take over
    assert_eq!(
        app.put(
            &format!("/apikeys/{kid}"),
            Some("t"),
            json!({"scope": "full"})
        )
        .0,
        200
    );
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 204);
    assert_eq!(app.agent("GET", "/agent/jobs", "old-key", None).status, 401);
}

#[test]
fn a_fetched_key_can_be_revoked_from_the_list_and_never_takes_over() {
    let app = App::new();
    let dev = office(&app);
    let (_, made) = make(&app, "t", json!({"computer_id": dev.computer_id}));
    let r = pair(&app, made["code"].as_str().unwrap(), "x");
    let key = r.json()["key"].as_str().unwrap().to_string();
    let (_, list) = app.get("/pairing", Some("t"));
    assert_eq!(list["codes"][0]["state"], "fetched");
    let key_id = list["codes"][0]["key_id"].as_i64().unwrap();
    assert_eq!(app.delete(&format!("/apikeys/{key_id}"), Some("t")).0, 200);
    assert_eq!(
        app.get("/pairing", Some("t")).1["codes"][0]["state"],
        "revoked"
    );
    assert_eq!(register_http(&app, &key, "office-pc").status, 401);
    assert_eq!(app.agent("GET", "/agent/jobs", &key, None).status, 401);
    assert_eq!(app.agent("GET", "/agent/jobs", "old-key", None).status, 204);
}

#[test]
fn the_reaper_ends_a_key_nobody_registered_after_a_day() {
    let app = App::new();
    let c = code(&app);
    let key = pair(&app, &c, "never").json()["key"]
        .as_str()
        .unwrap()
        .to_string();
    app.db.with(|cn| cn.execute("UPDATE pairing_codes SET used_at = used_at - 90000, expires_at = expires_at - 90000", [])).unwrap();
    printpapi_server::server::reaper_pass(&app.state, 300.0, 2);
    assert_eq!(register_http(&app, &key, "never").status, 401);
    assert_eq!(
        app.get("/pairing", Some("t")).1["codes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

// --- a waiting poll of the old machine ----------------------------------------------------------

#[test]
fn the_first_poll_of_the_new_key_ends_the_waiting_poll_of_the_old_one() {
    let mut cfg = Config::new("t");
    cfg.long_poll_timeout_s = 20.0;
    // longer than the test waits: only the wake-up of the swap can end the old poll in time
    cfg.poll_interval_s = 60.0;
    let db = Db::open(":memory:").unwrap();
    let state = Arc::new(AppState::new(
        cfg,
        db.clone(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ));
    let base = format!("http://{}", spawn_test_server(state.clone()));
    let http = Arc::new(Client::new(&HttpSettings::default()));
    add_key(&db, "office-pc", "old-key", DEFAULT_ORG);
    let dev = register(
        &db,
        "office-pc",
        "old-key",
        &[decl("Z", false)],
        DEFAULT_ORG,
    );

    let made = http
        .post(
            &format!("{base}/pairing"),
            "t",
            &json!({"computer_id": dev.computer_id}),
        )
        .unwrap()
        .unwrap();
    let got = http
        .post(
            &format!("{base}/agent/pair"),
            "",
            &json!({"code": made["code"], "name": "x"}),
        )
        .unwrap()
        .unwrap();
    let new_key = got["key"].as_str().unwrap().to_string();
    http.post(
        &format!("{base}/agent/register"),
        &new_key,
        &json!({"name": "office-pc", "printers": [{"name": "Z"}]}),
    )
    .unwrap();

    // the old machine waits in its long-poll ...
    let (tx, rx) = mpsc::channel();
    let (h, b) = (http.clone(), base.clone());
    let started = std::time::Instant::now();
    std::thread::spawn(move || {
        tx.send(h.get(&format!("{b}/agent/jobs"), "old-key"))
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(500));
    assert!(rx.try_recv().is_err(), "it is waiting");
    // ... the new machine's first poll takes the device over and wakes it
    let pending = std::thread::spawn({
        let (h, b, k) = (http.clone(), base.clone(), new_key.clone());
        move || h.get(&format!("{b}/agent/jobs"), &k)
    });
    let ended = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the old poll ended");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "woken, not timed out"
    );
    assert!(ended.unwrap_err().to_string().contains("401"));
    // the job that comes now goes to the new machine
    let job = enqueue(&db, dev.pid("Z"), b"x", |_| {});
    state.waker.wake(dev.computer_id);
    let claimed = pending.join().unwrap().unwrap().unwrap();
    assert_eq!(claimed["job_id"], job);
}

#[test]
fn the_single_claim_wakes_the_old_poll_on_a_swap_and_a_plain_poll_wakes_nobody() {
    use std::pin::pin;
    let app = App::new();
    let (dev, new_key, _) = reconnected(&app);
    let notify = app.state.waker.for_agent(dev.computer_id);
    let mut waiting = pin!(notify.notified());
    assert!(!waiting.as_mut().enable());
    // the old agent's own poll swaps nothing and wakes nobody
    assert_eq!(app.agent("GET", "/agent/jobs", "old-key", None).status, 204);
    assert!(!waiting.as_mut().enable());
    // the new key's first poll swaps the device and wakes the agent's waiting polls
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 204);
    assert!(waiting.as_mut().enable(), "woken by the swap");
}

#[test]
fn a_reconnect_key_under_another_name_is_refused_and_the_list_says_so() {
    let app = App::new();
    let (dev, new_key, _) = reconnected(&app);
    // the new machine's agent.ini kept its own name: no second device, no revoke button for the
    // device that still works
    let r = register_http(&app, &new_key, "new-pc");
    assert_eq!(r.status, 401, "{}", r.text());
    assert_eq!(app.agent("GET", "/agent/jobs", "old-key", None).status, 204);
    let (_, list) = app.get("/pairing", Some("t"));
    assert_eq!(list["codes"][0]["state"], "registered");
    assert_eq!(
        app.get("/computers", Some("t")).1["computers"]
            .as_array()
            .map(|a| a.len()),
        Some(1),
        "no second device"
    );
    assert_eq!(list["codes"][0]["computer_id"], dev.computer_id);
}

#[test]
fn the_late_result_of_an_agent_only_old_key_is_accepted_though_that_key_is_revoked() {
    let app = App::new();
    let dev = office(&app);
    let job = enqueue(&app.db, dev.pid("Z"), b"x", |_| {});
    claim(&app.db, dev.computer_id).unwrap();
    // the device's own key is agent-only (what a paired device has): the swap revokes it
    let kid: i64 = app
        .db
        .with(|c| {
            c.query_row("SELECT id FROM api_keys WHERE label='office-pc'", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(
        app.put(
            &format!("/apikeys/{kid}"),
            Some("t"),
            json!({"scope": "agent"})
        )
        .0,
        200
    );
    let (_, made) = make(&app, "t", json!({"computer_id": dev.computer_id}));
    let got = pair(&app, made["code"].as_str().unwrap(), "x").json();
    let new_key = got["key"].as_str().unwrap().to_string();
    assert_eq!(register_http(&app, &new_key, "office-pc").status, 200);
    assert_eq!(app.agent("GET", "/agent/jobs", &new_key, None).status, 204);
    let active: bool = app
        .db
        .with(|c| {
            c.query_row("SELECT active FROM api_keys WHERE id=?", [kid], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert!(!active, "revoked at the swap");

    let path = format!("/agent/jobs/{job}/result");
    let r = app.agent(
        "POST",
        &path,
        "old-key",
        Some(json!({"ok": false, "error": "x"})),
    );
    assert_eq!(r.status, 401);
    let r = app.agent("POST", &path, "old-key", Some(json!({"ok": true})));
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(job_state(&app.db, job), "done");
}

#[test]
fn two_codes_for_one_new_name_give_one_device_and_the_second_key_is_refused() {
    let app = App::new();
    let (c1, c2) = (code(&app), code(&app));
    let k1 = pair(&app, &c1, "new-pc").json()["key"]
        .as_str()
        .unwrap()
        .to_string();
    let k2 = pair(&app, &c2, "new-pc").json()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(register_http(&app, &k1, "new-pc").status, 200);
    let r = register_http(&app, &k2, "new-pc");
    assert_eq!(r.status, 401, "{}", r.text());
    assert_eq!(app.agent("GET", "/agent/jobs", &k1, None).status, 204);
    assert_eq!(app.agent("GET", "/agent/jobs", &k2, None).status, 401);
}

#[test]
fn an_account_name_nobody_can_change_is_cleaned_for_the_confirmation_and_never_refuses() {
    let app = App::new();
    // a phone adds U+FE0F to the coffee cup by itself; the name has no hidden character to clean
    let cafe = org(&app, "Café ☕\u{fe0f}");
    let session = app.account(cafe, "boss@cafe.test");
    let (st, made) = make(&app, &session, json!({}));
    assert_eq!(st, 200, "{made}");
    let r = pair(&app, made["code"].as_str().unwrap(), "pc");
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["org"], "Café ☕\u{fe0f}");

    // too long and with characters that reorder the text: cut and cleaned in the reply
    let acme = org(&app, &format!("{}\u{202e}", "A".repeat(300)));
    let (st, made) = make(&app, "t", json!({"org_id": acme}));
    assert_eq!(st, 200, "{made}");
    let c = made["code"].as_str().unwrap().to_string();
    // names with invisible or control characters never get that far
    assert_eq!(pair(&app, &c, "pc\u{202e}").status, 400);
    assert_eq!(pair(&app, &c, "pc\u{200b}").status, 400);
    let r = pair(&app, &c, "desk \u{2615}\u{fe0f}");
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["org"], "A".repeat(200));
    assert_eq!(r.json()["name"], "desk \u{2615}\u{fe0f}");
}

#[test]
fn a_reconnect_of_a_computer_named_cloudprnt_is_refused_before_the_code_is_spent() {
    let app = App::new();
    // a full key may register such a name (it is documented); a code cannot take it over, because
    // the key it hands out may not register the name
    add_key(&app.db, "cloudprnt-desk", "full-key", DEFAULT_ORG);
    let dev = register(
        &app.db,
        "cloudprnt-desk",
        "full-key",
        &[decl("Z", false)],
        DEFAULT_ORG,
    );
    let (st, body) = make(&app, "t", json!({"computer_id": dev.computer_id}));
    assert_eq!(st, 400, "{body}");
    let why = body["error"].as_str().unwrap();
    assert!(
        why.contains("cloudprnt-") && why.contains("another name"),
        "{why}"
    );
    let (_, list) = app.get("/pairing", Some("t"));
    assert_eq!(list["codes"].as_array().map(Vec::len), Some(0));

    // a code made before the computer got that name is refused at the redeem and stays unspent
    app.db
        .with(|c| {
            c.execute(
                "UPDATE agents SET name='office-pc' WHERE id=?",
                [dev.computer_id],
            )
        })
        .unwrap();
    let (st, made) = make(&app, "t", json!({"computer_id": dev.computer_id}));
    assert_eq!(st, 200, "{made}");
    app.db
        .with(|c| {
            c.execute(
                "UPDATE agents SET name='cloudprnt-desk' WHERE id=?",
                [dev.computer_id],
            )
        })
        .unwrap();
    let r = pair(&app, made["code"].as_str().unwrap(), "ignored");
    assert_eq!(r.status, 400, "{}", r.text());
    let (_, list) = app.get("/pairing", Some("t"));
    assert_eq!(list["codes"][0]["state"], "open", "still unspent");
}

#[test]
fn a_computer_registered_under_a_blank_or_untrimmed_name_gets_no_code_and_keeps_the_key_free() {
    let app = App::new();
    // the register route keeps whatever name it is sent: the agent would read "pc" back, and a
    // reconnect key registers only under the stored name, so the code would be spent for nothing
    for (n, name) in ["", "pc ", " pc2", "pc3\u{a0}"].into_iter().enumerate() {
        let key = format!("blank-key-{n}");
        add_key(&app.db, &format!("owner-{n}"), &key, DEFAULT_ORG);
        let r = register_http(&app, &key, name);
        assert_eq!(r.status, 200, "{name:?}: {}", r.text());
        let id = r.json()["computer_id"].as_i64().unwrap();
        let (st, body) = make(&app, "t", json!({"computer_id": id}));
        assert_eq!(st, 400, "{name:?}: {body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("empty or has a space"),
            "{body}"
        );
        // a code made before the name went wrong is refused at the redeem and stays unspent
        app.db
            .with(|c| c.execute("UPDATE agents SET name='fine' WHERE id=?", [id]))
            .unwrap();
        let (st, made) = make(&app, "t", json!({"computer_id": id}));
        assert_eq!(st, 200, "{made}");
        app.db
            .with(|c| {
                c.execute(
                    "UPDATE agents SET name=? WHERE id=?",
                    rusqlite::params![name, id],
                )
            })
            .unwrap();
        let r = pair(&app, made["code"].as_str().unwrap(), "ignored");
        assert_eq!(r.status, 400, "{name:?}: {}", r.text());
        let (_, list) = app.get("/pairing", Some("t"));
        assert_eq!(list["codes"][0]["state"], "open", "{name:?}: still unspent");
        let id = made["id"].as_i64().unwrap();
        assert_eq!(app.delete(&format!("/pairing/{id}"), Some("t")).0, 200);
    }
}
