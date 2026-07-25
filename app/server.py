# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
import hmac
import json
import os
import re
import secrets
import sqlite3
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote

from app import store
from app.dispatch import (decode_payload, agent_mode, parse_copies, parse_callback_url,
                          parse_options, DispatchError, FetchError, _http_get, _http_post)

_MAX_BODY = 32 * 1024 * 1024  # ponytail: flat 32 MB body cap; make env-tunable if someone needs bigger

_JOB_ID = re.compile(r"^/jobs/(\d+)$")
_AGENT_PAYLOAD = re.compile(r"^/agent/jobs/(\d+)/payload$")
_AGENT_RESULT = re.compile(r"^/agent/jobs/(\d+)/result$")
_APIKEY_ID = re.compile(r"^/apikeys/(\d+)$")
_ORG_ID = re.compile(r"^/orgs/(\d+)$")

# The dashboard is a static, secret-free Next.js export in app/web (source in web/, built with
# `npm run build:app`). It prompts for the API token, keeps it in localStorage, and calls the JSON
# endpoints with the bearer header — so all data stays behind auth and the bundle itself carries
# nothing sensitive. Serving it from here keeps the server stdlib-only: no Node at runtime.
_WEB_DIR = Path(__file__).resolve().parent / "web"
_INDEX = _WEB_DIR / "index.html"
_IMMUTABLE_DIR = _WEB_DIR / "_next" / "static"   # content-hashed names, safe to cache forever

# Explicit table: mimetypes.guess_type consults the Windows registry, where .js is routinely
# mapped to text/plain — which browsers refuse to execute as a module.
_CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json",
    ".txt": "text/plain; charset=utf-8",
    ".svg": "image/svg+xml",
    ".ico": "image/x-icon",
    ".png": "image/png",
    ".webmanifest": "application/manifest+json",
    ".woff": "font/woff",
    ".woff2": "font/woff2",
}

_NO_DASHBOARD_HTML = """<!doctype html><meta charset="utf-8"><title>printpapi</title>
<body style="font:15px system-ui;margin:3rem auto;max-width:34rem;padding:0 1rem">
<h1>printpapi</h1><p>The dashboard bundle is missing. Build it once:</p>
<pre>cd web &amp;&amp; npm install &amp;&amp; npm run build:app</pre>
<p>The JSON API works either way.</p>"""


def _static_path(url_path):
    """Map a URL path to a file inside app/web, or None if it escapes the dir or is missing."""
    rel = unquote(url_path.split("?", 1)[0].split("#", 1)[0]).lstrip("/")
    try:
        target = (_WEB_DIR / rel).resolve() if rel else _WEB_DIR
        if target != _WEB_DIR and not target.is_relative_to(_WEB_DIR):
            return None                   # ../ traversal — an absolute rel path lands here too
        if target.is_dir():
            target = target / "index.html"    # /devices/ -> devices/index.html
        return target if target.is_file() else None
    except (OSError, ValueError):
        # A path the OS refuses to even stat — on POSIX an embedded NUL ("/a%00b") makes
        # realpath raise ValueError, which would otherwise kill the request with a traceback.
        return None


_JOB_STATES = ("queued", "claimed", "done", "failed", "cancelled")


def _prometheus(m):
    """Render a store.metrics() snapshot as Prometheus text (v0.0.4)."""
    out = ["# HELP printpapi_jobs Jobs by state.", "# TYPE printpapi_jobs gauge"]
    for s in _JOB_STATES:                       # emit all states (incl. 0) for stable series
        out.append(f'printpapi_jobs{{state="{s}"}} {m["jobs"].get(s, 0)}')
    for name, help_, val in (
        ("printpapi_agents_online", "Agents seen within the online window.", m["agents_online"]),
        ("printpapi_agents_total", "Registered agents.", m["agents_total"]),
        ("printpapi_printers_total", "Registered printers.", m["printers_total"]),
    ):
        out += [f"# HELP {name} {help_}", f"# TYPE {name} gauge", f"{name} {val}"]
    return "\n".join(out) + "\n"


