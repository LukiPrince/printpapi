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

from app import store
from app.dispatch import decode_payload, agent_mode, parse_copies, DispatchError, FetchError, _http_get

_MAX_BODY = 32 * 1024 * 1024  # ponytail: flat 32 MB body cap; make env-tunable if someone needs bigger

_JOB_ID = re.compile(r"^/jobs/(\d+)$")
_AGENT_PAYLOAD = re.compile(r"^/agent/jobs/(\d+)/payload$")
_AGENT_RESULT = re.compile(r"^/agent/jobs/(\d+)/result$")
_APIKEY_ID = re.compile(r"^/apikeys/(\d+)$")

# Static, secret-free single-page dashboard. It prompts for the API token, keeps it in
# localStorage, and calls /printers + /jobs with the bearer header — so all data stays behind
# auth and the HTML itself carries nothing sensitive. Test-print picks a PDF for PDF-capable
# printers and a ZPL label otherwise (gotcha #1: never send PDF bytes raw to a label printer).
_TEST_PDF_B64 = ("JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIg"
                 "MCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBv"
                 "YmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCA2MTIgNzkyXSAvUmVz"
                 "b3VyY2VzIDw8IC9Gb250IDw8IC9GMSA0IDAgUiA+PiA+PiAvQ29udGVudHMgNSAwIFIgPj4KZW5kb2Jq"
                 "CjQgMCBvYmoKPDwgL1R5cGUgL0ZvbnQgL1N1YnR5cGUgL1R5cGUxIC9CYXNlRm9udCAvSGVsdmV0aWNh"
                 "ID4+CmVuZG9iago1IDAgb2JqCjw8IC9MZW5ndGggNTAgPj4Kc3RyZWFtCkJUIC9GMSAyNCBUZiA3MiA3"
                 "MDAgVGQgKHByaW50cGFwaSB0ZXN0IHBhZ2UpIFRqIEVUCmVuZHN0cmVhbQplbmRvYmoKeHJlZgowIDYK"
                 "MDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAK"
                 "MDAwMDAwMDExNSAwMDAwMCBuIAowMDAwMDAwMjQxIDAwMDAwIG4gCjAwMDAwMDAzMTEgMDAwMDAgbiAK"
                 "dHJhaWxlcgo8PCAvU2l6ZSA2IC9Sb290IDEgMCBSID4+CnN0YXJ0eHJlZgo0MTEKJSVFT0Y=")
_TEST_ZPL_B64 = "XlhBXkZPNDAsNDBeQUROLDM2LDIwXkZEcHJpbnRwYXBpIHRlc3ReRlNeWFo="

# Static, secret-free dashboard SPA loaded from app/dashboard.html. It prompts for the API token,
# keeps it in localStorage, and calls the JSON endpoints with the bearer header — so all data stays
# behind auth and the HTML carries nothing sensitive. __PDF_B64__/__ZPL_B64__ are the built-in test
# payloads (gotcha #1: PDF only reaches PDF-capable printers; label printers get the ZPL label).
_DASHBOARD_HTML = (
    (Path(__file__).resolve().parent / "dashboard.html").read_text(encoding="utf-8")
    .replace("__PDF_B64__", _TEST_PDF_B64)
    .replace("__ZPL_B64__", _TEST_ZPL_B64)
)


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
            # The bootstrap PRINTAPI_TOKEN is the root/admin credential (issues & revokes keys).
            return hmac.compare_digest(self._presented_key(), token)

        def _client_ok(self):
            key = self._presented_key()
            if hmac.compare_digest(key, token):     # bootstrap token is always a valid client too
                return True
            return store.authenticate_client(conn, key) is not None   # or any active per-client key

        def _read_json(self):
            length = int(self.headers.get("Content-Length", 0))   # ValueError -> caller's 400
            if length < 0 or length > _MAX_BODY:
                raise ValueError("bad content-length")
            return json.loads(self.rfile.read(length) or b"{}")

        def _agent_id(self):
            key = self._presented_key()
            return agent_auth(conn, key) if key else None

        def _html(self, code, body):
            data = body.encode()
            self.send_response(code)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path in ("/", "/index.html"):
                return self._html(200, _DASHBOARD_HTML)
            if self.path == "/health":
                return self._json(200, {"ok": True})
            if self.path == "/jobs":
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"jobs": store.recent_jobs(conn)})
            m = _JOB_ID.match(self.path)
            if m:
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                job = store.get_job(conn, int(m.group(1)))
                return self._json(200, job) if job else self._json(404, {"error": "not found"})
            if self.path == "/printers":
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"printers": store.list_printers(conn, online_window_s)})
            if self.path == "/apikeys":
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"keys": store.list_api_keys(conn)})
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
            self._json(404, {"error": "not found"})

        def do_POST(self):
            if self.path == "/jobs":
                if not self._client_ok():
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                try:
                    data = decode_payload(body, fetch_url=fetch)
                    mode = agent_mode(body.get("type"))
                    copies = parse_copies(body)
                    jid = store.enqueue_job(conn, body.get("printer_id"), body.get("type"),
                                            mode, data, title=body.get("title"), copies=copies)
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
                key = secrets.token_urlsafe(32)
                kid = store.add_api_key(conn, label, key)
                return self._json(200, {"id": kid, "label": label, "key": key})
            if self.path == "/agent/register":
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                auth = self.headers.get("Authorization", "")
                key = auth[7:] if auth.startswith("Bearer ") else ""
                # First contact binds name->key; re-register requires the same key.
                try:
                    reg = store.register_agent(conn, body.get("name"), key,
                                               body.get("printers", []))
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

        def do_DELETE(self):
            m = _APIKEY_ID.match(self.path)
            if m:
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                ok = store.revoke_api_key(conn, int(m.group(1)))
                return self._json(200, {"ok": True}) if ok else self._json(404, {"error": "not found"})
            self._json(404, {"error": "not found"})

    return Handler


def create_server(conn, token, host="127.0.0.1", port=0, **handler_kwargs):
    handler = make_handler(conn=conn, token=token, **handler_kwargs)
    return ThreadingHTTPServer((host, port), handler)


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
    httpd = create_server(conn, token, host="0.0.0.0", port=port)
    print(f"printpapi listening on :{port}")
    httpd.serve_forever()


if __name__ == "__main__":
    main()
