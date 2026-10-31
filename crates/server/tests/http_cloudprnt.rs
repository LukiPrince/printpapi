// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Star CloudPRNT over HTTP — the HTTP tests of `tests/test_cloudprnt.py` (the translation and
//! store-level ones are unit tests of `cloudprnt` and `store`).
mod common;

use common::*;
use printpapi_server::store;
use serde_json::{Value, json};

const MAC: &str = "00:11:62:0f:97:1a";

fn poll_body() -> Value {
    json!({"status": "23 6 0 0 0 0 0 0 0 ", "printerMAC": MAC, "statusCode": "200%20OK", "clientAction": null})
}

fn org_with_key(app: &App) -> i64 {
    let oid = create_org(&app.db, "acme");
    add_key(&app.db, "cloudprnt", "cpkey", oid);
    oid
}

/// One CloudPRNT client request -> (status, body bytes, Content-Type).
fn cp(
    app: &App,
    method: &str,
    target: &str,
    body: Option<&Value>,
    accept: Option<&str>,
    basic: Option<&str>,
) -> (u16, Vec<u8>, Option<String>) {
    let mut c = app.call(method, target);
    if let Some(b) = body {
        c = c.json(b);
    }
    if let Some(a) = accept {
        c = c.header("accept", a);
    }
    if let Some(k) = basic {
        c = c.basic(k);
    }
    let r = c.send();
    let ct = r.header("Content-Type").map(String::from);
    (r.status, r.body, ct)
}

fn poll(app: &App, url: &str, body: &Value) -> Value {
    serde_json::from_slice(&cp(app, "POST", url, Some(body), None, None).1).unwrap()
}

/// The id of the one printer `cpkey` sees.
fn printer_id(app: &App) -> i64 {
    app.get("/printers", Some("cpkey")).1["printers"][0]["id"]
        .as_i64()
        .unwrap()
}

fn submit(app: &App, extra: Value) -> i64 {
    let mut body = json!({"printer_id": printer_id(app), "type": "raw_base64", "content": "QUJD"});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    job_id(&app.post("/jobs", Some("cpkey"), body).1)
}

#[test]
fn cloudprnt_roundtrip_poll_get_delete() {
    let app = App::new();
    org_with_key(&app);
    let url = "/cloudprnt/cpkey";
    // first poll: nothing to print, and it enrols the printer
    let (code, raw, _) = cp(
        &app,
        "POST",
        url,
        Some(&poll_body()),
        Some("application/vnd.star.starprnt"),
        None,
    );
    assert_eq!(
        (code, serde_json::from_slice::<Value>(&raw).unwrap()),
        (200, json!({"jobReady": false}))
    );
    let (_, printers) = app.get("/printers", Some("cpkey"));
    assert_eq!(printers["printers"].as_array().unwrap().len(), 1);
    assert_eq!(printers["printers"][0]["name"], format!("cloudprnt-{MAC}"));

    // a client queues a raw job the ordinary way
    let jid = submit(&app, json!({}));

    // the next poll offers it
    let (_, raw, _) = cp(
        &app,
        "POST",
        url,
        Some(&poll_body()),
        Some("application/vnd.star.starprnt"),
        None,
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&raw).unwrap(),
        json!({"jobReady": true, "mediaTypes": ["application/vnd.star.starprnt"], "jobToken": jid.to_string()})
    );

    // the printer pulls the data
    let (code, data, ctype) = cp(
        &app,
        "GET",
        &format!("{url}?mac={MAC}&type=application/vnd.star.starprnt&token={jid}"),
        None,
        None,
        None,
    );
    assert!(code == 200 && data == b"ABC");
    assert_eq!(ctype.as_deref(), Some("application/vnd.star.starprnt"));

    // ... prints it, and confirms
    assert_eq!(
        cp(
            &app,
            "DELETE",
            &format!("{url}?mac={MAC}&code=200%20OK&token={jid}"),
            None,
            None,
            None
        )
        .0,
        200
    );
    assert_eq!(
        app.get(&format!("/jobs/{jid}"), Some("cpkey")).1["state"],
        "done"
    );
}