def make_handler(*, conn, token, agent_auth=store.authenticate_agent, fetch_url=None,
                 long_poll_timeout=25.0, poll_interval=1.0, online_window_s=60):
    fetch = fetch_url or _http_get

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            if os.environ.get("LOG_REQUESTS"):
                super().log_message(fmt, *args)

        def _json(self, code, obj):
            payload = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def _presented_key(self):
            auth = self.headers.get("Authorization", "")
            return auth[7:] if auth.startswith("Bearer ") else ""

        def _admin_ok(self):
            # The bootstrap PRINTAPI_TOKEN is the root/admin credential (orgs, keys) and the only
            # credential that spans orgs.
            return hmac.compare_digest(self._presented_key(), token)

        def _client_org(self):
            """Resolve the presented key to the org this request may act in.

            (True, None)  root — the bootstrap token: no org filter, sees and acts on every org.
            (True, <id>)  an issued client key: confined to that org, foreign ids read as 404.
            (False, None) no valid key.
            """
            key = self._presented_key()
            if hmac.compare_digest(key, token):     # bootstrap token is always a valid client too
                return True, None
            row = store.authenticate_client(conn, key)   # or any active per-client key
            return (True, row["org_id"]) if row else (False, None)

        def _read_json(self):
            length = int(self.headers.get("Content-Length", 0))   # ValueError -> caller's 400
            if length < 0 or length > _MAX_BODY:
                raise ValueError("bad content-length")
            return json.loads(self.rfile.read(length) or b"{}")

        def _agent_id(self):
            key = self._presented_key()
            return agent_auth(conn, key) if key else None

        def _html(self, code, html, body=True):
            data = html.encode()
            self.send_response(code)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if body:                      # a HEAD response must not carry one
                self.wfile.write(data)

        def _serve_dashboard(self, body=True):
            """Serve the static dashboard bundle. Returns False if the path isn't one of ours."""
            target = _static_path(self.path)
            if target is None:
                if self.path in ("/", "/index.html") and not _INDEX.is_file():
                    self._html(503, _NO_DASHBOARD_HTML, body)   # bundle not built yet
                    return True
                return False
            data = target.read_bytes()
            self.send_response(200)
            self.send_header("Content-Type",
                             _CONTENT_TYPES.get(target.suffix.lower(), "application/octet-stream"))
            self.send_header("Content-Length", str(len(data)))
            # Decide from the *resolved* file, not the raw URL: "/_next/static/..%2f..%2findex.html"
            # is inside the bundle but is not a content-hashed asset.
            self.send_header("Cache-Control",
                             "public, max-age=31536000, immutable"
                             if target.is_relative_to(_IMMUTABLE_DIR) else "no-cache")
            self.end_headers()
            if body:
                self.wfile.write(data)
            return True

        def _empty(self, code):
            self.send_response(code)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_HEAD(self):
            # Uptime checks and the browser's own probes HEAD the dashboard and /health;
            # without this BaseHTTPRequestHandler answers 501 to both.
            if self.path == "/health":
                return self._empty(200)
            if self._serve_dashboard(body=False):
                return
            self._empty(404)

        def do_GET(self):
            if self.path in ("/", "/index.html"):
                return None if self._serve_dashboard() else self._json(404, {"error": "not found"})
            if self.path == "/health":
                return self._json(200, {"ok": True})
            if self.path == "/metrics":
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                body = _prometheus(store.metrics(conn, online_window_s, org_id=org)).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            if self.path == "/jobs":
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"jobs": store.recent_jobs(conn, org_id=org)})
            m = _JOB_ID.match(self.path)
            if m:
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                job = store.get_job(conn, int(m.group(1)), org_id=org)
                return self._json(200, job) if job else self._json(404, {"error": "not found"})
            if self.path == "/printers":
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200,
                                  {"printers": store.list_printers(conn, online_window_s, org_id=org)})
            if self.path == "/computers":
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200,
                                  {"computers": store.list_agents(conn, online_window_s, org_id=org)})
            if self.path == "/apikeys":
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"keys": store.list_api_keys(conn)})
            if self.path == "/orgs":
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"orgs": store.list_orgs(conn)})
            mp = _AGENT_PAYLOAD.match(self.path)
            if mp:
                aid = self._agent_id()
                if aid is None:
                    return self._json(401, {"error": "unauthorized"})
                data = store.get_payload(conn, int(mp.group(1)), aid)
                if data is None:
                    return self._json(404, {"error": "not found"})
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            if self.path == "/agent/jobs":
                aid = self._agent_id()
                if aid is None:
                    return self._json(401, {"error": "unauthorized"})
                deadline = time.time() + long_poll_timeout
                while True:
                    job = store.claim_job(conn, aid)
                    if job is not None:
                        return self._json(200, job)
                    if time.time() >= deadline:
                        self.send_response(204)
                        self.end_headers()
                        return
                    time.sleep(poll_interval)  # ponytail: DB poll; notify-on-enqueue if latency matters
            # Last: the dashboard bundle (/_next/..., /devices/, …). After the API routes, so a
            # stray file in app/web can never shadow an endpoint.
            if self._serve_dashboard():
                return
            self._json(404, {"error": "not found"})

        def do_POST(self):
            if self.path == "/jobs":
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                try:
                    data = decode_payload(body, fetch_url=fetch)
                    mode = agent_mode(body.get("type"))
                    copies = parse_copies(body)
                    callback_url = parse_callback_url(body)
                    options = parse_options(body, mode)
                    jid = store.enqueue_job(conn, body.get("printer_id"), body.get("type"),
                                            mode, data, title=body.get("title"), copies=copies,
                                            callback_url=callback_url, options=options,
                                            org_id=org)
                except FetchError as e:
                    return self._json(502, {"error": f"downstream: {e}"})
                except (DispatchError, store.UnknownPrinter) as e:
                    return self._json(400, {"error": str(e)})
                return self._json(200, {"job_id": jid})
            if self.path == "/apikeys":
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                label = (body.get("label") or "client").strip() or "client"
                org_id = body.get("org_id") or store.DEFAULT_ORG
                if not store.org_exists(conn, org_id):
                    return self._json(400, {"error": "unknown org"})
                key = secrets.token_urlsafe(32)
                kid = store.add_api_key(conn, label, key, org_id=org_id)
                return self._json(200, {"id": kid, "label": label, "org_id": org_id, "key": key})
            if self.path == "/orgs":
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                name = (body.get("name") or "").strip()
                if not name:
                    return self._json(400, {"error": "name required"})
                return self._json(200, {"id": store.create_org(conn, name), "name": name})
            if self.path == "/agent/register":
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                auth = self.headers.get("Authorization", "")
                key = auth[7:] if auth.startswith("Bearer ") else ""
                # An agent enrolls into the org of the key it presents: an issued client key puts it
                # in that org, anything else in the default org (the pre-multi-tenancy behaviour, so
                # existing agents keep their key and their org).
                # ponytail: the agent key doubles as that org's client key (issue one key per agent
                # to keep the blast radius sane), and revoking it stops client calls but not an
                # already-registered agent's polling — add an agent-only key kind, checked on every
                # agent request, if either matters.
                row = store.authenticate_client(conn, key)
                # First contact binds name->key; re-register requires the same key.
                try:
                    reg = store.register_agent(conn, body.get("name"), key,
                                               body.get("printers", []),
                                               org_id=row["org_id"] if row else store.DEFAULT_ORG)
                except store.AuthError as e:
                    return self._json(401, {"error": str(e)})
                except ValueError as e:
                    return self._json(400, {"error": str(e)})
                except sqlite3.IntegrityError:
                    return self._json(409, {"error": "agent key already in use"})
                return self._json(200, reg)
            mr = _AGENT_RESULT.match(self.path)
            if mr:
                aid = self._agent_id()
                if aid is None:
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                ok = store.finish_job(conn, int(mr.group(1)), aid, bool(body.get("ok")),
                                      body.get("error"))
                return self._json(200, {"ok": True}) if ok else self._json(404, {"error": "not found"})
            self._json(404, {"error": "not found"})

        def do_PUT(self):
            mo = _ORG_ID.match(self.path)
            if mo:
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                try:
                    # Same http(s) check as a job's callback_url; null/"" clears the URL.
                    url = parse_callback_url({"callback_url": body.get("event_url")})
                except DispatchError as e:
                    return self._json(400, {"error": str(e).replace("callback_url", "event_url")})
                if not store.set_org_event_url(conn, int(mo.group(1)), url):
                    return self._json(404, {"error": "not found"})
                return self._json(200, {"ok": True, "event_url": url})
            self._json(404, {"error": "not found"})

        def do_DELETE(self):
            m = _APIKEY_ID.match(self.path)
            if m:
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                ok = store.revoke_api_key(conn, int(m.group(1)))
                return self._json(200, {"ok": True}) if ok else self._json(404, {"error": "not found"})
            mj = _JOB_ID.match(self.path)
            if mj:
                ok, org = self._client_org()
                if not ok:
                    return self._json(401, {"error": "unauthorized"})
                res = store.cancel_job(conn, int(mj.group(1)), org_id=org)
                if res == "cancelled":
                    return self._json(200, {"ok": True, "state": "cancelled"})
                if res == "not_found":
                    return self._json(404, {"error": "not found"})
                return self._json(409, {"error": "job not cancellable (already claimed or finished)"})
            self._json(404, {"error": "not found"})

    return Handler


