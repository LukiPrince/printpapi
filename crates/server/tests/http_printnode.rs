// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The PrintNode-compatible layer over HTTP — the HTTP tests of `tests/test_printnode.py` (the
//! translation functions are unit tests of `printnode`).
mod common;

use common::*;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};

fn pn(app: &App, method: &str, target: &str, body: Option<Value>) -> (u16, Value) {
    app.pn(method, target, "t", body)
}

#[test]
fn whoami_and_bad_key() {
    let app = App::new();
    let (code, body) = pn(&app, "GET", "/whoami", None);
    assert!(code == 200 && body["id"] == 0 && body["credits"].is_null());
    assert!(body["numComputers"] == 0 && body["totalPrints"] == 0);
    let (code, body) = app.pn("GET", "/whoami", "nope", None);
    assert_eq!(code, 401);
    assert!(
        !body["code"].as_str().unwrap().is_empty() && !body["message"].as_str().unwrap().is_empty()
    );
    assert_eq!(
        body,
        json!({"code": "Unauthorized", "message": "invalid API key"})
    );
}

#[test]
fn computers_and_printers_shapes() {
    let app = App::new();
    let caps = json!({"papers": ["A4"], "bins": ["Tray 1"], "duplex": true, "color": true});
    app.db
        .with(|c| {
            store::register_agent(
                c,
                "win-1",
                "ak",
                &[
                    decl("Zebra", false),
                    store::PrinterDecl {
                        name: "Office".into(),
                        can_pdf: true,
                        capabilities: Some(caps),
                    },
                ],
                DEFAULT_ORG,
            )
        })
        .unwrap();
    let (code, comps) = pn(&app, "GET", "/computers", None);
    assert!(code == 200 && comps.as_array().unwrap().len() == 1);
    assert!(comps[0]["name"] == "win-1" && comps[0]["state"] == "connected");
    assert!(comps[0]["hostname"] == "win-1" && comps[0]["inet"].is_null());

    let (code, printers) = pn(&app, "GET", "/printers", None);
    assert_eq!(code, 200);
    let names: Vec<&str> = printers
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Zebra", "Office"]);
    assert_eq!(printers[0]["computer"]["id"], comps[0]["id"]);
    assert!(printers[0]["state"] == "online" && printers[0]["capabilities"].is_null());
    assert_eq!(printers[1]["capabilities"]["papers"], json!({"A4": null}));

    // id sets, and the computer-scoped printer list
    let one = printers[1]["id"].as_i64().unwrap();
    assert_eq!(
        pn(&app, "GET", &format!("/printers/{one}"), None).1[0]["id"],
        one
    );
    assert_eq!(
        pn(&app, "GET", &format!("/printers/{one}"), None)
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let comp = comps[0]["id"].as_i64().unwrap();
    assert_eq!(
        pn(&app, "GET", &format!("/computers/{comp}/printers"), None)
            .1
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        pn(&app, "GET", &format!("/computers/{comp}"), None)
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(pn(&app, "GET", "/printers/9-1", None).0, 400);
    // not routes of theirs: they fall through to the dashboard's 404
    assert_eq!(
        pn(&app, "GET", &format!("/printers/{one}/states"), None).0,
        404
    );
    assert_eq!(
        pn(&app, "GET", &format!("/printers/{one}/printers"), None).0,
        404
    );

    // Bearer on the same path still gets printpapi's own shape
    let (code, own) = app.get("/printers", Some("t"));
    assert!(code == 200 && own["printers"][0]["can_pdf"] == false);
}

#[test]
fn printjob_submit_list_states_and_delete() {
    let app = App::new();
    let reg = register(&app.db, "win-1", "ak", &[decl("Zebra", false)], DEFAULT_ORG);
    let pid = reg.pid("Zebra");
    let (code, jid) = pn(
        &app,
        "POST",
        "/printjobs",
        Some(
            json!({"printerId": pid, "title": "Label", "contentType": "raw_base64", "content": "QUJD", "source": "my plugin"}),
        ),
    );
    assert_eq!(code, 201);
    let jid = jid.as_i64().expect("PrintNode answers with the bare id");
    assert_eq!(job_state(&app.db, jid), "queued");

    let (code, jobs) = pn(&app, "GET", "/printjobs", None);
    assert!(code == 200 && jobs[0]["id"] == jid && jobs[0]["state"] == "queued");
    assert!(jobs[0]["title"] == "Label" && jobs[0]["contentType"] == "raw_base64");
    assert_eq!(jobs[0]["printer"]["id"], pid);

    let (code, states) = pn(&app, "GET", &format!("/printjobs/{jid}/states"), None);
    assert!(code == 200 && states[0][0]["printJobId"] == jid);
    assert_eq!(states[0][0]["state"], "queued");

    // a claimed job reads as `sent`, a finished one as `done`
    claim(&app.db, reg.computer_id);
    assert_eq!(
        pn(&app, "GET", &format!("/printjobs/{jid}"), None).1[0]["state"],
        "sent"
    );
    finish(&app.db, jid, reg.computer_id, true, None);
    assert_eq!(
        pn(&app, "GET", &format!("/printjobs/{jid}/states"), None).1[0][0]["state"],
        "done"
    );

    // delete cancels what is still queued
    let (code, jid2) = pn(
        &app,
        "POST",
        "/printjobs",
        Some(json!({"printerId": pid, "contentType": "raw_base64", "content": "QUJD"})),
    );
    assert_eq!(code, 201);
    let jid2 = jid2.as_i64().unwrap();
    assert_eq!(
        pn(&app, "DELETE", &format!("/printjobs/{jid2}"), None),
        (200, json!(1))
    );
    assert_eq!(job_state(&app.db, jid2), "cancelled");
    assert_eq!(
        pn(&app, "GET", &format!("/printjobs/{jid2}"), None).1[0]["state"],
        "deleted"
    );
    // already cancelled -> nothing affected
    assert_eq!(
        pn(&app, "DELETE", &format!("/printjobs/{jid2},{jid}"), None),
        (200, json!(0))
    );

    // their clients page the list with ?limit=
    let n = |app: &App, t: &str| pn(app, "GET", t, None).1.as_array().unwrap().len();
    assert_eq!(n(&app, "/printjobs"), 2);
    assert_eq!(n(&app, "/printjobs?limit=1"), 1);
    assert_eq!(n(&app, "/printjobs?limit=junk"), 2);
    assert_eq!(n(&app, "/printjobs?limit=0"), 1); // clamped up to 1
    assert_eq!(n(&app, "/printjobs?limit=-5"), 1);
    assert_eq!(n(&app, "/printjobs?limit="), 2); // blank values are dropped: the default

    let (code, body) = pn(
        &app,
        "POST",
        "/printjobs",
        Some(json!({"printerId": 999, "contentType": "raw_base64", "content": "QUJD"})),
    );
    assert_eq!(
        (code, body),
        (
            400,
            json!({"code": "BadRequest", "message": "unknown printer: 999"})
        )
    );
}

#[test]
fn printjob_post_errors_use_their_shape() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let r = app.call("POST", "/printjobs").basic("t").raw(b"{").send();
    assert_eq!(
        (r.status, r.json()),
        (
            400,
            json!({"code": "BadRequest", "message": "malformed json body"})
        )
    );
    let r = app.call("POST", "/printjobs").basic("t").raw(b"[1]").send();
    assert_eq!(
        (r.status, r.json()["message"].clone()),
        (400, json!("body must be an object"))
    );
    let (code, body) = pn(
        &app,
        "POST",
        "/printjobs",
        Some(json!({"printerId": pid, "contentType": "docx", "content": "x"})),
    );
    assert_eq!(code, 400);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("unsupported contentType: 'docx'")
    );
    assert_eq!(app.pn("POST", "/printjobs", "nope", Some(json!({}))).0, 401);
    // a failed download is theirs to retry: 502 DownstreamError
    *app.fetch.reply.lock().unwrap() = Some(Err("HTTP Error 500: Boom".into()));
    let (code, body) = pn(
        &app,
        "POST",
        "/printjobs",
        Some(json!({"printerId": pid, "contentType": "raw_uri", "content": "https://x.test/a"})),
    );
    assert_eq!(
        (code, body["code"].clone()),
        (502, json!("DownstreamError"))
    );
    assert_eq!(app.pn("DELETE", "/printjobs/9-1", "t", None).0, 400);
    assert_eq!(app.pn("DELETE", "/printjobs/1", "nope", None).0, 401);
}

