// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Label templates over HTTP: the CRUD endpoints and the `label_uri` / `label_base64` /
//! `label_text` job types, which render through a template into an ordinary queued job.
mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::*;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};

/// A black 40x20 PNG: something every output can render.
fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([0, 0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn tpl(name: &str, content: &str) -> Value {
    json!({
        "name": name, "content": content,
        "label_mm": {"w": 50, "h": 30}, "dpi": 203, "rotate": 0,
        "crop_percent": {"t": 0, "r": 0, "b": 0, "l": 0},
        "place_mm": {"x": 0, "y": 0, "w": 50, "h": 30},
    })
}

struct Fixture {
    app: App,
    reg: Registered,
}

/// The default org with a client key "k1", a PDF printer "P" and a raw printer "Z" on one agent.
fn fixture() -> Fixture {
    let app = App::new();
    add_key(&app.db, "one", "k1", DEFAULT_ORG);
    let reg = register(
        &app.db,
        "pc",
        "ak",
        &[decl("P", true), decl("Z", false)],
        DEFAULT_ORG,
    );
    Fixture { app, reg }
}

fn make(f: &Fixture, body: Value) -> i64 {
    let (code, v) = f.app.post("/labels", Some("k1"), body);
    assert_eq!(code, 200, "{v}");
    v["id"].as_i64().unwrap()
}

fn fetched(f: &Fixture, bytes: Vec<u8>) {
    *f.app.fetch.reply.lock().unwrap() = Some(Ok(bytes));
}

/// The payload and mode of the one job the agent can claim.
fn claimed(f: &Fixture) -> (store::ClaimedJob, Vec<u8>) {
    let job = claim(&f.app.db, f.reg.computer_id).expect("a queued job");
    let data = f
        .app
        .db
        .with(|c| store::get_payload(c, job.job_id, f.reg.computer_id))
        .unwrap()
        .unwrap();
    (job, data)
}

fn err_of(v: &Value) -> &str {
    v["error"].as_str().unwrap()
}

// --- CRUD -------------------------------------------------------------------------------------

#[test]
fn crud_round_trip() {
    let f = fixture();
    let (code, made) = f.app.post("/labels", Some("k1"), tpl("shipping", "image"));
    assert_eq!(code, 200, "{made}");
    let id = made["id"].as_i64().unwrap();
    assert_eq!(made["name"], "shipping");
    assert_eq!(made["label_mm"], json!({"w": 50.0, "h": 30.0}));
    // the optional text block comes back with its defaults
    assert_eq!(made["text"], json!({"wrap_chars": 0, "lines_per_label": 0}));
    assert!(made["created_at"].as_f64().unwrap() > 0.0);
    assert!(made["updated_at"].as_f64().unwrap() >= made["created_at"].as_f64().unwrap());

    let (code, got) = f.app.get(&format!("/labels/{id}"), Some("k1"));
    assert_eq!((code, &got), (200, &made));

    let mut next = tpl("shipping-300", "image");
    next["dpi"] = json!(300);
    let (code, put) = f.app.put(&format!("/labels/{id}"), Some("k1"), next);
    assert_eq!(code, 200, "{put}");
    assert_eq!(
        (
            put["id"].as_i64(),
            put["name"].as_str(),
            put["dpi"].as_i64()
        ),
        (Some(id), Some("shipping-300"), Some(300))
    );
    assert_eq!(put["created_at"], made["created_at"]);
    let (_, got) = f.app.get(&format!("/labels/{id}"), Some("k1"));
    assert_eq!(got["name"], "shipping-300");

    let (code, del) = f.app.delete(&format!("/labels/{id}"), Some("k1"));
    assert_eq!((code, del), (200, json!({"ok": true})));
    assert_eq!(f.app.get(&format!("/labels/{id}"), Some("k1")).0, 404);
    assert_eq!(f.app.delete(&format!("/labels/{id}"), Some("k1")).0, 404);
    assert_eq!(
        f.app
            .put(&format!("/labels/{id}"), Some("k1"), tpl("x", "text"))
            .0,
        404
    );
}

#[test]
fn list_is_sorted_by_name() {
    let f = fixture();
    for n in ["b-label", "a-label", "c-label"] {
        make(&f, tpl(n, "text"));
    }
    let (code, v) = f.app.get("/labels", Some("k1"));
    assert_eq!(code, 200);
    let names: Vec<_> = v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a-label", "b-label", "c-label"]);
    // the root token has no org of its own and reads the default org's templates
    assert_eq!(
        f.app.get("/labels", Some("t")).1["templates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn needs_auth() {
    let f = fixture();
    let id = make(&f, tpl("x", "text"));
    for tok in [None, Some("wrong")] {
        assert_eq!(f.app.get("/labels", tok).0, 401);
        assert_eq!(f.app.post("/labels", tok, tpl("y", "text")).0, 401);
        assert_eq!(f.app.get(&format!("/labels/{id}"), tok).0, 401);
        assert_eq!(
            f.app.put(&format!("/labels/{id}"), tok, tpl("y", "text")).0,
            401
        );
        assert_eq!(f.app.delete(&format!("/labels/{id}"), tok).0, 401);
    }
    assert_eq!(
        f.app.get("/labels", Some("k1")).1["templates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_template_is_400() {
    let f = fixture();
    let id = make(&f, tpl("ok", "text"));
    let mut bad = Vec::new();
    for (field, value) in [
        ("rotate", json!(45)),
        ("dpi", json!(50)),
        ("name", json!("")),
        ("name", json!("bad,name")),
        ("content", json!("video")),
        ("surprise", json!(1)),
    ] {
        let mut t = tpl("t", "text");
        t[field] = value;
        bad.push(t);
    }
    let mut huge = tpl("t", "text");
    huge["label_mm"] = json!({"w": 5000, "h": 30});
    bad.push(huge);
    for t in bad {
        let (code, v) = f.app.post("/labels", Some("k1"), t.clone());
        assert_eq!(code, 400, "{t} -> {v}");
        assert!(!err_of(&v).is_empty());
        assert_eq!(f.app.put(&format!("/labels/{id}"), Some("k1"), t).0, 400);
    }
    // not an object at all
    assert_eq!(f.app.post("/labels", Some("k1"), json!([1])).0, 400);
    // nothing bad was stored, and the good one is untouched
    let (_, v) = f.app.get("/labels", Some("k1"));
    assert_eq!(v["templates"].as_array().unwrap().len(), 1);
    assert_eq!(v["templates"][0]["name"], "ok");
}

#[test]
fn duplicate_name_is_409() {
    let f = fixture();
    make(&f, tpl("same", "text"));
    let other = make(&f, tpl("other", "text"));
    let (code, v) = f.app.post("/labels", Some("k1"), tpl("same", "image"));
    assert_eq!(code, 409);
    assert_eq!(err_of(&v), "label template name already in use");
    // renaming onto a taken name is the same conflict; keeping your own name is fine
    let (code, v) = f
        .app
        .put(&format!("/labels/{other}"), Some("k1"), tpl("same", "text"));
    assert_eq!(
        (code, err_of(&v)),
        (409, "label template name already in use")
    );
    assert_eq!(
        f.app
            .put(
                &format!("/labels/{other}"),
                Some("k1"),
                tpl("other", "image")
            )
            .0,
        200
    );
    // another org may reuse the name
    let oid = create_org(&f.app.db, "acme");
    add_key(&f.app.db, "two", "k2", oid);
    assert_eq!(
        f.app.post("/labels", Some("k2"), tpl("same", "text")).0,
        200
    );
}

#[test]
fn other_org_cannot_see_or_use_a_template() {
    let f = fixture();
    let oid = create_org(&f.app.db, "acme");
    add_key(&f.app.db, "two", "k2", oid);
    let (_, theirs) = f.app.post("/labels", Some("k2"), tpl("theirs", "image"));
    let tid = theirs["id"].as_i64().unwrap();

    assert_eq!(f.app.get(&format!("/labels/{tid}"), Some("k1")).0, 404);
    assert_eq!(f.app.get("/labels", Some("k1")).1["templates"], json!([]));
    assert_eq!(
        f.app
            .put(&format!("/labels/{tid}"), Some("k1"), tpl("mine", "text"))
            .0,
        404
    );
    assert_eq!(f.app.delete(&format!("/labels/{tid}"), Some("k1")).0, 404);

    fetched(&f, png());
    for template in [json!("theirs"), json!(tid)] {
        let (code, v) = f.app.post(
            "/jobs",
            Some("k1"),
            json!({"printer_id": f.reg.pid("P"), "type": "label_uri",
                   "url": "https://x/img.png", "template": template}),
        );
        assert_eq!(code, 400, "{v}");
        assert!(err_of(&v).starts_with("unknown label template: "), "{v}");
    }
    assert!(
        f.app.fetch.gets.lock().unwrap().is_empty(),
        "nothing is fetched"
    );
    // still there for its owner
    assert_eq!(f.app.get(&format!("/labels/{tid}"), Some("k2")).0, 200);
}

#[test]
fn delete_org_removes_templates() {
    let f = fixture();
    let oid = create_org(&f.app.db, "acme");
    add_key(&f.app.db, "two", "k2", oid);
    f.app.post("/labels", Some("k2"), tpl("a", "text"));
    f.app.post("/labels", Some("k2"), tpl("b", "text"));
    make(&f, tpl("keep", "text"));
    let count = |org| {
        sql_i64(
            &f.app.db,
            "SELECT COUNT(*) FROM label_templates WHERE org_id=?",
            org,
        )
    };
    assert_eq!(count(oid), Some(2));
    assert!(f.app.db.with(|c| store::delete_org(c, oid)).unwrap());
    assert_eq!(count(oid), Some(0));
    assert_eq!(count(DEFAULT_ORG), Some(1));
}

// --- jobs -------------------------------------------------------------------------------------

fn label_job(f: &Fixture, printer: &str, extra: Value) -> (u16, Value) {
    let mut body = json!({"printer_id": f.reg.pid(printer)});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    f.app.post("/jobs", Some("k1"), body)
}

#[test]
fn label_uri_on_a_pdf_printer_queues_a_pdf() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, png());
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_uri", "url": "https://x/img.png", "template": "img", "copies": 2}),
    );
    assert_eq!(code, 200, "{v}");
    assert_eq!(*f.app.fetch.gets.lock().unwrap(), ["https://x/img.png"]);
    let (job, data) = claimed(&f);
    assert_eq!(
        (job.job_id, job.mode.as_str(), job.copies),
        (job_id(&v), "pdf", 2)
    );
    assert!(data.starts_with(b"%PDF"));
}

#[test]
fn label_uri_on_a_raw_printer_queues_zpl() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, png());
    let (code, v) = label_job(
        &f,
        "Z",
        json!({"type": "label_uri", "url": "https://x/img.png", "template": "img"}),
    );
    assert_eq!(code, 200, "{v}");
    let (job, data) = claimed(&f);
    assert_eq!(job.mode, "raw");
    let zpl = String::from_utf8(data).unwrap();
    assert!(
        zpl.starts_with("^XA") && zpl.contains("^GFA") && zpl.ends_with("^XZ"),
        "{zpl}"
    );
    // the gotcha: a raw printer never gets a PDF
    assert!(!zpl.contains("%PDF"));
}

#[test]
fn label_uri_needs_an_http_url() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    for url in [json!("file:///etc/passwd"), json!(5), Value::Null] {
        let (code, _) = label_job(
            &f,
            "P",
            json!({"type": "label_uri", "url": url, "template": "img"}),
        );
        assert_eq!(code, 400);
    }
    assert!(f.app.fetch.gets.lock().unwrap().is_empty());
}