def create_server(conn, token, host="127.0.0.1", port=0, **handler_kwargs):
    handler = make_handler(conn=conn, token=token, **handler_kwargs)
    return ThreadingHTTPServer((host, port), handler)


def deliver_webhooks(conn, post, max_attempts):
    """One delivery pass: POST each pending terminal job to its callback_url (outside the DB
    lock). 2xx (no exception from `post`) -> mark delivered; any error -> bump the attempt count
    (retried next pass until the cap, then given up). One bad URL never aborts the pass."""
    for job in store.pending_webhooks(conn, max_attempts):
        payload = {"job_id": job["job_id"], "state": job["state"], "error": job["error"],
                   "title": job["title"], "printer_id": job["printer_id"]}
        try:
            post(job["callback_url"], payload)
        except Exception as e:                   # only a POST failure counts as a delivery failure
            store.bump_webhook_attempt(conn, job["job_id"])
            print(f"webhook {job['job_id']} -> {job['callback_url']} failed: {e}", file=sys.stderr)
        else:                                    # mark outside the post try: a mark-time DB error
            store.mark_webhook_delivered(conn, job["job_id"])   # must not look like a POST failure


def deliver_agent_events(conn, post, online_window_s, now=None):
    """One pass of agent liveness events: POST `computer_offline` / `computer_online` to the
    event_url of the agent's org (fleet operators watch customer-site machines with these).
    # ponytail: at-most-once — the edge is consumed when it is claimed, so a failed POST is logged,
    # not retried. Add an attempt counter like the job hooks if these ever need a guarantee."""
    for ev in store.claim_agent_transitions(conn, online_window_s, now=now):
        payload = {"event": f"computer_{ev['event']}", "computer_id": ev["agent_id"],
                   "name": ev["name"], "org_id": ev["org_id"], "last_seen_at": ev["last_seen_at"]}
        try:
            post(ev["event_url"], payload)
        except Exception as e:
            print(f"agent event {payload['event']} for {ev['name']!r} -> {ev['event_url']} "
                  f"failed: {e}", file=sys.stderr)


