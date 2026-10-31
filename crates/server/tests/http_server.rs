// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `handle` against hand-built requests — the port of the HTTP-level tests of `tests/test_server.py`.
//! What needs a real socket (raw-socket traversal/content-length, the long-poll timing, HEAD over
//! the wire, `create_server`) lives with `http`.
mod common;

use common::*;
use printpapi_server::server::{deliver_agent_events, deliver_webhooks, prometheus, reaper_pass};
use printpapi_server::store::{self, DEFAULT_ORG};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn web_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("web")
}

fn files_with_suffix(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            files_with_suffix(&p, suffix, out);
        } else if p.to_str().unwrap().ends_with(suffix) {
            out.push(p);
        }
    }
}

#[test]
fn health_and_jobs_roundtrip_via_http() {
    let app = App::new();
    let reg = register(
        &app.db,
        "win-1",
        "agentkey",
        &[decl("Z", false)],
        DEFAULT_ORG,
    );
    let pid = reg.pid("Z");
    assert_eq!(app.get("/health", None).0, 200);
    // auth required
    assert_eq!(app.post("/jobs", None, job_body(pid)).0, 401);
    // submit (QUJD == base64 "ABC")
    let (code, body) = app.post("/jobs", Some("t"), job_body(pid));
    assert_eq!(code, 200);
    let jid = job_id(&body);
    let (code, job) = app.get(&format!("/jobs/{jid}"), Some("t"));
    assert!(code == 200 && job["state"] == "queued");
    // unknown printer -> 400
    assert_eq!(app.post("/jobs", Some("t"), job_body(999)).0, 400);
    // printers list
    let (code, body) = app.get("/printers", Some("t"));
    assert!(code == 200 && body["printers"][0]["name"] == "Z");
}

#[test]
fn agent_register_payload_result() {
    let app = App::with(|c| c.open_enrollment = true);
    let r = app.agent(
        "POST",
        "/agent/register",
        "ak",
        Some(json!({"name": "win-1", "printers": [{"name": "Z", "can_pdf": false}]})),
    );
    assert_eq!(r.status, 200);
    let reg = r.json();
    let pid = reg["printer_ids"]["Z"].as_i64().unwrap();
    // same name, different key -> 401 (name<->key binding)
    let r = app.agent(
        "POST",
        "/agent/register",
        "wrongkey",
        Some(json!({"name": "win-1", "printers": []})),
    );
    assert_eq!(r.status, 401);
    let jid = enqueue(&app.db, pid, b"BYTES", |_| {});
    claim(&app.db, reg["computer_id"].as_i64().unwrap());
    // wrong key -> 401
    assert_eq!(
        app.agent("GET", &format!("/agent/jobs/{jid}/payload"), "wrong", None)
            .status,
        401
    );
    let r = app.agent("GET", &format!("/agent/jobs/{jid}/payload"), "ak", None);
    assert!(r.status == 200 && r.body == b"BYTES");
    assert_eq!(r.header("Content-Type"), Some("application/octet-stream"));
    let r = app.agent(
        "POST",
        &format!("/agent/jobs/{jid}/result"),
        "ak",
        Some(json!({"ok": true})),
    );
    assert_eq!(r.status, 200);
    assert_eq!(job_state(&app.db, jid), "done");
}

#[test]
fn the_single_claim_of_get_agent_jobs_answers_204_then_the_job() {
    // handle() never waits: the real long-poll is http's; this is its non-waiting claim.
    let app = App::new();
    let reg = agent_z(&app.db);
    assert_eq!(app.agent("GET", "/agent/jobs", "ak", None).status, 204);
    assert_eq!(app.agent("GET", "/agent/jobs", "wrong", None).status, 401);
    let jid = enqueue(&app.db, reg.pid("Z"), b"x", |_| {});
    let r = app.agent("GET", "/agent/jobs", "ak", None);
    assert!(r.status == 200 && r.json()["job_id"] == jid);
}

#[test]
fn register_duplicate_key_returns_409() {
    let app = App::with(|c| c.open_enrollment = true);
    let r = app.agent(
        "POST",
        "/agent/register",
        "k",
        Some(json!({"name": "a", "printers": []})),
    );
    assert_eq!(r.status, 200);
    // the same key under another agent name
    let r = app.agent(
        "POST",
        "/agent/register",
        "k",
        Some(json!({"name": "b", "printers": []})),
    );
    assert_eq!(r.status, 409);
    assert_eq!(r.json()["error"], "agent key already in use");
}

#[test]
fn register_printer_missing_name_returns_400() {
    let app = App::with(|c| c.open_enrollment = true);
    let r = app.agent(
        "POST",
        "/agent/register",
        "k2",
        Some(json!({"name": "c", "printers": [{"can_pdf": true}]})),
    );
    assert_eq!(r.status, 400);
    assert_eq!(
        r.json()["error"],
        "printer entry missing 'name': {'can_pdf': True}"
    );
}

#[test]
fn register_rejects_garbage_with_a_clean_400() {
    // The Python crashed or answered a NOT NULL 409 on each of these.
    let app = App::with(|c| c.open_enrollment = true);
    for (body, msg) in [
        (
            json!({"name": "a", "printers": "Z"}),
            "printers must be a list",
        ),
        (
            json!({"name": "a", "printers": null}),
            "printers must be a list",
        ),
        (
            json!({"name": "a", "printers": ["Z"]}),
            "printer entry missing 'name': 'Z'",
        ),
        (json!({"printers": []}), "name required"),
        (json!({"name": [5]}), "name required"),
        (json!([1]), "bad json"),
    ] {
        let r = app.agent("POST", "/agent/register", "k", Some(body.clone()));
        assert_eq!(
            (r.status, r.json()["error"].as_str().unwrap().to_string()),
            (400, msg.into()),
            "{body}"
        );
    }
}

