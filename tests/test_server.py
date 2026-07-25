import json, socket, time, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer
from app import store, server


def _serve(conn, token="t"):
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3, poll_interval=0.05)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}"


def _mem():
    conn = store.connect(":memory:")
    store.init_db(conn)
    return conn


def _req(method, url, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def test_health_and_jobs_roundtrip_via_http():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "agentkey", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        assert _req("GET", base + "/health")[0] == 200
        # auth required
        assert _req("POST", base + "/jobs", body={"printer_id": pid, "type": "raw_base64",
                                                   "content": "QUJD"})[0] == 401
        # submit (QUJD == base64 "ABC")
        code, raw = _req("POST", base + "/jobs", token="t",
                         body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        assert code == 200
        jid = json.loads(raw)["job_id"]
        code, raw = _req("GET", base + f"/jobs/{jid}", token="t")
        assert code == 200 and json.loads(raw)["state"] == "queued"
        # unknown printer -> 400
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": 999, "type": "raw_base64", "content": "QUJD"})[0] == 400
        # printers list
        code, raw = _req("GET", base + "/printers", token="t")
        assert code == 200 and json.loads(raw)["printers"][0]["name"] == "Z"
    finally:
        httpd.shutdown()


def _areq(method, url, key, body=None, raw=False):
    data = body if raw else (json.dumps(body).encode() if body is not None else None)
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Authorization", f"Bearer {key}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def test_agent_register_payload_result():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, raw = _areq("POST", base + "/agent/register", "ak",
                          {"name": "win-1", "printers": [{"name": "Z", "can_pdf": False}]})
        assert code == 200
        reg = json.loads(raw)
        pid = reg["printer_ids"]["Z"]
        # same name, different key -> 401 (name<->key binding)
        assert _areq("POST", base + "/agent/register", "wrongkey",
                     {"name": "win-1", "printers": []})[0] == 401
        jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"BYTES")
        store.claim_job(conn, reg["computer_id"])
        # wrong key -> 401
        assert _areq("GET", base + f"/agent/jobs/{jid}/payload", "wrong")[0] == 401
        code, payload = _areq("GET", base + f"/agent/jobs/{jid}/payload", "ak")
        assert code == 200 and payload == b"BYTES"
        code, _ = _areq("POST", base + f"/agent/jobs/{jid}/result", "ak", {"ok": True})
        assert code == 200
        assert store.get_job(conn, jid)["state"] == "done"
    finally:
        httpd.shutdown()


def test_long_poll_204_then_job():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "ak", [{"name": "Z", "can_pdf": False}])
    httpd, base = _serve(conn)  # long_poll_timeout=0.3, poll_interval=0.05
    try:
        # nothing queued -> 204 after timeout
        t0 = time.time()
        assert _areq("GET", base + "/agent/jobs", "ak")[0] == 204
        assert time.time() - t0 >= 0.25
        # enqueue, then poll returns it
        pid = reg["printer_ids"]["Z"]
        jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
        code, raw = _areq("GET", base + "/agent/jobs", "ak")
        assert code == 200 and json.loads(raw)["job_id"] == jid
    finally:
        httpd.shutdown()


def test_create_server_serves_health():
    conn = _mem()
    httpd = server.create_server(conn, "t", port=0, long_poll_timeout=0.2, poll_interval=0.05)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{httpd.server_address[1]}"
    try:
        assert _req("GET", base + "/health")[0] == 200
    finally:
        httpd.shutdown()


def test_start_reaper_thread_runs():
    conn = _mem()
    t = server.start_reaper(conn, timeout_s=0, max_retries=5, interval_s=0.05)
    assert t.is_alive()