def _hook_post(url, body):
    return _http_post(url, body, timeout=10)   # background sender: shorter than the 30s default


def start_webhook_dispatcher(conn, *, post=_hook_post, interval_s=5, max_attempts=5,
                             online_window_s=60):
    # ponytail: one thread, sequential delivery — a slow callback delays the ones behind it (bounded
    # by the 10s timeout x attempt cap). A worker pool / async delivery only if hook volume grows.
    def loop():
        while True:
            try:
                deliver_webhooks(conn, post, max_attempts)
                deliver_agent_events(conn, post, online_window_s)
            except Exception as e:
                print(f"webhook dispatcher error: {e}", file=sys.stderr)
            time.sleep(interval_s)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def start_reaper(conn, *, timeout_s=300, max_retries=2, interval_s=30):
    def loop():
        while True:
            try:
                store.requeue_stale(conn, timeout_s, max_retries)
            except Exception as e:
                print(f"reaper error: {e}", file=sys.stderr)
            time.sleep(interval_s)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def main():
    token = os.environ["PRINTAPI_TOKEN"]
    db_path = os.environ.get("PRINT_DB", "printpapi.db")
    port = int(os.environ.get("PRINT_PORT", "3460"))
    conn = store.connect(db_path)
    store.init_db(conn)
    start_reaper(conn)
    start_webhook_dispatcher(conn)
    httpd = create_server(conn, token, host="0.0.0.0", port=port)
    print(f"printpapi listening on :{port}")
    httpd.serve_forever()


if __name__ == "__main__":
    main()
