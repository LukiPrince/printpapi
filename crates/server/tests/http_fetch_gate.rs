// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The gate before an outbound fetch: for `*_uri` jobs the printer, the idempotency key, the quota
//! and a per-org cap on concurrent downloads are checked before the server fetches the payload.
mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::*;
use printpapi_server::dispatch::Fetcher;
use printpapi_server::server::Config;
use printpapi_server::store::DEFAULT_ORG;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([0, 0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// An app with a client key "k1" in the default org, whose printer "P" has the id returned.
fn app_with_printer() -> (App, i64) {
    let app = App::new();
    add_key(&app.db, "one", "k1", DEFAULT_ORG);
    let pid = register(&app.db, "pc", "ak", &[decl("P", true)], DEFAULT_ORG).pid("P");
    *app.fetch.reply.lock().unwrap() = Some(Ok(png()));
    (app, pid)
}

fn template(app: &App, name: &str, content: &str) {
    let (code, v) = app.post(
        "/labels",
        Some("k1"),
        json!({
            "name": name, "content": content,
            "label_mm": {"w": 50, "h": 30}, "dpi": 203, "rotate": 0,
            "crop_percent": {"t": 0, "r": 0, "b": 0, "l": 0},
            "place_mm": {"x": 0, "y": 0, "w": 50, "h": 30},
        }),
    );
    assert_eq!(code, 200, "{v}");
}

fn set_quota(app: &App, org: i64, quota: i64) {
    let (code, v) = app.put(
        &format!("/orgs/{org}"),
        Some("t"),
        json!({"job_quota": quota}),
    );
    assert_eq!(code, 200, "{v}");
}

fn fetched(app: &App) -> usize {
    app.fetch.gets.lock().unwrap().len()
}

fn err_of(v: &Value) -> &str {
    v["error"].as_str().unwrap()
}

/// One submit body per fetching job type, each with a fresh idempotency key.
fn fetching_bodies(pid: i64, n: usize) -> Vec<Value> {
    let key = |t: &str| format!("{t}-{n}");
    vec![
        json!({"printer_id": pid, "type": "raw_uri", "url": "https://x/a", "idempotency_key": key("r")}),
        json!({"printer_id": pid, "type": "pdf_uri", "url": "https://x/a", "idempotency_key": key("p")}),
        json!({"printer_id": pid, "type": "raw_uri_post", "url": "https://x/a", "json": {}, "idempotency_key": key("rp")}),
        json!({"printer_id": pid, "type": "pdf_uri_post", "url": "https://x/a", "json": {}, "idempotency_key": key("pp")}),
        json!({"printer_id": pid, "type": "label_uri", "url": "https://x/a", "template": "img", "idempotency_key": key("l")}),
    ]
}

#[test]
fn a_spent_quota_answers_402_without_fetching() {
    let (app, pid) = app_with_printer();
    template(&app, "img", "image");
    set_quota(&app, DEFAULT_ORG, 0);
    for round in 0..2 {
        for mut body in fetching_bodies(pid, round) {
            // round 0 sends a fresh key per request, round 1 none at all
            if round == 1 {
                body.as_object_mut().unwrap().remove("idempotency_key");
            }
            let (code, v) = app.post("/jobs", Some("k1"), body.clone());
            assert_eq!(code, 402, "{body} -> {v}");
        }
    }
    assert_eq!(fetched(&app), 0, "{:?}", app.fetch.gets.lock().unwrap());
}

#[test]
fn an_unknown_or_foreign_printer_answers_400_without_fetching() {
    let (app, _pid) = app_with_printer();
    let other = create_org(&app.db, "other");
    let foreign = register(&app.db, "pc2", "ak2", &[decl("Q", true)], other).pid("Q");
    for printer in [json!(4242), json!(foreign), json!("nope")] {
        let (code, v) = app.post(
            "/jobs",
            Some("k1"),
            json!({"printer_id": printer, "type": "raw_uri", "url": "https://x/a"}),
        );
        assert_eq!(code, 400, "{v}");
        assert!(err_of(&v).starts_with("unknown printer: "), "{v}");
    }
    assert_eq!(fetched(&app), 0);
}

#[test]
fn a_keyed_resubmit_returns_the_job_without_fetching_again() {
    let (app, pid) = app_with_printer();
    template(&app, "img", "image");
    for body in [
        json!({"printer_id": pid, "type": "raw_uri", "url": "https://x/a", "idempotency_key": "k-raw"}),
        json!({"printer_id": pid, "type": "label_uri", "url": "https://x/a", "template": "img", "idempotency_key": "k-label"}),
    ] {
        let before = fetched(&app);
        let (c1, v1) = app.post("/jobs", Some("k1"), body.clone());
        let (c2, v2) = app.post("/jobs", Some("k1"), body.clone());
        assert_eq!((c1, c2), (200, 200), "{v1} {v2}");
        assert_eq!(v1["job_id"], v2["job_id"], "{body}");
        assert_eq!(fetched(&app) - before, 1, "{body}");
    }
}

#[test]
fn a_keyed_label_text_resubmit_is_answered_before_the_quota() {
    // the keyed shortcut covers every label job: rendering costs a slot
    let (app, pid) = app_with_printer();
    template(&app, "txt", "text");
    let body = json!({"printer_id": pid, "type": "label_text", "text": "hi", "template": "txt", "idempotency_key": "k-t"});
    let (c1, v1) = app.post("/jobs", Some("k1"), body.clone());
    assert_eq!(c1, 200, "{v1}");
    // a spent quota cannot stop the resubmit of a job that already exists
    set_quota(&app, DEFAULT_ORG, 1);
    let (c2, v2) = app.post("/jobs", Some("k1"), body);
    assert_eq!(c2, 200, "{v2}");
    assert_eq!(v1["job_id"], v2["job_id"]);
}

/// "slow" URLs block until `release` (or two seconds), "bad" ones fail, the rest answer at once.
struct GatedFetcher {
    entered: AtomicUsize,
    open: Mutex<bool>,
    cv: Condvar,
}

impl GatedFetcher {
    fn new() -> Arc<Self> {
        Arc::new(GatedFetcher {
            entered: AtomicUsize::new(0),
            open: Mutex::new(false),
            cv: Condvar::new(),
        })
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn wait_entered(&self, n: usize) {
        for _ in 0..400 {
            if self.entered.load(Ordering::SeqCst) >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "only {} fetches started",
            self.entered.load(Ordering::SeqCst)
        );
    }
}

impl Fetcher for GatedFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        if url.contains("bad") {
            return Err("HTTP Error 404: Not Found".into());
        }
        if url.contains("slow") {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let g = self.open.lock().unwrap();
            let _ = self
                .cv
                .wait_timeout_while(g, Duration::from_secs(2), |open| !*open)
                .unwrap();
        }
        Ok(b"FETCHED".to_vec())
    }

    fn post_json(&self, url: &str, _body: &Value) -> Result<Vec<u8>, String> {
        self.get(url)
    }
}

fn uri_job(pid: i64, url: &str) -> Value {
    json!({"printer_id": pid, "type": "raw_uri", "url": url})
}

#[test]
fn one_org_cannot_hold_more_than_four_downloads_in_flight() {
    let mut app = App::with(|c| c.fetches_per_org = Some(4));
    let fetcher = GatedFetcher::new();
    app.state.fetcher = fetcher.clone();
    add_key(&app.db, "a", "ka", DEFAULT_ORG);
    let pa = register(&app.db, "pa", "aka", &[decl("P", false)], DEFAULT_ORG).pid("P");
    let org_b = create_org(&app.db, "b");
    add_key(&app.db, "b", "kb", org_b);
    let pb = register(&app.db, "pb", "akb", &[decl("P", false)], org_b).pid("P");
    std::thread::scope(|s| {
        let held: Vec<_> = (0..4)
            .map(|i| {
                let app = &app;
                s.spawn(move || {
                    app.post(
                        "/jobs",
                        Some("ka"),
                        uri_job(pa, &format!("https://x/slow{i}")),
                    )
                })
            })
            .collect();
        fetcher.wait_entered(4);
        // the fifth from the same org is turned away, one from another org still goes through
        let (code, v) = app.post("/jobs", Some("ka"), uri_job(pa, "https://x/fast"));
        assert_eq!(code, 503, "{v}");
        assert!(err_of(&v).contains("too many downloads"), "{v}");
        let (code, v) = app.post("/jobs", Some("kb"), uri_job(pb, "https://x/fast"));
        assert_eq!(code, 200, "{v}");
        // the PrintNode layer answers the same
        let r = app
            .call("POST", "/printjobs")
            .basic("ka")
            .json(&json!({"printerId": pa, "contentType": "raw_uri", "content": "https://x/fast", "source": "t"}))
            .send();
        assert_eq!(r.status, 503);
        fetcher.release();
        for h in held {
            assert_eq!(h.join().unwrap().0, 200);
        }
    });
    // every slot is free again, also after fetches that fail
    for _ in 0..6 {
        let (code, v) = app.post("/jobs", Some("ka"), uri_job(pa, "https://x/bad"));
        assert_eq!(code, 502, "{v}");
    }
    let (code, v) = app.post("/jobs", Some("ka"), uri_job(pa, "https://x/fast"));
    assert_eq!(code, 200, "{v}");
}

#[test]
fn the_printnode_layer_checks_before_it_fetches() {
    let (app, pid) = app_with_printer();
    set_quota(&app, DEFAULT_ORG, 0);
    let r = app
        .call("POST", "/printjobs")
        .basic("k1")
        .json(&json!({"printerId": pid, "contentType": "raw_uri", "content": "https://x/a", "source": "t"}))
        .send();
    assert_eq!(r.status, 402);
    assert_eq!(r.json()["code"], "QuotaExceeded");
    let r = app
        .call("POST", "/printjobs")
        .basic("k1")
        .json(&json!({"printerId": 4242, "contentType": "raw_uri", "content": "https://x/a", "source": "t"}))
        .send();
    assert_eq!(r.status, 400);
    assert_eq!(fetched(&app), 0);
}

#[test]
fn base64_jobs_keep_their_order_of_checks() {
    // no outbound IO: bad content still answers before an unknown printer does
    let (app, _pid) = app_with_printer();
    let (code, v) = app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": 4242, "type": "raw_base64", "content": "!!"}),
    );
    assert_eq!(code, 400);
    assert!(err_of(&v).starts_with("bad base64"), "{v}");
    let (code, v) = app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": 4242, "type": "raw_base64", "content": B64.encode("x")}),
    );
    assert_eq!(code, 400);
    assert!(err_of(&v).starts_with("unknown printer: "), "{v}");
}