#[test]
fn image_renders_are_limited_in_number() {
    // Rendering a large image costs hundreds of MB; only a few may run at once, the rest are told
    // to come back instead of taking the server down.
    let f = fixture();
    make(&f, tpl("img", "image"));
    let body = || json!({"type": "label_base64", "content": B64.encode(png()), "template": "img"});
    let held: Vec<_> = std::iter::from_fn(|| f.app.state.render_slots.try_enter()).collect();
    assert!(!held.is_empty());
    let (code, v) = label_job(&f, "Z", body());
    assert_eq!(code, 503, "{v}");
    assert!(err_of(&v).contains("busy"), "{v}");
    // text is cheap and does not need a slot
    make(&f, tpl("txt", "text"));
    let (code, v) = label_job(
        &f,
        "Z",
        json!({"type": "label_text", "text": "hello", "template": "txt"}),
    );
    assert_eq!(code, 200, "{v}");
    drop(held);
    let (code, v) = label_job(&f, "Z", body());
    assert_eq!(code, 200, "{v}");
}

#[test]
fn label_base64_renders() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    let content = B64.encode(png());
    let (code, v) = label_job(
        &f,
        "Z",
        json!({"type": "label_base64", "content": content, "template": "img"}),
    );
    assert_eq!(code, 200, "{v}");
    assert!(f.app.fetch.gets.lock().unwrap().is_empty());
    let (job, data) = claimed(&f);
    assert_eq!(job.mode, "raw");
    assert!(data.starts_with(b"^XA"));
    // same input on the PDF printer
    let (code, _) = label_job(
        &f,
        "P",
        json!({"type": "label_base64", "content": B64.encode(png()), "template": "img"}),
    );
    assert_eq!(code, 200);
    let (job, data) = claimed(&f);
    assert_eq!(job.mode, "pdf");
    assert!(data.starts_with(b"%PDF"));
    // the same error texts as raw_base64
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_base64", "content": "!!!notbase64!!!", "template": "img"}),
    );
    assert_eq!(code, 400);
    assert!(err_of(&v).starts_with("bad base64: "), "{v}");
    let (code, v) = label_job(&f, "P", json!({"type": "label_base64", "template": "img"}));
    assert_eq!((code, err_of(&v)), (400, "missing 'content'"));
}