def test_reaper_logs_failures(capsys, monkeypatch):
    calls = []
    def boom(*a, **k):
        if calls:
            time.sleep(3600)   # park the leaked daemon thread (no stop handle by design)
        calls.append(1)
        raise RuntimeError("db gone")
    monkeypatch.setattr(store, "requeue_stale", boom)
    server.start_reaper(None, interval_s=0.01)
    for _ in range(100):
        if calls:
            break
        time.sleep(0.01)
    time.sleep(0.05)
    assert "reaper" in capsys.readouterr().err


def test_register_duplicate_key_returns_409():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        # Register agent "a" with key "k"
        code, _ = _areq("POST", base + "/agent/register", "k",
                         {"name": "a", "printers": []})
        assert code == 200
        # Register agent "b" with the SAME key "k" -> 409
        code, raw = _areq("POST", base + "/agent/register", "k",
                           {"name": "b", "printers": []})
        assert code == 409
    finally:
        httpd.shutdown()


def test_register_printer_missing_name_returns_400():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, raw = _areq("POST", base + "/agent/register", "k2",
                           {"name": "c", "printers": [{"can_pdf": True}]})
        assert code == 400
    finally:
        httpd.shutdown()


def test_jobs_history_endpoint_requires_bearer_and_lists_newest_first():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    store.enqueue_job(conn, pid, "pdf_base64", "pdf", b"y")
    httpd, base = _serve(conn)
    try:
        assert _req("GET", base + "/jobs")[0] == 401          # bearer required
        code, raw = _req("GET", base + "/jobs", token="t")
        assert code == 200
        jobs = json.loads(raw)["jobs"]
        assert len(jobs) == 2 and jobs[0]["id"] > jobs[1]["id"]   # newest first
        assert jobs[0]["printer_name"] == "Z"
        assert "payload" not in jobs[0]                            # bytes never exposed
    finally:
        httpd.shutdown()


