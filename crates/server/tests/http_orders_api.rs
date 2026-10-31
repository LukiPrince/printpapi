// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `POST /orders` and the Shopify webhook — the port of `tests/test_orders_api.py`.
mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::*;
use hmac::{Hmac, KeyInit, Mac};
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};
use sha2::Sha256;

fn shopify() -> Value {
    json!({
        "id": 450789469, "order_number": 1001, "name": "#1001",
        "created_at": "2026-07-25T10:22:51-04:00", "currency": "EUR",
        "subtotal_price": "29.70", "total_tax": "0.00", "total_price": "34.60",
        "note": "leave at the door", "email": "jane@example.com",
        "customer": {"first_name": "Jane", "last_name": "Doe"},
        "shipping_address": {"name": "Jane Doe", "address1": "Musterweg 1", "address2": "",
                             "zip": "12345", "city": "Berlin", "country": "Germany",
                             "phone": "+49 30 123456"},
        "shipping_lines": [{"title": "Standard", "price": "4.90"}],
        "line_items": [
            {"quantity": 2, "sku": "A-1", "title": "Widget", "variant_title": "blue", "price": "9.90"},
            {"quantity": 1, "sku": "B-2", "title": "Gadget", "variant_title": "Default Title", "price": "9.90"},
        ],
    })
}

fn order() -> Value {
    json!({"number": "1001", "customer": "Jane Doe", "address": ["Musterweg 1", "12345 Berlin"],
           "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80"}],
           "totals": [["Total", "19.80"]]})
}

/// A PDF-capable and a raw-only printer on one agent: (pdf_id, raw_id, agent_id).
fn printers(db: &store::Db, org_id: i64) -> (i64, i64, i64) {
    let reg = register(
        db,
        &format!("pc-{org_id}"),
        &format!("agent-{org_id}"),
        &[decl("HP", true), decl("Zebra", false)],
        org_id,
    );
    (reg.pid("HP"), reg.pid("Zebra"), reg.computer_id)
}

