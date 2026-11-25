// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The agent's outbox against the server: the job identity handed out on request, the late result
//! route, and (below) the real agent client and journal against a real server over loopback.
mod common;

use common::*;
use printpapi_agent::client::{Client, Http, HttpError};
use printpapi_agent::config::{HttpSettings, Printer};
use printpapi_agent::journal::{self, Journal};
use printpapi_agent::print::{Fns, write_to_file};
use printpapi_agent::run::{JobDone, Outbox, PrinterMap, run_once};
use printpapi_server::auth::Scope;
use printpapi_server::dispatch::HttpFetcher;
use printpapi_server::http::spawn_test_server;
use printpapi_server::mail::SmtpMailer;
use printpapi_server::server::{AppState, Config};
use printpapi_server::store::Db;
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};
use std::cell::Cell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

fn agent_key(app: &App, key: &str) -> (i64, i64) {
    let r = register(&app.db, "pc", key, &[decl("Z", false)], DEFAULT_ORG);
    (r.computer_id, r.pid("Z"))
}

fn reaper_pass(app: &App, now: f64) {
    app.db
        .with(|c| store::requeue_stale(c, 300.0, 2, Some(now), false))
        .unwrap();
}

fn late(app: &App, key: &str, body: Value) -> Reply {
    app.agent("POST", "/agent/results", key, Some(body))
}

// --- GET /agent/jobs: the identity only on request ----------------------------------------------

#[test]
fn the_claim_has_the_five_keys_of_today_without_the_header() {
    let app = App::new();
    app.db
        .with(|c| store::add_api_key(c, "k", "ak", DEFAULT_ORG))
        .unwrap();
    let (_, pid) = agent_key(&app, "ak");
    enqueue(&app.db, pid, b"x", |_| {});
    let r = app.agent("GET", "/agent/jobs", "ak", None);
    assert_eq!(r.status, 200);
    let keys: Vec<String> = r.json().as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["job_id", "printer_id", "mode", "copies", "options"]);
}

#[test]
fn the_claim_has_job_uid_when_the_header_asks_for_it() {
    for header in ["job-uid", "other, job-uid", " JOB-UID ,x"] {
        let app = App::new();
        let (_, pid) = agent_key(&app, "ak");
        let jid = enqueue(&app.db, pid, b"x", |_| {});
        let r = app
            .call("GET", "/agent/jobs")
            .bearer("ak")
            .header("x-printapi-features", header)
            .send();
        assert_eq!(r.status, 200, "{header}");
        let created = sql_f64(&app.db, "SELECT created_at FROM jobs WHERE id=?", jid).unwrap();
        assert_eq!(
            r.json()["job_uid"],
            store::job_uid(jid, created),
            "{header}"
        );
    }
    // a header that names something else does not
    let app = App::new();
    let (_, pid) = agent_key(&app, "ak");
    enqueue(&app.db, pid, b"x", |_| {});
    let r = app
        .call("GET", "/agent/jobs")
        .bearer("ak")
        .header("x-printapi-features", "job-uid-v2, nothing")
        .send();
    assert!(r.json().get("job_uid").is_none());
}

// --- POST /agent/results ------------------------------------------------------------------------

#[test]
fn results_need_the_agent_key() {
    let app = App::new();
    agent_key(&app, "ak");
    let body = json!({"job_id": 1, "job_uid": "ab", "ok": true});
    for key in [None, Some("wrong"), Some("")] {
        let mut c = app.call("POST", "/agent/results").json(&body);
        if let Some(k) = key {
            c = c.bearer(k);
        }
        assert_eq!(c.send().status, 401, "{key:?}");
    }
    // an ordinary client key is no agent
    app.db
        .with(|c| store::add_api_key(c, "client", "ck", DEFAULT_ORG))
        .unwrap();
    assert_eq!(late(&app, "ck", body.clone()).status, 401);
    // and 401 comes before the body is read
    assert_eq!(
        app.call("POST", "/agent/results").raw(b"{{").send().status,
        401
    );
}