#[test]
fn jobs_history_endpoint_requires_bearer_and_lists_newest_first() {
    let app = App::new();
    let reg = agent_z(&app.db);
    enqueue(&app.db, reg.pid("Z"), b"x", |_| {});
    enqueue(&app.db, reg.pid("Z"), b"y", |j| {
        j.type_ = "pdf_base64";
        j.mode = "pdf";
    });
    assert_eq!(app.get("/jobs", None).0, 401); // bearer required
    let (code, body) = app.get("/jobs", Some("t"));
    assert_eq!(code, 200);
    let jobs = body["jobs"].as_array().unwrap();
    assert!(jobs.len() == 2 && jobs[0]["id"].as_i64() > jobs[1]["id"].as_i64()); // newest first
    assert_eq!(jobs[0]["printer_name"], "Z");
    assert!(jobs[0].get("payload").is_none()); // bytes never exposed
}

#[test]
fn apikeys_issue_list_revoke_and_scoped_client_auth() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    // issuing keys needs the admin (bootstrap) token
    assert_eq!(app.post("/apikeys", None, json!({"label": "n8n"})).0, 401);
    assert_eq!(
        app.post("/apikeys", Some("random"), json!({"label": "n8n"}))
            .0,
        401
    );
    let (code, issued) = app.post("/apikeys", Some("t"), json!({"label": "n8n"}));
    assert_eq!(code, 200);
    let newkey = issued["key"].as_str().unwrap().to_string();
    assert!(!newkey.is_empty() && issued["label"] == "n8n");
    // the issued per-client key can submit jobs
    assert_eq!(app.post("/jobs", Some(&newkey), job_body(pid)).0, 200);
    // list is admin-only and never leaks the secret
    assert_eq!(app.get("/apikeys", Some(&newkey)).0, 401); // client key can't manage keys
    let (_, body) = app.get("/apikeys", Some("t"));
    let keys = body["keys"].as_array().unwrap();
    assert!(
        keys[0]["label"] == "n8n"
            && keys[0].get("key").is_none()
            && keys[0].get("key_hash").is_none()
    );
    let kid = keys[0]["id"].as_i64().unwrap();
    // revoke -> the issued key is no longer authorized
    assert_eq!(app.delete(&format!("/apikeys/{kid}"), Some("t")).0, 200);
    assert_eq!(app.post("/jobs", Some(&newkey), job_body(pid)).0, 401);
    // bootstrap token still works after revoking client keys
    assert_eq!(app.get("/printers", Some("t")).0, 200);
}

#[test]
fn apikeys_echo_the_org_id_exactly_as_given() {
    // The Python echoed the raw body value: a string "2" stays "2" in the answer.
    let app = App::new();
    let oid = create_org(&app.db, "acme");
    let (code, body) = app.post(
        "/apikeys",
        Some("t"),
        json!({"label": "c", "org_id": oid.to_string()}),
    );
    assert_eq!(code, 200);
    assert_eq!(body["org_id"], json!(oid.to_string()));
    let (_, keys) = app.get("/apikeys", Some("t"));
    assert_eq!(keys["keys"][0]["org_id"], oid);
    assert_eq!(
        app.post("/apikeys", Some("t"), json!({"org_id": "nope"})).0,
        400
    );
}

#[test]
fn dashboard_served_at_root_as_html() {
    let app = App::with(|c| c.token = "bootstrap-s3cret-value".into());
    let r = app.call("GET", "/").send();
    assert_eq!(r.status, 200);
    let html = r.text();
    assert!(html.to_lowercase().contains("<!doctype html>"));
    assert!(html.to_lowercase().contains("printpapi"));
    assert!(html.contains("/_next/static/")); // the bundle, which fetches with the token
    assert!(!html.contains("bootstrap-s3cret-value")); // the shell itself carries no secret
    assert_eq!(r.header("Content-Type"), Some("text/html; charset=utf-8"));
}

#[test]
fn dashboard_deep_links_resolve_to_their_page() {
    let app = App::new();
    for route in ["/print/", "/devices/", "/history/", "/keys/", "/downloads/"] {
        let r = app.call("GET", route).send();
        assert_eq!(r.status, 200, "{route}");
        // byte-identity, not just "some HTML" — serving the root shell for every deep link would
        // otherwise pass, and each exported page is a distinct file
        let expected =
            std::fs::read(web_dir().join(route.trim_matches('/')).join("index.html")).unwrap();
        assert!(r.body == expected, "{route}");
    }
}

#[test]
fn dashboard_assets_get_correct_type_and_cache() {
    let app = App::new();
    let web = web_dir();
    // every extension the bundle actually ships must be in the explicit table: a .css served as
    // octet-stream leaves the dashboard unstyled, a mistyped .js won't execute at all
    for (suffix, ctype) in [
        (".js", "text/javascript"),
        (".css", "text/css"),
        (".woff2", "font/woff2"),
    ] {
        let mut found = Vec::new();
        files_with_suffix(&web.join("_next").join("static"), suffix, &mut found);
        let asset = found
            .first()
            .unwrap_or_else(|| panic!("no {suffix} in the bundle"));
        let rel = asset
            .strip_prefix(&web)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let r = app.call("GET", &format!("/{rel}")).send();
        assert_eq!(r.status, 200, "{rel}");
        assert!(
            r.header("Content-Type").unwrap().starts_with(ctype),
            "{rel}"
        );
        assert!(
            r.header("Cache-Control").unwrap().contains("immutable"),
            "{rel}"
        );
    }
    assert_eq!(
        app.call("GET", "/").send().header("Cache-Control"),
        Some("no-cache")
    ); // HTML must not be pinned
}

#[test]
fn immutable_cache_is_decided_by_the_resolved_file() {
    // A dotted path back out of /_next/static/ must not inherit the year-long cache.
    let app = App::new();
    let r = app.call("GET", "/_next/static/..%2f..%2findex.html").send();
    assert_eq!(r.status, 200);
    assert_eq!(r.header("Cache-Control"), Some("no-cache"));
}