#[test]
fn label_text_renders_zpl_and_pdf() {
    let f = fixture();
    let mut t = tpl("txt", "text");
    t["text"] = json!({"wrap_chars": 12, "lines_per_label": 2});
    make(&f, t);
    let (code, v) = label_job(
        &f,
        "Z",
        json!({"type": "label_text", "text": "B1^XZ^XA^PQ9999 Außen defekt", "template": "txt"}),
    );
    assert_eq!(code, 200, "{v}");
    let (job, data) = claimed(&f);
    assert_eq!(job.mode, "raw");
    let zpl = String::from_utf8(data).unwrap();
    assert!(zpl.starts_with("^XA") && zpl.contains("Außen"), "{zpl}");
    // the caller's text cannot add commands
    assert!(!zpl.contains("^PQ9999") && zpl.matches("^XA").count() == zpl.matches("^XZ").count());

    let (code, _) = label_job(
        &f,
        "P",
        json!({"type": "label_text", "text": "Hello", "template": "txt"}),
    );
    assert_eq!(code, 200);
    let (job, data) = claimed(&f);
    assert_eq!(job.mode, "pdf");
    assert!(data.starts_with(b"%PDF"));
    assert_eq!(f.app.fetch.gets.lock().unwrap().len(), 0);
}

#[test]
fn template_by_id_or_name() {
    let f = fixture();
    let id = make(&f, tpl("txt", "text"));
    for template in [json!("txt"), json!(id)] {
        let (code, v) = label_job(
            &f,
            "Z",
            json!({"type": "label_text", "text": "Hi", "template": template}),
        );
        assert_eq!(code, 200, "{v}");
        claimed(&f);
    }
}