#[test]
fn a_bad_body_is_a_400() {
    let app = App::new();
    agent_key(&app, "ak");
    for body in [
        json!([1]),
        json!("x"),
        json!({}),
        json!({"job_id": 1}),
        json!({"job_uid": "ab"}),
        json!({"job_id": "1", "job_uid": "ab"}),
        json!({"job_id": 1.5, "job_uid": "ab"}),
        json!({"job_id": 1, "job_uid": 5}),
        json!({"job_id": null, "job_uid": null}),
    ] {
        let r = late(&app, "ak", body.clone());
        assert_eq!(r.status, 400, "{body}");
    }
    let r = app
        .call("POST", "/agent/results")
        .bearer("ak")
        .raw(b"not json")
        .send();
    assert_eq!(r.status, 400);
    assert_eq!(
        late(&app, "ak", json!({"job_id": 1})).json()["error"],
        "job_id and job_uid required"
    );
}

#[test]
fn an_unknown_job_is_answered_with_state_null() {
    let app = App::new();
    agent_key(&app, "ak");
    let r = late(
        &app,
        "ak",
        json!({"job_id": 99, "job_uid": "ab", "ok": true}),
    );
    assert_eq!(r.status, 200);
    assert_eq!(r.json(), json!({"ok": true, "state": null}));
}

#[test]
fn a_late_result_finishes_the_job_and_a_repeat_changes_nothing() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let jid = enqueue(&app.db, pid, b"x", |_| {});
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    let body = json!({"job_id": jid, "job_uid": uid, "ok": true});
    for _ in 0..2 {
        let r = late(&app, "ak", body.clone());
        assert_eq!(
            (r.status, r.json()),
            (200, json!({"ok": true, "state": "done"}))
        );
    }
    assert_eq!(job_state(&app.db, jid), "done");
    // a failure after that does not undo it
    let r = late(
        &app,
        "ak",
        json!({"job_id": jid, "job_uid": uid, "ok": false, "error": "x"}),
    );
    assert_eq!(r.json()["state"], "done");
}

#[test]
fn a_late_failure_is_recorded_with_its_error() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let jid = enqueue(&app.db, pid, b"x", |j| j.copies = 3);
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    let r = late(
        &app,
        "ak",
        json!({"job_id": jid, "job_uid": uid, "ok": false, "error": "copy 2 of 3 failed"}),
    );
    assert_eq!(r.json()["state"], "failed");
    assert_eq!(
        job_error(&app.db, jid).as_deref(),
        Some("copy 2 of 3 failed")
    );
}

#[test]
fn a_requeued_job_takes_the_late_result_and_prints_no_second_time() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let jid = enqueue(&app.db, pid, b"x", |_| {});
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    reaper_pass(&app, store_now() + 1000.0);
    assert_eq!(job_state(&app.db, jid), "queued");
    let r = late(
        &app,
        "ak",
        json!({"job_id": jid, "job_uid": uid, "ok": true}),
    );
    assert_eq!(r.json()["state"], "done");
    assert!(claim(&app.db, aid).is_none());
}

fn store_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[test]
fn another_job_with_the_same_id_is_not_touched() {
    // a database reset: the id is used again, the agent's uid belongs to the old job
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let jid = enqueue(&app.db, pid, b"x", |_| {});
    claim(&app.db, aid).unwrap();
    let old = store::job_uid(jid, 1.0);
    let r = late(
        &app,
        "ak",
        json!({"job_id": jid, "job_uid": old, "ok": true}),
    );
    assert_eq!(r.json(), json!({"ok": true, "state": null}));
    assert_eq!(job_state(&app.db, jid), "claimed");
}

#[test]
fn another_agents_job_is_not_touched() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    register(&app.db, "other", "bk", &[decl("Z", false)], DEFAULT_ORG);
    let jid = enqueue(&app.db, pid, b"x", |_| {});
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    let r = late(
        &app,
        "bk",
        json!({"job_id": jid, "job_uid": uid, "ok": true}),
    );
    assert_eq!(r.json()["state"], Value::Null);
    assert_eq!(job_state(&app.db, jid), "claimed");
}