/// Holds `n` slow downloads of one org, then submits one more fast `raw_uri` job; its status.
fn status_with_held(tweak: impl FnOnce(&mut Config), n: usize) -> u16 {
    let mut app = App::with(tweak);
    let fetcher = GatedFetcher::new();
    app.state.fetcher = fetcher.clone();
    add_key(&app.db, "a", "ka", DEFAULT_ORG);
    let pa = register(&app.db, "pa", "aka", &[decl("P", false)], DEFAULT_ORG).pid("P");
    std::thread::scope(|s| {
        let held: Vec<_> = (0..n)
            .map(|i| {
                let app = &app;
                s.spawn(move || {
                    app.post(
                        "/jobs",
                        Some("ka"),
                        uri_job(pa, &format!("https://x/slow{i}")),
                    )
                })
            })
            .collect();
        fetcher.wait_entered(n);
        let (code, _) = app.post("/jobs", Some("ka"), uri_job(pa, "https://x/fast"));
        fetcher.release();
        for h in held {
            assert_eq!(h.join().unwrap().0, 200);
        }
        code
    })
}

#[test]
fn without_open_signup_and_a_setting_there_is_no_per_org_cap() {
    assert_eq!(status_with_held(|_| {}, 6), 200);
}

#[test]
fn with_open_signup_and_no_setting_the_fifth_download_answers_503() {
    assert_eq!(status_with_held(|c| c.signup_open = true, 4), 503);
}