#[test]
fn unstattable_path_is_a_clean_404() {
    let app = App::new();
    for path in ["/a%00b", "/jobs%00", "/_next/static/x%00.js"] {
        let r = app.call("GET", path).send();
        assert_eq!(r.status, 404, "{path}");
        assert_eq!(r.json()["error"], "not found", "{path}");
    }
    assert_eq!(app.get("/health", None).0, 200); // still healthy afterwards
}

#[test]
fn head_requests_answer_without_a_body() {
    let app = App::new();
    for (path, code) in [("/", 200), ("/health", 200), ("/nope", 404)] {
        let r = app.call("HEAD", path).send();
        assert_eq!(r.status, code, "{path}");
        assert!(r.body.is_empty(), "{path}"); // RFC 9110: HEAD carries no body
    }
    // ...but the dashboard's HEAD reports the length its GET would send
    let get = app.call("GET", "/").send();
    let head = app.call("HEAD", "/").send();
    assert_eq!(
        head.header("Content-Length"),
        Some(get.body.len().to_string().as_str())
    );
    assert_eq!(head.header("Content-Type"), get.header("Content-Type"));
    assert_eq!(head.header("Cache-Control"), Some("no-cache"));
}

#[test]
fn dashboard_path_traversal_is_blocked() {
    let app = App::new();
    for path in [
        "/../server.py",
        "/..%2fserver.py",
        "/%2e%2e/store.py",
        "/../../app/store.py",
        "/..\\Cargo.toml",
    ] {
        let r = app.call("GET", path).send();
        assert_eq!(r.status, 404, "{path}");
        assert!(
            !r.text().contains("import sqlite3"),
            "leaked source via {path}"
        );
    }
}

#[test]
fn metrics_endpoint_prometheus_text() {
    let app = App::new();
    let reg = agent_z(&app.db);
    let j = enqueue(&app.db, reg.pid("Z"), b"x", |_| {});
    claim(&app.db, reg.computer_id);
    finish(&app.db, j, reg.computer_id, true, None);
    assert_eq!(app.get("/metrics", None).0, 401); // auth required
    let r = app.call("GET", "/metrics").bearer("t").send();
    assert_eq!(r.status, 200);
    assert_eq!(
        r.header("Content-Type"),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    let text = r.text();
    assert!(text.contains("printpapi_jobs{state=\"done\"} 1"));
    assert!(text.contains("printpapi_jobs{state=\"queued\"} 0")); // stable zero series
    assert!(text.contains("printpapi_printers_total 1"));
    assert!(text.contains("# TYPE printpapi_jobs gauge"));
}

#[test]
fn prometheus_text_is_byte_identical_to_the_python() {
    let mut m = store::Metrics::default();
    m.jobs.insert("done".into(), 3);
    m.agents_total = 2;
    m.agents_online = 1;
    m.printers_total = 4;
    assert_eq!(
        prometheus(&m),
        "# HELP printpapi_jobs Jobs by state.\n# TYPE printpapi_jobs gauge\n\
         printpapi_jobs{state=\"queued\"} 0\nprintpapi_jobs{state=\"claimed\"} 0\n\
         printpapi_jobs{state=\"done\"} 3\nprintpapi_jobs{state=\"failed\"} 0\n\
         printpapi_jobs{state=\"cancelled\"} 0\n\
         # HELP printpapi_agents_online Agents seen within the online window.\n\
         # TYPE printpapi_agents_online gauge\nprintpapi_agents_online 1\n\
         # HELP printpapi_agents_total Registered agents.\n# TYPE printpapi_agents_total gauge\n\
         printpapi_agents_total 2\n\
         # HELP printpapi_printers_total Registered printers.\n\
         # TYPE printpapi_printers_total gauge\nprintpapi_printers_total 4\n"
    );
}

#[test]
fn unknown_path_is_json_404_not_the_dashboard() {
    let app = App::new();
    let (code, body) = app.get("/nope", None);
    assert!(code == 404 && body["error"] == "not found");
    // a path-and-query that is not an exact route falls through, as self.path matched before
    assert_eq!(app.get("/jobs?x=1", Some("t")).0, 404);
    assert_eq!(app.get("/jobs/99999999999999999999", Some("t")).0, 404);
}

#[test]
fn other_methods_get_a_json_501() {
    let app = App::new();
    let r = app.call("PATCH", "/jobs").send();
    assert_eq!(r.status, 501);
    assert_eq!(r.json()["error"], "unsupported method");
}

#[test]
fn an_unreadable_body_is_a_400_on_every_route_that_reads_one() {
    // Over the wire this is the bad Content-Length cases (-1, 999999999999, nan).
    let app = App::new();
    for (method, target) in [
        ("POST", "/jobs"),
        ("POST", "/orders"),
        ("POST", "/orgs"),
        ("POST", "/login"),
        ("PUT", "/orgs/1"),
        ("POST", "/agent/register"),
    ] {
        let r = app.call(method, target).bearer("t").body_error().send();
        assert_eq!(
            (r.status, r.json()["error"].clone()),
            (400, json!("bad json")),
            "{method} {target}"
        );
    }
    let r = app.call("POST", "/billing/webhook").body_error().send();
    assert_eq!(
        (r.status, r.json()["error"].clone()),
        (400, json!("bad content-length"))
    );
    assert_eq!(app.get("/health", None).0, 200);
}

#[test]
fn a_json_body_that_is_not_an_object_is_a_400() {
    // The Python crashed (AttributeError) without answering.
    let app = App::new();
    for body in ["[1]", "5", "\"x\"", "null"] {
        let r = app
            .call("POST", "/jobs")
            .bearer("t")
            .raw(body.as_bytes())
            .send();
        assert_eq!(
            (r.status, r.json()["error"].clone()),
            (400, json!("bad json")),
            "{body}"
        );
    }
    let r = app.call("POST", "/jobs").bearer("t").raw(b"{").send();
    assert_eq!(r.status, 400);
}

#[test]
fn copies_roundtrip_and_validation_via_http() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    // submit with copies=2, agent claim sees copies=2
    let mut body = job_body(pid);
    body["copies"] = json!(2);
    assert_eq!(app.post("/jobs", Some("t"), body.clone()).0, 200);
    assert_eq!(
        app.agent("GET", "/agent/jobs", "ak", None).json()["copies"],
        2
    );
    // omitted -> default 1
    app.post("/jobs", Some("t"), job_body(pid));
    assert_eq!(
        app.agent("GET", "/agent/jobs", "ak", None).json()["copies"],
        1
    );
    // invalid copies -> 400, and nothing was enqueued (queue is empty on the next claim)
    body["copies"] = json!(0);
    assert_eq!(app.post("/jobs", Some("t"), body).0, 400);
    assert_eq!(app.agent("GET", "/agent/jobs", "ak", None).status, 204);
}