#[test]
fn a_narrowed_key_is_a_401_here_and_delivers_when_the_right_is_back() {
    let app = App::new();
    let kid = app
        .db
        .with(|c| store::add_api_key_scoped(c, "pc", "ak", DEFAULT_ORG, Scope::Full))
        .unwrap();
    let r = register(&app.db, "pc", "ak", &[decl("Z", false)], DEFAULT_ORG);
    let jid = enqueue(&app.db, r.pid("Z"), b"x", |_| {});
    let uid = claim(&app.db, r.computer_id).unwrap().job_uid.unwrap();
    let body = json!({"job_id": jid, "job_uid": uid, "ok": true});
    app.db
        .with(|c| store::set_api_key_scope(c, kid, None, Scope::Print))
        .unwrap();
    assert_eq!(late(&app, "ak", body.clone()).status, 401);
    assert_eq!(job_state(&app.db, jid), "claimed");
    app.db
        .with(|c| store::set_api_key_scope(c, kid, None, Scope::Agent))
        .unwrap();
    assert_eq!(late(&app, "ak", body).json()["state"], "done");
}

#[test]
fn only_the_agent_and_full_scopes_may_post_a_late_result() {
    for (scope, want) in [
        (Scope::Full, 200),
        (Scope::Agent, 200),
        (Scope::CloudPrnt, 401),
        (Scope::Print, 401),
        (Scope::Read, 401),
    ] {
        let app = App::new();
        let kid = app
            .db
            .with(|c| store::add_api_key_scoped(c, "pc", "ak", DEFAULT_ORG, Scope::Full))
            .unwrap();
        let r = register(&app.db, "pc", "ak", &[decl("Z", false)], DEFAULT_ORG);
        let jid = enqueue(&app.db, r.pid("Z"), b"x", |_| {});
        let uid = claim(&app.db, r.computer_id).unwrap().job_uid.unwrap();
        app.db
            .with(|c| store::set_api_key_scope(c, kid, None, scope))
            .unwrap();
        let reply = late(
            &app,
            "ak",
            json!({"job_id": jid, "job_uid": uid, "ok": true}),
        );
        assert_eq!(reply.status, want, "{scope:?}");
        assert_eq!(
            job_state(&app.db, jid),
            if want == 200 { "done" } else { "claimed" }
        );
    }
    // a word this version does not know allows nothing
    let app = App::new();
    let kid = app
        .db
        .with(|c| store::add_api_key_scoped(c, "pc", "ak", DEFAULT_ORG, Scope::Full))
        .unwrap();
    register(&app.db, "pc", "ak", &[decl("Z", false)], DEFAULT_ORG);
    app.db.with(|c| {
        c.execute("UPDATE api_keys SET scope='martian' WHERE id=?", [kid])
            .unwrap()
    });
    let body = json!({"job_id": 1, "job_uid": "ab", "ok": true});
    assert_eq!(late(&app, "ak", body).status, 401);
}

// --- POST /agent/jobs/{id}/result ---------------------------------------------------------------

#[test]
fn the_old_result_route_still_answers_404_for_a_requeued_job() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let jid = enqueue(&app.db, pid, b"x", |_| {});
    claim(&app.db, aid).unwrap();
    reaper_pass(&app, store_now() + 1000.0);
    let r = app.agent(
        "POST",
        &format!("/agent/jobs/{jid}/result"),
        "ak",
        Some(json!({"ok": true})),
    );
    assert_eq!(r.status, 404);
    assert_eq!(job_state(&app.db, jid), "queued");
}

#[test]
fn the_old_result_route_without_job_uid_is_as_before_and_with_it_checks_the_job() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let j1 = enqueue(&app.db, pid, b"x", |_| {});
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    let url = format!("/agent/jobs/{j1}/result");
    // a uid of another job: 404 and the job is untouched
    let wrong = store::job_uid(j1, 1.0);
    let r = app.agent(
        "POST",
        &url,
        "ak",
        Some(json!({"ok": true, "job_uid": wrong})),
    );
    assert_eq!(r.status, 404);
    assert_eq!(job_state(&app.db, j1), "claimed");
    // the right one: today's answer
    let r = app.agent(
        "POST",
        &url,
        "ak",
        Some(json!({"ok": true, "job_uid": uid})),
    );
    assert_eq!((r.status, r.json()), (200, json!({"ok": true})));
    assert_eq!(job_state(&app.db, j1), "done");
    // absent: exactly today's behaviour
    let j2 = enqueue(&app.db, pid, b"y", |_| {});
    claim(&app.db, aid).unwrap();
    let r = app.agent(
        "POST",
        &format!("/agent/jobs/{j2}/result"),
        "ak",
        Some(json!({"ok": false, "error": "e"})),
    );
    assert_eq!(r.status, 200);
    assert_eq!(job_state(&app.db, j2), "failed");
}

