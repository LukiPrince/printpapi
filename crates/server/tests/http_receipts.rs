// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Receipts over HTTP: the job type, the profile routes and the two previews, on real routes over an
//! in-memory database. The pure side (layout, bytes, PDF) is in receipt_golden.rs.
mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::*;
use printpapi_server::server::{
    PREVIEWS_PER_SERVER, RECEIPT_SLOTS, RECEIPT_SLOTS_PER_ORG, RENDER_SLOTS,
};
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn png_b64() -> String {
    let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([0, 0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    B64.encode(out.into_inner())
}

fn text(t: &str) -> Value {
    json!({"cut": true, "content": [{"type": "text", "text": t}]})
}

fn with_image() -> Value {
    json!({"content": [{"type": "text", "text": "logo"}, {"type": "image", "content": png_b64()}]})
}

struct Fixture {
    app: App,
    reg: Registered,
}

/// The default org with a client key "k1", a raw printer "Z" and a PDF printer "P" on one agent.
fn fixture() -> Fixture {
    fixture_with(|_| {})
}

fn fixture_with(tweak: impl FnOnce(&mut printpapi_server::server::Config)) -> Fixture {
    let app = App::with(tweak);
    add_key(&app.db, "one", "k1", DEFAULT_ORG);
    let reg = register(
        &app.db,
        "pc",
        "ak",
        &[decl("Z", false), decl("P", true)],
        DEFAULT_ORG,
    );
    Fixture { app, reg }
}

fn job(f: &Fixture, printer: &str, receipt: Value) -> (u16, Value) {
    f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": f.reg.pid(printer), "type": "receipt", "receipt": receipt}),
    )
}

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

fn jobs_in_db(f: &Fixture) -> i64 {
    f.app
        .db
        .with(|c| c.query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0)))
        .unwrap()
}

fn copies_in_db(app: &App) -> i64 {
    app.db
        .with(|c| c.query_row("SELECT COUNT(*) FROM job_receipts", [], |r| r.get(0)))
        .unwrap()
}

fn preview(app: &App, token: &str, body: Value) -> Reply {
    app.call("POST", "/receipts/preview")
        .bearer(token)
        .json(&body)
        .send()
}

fn png_size(r: &Reply) -> (u32, u32) {
    assert_eq!(r.status, 200, "{}", r.text());
    image::load_from_memory(&r.body)
        .unwrap()
        .to_luma8()
        .dimensions()
}

// --- 31, 32 mode and options ----------------------------------------------------------------------

#[test]
fn a_receipt_on_a_raw_printer_is_escpos_bytes() {
    let f = fixture();
    let (code, v) = job(&f, "Z", text("Grüße"));
    assert_eq!(code, 200, "{v}");
    let (j, data) = claimed(&f);
    assert_eq!(j.mode, "raw");
    assert!(data.starts_with(&[0x1b, 0x40]) && data.ends_with(&[0x1b, 0x40]));
    // the type is kept in the history
    let (_, listed) = f.app.get("/jobs", Some("k1"));
    assert_eq!(listed["jobs"][0]["type"], "receipt");
    assert_eq!(listed["jobs"][0]["mode"], "raw");
}

#[test]
fn a_receipt_on_a_pdf_printer_is_a_pdf_unless_the_profile_says_escpos() {
    let f = fixture();
    let (code, _) = job(&f, "P", text("x"));
    assert_eq!(code, 200);
    let (j, data) = claimed(&f);
    assert_eq!(j.mode, "pdf");
    assert!(data.starts_with(b"%PDF-1."));
    // the same printer with output escpos
    let p = f.reg.pid("P");
    let (code, v) = f.app.put(
        &format!("/printers/{p}/receipt-profile"),
        Some("k1"),
        json!({"output": "escpos"}),
    );
    assert_eq!(code, 200, "{v}");
    let (code, _) = job(&f, "P", text("x"));
    assert_eq!(code, 200);
    let (j, data) = claimed(&f);
    assert_eq!(j.mode, "raw");
    assert!(data.starts_with(&[0x1b, 0x40]));
}

#[test]
fn a_receipt_never_reaches_a_raw_printer_as_a_pdf() {
    // whatever the profile says: the profile has no pdf value at all
    let f = fixture();
    let z = f.reg.pid("Z");
    for v in ["pdf", "PDF", "auto "] {
        let (code, _) = f.app.put(
            &format!("/printers/{z}/receipt-profile"),
            Some("k1"),
            json!({"output": v}),
        );
        assert_eq!(code, 400, "{v}");
    }
    let (code, _) = job(&f, "Z", text("x"));
    assert_eq!(code, 200);
    assert_eq!(claimed(&f).0.mode, "raw");
}

