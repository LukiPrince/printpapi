// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Org accounts: e-mail/password login, browser sessions, and what a session may manage — the port
//! of `tests/test_accounts.py`.
mod common;

use common::*;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};

fn labels(v: &Value, key: &str, field: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x[field].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn login_returns_a_session_that_acts_inside_its_own_org() {
    let app = App::new();
    register(&app.db, "pc", "agentkey", &[decl("Z", false)], DEFAULT_ORG);
    let (code, body) = app.post(
        "/orgs/1/users",
        Some("t"),
        json!({"email": "Ops@Shop.test", "password": PW}),
    );
    assert!(code == 200 && body["email"] == "ops@shop.test");
    let (code, login) = app.login("ops@shop.test", PW);
    assert_eq!(code, 200);
    assert!(
        login["token"].as_str().unwrap().starts_with("sess_") && login["org_id"] == DEFAULT_ORG
    );
    assert!(login["expires_at"].as_f64().unwrap() > 0.0 && !login.to_string().contains("password"));
    let keys: Vec<&String> = login.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["token", "expires_at", "org_id", "user_id", "email"]);
    let sess = login["token"].as_str().unwrap();
    let (code, printers) = app.get("/printers", Some(sess));
    assert!(code == 200 && printers["printers"][0]["name"] == "Z");
    let (code, me) = app.get("/me", Some(sess));
    assert_eq!(code, 200);
    assert_eq!(
        me,
        json!({"kind": "session", "org_id": DEFAULT_ORG, "email": "ops@shop.test", "user_id": login["user_id"]})
    );
}

#[test]
fn a_bad_password_unknown_user_and_junk_body_are_all_401_or_400() {
    let app = App::new();
    app.default_account();
    assert_eq!(app.login("ops@shop.test", "wrong password").0, 401);
    assert_eq!(app.login("nobody@shop.test", PW).0, 401); // no user-enumeration difference
    assert_eq!(
        app.post("/login", None, json!({"email": "ops@shop.test"}))
            .0,
        401
    );
    assert_eq!(app.post("/login", None, json!({})).0, 401);
    // not strings at all: the Python crashed, this is a plain 401
    assert_eq!(
        app.post(
            "/login",
            None,
            json!({"email": "ops@shop.test", "password": 12345678})
        )
        .0,
        401
    );
    assert_eq!(
        app.post("/login", None, json!({"email": ["x"], "password": PW}))
            .0,
        401
    );
    assert_eq!(app.call("POST", "/login").raw(b"[1]").send().status, 400);
}

#[test]
fn repeated_failures_are_throttled_per_email() {
    let app = App::with(|c| c.max_login_fails = 3);
    app.default_account();
    app.account(DEFAULT_ORG, "other@shop.test");
    for _ in 0..3 {
        assert_eq!(app.login("ops@shop.test", "wrong password").0, 401);
    }
    assert_eq!(app.login("ops@shop.test", "wrong password").0, 429);
    assert_eq!(app.login("ops@shop.test", PW).0, 429); // even the right password waits
    assert_eq!(app.login("other@shop.test", PW).0, 200); // other accounts unaffected
}

#[test]
fn a_successful_login_clears_the_failure_count() {
    let app = App::with(|c| c.max_login_fails = 3);
    app.default_account();
    assert_eq!(app.login("ops@shop.test", "wrong password").0, 401);
    assert_eq!(app.login("ops@shop.test", "wrong password").0, 401);
    assert_eq!(app.login("ops@shop.test", PW).0, 200);
    assert_eq!(app.login("ops@shop.test", "wrong password").0, 401);
    assert_eq!(app.login("ops@shop.test", PW).0, 200); // counter reset by the success
}

#[test]
fn an_expired_session_stops_authorizing() {
    let app = App::with(|c| c.session_ttl_s = 0.0);
    let sess = app.default_account();
    assert_eq!(app.get("/printers", Some(&sess)).0, 401);
}

#[test]
fn logout_kills_the_session() {
    let app = App::new();
    let sess = app.default_account();
    assert_eq!(
        app.post("/logout", Some(&sess), json!({})),
        (200, json!({"ok": true}))
    );
    assert_eq!(app.get("/printers", Some(&sess)).0, 401);
    assert_eq!(app.post("/logout", Some(&sess), json!({})).0, 401);
    // only a session logs out: root and machine keys have nothing to end
    assert_eq!(app.post("/logout", Some("t"), json!({})).0, 401);
}

#[test]
fn me_tells_root_session_and_machine_key_apart() {
    let app = App::new();
    let (code, key) = app.post("/apikeys", Some("t"), json!({"label": "n8n"}));
    assert_eq!(code, 200);
    assert_eq!(
        app.get("/me", Some("t")).1,
        json!({"kind": "root", "org_id": null})
    );
    assert_eq!(
        app.get("/me", Some(key["key"].as_str().unwrap())).1,
        json!({"kind": "key", "org_id": DEFAULT_ORG})
    );
    assert_eq!(app.get("/me", Some("nope")).0, 401);
    // the Bearer prefix is case-sensitive, as before
    let r = app
        .call("GET", "/me")
        .header("authorization", "bearer t")
        .send();
    assert_eq!(r.status, 401);
    // a token with a non-ASCII byte is a plain 401, not a crash
    let r = app
        .call("GET", "/me")
        .header("authorization", "Bearer t\u{e9}")
        .send();
    assert_eq!(r.status, 401);
}