// --- webhooks -----------------------------------------------------------------------------------

#[test]
fn a_late_ok_after_the_reaper_failed_the_job_fires_the_webhook_again_with_done() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let jid = enqueue(&app.db, pid, b"x", |j| {
        j.callback_url = Some("https://hook.test/a".into())
    });
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    app.db.with(|c| {
        c.execute("UPDATE jobs SET retries=2 WHERE id=?", [jid])
            .unwrap()
    });
    reaper_pass(&app, store_now() + 1000.0);
    assert_eq!(job_state(&app.db, jid), "failed");
    let posted = std::sync::Mutex::new(Vec::<Value>::new());
    let post = |_: &str, v: &Value| {
        posted.lock().unwrap().push(v.clone());
        Ok(())
    };
    printpapi_server::server::deliver_webhooks(&app.db, &post, 5);
    late(
        &app,
        "ak",
        json!({"job_id": jid, "job_uid": uid, "ok": true}),
    );
    printpapi_server::server::deliver_webhooks(&app.db, &post, 5);
    printpapi_server::server::deliver_webhooks(&app.db, &post, 5);
    let states: Vec<String> = posted
        .lock()
        .unwrap()
        .iter()
        .map(|v| v["state"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(states, ["failed", "done"]);
}

#[test]
fn maybe_printed_is_sent_only_while_the_outcome_is_unknown() {
    let app = App::new();
    let (aid, pid) = agent_key(&app, "ak");
    let cb = |j: &mut store::NewJob<'_>| j.callback_url = Some("https://hook.test/a".into());
    let lost = enqueue(&app.db, pid, b"x", cb);
    let uid = claim(&app.db, aid).unwrap().job_uid.unwrap();
    reaper_pass(&app, store_now() + 1000.0);
    let never = enqueue(&app.db, pid, b"y", cb);
    app.db.with(|c| {
        c.execute("UPDATE jobs SET expires_at=1", []).unwrap();
        assert_eq!(store::expire_jobs(c, None).unwrap(), 2);
    });
    let posted = std::sync::Mutex::new(Vec::<Value>::new());
    let post = |_: &str, v: &Value| {
        posted.lock().unwrap().push(v.clone());
        Ok(())
    };
    printpapi_server::server::deliver_webhooks(&app.db, &post, 5);

    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    const STATUS: [&str; 5] = ["state", "error", "printer_id", "created_at", "finished_at"];
    const HOOK: [&str; 5] = ["job_id", "state", "error", "title", "printer_id"];
    let status = |id: i64| app.get(&format!("/jobs/{id}"), Some("t")).1;

    // handed out and never answered: said in the job, the list and the webhook, after today's fields
    let j = status(lost);
    assert_eq!(
        (&j["error"], &j["maybe_printed"]),
        (&json!("expired"), &json!(true))
    );
    assert_eq!(keys(&j), [&STATUS[..], &["maybe_printed"]].concat());
    // never handed out: byte for byte what it was
    assert_eq!(keys(&status(never)), STATUS);
    let list = app.get("/jobs", Some("t")).1;
    for row in list["jobs"].as_array().unwrap() {
        let want = (row["id"] == lost).then_some(&json!(true)).cloned();
        assert_eq!(row.get("maybe_printed").cloned(), want, "{row}");
    }
    let hook = |id: i64| -> Vec<Value> {
        let sent = posted.lock().unwrap();
        sent.iter().filter(|v| v["job_id"] == id).cloned().collect()
    };
    assert_eq!(
        keys(&hook(lost)[0]),
        [&HOOK[..], &["maybe_printed"]].concat()
    );
    assert_eq!(hook(lost)[0]["maybe_printed"], json!(true));
    assert_eq!(keys(&hook(never)[0]), HOOK);
    // the PrintNode shapes do not know it
    let pn = app.pn("GET", "/printjobs", "t", None).1;
    assert!(!pn.to_string().contains("maybe_printed"), "{pn}");

    // the late result settles it
    let body = json!({"job_id": lost, "job_uid": uid, "ok": true});
    assert_eq!(late(&app, "ak", body).status, 200);
    printpapi_server::server::deliver_webhooks(&app.db, &post, 5);
    assert_eq!(keys(&status(lost)), STATUS);
    assert_eq!(hook(lost)[1]["state"], "done");
    assert_eq!(keys(&hook(lost)[1]), HOOK);
}

// --- the long poll over a real socket -----------------------------------------------------------

/// One raw GET /agent/jobs: status and JSON body, with or without the features header.
fn raw_claim(addr: std::net::SocketAddr, key: &str, features: Option<&str>) -> (u16, Value) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let extra = features.map_or(String::new(), |f| format!("X-Printapi-Features: {f}\r\n"));
    write!(
        s,
        "GET /agent/jobs HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer {key}\r\n{extra}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw[9..12].parse().unwrap();
    let body = raw.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

#[test]
fn the_long_poll_hands_out_job_uid_only_when_asked() {
    let state = Arc::new(AppState::new(
        Config::new("root-token"),
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ));
    let addr = spawn_test_server(state.clone());
    let r = register(&state.db, "pc", "ak", &[decl("Z", false)], DEFAULT_ORG);
    for features in [None, Some("job-uid")] {
        let jid = enqueue(&state.db, r.pid("Z"), b"x", |_| {});
        let (status, body) = raw_claim(addr, "ak", features);
        assert_eq!(status, 200);
        assert_eq!(body["job_id"], jid);
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        if features.is_some() {
            let created =
                sql_f64(&state.db, "SELECT created_at FROM jobs WHERE id=?", jid).unwrap();
            assert_eq!(body["job_uid"], store::job_uid(jid, created));
            assert_eq!(
                keys,
                [
                    "copies",
                    "job_id",
                    "job_uid",
                    "mode",
                    "options",
                    "printer_id"
                ]
            );
        } else {
            assert_eq!(keys, ["copies", "job_id", "mode", "options", "printer_id"]);
        }
    }
}

// --- the real agent client and journal against a real server ------------------------------------

/// The real client with switches that make the result posts fail, like a dropped uplink.
struct Flaky<'a> {
    inner: &'a Client,
    fail_old_route: Cell<bool>,
    fail_late_route: Cell<bool>,
}

impl Http for Flaky<'_> {
    fn get(&self, url: &str, key: &str) -> Result<Option<Value>, HttpError> {
        self.inner.get(url, key)
    }
    fn get_bytes(&self, url: &str, key: &str) -> Result<Option<Vec<u8>>, HttpError> {
        self.inner.get_bytes(url, key)
    }
    fn post(&self, url: &str, key: &str, body: &Value) -> Result<Option<Value>, HttpError> {
        let late = url.ends_with("/agent/results");
        let old = url.contains("/agent/jobs/") && url.ends_with("/result");
        if (late && self.fail_late_route.get()) || (old && self.fail_old_route.get()) {
            return Err(HttpError::Os("connection failed: uplink down".into()));
        }
        self.inner.post(url, key, body)
    }
}