#[test]
fn options_are_for_pdf_output_only() {
    let f = fixture();
    let opts = json!({"duplex": "long-edge"});
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": f.reg.pid("P"), "type": "receipt", "receipt": text("x"), "options": opts}),
    );
    assert_eq!(code, 200, "{v}");
    assert_eq!(claimed(&f).0.options, Some(opts.clone()));
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": f.reg.pid("Z"), "type": "receipt", "receipt": text("x"), "options": opts}),
    );
    assert_eq!(code, 400);
    assert_eq!(err_of(&v), "options are only supported on pdf jobs");
    // a PDF printer told to send ESC/POS refuses them too
    let p = f.reg.pid("P");
    f.app.put(
        &format!("/printers/{p}/receipt-profile"),
        Some("k1"),
        json!({"output": "escpos"}),
    );
    let (code, _) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": p, "type": "receipt", "receipt": text("x"), "options": opts}),
    );
    assert_eq!(code, 400);
}

// --- 33 refusals ------------------------------------------------------------------------------------

#[test]
fn refused_receipts_make_no_job() {
    let f = fixture();
    let other = create_org(&f.app.db, "acme");
    let foreign = register(&f.app.db, "pc2", "ak2", &[decl("F", false)], other).pid("F");
    let dev = f
        .app
        .db
        .with(|c| store::register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-mac1"))
        .unwrap();
    let post = |body: Value| f.app.post("/jobs", Some("k1"), body);
    let z = f.reg.pid("Z");
    let cases = [
        (
            json!({"printer_id": 99999, "type": "receipt", "receipt": text("x")}),
            "unknown printer: 99999",
        ),
        (
            json!({"printer_id": foreign, "type": "receipt", "receipt": text("x")}),
            &format!("unknown printer: {foreign}"),
        ),
        (
            json!({"type": "receipt", "receipt": text("x")}),
            "unknown printer: None",
        ),
        (
            json!({"printer_id": dev.printer_id, "type": "receipt", "receipt": text("x")}),
            "CloudPRNT printers cannot print receipts — send raw Star commands",
        ),
        (
            json!({"printer_id": z, "type": "receipt"}),
            "missing 'receipt'",
        ),
        (
            json!({"printer_id": z, "type": "receipt", "receipt": null}),
            "missing 'receipt'",
        ),
        (
            json!({"printer_id": z, "type": "receipt", "receipt": {"content": [{"type": "text", "text": "x"}, {"type": "foo"}]}}),
            "receipt.content[1]: unknown type \"foo\"",
        ),
        (
            json!({"printer_id": z, "type": "receipt", "receipt": {"content": [{"type": "text", "text": "x", "size": 9}]}}),
            "receipt.content[0].size must be 1-8 or [w, h]",
        ),
        (
            json!({"printer_id": f.reg.pid("P"), "type": "receipt", "receipt": {"content": [{"type": "drawer"}]}}),
            "receipt prints nothing on a PDF printer",
        ),
        (
            json!({"printer_id": z, "type": "receipt", "receipt": {"content": [{"type": "qr", "data": "x".repeat(2000), "ec": "H"}]}}),
            "receipt.content[0]: qr data is too long for ec H",
        ),
    ];
    for (body, want) in cases {
        let (code, v) = post(body.clone());
        assert_eq!((code, err_of(&v)), (400, want), "{body}");
    }
    assert_eq!(jobs_in_db(&f), 0);
    assert_eq!(copies_in_db(&f.app), 0);
}

// --- 34 shared job rules ----------------------------------------------------------------------------

#[test]
fn the_ordinary_job_rules_apply() {
    let f = fixture();
    // copies and expire_after pass through, a title is kept
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": f.reg.pid("Z"), "type": "receipt", "receipt": text("x"),
               "copies": 3, "expire_after": 600, "title": "Bon 4711"}),
    );
    assert_eq!(code, 200, "{v}");
    let (j, _) = claimed(&f);
    assert_eq!(j.copies, 3);
    let (_, listed) = f.app.get("/jobs", Some("k1"));
    assert_eq!(listed["jobs"][0]["title"], "Bon 4711");
    let expires: Option<f64> = f
        .app
        .db
        .with(|c| {
            c.query_row("SELECT expires_at FROM jobs WHERE id=?", [j.job_id], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert!(expires.is_some());
}

#[test]
fn the_quota_stops_a_receipt() {
    let f = fixture();
    let (code, v) = f.app.put(
        &format!("/orgs/{DEFAULT_ORG}"),
        Some("t"),
        json!({"job_quota": 1}),
    );
    assert_eq!(code, 200, "{v}");
    assert_eq!(job(&f, "Z", text("a")).0, 200);
    let (code, v) = job(&f, "Z", text("b"));
    assert_eq!(code, 402, "{v}");
    assert_eq!(copies_in_db(&f.app), 1);
}

#[test]
fn a_resubmit_returns_the_same_job_without_rendering() {
    let f = fixture();
    let body = |r: Value| json!({"printer_id": f.reg.pid("Z"), "type": "receipt", "idempotency_key": "bon-4711", "receipt": r});
    let (_, a) = f.app.post("/jobs", Some("k1"), body(with_image()));
    // every receipt slot taken: a real render would be a 503, the resubmit is not rendered at all
    let _held: Vec<_> = (0..RECEIPT_SLOTS)
        .map(|_| f.app.state.receipt_slots.try_enter().unwrap())
        .collect();
    let started = Instant::now();
    let (code, b) = f.app.post("/jobs", Some("k1"), body(with_image()));
    assert_eq!(code, 200, "{b}");
    assert_eq!(b["job_id"], a["job_id"]);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(copies_in_db(&f.app), 1);
}

#[test]
fn a_locked_org_gets_no_receipt() {
    let f = fixture_with(|c| {
        c.operator_token = Some("op-token-op-token-op-token-op-token".into());
    });
    f.app.db.with(|c| {
        c.execute(
            "INSERT INTO org_locks(org_id, org_created_at, reason, locked_at) \
             SELECT id, created_at, 'testing the lock', 0 FROM orgs WHERE id=?",
            [DEFAULT_ORG],
        )
        .unwrap()
    });
    let (code, v) = job(&f, "Z", text("x"));
    assert_eq!(code, 403, "{v}");
    assert_eq!(copies_in_db(&f.app), 0);
}

// --- 35, 36 slots -----------------------------------------------------------------------------------

#[test]
fn one_org_cannot_hold_every_image_receipt_slot_where_the_org_cap_applies() {
    // a receipt that renders an image and then fails (a QR too long, say) costs no quota, so an
    // org could repeat it for ever and keep all four slots busy for everybody else; its own
    // receipts over the limit wait like the pool and then answer 503
    const { assert!(RECEIPT_SLOTS_PER_ORG < RECEIPT_SLOTS) };
    let f = fixture_with(|c| c.signup_open = true);
    let other = create_org(&f.app.db, "Other");
    add_key(&f.app.db, "two", "k2", other);
    let theirs = register(&f.app.db, "pc2", "ak2", &[decl("Z", false)], other);
    let held: Vec<_> = (0..RECEIPT_SLOTS_PER_ORG)
        .map(|_| {
            f.app
                .state
                .try_receipt_org_slot(DEFAULT_ORG)
                .unwrap()
                .unwrap()
        })
        .collect();
    let started = Instant::now();
    let (code, v) = job(&f, "Z", with_image());
    assert_eq!(code, 503, "{v}");
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "the org's own limit waits like the pool: {:?}",
        started.elapsed()
    );
    // a receipt without an image and another org are not affected
    assert_eq!(job(&f, "Z", text("plain")).0, 200);
    let (code, v) = f.app.post(
        "/jobs",
        Some("k2"),
        json!({"printer_id": theirs.pid("Z"), "type": "receipt", "receipt": with_image()}),
    );
    assert_eq!(code, 200, "{v}");
    drop(held);
    assert_eq!(job(&f, "Z", with_image()).0, 200);
    // the slot of a finished request is back
    assert_eq!(f.app.state.receipt_org_in_flight(DEFAULT_ORG), 0);
}

#[test]
fn an_image_receipt_over_the_org_limit_waits_for_its_own_slot_to_free() {
    // three receipts at once (kitchen, bar, customer copy) are ordinary traffic, not an attack
    let f = fixture_with(|c| c.signup_open = true);
    let held: Vec<_> = (0..RECEIPT_SLOTS_PER_ORG)
        .map(|_| {
            f.app
                .state
                .try_receipt_org_slot(DEFAULT_ORG)
                .unwrap()
                .unwrap()
        })
        .collect();
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let (code, v) = job(&f, "Z", with_image());
        assert_eq!(code, 200, "{v}");
    });
    assert_eq!(f.app.state.receipt_org_in_flight(DEFAULT_ORG), 0);
}