def _dreq(method, url, token=None):
    r = urllib.request.Request(url, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def test_apikeys_issue_list_revoke_and_scoped_client_auth():
    conn = _mem()
    reg = store.register_agent(conn, "w", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)  # bootstrap/admin token = "t"
    try:
        # issuing keys needs the admin (bootstrap) token
        assert _req("POST", base + "/apikeys", body={"label": "n8n"})[0] == 401
        assert _req("POST", base + "/apikeys", token="random", body={"label": "n8n"})[0] == 401
        code, raw = _req("POST", base + "/apikeys", token="t", body={"label": "n8n"})
        assert code == 200
        issued = json.loads(raw)
        newkey = issued["key"]
        assert newkey and issued["label"] == "n8n"
        # the issued per-client key can submit jobs
        code, _ = _req("POST", base + "/jobs", token=newkey,
                       body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        assert code == 200
        # list is admin-only and never leaks the secret
        assert _req("GET", base + "/apikeys", token=newkey)[0] == 401   # client key can't manage keys
        code, raw = _req("GET", base + "/apikeys", token="t")
        keys = json.loads(raw)["keys"]
        assert keys[0]["label"] == "n8n" and "key" not in keys[0] and "key_hash" not in keys[0]
        kid = keys[0]["id"]
        # revoke -> the issued key is no longer authorized
        assert _dreq("DELETE", base + f"/apikeys/{kid}", token="t")[0] == 200
        assert _req("POST", base + "/jobs", token=newkey,
                    body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})[0] == 401
        # bootstrap token still works after revoking client keys
        assert _req("GET", base + "/printers", token="t")[0] == 200
    finally:
        httpd.shutdown()


def test_dashboard_served_at_root_as_html():
    conn = _mem()
    httpd, base = _serve(conn, token="bootstrap-s3cret-value")
    try:
        code, raw = _req("GET", base + "/")
        assert code == 200
        html = raw.decode()
        assert "<!doctype html>" in html.lower()
        assert "printpapi" in html.lower()
        assert "/_next/static/" in html            # the bundle, which fetches with the token
        assert "bootstrap-s3cret-value" not in html  # the shell itself carries no secret
    finally:
        httpd.shutdown()


def test_dashboard_deep_links_resolve_to_their_page():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        for route in ("/print/", "/devices/", "/history/", "/keys/", "/downloads/"):
            code, raw = _req("GET", base + route)
            assert code == 200, route
            # byte-identity, not just "some HTML" — serving the root shell for every deep link
            # would otherwise pass, and each exported page is a distinct file
            expected = (server._WEB_DIR / route.strip("/") / "index.html").read_bytes()
            assert raw == expected, route
    finally:
        httpd.shutdown()


def test_dashboard_assets_get_correct_type_and_cache():
    conn = _mem()
    httpd, base = _serve(conn)
    static = server._WEB_DIR / "_next" / "static"
    # every extension the bundle actually ships must be in the explicit table: a .css served as
    # octet-stream leaves the dashboard unstyled, a mistyped .js won't execute at all
    wanted = {".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2"}
    try:
        for suffix, ctype in wanted.items():
            asset = next((p for p in static.rglob(f"*{suffix}")), None)
            assert asset is not None, f"no {suffix} in the bundle — run `npm run build:app` in web/"
            url = base + "/" + asset.relative_to(server._WEB_DIR).as_posix()
            with urllib.request.urlopen(url) as resp:
                assert resp.status == 200, url
                assert resp.headers["Content-Type"].startswith(ctype), url
                assert "immutable" in resp.headers["Cache-Control"], url
        with urllib.request.urlopen(base + "/") as resp:
            assert resp.headers["Cache-Control"] == "no-cache"   # HTML must not be pinned
    finally:
        httpd.shutdown()


def test_immutable_cache_is_decided_by_the_resolved_file():
    """A dotted path back out of /_next/static/ must not inherit the year-long cache."""
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        with urllib.request.urlopen(base + "/_next/static/..%2f..%2findex.html") as resp:
            assert resp.status == 200
            assert resp.headers["Cache-Control"] == "no-cache"
    finally:
        httpd.shutdown()


def test_unstattable_path_is_a_clean_404():
    """An embedded NUL makes realpath raise on POSIX; it must not kill the request."""
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        for path in ("/a%00b", "/jobs%00", "/_next/static/x%00.js"):
            code, raw = _req("GET", base + path)
            assert code == 404, path
            assert json.loads(raw)["error"] == "not found", path
        assert _req("GET", base + "/health")[0] == 200      # server still healthy afterwards
    finally:
        httpd.shutdown()


def test_head_requests_answer_without_a_body():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        for path, code in (("/", 200), ("/health", 200), ("/nope", 404)):
            req = urllib.request.Request(base + path, method="HEAD")
            try:
                with urllib.request.urlopen(req) as resp:
                    assert resp.status == code, path
                    assert resp.read() == b"", path       # RFC 9110: HEAD carries no body
            except urllib.error.HTTPError as e:
                assert e.code == code, path
                assert e.read() == b"", path
    finally:
        httpd.shutdown()


def test_missing_bundle_explains_itself_instead_of_500(tmp_path, monkeypatch):
    conn = _mem()
    monkeypatch.setattr(server, "_WEB_DIR", tmp_path / "absent")
    monkeypatch.setattr(server, "_INDEX", tmp_path / "absent" / "index.html")
    httpd, base = _serve(conn)
    try:
        code, raw = _req("GET", base + "/")
        assert code == 503
        assert "build:app" in raw.decode()
        assert _req("GET", base + "/health")[0] == 200      # the JSON API is unaffected
    finally:
        httpd.shutdown()


def test_dashboard_path_traversal_is_blocked():
    conn = _mem()
    httpd, base = _serve(conn)
    host, port = base.replace("http://", "").split(":")
    try:
        # urllib collapses "..", so speak HTTP directly to get the raw path through
        for path in ("/../server.py", "/..%2fserver.py", "/%2e%2e/store.py", "/../../app/store.py"):
            s = socket.create_connection((host, int(port)), timeout=5)
            s.sendall(f"GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".encode())
            body = b""
            while chunk := s.recv(4096):
                body += chunk
            s.close()
            assert b" 404 " in body.split(b"\r\n", 1)[0], path
            assert b"import sqlite3" not in body, f"leaked source via {path}"
    finally:
        httpd.shutdown()


def test_metrics_endpoint_prometheus_text():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    j = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    store.claim_job(conn, reg["computer_id"])
    store.finish_job(conn, j, reg["computer_id"], ok=True)
    httpd, base = _serve(conn)
    try:
        assert _req("GET", base + "/metrics")[0] == 401                 # auth required
        code, raw = _req("GET", base + "/metrics", token="t")
        assert code == 200
        text = raw.decode()
        assert 'printpapi_jobs{state="done"} 1' in text
        assert 'printpapi_jobs{state="queued"} 0' in text               # stable zero series
        assert "printpapi_printers_total 1" in text
        assert "# TYPE printpapi_jobs gauge" in text
    finally:
        httpd.shutdown()


def test_unknown_path_is_json_404_not_the_dashboard():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, raw = _req("GET", base + "/nope")
        assert code == 404 and json.loads(raw)["error"] == "not found"
    finally:
        httpd.shutdown()


def test_bad_content_length_rejected():
    import socket as socketlib
    conn = _mem()
    httpd, base = _serve(conn)
    host, port = base.replace("http://", "").split(":")
    try:
        for cl in ("-1", "999999999999", "nan"):
            with socketlib.create_connection((host, int(port)), timeout=5) as sock:
                sock.sendall((
                    f"POST /jobs HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer t\r\n"
                    f"Content-Length: {cl}\r\nConnection: close\r\n\r\n").encode())
                resp = sock.recv(1024).decode()
            assert " 400 " in resp.splitlines()[0], f"Content-Length {cl}: got {resp.splitlines()[0]!r}"
        # server still alive afterwards
        assert _req("GET", base + "/health")[0] == 200
    finally:
        httpd.shutdown()


def test_copies_roundtrip_and_validation_via_http():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        # submit with copies=2, agent claim sees copies=2
        code, _ = _req("POST", base + "/jobs", token="t",
                       body={"printer_id": pid, "type": "raw_base64", "content": "QUJD", "copies": 2})
        assert code == 200
        claim = json.loads(_areq("GET", base + "/agent/jobs", "ak")[1])
        assert claim["copies"] == 2
        # omitted -> default 1
        _req("POST", base + "/jobs", token="t",
             body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        assert json.loads(_areq("GET", base + "/agent/jobs", "ak")[1])["copies"] == 1
        # invalid copies -> 400, and nothing was enqueued (queue is empty on the next claim)
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": pid, "type": "raw_base64", "content": "QUJD",
                          "copies": 0})[0] == 400
        assert _areq("GET", base + "/agent/jobs", "ak")[0] == 204
    finally:
        httpd.shutdown()


