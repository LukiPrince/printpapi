// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Key scopes over HTTP: the rights table on every machine surface, own jobs of a print key, a job
//! already handed out, changing a scope and the targeted wake. No mocks: the real routes over an
//! in-memory database.
mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::*;
use hmac::{Hmac, KeyInit, Mac};
use printpapi_server::auth::Scope;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};
use sha2::Sha256;
use std::pin::pin;

const WORDS: &str = "scope must be one of: full, agent, cloudprnt, print, read";

fn issue(app: &App, label: &str, key: &str, scope: Scope) -> i64 {
    app.db
        .with(|c| store::add_api_key_scoped(c, label, key, DEFAULT_ORG, scope))
        .unwrap()
}

/// Writes any word into the column, as a newer server would.
fn force_scope(app: &App, key_id: i64, word: &str) {
    app.db.with(|c| {
        c.execute(
            "UPDATE api_keys SET scope=? WHERE id=?",
            rusqlite::params![word, key_id],
        )
        .unwrap()
    });
}

/// One key per scope word, plus one whose stored scope this version does not know.
fn six_keys(app: &App) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (word, scope) in [
        ("full", Scope::Full),
        ("agent", Scope::Agent),
        ("cloudprnt", Scope::CloudPrnt),
        ("print", Scope::Print),
        ("read", Scope::Read),
    ] {
        let key = format!("k-{word}");
        issue(app, word, &key, scope);
        out.push((word, key));
    }
    let kid = issue(app, "unknown", "k-unknown", Scope::Full);
    force_scope(app, kid, "martian");
    out.push(("unknown", "k-unknown".to_string()));
    out
}

fn denied_text(scope: &str) -> String {
    format!("this key's rights ({scope}) do not allow this")
}

fn register_http(app: &App, key: &str, name: &str) -> Reply {
    app.agent(
        "POST",
        "/agent/register",
        key,
        Some(json!({"name": name, "printers": [{"name": "Z"}]})),
    )
}

fn printer_of(reply: &Reply) -> i64 {
    reply.json()["printer_ids"]["Z"].as_i64().unwrap()
}

fn agents_in_db(app: &App) -> i64 {
    app.db
        .with(|c| c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)))
        .unwrap()
}

// --- POST /apikeys, GET /apikeys, PUT /apikeys/{id}, GET /me ------------------------------------

#[test]
fn issuing_without_a_scope_answers_exactly_as_before() {
    let app = App::new();
    for body in [
        json!({"label": "n8n"}),
        json!({"label": "n8n", "scope": null}),
    ] {
        let (code, b) = app.post("/apikeys", Some("t"), body);
        assert_eq!(code, 200);
        let fields: Vec<_> = b.as_object().unwrap().keys().cloned().collect();
        assert_eq!(fields, ["id", "label", "org_id", "key"]);
    }
    // an explicit full is omitted too
    let (_, b) = app.post(
        "/apikeys",
        Some("t"),
        json!({"label": "x", "scope": "full"}),
    );
    assert!(b.get("scope").is_none());
    let (_, list) = app.get("/apikeys", Some("t"));
    for k in list["keys"].as_array().unwrap() {
        let fields: Vec<_> = k.as_object().unwrap().keys().cloned().collect();
        assert_eq!(fields, ["id", "org_id", "label", "active", "created_at"]);
    }
}