#[test]
fn an_explicit_setting_wins_either_way() {
    assert_eq!(
        status_with_held(
            |c| {
                c.signup_open = true;
                c.fetches_per_org = Some(0);
            },
            6
        ),
        200
    );
    assert_eq!(status_with_held(|c| c.fetches_per_org = Some(2), 2), 503);
}

#[test]
fn a_label_base64_job_needs_no_download_slot() {
    let mut app = App::with(|c| c.fetches_per_org = Some(4));
    let fetcher = GatedFetcher::new();
    app.state.fetcher = fetcher.clone();
    add_key(&app.db, "a", "ka", DEFAULT_ORG);
    let pa = register(&app.db, "pa", "aka", &[decl("P", true)], DEFAULT_ORG).pid("P");
    let (code, v) = app.post(
        "/labels",
        Some("ka"),
        json!({
            "name": "img", "content": "image",
            "label_mm": {"w": 50, "h": 30}, "dpi": 203, "rotate": 0,
            "crop_percent": {"t": 0, "r": 0, "b": 0, "l": 0},
            "place_mm": {"x": 0, "y": 0, "w": 50, "h": 30},
        }),
    );
    assert_eq!(code, 200, "{v}");
    std::thread::scope(|s| {
        let held: Vec<_> = (0..4)
            .map(|i| {
                let app = &app;
                s.spawn(move || {
                    app.post(
                        "/jobs",
                        Some("ka"),
                        uri_job(pa, &format!("https://x/slow{i}")),
                    )
                })
            })
            .collect();
        fetcher.wait_entered(4);
        let (code, v) = app.post(
            "/jobs",
            Some("ka"),
            json!({"printer_id": pa, "type": "label_base64", "template": "img", "content": B64.encode(png())}),
        );
        assert_eq!(code, 200, "{v}");
        fetcher.release();
        for h in held {
            h.join().unwrap();
        }
    });
}