def test_register_capabilities_visible_in_printers_via_http():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        caps = {"papers": ["A4"], "bins": ["Tray 1"], "duplex": True, "color": True}
        code, _ = _areq("POST", base + "/agent/register", "ak",
                        {"name": "pc", "printers": [{"name": "HP", "can_pdf": True,
                                                     "capabilities": caps}]})
        assert code == 200
        code, raw = _req("GET", base + "/printers", token="t")
        assert code == 200 and json.loads(raw)["printers"][0]["capabilities"] == caps
    finally:
        httpd.shutdown()


def test_options_roundtrip_and_validation_via_http():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "HP", "can_pdf": True}])
    pid = reg["printer_ids"]["HP"]
    httpd, base = _serve(conn)
    try:
        # submit with options, agent claim carries them (JVBERg== == base64 "%PDF")
        opts = {"duplex": "long-edge", "paper": "A4", "bin": "Tray 1"}
        code, _ = _req("POST", base + "/jobs", token="t",
                       body={"printer_id": pid, "type": "pdf_base64", "content": "JVBERg==",
                             "options": opts})
        assert code == 200
        claim = json.loads(_areq("GET", base + "/agent/jobs", "ak")[1])
        assert claim["options"] == opts
        # options on a raw job -> 400; bad option value -> 400; neither enqueues anything
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": pid, "type": "raw_base64", "content": "QUJD",
                          "options": opts})[0] == 400
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": pid, "type": "pdf_base64", "content": "JVBERg==",
                          "options": {"duplex": "both"}})[0] == 400
        assert _areq("GET", base + "/agent/jobs", "ak")[0] == 204
    finally:
        httpd.shutdown()