#[test]
fn unknown_template_is_400_and_fetches_nothing() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, png());
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_uri", "url": "https://x/img.png", "template": "nope"}),
    );
    assert_eq!((code, err_of(&v)), (400, "unknown label template: nope"));
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_uri", "url": "https://x/img.png", "template": 9999}),
    );
    assert_eq!((code, err_of(&v)), (400, "unknown label template: 9999"));
    // an unknown printer is reported as such, also without a fetch
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": 4242, "type": "label_uri", "url": "https://x/i.png", "template": "img"}),
    );
    assert_eq!((code, err_of(&v)), (400, "unknown printer: 4242"));
    assert!(f.app.fetch.gets.lock().unwrap().is_empty());
    assert_eq!(f.app.get("/jobs", Some("k1")).1["jobs"], json!([]));
}

#[test]
fn bad_template_field_is_400() {
    let f = fixture();
    make(&f, tpl("txt", "text"));
    for extra in [
        json!({"type": "label_text", "text": "x"}),
        json!({"type": "label_text", "text": "x", "template": 1.5}),
        json!({"type": "label_text", "text": "x", "template": true}),
        json!({"type": "label_text", "text": "x", "template": ""}),
        json!({"type": "label_text", "template": "txt"}),
        json!({"type": "label_text", "text": 5, "template": "txt"}),
    ] {
        let (code, v) = label_job(&f, "Z", extra.clone());
        assert_eq!(code, 400, "{extra} -> {v}");
    }
    assert_eq!(f.app.get("/jobs", Some("k1")).1["jobs"], json!([]));
}