struct World {
    state: Arc<AppState>,
    base: String,
    client: Client,
    key: String,
    printers: PrinterMap,
    dir: tempfile::TempDir,
}

fn file_uri(dir: &Path) -> String {
    let s = dir.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// A server with one agent key, one enrolled computer and a file:// printer that archives into
/// a folder of the world.
fn world_in(dir: tempfile::TempDir) -> World {
    let mut cfg = Config::new("root-token");
    // a poll that finds nothing ends quickly, so a test need not wait 25 s for it
    cfg.long_poll_timeout_s = 0.3;
    let state = Arc::new(AppState::new(
        cfg,
        Db::open(":memory:").unwrap(),
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::new(Duration::from_secs(1))),
    ));
    let base = format!("http://{}", spawn_test_server(state.clone()));
    let client = Client::new(&HttpSettings::default());
    let issued = client
        .post(
            &format!("{base}/apikeys"),
            "root-token",
            &json!({"label": "pc", "scope": "agent"}),
        )
        .unwrap()
        .unwrap();
    let key = issued["key"].as_str().unwrap().to_string();
    let reg = client
        .post(
            &format!("{base}/agent/register"),
            &key,
            &json!({"name": "pc", "printers": [{"name": "Archive", "can_pdf": false}]}),
        )
        .unwrap()
        .unwrap();
    let pid = reg["printer_ids"]["Archive"].as_i64().unwrap();
    let out = dir.path().join("out");
    let printers = PrinterMap(vec![(pid, Printer::new("Archive", false, &file_uri(&out)))]);
    World {
        state,
        base,
        client,
        key,
        printers,
        dir,
    }
}