def test_cancel_job_via_http_states_and_auth():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        jid = json.loads(_req("POST", base + "/jobs", token="t",
                              body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})[1])["job_id"]
        # cancel requires client auth
        assert _dreq("DELETE", base + f"/jobs/{jid}")[0] == 401
        # queued -> cancelled (200)
        code, raw = _dreq("DELETE", base + f"/jobs/{jid}", token="t")
        assert code == 200 and json.loads(raw)["state"] == "cancelled"
        assert store.get_job(conn, jid)["state"] == "cancelled"
        # agent never sees a cancelled job
        assert _areq("GET", base + "/agent/jobs", "ak")[0] == 204
        # cancelling again (now cancelled, not queued) -> 409
        assert _dreq("DELETE", base + f"/jobs/{jid}", token="t")[0] == 409
        # unknown job -> 404
        assert _dreq("DELETE", base + "/jobs/999999", token="t")[0] == 404
        # a per-client (non-bootstrap) issued key can also cancel (not admin-only)
        newkey = json.loads(_req("POST", base + "/apikeys", token="t", body={"label": "c"})[1])["key"]
        jid2 = store.enqueue_job(conn, pid, "raw_base64", "raw", b"y")
        assert _dreq("DELETE", base + f"/jobs/{jid2}", token=newkey)[0] == 200
    finally:
        httpd.shutdown()


def test_cancel_claimed_job_returns_409_via_http():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    store.claim_job(conn, reg["computer_id"])
    httpd, base = _serve(conn)
    try:
        assert _dreq("DELETE", base + f"/jobs/{jid}", token="t")[0] == 409
    finally:
        httpd.shutdown()


def test_bad_callback_url_rejected_via_http():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": pid, "type": "raw_base64", "content": "QUJD",
                          "callback_url": "file:///etc/passwd"})[0] == 400
        # a valid https callback is accepted
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": pid, "type": "raw_base64", "content": "QUJD",
                          "callback_url": "https://hook/x"})[0] == 200
    finally:
        httpd.shutdown()


def test_webhook_dispatcher_delivers_and_marks():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    a = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x", callback_url="https://h/a", title="T")
    store.claim_job(conn, aid); store.finish_job(conn, a, aid, ok=True)
    posts = []
    server.deliver_webhooks(conn, lambda url, body: posts.append((url, body)), max_attempts=5)
    assert posts == [("https://h/a",
                      {"job_id": a, "state": "done", "error": None, "title": "T", "printer_id": pid})]
    assert store.pending_webhooks(conn, max_attempts=5) == []   # marked delivered, not re-sent
    server.deliver_webhooks(conn, lambda url, body: posts.append((url, body)), max_attempts=5)
    assert len(posts) == 1                                       # second pass sends nothing


def test_webhook_payload_carries_failure_error():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    a = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x", callback_url="https://h/a", title="T")
    store.claim_job(conn, aid); store.finish_job(conn, a, aid, ok=False, error="boom")
    posts = []
    server.deliver_webhooks(conn, lambda url, body: posts.append(body), max_attempts=5)
    assert posts == [{"job_id": a, "state": "failed", "error": "boom",
                      "title": "T", "printer_id": pid}]