#[test]
fn register_capabilities_visible_in_printers_via_http() {
    let app = App::with(|c| c.open_enrollment = true);
    let caps = json!({"papers": ["A4"], "bins": ["Tray 1"], "duplex": true, "color": true});
    let r = app.agent(
        "POST",
        "/agent/register",
        "ak",
        Some(json!({"name": "pc", "printers": [{"name": "HP", "can_pdf": true, "capabilities": caps}]})),
    );
    assert_eq!(r.status, 200);
    let (code, body) = app.get("/printers", Some("t"));
    assert!(code == 200 && body["printers"][0]["capabilities"] == caps);
    assert_eq!(body["printers"][0]["can_pdf"], true);
}

#[test]
fn options_roundtrip_and_validation_via_http() {
    let app = App::new();
    let reg = register(&app.db, "pc", "ak", &[decl("HP", true)], DEFAULT_ORG);
    let pid = reg.pid("HP");
    // submit with options, agent claim carries them (JVBERg== == base64 "%PDF")
    let opts = json!({"duplex": "long-edge", "paper": "A4", "bin": "Tray 1"});
    let pdf =
        json!({"printer_id": pid, "type": "pdf_base64", "content": "JVBERg==", "options": opts});
    assert_eq!(app.post("/jobs", Some("t"), pdf).0, 200);
    assert_eq!(
        app.agent("GET", "/agent/jobs", "ak", None).json()["options"],
        opts
    );
    // options on a raw job -> 400; bad option value -> 400; neither enqueues anything
    let mut raw = job_body(pid);
    raw["options"] = opts.clone();
    assert_eq!(app.post("/jobs", Some("t"), raw).0, 400);
    let bad = json!({"printer_id": pid, "type": "pdf_base64", "content": "JVBERg==",
                     "options": {"duplex": "both"}});
    assert_eq!(app.post("/jobs", Some("t"), bad).0, 400);
    assert_eq!(app.agent("GET", "/agent/jobs", "ak", None).status, 204);
}

#[test]
fn cancel_job_via_http_states_and_auth() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let jid = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    // cancel requires client auth
    assert_eq!(app.delete(&format!("/jobs/{jid}"), None).0, 401);
    // queued -> cancelled (200)
    let (code, body) = app.delete(&format!("/jobs/{jid}"), Some("t"));
    assert!(code == 200 && body["state"] == "cancelled");
    assert_eq!(job_state(&app.db, jid), "cancelled");
    // agent never sees a cancelled job
    assert_eq!(app.agent("GET", "/agent/jobs", "ak", None).status, 204);
    // cancelling again (now cancelled, not queued) -> 409
    assert_eq!(app.delete(&format!("/jobs/{jid}"), Some("t")).0, 409);
    // unknown job -> 404
    assert_eq!(app.delete("/jobs/999999", Some("t")).0, 404);
    // a per-client (non-bootstrap) issued key can also cancel (not admin-only)
    let newkey = app.post("/apikeys", Some("t"), json!({"label": "c"})).1["key"]
        .as_str()
        .unwrap()
        .to_string();
    let jid2 = enqueue(&app.db, pid, b"y", |_| {});
    assert_eq!(app.delete(&format!("/jobs/{jid2}"), Some(&newkey)).0, 200);
}

#[test]
fn cancel_claimed_job_returns_409_via_http() {
    let app = App::new();
    let reg = agent_z(&app.db);
    let jid = enqueue(&app.db, reg.pid("Z"), b"x", |_| {});
    claim(&app.db, reg.computer_id);
    let (code, body) = app.delete(&format!("/jobs/{jid}"), Some("t"));
    assert_eq!(code, 409);
    assert_eq!(
        body["error"],
        "job not cancellable (already claimed or finished)"
    );
}

#[test]
fn bad_callback_url_rejected_via_http() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let mut body = job_body(pid);
    body["callback_url"] = json!("file:///etc/passwd");
    assert_eq!(app.post("/jobs", Some("t"), body.clone()).0, 400);
    // a valid https callback is accepted
    body["callback_url"] = json!("https://hook/x");
    assert_eq!(app.post("/jobs", Some("t"), body).0, 200);
}

#[test]
fn a_failed_download_is_a_502_downstream() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    *app.fetch.reply.lock().unwrap() = Some(Err("HTTP Error 404: Not Found".into()));
    let body = json!({"printer_id": pid, "type": "raw_uri", "url": "https://x.test/a"});
    let (code, out) = app.post("/jobs", Some("t"), body);
    assert_eq!(code, 502);
    assert_eq!(
        out["error"],
        "downstream: fetch failed: HTTP Error 404: Not Found"
    );
    *app.fetch.reply.lock().unwrap() = None;
    let body = json!({"printer_id": pid, "type": "raw_uri", "url": "https://x.test/a"});
    assert_eq!(app.post("/jobs", Some("t"), body).0, 200);
    assert_eq!(app.fetch.gets.lock().unwrap().len(), 2);
}