#[test]
fn every_scope_word_is_echoed_when_it_is_not_full() {
    let app = App::new();
    for word in ["agent", "cloudprnt", "print", "read"] {
        let (code, b) = app.post("/apikeys", Some("t"), json!({"label": word, "scope": word}));
        assert_eq!(code, 200, "{word}");
        let fields: Vec<_> = b.as_object().unwrap().keys().cloned().collect();
        assert_eq!(fields, ["id", "label", "org_id", "key", "scope"]);
        assert_eq!(b["scope"], word);
        // the key works and says what it is
        let key = b["key"].as_str().unwrap();
        assert_eq!(app.get("/me", Some(key)).1["scope"], word);
    }
    let (_, list) = app.get("/apikeys", Some("t"));
    let scopes: Vec<_> = list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["scope"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(scopes, ["agent", "cloudprnt", "print", "read"]);
    assert_eq!(
        list["keys"][0]
            .as_object()
            .unwrap()
            .keys()
            .next_back()
            .unwrap(),
        "scope"
    );
}

#[test]
fn a_full_key_says_nothing_about_scope_in_me() {
    let app = App::new();
    add_key(&app.db, "n8n", "kf", DEFAULT_ORG);
    let (code, me) = app.get("/me", Some("kf"));
    assert_eq!(code, 200);
    assert_eq!(me, json!({"kind": "key", "org_id": DEFAULT_ORG}));
}

#[test]
fn a_bad_scope_is_a_400_and_issues_nothing() {
    let app = App::new();
    for bad in [
        json!(""),
        json!("admin"),
        json!("Full"),
        json!(5),
        json!([]),
    ] {
        let (code, b) = app.post("/apikeys", Some("t"), json!({"label": "x", "scope": bad}));
        assert_eq!((code, &b), (400, &json!({ "error": WORDS })), "{bad}");
    }
    assert_eq!(app.get("/apikeys", Some("t")).1["keys"], json!([]));
    // after the label check, before the org check
    let (_, b) = app.post("/apikeys", Some("t"), json!({"label": 5, "scope": "x"}));
    assert_eq!(b["error"], "label must be a string");
    let (_, b) = app.post("/apikeys", Some("t"), json!({"scope": "x", "org_id": 999}));
    assert_eq!(b["error"], WORDS);
}

#[test]
fn sessions_and_root_issue_scoped_keys_and_a_key_may_not() {
    let app = App::new();
    let session = app.default_account();
    let (code, b) = app.post("/apikeys", Some(&session), json!({"scope": "print"}));
    assert_eq!((code, b["scope"].clone()), (200, json!("print")));
    add_key(&app.db, "n8n", "kf", DEFAULT_ORG);
    let (code, _) = app.post("/apikeys", Some("kf"), json!({"scope": "print"}));
    assert_eq!(code, 401);
}

#[test]
fn put_apikeys_changes_the_scope_and_answers_ok_and_scope() {
    let app = App::new();
    let kid = issue(&app, "shop", "ks", Scope::Full);
    let (code, b) = app.put(
        &format!("/apikeys/{kid}"),
        Some("t"),
        json!({"scope": "print"}),
    );
    assert_eq!((code, b), (200, json!({"ok": true, "scope": "print"})));
    assert_eq!(app.get("/me", Some("ks")).1["scope"], "print");
    // back to full: the answer still carries the scope, /me does not
    let (_, b) = app.put(
        &format!("/apikeys/{kid}"),
        Some("t"),
        json!({"scope": "full"}),
    );
    assert_eq!(b, json!({"ok": true, "scope": "full"}));
    assert!(app.get("/me", Some("ks")).1.get("scope").is_none());
    // a session of the org may too
    let session = app.default_account();
    let (code, _) = app.put(
        &format!("/apikeys/{kid}"),
        Some(&session),
        json!({"scope": "read"}),
    );
    assert_eq!(code, 200);
}

#[test]
fn put_apikeys_refuses_a_bad_scope_and_unknown_revoked_or_foreign_keys() {
    let app = App::new();
    let kid = issue(&app, "shop", "ks", Scope::Full);
    let path = format!("/apikeys/{kid}");
    for body in [
        json!({}),
        json!({"scope": null}),
        json!({"scope": "Full"}),
        json!({"scope": 3}),
    ] {
        let (code, b) = app.put(&path, Some("t"), body.clone());
        assert_eq!((code, &b), (400, &json!({ "error": WORDS })), "{body}");
    }
    assert_eq!(app.get("/me", Some("ks")).1["kind"], "key");
    assert!(app.get("/me", Some("ks")).1.get("scope").is_none());
    assert_eq!(
        app.put("/apikeys/9999", Some("t"), json!({"scope": "read"}))
            .0,
        404
    );
    // another org's session reads the key as missing
    let other = create_org(&app.db, "acme");
    let theirs = app.account(other, "boss@acme.test");
    assert_eq!(
        app.put(&path, Some(&theirs), json!({"scope": "read"})).0,
        404
    );
    assert!(app.get("/me", Some("ks")).1.get("scope").is_none());
    // a machine key (even a full one) is no manager
    assert_eq!(app.put(&path, Some("ks"), json!({"scope": "read"})).0, 401);
    assert_eq!(app.put(&path, None, json!({"scope": "read"})).0, 401);
    // revoked
    assert_eq!(app.delete(&path, Some("t")).0, 200);
    assert_eq!(app.put(&path, Some("t"), json!({"scope": "read"})).0, 404);
}

// --- the rights table ---------------------------------------------------------------------------

#[test]
fn every_client_route_follows_the_rights_table() {
    let app = App::new();
    let keys = six_keys(&app);
    const ALL: &[&str] = &["full", "agent", "cloudprnt", "print", "read", "unknown"];
    let rows: &[(&str, &str, &[&str])] = &[
        ("GET", "/me", ALL),
        ("GET", "/computers", &["full", "agent", "print", "read"]),
        ("GET", "/printers", &["full", "print", "read"]),
        ("GET", "/labels", &["full", "print", "read"]),
        ("GET", "/labels/1", &["full", "print", "read"]),
        ("GET", "/jobs", &["full", "print", "read"]),
        ("GET", "/jobs/1", &["full", "print", "read"]),
        ("GET", "/metrics", &["full", "print", "read"]),
        ("GET", "/plans", &["full", "print", "read"]),
        (
            "GET",
            "/printers/1/receipt-profile",
            &["full", "print", "read"],
        ),
        ("POST", "/jobs", &["full", "print"]),
        ("POST", "/receipts/preview", &["full", "print"]),
        ("POST", "/orders", &["full", "print"]),
        ("DELETE", "/jobs/1", &["full", "print"]),
        ("POST", "/labels", &["full"]),
        ("PUT", "/labels/1", &["full"]),
        ("DELETE", "/labels/1", &["full"]),
        ("PUT", "/printers/1/receipt-profile", &["full"]),
        ("DELETE", "/printers/1/receipt-profile", &["full"]),
    ];
    for (method, path, allowed) in rows {
        for (word, key) in &keys {
            let r = app.call(method, path).bearer(key).json(&json!({})).send();
            if allowed.contains(word) {
                assert!(
                    r.status != 401 && r.status != 403,
                    "{method} {path} with {word}: {}",
                    r.status
                );
            } else {
                assert_eq!(r.status, 403, "{method} {path} with {word}");
                assert_eq!(r.json(), json!({ "error": denied_text(word) }));
            }
        }
    }
    // managing keys, users, the org and the support log stays out of reach of every key
    for (method, path) in [
        ("GET", "/apikeys"),
        ("POST", "/apikeys"),
        ("PUT", "/apikeys/1"),
        ("DELETE", "/apikeys/1"),
        ("GET", "/users"),
        ("POST", "/users"),
        ("GET", "/orgs/1"),
        ("PUT", "/orgs/1"),
        ("GET", "/support-access"),
        ("GET", "/jobs/1/preview"),
    ] {
        for (word, key) in &keys {
            let r = app
                .call(method, path)
                .bearer(key)
                .json(&json!({"scope": "full"}))
                .send();
            assert_eq!(r.status, 401, "{method} {path} with {word}");
        }
    }
}

#[test]
fn the_right_is_checked_before_the_body_is_read() {
    let app = App::new();
    let keys = six_keys(&app);
    let key = |w: &str| keys.iter().find(|(x, _)| *x == w).unwrap().1.clone();
    // a denied key with a broken body is 403, not 400
    for (method, path, word) in [
        ("POST", "/jobs", "read"),
        ("POST", "/jobs", "agent"),
        ("POST", "/orders", "read"),
        ("POST", "/labels", "print"),
        ("PUT", "/labels/1", "print"),
        ("POST", "/receipts/preview", "read"),
        ("PUT", "/printers/1/receipt-profile", "print"),
    ] {
        let r = app
            .call(method, path)
            .bearer(&key(word))
            .raw(b"{broken")
            .send();
        assert_eq!(r.status, 403, "{method} {path} {word}");
        assert_eq!(r.json()["error"], denied_text(word));
    }
    // and on the PrintNode layer
    let r = app
        .call("POST", "/printjobs")
        .basic(&key("read"))
        .raw(b"{broken")
        .send();
    assert_eq!(r.status, 403);
    assert_eq!(r.json()["code"], "Forbidden");
    // an allowed key still reaches the body check
    let r = app
        .call("POST", "/jobs")
        .bearer(&key("print"))
        .raw(b"{broken")
        .send();
    assert_eq!(r.status, 400);
}

// --- print keys see only their own jobs ---------------------------------------------------------

fn org_with_agent(app: &App) -> (Registered, i64) {
    let reg = register(
        &app.db,
        "pc",
        "agent-key",
        &[decl("Z", false), decl("HP", true)],
        DEFAULT_ORG,
    );
    let z = reg.pid("Z");
    (reg, z)
}

fn job_ids(v: &Value) -> Vec<i64> {
    v["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_i64().unwrap())
        .collect()
}

#[test]
fn a_print_key_sees_and_cancels_only_its_own_jobs() {
    let app = App::new();
    let (reg, z) = org_with_agent(&app);
    issue(&app, "p1", "p1", Scope::Print);
    issue(&app, "p2", "p2", Scope::Print);
    issue(&app, "ro", "ro", Scope::Read);
    issue(&app, "fu", "fu", Scope::Full);
    // p2's job is claimed already when p1 asks about it
    let b = job_id(&app.post("/jobs", Some("p2"), job_body(z)).1);
    claim(&app.db, reg.computer_id).unwrap();
    let a = job_id(&app.post("/jobs", Some("p1"), job_body(z)).1);
    let c = job_id(&app.post("/jobs", Some("fu"), job_body(z)).1);

    assert_eq!(app.get(&format!("/jobs/{a}"), Some("p1")).0, 200);
    assert_eq!(app.get(&format!("/jobs/{b}"), Some("p1")).0, 404);
    assert_eq!(app.get(&format!("/jobs/{c}"), Some("p1")).0, 404);
    // 404 before any 409: the claimed job of another key is not even there
    assert_eq!(app.delete(&format!("/jobs/{b}"), Some("p1")).0, 404);
    assert_eq!(app.delete(&format!("/jobs/{c}"), Some("p1")).0, 404);
    assert_eq!(job_state(&app.db, c), "queued");
    assert_eq!(job_ids(&app.get("/jobs", Some("p1")).1), [a]);
    assert_eq!(job_ids(&app.get("/jobs", Some("p2")).1), [b]);
    // read, full and root see the whole org
    for who in ["ro", "fu", "t"] {
        assert_eq!(job_ids(&app.get("/jobs", Some(who)).1), [c, a, b], "{who}");
        assert_eq!(app.get(&format!("/jobs/{b}"), Some(who)).0, 200, "{who}");
    }
    // its own job it may cancel
    let (code, body) = app.delete(&format!("/jobs/{a}"), Some("p1"));
    assert_eq!(
        (code, body),
        (200, json!({"ok": true, "state": "cancelled"}))
    );
    // org-wide counts stay visible
    assert_eq!(app.call("GET", "/metrics").bearer("p1").send().status, 200);
}

#[test]
fn a_print_keys_orders_and_shopify_jobs_count_as_its_own() {
    let app = App::new();
    let (reg, _) = org_with_agent(&app);
    let hp = reg.pid("HP");
    issue(&app, "p1", "p1", Scope::Print);
    issue(&app, "p2", "p2", Scope::Print);
    let order = json!({"number": "1001", "customer": "Jane", "address": ["Musterweg 1"],
        "lines": [{"qty": 1, "sku": "A", "name": "Widget", "total": "9.90"}],
        "totals": [["Total", "9.90"]]});
    let (code, b) = app.post(
        "/orders",
        Some("p1"),
        json!({"printer_id": hp, "order": order, "format": null}),
    );
    assert_eq!(code, 200, "{b}");
    let by_order = job_id(&b);
    // Shopify: the key rides in the URL, authenticity in the HMAC
    app.db
        .with(|c| store::set_org_shopify_secret(c, DEFAULT_ORG, Some("s3cret")))
        .unwrap();
    let body = json!({"id": 99, "name": "#99", "line_items": [{"quantity": 1, "title": "W", "price": "1.00"}]});
    let data = serde_json::to_vec(&body).unwrap();
    let sign = |secret: &str| {
        B64.encode(
            Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .unwrap()
                .chain_update(&data)
                .finalize()
                .into_bytes(),
        )
    };
    let hook = |key: &str, sig: &str| {
        app.call(
            "POST",
            &format!("/integrations/shopify/orders?key={key}&printer_id={hp}"),
        )
        .header("x-shopify-hmac-sha256", sig)
        .header("x-shopify-shop-domain", "acme.myshopify.com")
        .raw(&data)
        .send()
    };
    let r = hook("p1", &sign("s3cret"));
    assert_eq!(r.status, 200, "{}", r.text());
    let by_shopify = r.json()["job_id"].as_i64().unwrap();
    assert_eq!(
        job_ids(&app.get("/jobs", Some("p1")).1),
        [by_shopify, by_order]
    );
    assert!(job_ids(&app.get("/jobs", Some("p2")).1).is_empty());
}

#[test]
fn an_idempotent_resubmit_returns_the_orgs_job_even_if_another_key_made_it() {
    let app = App::new();
    let (_, z) = org_with_agent(&app);
    issue(&app, "p1", "p1", Scope::Print);
    issue(&app, "fu", "fu", Scope::Full);
    let mut body = job_body(z);
    body["idempotency_key"] = json!("same-order");
    let first = job_id(&app.post("/jobs", Some("fu"), body.clone()).1);
    let again = job_id(&app.post("/jobs", Some("p1"), body).1);
    assert_eq!(first, again);
    assert_eq!(app.get(&format!("/jobs/{again}"), Some("p1")).0, 404);
    assert!(job_ids(&app.get("/jobs", Some("p1")).1).is_empty());
}

#[test]
fn a_full_keys_jobs_stay_visible_to_it_after_it_is_narrowed_to_print() {
    let app = App::new();
    let (reg, z) = org_with_agent(&app);
    let hp = reg.pid("HP");
    let kid = issue(&app, "shop", "shop", Scope::Full);
    let by_job = job_id(&app.post("/jobs", Some("shop"), job_body(z)).1);
    let order = json!({"number": "1001", "customer": "Jane", "address": ["Musterweg 1"],
        "lines": [{"qty": 1, "sku": "A", "name": "Widget", "total": "9.90"}],
        "totals": [["Total", "9.90"]]});
    let (code, b) = app.post(
        "/orders",
        Some("shop"),
        json!({"printer_id": hp, "order": order, "format": null}),
    );
    assert_eq!(code, 200, "{b}");
    let by_order = job_id(&b);
    // a job of another key stays out of its view
    issue(&app, "fu", "fu", Scope::Full);
    let other = job_id(&app.post("/jobs", Some("fu"), job_body(z)).1);
    let (code, _) = app.put(
        &format!("/apikeys/{kid}"),
        Some("t"),
        json!({"scope": "print"}),
    );
    assert_eq!(code, 200);
    assert_eq!(
        job_ids(&app.get("/jobs", Some("shop")).1),
        [by_order, by_job]
    );
    assert_eq!(app.get(&format!("/jobs/{by_job}"), Some("shop")).0, 200);
    assert_eq!(app.get(&format!("/jobs/{by_order}"), Some("shop")).0, 200);
    assert_eq!(app.get(&format!("/jobs/{other}"), Some("shop")).0, 404);
}

#[test]
fn a_print_key_cannot_occupy_the_idempotency_key_of_the_shopify_webhook() {
    let app = App::new();
    let (reg, z) = org_with_agent(&app);
    let hp = reg.pid("HP");
    issue(&app, "p1", "p1", Scope::Print);
    issue(&app, "fu", "fu", Scope::Full);
    let reserved = "shopify-5001";
    let mut body = job_body(z);
    body["idempotency_key"] = json!(reserved);
    let (code, b) = app.post("/jobs", Some("p1"), body.clone());
    assert_eq!(code, 400, "{b}");
    assert_eq!(
        b["error"],
        "idempotency_key may not start with shopify-: the Shopify webhook uses that prefix"
    );
    let order = json!({"number": "1", "customer": "J", "address": ["x"],
        "lines": [{"qty": 1, "sku": "A", "name": "W", "total": "1.00"}], "totals": []});
    let (code, b) = app.post(
        "/orders",
        Some("p1"),
        json!({"printer_id": hp, "order": order, "idempotency_key": reserved}),
    );
    assert_eq!(code, 400, "{b}");
    // other keys of its own are fine, and nothing was queued by the refusals
    body["idempotency_key"] = json!("woo-5001-processing");
    assert_eq!(app.post("/jobs", Some("p1"), body.clone()).0, 200);
    assert_eq!(job_ids(&app.get("/jobs", Some("t")).1).len(), 1);
    // a key that is trusted with everything may, as before
    body["idempotency_key"] = json!(reserved);
    assert_eq!(app.post("/jobs", Some("fu"), body).0, 200);
}

// --- the PrintNode layer ------------------------------------------------------------------------

#[test]
fn printnode_answers_a_missing_right_with_403_forbidden() {
    let app = App::new();
    let keys = six_keys(&app);
    let key = |w: &str| keys.iter().find(|(x, _)| *x == w).unwrap().1.clone();
    let (_, z) = org_with_agent(&app);
    for word in ["agent", "cloudprnt", "unknown"] {
        for path in ["/whoami", "/computers", "/printers", "/printjobs"] {
            let (code, b) = app.pn("GET", path, &key(word), None);
            assert_eq!(code, 403, "{word} {path}");
            assert_eq!(
                b,
                json!({"code": "Forbidden", "message": denied_text(word)}),
                "{word} {path}"
            );
        }
    }
    // a read key looks but does not touch
    for path in ["/whoami", "/computers", "/printers", "/printjobs"] {
        assert_eq!(app.pn("GET", path, &key("read"), None).0, 200, "{path}");
    }
    let post = json!({"printerId": z, "contentType": "raw_base64", "content": "QUJD"});
    assert_eq!(
        app.pn("POST", "/printjobs", &key("read"), Some(post.clone()))
            .0,
        403
    );
    assert_eq!(app.pn("DELETE", "/printjobs/1", &key("read"), None).0, 403);
    // unknown key stays 401
    assert_eq!(app.pn("GET", "/whoami", "nope", None).0, 401);
}

#[test]
fn a_print_key_on_the_printnode_layer_sees_only_its_own_jobs() {
    let app = App::new();
    let (_, z) = org_with_agent(&app);
    issue(&app, "p1", "p1", Scope::Print);
    issue(&app, "p2", "p2", Scope::Print);
    let post = json!({"printerId": z, "contentType": "raw_base64", "content": "QUJD"});
    let (code, a) = app.pn("POST", "/printjobs", "p1", Some(post.clone()));
    assert_eq!(code, 201);
    let a = a.as_i64().unwrap();
    let b = app
        .pn("POST", "/printjobs", "p2", Some(post))
        .1
        .as_i64()
        .unwrap();
    let ids = |v: Value| -> Vec<i64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|j| j["id"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(ids(app.pn("GET", "/printjobs", "p1", None).1), [a]);
    assert_eq!(
        ids(app.pn("GET", &format!("/printjobs/{a},{b}"), "p1", None).1),
        [a]
    );
    assert!(ids(app.pn("GET", &format!("/printjobs/{b}"), "p1", None).1).is_empty());
    let states = app
        .pn("GET", &format!("/printjobs/{a},{b}/states"), "p1", None)
        .1;
    assert_eq!(states.as_array().unwrap().len(), 1);
    // the delete counts only its own
    assert_eq!(
        app.pn("DELETE", &format!("/printjobs/{a},{b}"), "p1", None)
            .1,
        json!(1)
    );
    assert_eq!(job_state(&app.db, a), "cancelled");
    assert_eq!(job_state(&app.db, b), "queued");
    // the root token sees everything
    assert_eq!(ids(app.pn("GET", "/printjobs", "t", None).1).len(), 2);
}

// --- the Shopify webhook ------------------------------------------------------------------------

#[test]
fn shopify_refuses_a_key_that_may_not_submit_before_it_checks_the_signature() {
    let app = App::new();
    let keys = six_keys(&app);
    let (reg, _) = org_with_agent(&app);
    let hp = reg.pid("HP");
    app.db
        .with(|c| store::set_org_shopify_secret(c, DEFAULT_ORG, Some("s3cret")))
        .unwrap();
    for (word, key) in &keys {
        let r = app
            .call(
                "POST",
                &format!("/integrations/shopify/orders?key={key}&printer_id={hp}"),
            )
            .header("x-shopify-hmac-sha256", "bad")
            .raw(b"{}")
            .send();
        if ["full", "print"].contains(word) {
            assert_eq!(r.status, 401, "{word}: reaches the signature check");
            assert_eq!(r.json()["error"], "bad signature");
        } else {
            assert_eq!(r.status, 403, "{word}");
            assert_eq!(r.json()["error"], denied_text(word));
        }
    }
}

// --- CloudPRNT ----------------------------------------------------------------------------------

const MAC: &str = "00:11:62:0f:97:1a";

fn poll_body(mac: &str) -> Value {
    json!({"status": "23 6 0 0 0 0 0 0 0 ", "printerMAC": mac, "statusCode": "200%20OK", "clientAction": null})
}

/// One CloudPRNT request, the key in the path or as the Basic user name.
fn cp(app: &App, method: &str, key: &str, basic: bool, query: &str, body: Option<&Value>) -> Reply {
    let target = if basic {
        format!("/cloudprnt{query}")
    } else {
        format!("/cloudprnt/{key}{query}")
    };
    let mut c = app.call(method, &target);
    if basic {
        c = c.basic(key);
    }
    if let Some(b) = body {
        c = c.json(b);
    }
    c.send()
}

#[test]
fn cloudprnt_refuses_a_key_that_may_not_act_as_a_star_printer() {
    let app = App::new();
    let keys = six_keys(&app);
    // a Star printer enrolled with the CloudPRNT key, and a job waiting for it
    let r = cp(
        &app,
        "POST",
        "k-cloudprnt",
        false,
        "",
        Some(&poll_body(MAC)),
    );
    assert_eq!((r.status, r.json()), (200, json!({"jobReady": false})));
    let printers = app.get("/printers", Some("k-full")).1;
    let pid = printers["printers"][0]["id"].as_i64().unwrap();
    let jid = job_id(&app.post("/jobs", Some("k-full"), job_body(pid)).1);
    let agents = agents_in_db(&app);
    for (word, key) in keys
        .iter()
        .filter(|(w, _)| !["full", "cloudprnt"].contains(w))
    {
        for basic in [false, true] {
            for (method, query, body) in [
                ("POST", String::new(), Some(poll_body(MAC))),
                ("POST", String::new(), Some(poll_body("aa:bb:cc:dd:ee:ff"))),
                (
                    "GET",
                    format!("?mac={MAC}&type=application/vnd.star.starprnt"),
                    None,
                ),
                (
                    "DELETE",
                    format!("?mac={MAC}&code=200%20OK&token={jid}"),
                    None,
                ),
            ] {
                let r = cp(&app, method, key, basic, &query, body.as_ref());
                assert_eq!(r.status, 403, "{word} {method} basic={basic}");
                assert_eq!(r.json()["error"], denied_text(word));
            }
        }
    }
    assert_eq!(
        job_state(&app.db, jid),
        "queued",
        "nothing was offered, swallowed or confirmed"
    );
    assert_eq!(agents_in_db(&app), agents, "no device was created");
}

#[test]
fn a_cloudprnt_key_enrols_downloads_and_confirms() {
    let app = App::new();
    six_keys(&app);
    let url = "k-cloudprnt";
    cp(&app, "POST", url, false, "", Some(&poll_body(MAC)));
    let pid = app.get("/printers", Some("k-full")).1["printers"][0]["id"]
        .as_i64()
        .unwrap();
    let jid = job_id(&app.post("/jobs", Some("k-full"), job_body(pid)).1);
    let r = cp(&app, "POST", url, false, "", Some(&poll_body(MAC)));
    assert_eq!(r.json()["jobReady"], true);
    let r = cp(
        &app,
        "GET",
        url,
        false,
        &format!("?mac={MAC}&token={jid}"),
        None,
    );
    assert_eq!((r.status, r.body.as_slice()), (200, b"ABC".as_slice()));
    let r = cp(
        &app,
        "DELETE",
        url,
        false,
        &format!("?mac={MAC}&code=200%20OK&token={jid}"),
        None,
    );
    assert_eq!(r.status, 200);
    assert_eq!(job_state(&app.db, jid), "done");
    // the same through Basic
    let r = cp(&app, "POST", url, true, "", Some(&poll_body(MAC)));
    assert_eq!(r.status, 200);
}

#[test]
fn a_cloudprnt_key_is_no_agent_and_no_client() {
    let app = App::new();
    six_keys(&app);
    let k = "k-cloudprnt";
    for (method, path, want) in [
        ("POST", "/agent/register", 401),
        ("GET", "/agent/jobs", 401),
        ("GET", "/agent/jobs/1/payload", 401),
        ("POST", "/agent/jobs/1/result", 401),
        ("GET", "/computers", 403),
    ] {
        let r = app
            .call(method, path)
            .bearer(k)
            .json(&json!({"name": "pc"}))
            .send();
        assert_eq!(r.status, want, "{method} {path}");
    }
    assert_eq!(agents_in_db(&app), 0);
}

// --- enrolling and using an agent ---------------------------------------------------------------

#[test]
fn only_a_key_with_the_agent_right_enrolls_a_computer() {
    for open in [false, true] {
        let app = App::with(|c| c.open_enrollment = open);
        let keys = six_keys(&app);
        for (word, key) in keys
            .iter()
            .filter(|(w, _)| ["cloudprnt", "print", "read", "unknown"].contains(w))
        {
            for body in [
                json!({"name": "pc", "printers": [{"name": "Z"}]}),
                json!({"name": "pc", "printers": 5}),
                json!({}),
            ] {
                let r = app.agent("POST", "/agent/register", key, Some(body));
                assert_eq!(r.status, 401, "{word} open={open}");
                assert_eq!(
                    r.json()["error"],
                    "this key may not enroll agents: issue one with the rights \"agent\" or \"full\""
                );
            }
        }
        assert_eq!(agents_in_db(&app), 0, "open={open}");
        for key in ["k-full", "k-agent"] {
            assert_eq!(register_http(&app, key, key).status, 200, "{key}");
        }
    }
}

#[test]
fn an_agent_key_may_not_take_a_star_printers_name() {
    let app = App::new();
    six_keys(&app);
    let name = "cloudprnt-00:11:62:0f:97:1a";
    let r = app.agent(
        "POST",
        "/agent/register",
        "k-agent",
        Some(json!({"name": name, "printers": []})),
    );
    assert_eq!(r.status, 401);
    assert_eq!(
        r.json()["error"],
        "names starting with cloudprnt- are reserved for Star printers"
    );
    assert_eq!(agents_in_db(&app), 0);
    // a full key and root may, as today
    for key in ["k-full", "t"] {
        let n = format!("{name}-{key}");
        let r = app.agent(
            "POST",
            "/agent/register",
            key,
            Some(json!({"name": n, "printers": []})),
        );
        assert_eq!(r.status, 200, "{key}");
    }
    // an ordinary name is fine for an agent key
    assert_eq!(register_http(&app, "k-agent", "pc").status, 200);
}

#[test]
fn a_key_holding_a_star_printers_name_cannot_be_narrowed_to_the_agent_right_alone() {
    let app = App::new();
    let kid = issue(&app, "pc", "kf", Scope::Full);
    let name = "cloudprnt-00:11:62:0f:97:1a";
    let pid = printer_of(&register_http(&app, "kf", name));
    let put = |scope: &str| {
        app.put(
            &format!("/apikeys/{kid}"),
            Some("t"),
            json!({"scope": scope}),
        )
    };
    // an agent-only key would keep the Star printer's queue, and its next register would be
    // refused: nothing changes and the answer says why
    let (code, b) = put("agent");
    assert_eq!(code, 400, "{b}");
    assert_eq!(
        b["error"],
        "this key's computer is named cloudprnt-..., a name reserved for Star printers: keep the rights \"full\" or enrol the computer under another name"
    );
    let (_, keys) = app.get("/apikeys", Some("t"));
    assert!(keys["keys"][0].get("scope").is_none(), "{keys}");
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 204);
    // the same key keeps its right to register again, and a job for the name is the agent's own
    assert_eq!(register_http(&app, "kf", name).status, 200);
    let jid = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    assert_eq!(
        app.agent("GET", "/agent/jobs", "kf", None).json()["job_id"].as_i64(),
        Some(jid)
    );
    // losing the agent right is no problem, a computer without such a name is no problem either
    for scope in ["print", "read", "cloudprnt", "full"] {
        assert_eq!(put(scope).0, 200, "{scope}");
    }
    let plain = issue(&app, "plain", "kp", Scope::Full);
    register_http(&app, "kp", "pc");
    let (code, _) = app.put(
        &format!("/apikeys/{plain}"),
        Some("t"),
        json!({"scope": "agent"}),
    );
    assert_eq!(code, 200);
    // an unknown key is still a 404, whatever the scope asked for
    let (code, _) = app.put("/apikeys/9999", Some("t"), json!({"scope": "agent"}));
    assert_eq!(code, 404);
}

#[test]
fn an_agent_key_registers_polls_downloads_and_reports_and_nothing_else() {
    let app = App::new();
    six_keys(&app);
    let k = "k-agent";
    let pid = printer_of(&register_http(&app, k, "pc"));
    assert_eq!(app.agent("GET", "/agent/jobs", k, None).status, 204);
    let jid = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    let r = app.agent("GET", "/agent/jobs", k, None);
    assert_eq!((r.status, r.json()["job_id"].as_i64()), (200, Some(jid)));
    let r = app.agent("GET", &format!("/agent/jobs/{jid}/payload"), k, None);
    assert_eq!((r.status, r.body.as_slice()), (200, b"ABC".as_slice()));
    let r = app.agent(
        "POST",
        &format!("/agent/jobs/{jid}/result"),
        k,
        Some(json!({"ok": true})),
    );
    assert_eq!(r.status, 200);
    assert_eq!(job_state(&app.db, jid), "done");
    // the org's computer list, as released setups read it
    let (code, b) = app.get("/computers", Some(k));
    assert_eq!(code, 200);
    assert_eq!(b["computers"].as_array().unwrap().len(), 1);
    for (method, path) in [
        ("POST", "/jobs"),
        ("GET", "/printers"),
        ("GET", "/jobs"),
        ("GET", "/metrics"),
    ] {
        let r = app.call(method, path).bearer(k).json(&job_body(pid)).send();
        assert_eq!(r.status, 403, "{method} {path}");
    }
}

#[test]
fn narrowing_a_key_in_place_keeps_its_agent_and_widening_restores_it() {
    let app = App::new();
    let kid = issue(&app, "pc", "kf", Scope::Full);
    register_http(&app, "kf", "pc");
    let put = |scope: &str| {
        app.put(
            &format!("/apikeys/{kid}"),
            Some("t"),
            json!({"scope": scope}),
        )
        .0
    };
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 204);
    assert_eq!(put("agent"), 200);
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 204);
    assert_eq!(put("print"), 200);
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 401);
    assert_eq!(put("full"), 200);
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 204);
    // the agent row never changed
    assert_eq!(agents_in_db(&app), 1);
}