def test_webhook_dispatcher_retries_then_gives_up():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    a = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x", callback_url="https://h/a")
    store.claim_job(conn, aid); store.finish_job(conn, a, aid, ok=False, error="boom")
    calls = []
    def boom(url, body):
        calls.append(url)
        raise OSError("hook down")
    for _ in range(5):                                           # more passes than the cap
        server.deliver_webhooks(conn, boom, max_attempts=3)
    assert len(calls) == 3                                       # tried exactly cap times, then gave up
    assert store.get_job(conn, a)["state"] == "failed"           # job state untouched by hook failure


# --- multi-tenancy -----------------------------------------------------------------


def test_orgs_are_created_and_listed_by_the_root_token_only():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        assert _req("POST", base + "/orgs", body={"name": "acme"})[0] == 401
        code, raw = _req("POST", base + "/orgs", token="t", body={"name": "acme"})
        assert code == 200
        oid = json.loads(raw)["id"]
        assert oid != store.DEFAULT_ORG
        orgs = json.loads(_req("GET", base + "/orgs", token="t")[1])["orgs"]
        assert {o["name"] for o in orgs} == {"default", "acme"}
        # an org's own client key is not root: it can neither list nor create orgs
        key = json.loads(_req("POST", base + "/apikeys", token="t",
                              body={"label": "c", "org_id": oid})[1])["key"]
        assert _req("GET", base + "/orgs", token=key)[0] == 401
        assert _req("POST", base + "/orgs", token=key, body={"name": "x"})[0] == 401
        assert _req("POST", base + "/orgs", token="t", body={"name": "  "})[0] == 400
    finally:
        httpd.shutdown()


def test_apikeys_are_issued_into_an_org_and_unknown_orgs_are_rejected():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        oid = json.loads(_req("POST", base + "/orgs", token="t", body={"name": "acme"})[1])["id"]
        code, raw = _req("POST", base + "/apikeys", token="t", body={"label": "c", "org_id": oid})
        assert code == 200 and json.loads(raw)["org_id"] == oid
        keys = json.loads(_req("GET", base + "/apikeys", token="t")[1])["keys"]
        assert keys[0]["org_id"] == oid
        assert _req("POST", base + "/apikeys", token="t",
                    body={"label": "c", "org_id": 4242})[0] == 400      # no such org
        # no org given -> the default org, exactly as before multi-tenancy
        code, raw = _req("POST", base + "/apikeys", token="t", body={"label": "legacy"})
        assert json.loads(raw)["org_id"] == store.DEFAULT_ORG
    finally:
        httpd.shutdown()


def _two_org_fixture(conn):
    """org 1 with printer Z + a queued job, 'acme' with printer Y and its own client key."""
    other = store.create_org(conn, "acme")
    a = store.register_agent(conn, "pc", "ka", [{"name": "Z", "can_pdf": False}])
    b = store.register_agent(conn, "pc", "kb", [{"name": "Y", "can_pdf": False}], org_id=other)
    store.add_api_key(conn, "acme", "acme-key", org_id=other)
    ja = store.enqueue_job(conn, a["printer_ids"]["Z"], "raw_base64", "raw", b"a")
    return other, a, b, ja