#[test]
fn without_the_org_cap_an_org_may_use_every_image_receipt_slot() {
    // a self-hosted box has one org: it keeps all four
    let f = fixture();
    let held: Vec<_> = (0..RECEIPT_SLOTS_PER_ORG)
        .map(|_| f.app.state.try_receipt_org_slot(DEFAULT_ORG).unwrap())
        .collect();
    assert!(held.iter().all(Option::is_none));
    assert_eq!(job(&f, "Z", with_image()).0, 200);
}

#[test]
fn image_receipts_wait_for_a_slot_and_fail_with_503_when_none_frees() {
    let f = fixture();
    let held: Vec<_> = (0..RECEIPT_SLOTS)
        .map(|_| f.app.state.receipt_slots.try_enter().unwrap())
        .collect();
    // everything that needs no slot still queues
    assert_eq!(job(&f, "Z", text("plain")).0, 200);
    assert_eq!(job(&f, "P", text("plain")).0, 200);
    // an image receipt waits, then answers 503
    let started = Instant::now();
    let (code, v) = job(&f, "Z", with_image());
    assert_eq!(code, 503, "{v}");
    assert_eq!(err_of(&v), "receipt rendering is busy, try again later");
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    // a slot that frees in time lets it through
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let (code, v) = job(&f, "Z", with_image());
        assert_eq!(code, 200, "{v}");
    });
}