fn payload_of(db: &store::Db, job_id: i64, agent_id: i64) -> Vec<u8> {
    claim(db, agent_id);
    db.with(|c| store::get_payload(c, job_id, agent_id))
        .unwrap()
        .unwrap()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn post_orders_renders_a_packing_slip_and_queues_it() {
    let app = App::new();
    let (pdf, _, agent) = printers(&app.db, DEFAULT_ORG);
    let (code, body) = app.post(
        "/orders",
        Some("t"),
        json!({"printer_id": pdf, "order": order()}),
    );
    assert_eq!(code, 200);
    let jid = job_id(&body);
    assert!(payload_of(&app.db, jid, agent).starts_with(b"%PDF"));
    let job = &app
        .db
        .with(|c| store::recent_jobs(c, 50, None, None))
        .unwrap()[0];
    assert_eq!(
        (job.mode.as_str(), job.type_.as_str(), job.title.as_deref()),
        ("pdf", "order", Some("Packing slip 1001"))
    );
}

#[test]
fn post_orders_maps_a_store_payload_when_a_format_is_given() {
    let app = App::new();
    let (pdf, _, agent) = printers(&app.db, DEFAULT_ORG);
    let (code, body) = app.post(
        "/orders",
        Some("t"),
        json!({"printer_id": pdf, "format": "shopify", "order": shopify()}),
    );
    assert_eq!(code, 200);
    let payload = payload_of(&app.db, job_id(&body), agent);
    assert!(contains(&payload, br"Widget \(blue\)") && contains(&payload, b"#1001")); // parens are PDF-escaped
    let (code, body) = app.post(
        "/orders",
        Some("t"),
        json!({"printer_id": pdf, "format": "magento", "order": shopify()}),
    );
    assert_eq!(code, 400);
    assert_eq!(
        body["error"],
        "unknown order format: 'magento' (expected one of shopify, woocommerce)"
    );
}

#[test]
fn post_orders_rejects_bad_input() {
    let app = App::new();
    let (pdf, raw, _) = printers(&app.db, DEFAULT_ORG);
    assert_eq!(
        app.post(
            "/orders",
            None,
            json!({"printer_id": pdf, "order": order()})
        )
        .0,
        401
    );
    // a label printer cannot render a PDF (gotcha #1) — refuse before queueing blanks
    let r = app
        .call("POST", "/orders")
        .bearer("t")
        .json(&json!({"printer_id": raw, "order": order()}))
        .send();
    assert!(r.status == 400 && contains(&r.body, b"raw-only"));
    let (code, body) = app.post(
        "/orders",
        Some("t"),
        json!({"printer_id": 999, "order": order()}),
    );
    assert_eq!(
        (code, body["error"].as_str().unwrap()),
        (400, "unknown printer: 999")
    );
    let (code, body) = app.post("/orders", Some("t"), json!({"order": order()}));
    assert_eq!(
        (code, body["error"].as_str().unwrap()),
        (400, "unknown printer: None")
    );
    assert_eq!(
        app.post(
            "/orders",
            Some("t"),
            json!({"printer_id": pdf, "order": {"lines": []}})
        )
        .0,
        400
    );
    assert_eq!(
        app.post("/orders", Some("t"), json!({"printer_id": pdf})).0,
        400
    );
    assert_eq!(
        app.post(
            "/orders",
            Some("t"),
            json!({"printer_id": pdf, "order": order(), "copies": 0})
        )
        .0,
        400
    );
}

#[test]
fn post_orders_takes_the_job_knobs_of_post_jobs() {
    let app = App::new();
    let (pdf, _, _) = printers(&app.db, DEFAULT_ORG);
    let body = json!({"printer_id": pdf, "order": order(), "title": "Rush", "copies": 2, "idempotency_key": "o-1"});
    let first = job_id(&app.post("/orders", Some("t"), body.clone()).1);
    assert_eq!(job_id(&app.post("/orders", Some("t"), body).1), first);
    let jobs = app
        .db
        .with(|c| store::recent_jobs(c, 50, None, None))
        .unwrap();
    assert_eq!((jobs.len(), jobs[0].title.as_deref()), (1, Some("Rush")));
}

#[test]
fn orders_are_org_scoped() {
    let app = App::new();
    let other = create_org(&app.db, "other");
    add_key(&app.db, "other-key", "k-other", other);
    let (mine, _, _) = printers(&app.db, DEFAULT_ORG);
    // foreign printer
    assert_eq!(
        app.post(
            "/orders",
            Some("k-other"),
            json!({"printer_id": mine, "order": order()})
        )
        .0,
        400
    );
}

fn shopify_post(
    app: &App,
    target: &str,
    body: &Value,
    secret: &str,
    header: Option<&str>,
) -> (u16, Value) {
    let data = serde_json::to_vec(body).unwrap();
    let digest = B64.encode(
        Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .unwrap()
            .chain_update(&data)
            .finalize()
            .into_bytes(),
    );
    let r = app
        .call("POST", target)
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", header.unwrap_or(&digest))
        .header("x-shopify-topic", "orders/create")
        .header("x-shopify-shop-domain", "acme.myshopify.com")
        .raw(&data)
        .send();
    (r.status, r.json())
}

#[test]
fn shopify_webhook_verifies_hmac_and_prints_once_per_order() {
    let app = App::new();
    add_key(&app.db, "shop", "k-shop", DEFAULT_ORG);
    app.db
        .with(|c| store::set_org_shopify_secret(c, DEFAULT_ORG, Some("s3cret")))
        .unwrap();
    let (pdf, _, agent) = printers(&app.db, DEFAULT_ORG);
    let hook = format!("/integrations/shopify/orders?key=k-shop&printer_id={pdf}");
    let (code, body) = shopify_post(&app, &hook, &shopify(), "s3cret", None);
    assert_eq!(code, 200);
    let jid = job_id(&body);
    let payload = payload_of(&app.db, jid, agent);
    assert!(payload.starts_with(b"%PDF") && contains(&payload, b"acme.myshopify.com")); // shop domain
    // Shopify redelivers webhooks; the same order must never print twice
    let (code, body) = shopify_post(&app, &hook, &shopify(), "s3cret", None);
    assert!(code == 200 && job_id(&body) == jid);
    assert_eq!(
        app.db
            .with(|c| store::recent_jobs(c, 50, None, None))
            .unwrap()
            .len(),
        1
    );
    // a forged/altered payload is rejected
    assert_eq!(
        shopify_post(&app, &hook, &shopify(), "wrong-secret", None).0,
        401
    );
    assert_eq!(
        shopify_post(&app, &hook, &shopify(), "s3cret", Some("not-base64")).0,
        401
    );
    let nokey = format!("/integrations/shopify/orders?key=nope&printer_id={pdf}");
    assert_eq!(
        shopify_post(&app, &nokey, &shopify(), "s3cret", None).0,
        401
    );
    // the idempotency key is "shopify-<id>"
    let idem: Option<String> = app.db.with(|c| {
        c.query_row("SELECT idempotency_key FROM jobs WHERE id=?", [jid], |r| {
            r.get(0)
        })
        .unwrap()
    });
    assert_eq!(idem.as_deref(), Some("shopify-450789469"));
}

#[test]
fn shopify_webhook_reads_its_query_like_parse_qs() {
    let app = App::new();
    add_key(&app.db, "shop", "k shop+1", DEFAULT_ORG);
    app.db
        .with(|c| store::set_org_shopify_secret(c, DEFAULT_ORG, Some("s3cret")))
        .unwrap();
    let (pdf, _, _) = printers(&app.db, DEFAULT_ORG);
    // %20 and + both decode to a space; the first value of a repeated name wins
    let hook = format!(
        "/integrations/shopify/orders?key=k%20shop%2B1&key=other&printer_id={pdf}&printer_id=999"
    );
    assert_eq!(shopify_post(&app, &hook, &shopify(), "s3cret", None).0, 200);
    let hook = format!("/integrations/shopify/orders?key=k+shop%2B1&printer_id={pdf}");
    assert_eq!(shopify_post(&app, &hook, &shopify(), "s3cret", None).0, 200);
    // a blank value is dropped, as parse_qs does without keep_blank_values
    let hook = "/integrations/shopify/orders?key=k+shop%2B1&printer_id=".to_string();
    assert_eq!(shopify_post(&app, &hook, &shopify(), "s3cret", None).0, 400);
}

#[test]
fn shopify_webhook_without_a_configured_secret_is_refused() {
    let app = App::new();
    add_key(&app.db, "shop", "k-shop", DEFAULT_ORG);
    let (pdf, _, _) = printers(&app.db, DEFAULT_ORG);
    let r = shopify_post(
        &app,
        &format!("/integrations/shopify/orders?key=k-shop&printer_id={pdf}"),
        &shopify(),
        "s3cret",
        None,
    );
    assert!(r.0 == 400 && r.1["error"].as_str().unwrap().contains("shopify_secret"));
    // ...and a missing printer_id is a client error, not a traceback
    app.db
        .with(|c| store::set_org_shopify_secret(c, DEFAULT_ORG, Some("s3cret")))
        .unwrap();
    let r = shopify_post(
        &app,
        "/integrations/shopify/orders?key=k-shop",
        &shopify(),
        "s3cret",
        None,
    );
    assert_eq!(
        (r.0, r.1["error"].as_str().unwrap()),
        (400, "bad json or missing printer_id")
    );
    // body_error answers its own 400 before anything is looked up
    let r = app
        .call("POST", "/integrations/shopify/orders?key=k-shop")
        .body_error()
        .send();
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!("bad content-length"))
    );
}

#[test]
fn root_sets_the_shopify_secret_and_it_is_never_echoed_back() {
    let app = App::new();
    let url = format!("/orgs/{DEFAULT_ORG}");
    assert_eq!(
        app.put(&url, None, json!({"shopify_secret": "s3cret"})).0,
        401
    ); // root only
    assert_eq!(
        app.put(&url, Some("t"), json!({"shopify_secret": "s3cret"}))
            .0,
        200
    );
    let (_, orgs) = app.get("/orgs", Some("t"));
    let org = &orgs["orgs"][0];
    assert!(org["shopify_secret_set"] == true && org.get("shopify_secret").is_none());
    let secret = |app: &App| {
        app.db
            .with(|c| store::get_org(c, DEFAULT_ORG))
            .unwrap()
            .unwrap()
            .shopify_secret
    };
    assert_eq!(secret(&app).as_deref(), Some("s3cret"));
    assert_eq!(
        app.put(&url, Some("t"), json!({"shopify_secret": null})).0,
        200
    );
    assert_eq!(secret(&app), None);
}