def test_client_key_is_confined_to_its_own_org():
    conn = _mem()
    other, a, b, ja = _two_org_fixture(conn)
    httpd, base = _serve(conn)
    try:
        # a foreign job id is 404 — never 200, never 403 (which would confirm it exists)
        assert _req("GET", base + f"/jobs/{ja}", token="acme-key")[0] == 404
        jb = json.loads(_req("POST", base + "/jobs", token="acme-key",
                             body={"printer_id": b["printer_ids"]["Y"], "type": "raw_base64",
                                   "content": "QUJD"})[1])["job_id"]
        assert _req("GET", base + f"/jobs/{jb}", token="acme-key")[0] == 200
        # printing to a foreign printer is "unknown printer" (400), same as a nonexistent one
        assert _req("POST", base + "/jobs", token="acme-key",
                    body={"printer_id": a["printer_ids"]["Z"], "type": "raw_base64",
                          "content": "QUJD"})[0] == 400
        # lists carry only the org's own rows
        printers = json.loads(_req("GET", base + "/printers", token="acme-key")[1])["printers"]
        assert [p["name"] for p in printers] == ["Y"]
        jobs = json.loads(_req("GET", base + "/jobs", token="acme-key")[1])["jobs"]
        assert [j["id"] for j in jobs] == [jb]
        assert "printpapi_printers_total 1" in _req("GET", base + "/metrics",
                                                    token="acme-key")[1].decode()
        # cancelling someone else's job is a 404 and leaves it untouched
        assert _dreq("DELETE", base + f"/jobs/{ja}", token="acme-key")[0] == 404
        assert store.get_job(conn, ja)["state"] == "queued"
        # key management stays root-only
        assert _req("GET", base + "/apikeys", token="acme-key")[0] == 401
    finally:
        httpd.shutdown()


def test_root_token_spans_all_orgs():
    conn = _mem()
    other, a, b, ja = _two_org_fixture(conn)
    jb = store.enqueue_job(conn, b["printer_ids"]["Y"], "raw_base64", "raw", b"b", org_id=other)
    httpd, base = _serve(conn)
    try:
        printers = json.loads(_req("GET", base + "/printers", token="t")[1])["printers"]
        assert {p["name"] for p in printers} == {"Z", "Y"}
        jobs = json.loads(_req("GET", base + "/jobs", token="t")[1])["jobs"]
        assert {j["id"] for j in jobs} == {ja, jb}
        assert "printpapi_printers_total 2" in _req("GET", base + "/metrics", token="t")[1].decode()
        assert _req("GET", base + f"/jobs/{jb}", token="t")[0] == 200      # reads any org's job
        assert _dreq("DELETE", base + f"/jobs/{jb}", token="t")[0] == 200  # and cancels it
        # and prints to any org's printer
        assert _req("POST", base + "/jobs", token="t",
                    body={"printer_id": b["printer_ids"]["Y"], "type": "raw_base64",
                          "content": "QUJD"})[0] == 200
    finally:
        httpd.shutdown()


def test_agent_registers_into_the_org_of_its_key():
    conn = _mem()
    oid = store.create_org(conn, "acme")
    store.add_api_key(conn, "acme-agent", "acme-agent-key", org_id=oid)
    httpd, base = _serve(conn)
    try:
        code, raw = _areq("POST", base + "/agent/register", "acme-agent-key",
                          {"name": "pc", "printers": [{"name": "Z"}]})
        assert code == 200
        pid = json.loads(raw)["printer_ids"]["Z"]
        assert conn.execute("SELECT org_id FROM printers WHERE id=?",
                            (pid,)).fetchone()["org_id"] == oid
        # a key that is not an issued client key still enrolls into the default org (legacy path)
        code, raw = _areq("POST", base + "/agent/register", "plain-key",
                          {"name": "old", "printers": [{"name": "Y"}]})
        assert code == 200
        pid2 = json.loads(raw)["printer_ids"]["Y"]
        assert conn.execute("SELECT org_id FROM printers WHERE id=?",
                            (pid2,)).fetchone()["org_id"] == store.DEFAULT_ORG
        # the org's key sees its own printer only, and its jobs reach that agent
        printers = json.loads(_req("GET", base + "/printers", token="acme-agent-key")[1])["printers"]
        assert [p["name"] for p in printers] == ["Z"]
        jid = json.loads(_req("POST", base + "/jobs", token="acme-agent-key",
                              body={"printer_id": pid, "type": "raw_base64",
                                    "content": "QUJD"})[1])["job_id"]
        assert json.loads(_areq("GET", base + "/agent/jobs", "acme-agent-key")[1])["job_id"] == jid
    finally:
        httpd.shutdown()