#[test]
fn receipts_and_labels_do_not_share_slots() {
    let f = fixture();
    // all label render slots taken: receipts (even with images) still print
    let held: Vec<_> = (0..RENDER_SLOTS)
        .map(|_| f.app.state.render_slots.try_enter().unwrap())
        .collect();
    assert_eq!(job(&f, "Z", with_image()).0, 200);
    drop(held);
    // all receipt slots taken: a label_base64 job still renders
    let (code, _) = f.app.post(
        "/labels",
        Some("k1"),
        json!({"name": "img", "content": "image", "label_mm": {"w": 50, "h": 30}, "dpi": 203,
               "rotate": 0, "crop_percent": {"t": 0, "r": 0, "b": 0, "l": 0},
               "place_mm": {"x": 0, "y": 0, "w": 50, "h": 30}}),
    );
    assert_eq!(code, 200);
    let _held: Vec<_> = (0..RECEIPT_SLOTS)
        .map(|_| f.app.state.receipt_slots.try_enter().unwrap())
        .collect();
    let (code, v) = f.app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": f.reg.pid("P"), "type": "label_base64", "content": png_b64(), "template": "img"}),
    );
    assert_eq!(code, 200, "{v}");
}

#[test]
fn an_image_receipt_counts_against_the_orgs_download_cap() {
    let f = fixture_with(|c| c.fetches_per_org = Some(1));
    let z = f.reg.pid("Z");
    let body = |key: Option<&str>| {
        let mut b = json!({"printer_id": z, "type": "receipt", "receipt": with_image()});
        if let Some(k) = key {
            b["idempotency_key"] = json!(k);
        }
        b
    };
    // the first one takes the cap and holds its key, waiting for a render slot
    let held: Vec<_> = (0..RECEIPT_SLOTS)
        .map(|_| f.app.state.receipt_slots.try_enter().unwrap())
        .collect();
    std::thread::scope(|s| {
        let first = s.spawn(|| f.app.post("/jobs", Some("k1"), body(Some("K"))));
        // wait until the first request holds the org's one download slot (and its key)
        let deadline = Instant::now() + Duration::from_millis(1500);
        while f.app.state.fetches_in_flight(DEFAULT_ORG) == 0 {
            assert!(
                Instant::now() < deadline,
                "the first request never took its slot"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // a second image receipt of the org: refused at once, with the receipt text
        let started = Instant::now();
        let (code, v) = f.app.post("/jobs", Some("k1"), body(None));
        assert_eq!(code, 503, "{v}");
        assert_eq!(err_of(&v), "receipt rendering is busy, try again later");
        assert!(started.elapsed() < Duration::from_secs(1));
        // the same key while the first is in flight: 409
        let (code, v) = f.app.post("/jobs", Some("k1"), body(Some("K")));
        assert_eq!(code, 409, "{v}");
        // a receipt without images is not held up by any of it
        assert_eq!(job(&f, "Z", text("plain")).0, 200);
        drop(held);
        let (code, v) = first.join().unwrap();
        assert_eq!(code, 200, "{v}");
    });
}

// --- 37, 38 POST /receipts/preview ------------------------------------------------------------------

#[test]
fn the_preview_needs_a_credential_and_answers_a_png() {
    let f = fixture();
    let body = json!({"receipt": text("Grüße")});
    assert_eq!(f.app.post("/receipts/preview", None, body.clone()).0, 401);
    assert_eq!(
        f.app
            .post("/receipts/preview", Some("nope"), body.clone())
            .0,
        401
    );
    let r = preview(&f.app, "k1", body.clone());
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("image/png"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(png_size(&r), (576 + 32, 16 + 30 + 40 + 16));
    // a print key may preview (it may submit), a read key may not
    for (scope, want) in [("print", 200), ("read", 403), ("agent", 403)] {
        let (_, k) = f.app.post(
            "/apikeys",
            Some("t"),
            json!({"label": scope, "scope": scope}),
        );
        let r = preview(&f.app, k["key"].as_str().unwrap(), body.clone());
        assert_eq!(r.status, want, "{scope}");
    }
    // the same with a session and root
    let token = f.app.default_account();
    assert_eq!(preview(&f.app, &token, body.clone()).status, 200);
    assert_eq!(preview(&f.app, "t", body).status, 200);
}

#[test]
fn the_preview_takes_a_printer_or_a_profile() {
    let f = fixture();
    let z = f.reg.pid("Z");
    let rc = text("x");
    f.app.put(
        &format!("/printers/{z}/receipt-profile"),
        Some("k1"),
        json!({"paper_mm": 58}),
    );
    // the stored profile of the printer
    let r = preview(&f.app, "k1", json!({"receipt": rc, "printer_id": z}));
    assert_eq!(png_size(&r).0, 384 + 32);
    // a profile in the body
    let r = preview(
        &f.app,
        "k1",
        json!({"receipt": rc, "profile": {"width_dots": 192, "paper_mm": 58}}),
    );
    assert_eq!(png_size(&r).0, 192 + 32);
    // neither: the defaults; the other printer has none stored
    assert_eq!(
        png_size(&preview(&f.app, "k1", json!({"receipt": rc}))).0,
        576 + 32
    );
    assert_eq!(
        png_size(&preview(
            &f.app,
            "k1",
            json!({"receipt": rc, "printer_id": f.reg.pid("P")})
        ))
        .0,
        576 + 32
    );
    // both is an error, so is a bad receipt, a bad profile and an unknown printer
    for (body, want) in [
        (
            json!({"receipt": rc, "printer_id": z, "profile": {}}),
            "send printer_id or profile, not both",
        ),
        (json!({}), "missing 'receipt'"),
        (
            json!({"receipt": {"content": [{"type": "foo"}]}}),
            "receipt.content[0]: unknown type \"foo\"",
        ),
        (
            json!({"receipt": rc, "profile": {"paper_mm": 57}}),
            "paper_mm must be 58 or 80",
        ),
        (
            json!({"receipt": rc, "profile": []}),
            "profile must be an object",
        ),
        (
            json!({"receipt": rc, "printer_id": 999}),
            "unknown printer: 999",
        ),
        (
            json!({"receipt": rc, "printer_id": "abc"}),
            "unknown printer: abc",
        ),
    ] {
        let r = preview(&f.app, "k1", body.clone());
        assert_eq!(
            (r.status, r.json()["error"].as_str().unwrap().to_string()),
            (400, want.to_string()),
            "{body}"
        );
    }
    // a foreign printer reads like a missing one, a CloudPRNT device is refused with its own text
    let other = create_org(&f.app.db, "acme");
    let foreign = register(&f.app.db, "pc2", "ak2", &[decl("F", false)], other).pid("F");
    let r = preview(&f.app, "k1", json!({"receipt": rc, "printer_id": foreign}));
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!(format!("unknown printer: {foreign}")))
    );
    let dev = f
        .app
        .db
        .with(|c| store::register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-mac1"))
        .unwrap();
    let r = preview(
        &f.app,
        "k1",
        json!({"receipt": rc, "printer_id": dev.printer_id}),
    );
    assert_eq!(r.status, 400);
    assert_eq!(
        r.json()["error"],
        "CloudPRNT printers cannot print receipts — send raw Star commands"
    );
    // a receipt that prints nothing on a PDF printer is the same 400 as a job
    let r = preview(
        &f.app,
        "k1",
        json!({"receipt": {"content": [{"type": "drawer"}]}, "printer_id": f.reg.pid("P")}),
    );
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!("receipt prints nothing on a PDF printer"))
    );
    // ...but a raw printer's preview of a drawer-only receipt is just the drawer row
    assert_eq!(
        preview(
            &f.app,
            "k1",
            json!({"receipt": {"content": [{"type": "drawer"}]}, "printer_id": z})
        )
        .status,
        200
    );
}