// --- a job already handed out -------------------------------------------------------------------

#[test]
fn a_job_in_flight_can_be_finished_after_the_key_was_narrowed() {
    let app = App::new();
    let kid = issue(&app, "pc", "kf", Scope::Full);
    let pid = printer_of(&register_http(&app, "kf", "pc"));
    let j1 = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    let j2 = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    let r = app.agent("GET", "/agent/jobs", "kf", None);
    assert_eq!(r.json()["job_id"].as_i64(), Some(j1));
    let put = |scope: &str| {
        app.put(
            &format!("/apikeys/{kid}"),
            Some("t"),
            json!({"scope": scope}),
        )
        .0
    };
    assert_eq!(put("print"), 200);

    let payload = |j: i64| {
        app.agent("GET", &format!("/agent/jobs/{j}/payload"), "kf", None)
            .status
    };
    let result = |j: i64| {
        app.agent(
            "POST",
            &format!("/agent/jobs/{j}/result"),
            "kf",
            Some(json!({"ok": true})),
        )
        .status
    };
    // new work stays closed, the claimed job is open
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 401);
    assert_eq!(payload(j1), 200);
    assert_eq!(payload(j2), 401, "a queued job is not handed out");
    assert_eq!(result(j2), 401);
    assert_eq!(result(j1), 200);
    assert_eq!(job_state(&app.db, j1), "done");
    // once done it is closed again
    assert_eq!(payload(j1), 401);
    assert_eq!(result(j1), 401);
    // the queued job waits until the right is back
    assert_eq!(job_state(&app.db, j2), "queued");
    assert_eq!(put("full"), 200);
    let r = app.agent("GET", "/agent/jobs", "kf", None);
    assert_eq!(r.json()["job_id"].as_i64(), Some(j2));
}