def test_job_title_roundtrips_through_http():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    httpd, base = _serve(conn)
    try:
        code, _ = _req("POST", base + "/jobs", token="t",
                       body={"printer_id": pid, "type": "raw_base64",
                             "content": "QUJD", "title": "Label #5"})
        assert code == 200
        jobs = json.loads(_req("GET", base + "/jobs", token="t")[1])["jobs"]
        assert jobs[0]["title"] == "Label #5"
        assert jobs[0]["agent_name"] == "pc"
    finally:
        httpd.shutdown()


def test_computers_endpoint_lists_agents_scoped_to_the_org():
    conn = _mem()
    other, a, b, ja = _two_org_fixture(conn)
    httpd, base = _serve(conn)
    try:
        assert _req("GET", base + "/computers")[0] == 401
        cs = json.loads(_req("GET", base + "/computers", token="acme-key")[1])["computers"]
        assert [c["id"] for c in cs] == [b["computer_id"]]
        assert cs[0]["name"] == "pc" and cs[0]["online"] is True and cs[0]["printers"] == 1
        assert len(json.loads(_req("GET", base + "/computers", token="t")[1])["computers"]) == 2
    finally:
        httpd.shutdown()


def test_org_event_url_is_set_by_root_only_and_must_be_http():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        oid = json.loads(_req("POST", base + "/orgs", token="t", body={"name": "acme"})[1])["id"]
        key = json.loads(_req("POST", base + "/apikeys", token="t",
                              body={"label": "c", "org_id": oid})[1])["key"]
        assert _req("PUT", base + f"/orgs/{oid}", token=key,
                    body={"event_url": "https://h"})[0] == 401          # its own key is not root
        assert _req("PUT", base + f"/orgs/{oid}", token="t",
                    body={"event_url": "ftp://h"})[0] == 400
        assert _req("PUT", base + f"/orgs/{oid}", token="t",
                    body={"event_url": "https://h"})[0] == 200
        orgs = {o["id"]: o for o in json.loads(_req("GET", base + "/orgs", token="t")[1])["orgs"]}
        assert orgs[oid]["event_url"] == "https://h"
        assert _req("PUT", base + "/orgs/4242", token="t", body={"event_url": "https://h"})[0] == 404
        assert _req("PUT", base + f"/orgs/{oid}", token="t", body={"event_url": None})[0] == 200
        orgs = {o["id"]: o for o in json.loads(_req("GET", base + "/orgs", token="t")[1])["orgs"]}
        assert orgs[oid]["event_url"] is None                            # null clears it
    finally:
        httpd.shutdown()


def test_agent_liveness_events_are_posted_to_the_orgs_event_url():
    conn = _mem()
    store.set_org_event_url(conn, store.DEFAULT_ORG, "https://hooks.example/x")
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    sent = []
    now = time.time()
    server.deliver_agent_events(conn, lambda u, b: sent.append((u, b)), 60, now=now)
    assert sent == []                                                    # still online
    server.deliver_agent_events(conn, lambda u, b: sent.append((u, b)), 60, now=now + 300)
    (url, body), = sent
    assert url == "https://hooks.example/x"
    assert body["event"] == "computer_offline" and body["computer_id"] == reg["computer_id"]
    assert body["name"] == "pc" and body["org_id"] == store.DEFAULT_ORG
    assert body["last_seen_at"] > 0


def test_a_failing_event_post_does_not_abort_the_pass():
    conn = _mem()
    store.set_org_event_url(conn, store.DEFAULT_ORG, "https://down.example/x")
    store.register_agent(conn, "pc", "ak", [])

    def boom(url, body):
        raise RuntimeError("connection refused")

    server.deliver_agent_events(conn, boom, 60, now=time.time() + 300)   # must not raise