#[test]
fn a_preview_prints_nothing_and_spends_no_quota() {
    let f = fixture();
    f.app.put(
        &format!("/orgs/{DEFAULT_ORG}"),
        Some("t"),
        json!({"job_quota": 1}),
    );
    for _ in 0..3 {
        assert_eq!(
            preview(&f.app, "k1", json!({"receipt": text("x")})).status,
            200
        );
    }
    assert_eq!(jobs_in_db(&f), 0);
    assert_eq!(copies_in_db(&f.app), 0);
    assert_eq!(job(&f, "Z", text("x")).0, 200);
}

#[test]
fn previews_have_a_small_budget_of_their_own() {
    let f = fixture();
    let rc = json!({"receipt": text("x")});
    // one per org: the org's own entry is taken
    let mine = f.app.state.try_preview(DEFAULT_ORG).unwrap();
    let r = preview(&f.app, "k1", rc.clone());
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (503, json!("preview is busy, try again shortly"))
    );
    // and again with root (key 0) while the org is taken: root has its own entry
    assert_eq!(preview(&f.app, "t", rc.clone()).status, 200);
    // prints and labels are unaffected
    assert_eq!(job(&f, "Z", text("x")).0, 200);
    assert_eq!(job(&f, "Z", with_image()).0, 200);
    drop(mine);
    assert_eq!(preview(&f.app, "k1", rc.clone()).status, 200);
    // two on the whole server: two other orgs hold the budget
    let (a, b) = (
        f.app.state.try_preview(101).unwrap(),
        f.app.state.try_preview(102).unwrap(),
    );
    assert!(f.app.state.try_preview(103).is_none());
    assert_eq!(PREVIEWS_PER_SERVER, 2);
    assert_eq!(preview(&f.app, "k1", rc.clone()).status, 503);
    assert_eq!(job(&f, "P", text("x")).0, 200);
    drop((a, b));
    // the entry is freed after an error behind the guard: this receipt parses (so the guard is
    // taken) and fails in the layout
    let too_wide = json!({"receipt": {"content": [{"type": "columns", "cells": [
        {"text": "a", "width": 100}, {"text": "b", "width": 100}]}]}});
    let r = preview(&f.app, "k1", too_wide);
    assert_eq!(r.status, 400);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("columns do not fit")
    );
    assert_eq!(preview(&f.app, "k1", rc).status, 200);
}