fn world() -> World {
    world_in(tempfile::tempdir().unwrap())
}

impl World {
    fn pid(&self) -> i64 {
        self.printers.0[0].0
    }

    fn submit(&self) -> i64 {
        let r = self
            .client
            .post(
                &format!("{}/jobs", self.base),
                "root-token",
                &json!({"printer_id": self.pid(), "type": "raw_base64", "content": "QUJD"}),
            )
            .unwrap()
            .unwrap();
        r["job_id"].as_i64().unwrap()
    }

    fn state(&self, jid: i64) -> String {
        job_state(&self.state.db, jid)
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.path().join(journal::FILE_NAME)
    }

    /// A fresh outbox over the journal file: what an agent that was restarted has.
    fn outbox(&self) -> Outbox {
        let started = std::time::Instant::now();
        Outbox::new(
            Journal::open(self.journal_path(), &self.key, &|_| {}),
            Box::new(move || started.elapsed().as_secs_f64()),
        )
    }

    /// The reaper at a time far past every timeout.
    fn reaper(&self) {
        let now = store_now() + 10_000.0;
        self.state
            .db
            .with(|c| store::requeue_stale(c, 300.0, 2, Some(now), false))
            .unwrap();
    }

    fn run(
        &self,
        http: &dyn Http,
        fns: &Fns,
        outbox: &mut Outbox,
    ) -> Result<Option<JobDone>, String> {
        run_once(
            &self.base,
            &self.key,
            &self.printers,
            http,
            fns,
            &|_| {},
            outbox,
            &|_| {},
        )
    }
}

/// A file printer that counts every copy it is handed; `during` runs while the job "prints".
fn archive(prints: &Rc<Cell<u32>>, during: impl Fn() + 'static) -> Fns {
    let prints = prints.clone();
    Fns {
        file: Box::new(move |j| {
            during();
            prints.set(prints.get() + 1);
            write_to_file(j.target, j.data, j.mode, j.job_id, j.index).map(|_| ())
        }),
        ..Fns::refusing()
    }
}

#[test]
fn a_result_that_never_arrived_does_not_print_the_job_a_second_time() {
    let w = world();
    let jid = w.submit();
    let prints = Rc::new(Cell::new(0));
    let fns = archive(&prints, || {});
    let flaky = Flaky {
        inner: &w.client,
        fail_old_route: Cell::new(true),
        fail_late_route: Cell::new(true),
    };
    // the agent prints and cannot report
    let r = w.run(&flaky, &fns, &mut w.outbox());
    assert_eq!(r, Ok(Some(JobDone { id: jid, ok: true })));
    assert_eq!((prints.get(), w.state(jid).as_str()), (1, "claimed"));
    // the reaper gives up on the agent and requeues
    w.reaper();
    assert_eq!(w.state(jid), "queued");
    // the agent restarts (a new Outbox over the same file) and the uplink is back
    flaky.fail_old_route.set(false);
    flaky.fail_late_route.set(false);
    let mut outbox = w.outbox();
    assert_eq!(outbox.journal().pending().len(), 1);
    assert_eq!(w.run(&flaky, &fns, &mut outbox), Ok(None));
    assert_eq!(w.state(jid), "done");
    assert_eq!(prints.get(), 1, "printed exactly once");
    assert!(outbox.journal().pending().is_empty());
}