#[test]
fn a_session_manages_only_its_own_orgs_keys() {
    let app = App::new();
    let (code, other) = app.post("/orgs", Some("t"), json!({"name": "other"}));
    assert_eq!(code, 200);
    let (_, foreign) = app.post(
        "/apikeys",
        Some("t"),
        json!({"label": "theirs", "org_id": other["id"]}),
    );
    let sess = app.default_account();
    let (code, mine) = app.post("/apikeys", Some(&sess), json!({"label": "mine"}));
    assert!(
        code == 200 && mine["org_id"] == DEFAULT_ORG && !mine["key"].as_str().unwrap().is_empty()
    );
    let (code, listing) = app.get("/apikeys", Some(&sess));
    assert!(code == 200 && labels(&listing, "keys", "label") == ["mine"]);
    // a foreign key is invisible and unrevokable — 404, never 403
    assert_eq!(
        app.delete(&format!("/apikeys/{}", foreign["id"]), Some(&sess))
            .0,
        404
    );
    assert_eq!(
        app.delete(&format!("/apikeys/{}", mine["id"]), Some(&sess))
            .0,
        200
    );
    assert_eq!(
        app.post(
            "/apikeys",
            Some(&sess),
            json!({"label": "x", "org_id": other["id"]})
        )
        .0,
        400
    );
    // an org id spelled as a string is not "its own org" to a session either
    assert_eq!(
        app.post(
            "/apikeys",
            Some(&sess),
            json!({"label": "x", "org_id": "1"})
        )
        .0,
        400
    );
    // root still sees every org
    assert_eq!(
        app.get("/apikeys", Some("t")).1["keys"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_machine_key_can_print_but_cannot_manage_the_org() {
    let app = App::new();
    let (_, key) = app.post("/apikeys", Some("t"), json!({"label": "n8n"}));
    let k = key["key"].as_str().unwrap();
    assert_eq!(app.get("/printers", Some(k)).0, 200); // printing path intact
    assert_eq!(app.get("/apikeys", Some(k)).0, 401);
    assert_eq!(
        app.post("/apikeys", Some(k), json!({"label": "self-issued"}))
            .0,
        401
    );
    assert_eq!(
        app.delete(&format!("/apikeys/{}", key["id"]), Some(k)).0,
        401
    );
    assert_eq!(app.get("/users", Some(k)).0, 401);
    assert_eq!(
        app.post("/users", Some(k), json!({"email": "x@y.z", "password": PW}))
            .0,
        401
    );
    assert_eq!(
        app.put(
            "/orgs/1",
            Some(k),
            json!({"event_url": "https://h.example/x"})
        )
        .0,
        401
    );
    assert_eq!(
        app.put(
            "/me/password",
            Some(k),
            json!({"current": PW, "new": "another password"})
        )
        .0,
        401
    );
}

#[test]
fn a_session_invites_further_users_into_its_own_org() {
    let app = App::new();
    let sess = app.default_account();
    let (code, u) = app.post(
        "/users",
        Some(&sess),
        json!({"email": "packer@shop.test", "password": PW}),
    );
    assert!(code == 200 && u["org_id"] == DEFAULT_ORG);
    let (code, listing) = app.get("/users", Some(&sess));
    assert!(
        code == 200 && labels(&listing, "users", "email") == ["ops@shop.test", "packer@shop.test"]
    );
    assert!(!listing.to_string().contains("password_hash"));
    assert_eq!(app.login("packer@shop.test", PW).0, 200);
    // taken e-mail, weak password, missing fields
    assert_eq!(
        app.post(
            "/users",
            Some(&sess),
            json!({"email": "packer@shop.test", "password": PW})
        )
        .0,
        409
    );
    assert_eq!(
        app.post(
            "/users",
            Some(&sess),
            json!({"email": "new@shop.test", "password": "short"})
        )
        .0,
        400
    );
    assert_eq!(
        app.post("/users", Some(&sess), json!({"email": "new@shop.test"}))
            .0,
        400
    );
    assert_eq!(
        app.post("/users", Some(&sess), json!({"password": PW})).0,
        400
    );
}

#[test]
fn root_creates_the_first_user_of_any_org_but_has_no_org_of_its_own() {
    let app = App::new();
    let (_, other) = app.post("/orgs", Some("t"), json!({"name": "other"}));
    let (code, u) = app.post(
        &format!("/orgs/{}/users", other["id"]),
        Some("t"),
        json!({"email": "them@other.test", "password": PW}),
    );
    assert!(code == 200 && u["org_id"] == other["id"]);
    assert_eq!(
        app.post(
            "/orgs/4242/users",
            Some("t"),
            json!({"email": "x@y.z", "password": PW})
        )
        .0,
        404
    );
    let (code, body) = app.post(
        "/users",
        Some("t"),
        json!({"email": "x@y.z", "password": PW}),
    );
    assert_eq!(code, 400); // root belongs to no org
    assert_eq!(body["error"], "root has no org; use POST /orgs/{id}/users");
    assert_eq!(
        app.get("/users", Some("t")).1["users"]
            .as_array()
            .unwrap()
            .len(),
        1
    ); // every org
    let (code, login) = app.login("them@other.test", PW);
    assert!(code == 200 && login["org_id"] == other["id"]);
}

#[test]
fn a_session_of_one_org_cannot_touch_another_orgs_data() {
    let app = App::new();
    let (_, other) = app.post("/orgs", Some("t"), json!({"name": "other"}));
    let oid = other["id"].as_i64().unwrap();
    let reg = register(&app.db, "theirs", "ak", &[decl("Z", false)], oid);
    let foreign_job = enqueue(&app.db, reg.pid("Z"), b"x", |j| j.org_id = Some(oid));
    app.post(
        &format!("/orgs/{oid}/users"),
        Some("t"),
        json!({"email": "them@other.test", "password": PW}),
    );
    let sess = app.default_account();
    assert_eq!(app.get(&format!("/jobs/{foreign_job}"), Some(&sess)).0, 404);
    assert_eq!(app.get("/printers", Some(&sess)).1["printers"], json!([]));
    assert_eq!(app.get("/computers", Some(&sess)).1["computers"], json!([]));
    assert_eq!(
        app.put(
            &format!("/orgs/{oid}"),
            Some(&sess),
            json!({"event_url": "https://evil.example/x"})
        )
        .0,
        404
    );
    // the other org's user stays invisible
    assert_eq!(
        labels(&app.get("/users", Some(&sess)).1, "users", "email"),
        ["ops@shop.test"]
    );
}

#[test]
fn a_session_configures_its_own_org() {
    let app = App::new();
    let sess = app.default_account();
    let (code, body) = app.put(
        "/orgs/1",
        Some(&sess),
        json!({"event_url": "https://hooks.example/x", "shopify_secret": "s3cr3t"}),
    );
    assert!(code == 200 && body["event_url"] == "https://hooks.example/x");
    assert!(body["shopify_secret_set"] == true && !body.to_string().contains("s3cr3t"));
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["ok", "event_url", "shopify_secret_set"]);
    let org = app
        .db
        .with(|c| store::get_org(c, DEFAULT_ORG))
        .unwrap()
        .unwrap();
    assert_eq!(org.shopify_secret.as_deref(), Some("s3cr3t"));
}

#[test]
fn a_password_change_needs_the_old_one_and_logs_the_browsers_out() {
    let app = App::new();
    let sess = app.default_account();
    assert_eq!(
        app.put(
            "/me/password",
            Some(&sess),
            json!({"current": "wrong password", "new": "brand new password"})
        )
        .0,
        401
    );
    assert_eq!(
        app.put(
            "/me/password",
            Some(&sess),
            json!({"current": PW, "new": "short"})
        )
        .0,
        400
    );
    assert_eq!(
        app.put(
            "/me/password",
            Some(&sess),
            json!({"current": PW, "new": "brand new password"})
        ),
        (200, json!({"ok": true}))
    );
    assert_eq!(app.get("/printers", Some(&sess)).0, 401); // old session invalidated
    assert_eq!(app.login("ops@shop.test", PW).0, 401); // old password gone
    assert_eq!(app.login("ops@shop.test", "brand new password").0, 200);
}

#[test]
fn a_job_submitted_from_a_session_records_who_printed_it() {
    let app = App::new();
    let reg = agent_z(&app.db);
    let sess = app.default_account();
    let (_, login) = app.login("ops@shop.test", PW);
    let (code, job) = app.post("/jobs", Some(&sess), job_body(reg.pid("Z")));
    assert_eq!(code, 200);
    let uid = sql_i64(
        &app.db,
        "SELECT user_id FROM jobs WHERE id=?",
        job["job_id"].as_i64().unwrap(),
    );
    assert_eq!(uid, login["user_id"].as_i64());
}

#[test]
fn deleting_an_org_leaves_its_bytes_neither_in_the_db_nor_in_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.db");
    let app = App::on(store::Db::open(path.to_str().unwrap()).unwrap(), |_| {});
    let oid = create_org(&app.db, "gone");
    let reg = register(&app.db, "pc", "gk", &[decl("Z", false)], oid);
    let marker = b"ORG-DELETE-MARKER-31;";
    enqueue(&app.db, reg.pid("Z"), &marker.repeat(1000), |j| {
        j.org_id = Some(oid)
    });
    assert_eq!(app.delete(&format!("/orgs/{oid}"), Some("t")).0, 200);
    for f in ["d.db", "d.db-wal"] {
        let bytes = std::fs::read(dir.path().join(f)).unwrap_or_default();
        assert!(!bytes.windows(marker.len()).any(|w| w == marker), "{f}");
    }
}