#[test]
fn a_locked_org_gets_no_preview_where_the_lock_shows() {
    let f = fixture_with(|c| c.operator_token = Some("op-token-op-token-op-token-op-token".into()));
    let rc = json!({"receipt": text("x")});
    assert_eq!(preview(&f.app, "k1", rc.clone()).status, 200);
    f.app.db.with(|c| {
        c.execute(
            "INSERT INTO org_locks(org_id, org_created_at, reason, locked_at) \
             SELECT id, created_at, 'testing the lock', 0 FROM orgs WHERE id=?",
            [DEFAULT_ORG],
        )
        .unwrap()
    });
    let r = preview(&f.app, "k1", rc.clone());
    assert_eq!(r.status, 403, "{}", r.text());
    // root naming a printer of the locked org is stopped too
    let r = preview(
        &f.app,
        "t",
        json!({"receipt": text("x"), "printer_id": f.reg.pid("Z")}),
    );
    assert_eq!(r.status, 403);
    // without the operator API there is no lock to honour
    let g = fixture();
    assert_eq!(preview(&g.app, "k1", rc).status, 200);
}

// --- 39, 40 GET /jobs/{id}/preview ------------------------------------------------------------------

#[test]
fn the_history_preview_shows_a_job_to_its_own_org() {
    let f = fixture();
    let other = create_org(&f.app.db, "acme");
    let theirs = f.app.account(other, "boss@acme.test");
    let mine = f.app.default_account();
    f.app.put(
        &format!("/printers/{}/receipt-profile", f.reg.pid("Z")),
        Some("k1"),
        json!({"paper_mm": 58}),
    );
    let (_, v) = job(&f, "Z", text("Grüße"));
    let id = v["job_id"].as_i64().unwrap();
    let path = format!("/jobs/{id}/preview");
    let get = |tok: &str| f.app.call("GET", &path).bearer(tok).send();
    // the session of the org sees it as printed: the 58 mm profile it was printed with
    let r = get(&mine);
    assert_eq!(r.header("content-type"), Some("image/png"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(png_size(&r).0, 384 + 32);
    // a later change of the profile does not change what the job shows
    f.app.delete(
        &format!("/printers/{}/receipt-profile", f.reg.pid("Z")),
        Some("k1"),
    );
    assert_eq!(png_size(&get(&mine)).0, 384 + 32);
    // another org's session: not found; root: any org
    assert_eq!(get(&theirs).status, 404);
    assert_eq!(get("t").status, 200);
    // a machine key, even a full one, is no manager
    assert_eq!(get("k1").status, 401);
    assert_eq!(f.app.call("GET", &path).send().status, 401);
    // an unknown id, and a job that is no receipt
    assert_eq!(
        f.app
            .call("GET", "/jobs/9999/preview")
            .bearer("t")
            .send()
            .status,
        404
    );
    let plain = job_id(&f.app.post("/jobs", Some("k1"), job_body(f.reg.pid("Z"))).1);
    assert_eq!(
        f.app
            .call("GET", &format!("/jobs/{plain}/preview"))
            .bearer("t")
            .send()
            .status,
        404
    );
    // the ordinary job routes did not change shape
    assert_eq!(f.app.get(&format!("/jobs/{id}"), Some("k1")).0, 200);
}

#[test]
fn a_purged_job_has_no_preview_and_the_preview_shares_the_budget() {
    let f = fixture();
    let mine = f.app.default_account();
    let (_, v) = job(&f, "Z", text("x"));
    let id = v["job_id"].as_i64().unwrap();
    let path = format!("/jobs/{id}/preview");
    // two entries for other orgs hold the whole budget
    let held = (
        f.app.state.try_preview(101).unwrap(),
        f.app.state.try_preview(102).unwrap(),
    );
    assert_eq!(f.app.call("GET", &path).bearer(&mine).send().status, 503);
    drop(held);
    assert_eq!(f.app.call("GET", &path).bearer(&mine).send().status, 200);
    // retention empties the payload: the trigger drops the copy, and the route says 404
    let (c, _) = (claim(&f.app.db, f.reg.computer_id).unwrap(), ());
    finish(&f.app.db, c.job_id, f.reg.computer_id, true, None);
    f.app.db.with(|c| {
        c.execute("UPDATE jobs SET payload=x'' WHERE id=?", [id])
            .unwrap()
    });
    assert_eq!(copies_in_db(&f.app), 0);
    assert_eq!(f.app.call("GET", &path).bearer(&mine).send().status, 404);
    assert_eq!(f.app.call("GET", &path).bearer("t").send().status, 404);
}

#[test]
fn with_the_operator_api_no_copy_is_kept_and_no_history_preview_exists() {
    let tweak = |c: &mut printpapi_server::server::Config| {
        c.operator_token = Some("op-token-op-token-op-token-op-token".into());
    };
    let f = fixture_with(tweak);
    let (code, v) = job(&f, "Z", text("secret"));
    assert_eq!(code, 200, "{v}");
    let id = v["job_id"].as_i64().unwrap();
    assert_eq!(copies_in_db(&f.app), 0);
    let mine = f.app.default_account();
    let other = create_org(&f.app.db, "acme");
    // root seeds a user into any org: still no copy to read
    let seeded = f.app.account(other, "root-made@acme.test");
    let path = format!("/jobs/{id}/preview");
    for tok in [mine.as_str(), "t", seeded.as_str()] {
        assert_eq!(f.app.call("GET", &path).bearer(tok).send().status, 404);
    }
    // a row written before the token was set is not served either
    let db = f.app.db.clone();
    drop(f);
    let before = App::on(db.clone(), |_| {});
    let reg = register(&db, "pc", "ak", &[decl("Z", false)], DEFAULT_ORG);
    let (code, v) = before.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": reg.pid("Z"), "type": "receipt", "receipt": text("old")}),
    );
    assert_eq!(code, 200, "{v}");
    let old = v["job_id"].as_i64().unwrap();
    assert_eq!(copies_in_db(&before), 1);
    let after = App::on(db, tweak);
    let token = after.account(DEFAULT_ORG, "second@shop.test");
    for tok in [token.as_str(), "t"] {
        assert_eq!(
            after
                .call("GET", &format!("/jobs/{old}/preview"))
                .bearer(tok)
                .send()
                .status,
            404
        );
    }
}

// --- 41 profiles ------------------------------------------------------------------------------------

#[test]
fn the_profile_routes() {
    let f = fixture();
    let z = f.reg.pid("Z");
    let path = format!("/printers/{z}/receipt-profile");
    let default = json!({"paper_mm": 80, "width_dots": 576, "codepage": "raster", "codepage_number": 16,
                         "cut": "partial", "output": "auto"});
    let (code, v) = f.app.get(&path, Some("k1"));
    assert_eq!(code, 200);
    assert_eq!(
        v,
        json!({"printer_id": z, "stored": false, "profile": default})
    );
    // put replaces the whole profile; omitted fields take the defaults
    let (code, v) = f.app.put(
        &path,
        Some("k1"),
        json!({"paper_mm": 58, "codepage": "cp1252", "cut": "none"}),
    );
    assert_eq!(code, 200, "{v}");
    let want = json!({"paper_mm": 58, "width_dots": 384, "codepage": "cp1252", "codepage_number": 16,
                      "cut": "none", "output": "auto"});
    assert_eq!(v, json!({"printer_id": z, "stored": true, "profile": want}));
    assert_eq!(f.app.get(&path, Some("k1")).1, v);
    let (_, v2) = f.app.put(&path, Some("k1"), json!({"output": "escpos"}));
    assert_eq!(v2["profile"]["paper_mm"], 80);
    // invalid bodies
    for body in [
        json!({"paper_mm": 57}),
        json!({"colour": 1}),
        json!({"width_dots": 4000}),
    ] {
        assert_eq!(f.app.put(&path, Some("k1"), body).0, 400);
    }
    assert_eq!(
        f.app
            .call("PUT", &path)
            .bearer("k1")
            .raw(b"[1]")
            .send()
            .status,
        400
    );
    // it survives the agent registering again
    register(
        &f.app.db,
        "pc",
        "ak",
        &[decl("Z", true), decl("P", false)],
        DEFAULT_ORG,
    );
    assert_eq!(
        f.app.get(&path, Some("k1")).1["profile"]["output"],
        "escpos"
    );
    // delete: the defaults again, ok for a printer without one, 404 for an unknown printer
    assert_eq!(f.app.delete(&path, Some("k1")), (200, json!({"ok": true})));
    assert_eq!(f.app.get(&path, Some("k1")).1["stored"], false);
    assert_eq!(f.app.delete(&path, Some("k1")).0, 200);
    assert_eq!(
        f.app.delete("/printers/9999/receipt-profile", Some("k1")).0,
        404
    );
    assert_eq!(
        f.app.get("/printers/9999/receipt-profile", Some("k1")).0,
        404
    );
    assert_eq!(
        f.app
            .put("/printers/9999/receipt-profile", Some("k1"), json!({}))
            .0,
        404
    );
}

#[test]
fn the_profile_routes_are_org_scoped_and_refuse_cloudprnt() {
    let f = fixture();
    let other = create_org(&f.app.db, "acme");
    let foreign = register(&f.app.db, "pc2", "ak2", &[decl("F", false)], other).pid("F");
    let p = format!("/printers/{foreign}/receipt-profile");
    assert_eq!(f.app.get(&p, Some("k1")).0, 404);
    assert_eq!(f.app.put(&p, Some("k1"), json!({})).0, 404);
    assert_eq!(f.app.delete(&p, Some("k1")).0, 404);
    // root sees every org's
    assert_eq!(f.app.get(&p, Some("t")).0, 200);
    let dev = f
        .app
        .db
        .with(|c| store::register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-mac1"))
        .unwrap();
    let p = format!("/printers/{}/receipt-profile", dev.printer_id);
    for (m, body) in [
        ("GET", json!(null)),
        ("PUT", json!({})),
        ("DELETE", json!(null)),
    ] {
        let r = f.app.call(m, &p).bearer("k1").json(&body).send();
        assert_eq!(r.status, 400, "{m}");
        assert_eq!(
            r.json()["error"],
            "CloudPRNT printers cannot print receipts — send raw Star commands"
        );
    }
    assert_eq!(f.app.get("/printers/x/receipt-profile", Some("k1")).0, 404);
}

// --- 42 nothing else changed ------------------------------------------------------------------------

fn shape(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn existing_responses_keep_their_shape() {
    let f = fixture();
    let plain = job_id(&f.app.post("/jobs", Some("k1"), job_body(f.reg.pid("Z"))).1);
    let before = (
        shape(&f.app.get("/printers", Some("k1")).1["printers"][0]),
        shape(&f.app.get("/jobs", Some("k1")).1["jobs"][0]),
        shape(&f.app.get(&format!("/jobs/{plain}"), Some("k1")).1),
    );
    f.app.put(
        &format!("/printers/{}/receipt-profile", f.reg.pid("Z")),
        Some("k1"),
        json!({"paper_mm": 58}),
    );
    let (_, v) = job(&f, "Z", text("x"));
    let rid = v["job_id"].as_i64().unwrap();
    let after = (
        shape(&f.app.get("/printers", Some("k1")).1["printers"][0]),
        shape(&f.app.get("/jobs", Some("k1")).1["jobs"][0]),
        shape(&f.app.get(&format!("/jobs/{rid}"), Some("k1")).1),
    );
    assert_eq!(before, after);
    // the printers list holds no profile and the job holds no document
    let text = f.app.get("/printers", Some("k1")).1.to_string();
    assert!(!text.contains("paper_mm") && !text.contains("receipt"));
    assert!(
        !f.app
            .get(&format!("/jobs/{rid}"), Some("k1"))
            .1
            .to_string()
            .contains("content")
    );
}

// --- 43 PrintNode -----------------------------------------------------------------------------------

#[test]
fn the_printnode_layer_neither_submits_nor_serves_receipts() {
    let f = fixture();
    let z = f.reg.pid("Z");
    let (code, v) = f.app.pn(
        "POST",
        "/printjobs",
        "k1",
        Some(json!({"printerId": z, "contentType": "receipt", "content": "x"})),
    );
    assert_eq!(code, 400, "{v}");
    // refused exactly as before: the list of supported types did not grow
    assert_eq!(
        v["message"],
        "unsupported contentType: 'receipt' (supported: pdf_uri, pdf_base64, raw_uri, raw_base64)"
    );
    // a receipt job made over the native API is listed with its contentType
    assert_eq!(job(&f, "Z", text("x")).0, 200);
    let (code, v) = f.app.pn("GET", "/printjobs", "k1", None);
    assert_eq!(code, 200, "{v}");
    assert_eq!(v[0]["contentType"], "receipt");
    // the receipt routes are no PrintNode routes: Basic is not a credential there
    for (m, p) in [
        ("GET", format!("/printers/{z}/receipt-profile")),
        ("PUT", format!("/printers/{z}/receipt-profile")),
        ("DELETE", format!("/printers/{z}/receipt-profile")),
        ("POST", "/receipts/preview".to_string()),
        ("GET", "/jobs/1/preview".to_string()),
    ] {
        let r = f
            .app
            .call(m, &p)
            .basic("k1")
            .json(&json!({"receipt": text("x")}))
            .send();
        assert_eq!(r.status, 401, "{m} {p}");
    }
}