type Posts = Mutex<Vec<(String, Value)>>;

fn recorder(posts: &Posts) -> impl Fn(&str, &Value) -> Result<(), String> + '_ {
    |url, body| {
        posts.lock().unwrap().push((url.to_string(), body.clone()));
        Ok(())
    }
}

#[test]
fn webhook_dispatcher_delivers_and_marks() {
    let db = mem();
    let reg = agent_z(&db);
    let (aid, pid) = (reg.computer_id, reg.pid("Z"));
    let a = enqueue(&db, pid, b"x", |j| {
        j.callback_url = Some("https://h/a".into());
        j.title = Some("T".into());
    });
    claim(&db, aid);
    finish(&db, a, aid, true, None);
    let posts = Posts::default();
    deliver_webhooks(&db, &recorder(&posts), 5);
    assert_eq!(
        *posts.lock().unwrap(),
        vec![(
            "https://h/a".to_string(),
            json!({"job_id": a, "state": "done", "error": null, "title": "T", "printer_id": pid})
        )]
    );
    // marked delivered, not re-sent
    assert!(
        db.with(|c| store::pending_webhooks(c, 5))
            .unwrap()
            .is_empty()
    );
    deliver_webhooks(&db, &recorder(&posts), 5);
    assert_eq!(posts.lock().unwrap().len(), 1); // second pass sends nothing
}

#[test]
fn webhook_payload_carries_failure_error() {
    let db = mem();
    let reg = agent_z(&db);
    let (aid, pid) = (reg.computer_id, reg.pid("Z"));
    let a = enqueue(&db, pid, b"x", |j| {
        j.callback_url = Some("https://h/a".into());
        j.title = Some("T".into());
    });
    claim(&db, aid);
    finish(&db, a, aid, false, Some("boom"));
    let posts = Posts::default();
    deliver_webhooks(&db, &recorder(&posts), 5);
    let sent = posts.lock().unwrap();
    assert_eq!(
        sent[0].1,
        json!({"job_id": a, "state": "failed", "error": "boom", "title": "T", "printer_id": pid})
    );
    // Python's dict order is the JSON order
    assert_eq!(
        serde_json::to_string(&sent[0].1).unwrap().find("job_id"),
        Some(2)
    );
}

#[test]
fn webhook_dispatcher_retries_then_gives_up() {
    let db = mem();
    let reg = agent_z(&db);
    let (aid, pid) = (reg.computer_id, reg.pid("Z"));
    let a = enqueue(&db, pid, b"x", |j| {
        j.callback_url = Some("https://h/a".into())
    });
    claim(&db, aid);
    finish(&db, a, aid, false, Some("boom"));
    let calls = Mutex::new(Vec::new());
    let boom = |url: &str, _: &Value| {
        calls.lock().unwrap().push(url.to_string());
        Err("hook down".to_string())
    };
    for _ in 0..5 {
        // more passes than the cap
        deliver_webhooks(&db, &boom, 3);
    }
    assert_eq!(calls.lock().unwrap().len(), 3); // tried exactly cap times, then gave up
    assert_eq!(job_state(&db, a), "failed"); // job state untouched by hook failure
}

#[test]
fn one_bad_webhook_url_does_not_abort_the_pass() {
    let db = mem();
    let reg = agent_z(&db);
    let (aid, pid) = (reg.computer_id, reg.pid("Z"));
    let a = enqueue(&db, pid, b"x", |j| {
        j.callback_url = Some("https://bad/a".into())
    });
    let b = enqueue(&db, pid, b"x", |j| {
        j.callback_url = Some("https://good/b".into())
    });
    for j in [a, b] {
        claim(&db, aid);
        finish(&db, j, aid, true, None);
    }
    let seen = Mutex::new(Vec::new());
    let post = |url: &str, _: &Value| {
        seen.lock().unwrap().push(url.to_string());
        if url.contains("bad") {
            Err("refused".into())
        } else {
            Ok(())
        }
    };
    deliver_webhooks(&db, &post, 5);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["https://bad/a", "https://good/b"]
    );
    let left = db.with(|c| store::pending_webhooks(c, 5)).unwrap();
    assert_eq!(left.iter().map(|h| h.job_id).collect::<Vec<_>>(), vec![a]);
}

// --- multi-tenancy ----------------------------------------------------------------------------