#[test]
fn text_job_on_image_template_is_400() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    make(&f, tpl("txt", "text"));
    fetched(&f, png());
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_text", "text": "x", "template": "img"}),
    );
    assert_eq!(code, 400, "{v}");
    // and the other way round, before anything is fetched
    for body in [
        json!({"type": "label_uri", "url": "https://x/i.png", "template": "txt"}),
        json!({"type": "label_base64", "content": B64.encode(png()), "template": "txt"}),
    ] {
        let (code, v) = label_job(&f, "P", body);
        assert_eq!(code, 400, "{v}");
    }
    assert!(f.app.fetch.gets.lock().unwrap().is_empty());
}

#[test]
fn broken_image_is_400() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, b"this is not an image".to_vec());
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_uri", "url": "https://x/i.png", "template": "img"}),
    );
    assert_eq!(code, 400, "{v}");
    assert!(err_of(&v).starts_with("label: "), "{v}");
    let (code, v) = label_job(
        &f,
        "Z",
        json!({"type": "label_base64", "content": B64.encode(b"garbage"), "template": "img"}),
    );
    assert_eq!(code, 400, "{v}");
    assert!(err_of(&v).starts_with("label: "), "{v}");
    // an empty text is a rendering error too
    make(&f, tpl("txt", "text"));
    let (code, v) = label_job(
        &f,
        "Z",
        json!({"type": "label_text", "text": " ^~ ", "template": "txt"}),
    );
    assert_eq!((code, err_of(&v)), (400, "label: text is empty"));
    assert_eq!(f.app.get("/jobs", Some("k1")).1["jobs"], json!([]));
}

#[test]
fn failed_fetch_is_502() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    *f.app.fetch.reply.lock().unwrap() = Some(Err("HTTP Error 404: Not Found".into()));
    let (code, v) = label_job(
        &f,
        "P",
        json!({"type": "label_uri", "url": "https://x/gone.png", "template": "img"}),
    );
    assert_eq!(code, 502);
    assert_eq!(
        err_of(&v),
        "downstream: fetch failed: HTTP Error 404: Not Found"
    );
}

#[test]
fn options_refused_on_raw_label() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, png());
    let body = |opts: Value| json!({"type": "label_uri", "url": "https://x/i.png", "template": "img", "options": opts});
    let (code, v) = label_job(&f, "Z", body(json!({"duplex": "long-edge"})));
    assert_eq!(
        (code, err_of(&v)),
        (400, "options are only supported on pdf jobs")
    );
    assert!(f.app.fetch.gets.lock().unwrap().is_empty());
    // the PDF printer takes them, as any pdf job does
    let (code, v) = label_job(&f, "P", body(json!({"duplex": "long-edge"})));
    assert_eq!(code, 200, "{v}");
    assert_eq!(claimed(&f).0.options, Some(json!({"duplex": "long-edge"})));
}