#[test]
fn a_revoked_key_gets_nothing_not_even_its_claimed_job() {
    let app = App::new();
    let kid = issue(&app, "pc", "kf", Scope::Full);
    let pid = printer_of(&register_http(&app, "kf", "pc"));
    let jid = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    assert_eq!(app.agent("GET", "/agent/jobs", "kf", None).status, 200);
    assert_eq!(app.delete(&format!("/apikeys/{kid}"), Some("t")).0, 200);
    assert_eq!(
        app.agent("GET", &format!("/agent/jobs/{jid}/payload"), "kf", None)
            .status,
        401
    );
    let r = app.agent(
        "POST",
        &format!("/agent/jobs/{jid}/result"),
        "kf",
        Some(json!({"ok": true})),
    );
    assert_eq!(r.status, 401);
    assert_eq!(job_state(&app.db, jid), "claimed");
}

#[test]
fn a_request_without_a_key_gets_nothing_from_an_agent_registered_under_the_empty_key() {
    let app = App::new();
    // a v2 server let a request without an Authorization header register under the empty key
    let reg = register(&app.db, "headless", "", &[decl("Z", false)], DEFAULT_ORG);
    let jid = job_id(&app.post("/jobs", Some("t"), job_body(reg.pid("Z"))).1);
    claim(&app.db, reg.computer_id).unwrap();
    let payload = format!("/agent/jobs/{jid}/payload");
    let result = format!("/agent/jobs/{jid}/result");
    assert_eq!(app.call("GET", &payload).send().status, 401);
    assert_eq!(app.call("GET", &payload).bearer("").send().status, 401);
    let r = app.call("POST", &result).json(&json!({"ok": true})).send();
    assert_eq!(r.status, 401);
    let r = app
        .call("POST", &result)
        .bearer("")
        .json(&json!({"ok": true}))
        .send();
    assert_eq!(r.status, 401);
    assert_eq!(job_state(&app.db, jid), "claimed");
}