#[test]
fn orgs_are_created_and_listed_by_the_root_token_only() {
    let app = App::new();
    assert_eq!(app.post("/orgs", None, json!({"name": "acme"})).0, 401);
    let (code, body) = app.post("/orgs", Some("t"), json!({"name": "acme"}));
    assert_eq!(code, 200);
    let oid = body["id"].as_i64().unwrap();
    assert_ne!(oid, DEFAULT_ORG);
    let (_, orgs) = app.get("/orgs", Some("t"));
    let names: Vec<&str> = orgs["orgs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["default", "acme"]);
    // an org's own client key is not root: it can neither list nor create orgs
    let key = app
        .post("/apikeys", Some("t"), json!({"label": "c", "org_id": oid}))
        .1["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(app.get("/orgs", Some(&key)).0, 401);
    assert_eq!(app.post("/orgs", Some(&key), json!({"name": "x"})).0, 401);
    assert_eq!(app.post("/orgs", Some("t"), json!({"name": "  "})).0, 400);
}

#[test]
fn apikeys_are_issued_into_an_org_and_unknown_orgs_are_rejected() {
    let app = App::new();
    let oid = app.post("/orgs", Some("t"), json!({"name": "acme"})).1["id"]
        .as_i64()
        .unwrap();
    let (code, body) = app.post("/apikeys", Some("t"), json!({"label": "c", "org_id": oid}));
    assert!(code == 200 && body["org_id"] == oid);
    let (_, keys) = app.get("/apikeys", Some("t"));
    assert_eq!(keys["keys"][0]["org_id"], oid);
    assert_eq!(
        app.post("/apikeys", Some("t"), json!({"label": "c", "org_id": 4242}))
            .0,
        400
    ); // no such org
    // no org given -> the default org, exactly as before multi-tenancy
    let (_, body) = app.post("/apikeys", Some("t"), json!({"label": "legacy"}));
    assert_eq!(body["org_id"], DEFAULT_ORG);
}

/// org 1 with printer Z + a queued job, 'acme' with printer Y and its own client key.
fn two_org_fixture(db: &store::Db) -> (i64, Registered, Registered, i64) {
    let other = create_org(db, "acme");
    let a = register(db, "pc", "ka", &[decl("Z", false)], DEFAULT_ORG);
    let b = register(db, "pc", "kb", &[decl("Y", false)], other);
    add_key(db, "acme", "acme-key", other);
    let ja = enqueue(db, a.pid("Z"), b"a", |_| {});
    (other, a, b, ja)
}

#[test]
fn client_key_is_confined_to_its_own_org() {
    let app = App::new();
    let (_, a, b, ja) = two_org_fixture(&app.db);
    // a foreign job id is 404 — never 200, never 403 (which would confirm it exists)
    assert_eq!(app.get(&format!("/jobs/{ja}"), Some("acme-key")).0, 404);
    let jb = job_id(&app.post("/jobs", Some("acme-key"), job_body(b.pid("Y"))).1);
    assert_eq!(app.get(&format!("/jobs/{jb}"), Some("acme-key")).0, 200);
    // printing to a foreign printer is "unknown printer" (400), same as a nonexistent one
    assert_eq!(
        app.post("/jobs", Some("acme-key"), job_body(a.pid("Z"))).0,
        400
    );
    // lists carry only the org's own rows
    let (_, printers) = app.get("/printers", Some("acme-key"));
    let names: Vec<&str> = printers["printers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Y"]);
    let (_, jobs) = app.get("/jobs", Some("acme-key"));
    let ids: Vec<i64> = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [jb]);
    let m = app.call("GET", "/metrics").bearer("acme-key").send().text();
    assert!(m.contains("printpapi_printers_total 1"));
    // cancelling someone else's job is a 404 and leaves it untouched
    assert_eq!(app.delete(&format!("/jobs/{ja}"), Some("acme-key")).0, 404);
    assert_eq!(job_state(&app.db, ja), "queued");
    // key management stays root-only
    assert_eq!(app.get("/apikeys", Some("acme-key")).0, 401);
}

#[test]
fn root_token_spans_all_orgs() {
    let app = App::new();
    let (other, _, b, ja) = two_org_fixture(&app.db);
    let jb = enqueue(&app.db, b.pid("Y"), b"b", |j| j.org_id = Some(other));
    let (_, printers) = app.get("/printers", Some("t"));
    let mut names: Vec<&str> = printers["printers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["Y", "Z"]);
    let (_, jobs) = app.get("/jobs", Some("t"));
    let mut ids: Vec<i64> = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_i64().unwrap())
        .collect();
    ids.sort();
    assert_eq!(ids, [ja, jb]);
    assert!(
        app.call("GET", "/metrics")
            .bearer("t")
            .send()
            .text()
            .contains("printpapi_printers_total 2")
    );
    assert_eq!(app.get(&format!("/jobs/{jb}"), Some("t")).0, 200); // reads any org's job
    assert_eq!(app.delete(&format!("/jobs/{jb}"), Some("t")).0, 200); // and cancels it
    // and prints to any org's printer
    assert_eq!(app.post("/jobs", Some("t"), job_body(b.pid("Y"))).0, 200);
}

#[test]
fn agent_registers_into_the_org_of_its_key() {
    let app = App::with(|c| c.open_enrollment = true);
    let oid = create_org(&app.db, "acme");
    add_key(&app.db, "acme-agent", "acme-agent-key", oid);
    let r = app.agent(
        "POST",
        "/agent/register",
        "acme-agent-key",
        Some(json!({"name": "pc", "printers": [{"name": "Z"}]})),
    );
    assert_eq!(r.status, 200);
    let pid = r.json()["printer_ids"]["Z"].as_i64().unwrap();
    assert_eq!(
        sql_i64(&app.db, "SELECT org_id FROM printers WHERE id=?", pid),
        Some(oid)
    );
    // a key that is not an issued client key still enrolls into the default org (legacy path)
    let r = app.agent(
        "POST",
        "/agent/register",
        "plain-key",
        Some(json!({"name": "old", "printers": [{"name": "Y"}]})),
    );
    assert_eq!(r.status, 200);
    let pid2 = r.json()["printer_ids"]["Y"].as_i64().unwrap();
    assert_eq!(
        sql_i64(&app.db, "SELECT org_id FROM printers WHERE id=?", pid2),
        Some(DEFAULT_ORG)
    );
    // the org's key sees its own printer only, and its jobs reach that agent
    let (_, printers) = app.get("/printers", Some("acme-agent-key"));
    let names: Vec<&str> = printers["printers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Z"]);
    let jid = job_id(&app.post("/jobs", Some("acme-agent-key"), job_body(pid)).1);
    assert_eq!(
        app.agent("GET", "/agent/jobs", "acme-agent-key", None)
            .json()["job_id"],
        jid
    );
}

#[test]
fn job_title_roundtrips_through_http() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let mut body = job_body(pid);
    body["title"] = json!("Label #5");
    assert_eq!(app.post("/jobs", Some("t"), body).0, 200);
    let (_, jobs) = app.get("/jobs", Some("t"));
    assert_eq!(jobs["jobs"][0]["title"], "Label #5");
    assert_eq!(jobs["jobs"][0]["agent_name"], "pc");
    // a non-string title binds as its Python str()
    let mut body = job_body(pid);
    body["title"] = json!(42);
    assert_eq!(app.post("/jobs", Some("t"), body).0, 200);
    assert_eq!(app.get("/jobs", Some("t")).1["jobs"][0]["title"], "42");
}