#[test]
fn a_job_handed_out_again_is_answered_from_the_journal_over_the_old_route() {
    // the new route is blocked (a WAF that knows only the old paths): the requeued job is handed
    // out again and its recorded result goes over the old route
    let w = world();
    let jid = w.submit();
    let prints = Rc::new(Cell::new(0));
    let fns = archive(&prints, || {});
    let flaky = Flaky {
        inner: &w.client,
        fail_old_route: Cell::new(true),
        fail_late_route: Cell::new(true),
    };
    w.run(&flaky, &fns, &mut w.outbox()).unwrap();
    w.reaper();
    flaky.fail_old_route.set(false);
    let r = w.run(&flaky, &fns, &mut w.outbox());
    assert_eq!(r, Ok(Some(JobDone { id: jid, ok: true })));
    assert_eq!(prints.get(), 1);
    assert_eq!(w.state(jid), "done");
}

#[test]
fn a_job_that_prints_past_the_reaper_timeout_ends_done_with_one_print() {
    let w = world();
    let jid = w.submit();
    let prints = Rc::new(Cell::new(0));
    let state = w.state.clone();
    let reaper = move || {
        let now = store_now() + 10_000.0;
        state
            .db
            .with(|c| store::requeue_stale(c, 300.0, 2, Some(now), false))
            .unwrap();
    };
    let fns = archive(&prints, reaper);
    let flaky = Flaky {
        inner: &w.client,
        fail_old_route: Cell::new(false),
        fail_late_route: Cell::new(false),
    };
    let r = w.run(&flaky, &fns, &mut w.outbox());
    assert_eq!(r, Ok(Some(JobDone { id: jid, ok: true })));
    assert_eq!(w.state(jid), "done");
    assert_eq!(prints.get(), 1);
    // nothing is left to hand out
    assert_eq!(w.run(&flaky, &fns, &mut w.outbox()), Ok(None));
    assert_eq!(prints.get(), 1);
}

#[test]
fn a_job_the_reaper_already_failed_becomes_done_when_the_late_ok_arrives() {
    let w = world();
    let jid = w.submit();
    let prints = Rc::new(Cell::new(0));
    let state = w.state.clone();
    let reaper = move || {
        // the job has been requeued twice already: the next pass fails it
        state
            .db
            .with(|c| c.execute("UPDATE jobs SET retries=2", []).unwrap());
        let now = store_now() + 10_000.0;
        state
            .db
            .with(|c| store::requeue_stale(c, 300.0, 2, Some(now), false))
            .unwrap();
    };
    let fns = archive(&prints, reaper);
    let flaky = Flaky {
        inner: &w.client,
        fail_old_route: Cell::new(false),
        fail_late_route: Cell::new(false),
    };
    w.run(&flaky, &fns, &mut w.outbox()).unwrap();
    assert_eq!(prints.get(), 1);
    assert_eq!(w.state(jid), "done");
    assert!(job_error(&w.state.db, jid).is_none());
}

#[test]
fn a_server_database_that_was_replaced_prints_its_new_job_and_the_old_result_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let w = world_in(dir);
    let old = w.submit();
    let prints = Rc::new(Cell::new(0));
    let fns = archive(&prints, || {});
    let flaky = Flaky {
        inner: &w.client,
        fail_old_route: Cell::new(true),
        fail_late_route: Cell::new(true),
    };
    w.run(&flaky, &fns, &mut w.outbox()).unwrap();
    assert_eq!((prints.get(), old), (1, 1));
    // the database is lost: a new server, the same job number for another job
    std::thread::sleep(Duration::from_millis(20));
    let fresh = world_in(tempfile::tempdir().unwrap());
    let new = fresh.submit();
    assert_eq!(new, old, "the same job id");
    // the agent's journal (pending: the old report never arrived) meets the new server
    let w2 = World {
        dir: w.dir,
        ..fresh
    };
    let ok = Flaky {
        inner: &w2.client,
        fail_old_route: Cell::new(false),
        fail_late_route: Cell::new(false),
    };
    let mut outbox = w2.outbox();
    assert_eq!(outbox.journal().pending().len(), 1);
    let r = w2.run(&ok, &fns, &mut outbox);
    assert_eq!(r, Ok(Some(JobDone { id: new, ok: true })));
    assert_eq!(prints.get(), 2, "the new job printed");
    assert_eq!(w2.state(new), "done");
    // the old result was answered with state null and is settled
    assert!(outbox.journal().pending().is_empty());
}