#[test]
fn a_label_job_keeps_the_orientation_it_asked_for() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, png());
    let body = json!({"type": "label_uri", "url": "https://x/i.png", "template": "img",
                      "options": {"orientation": "landscape"}});
    let (code, v) = label_job(&f, "P", body);
    assert_eq!(code, 200, "{v}");
    assert_eq!(
        claimed(&f).0.options,
        Some(json!({"orientation": "landscape"}))
    );
}

// A forced orientation halves a wide page on a tall sheet, where the default fills it. Which one a
// printer needs depends on its driver, so the server must not choose.
#[test]
fn a_label_job_gets_no_orientation_of_its_own() {
    let f = fixture();
    make(&f, tpl("img", "image"));
    fetched(&f, png());
    let body = json!({"type": "label_uri", "url": "https://x/i.png", "template": "img"});
    let (code, v) = label_job(&f, "P", body);
    assert_eq!(code, 200, "{v}");
    assert_eq!(claimed(&f).0.options, None);
}

#[test]
fn cloudprnt_printer_is_refused() {
    let f = fixture();
    make(&f, tpl("txt", "text"));
    let dev = f
        .app
        .db
        .with(|c| store::register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-mac1"))
        .unwrap();
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": dev.printer_id, "type": "label_text", "text": "x", "template": "txt"}),
    );
    assert_eq!(code, 400);
    assert_eq!(
        err_of(&v),
        "CloudPRNT printers cannot print label templates — send raw Star commands"
    );
    assert_eq!(f.app.get("/jobs", Some("k1")).1["jobs"], json!([]));
}

#[test]
fn label_jobs_keep_the_shared_job_rules() {
    // quota, idempotency, title and expiry are the ordinary job plumbing
    let f = fixture();
    make(&f, tpl("txt", "text"));
    let body = json!({"type": "label_text", "text": "x", "template": "txt",
                      "idempotency_key": "once", "title": "Box 7"});
    let (_, a) = label_job(&f, "Z", body.clone());
    let (_, b) = label_job(&f, "Z", body);
    assert_eq!(a["job_id"], b["job_id"]);
    let (code, _) = label_job(
        &f,
        "Z",
        json!({"type": "label_text", "text": "x", "template": "txt", "copies": 0}),
    );
    assert_eq!(code, 400);
    let (_, jobs) = f.app.get("/jobs", Some("k1"));
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(jobs["jobs"][0]["title"], "Box 7");
    assert_eq!(jobs["jobs"][0]["type"], "label_text");
}

// --- quota ------------------------------------------------------------------------------------

#[test]
fn an_org_past_its_quota_fetches_and_renders_nothing() {
    let f = fixture();
    make(&f, tpl("ship", "image"));
    fetched(&f, png());
    f.app
        .db
        .with(|c| store::set_org_quota(c, DEFAULT_ORG, Some(0)))
        .unwrap();
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": f.reg.printer_ids["P"], "type": "label_uri",
               "url": "https://x/img.png", "template": "ship"}),
    );
    assert_eq!(code, 402, "{v}");
    assert!(f.app.fetch.gets.lock().unwrap().is_empty());
}

#[test]
fn a_resubmit_past_the_quota_is_still_the_same_job() {
    let f = fixture();
    make(&f, tpl("ship", "image"));
    let job = json!({"printer_id": f.reg.printer_ids["P"], "type": "label_base64",
                     "content": B64.encode(png()), "template": "ship",
                     "idempotency_key": "order-1"});
    let (code, first) = f.app.post("/jobs", Some("k1"), job.clone());
    assert_eq!(code, 200, "{first}");
    f.app
        .db
        .with(|c| store::set_org_quota(c, DEFAULT_ORG, Some(1)))
        .unwrap();
    let (code, again) = f.app.post("/jobs", Some("k1"), job);
    assert_eq!(code, 200, "{again}");
    assert_eq!(first["job_id"], again["job_id"]);
}