#[test]
fn a_second_request_with_the_same_new_key_is_refused_while_the_first_fetches() {
    let mut app = App::new();
    let fetcher = GatedFetcher::new();
    app.state.fetcher = fetcher.clone();
    add_key(&app.db, "a", "ka", DEFAULT_ORG);
    let pa = register(&app.db, "pa", "aka", &[decl("P", false)], DEFAULT_ORG).pid("P");
    let body = json!({"printer_id": pa, "type": "raw_uri_post", "url": "https://x/slow", "json": {}, "idempotency_key": "K"});
    std::thread::scope(|s| {
        let first = {
            let (app, body) = (&app, body.clone());
            s.spawn(move || app.post("/jobs", Some("ka"), body))
        };
        fetcher.wait_entered(1);
        let (code, v) = app.post("/jobs", Some("ka"), body.clone());
        assert_eq!(code, 409, "{v}");
        assert!(err_of(&v).contains("still in progress"), "{v}");
        assert_eq!(fetcher.entered.load(Ordering::SeqCst), 1);
        fetcher.release();
        let (c1, v1) = first.join().unwrap();
        assert_eq!(c1, 200, "{v1}");
        let (c2, v2) = app.post("/jobs", Some("ka"), body.clone());
        assert_eq!(c2, 200, "{v2}");
        assert_eq!(v1["job_id"], v2["job_id"]);
        assert_eq!(fetcher.entered.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn a_failed_fetch_frees_its_key_again() {
    let (app, pid) = app_with_printer();
    *app.fetch.reply.lock().unwrap() = Some(Err("HTTP Error 500".into()));
    let body =
        json!({"printer_id": pid, "type": "raw_uri", "url": "https://x/a", "idempotency_key": "K"});
    for _ in 0..2 {
        let (code, v) = app.post("/jobs", Some("k1"), body.clone());
        assert_eq!(code, 502, "{v}");
    }
}

#[test]
fn label_uri_with_every_render_slot_taken_answers_503_without_fetching() {
    let (app, pid) = app_with_printer();
    template(&app, "img", "image");
    let _held: Vec<_> = (0..printpapi_server::server::RENDER_SLOTS)
        .map(|_| app.state.render_slots.try_enter().unwrap())
        .collect();
    let (code, v) = app.post(
        "/jobs",
        Some("k1"),
        json!({"printer_id": pid, "type": "label_uri", "url": "https://x/a", "template": "img"}),
    );
    assert_eq!(code, 503, "{v}");
    assert!(err_of(&v).contains("rendering is busy"), "{v}");
    assert_eq!(fetched(&app), 0);
}