#[test]
fn computers_endpoint_lists_agents_scoped_to_the_org() {
    let app = App::new();
    let (_, _, b, _) = two_org_fixture(&app.db);
    assert_eq!(app.get("/computers", None).0, 401);
    let (_, cs) = app.get("/computers", Some("acme-key"));
    let cs = cs["computers"].as_array().unwrap();
    assert_eq!(
        cs.iter()
            .map(|c| c["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [b.computer_id]
    );
    assert!(cs[0]["name"] == "pc" && cs[0]["online"] == true && cs[0]["printers"] == 1);
    assert_eq!(
        app.get("/computers", Some("t")).1["computers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn org_event_url_is_set_by_root_only_and_must_be_http() {
    let app = App::new();
    let oid = app.post("/orgs", Some("t"), json!({"name": "acme"})).1["id"]
        .as_i64()
        .unwrap();
    let key = app
        .post("/apikeys", Some("t"), json!({"label": "c", "org_id": oid}))
        .1["key"]
        .as_str()
        .unwrap()
        .to_string();
    let url = format!("/orgs/{oid}");
    // its own key is not root
    assert_eq!(
        app.put(&url, Some(&key), json!({"event_url": "https://h"}))
            .0,
        401
    );
    let (code, body) = app.put(&url, Some("t"), json!({"event_url": "ftp://h"}));
    assert_eq!(code, 400);
    assert!(
        body["error"].as_str().unwrap().contains("event_url"),
        "{body}"
    );
    assert_eq!(
        app.put(&url, Some("t"), json!({"event_url": "https://h"}))
            .0,
        200
    );
    let event_url = |app: &App| {
        app.get("/orgs", Some("t")).1["orgs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == oid)
            .unwrap()["event_url"]
            .clone()
    };
    assert_eq!(event_url(&app), "https://h");
    assert_eq!(
        app.put("/orgs/4242", Some("t"), json!({"event_url": "https://h"}))
            .0,
        404
    );
    assert_eq!(app.put(&url, Some("t"), json!({"event_url": null})).0, 200);
    assert_eq!(event_url(&app), Value::Null); // null clears it
}

#[test]
fn agent_liveness_events_are_posted_to_the_orgs_event_url() {
    let db = mem();
    db.with(|c| store::set_org_event_url(c, DEFAULT_ORG, Some("https://hooks.example/x")))
        .unwrap();
    let reg = agent_z(&db);
    let posts = Posts::default();
    let now = printpapi_server::now();
    deliver_agent_events(&db, &recorder(&posts), 60.0, Some(now));
    assert!(posts.lock().unwrap().is_empty()); // still online
    deliver_agent_events(&db, &recorder(&posts), 60.0, Some(now + 300.0));
    let sent = posts.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let (url, body) = &sent[0];
    assert_eq!(url, "https://hooks.example/x");
    assert!(body["event"] == "computer_offline" && body["computer_id"] == reg.computer_id);
    assert!(body["name"] == "pc" && body["org_id"] == DEFAULT_ORG);
    assert!(body["last_seen_at"].as_f64().unwrap() > 0.0);
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["event", "computer_id", "name", "org_id", "last_seen_at"]
    );
}

#[test]
fn a_failing_event_post_does_not_abort_the_pass() {
    let db = mem();
    db.with(|c| store::set_org_event_url(c, DEFAULT_ORG, Some("https://down.example/x")))
        .unwrap();
    register(&db, "pc", "ak", &[], DEFAULT_ORG);
    let boom = |_: &str, _: &Value| Err("connection refused".to_string());
    deliver_agent_events(&db, &boom, 60.0, Some(printpapi_server::now() + 300.0)); // must not panic
}

#[test]
fn idempotency_key_makes_a_resubmitted_job_a_no_op() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let mut body = job_body(pid);
    body["idempotency_key"] = json!("order-42");
    let first = job_id(&app.post("/jobs", Some("t"), body.clone()).1);
    let again = job_id(&app.post("/jobs", Some("t"), body.clone()).1);
    assert_eq!(again, first); // retry, not a second print
    assert_eq!(
        app.get("/jobs", Some("t")).1["jobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    body["idempotency_key"] = json!(42);
    assert_eq!(app.post("/jobs", Some("t"), body).0, 400);
}

#[test]
fn expire_after_stores_a_deadline_and_is_validated() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let mut body = job_body(pid);
    body["expire_after"] = json!(120);
    let jid = job_id(&app.post("/jobs", Some("t"), body).1);
    let exp = sql_f64(&app.db, "SELECT expires_at FROM jobs WHERE id=?", jid).unwrap();
    let left = exp - printpapi_server::now();
    assert!(left > 110.0 && left <= 120.0, "{left}");
    let jid2 = job_id(&app.post("/jobs", Some("t"), job_body(pid)).1);
    assert_eq!(
        sql_f64(&app.db, "SELECT expires_at FROM jobs WHERE id=?", jid2),
        None
    ); // opt-in only
    let mut zero = job_body(pid);
    zero["expire_after"] = json!(0);
    assert_eq!(app.post("/jobs", Some("t"), zero).0, 400);
}

#[test]
fn reaper_expires_deadline_passed_jobs() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let jid = enqueue(&app.db, pid, b"a", |j| j.expire_after = Some(-1)); // a deadline already in the past
    reaper_pass(&app.state, 300.0, 2);
    assert_eq!(job_error(&app.db, jid).as_deref(), Some("expired"));
}

#[test]
fn reaper_requeues_stale_claims_and_wakes_every_long_poll() {
    let app = App::new();
    let reg = agent_z(&app.db);
    let jid = enqueue(&app.db, reg.pid("Z"), b"a", |_| {});
    claim(&app.db, reg.computer_id);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let notify = app.state.waker.for_agent(reg.computer_id);
    rt.block_on(async {
        let waiter = notify.notified();
        tokio::pin!(waiter);
        waiter.as_mut().enable();
        reaper_pass(&app.state, -1.0, 2); // every claim counts as stale
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("the long-poll was woken");
    });
    assert_eq!(job_state(&app.db, jid), "queued");
}

#[test]
fn reaper_pass_logs_a_store_failure_instead_of_propagating_it() {
    let app = App::new();
    let pid = agent_z(&app.db).pid("Z");
    let jid = enqueue(&app.db, pid, b"a", |j| j.expire_after = Some(-1));
    app.db
        .with(|c| c.execute_batch("DROP TABLE sessions"))
        .unwrap();
    reaper_pass(&app.state, 300.0, 2); // the session purge fails: logged, no panic
    // the passes before the failing one ran
    assert_eq!(job_error(&app.db, jid).as_deref(), Some("expired"));
}

// --- v3: enrollment needs an issued key, and revoking a key stops its agent ------------------

fn issue(app: &App, label: &str) -> (i64, String) {
    let (st, v) = app.post("/apikeys", Some("t"), json!({"label": label}));
    assert_eq!(st, 200, "{v}");
    (
        v["id"].as_i64().unwrap(),
        v["key"].as_str().unwrap().to_string(),
    )
}

#[test]
fn a_new_agent_needs_an_issued_key_or_the_root_token() {
    let app = App::new();
    let body = json!({"name": "pc-1", "printers": [{"name": "Z"}]});
    let (st, v) = app.post("/agent/register", Some("made-up-key"), body.clone());
    assert_eq!(st, 401, "{v}");
    assert_eq!(
        v["error"],
        "unknown agent key: issue one with POST /apikeys (the dashboard's Keys page)"
    );
    let (st, v) = app.post("/agent/register", None, body.clone());
    assert_eq!(st, 401, "no key at all: {v}");
    let (_, key) = issue(&app, "pc-1");
    let (st, v) = app.post("/agent/register", Some(&key), body.clone());
    assert_eq!(st, 200, "{v}");
    let (st, v) = app.post(
        "/agent/register",
        Some("t"),
        json!({"name": "ops", "printers": []}),
    );
    assert_eq!(st, 200, "the root token still enrolls (default org): {v}");
}

#[test]
fn an_agent_enrolled_before_v3_with_a_self_chosen_key_keeps_working() {
    let app = App::new();
    // what a v2 server left in the DB: an agent bound to a key that was never issued
    app.state
        .db
        .with(|c| store::register_agent(c, "old-pc", "legacy-key", &[], store::DEFAULT_ORG))
        .unwrap();
    let (st, v) = app.post(
        "/agent/register",
        Some("legacy-key"),
        json!({"name": "old-pc"}),
    );
    assert_eq!(st, 200, "re-register after a restart: {v}");
}

#[test]
fn open_enrollment_restores_the_v2_rule() {
    let app = App::with(|c| c.open_enrollment = true);
    let (st, v) = app.post(
        "/agent/register",
        Some("made-up-key"),
        json!({"name": "pc"}),
    );
    assert_eq!(st, 200, "{v}");
}

#[test]
fn revoking_a_key_stops_the_agent_it_enrolled() {
    let app = App::new();
    let (kid, key) = issue(&app, "pc-1");
    let (st, reg) = app.post(
        "/agent/register",
        Some(&key),
        json!({"name": "pc-1", "printers": [{"name": "Z"}]}),
    );
    assert_eq!(st, 200, "{reg}");
    let pid = reg["printer_ids"]["Z"].clone();
    let (st, v) = app.post(
        "/jobs",
        Some("t"),
        json!({"printer_id": pid, "type": "raw_base64", "content": "QUJD"}),
    );
    assert_eq!(st, 200, "{v}");
    let jid = v["job_id"].as_i64().unwrap();
    // before: the agent claims and fetches
    assert_eq!(app.get("/agent/jobs", Some(&key)).0, 200);
    assert_eq!(
        app.agent("GET", &format!("/agent/jobs/{jid}/payload"), &key, None)
            .status,
        200
    );
    let (st, _) = app.delete(&format!("/apikeys/{kid}"), Some("t"));
    assert_eq!(st, 200);
    // after: every agent route refuses the key, and it cannot enroll again
    assert_eq!(
        app.agent("GET", &format!("/agent/jobs/{jid}/payload"), &key, None)
            .status,
        401
    );
    let (st, _) = app.post(
        &format!("/agent/jobs/{jid}/result"),
        Some(&key),
        json!({"ok": true}),
    );
    assert_eq!(st, 401);
    assert_eq!(app.get("/agent/jobs", Some(&key)).0, 401);
    let (st, v) = app.post("/agent/register", Some(&key), json!({"name": "pc-1"}));
    assert_eq!((st, v["error"].as_str()), (401, Some("agent key revoked")));
}

#[test]
fn an_agent_registered_without_any_key_does_not_count_as_known() {
    let app = App::new();
    // a v2 server let a request without an Authorization header register under the empty key
    app.state
        .db
        .with(|c| store::register_agent(c, "headless", "", &[], DEFAULT_ORG))
        .unwrap();
    let (st, v) = app.post(
        "/agent/register",
        None,
        json!({"name": "headless", "printers": [{"name": "Z"}]}),
    );
    assert_eq!(st, 401, "{v}");
}

#[test]
fn the_root_token_enrolls_even_if_an_issued_key_with_its_value_was_revoked() {
    let app = App::new();
    let kid = app
        .state
        .db
        .with(|c| store::add_api_key(c, "odd", "t", DEFAULT_ORG))
        .unwrap();
    app.state
        .db
        .with(|c| store::revoke_api_key(c, kid, None))
        .unwrap();
    let (st, v) = app.post("/agent/register", Some("t"), json!({"name": "ops"}));
    assert_eq!(st, 200, "{v}");
}