#[test]
fn compat_layer_is_org_scoped() {
    let app = App::new();
    let other = create_org(&app.db, "other");
    let (key_a, key_b) = ("ka", "kb");
    add_key(&app.db, "a", key_a, DEFAULT_ORG);
    add_key(&app.db, "b", key_b, other);
    let pid = register(&app.db, "win-1", "ak", &[decl("Zebra", false)], DEFAULT_ORG).pid("Zebra");
    let jid = enqueue(&app.db, pid, b"ABC", |_| {});
    assert_eq!(
        app.pn("GET", "/printers", key_a, None)
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(app.pn("GET", "/printers", key_b, None).1, json!([]));
    assert_eq!(app.pn("GET", "/computers", key_b, None).1, json!([]));
    assert_eq!(
        app.pn("GET", &format!("/printjobs/{jid}"), key_b, None).1,
        json!([])
    );
    // a foreign printer is simply unknown
    let body = json!({"printerId": pid, "contentType": "raw_base64", "content": "QUJD"});
    assert_eq!(app.pn("POST", "/printjobs", key_b, Some(body)).0, 400);
    // and cancelling a foreign job affects nothing
    assert_eq!(
        app.pn("DELETE", &format!("/printjobs/{jid}"), key_b, None),
        (200, json!(0))
    );
    assert_eq!(job_state(&app.db, jid), "queued");
}

#[test]
fn pdf_options_reach_the_job_and_unknown_ones_are_ignored() {
    let app = App::new();
    let reg = register(&app.db, "win-1", "ak", &[decl("Office", true)], DEFAULT_ORG);
    let pid = reg.pid("Office");
    let (code, _) = pn(
        &app,
        "POST",
        "/printjobs",
        Some(
            json!({"printerId": pid, "contentType": "pdf_base64", "content": "QUJD", "qty": 2,
                    "options": {"paper": "A4", "duplex": "long-edge", "rotate": 90, "dpi": "300x300"}}),
        ),
    );
    assert_eq!(code, 201);
    let job = claim(&app.db, reg.computer_id).unwrap();
    assert_eq!(job.copies, 2);
    assert_eq!(
        job.options,
        Some(json!({"paper": "A4", "duplex": "long-edge"}))
    );
}