#[test]
fn a_narrowed_key_without_an_agent_gains_nothing_for_a_claimed_job() {
    let app = App::new();
    let (reg, z) = org_with_agent(&app);
    issue(&app, "p", "kp", Scope::Print);
    let jid = job_id(&app.post("/jobs", Some("t"), job_body(z)).1);
    claim(&app.db, reg.computer_id).unwrap();
    assert_eq!(
        app.agent("GET", &format!("/agent/jobs/{jid}/payload"), "kp", None)
            .status,
        401
    );
}

// --- waking the long-poll of exactly one agent ---------------------------------------------------

#[test]
fn changing_or_revoking_a_key_wakes_only_the_agent_enrolled_with_it() {
    let app = App::new();
    let ka = issue(&app, "a", "ka", Scope::Full);
    issue(&app, "b", "kb", Scope::Full);
    let a = register_http(&app, "ka", "pc-a").json()["computer_id"]
        .as_i64()
        .unwrap();
    let b = register_http(&app, "kb", "pc-b").json()["computer_id"]
        .as_i64()
        .unwrap();
    let (na, nb) = (app.state.waker.for_agent(a), app.state.waker.for_agent(b));
    let (mut fa, mut fb) = (pin!(na.notified()), pin!(nb.notified()));
    assert!(!fa.as_mut().enable() && !fb.as_mut().enable());
    // widening wakes nobody
    app.put(
        &format!("/apikeys/{ka}"),
        Some("t"),
        json!({"scope": "agent"}),
    );
    assert!(!fa.as_mut().enable() && !fb.as_mut().enable());
    app.put(
        &format!("/apikeys/{ka}"),
        Some("t"),
        json!({"scope": "full"}),
    );
    assert!(!fa.as_mut().enable() && !fb.as_mut().enable());
    // taking the agent right away wakes that agent, not the other
    app.put(
        &format!("/apikeys/{ka}"),
        Some("t"),
        json!({"scope": "print"}),
    );
    assert!(fa.as_mut().enable(), "the enrolled agent is woken");
    assert!(!fb.as_mut().enable(), "another agent is not");
    // revoking wakes only the enrolled agent, once
    let (mut fa, mut fb) = (pin!(na.notified()), pin!(nb.notified()));
    assert!(!fa.as_mut().enable() && !fb.as_mut().enable());
    app.put(
        &format!("/apikeys/{ka}"),
        Some("t"),
        json!({"scope": "full"}),
    );
    assert!(!fa.as_mut().enable());
    assert_eq!(app.delete(&format!("/apikeys/{ka}"), Some("t")).0, 200);
    assert!(fa.as_mut().enable() && !fb.as_mut().enable());
    // a second DELETE answers 200 as ever and wakes nobody
    let (mut fa, mut fb) = (pin!(na.notified()), pin!(nb.notified()));
    assert!(!fa.as_mut().enable() && !fb.as_mut().enable());
    assert_eq!(app.delete(&format!("/apikeys/{ka}"), Some("t")).0, 200);
    assert!(!fa.as_mut().enable() && !fb.as_mut().enable());
}