#[test]
fn printer_error_code_fails_the_job() {
    let app = App::new();
    org_with_key(&app);
    let url = "/cloudprnt/cpkey";
    poll(&app, url, &poll_body());
    let jid = submit(&app, json!({}));
    poll(&app, url, &poll_body());
    cp(&app, "GET", &format!("{url}?mac={MAC}"), None, None, None);
    assert_eq!(
        cp(
            &app,
            "DELETE",
            &format!("{url}?mac={MAC}&code=420%20Cover%20open"),
            None,
            None,
            None
        )
        .0,
        200
    );
    let job = app.get(&format!("/jobs/{jid}"), Some("cpkey")).1;
    let error = job["error"].as_str().unwrap();
    assert!(
        job["state"] == "failed" && error.contains("420") && error.contains("cover open"),
        "{job}"
    );
    assert_eq!(error, "printer reported 420 Cover open (cover open)");
}

#[test]
fn an_unconfirmed_job_is_re_offered_not_replaced() {
    let app = App::new();
    org_with_key(&app);
    let url = "/cloudprnt/cpkey";
    poll(&app, url, &poll_body());
    let first = submit(&app, json!({}));
    let second = submit(&app, json!({"content": "REVG"}));
    let tok1 = poll(&app, url, &poll_body())["jobToken"].clone();
    let tok2 = poll(&app, url, &poll_body())["jobToken"].clone();
    let first_token = first.to_string();
    assert!(tok1 == tok2 && tok1 == first_token.as_str()); // the lost response does not burn the second job
    assert_eq!(
        app.get(&format!("/jobs/{second}"), Some("cpkey")).1["state"],
        "queued"
    );
    // a busy printer is offered nothing at all
    let mut busy = poll_body();
    busy["printingInProgress"] = json!(true);
    assert_eq!(poll(&app, url, &busy), json!({"jobReady": false}));
}

#[test]
fn copies_are_repeated_in_the_stream() {
    let app = App::new();
    org_with_key(&app);
    let url = "/cloudprnt/cpkey";
    poll(&app, url, &poll_body());
    submit(&app, json!({"copies": 3}));
    poll(&app, url, &poll_body());
    // the printer has no copy count of its own — the stream carries them
    assert_eq!(
        cp(&app, "GET", &format!("{url}?mac={MAC}"), None, None, None).1,
        b"ABCABCABC"
    );
}

#[test]
fn pdf_job_fails_instead_of_printing_blanks() {
    let app = App::new();
    org_with_key(&app);
    let url = "/cloudprnt/cpkey";
    poll(&app, url, &poll_body());
    let jid = submit(&app, json!({"type": "pdf_base64"}));
    assert_eq!(poll(&app, url, &poll_body()), json!({"jobReady": false}));
    let job = app.get(&format!("/jobs/{jid}"), Some("cpkey")).1;
    assert!(job["state"] == "failed" && job["error"].as_str().unwrap().contains("PDF"));
    assert_eq!(
        job["error"],
        "CloudPRNT printers cannot render PDF — send raw Star commands"
    );
}

#[test]
fn auth_mac_and_media_type_errors() {
    let app = App::new();
    org_with_key(&app);
    let url = "/cloudprnt/cpkey";
    // unknown key on every method
    assert_eq!(
        cp(
            &app,
            "POST",
            "/cloudprnt/nope",
            Some(&poll_body()),
            None,
            None
        )
        .0,
        401
    );
    assert_eq!(
        cp(
            &app,
            "GET",
            &format!("/cloudprnt/nope?mac={MAC}"),
            None,
            None,
            None
        )
        .0,
        401
    );
    assert_eq!(
        cp(
            &app,
            "DELETE",
            &format!("/cloudprnt/nope?mac={MAC}&code=OK"),
            None,
            None,
            None
        )
        .0,
        401
    );
    // the root token is not an org, so it cannot enrol a printer either
    assert_eq!(
        cp(&app, "POST", "/cloudprnt/t", Some(&poll_body()), None, None).0,
        401
    );
    // a device that does not say who it is
    let mut nomac = poll_body();
    nomac["printerMAC"] = json!("");
    let (code, raw, _) = cp(&app, "POST", url, Some(&nomac), None, None);
    assert_eq!(
        (code, serde_json::from_slice::<Value>(&raw).unwrap()),
        (400, json!({"error": "mac required"}))
    );
    assert_eq!(cp(&app, "GET", url, None, None, None).0, 400);
    // enrolled, nothing queued -> the protocol's "no data available"
    poll(&app, url, &poll_body());
    assert_eq!(
        cp(&app, "GET", &format!("{url}?mac={MAC}"), None, None, None).0,
        404
    );
    let jid = submit(&app, json!({}));
    poll(&app, url, &poll_body());
    // a type we cannot label these bytes with (we never transcode)
    let (code, raw, _) = cp(
        &app,
        "GET",
        &format!("{url}?mac={MAC}&type=image/png"),
        None,
        None,
        None,
    );
    assert_eq!(
        (code, serde_json::from_slice::<Value>(&raw).unwrap()),
        (415, json!({"error": "cannot serve image/png"}))
    );
    // someone else's token
    assert_eq!(
        cp(
            &app,
            "GET",
            &format!("{url}?mac={MAC}&token={}", jid + 99),
            None,
            None,
            None
        )
        .0,
        404
    );
    // confirming without a code is not a success
    assert_eq!(
        cp(
            &app,
            "DELETE",
            &format!("{url}?mac={MAC}"),
            None,
            None,
            None
        )
        .0,
        200
    );
    assert_eq!(
        app.get(&format!("/jobs/{jid}"), Some("cpkey")).1["state"],
        "failed"
    );
}

#[test]
fn a_poll_body_that_is_not_an_object_reads_as_empty() {
    let app = App::new();
    org_with_key(&app);
    // the Python treated a non-object body as {} -> no MAC -> 400; invalid JSON is its own 400
    let r = app.call("POST", "/cloudprnt/cpkey").raw(b"[1]").send();
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!("mac required"))
    );
    let r = app.call("POST", "/cloudprnt/cpkey").raw(b"{").send();
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!("bad json"))
    );
    let r = app.call("POST", "/cloudprnt/cpkey").body_error().send();
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!("bad json"))
    );
}

#[test]
fn server_setting_request_is_a_404_so_the_printer_falls_back_to_http() {
    // An MQTT-capable printer asks for <cloudprnt path>/cloudprnt-setting.json once at power-on.
    // A server that speaks only CloudPRNT HTTP answers 404 to it — and then the printer polls.
    let app = App::new();
    org_with_key(&app);
    assert_eq!(
        cp(
            &app,
            "GET",
            &format!("/cloudprnt/cloudprnt-setting.json?mac={MAC}&replaced_path=cpkey"),
            None,
            None,
            None
        )
        .0,
        404
    );
    assert_eq!(
        cp(
            &app,
            "GET",
            &format!("/cloudprnt-setting.json?mac={MAC}"),
            None,
            None,
            Some("cpkey")
        )
        .0,
        404
    );
    // ... and nothing was enrolled by it
    assert_eq!(app.get("/printers", Some("cpkey")).1["printers"], json!([]));
}

#[test]
fn basic_auth_is_accepted_and_does_not_collide_with_the_printnode_layer() {
    let app = App::new();
    org_with_key(&app);
    // the printer's own "User Name / Password" settings, so the key stays out of the URL
    let (code, raw, _) = cp(
        &app,
        "POST",
        "/cloudprnt",
        Some(&poll_body()),
        None,
        Some("cpkey"),
    );
    assert_eq!(
        (code, serde_json::from_slice::<Value>(&raw).unwrap()),
        (200, json!({"jobReady": false}))
    );
    assert_eq!(
        cp(
            &app,
            "GET",
            &format!("/cloudprnt?mac={MAC}"),
            None,
            None,
            Some("cpkey")
        )
        .0,
        404
    );
    assert_eq!(
        cp(
            &app,
            "DELETE",
            &format!("/cloudprnt?mac={MAC}&code=OK"),
            None,
            None,
            Some("cpkey")
        )
        .0,
        200
    );
    // ... and the PrintNode compat layer still answers its own paths under Basic auth
    let (code, raw, _) = cp(&app, "GET", "/whoami", None, None, Some("cpkey"));
    assert!(
        code == 200
            && serde_json::from_slice::<Value>(&raw)
                .unwrap()
                .get("id")
                .is_some()
    );
}

#[test]
fn a_device_is_registered_in_the_org_of_its_key() {
    let app = App::new();
    let oid = org_with_key(&app);
    poll(&app, "/cloudprnt/cpkey", &poll_body());
    let printers = app
        .db
        .with(|c| store::list_printers(c, 60.0, None, Some(oid)))
        .unwrap();
    assert_eq!(printers.len(), 1);
    assert!(!printers[0].can_pdf && printers[0].online);
}
