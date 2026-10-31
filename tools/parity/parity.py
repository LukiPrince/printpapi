#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Black-box parity harness: does the Rust server answer exactly like the Python one?

Server A is the Python server taken from a git ref (default v2.5.1, via `git archive`, so this
keeps working after app/ is gone from the tree). Server B is the Rust binary. Both get a fresh DB
and the same environment; one scripted scenario (every route family) runs against each and the
answers are compared step by step. See README.md.

Python 3 stdlib only.
"""
import argparse
import atexit
import base64
import difflib
import hashlib
import hmac
import http.client
import io
import json
import os
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# Deliberate deviations: step name -> reason. Such diffs are printed but do not fail the run.
_HYPER_CL = ("hyper rejects an unparseable Content-Length at the protocol level: same 400 status, "
             "but before routing, so without the route's JSON body")
KNOWN_DIFFS = {
    "auth: header trailing space": "hyper trims whitespace around header values (RFC 9110 field "
                                   "values exclude it); Python kept the trailing space in the token",
    "agent: register null name": "a missing agent name is a 400 'name required'; Python hit a NOT NULL "
                                 "violation and answered a misleading 409 'agent key already in use'",
    "jobs: bad content-length": _HYPER_CL,
    "jobs: negative content-length": _HYPER_CL,
    "shopify: bad content-length": _HYPER_CL,
    "pn: post bad content-length": _HYPER_CL,
    "billing: bad content-length": _HYPER_CL,
    "billing: bad json": "the JSON decode error text comes from serde_json's position, not Python's "
                         "decoder (same 400, same 'line L column C (char N)' shape)",
    "dashboard: hashed js asset": "the v3 dashboard bundle was rebuilt (new Downloads page), so its "
                                  "content-hashed asset names differ from v2.5.1's",
    "dashboard: HEAD hashed js asset": "see 'dashboard: hashed js asset'",
}

TOKEN = "parity-root-token"
BILLING_SECRET = "parity-billing-secret"
PUBLIC_URL = "https://print.example.test"
PLANS = [
    {"id": "free", "name": "Free", "jobs": 3, "price": "0 EUR"},
    {"id": "pro", "name": "Pro", "jobs": None, "price": "9 EUR/month",
     "checkout_url": "https://pay.example.test/checkout?org={org}"},
]

# --- small helpers ----------------------------------------------------------------------------


def b64(data):
    return base64.b64encode(data).decode()


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def g(r, *path):
    """Safe nested lookup into a response's JSON (None when anything is missing)."""
    v = getattr(r, "j", r)
    for p in path:
        try:
            v = v[p]
        except (KeyError, IndexError, TypeError):
            return None
    return v


# --- HTTP client --------------------------------------------------------------------------------


class Resp:
    def __init__(self, status=None, headers=None, body=b"", error=None):
        self.status, self.headers, self.body, self.error = status, headers or {}, body, error
        self._j = False

    @property
    def j(self):
        if self._j is False:
            try:
                self._j = json.loads(self.body)
            except ValueError:
                self._j = None
        return self._j

    @property
    def ctype(self):
        return self.headers.get("content-type", "").split(";", 1)[0].strip().lower()


def http_call(port, method, path, headers=None, body=None, content_length=None, timeout=45):
    """One request on a fresh connection; the Content-Length can be forged (str)."""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        conn.putrequest(method, path, skip_accept_encoding=True)
        h = dict(headers or {})
        if content_length is not None:
            h["Content-Length"] = str(content_length)
        elif body is not None:
            h["Content-Length"] = str(len(body))
        h.setdefault("Connection", "close")
        for k, v in h.items():
            conn.putheader(k, v)
        conn.endheaders(body or None)
        r = conn.getresponse()
        return Resp(r.status, {k.lower(): v for k, v in r.getheaders()}, r.read())
    except (OSError, http.client.HTTPException) as e:
        # A Python handler that crashed on garbage input closes the socket without an answer.
        return Resp(error=type(e).__name__)
    finally:
        conn.close()


def auth_header(auth):
    if auth is None:
        return None
    if isinstance(auth, tuple):
        kind, val = auth
        return {"basic": lambda: "Basic " + b64(f"{val}:".encode()), "raw": lambda: val}[kind]()
    return "Bearer " + auth


# --- normalisation + comparison ----------------------------------------------------------------

TS_KEYS = {"created_at", "finished_at", "last_seen_at", "expires_at", "createTimestamp"}
SECRET_KEYS = {"key", "token"}
_SECRET_TEXT = re.compile(r"sess_[A-Za-z0-9_-]+")


def norm(v, label, key=None):
    """Volatile values (timestamps, issued secrets, per-server URLs) -> stable placeholders."""
    if isinstance(v, dict):
        return {k: norm(x, label, k) for k, x in v.items()}
    if isinstance(v, list):
        return [norm(x, label) for x in v]
    if key in TS_KEYS and v is not None:
        return "<ts>"
    if key in SECRET_KEYS and isinstance(v, str):
        return "<secret>"
    if isinstance(v, float) and 1.5e9 < v < 2.5e9:
        return "<ts>"
    if isinstance(v, str):
        return norm_text(v, label)
    return v


def norm_text(t, label):
    t = _SECRET_TEXT.sub("<secret>", t.replace(f"/hook/{label}/", "/hook/<srv>/"))
    t = re.sub(r"http://127\.0\.0\.1:\d+", "http://<stub>", t)
    return re.sub(r"token: \S+", "token: <secret>", t)


def norm_bytes(b):
    if len(b) <= 2048:
        return {"hex": b.hex()}
    return {"len": len(b), "sha256": hashlib.sha256(b).hexdigest(), "head": b[:48].hex()}


def norm_resp(r, label, body=True, cache=False):
    out = {"status": r.status, "ctype": r.ctype, "error": r.error}
    if cache:
        out["cache-control"] = r.headers.get("cache-control")
    if not body or r.error:
        return out
    if r.ctype == "application/json":
        try:
            out["body"] = norm(json.loads(r.body), label)
            return out
        except ValueError:
            pass
    if r.ctype.startswith("text/"):
        out["body"] = norm_text(r.body.decode("utf-8", "replace"), label)
    else:
        out["body"] = norm_bytes(r.body)
    return out


def canon(obj):
    return json.dumps(obj, indent=1, sort_keys=True, ensure_ascii=False)


# --- servers ----------------------------------------------------------------------------------


def base_env(db, port):
    drop = ("PRINT", "SMTP_", "PUBLIC_URL", "PYTHONPATH", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
            "NO_PROXY")
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(drop)}
    env.update(PRINTAPI_TOKEN=TOKEN, PRINTAPI_SIGNUP="open", PRINTAPI_PLANS=json.dumps(PLANS),
               PRINTAPI_BILLING_SECRET=BILLING_SECRET, PUBLIC_URL=PUBLIC_URL, PRINT_DB=str(db),
               PRINT_PORT=str(port), PYTHONUTF8="1", PYTHONIOENCODING="utf-8",
               NO_PROXY="127.0.0.1,localhost",
               # v3 hardens two v2 defaults; restore them so the scenario compares like with like
               # (Python ignores both). The stub URLs live on 127.0.0.1, which v3 refuses when
               # signup is open, and the scenario enrolls agents with self-chosen keys.
               PRINTAPI_PRIVATE_URLS="allow", PRINTAPI_OPEN_ENROLLMENT="1")
    return env


class Server:
    def __init__(self, label, cmd, cwd, workdir, db=None):
        self.label, self.cmd, self.cwd = label, cmd, str(cwd)
        self.dir = Path(workdir)
        self.db = Path(db) if db else self.dir / f"{label}.db"
        self.proc = None
        self.port = None
        self.err_path = self.dir / f"{label}.stderr"
        self.out_path = self.dir / f"{label}.stdout"
        self.generation = 0

    def start(self):
        self.port = free_port()
        self.generation += 1
        err = open(self.err_path, "ab")
        out = open(self.out_path, "ab")
        self.proc = subprocess.Popen(self.cmd, cwd=self.cwd, env=base_env(self.db, self.port),
                                     stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        err.close()
        out.close()
        deadline = time.time() + 30
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"server {self.label} exited early ({self.proc.returncode}):\n"
                                   + self.stderr()[-2000:])
            if http_call(self.port, "GET", "/health", timeout=2).status == 200:
                return self
            time.sleep(0.1)
        raise RuntimeError(f"server {self.label} did not answer /health within 30 s")

    def stop(self):
        p, self.proc = self.proc, None
        if p is None:
            return
        p.terminate()
        try:
            p.wait(5)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait(5)

    def stderr(self):
        try:
            return self.err_path.read_text("utf-8", "replace")
        except OSError:
            return ""


class Stub:
    """Local HTTP server the scenario points the servers at: payload sources and hook receiver."""

    def __init__(self):
        stub = self
        self.log, self.lock = [], threading.Lock()

        class H(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.0"

            def log_message(self, *a):
                pass

            def _serve(self):
                n = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(n) if n else b""
                with stub.lock:
                    stub.log.append({"method": self.command, "path": self.path,
                                     "ctype": self.headers.get("Content-Type"),
                                     "ua": self.headers.get("User-Agent"), "body": body})
                last = self.path.rsplit("/", 1)[-1]
                code, data, ct = 404, b"missing", "text/plain"
                if self.path.startswith("/hook/"):
                    code, data = 200, b"ok"
                elif self.command == "GET" and last == "raw":
                    code, data, ct = 200, b"\x1b@RAW-FETCHED\n\x00\xff", "application/octet-stream"
                elif self.command == "GET" and last == "pdf":
                    code, data, ct = 200, b"%PDF-1.4\n%stub\n" + bytes(range(200)), "application/pdf"
                elif self.command == "POST" and last == "render":
                    code, data, ct = 200, b"%PDF-RENDERED:" + body, "application/pdf"
                self.send_response(code)
                self.send_header("Content-Type", ct)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            do_GET = do_POST = _serve

        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.port = self.httpd.server_address[1]
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def stop(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def entries(self, prefix):
        with self.lock:
            return [e for e in self.log if e["path"].startswith(prefix)]


# --- a running server + its scenario state ------------------------------------------------------


class Run:
    """The scenario's handle on one server: issues calls and records every step."""

    def __init__(self, server, stub):
        self.server, self.stub = server, stub
        self.label, self.token = server.label, TOKEN
        self.steps, self._seen = [], {}
        self.t0 = time.time()

    # urls the servers are pointed at (the label keeps the two servers' logs apart)
    def url(self, name):
        return f"http://127.0.0.1:{self.stub.port}/{self.label}/{name}"

    def hook(self, name):
        return f"http://127.0.0.1:{self.stub.port}/hook/{self.label}/{name}"

    def _name(self, name):
        n = self._seen.get(name, 0) + 1
        self._seen[name] = n
        return name if n == 1 else f"{name} #{n}"

    def record(self, name, data, status=None):
        self.steps.append({"name": self._name(name), "req": "-", "crash_ok": False,
                           "data": {"status": status, "value": data}})

    def call(self, name, method, path, *, js=None, raw=None, auth=None, headers=None, cl=None,
             crash_ok=False, body=True, cache=False, timeout=45):
        h = dict(headers or {})
        if js is not None:
            raw = json.dumps(js).encode()
            h.setdefault("Content-Type", "application/json")
        a = auth_header(auth)
        if a is not None:
            h["Authorization"] = a
        r = http_call(self.server.port, method, path, h, raw, cl, timeout)
        self.steps.append({"name": self._name(name), "req": f"{method} {path}",
                           "crash_ok": crash_ok, "bytes": r.body,
                           "data": norm_resp(r, self.label, body, cache)})
        return r

    def mail_token(self, n):
        """The n-th reset token (1-based) the server wrote to stderr; waits a few seconds."""
        deadline = time.time() + 8
        while time.time() < deadline:
            toks = re.findall(r"token: (\S+)", self.server.stderr())
            if len(toks) >= n:
                return toks[n - 1]
            time.sleep(0.1)
        return None

    def mail_text(self, n):
        blocks = re.findall(r"\[mail\] SMTP_HOST.*?ignore this message\.\n", self.server.stderr(),
                            re.S)
        return norm_text(blocks[n - 1], self.label) if len(blocks) >= n else None


# --- the scenario ------------------------------------------------------------------------------

SHOPIFY = {
    "id": 820982911946154508, "order_number": 1001, "name": "#1001",
    "created_at": "2026-07-25T10:22:51-04:00", "currency": "EUR",
    "subtotal_price": "29.70", "total_tax": "0.00", "total_price": "34.60",
    "note": "leave at the door", "email": "jane@example.com",
    "customer": {"first_name": "Jane", "last_name": "Doe"},
    "shipping_address": {"name": "Jane Doe", "address1": "Musterweg 1", "address2": "",
                         "zip": "12345", "city": "Berlin", "province": "Berlin",
                         "country": "Germany", "phone": "+49 30 123456"},
    "shipping_lines": [{"title": "Standard", "price": "4.90"}],
    "line_items": [{"quantity": 2, "sku": "A-1", "title": "Widget (x)", "variant_title": "blue",
                    "price": "9.90"},
                   {"quantity": 1, "sku": "B-2", "title": "Gadget", "variant_title": "Default Title",
                    "price": "9.90"}],
}
WOO = {
    "id": 727, "number": "727", "date_created": "2026-07-25T10:22:51", "currency": "EUR",
    "total": "34.60", "shipping_total": "4.90", "total_tax": "0.00", "discount_total": "0.00",
    "customer_note": "Bitte klingeln", "billing": {"email": "max@example.com", "phone": "+49 89 1"},
    "shipping": {"first_name": "Max", "last_name": "Müller", "address_1": "Hauptstr. 5",
                 "address_2": "", "postcode": "80331", "city": "München", "state": "BY",
                 "country": "DE"},
    "line_items": [{"name": "Gizmo", "quantity": 2, "sku": "G-1", "total": "19.80"},
                   {"name": "Gadget", "quantity": 1, "sku": "B-2", "total": "9.90"}],
}
ORDER = {"number": "1001", "date": "2026-07-25", "shop": "Acme", "customer": "Jane Doe",
         "email": "jane@example.com", "phone": "+49 1", "note": "ring twice",
         "address": ["Musterweg 1", "12345 Berlin"],
         "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80"},
                   {"qty": 1, "sku": "B-2", "name": "Gadget with a rather long name that must "
                                                     "be cut off by the column", "total": "9.90"}],
         "totals": [["Subtotal", "29.70"], ["Total", "34.60"]]}
RAW = b"^XA^FO50,50^FDHello^FS^XZ"
PDF = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n"
MAC = "00:11:62:0F:97:1A"


def scenario(S):
    C, T = S.call, S.token
    oneoff = {}

    def drain(tag, key, fail_ids=(), keep=()):
        """Claim every queued job of this org through the agent API, fetch each payload and
        report it. Counts first (GET /jobs), so it never waits on the 25 s long-poll."""
        lst = C(f"{tag}: list queued", "GET", "/jobs", auth=key)
        n = len([j for j in (g(lst, "jobs") or []) if j.get("state") == "queued"])
        for i in range(n):
            c = C(f"{tag}: claim {i + 1}/{n}", "GET", "/agent/jobs", auth=key)
            jid = g(c, "job_id")
            if jid is None:
                return
            C(f"{tag}: payload of job {jid}", "GET", f"/agent/jobs/{jid}/payload", auth=key)
            if jid in keep:
                continue
            if jid in fail_ids:
                C(f"{tag}: result job {jid} (error)", "POST", f"/agent/jobs/{jid}/result",
                  js={"ok": False, "error": "boom"}, auth=key)
            else:
                C(f"{tag}: result job {jid}", "POST", f"/agent/jobs/{jid}/result",
                  js={"ok": True}, auth=key)

    # ===== 1. meta + auth failures ============================================================
    C("meta: health", "GET", "/health")
    C("meta: HEAD health", "HEAD", "/health")
    C("meta: HEAD unknown", "HEAD", "/nope")
    for m in ("GET", "POST", "PUT", "DELETE"):
        C(f"meta: unknown path {m}", m, "/no/such/route", auth=T)
    C("meta: unknown path unauthenticated", "GET", "/no/such/route")
    get_paths = ["/jobs", "/jobs/1", "/printers", "/computers", "/metrics", "/me", "/plans",
                 "/apikeys", "/users", "/orgs", "/orgs/1", "/agent/jobs", "/agent/jobs/1/payload"]
    for p in get_paths:
        C(f"auth: GET {p} without credential", "GET", p)
        C(f"auth: GET {p} wrong bearer", "GET", p, auth="wrong-token")
    for m, p in [("POST", "/jobs"), ("POST", "/orders"), ("POST", "/apikeys"), ("POST", "/orgs"),
                 ("POST", "/users"), ("POST", "/logout"), ("POST", "/orgs/1/users"),
                 ("POST", "/agent/jobs/1/result"), ("PUT", "/orgs/1"), ("PUT", "/me/password"),
                 ("DELETE", "/jobs/1"), ("DELETE", "/apikeys/1"), ("DELETE", "/users/1"),
                 ("DELETE", "/orgs/1")]:
        C(f"auth: {m} {p} wrong bearer", m, p, js={}, auth="wrong-token")
    for label, hv in [("garbage scheme", "Zorp abc"), ("bearer without space", "Bearer"),
                      ("lowercase bearer", "bearer " + T), ("double space", "Bearer  " + T),
                      ("trailing space", "Bearer " + T + " "), ("empty header", ""),
                      ("basic garbage", "Basic !!!"), ("basic wrong key", "Basic " + b64(b"nope:"))]:
        C(f"auth: header {label}", "GET", "/jobs", auth=("raw", hv))
    C("auth: root token accepted", "GET", "/jobs", auth=T)
    C("auth: root /me", "GET", "/me", auth=T)

    # ===== 2. orgs (root) ======================================================================
    C("orgs: list initial", "GET", "/orgs", auth=T)
    C("orgs: create without name", "POST", "/orgs", js={}, auth=T)
    C("orgs: create blank name", "POST", "/orgs", js={"name": "   "}, auth=T)
    C("orgs: create bad json", "POST", "/orgs", raw=b"{", auth=T)
    C("orgs: create non-string name", "POST", "/orgs", js={"name": 5}, auth=T, crash_ok=True)
    C("orgs: create non-object body", "POST", "/orgs", raw=b"[1]", auth=T, crash_ok=True)
    acme = g(C("orgs: create Acme", "POST", "/orgs", js={"name": "  Acme  "}, auth=T), "id")
    beta = g(C("orgs: create Beta", "POST", "/orgs", js={"name": "Beta"}, auth=T), "id")
    scrap = g(C("orgs: create Scrap", "POST", "/orgs", js={"name": "Ünïcode Scrap"}, auth=T), "id")
    C("orgs: list", "GET", "/orgs", auth=T)
    C("orgs: get acme (root)", "GET", f"/orgs/{acme}", auth=T)
    C("orgs: get default (root)", "GET", "/orgs/1", auth=T)
    C("orgs: get unknown", "GET", "/orgs/999", auth=T)
    C("orgs: get non-numeric", "GET", "/orgs/abc", auth=T)
    O = f"/orgs/{acme}"
    C("orgs: put nothing", "PUT", O, js={}, auth=T)
    C("orgs: put bad json", "PUT", O, raw=b"nope", auth=T)
    C("orgs: put unknown org empty", "PUT", "/orgs/999", js={}, auth=T)
    C("orgs: put unknown org event_url", "PUT", "/orgs/999", js={"event_url": None}, auth=T)
    C("orgs: put unknown org quota", "PUT", "/orgs/999", js={"job_quota": 1}, auth=T)
    C("orgs: put unknown org plan", "PUT", "/orgs/999", js={"plan": "pro"}, auth=T)
    C("orgs: put unknown org secret", "PUT", "/orgs/999", js={"shopify_secret": "x"}, auth=T)
    C("orgs: put event_url ftp", "PUT", O, js={"event_url": "ftp://x"}, auth=T)
    C("orgs: put event_url number", "PUT", O, js={"event_url": 5}, auth=T)
    C("orgs: put event_url set", "PUT", O, js={"event_url": S.hook("event")}, auth=T)
    C("orgs: put event_url clear null", "PUT", O, js={"event_url": None}, auth=T)
    C("orgs: put event_url clear empty", "PUT", O, js={"event_url": ""}, auth=T)
    C("orgs: put quota negative", "PUT", O, js={"job_quota": -1}, auth=T)
    C("orgs: put quota bool", "PUT", O, js={"job_quota": True}, auth=T)
    C("orgs: put quota string", "PUT", O, js={"job_quota": "5"}, auth=T)
    C("orgs: put quota float", "PUT", O, js={"job_quota": 5.0}, auth=T)
    C("orgs: put quota 5", "PUT", O, js={"job_quota": 5}, auth=T)
    C("orgs: put quota null", "PUT", O, js={"job_quota": None}, auth=T)
    C("orgs: put plan unknown", "PUT", O, js={"plan": "gold"}, auth=T)
    C("orgs: put plan null", "PUT", O, js={"plan": None}, auth=T)
    C("orgs: put plan free", "PUT", O, js={"plan": "free"}, auth=T)
    C("orgs: put plan pro", "PUT", O, js={"plan": " pro "}, auth=T)
    C("orgs: put secret blank", "PUT", O, js={"shopify_secret": "  "}, auth=T)
    C("orgs: put secret number", "PUT", O, js={"shopify_secret": 5}, auth=T)
    C("orgs: put secret set", "PUT", O, js={"shopify_secret": "s3cret"}, auth=T)
    C("orgs: put secret clear", "PUT", O, js={"shopify_secret": None}, auth=T)
    C("orgs: put several fields", "PUT", O, js={"event_url": S.hook("event"),
                                                "shopify_secret": "s3cret"}, auth=T)
    C("orgs: put plan overrides quota", "PUT", O, js={"plan": "free", "job_quota": 7}, auth=T)
    C("orgs: put partial then error", "PUT", O,
      js={"event_url": S.hook("event"), "job_quota": -3}, auth=T)
    C("orgs: put back to pro", "PUT", O, js={"plan": "pro"}, auth=T)
    C("orgs: get acme after puts", "GET", O, auth=T)
    C("orgs: delete default refused", "DELETE", "/orgs/1", auth=T)
    C("orgs: delete scrap", "DELETE", f"/orgs/{scrap}", auth=T)
    C("orgs: delete scrap again", "DELETE", f"/orgs/{scrap}", auth=T)
    C("orgs: delete non-numeric", "DELETE", "/orgs/abc", auth=T)

    # ===== 3. api keys (root) ===================================================================
    C("keys: list empty", "GET", "/apikeys", auth=T)
    kr = C("keys: issue acme", "POST", "/apikeys", js={"label": "acme-key", "org_id": acme}, auth=T)
    K = g(kr, "key")
    K2 = g(C("keys: issue acme second", "POST", "/apikeys",
             js={"label": "acme-second", "org_id": acme}, auth=T), "key")
    KB = g(C("keys: issue beta", "POST", "/apikeys", js={"label": "beta", "org_id": beta}, auth=T),
           "key")
    KD = g(C("keys: issue default org", "POST", "/apikeys", js={}, auth=T), "key")
    C("keys: issue blank label", "POST", "/apikeys", js={"label": "   ", "org_id": acme}, auth=T)
    C("keys: issue string org id", "POST", "/apikeys", js={"label": "str", "org_id": str(acme)},
      auth=T)
    C("keys: issue zero org id", "POST", "/apikeys", js={"label": "zero", "org_id": 0}, auth=T)
    C("keys: issue unknown org", "POST", "/apikeys", js={"org_id": 999}, auth=T)
    C("keys: issue bad json", "POST", "/apikeys", raw=b"[", auth=T)
    C("keys: issue label number", "POST", "/apikeys", js={"label": 5}, auth=T, crash_ok=True)
    C("keys: issue with a machine key", "POST", "/apikeys", js={}, auth=K)
    C("keys: list with a machine key", "GET", "/apikeys", auth=K)
    C("keys: list", "GET", "/apikeys", auth=T)
    tmp = g(C("keys: issue temp", "POST", "/apikeys", js={"label": "temp", "org_id": acme},
              auth=T), "id")
    C("keys: temp key works", "GET", "/jobs", auth=g(C("keys: reissue temp", "POST", "/apikeys",
                                                       js={"label": "temp2", "org_id": acme},
                                                       auth=T), "key"))
    C("keys: revoke temp", "DELETE", f"/apikeys/{tmp}", auth=T)
    C("keys: revoke temp again", "DELETE", f"/apikeys/{tmp}", auth=T)
    C("keys: revoke unknown", "DELETE", "/apikeys/999", auth=T)
    C("keys: revoke non-numeric", "DELETE", "/apikeys/abc", auth=T)
    C("keys: revoke with a machine key", "DELETE", "/apikeys/1", auth=K)
    C("keys: machine key /me", "GET", "/me", auth=K)
    C("keys: machine key sees only its org", "GET", "/jobs", auth=KB)

    # ===== 4. users, login, sessions ============================================================
    OU = f"/orgs/{acme}/users"
    pw = "correct horse"
    C("users: seed without root", "POST", OU, js={"email": "a@b.test", "password": pw}, auth=K)
    C("users: seed bad json", "POST", OU, raw=b"{", auth=T)
    C("users: seed unknown org", "POST", "/orgs/999/users",
      js={"email": "a@b.test", "password": pw}, auth=T)
    C("users: seed bad email", "POST", OU, js={"email": "nobody", "password": pw}, auth=T)
    C("users: seed missing email", "POST", OU, js={"password": pw}, auth=T)
    C("users: seed short password", "POST", OU, js={"email": "a@b.test", "password": "short"},
      auth=T)
    C("users: seed non-string password", "POST", OU, js={"email": "a@b.test", "password": 12345678},
      auth=T)
    C("users: seed password too long", "POST", OU,
      js={"email": "a@b.test", "password": "x" * 1025}, auth=T)
    C("users: seed owner", "POST", OU, js={"email": "  Owner@Acme.TEST ", "password": pw}, auth=T)
    C("users: seed duplicate", "POST", OU, js={"email": "owner@acme.test", "password": pw}, auth=T)
    C("users: seed beta owner", "POST", f"/orgs/{beta}/users",
      js={"email": "boss@beta.test", "password": "beta-password"}, auth=T)
    C("users: root list", "GET", "/users", auth=T)
    C("users: root create needs an org", "POST", "/users",
      js={"email": "x@y.test", "password": pw}, auth=T)
    C("users: machine key list", "GET", "/users", auth=K)
    C("login: bad json", "POST", "/login", raw=b"{")
    C("login: non-object body", "POST", "/login", raw=b"[]", crash_ok=True)
    C("login: empty body", "POST", "/login")
    C("login: empty object", "POST", "/login", js={})
    C("login: unknown user", "POST", "/login", js={"email": "ghost@x.test", "password": pw})
    C("login: wrong password", "POST", "/login", js={"email": "owner@acme.test", "password": "nope"})
    C("login: password not a string", "POST", "/login",
      js={"email": "owner@acme.test", "password": 5}, crash_ok=True)
    C("login: password too long", "POST", "/login",
      js={"email": "owner@acme.test", "password": "x" * 1025})
    lr = C("login: owner", "POST", "/login", js={"email": "OWNER@acme.test", "password": pw})
    SA = g(lr, "token")
    SB = g(C("login: beta boss", "POST", "/login",
             js={"email": "boss@beta.test", "password": "beta-password"}), "token")
    C("session: me", "GET", "/me", auth=SA)
    C("session: plans", "GET", "/plans", auth=SA)
    C("session: list users", "GET", "/users", auth=SA)
    C("session: create colleague", "POST", "/users",
      js={"email": "Second@Acme.test", "password": "second-pass"}, auth=SA)
    C("session: create duplicate", "POST", "/users",
      js={"email": "second@acme.test", "password": "second-pass"}, auth=SA)
    C("session: create bad json", "POST", "/users", raw=b"x", auth=SA)
    C("session: create bad email", "POST", "/users", js={"email": "x", "password": pw}, auth=SA)
    C("session: list users after", "GET", "/users", auth=SA)
    C("session: delete self", "DELETE", "/users/2", auth=SA)
    C("session: delete colleague", "DELETE", "/users/4", auth=SA)
    C("session: delete colleague again", "DELETE", "/users/4", auth=SA)
    C("session: delete legacy user", "DELETE", "/users/1", auth=SA)
    C("session: delete foreign user", "DELETE", "/users/3", auth=SA)
    C("session: delete non-numeric", "DELETE", "/users/abc", auth=SA)
    C("session: root deletes last user", "DELETE", "/users/2", auth=T)
    C("session: machine key deletes user", "DELETE", "/users/3", auth=K)
    C("session: keys list scoped", "GET", "/apikeys", auth=SA)
    C("session: key for own org", "POST", "/apikeys", js={"label": "via-session"}, auth=SA)
    C("session: key for foreign org", "POST", "/apikeys", js={"org_id": beta}, auth=SA)
    C("session: key for unknown org", "POST", "/apikeys", js={"org_id": 999}, auth=SA)
    bk = C("session: beta key", "POST", "/apikeys", js={"label": "b2"}, auth=SB)
    C("session: revoke foreign key", "DELETE", f"/apikeys/{g(bk, 'id')}", auth=SA)
    C("session: revoke own key", "DELETE", f"/apikeys/{g(bk, 'id')}", auth=SB)
    C("session: org list is root only", "GET", "/orgs", auth=SA)
    C("session: own org", "GET", O, auth=SA)
    C("session: foreign org", "GET", f"/orgs/{beta}", auth=SA)
    C("session: org event_url", "PUT", O, js={"event_url": S.hook("event")}, auth=SA)
    C("session: org event_url bad", "PUT", O, js={"event_url": "gopher://x"}, auth=SA)
    C("session: org quota refused", "PUT", O, js={"job_quota": 99}, auth=SA)
    C("session: org plan refused", "PUT", O, js={"plan": "pro"}, auth=SA)
    C("session: org secret", "PUT", O, js={"shopify_secret": "s3cret"}, auth=SA)
    C("session: foreign org put", "PUT", f"/orgs/{beta}", js={"event_url": None}, auth=SA)
    C("session: org put nothing", "PUT", O, js={"bogus": 1}, auth=SA)
    C("session: org put machine key", "PUT", O, js={"event_url": None}, auth=K)
    C("session: org delete refused", "DELETE", O, auth=SA)
    C("session: org create refused", "POST", "/orgs", js={"name": "x"}, auth=SA)
    C("session: org user seeding refused", "POST", OU,
      js={"email": "z@acme.test", "password": pw}, auth=SA)
    # password change
    C("password: change bad json", "PUT", "/me/password", raw=b"{", auth=SA)
    C("password: change by root", "PUT", "/me/password", js={"current": pw, "new": "x" * 9},
      auth=T)
    C("password: change by key", "PUT", "/me/password", js={"current": pw, "new": "x" * 9}, auth=K)
    C("password: wrong current", "PUT", "/me/password", js={"current": "nope", "new": "newpass99"},
      auth=SA)
    C("password: new too short", "PUT", "/me/password", js={"current": pw, "new": "short"},
      auth=SA)
    C("password: new missing", "PUT", "/me/password", js={"current": pw}, auth=SA)
    C("password: changed", "PUT", "/me/password", js={"current": pw, "new": "newpass99"}, auth=SA)
    C("password: old session is dead", "GET", "/me", auth=SA)
    C("password: old password refused", "POST", "/login",
      js={"email": "owner@acme.test", "password": pw})
    SA = g(C("password: login with the new one", "POST", "/login",
             js={"email": "owner@acme.test", "password": "newpass99"}), "token")
    C("logout: by session", "POST", "/logout", auth=SA)
    C("logout: again", "POST", "/logout", auth=SA)
    C("logout: by root", "POST", "/logout", auth=T)
    C("logout: by key", "POST", "/logout", auth=K)
    SA = g(C("logout: login again", "POST", "/login",
             js={"email": "owner@acme.test", "password": "newpass99"}), "token")
    # password reset
    C("reset: bad json", "POST", "/password/reset", raw=b"{")
    C("reset: unknown address", "POST", "/password/reset", js={"email": "ghost@x.test"})
    C("reset: empty body", "POST", "/password/reset")
    C("reset: request", "POST", "/password/reset", js={"email": " Owner@Acme.test "})
    tok1 = S.mail_token(1)
    S.record("reset: mail text", S.mail_text(1))
    S.record("reset: exactly one mail went out", len(re.findall(r"token: ", S.server.stderr())))
    C("reset: confirm bad json", "POST", "/password/reset/confirm", raw=b"{")
    C("reset: confirm unknown token", "POST", "/password/reset/confirm",
      js={"token": "sess_nope", "password": "another-pass"})
    C("reset: confirm missing token", "POST", "/password/reset/confirm",
      js={"password": "another-pass"})
    C("reset: confirm short password", "POST", "/password/reset/confirm",
      js={"token": tok1, "password": "short"})
    C("reset: confirm", "POST", "/password/reset/confirm",
      js={"token": tok1, "password": "another-pass"})
    C("reset: confirm twice", "POST", "/password/reset/confirm",
      js={"token": tok1, "password": "another-pass"})
    C("reset: sessions died", "GET", "/me", auth=SA)
    C("reset: old password refused", "POST", "/login",
      js={"email": "owner@acme.test", "password": "newpass99"})
    SA = g(C("reset: new password works", "POST", "/login",
             js={"email": "owner@acme.test", "password": "another-pass"}), "token")
    C("reset: second request supersedes", "POST", "/password/reset",
      js={"email": "owner@acme.test"})
    C("reset: second request again", "POST", "/password/reset", js={"email": "owner@acme.test"})
    t2, t3 = S.mail_token(2), S.mail_token(3)
    C("reset: superseded token refused", "POST", "/password/reset/confirm",
      js={"token": t2, "password": "third-password"})
    C("reset: latest token ok", "POST", "/password/reset/confirm",
      js={"token": t3, "password": "pw-after-reset"})
    SA = g(C("reset: login after last reset", "POST", "/login",
             js={"email": "owner@acme.test", "password": "pw-after-reset"}), "token")
    for i in range(1, 13):
        C(f"reset: throttle attempt {i}", "POST", "/password/reset", js={"email": "spam@x.test"})
    for i in range(1, 13):
        C(f"login: throttle attempt {i}", "POST", "/login",
          js={"email": "brute@x.test", "password": "guess-guess"})

    # ===== 5. agents ===========================================================================
    printers = [{"name": "Label", "can_pdf": False},
                {"name": "Laser", "can_pdf": True,
                 "capabilities": {"duplex": True, "color": True, "bins": ["Tray 1", "Tray 2"],
                                  "papers": ["A4", "Letter"]}}]
    C("agent: register bad json", "POST", "/agent/register", raw=b"{", auth=K)
    C("agent: register printer without name", "POST", "/agent/register",
      js={"name": "acme-agent", "printers": [{"can_pdf": True}]}, auth=K)
    C("agent: register printers not a list", "POST", "/agent/register",
      js={"name": "acme-agent", "printers": 5}, auth=K, crash_ok=True)
    C("agent: register non-object body", "POST", "/agent/register", raw=b"[]", auth=K,
      crash_ok=True)
    C("agent: register acme", "POST", "/agent/register",
      js={"name": "acme-agent", "printers": printers}, auth=K)
    C("agent: register acme again", "POST", "/agent/register",
      js={"name": "acme-agent", "printers": printers}, auth=K)
    C("agent: register wrong key for the name", "POST", "/agent/register",
      js={"name": "acme-agent", "printers": []}, auth=K2)
    C("agent: register key already in use", "POST", "/agent/register",
      js={"name": "acme-agent-2", "printers": []}, auth=K)
    C("agent: register null name", "POST", "/agent/register", js={"printers": []}, auth=K2)
    C("agent: register beta", "POST", "/agent/register",
      js={"name": "beta-agent", "printers": [{"name": "BetaPDF", "can_pdf": True}]}, auth=KB)
    C("agent: register default org", "POST", "/agent/register",
      js={"name": "default-agent", "printers": [{"name": "DefP", "can_pdf": True}]}, auth=KD)
    C("agent: poll without key", "GET", "/agent/jobs")
    C("agent: poll wrong key", "GET", "/agent/jobs", auth="wrong")
    C("agent: result without key", "POST", "/agent/jobs/1/result", js={"ok": True})
    C("agent: payload wrong key", "GET", "/agent/jobs/1/payload", auth="wrong")
    C("printers: acme", "GET", "/printers", auth=K)
    C("printers: beta", "GET", "/printers", auth=KB)
    C("printers: root", "GET", "/printers", auth=T)
    C("computers: acme", "GET", "/computers", auth=K)
    C("computers: root", "GET", "/computers", auth=T)
    LBL, LASER = 1, 2

    # ===== 6. submitting jobs ===================================================================
    def post(name, body, key=K, **kw):
        return C(name, "POST", "/jobs", js=body, auth=key, **kw)

    ok = {}
    ok["raw"] = post("jobs: raw_base64", {"type": "raw_base64", "printer_id": LBL,
                                          "content": b64(RAW), "title": "label 1"})
    ok["pdf"] = post("jobs: pdf_base64 with options",
                     {"type": "pdf_base64", "printer_id": LASER, "content": b64(PDF), "copies": 2,
                      "title": "Rechnung ü", "options": {"duplex": "long-edge", "paper": "A4",
                                                          "bin": "Tray 1", "color": True,
                                                          "pages": "1-3,5"}})
    ok["raw_uri"] = post("jobs: raw_uri", {"type": "raw_uri", "printer_id": LBL, "url": S.url("raw")})
    ok["pdf_uri"] = post("jobs: pdf_uri", {"type": "pdf_uri", "printer_id": LASER,
                                           "url": S.url("pdf")})
    ok["raw_post"] = post("jobs: raw_uri_post", {"type": "raw_uri_post", "printer_id": LBL,
                                                 "url": S.url("render"), "json": {"a": 1, "b": [2]}})
    ok["pdf_post"] = post("jobs: pdf_uri_post", {"type": "pdf_uri_post", "printer_id": LASER,
                                                 "url": S.url("render"), "json": {"template": "x"}})
    ok["pdf_post_nojson"] = post("jobs: pdf_uri_post without json",
                                 {"type": "pdf_uri_post", "printer_id": LASER, "url": S.url("render")})
    ok["idem"] = post("jobs: idempotency first", {"type": "raw_base64", "printer_id": LBL,
                                                  "content": b64(RAW), "idempotency_key": "idem-1"})
    post("jobs: idempotency resubmit", {"type": "raw_base64", "printer_id": LBL,
                                        "content": b64(b"different"), "idempotency_key": "idem-1"})
    post("jobs: idempotency resubmit trimmed", {"type": "raw_base64", "printer_id": LBL,
                                                "content": b64(RAW), "idempotency_key": "  idem-1 "})
    post("jobs: idempotency blank key", {"type": "raw_base64", "printer_id": LBL,
                                         "content": b64(RAW), "idempotency_key": "   "})
    post("jobs: idempotency other org same key", {"type": "pdf_base64", "printer_id": 3,
                                                  "content": b64(PDF), "idempotency_key": "idem-1"},
         key=KB)
    post("jobs: copies 100 expire_after 3600", {"type": "raw_base64", "printer_id": LBL,
                                                "content": b64(RAW), "copies": 100,
                                                "expire_after": 3600, "idempotency_key": ""})
    post("jobs: empty options on raw", {"type": "raw_base64", "printer_id": LBL,
                                        "content": b64(RAW), "options": {}})
    post("jobs: options null", {"type": "raw_base64", "printer_id": LBL, "content": b64(RAW),
                                "options": None, "copies": None})
    post("jobs: printer id as string", {"type": "raw_base64", "printer_id": "1",
                                        "content": b64(RAW)})
    post("jobs: printer id as bool", {"type": "raw_base64", "printer_id": True,
                                      "content": b64(RAW)})
    post("jobs: printer id as float", {"type": "raw_base64", "printer_id": 1.0,
                                       "content": b64(RAW)})
    cancel_me = g(post("jobs: with callback (to cancel)",
                       {"type": "pdf_base64", "printer_id": LASER, "content": b64(PDF),
                        "callback_url": S.hook("job"), "title": "hook cancelled"}), "job_id")
    hook_ok = g(post("jobs: with callback (to finish)",
                     {"type": "raw_base64", "printer_id": LBL, "content": b64(RAW),
                      "callback_url": S.hook("job"), "title": "hook done"}), "job_id")
    hook_fail = g(post("jobs: with callback (to fail)",
                       {"type": "raw_base64", "printer_id": LBL, "content": b64(RAW),
                        "callback_url": S.hook("job"), "title": "hook failed"}), "job_id")
    # validation errors
    good = {"type": "pdf_base64", "printer_id": LASER, "content": b64(PDF)}
    bad = {
        "copies zero": dict(good, copies=0), "copies 101": dict(good, copies=101),
        "copies string": dict(good, copies="2"), "copies bool": dict(good, copies=True),
        "copies float": dict(good, copies=1.5), "copies negative": dict(good, copies=-1),
        "copies beats unknown type": {"type": "nope", "printer_id": LASER, "copies": 0},
        "options list": dict(good, options=[1]), "options string": dict(good, options="duplex"),
        "options unknown key": dict(good, options={"zzz": 1, "aaa": 2}),
        "options on raw": {"type": "raw_base64", "printer_id": LBL, "content": b64(RAW),
                           "options": {"paper": "A4"}},
        "duplex bad": dict(good, options={"duplex": "both"}),
        "paper with comma": dict(good, options={"paper": "A4,fit"}),
        "paper number": dict(good, options={"paper": 4}),
        "bin too long": dict(good, options={"bin": "x" * 65}),
        "color string": dict(good, options={"color": "yes"}),
        "pages junk": dict(good, options={"pages": "a-b"}),
        "pages number": dict(good, options={"pages": 3}),
        "expire_after zero": dict(good, expire_after=0),
        "expire_after too big": dict(good, expire_after=30 * 24 * 3600 + 1),
        "expire_after string": dict(good, expire_after="5"),
        "expire_after bool": dict(good, expire_after=True),
        "idempotency number": dict(good, idempotency_key=123),
        "idempotency too long": dict(good, idempotency_key="k" * 129),
        "callback ftp": dict(good, callback_url="ftp://x/y"),
        "callback number": dict(good, callback_url=5),
        "callback list": dict(good, callback_url=["http://x"]),
        "callback no scheme": dict(good, callback_url="example.com/hook"),
        "unknown type": {"type": "foo", "printer_id": LBL},
        "missing type": {"printer_id": LBL, "content": b64(RAW)},
        "type number": {"type": 5, "printer_id": LBL},
        "content missing": {"type": "raw_base64", "printer_id": LBL},
        "content empty": {"type": "raw_base64", "printer_id": LBL, "content": ""},
        "content not base64": {"type": "raw_base64", "printer_id": LBL, "content": "###"},
        "content bad padding": {"type": "raw_base64", "printer_id": LBL, "content": "abc"},
        "url missing": {"type": "raw_uri", "printer_id": LBL},
        "url scheme": {"type": "pdf_uri", "printer_id": LASER, "url": "ftp://x/f"},
        "url scheme post": {"type": "raw_uri_post", "printer_id": LBL, "url": "file:///etc/passwd"},
        "url 404": {"type": "raw_uri", "printer_id": LBL, "url": S.url("missing")},
        "post url 404": {"type": "pdf_uri_post", "printer_id": LASER, "url": S.url("missing"),
                         "json": {}},
        "printer unknown": dict(good, printer_id=99),
        "printer text": dict(good, printer_id="abc"),
        "printer foreign": dict(good, printer_id=3),
        "printer null": {k: v for k, v in good.items() if k != "printer_id"} | {"printer_id": None},
        "printer missing": {"type": "raw_base64", "content": b64(RAW)},
        "printer negative": dict(good, printer_id=-1),
        "printer huge": dict(good, printer_id=2 ** 70),
        "printer list": dict(good, printer_id=[1]),
        "printer object": dict(good, printer_id={"a": 1}),
    }
    crashes = {"printer list", "printer object", "printer huge"}
    for k, body in bad.items():
        post(f"jobs: invalid {k}", body, crash_ok=k in crashes)
    C("jobs: bad json", "POST", "/jobs", raw=b"{", auth=K)
    C("jobs: empty body", "POST", "/jobs", auth=K)
    C("jobs: json null", "POST", "/jobs", raw=b"null", auth=K, crash_ok=True)
    C("jobs: json array", "POST", "/jobs", raw=b"[1,2]", auth=K, crash_ok=True)
    C("jobs: json string", "POST", "/jobs", raw=b'"x"', auth=K, crash_ok=True)
    C("jobs: bad content-length", "POST", "/jobs", auth=K, cl="abc")
    C("jobs: negative content-length", "POST", "/jobs", auth=K, cl="-1")
    C("jobs: oversized content-length", "POST", "/jobs", auth=K, cl=str(32 * 1024 * 1024 + 1))
    C("jobs: invalid utf-8 body", "POST", "/jobs", raw=b'{"type": "\xff\xfe"}', auth=K)
    C("jobs: non-ascii json escape", "POST", "/jobs",
      raw=b'{"type":"raw_base64","printer_id":1,"content":"UkFX","title":"\\u00e4\\ud83d\\ude00"}',
      auth=K)
    C("jobs: root may submit", "POST", "/jobs", js={"type": "raw_base64", "printer_id": LBL,
                                                    "content": b64(RAW), "title": "by root"},
      auth=T)
    C("jobs: foreign printer from beta", "POST", "/jobs", js=dict(good, printer_id=LASER), auth=KB)

    # ===== 7. reading + cancelling ==============================================================
    C("jobs: list acme", "GET", "/jobs", auth=K)
    C("jobs: list beta", "GET", "/jobs", auth=KB)
    C("jobs: list root", "GET", "/jobs", auth=T)
    C("jobs: get 1", "GET", "/jobs/1", auth=K)
    C("jobs: get unknown", "GET", "/jobs/9999", auth=K)
    C("jobs: get non-numeric", "GET", "/jobs/abc", auth=K)
    C("jobs: get foreign", "GET", "/jobs/1", auth=KB)
    C("jobs: get with query string", "GET", "/jobs/1?x=1", auth=K)
    C("jobs: cancel (queued)", "DELETE", f"/jobs/{cancel_me}", auth=K)
    C("jobs: cancel again", "DELETE", f"/jobs/{cancel_me}", auth=K)
    C("jobs: cancel unknown", "DELETE", "/jobs/9999", auth=K)
    C("jobs: cancel foreign", "DELETE", "/jobs/1", auth=KB)
    C("jobs: cancelled state", "GET", f"/jobs/{cancel_me}", auth=K)
    C("metrics: acme", "GET", "/metrics", auth=K)
    C("metrics: root", "GET", "/metrics", auth=T)

    # ===== 8. the agent flow ====================================================================
    c1 = C("agent: first claim", "GET", "/agent/jobs", auth=K)
    j1 = g(c1, "job_id")
    C("agent: payload", "GET", f"/agent/jobs/{j1}/payload", auth=K)
    C("agent: payload foreign agent", "GET", f"/agent/jobs/{j1}/payload", auth=KB)
    C("agent: payload unknown job", "GET", "/agent/jobs/9999/payload", auth=K)
    C("agent: payload non-numeric", "GET", "/agent/jobs/abc/payload", auth=K)
    C("jobs: cancel claimed", "DELETE", f"/jobs/{j1}", auth=K)
    C("agent: result foreign agent", "POST", f"/agent/jobs/{j1}/result", js={"ok": True}, auth=KB)
    C("agent: result bad json", "POST", f"/agent/jobs/{j1}/result", raw=b"{", auth=K)
    C("agent: result ok", "POST", f"/agent/jobs/{j1}/result", js={"ok": True}, auth=K)
    C("agent: result twice", "POST", f"/agent/jobs/{j1}/result", js={"ok": True}, auth=K)
    C("agent: result unknown job", "POST", "/agent/jobs/9999/result", js={"ok": True}, auth=K)
    C("jobs: cancel done", "DELETE", f"/jobs/{j1}", auth=K)
    c2 = C("agent: second claim", "GET", "/agent/jobs", auth=K)
    j2 = g(c2, "job_id")
    C("agent: payload 2", "GET", f"/agent/jobs/{j2}/payload", auth=K)
    C("agent: result failed", "POST", f"/agent/jobs/{j2}/result",
      js={"ok": False, "error": "paper jam"}, auth=K)
    c3 = C("agent: third claim", "GET", "/agent/jobs", auth=K)
    j3 = g(c3, "job_id")
    C("agent: result non-object", "POST", f"/agent/jobs/{j3}/result", raw=b"[1]", auth=K,
      crash_ok=True)
    C("agent: result truthy string", "POST", f"/agent/jobs/{j3}/result", js={"ok": "yes"}, auth=K)
    c4 = C("agent: fourth claim", "GET", "/agent/jobs", auth=K)
    j4 = g(c4, "job_id")
    C("agent: result failed without error", "POST", f"/agent/jobs/{j4}/result", js={}, auth=K)
    drain("flow", K, fail_ids=(hook_fail,))
    for jid in (j1, j2, j3, j4, hook_ok, hook_fail, cancel_me):
        C(f"jobs: final state of {jid}", "GET", f"/jobs/{jid}", auth=K)
    C("jobs: list after flow", "GET", "/jobs", auth=K)
    C("metrics: after flow", "GET", "/metrics", auth=K)
    C("computers: after flow", "GET", "/computers", auth=K)

    # ===== 9. orders ============================================================================
    def order(name, body, key=K, **kw):
        return C(name, "POST", "/orders", js=body, auth=key, **kw)

    order("orders: normalized", {"printer_id": LASER, "order": ORDER})
    order("orders: normalized with title+copies", {"printer_id": LASER, "order": ORDER,
                                                    "title": "Slip!", "copies": 2,
                                                    "callback_url": S.hook("order"),
                                                    "expire_after": 600})
    order("orders: shopify", {"printer_id": LASER, "format": "shopify", "order": SHOPIFY})
    order("orders: woocommerce", {"printer_id": LASER, "format": "woocommerce", "order": WOO})
    order("orders: idempotent", {"printer_id": LASER, "order": ORDER, "idempotency_key": "ord-1"})
    order("orders: idempotent again", {"printer_id": LASER, "order": ORDER, "idempotency_key": "ord-1"})
    order("orders: minimal", {"printer_id": LASER, "order": {"number": "1"}})
    order("orders: odd fields", {"printer_id": LASER, "order": {
        "number": 42, "address": ["a", "", None, "b"] + ["x"] * 20,
        "lines": [{"qty": 1, "name": "A (b) \\ c"}, "junk", {"sku": "z"}, {}],
        "totals": [["T", "1"], ["bad"], ["empty", ""], 5]}})
    order("orders: raw-only printer", {"printer_id": LBL, "order": ORDER})
    order("orders: unknown printer", {"printer_id": 99, "order": ORDER})
    order("orders: printer missing", {"order": ORDER})
    order("orders: foreign printer", {"printer_id": LASER, "order": ORDER}, key=KB)
    order("orders: missing number", {"printer_id": LASER, "order": {"lines": []}})
    order("orders: order missing", {"printer_id": LASER})
    order("orders: order not an object", {"printer_id": LASER, "order": [1]})
    order("orders: address not a list", {"printer_id": LASER, "order": {"number": "1", "address": "x"}})
    order("orders: lines not a list", {"printer_id": LASER, "order": {"number": "1", "lines": {}}})
    order("orders: totals not a list", {"printer_id": LASER, "order": {"number": "1", "totals": 1}})
    order("orders: too many lines", {"printer_id": LASER, "order": {
        "number": "1", "lines": [{"name": "x"}] * 1001}})
    order("orders: unknown format", {"printer_id": LASER, "format": "magento", "order": SHOPIFY})
    order("orders: format with non-object", {"printer_id": LASER, "format": "shopify", "order": 5})
    order("orders: bad copies", {"printer_id": LASER, "order": ORDER, "copies": 0})
    order("orders: bad callback", {"printer_id": LASER, "order": ORDER, "callback_url": "ftp://x"})
    order("orders: bad expire_after", {"printer_id": LASER, "order": ORDER, "expire_after": 0})
    order("orders: bad idempotency", {"printer_id": LASER, "order": ORDER, "idempotency_key": 5})
    order("orders: shopify with odd amounts", {"printer_id": LASER, "format": "shopify", "order": {
        "name": "#9", "line_items": [{"title": "T", "price": "x", "quantity": "two"},
                                     {"title": "U", "price": "1.5", "quantity": None}],
        "shipping_lines": [{"price": "oops"}], "total_price": "0.00"}})
    C("orders: bad json", "POST", "/orders", raw=b"{", auth=K)
    C("orders: non-object body", "POST", "/orders", raw=b"[]", auth=K, crash_ok=True)
    drain("orders", K)

    # ===== 10. shopify webhook ==================================================================
    def shop(name, payload, key=K, printer=LASER, secret="s3cret", extra="", headers=None,
             raw=None, sign=True):
        body = raw if raw is not None else json.dumps(payload).encode()
        h = {"Content-Type": "application/json", "X-Shopify-Shop-Domain": "acme.myshopify.com",
             "X-Shopify-Topic": "orders/create"}
        if sign:
            h["X-Shopify-Hmac-Sha256"] = b64(hmac.new(secret.encode(), body, hashlib.sha256).digest())
        h.update(headers or {})
        q = f"?key={key}" + (f"&printer_id={printer}" if printer is not None else "") + extra
        return C(name, "POST", "/integrations/shopify/orders" + q, raw=body, headers=h)

    sh = shop("shopify: order", SHOPIFY)
    shop("shopify: redelivery", SHOPIFY)
    shop("shopify: order named only", {"name": "#2002", "line_items": []})
    shop("shopify: same name again", {"name": "#2002", "line_items": []})
    shop("shopify: no id or name", {"line_items": [], "order_number": 7})
    shop("shopify: bad signature", SHOPIFY, secret="wrong")
    shop("shopify: missing signature", SHOPIFY, sign=False)
    shop("shopify: signature not base64", SHOPIFY, sign=False, headers={"X-Shopify-Hmac-Sha256": "!!"})
    shop("shopify: unknown key", SHOPIFY, key="nope")
    shop("shopify: root token as key", SHOPIFY, key=T)
    shop("shopify: org without secret", SHOPIFY, key=KB, secret="whatever")
    shop("shopify: missing printer_id", SHOPIFY, printer=None)
    shop("shopify: printer_id not a number", SHOPIFY, printer="abc")
    shop("shopify: raw-only printer", SHOPIFY, printer=LBL)
    shop("shopify: unknown printer", SHOPIFY, printer=99)
    shop("shopify: bad json", None, raw=b"{")
    shop("shopify: empty body", None, raw=b"")
    shop("shopify: json array", None, raw=b"[1]")
    shop("shopify: query printer_id twice", SHOPIFY, extra="&printer_id=99")
    C("shopify: bad content-length", "POST", f"/integrations/shopify/orders?key={K}&printer_id=2",
      cl="abc")
    C("shopify: GET is not a route", "GET", "/integrations/shopify/orders")
    drain("shopify", K)

    # ===== 11. PrintNode compat (HTTP Basic) ====================================================
    BK, BR = ("basic", K), ("basic", T)

    def pn(name, path, key=BK, **kw):
        return C(name, "GET", path, auth=key, **kw)

    pn("pn: whoami", "/whoami")
    pn("pn: whoami root", "/whoami", BR)
    pn("pn: whoami bad key", "/whoami", ("basic", "nope"))
    pn("pn: whoami no key", "/whoami", None)
    pn("pn: whoami bearer is not pn", "/whoami", K)
    pn("pn: computers", "/computers")
    pn("pn: computers root", "/computers", BR)
    pn("pn: computers set", "/computers/1")
    pn("pn: computers list", "/computers/1,2,3")
    pn("pn: computers range", "/computers/1-3")
    pn("pn: computers unknown", "/computers/99")
    pn("pn: computers printers", "/computers/1/printers")
    pn("pn: computers printers set", "/computers/1,2/printers")
    pn("pn: computers states is no route", "/computers/1/states")
    pn("pn: printers", "/printers")
    pn("pn: printers root", "/printers", BR)
    pn("pn: printers set", "/printers/1,2")
    pn("pn: printers range", "/printers/1-2")
    pn("pn: printers spaces", "/printers/1%20,%202")
    pn("pn: printers reversed", "/printers/5-1")
    pn("pn: printers empty element", "/printers/1,,2")
    pn("pn: printers letters", "/printers/a")
    pn("pn: printers dash only", "/printers/-")
    pn("pn: printers huge range", "/printers/1-100000")
    pn("pn: printers exactly 500", "/printers/1-500")
    pn("pn: printers 501", "/printers/1-501")
    pn("pn: printers sub-resource states", "/printers/1/states")
    pn("pn: printers sub-resource printers", "/printers/1/printers")
    pn("pn: printers with query", "/printers?x=1")
    pn("pn: printers bad auth", "/printers", ("basic", "nope"))
    pn("pn: printjobs", "/printjobs")
    pn("pn: printjobs root", "/printjobs", BR)
    pn("pn: printjobs limit 2", "/printjobs?limit=2")
    pn("pn: printjobs limit 0", "/printjobs?limit=0")
    pn("pn: printjobs limit 99999", "/printjobs?limit=99999")
    pn("pn: printjobs limit junk", "/printjobs?limit=abc")
    pn("pn: printjobs limit twice", "/printjobs?limit=1&limit=3")
    pn("pn: printjobs set", "/printjobs/1,2,3")
    pn("pn: printjobs set ignores limit", "/printjobs/1-5?limit=1")
    pn("pn: printjobs unknown", "/printjobs/9999")
    pn("pn: printjobs foreign", "/printjobs/1", ("basic", KB))
    pn("pn: printjobs states", "/printjobs/1-4/states")
    pn("pn: printjobs states of a failed job", f"/printjobs/{hook_fail}/states")
    pn("pn: printjobs states of a cancelled job", f"/printjobs/{cancel_me}/states")
    pn("pn: printjobs states without set", "/printjobs/states")
    pn("pn: beta printers", "/printers", ("basic", KB))
    pn("pn: unknown path with basic", "/nothing")
    pn("pn: dashboard-ish path with basic", "/jobs", BK)
    pdf_job = {"printerId": LASER, "title": "pn pdf", "contentType": "pdf_base64",
               "content": b64(PDF), "source": "pn-test"}
    raw_job = {"printerId": LBL, "title": "pn raw", "contentType": "raw_base64", "content": b64(RAW)}

    def pnpost(name, body, key=BK, **kw):
        return C(name, "POST", "/printjobs", js=body, auth=key, **kw)

    p1 = pnpost("pn: post pdf", pdf_job)
    pnpost("pn: post raw", raw_job)
    pnpost("pn: post pdf with options and qty", dict(pdf_job, qty=2, options={
        "paper": "A4", "bin": "Tray 1", "color": True, "duplex": "long-edge", "pages": "1-2",
        "rotate": 90, "dpi": 600, "fit_to_page": True}))
    pnpost("pn: post copies from options", dict(pdf_job, options={"copies": 3}))
    pnpost("pn: post raw drops options", dict(raw_job, options={"paper": "A4", "copies": 2}))
    pnpost("pn: post options not an object", dict(pdf_job, options=[1]))
    pnpost("pn: post pdf_uri", {"printerId": LASER, "contentType": "pdf_uri", "content": S.url("pdf")})
    pnpost("pn: post raw_uri", {"printerId": LBL, "contentType": "raw_uri", "content": S.url("raw")})
    pnpost("pn: post expireAfter", dict(pdf_job, expireAfter=60))
    pnpost("pn: post uri 404", {"printerId": LBL, "contentType": "raw_uri", "content": S.url("missing")})
    pnpost("pn: post bad scheme", {"printerId": LBL, "contentType": "raw_uri", "content": "ftp://x"})
    pnpost("pn: post unknown contentType", dict(pdf_job, contentType="png_base64"))
    pnpost("pn: post uri_post unsupported", dict(pdf_job, contentType="pdf_uri_post"))
    pnpost("pn: post no contentType", {"printerId": LASER, "content": "x"})
    pnpost("pn: post printer missing", {"contentType": "raw_base64", "content": b64(RAW)})
    pnpost("pn: post printer unknown", dict(raw_job, printerId=99))
    pnpost("pn: post printer foreign", dict(pdf_job), key=("basic", KB))
    pnpost("pn: post qty zero", dict(pdf_job, qty=0))
    pnpost("pn: post qty string", dict(pdf_job, qty="2"))
    pnpost("pn: post bad options value", dict(pdf_job, options={"duplex": "both"}))
    pnpost("pn: post content missing", {"printerId": LBL, "contentType": "raw_base64"})
    pnpost("pn: post bad base64", dict(raw_job, content="###"))
    pnpost("pn: post body not an object", [1])
    C("pn: post body null", "POST", "/printjobs", raw=b"null", auth=BK)
    C("pn: post bad json", "POST", "/printjobs", raw=b"{", auth=BK)
    C("pn: post bad key", "POST", "/printjobs", js=pdf_job, auth=("basic", "nope"))
    C("pn: post without auth", "POST", "/printjobs", js=pdf_job)
    C("pn: post with bearer is not pn", "POST", "/printjobs", js=pdf_job, auth=K)
    C("pn: post bad content-length", "POST", "/printjobs", auth=BK, cl="x")
    C("pn: post with query string", "POST", "/printjobs?x=1", js=raw_job, auth=BK)
    pid = p1.j if isinstance(p1.j, int) else None
    pn("pn: printjobs after posts", "/printjobs?limit=6")
    pn("pn: states of the new job", f"/printjobs/{pid}/states")
    C("pn: delete job", "DELETE", f"/printjobs/{pid}", auth=BK)
    C("pn: delete job again", "DELETE", f"/printjobs/{pid}", auth=BK)
    C("pn: delete a set", "DELETE", f"/printjobs/{(pid or 0) + 1}-{(pid or 0) + 3},{pid}", auth=BK)
    C("pn: delete a done job", "DELETE", f"/printjobs/{j1}", auth=BK)
    C("pn: delete foreign", "DELETE", f"/printjobs/{(pid or 0) + 4}", auth=("basic", KB))
    C("pn: delete not a set", "DELETE", "/printjobs/x", auth=BK)
    C("pn: delete bad set", "DELETE", "/printjobs/1,,2", auth=BK)
    C("pn: delete reversed", "DELETE", "/printjobs/3-1", auth=BK)
    C("pn: delete states sub-resource", "DELETE", "/printjobs/1/states", auth=BK)
    C("pn: delete bad auth", "DELETE", "/printjobs/1", auth=("basic", "nope"))
    C("pn: delete with bearer", "DELETE", "/printjobs/1", auth=K)
    C("pn: delete collection", "DELETE", "/printjobs", auth=BK)
    pn("pn: printjobs after deletes", "/printjobs?limit=12")
    drain("pn", K)

    # ===== 12. Star CloudPRNT ===================================================================
    star = g(C("cloudprnt: create org", "POST", "/orgs", js={"name": "Star shop"}, auth=T), "id")
    KS = g(C("cloudprnt: issue key", "POST", "/apikeys", js={"label": "star", "org_id": star},
             auth=T), "key")
    CP = f"/cloudprnt/{KS}"
    poll = {"status": "23 6 0 0 0 0 0 0 0 ", "printerMAC": MAC, "statusCode": "200%20OK",
            "clientAction": None}
    star_acc = {"Accept": "application/vnd.star.starprnt"}

    def cp(name, method, path, **kw):
        return C(name, method, path, **kw)

    cp("cloudprnt: poll without a key", "POST", "/cloudprnt", js=poll)
    cp("cloudprnt: poll unknown key", "POST", "/cloudprnt/nope", js=poll)
    cp("cloudprnt: poll with root token", "POST", f"/cloudprnt/{T}", js=poll)
    cp("cloudprnt: poll bad json", "POST", CP, raw=b"{")
    cp("cloudprnt: poll without mac", "POST", CP, js={"status": "x"})
    cp("cloudprnt: poll blank mac", "POST", CP, js=dict(poll, printerMAC="  "))
    cp("cloudprnt: poll non-object body", "POST", CP, raw=b"[1]")
    cp("cloudprnt: first poll enrols", "POST", CP, js=poll, headers=star_acc)
    cp("cloudprnt: poll again", "POST", CP, js=poll, headers=star_acc)
    cp("cloudprnt: poll with basic auth", "POST", "/cloudprnt", js=poll, auth=("basic", KS))
    cp("cloudprnt: poll basic auth + wrong path key", "POST", "/cloudprnt/nope", js=poll,
       auth=("basic", KS))
    cp("cloudprnt: poll with query string", "POST", CP + "?x=1", js=poll)
    cp("cloudprnt: GET nothing to print", "GET", CP + f"?mac={MAC}")
    cp("cloudprnt: GET without mac", "GET", CP)
    cp("cloudprnt: GET unknown key", "GET", f"/cloudprnt/nope?mac={MAC}")
    cp("cloudprnt: GET setting file", "GET", f"/cloudprnt/cloudprnt-setting.json?mac={MAC}")
    cp("cloudprnt: GET setting file basic", "GET", f"/cloudprnt-setting.json?mac={MAC}",
       auth=("basic", KS))
    cp("cloudprnt: DELETE without mac", "DELETE", CP)
    cp("cloudprnt: DELETE unknown key", "DELETE", f"/cloudprnt/nope?mac={MAC}&code=OK")
    cp("cloudprnt: DELETE nothing in flight", "DELETE", CP + f"?mac={MAC}&code=OK")
    sp = g(C("cloudprnt: printers", "GET", "/printers", auth=KS), "printers", 0, "id")
    C("cloudprnt: computers", "GET", "/computers", auth=KS)

    def sjob(name, body):
        return g(C(name, "POST", "/jobs", js=dict(body, printer_id=sp), auth=KS), "job_id")

    j_a = sjob("cloudprnt: submit raw x2", {"type": "raw_base64", "content": b64(b"ABC"), "copies": 2})
    cp("cloudprnt: poll offers the job", "POST", CP, js=poll, headers=star_acc)
    cp("cloudprnt: poll re-offers it", "POST", CP, js=poll, headers=star_acc)
    cp("cloudprnt: poll offers nothing while printing", "POST", CP,
       js=dict(poll, printingInProgress=True))
    cp("cloudprnt: poll picks media from Accept", "POST", CP, js=poll,
       headers={"Accept": "text/plain;q=0.5, application/vnd.star.line"})
    cp("cloudprnt: GET unsupported type", "GET", CP + f"?mac={MAC}&type=image/png")
    cp("cloudprnt: GET wrong token", "GET", CP + f"?mac={MAC}&token={(j_a or 0) + 99}")
    cp("cloudprnt: GET payload", "GET", CP + f"?mac={MAC}&type=application/vnd.star.starprnt&token={j_a}")
    cp("cloudprnt: GET payload by Accept", "GET", CP + f"?mac={MAC}",
       headers={"Accept": "application/vnd.star.line"})
    cp("cloudprnt: GET payload text", "GET", CP + f"?mac={MAC}&type=text/plain")
    cp("cloudprnt: GET payload uppercase mac", "GET", CP + f"?mac={MAC.upper()}")
    cp("cloudprnt: GET payload with basic auth", "GET", f"/cloudprnt?mac={MAC}", auth=("basic", KS))
    cp("cloudprnt: confirm ok", "DELETE", CP + f"?mac={MAC}&code=200%20OK&token={j_a}")
    C("cloudprnt: job is done", "GET", f"/jobs/{j_a}", auth=KS)
    j_b = sjob("cloudprnt: submit raw (error)", {"type": "raw_base64", "content": b64(b"ERR")})
    cp("cloudprnt: poll offers the second", "POST", CP, js=poll)
    cp("cloudprnt: confirm cover open", "DELETE", CP + f"?mac={MAC}&code=420%20Cover%20open")
    C("cloudprnt: job failed with the code", "GET", f"/jobs/{j_b}", auth=KS)
    j_c = sjob("cloudprnt: submit raw (no code)", {"type": "raw_base64", "content": b64(b"NOC")})
    cp("cloudprnt: poll offers the third", "POST", CP, js=poll)
    cp("cloudprnt: confirm without code", "DELETE", CP + f"?mac={MAC}")
    C("cloudprnt: job failed without code", "GET", f"/jobs/{j_c}", auth=KS)
    j_d = sjob("cloudprnt: submit raw (2xx warning)", {"type": "raw_base64", "content": b64(b"WRN")})
    cp("cloudprnt: poll offers the fourth", "POST", CP, js=poll)
    cp("cloudprnt: confirm 211 paper low", "DELETE", CP + f"?mac={MAC}&code=211%20Paper%20low")
    C("cloudprnt: job done despite warning", "GET", f"/jobs/{j_d}", auth=KS)
    j_e = sjob("cloudprnt: submit raw (unknown code)", {"type": "raw_base64", "content": b64(b"UNK")})
    cp("cloudprnt: poll offers the fifth", "POST", CP, js=poll)
    cp("cloudprnt: confirm unknown code", "DELETE", CP + f"?mac={MAC}&code=999%20Weird")
    C("cloudprnt: job failed with unknown code", "GET", f"/jobs/{j_e}", auth=KS)
    j_f = sjob("cloudprnt: submit pdf", {"type": "pdf_base64", "content": b64(PDF)})
    cp("cloudprnt: poll fails the pdf instead of offering it", "POST", CP, js=poll)
    C("cloudprnt: pdf job failed", "GET", f"/jobs/{j_f}", auth=KS)
    C("cloudprnt: metrics", "GET", "/metrics", auth=KS)
    C("cloudprnt: printers after", "GET", "/printers", auth=KS)

    # ===== 13. quota + billing ==================================================================
    C("plans: root", "GET", "/plans", auth=T)
    C("plans: machine key", "GET", "/plans", auth=KB)
    C("plans: session", "GET", "/plans", auth=SB)
    C("quota: put free plan on beta", "PUT", f"/orgs/{beta}", js={"plan": "free"}, auth=T)
    bj = {"type": "raw_base64", "printer_id": 3, "content": b64(RAW)}
    for i in range(1, 3):
        post(f"quota: beta job {i}", dict(bj, idempotency_key=f"b{i}"), key=KB)
    post("quota: beta job 3 refused", dict(bj, idempotency_key="b3"), key=KB)
    post("quota: idempotent resubmit at the cap", dict(bj, idempotency_key="idem-1"), key=KB)
    order("quota: order refused", {"printer_id": 3, "order": ORDER}, key=KB)
    pnpost("quota: printnode refused", dict(raw_job, printerId=3), key=("basic", KB))
    C("quota: org usage", "GET", f"/orgs/{beta}", auth=T)

    def bill(name, event, secret=BILLING_SECRET, prefix="", sign=True, raw=None, headers=None, **kw):
        body = raw if raw is not None else json.dumps(event).encode()
        h = dict(headers or {})
        if sign:
            h["X-Signature"] = prefix + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
        return C(name, "POST", "/billing/webhook", raw=body, headers=h, **kw)

    bill("billing: unsigned", {"org_id": beta, "plan": "pro"}, sign=False)
    bill("billing: wrong secret", {"org_id": beta, "plan": "pro"}, secret="wrong")
    bill("billing: garbage signature", {"org_id": beta, "plan": "pro"}, sign=False,
         headers={"X-Signature": "sha256=zz"})
    bill("billing: bad json", None, raw=b"{")
    bill("billing: empty body", None, raw=b"")
    bill("billing: non-object event", [1])
    bill("billing: no org or email", {"plan": "pro"})
    bill("billing: org id not an integer", {"org_id": "x", "plan": "pro"})
    bill("billing: unknown plan", {"org_id": beta, "plan": "gold"})
    bill("billing: unknown org", {"org_id": 999, "plan": "pro"})
    bill("billing: unknown email", {"email": "ghost@x.test", "plan": "pro"})
    bill("billing: upgrade beta", {"org_id": beta, "plan": "pro"}, prefix="sha256=")
    post("quota: beta job after upgrade", dict(bj, idempotency_key="b5"), key=KB)
    bill("billing: org id as string", {"org_id": f" {beta} ", "plan": "free", "status": "active"})
    bill("billing: upgrade by e-mail", {"email": " Boss@Beta.test ", "plan": "pro"})
    bill("billing: cancel downgrades", {"org_id": beta, "plan": "pro", "status": "canceled"})
    bill("billing: past_due downgrades", {"email": "boss@beta.test", "plan": "pro",
                                          "status": "past_due"})
    post("quota: beta job after downgrade", dict(bj, idempotency_key="b6"), key=KB)
    bill("billing: trialing is active", {"org_id": beta, "plan": "pro", "status": "TRIALING"})
    C("billing: beta org after events", "GET", f"/orgs/{beta}", auth=SB)
    C("billing: GET is not a route", "GET", "/billing/webhook", auth=T)
    C("billing: bad content-length", "POST", "/billing/webhook", cl="abc")

    # ===== 14. signup + throttles ===============================================================
    C("signup: health advertises open signup", "GET", "/health")
    C("signup: bad json", "POST", "/signup", raw=b"{")
    C("signup: bad email", "POST", "/signup", js={"email": "nobody", "password": "password1"})
    C("signup: short password", "POST", "/signup", js={"email": "a@shop.test", "password": "x"})
    C("signup: non-object body", "POST", "/signup", raw=b"[]", crash_ok=True)
    sg = C("signup: ok", "POST", "/signup", js={"email": " New@Shop.test ", "password": "password1",
                                                "org_name": "  Shop  "})
    C("signup: session works", "GET", "/me", auth=g(sg, "token"))
    C("signup: taken", "POST", "/signup", js={"email": "new@shop.test", "password": "password1"})
    C("signup: org named after the domain", "POST", "/signup",
      js={"email": "dom@domain.test", "password": "password1"})
    for i in range(1, 12):
        C(f"signup: throttle attempt {i}", "POST", "/signup",
          js={"email": f"spray{i}@spray.test", "password": "password1"})
    C("signup: throttled even when invalid", "POST", "/signup", js={"email": "nobody", "password": "x"})
    C("signup: orgs afterwards", "GET", "/orgs", auth=T)
    C("signup: users afterwards", "GET", "/users", auth=T)

    # ===== 15. webhook delivery =================================================================
    want = 4                              # three job callbacks + the order's
    deadline = time.time() + 20
    while time.time() < deadline and (len(S.stub.entries(f"/hook/{S.label}/job"))
                                      + len(S.stub.entries(f"/hook/{S.label}/order"))) < want:
        time.sleep(0.25)
    time.sleep(0.5)                       # catch an extra (wrong) delivery too
    hooks = S.stub.entries(f"/hook/{S.label}/job")
    S.record("webhooks: job callbacks received", sorted(
        ({"method": e["method"], "ctype": e["ctype"], "ua": e["ua"],
          "path": norm_text(e["path"], S.label), "body": norm(_jsonish(e["body"]), S.label)}
         for e in hooks), key=lambda e: canon(e)))
    orders_hooks = S.stub.entries(f"/hook/{S.label}/order")
    S.record("webhooks: order callback received", [
        {"ctype": e["ctype"], "body": norm(_jsonish(e["body"]), S.label)} for e in orders_hooks])
    S.record("fetches: payload sources seen by the stub", [
        {"method": e["method"], "path": e["path"].split("/", 2)[2], "ua": e["ua"],
         "ctype": e["ctype"] if e["method"] == "POST" else None, "body": _jsonish(e["body"])}
        for e in S.stub.entries(f"/{S.label}/")])
    if S.events:
        # An agent goes offline 60 s after its last request; the dispatcher then POSTs the edge to
        # its org's event_url. Two orgs (acme, beta) have agents and an event_url -> two events.
        C("events: set event_url", "PUT", f"/orgs/{beta}", js={"event_url": S.hook("event")}, auth=T)
        deadline = time.time() + 100
        while time.time() < deadline and len(S.stub.entries(f"/hook/{S.label}/event")) < 2:
            time.sleep(1)
        time.sleep(0.5)
        S.record("events: agent liveness event received", sorted(
            ({"ctype": e["ctype"], "ua": e["ua"], "body": norm(_jsonish(e["body"]), S.label)}
             for e in S.stub.entries(f"/hook/{S.label}/event")), key=lambda e: canon(e)))

    # ===== 16. dashboard ========================================================================
    web = Path(S.server.cwd) / "app" / "web"
    chunk = next(iter(sorted((web / "_next" / "static").rglob("*.js"))), None) if web.is_dir() else None
    sub = next((p.parent.name for p in sorted(web.glob("*/index.html"))), "devices") if web.is_dir() else "devices"
    C("dashboard: root", "GET", "/", body=False, cache=True)
    C("dashboard: index.html", "GET", "/index.html", body=False, cache=True)
    C("dashboard: HEAD root", "HEAD", "/")
    C("dashboard: root with query", "GET", "/?reset=abc", body=False, cache=True)
    C("dashboard: deep link", "GET", f"/{sub}/", body=False, cache=True)
    C("dashboard: deep link without slash", "GET", f"/{sub}", body=False, cache=True)
    C("dashboard: HEAD deep link", "HEAD", f"/{sub}/")
    if chunk:
        rel = chunk.relative_to(web).as_posix()
        C("dashboard: hashed js asset", "GET", f"/{rel}", body=False, cache=True)
        C("dashboard: HEAD hashed js asset", "HEAD", f"/{rel}")
    C("dashboard: svg icon", "GET", "/icon.svg", body=False, cache=True)
    C("dashboard: txt file", "GET", "/index.txt", body=False, cache=True)
    C("dashboard: 404 page", "GET", "/404.html", body=False, cache=True)
    C("dashboard: missing file", "GET", "/no-such-file.js")
    C("dashboard: traversal", "GET", "/..%2f..%2fapp%2fstore.py")
    C("dashboard: traversal in static", "GET", "/_next/static/..%2f..%2findex.html", body=False, cache=True)
    C("dashboard: encoded nul", "GET", "/a%00b")
    C("dashboard: POST root", "POST", "/", js={})
    C("dashboard: file never shadows the API", "GET", "/jobs.txt")

    # ===== 17. final state ======================================================================
    C("final: orgs", "GET", "/orgs", auth=T)
    C("final: metrics", "GET", "/metrics", auth=T)
    C("final: jobs", "GET", "/jobs", auth=T)
    C("final: keys", "GET", "/apikeys", auth=T)
    C("final: computers", "GET", "/computers", auth=T)
    C("final: printers", "GET", "/printers", auth=T)


def _jsonish(b):
    try:
        return json.loads(b)
    except ValueError:
        return {"bytes": b.hex()}


# --- running both and comparing ---------------------------------------------------------------


def run_scenario(srv, stub, events):
    S = Run(srv, stub)
    S.events = events
    try:
        scenario(S)
    except Exception as e:                       # a scenario that cannot continue is a result too
        S.record("SCENARIO ABORTED", f"{type(e).__name__}: {e}")
    return S


def compare(a, b, known):
    """-> list of result dicts: name, verdict (PASS/DIFF/KNOWN), reason, diff."""
    out, bmap = [], {s["name"]: s for s in b}
    order = [s["name"] for s in a] + [s["name"] for s in b if s["name"] not in {x["name"] for x in a}]
    amap = {s["name"]: s for s in a}
    for name in order:
        sa, sb = amap.get(name), bmap.get(name)
        res = {"name": name, "verdict": "PASS", "reason": "", "diff": "",
               "req": (sa or sb).get("req", ""),
               "status_a": _status(sa), "status_b": _status(sb)}
        if sa is None or sb is None:
            res.update(verdict="DIFF", diff=f"step only present in {'B' if sa is None else 'A'}")
        else:
            ca, cb = canon(sa["data"]), canon(sb["data"])
            if ca != cb:
                a_crash = sa["data"].get("error") and not sb["data"].get("error")
                if sa["crash_ok"] and a_crash:
                    res.update(verdict="KNOWN",
                               reason="Python crashes on this input (no response); B answers cleanly")
                else:
                    res["diff"] = "".join(difflib.unified_diff(
                        (ca + "\n").splitlines(True), (cb + "\n").splitlines(True),
                        "A (python)", "B (rust)", n=2))
                    if sa.get("bytes") is not None and sb.get("bytes") is not None \
                            and sa["bytes"] != sb["bytes"] and sa["data"].get("ctype", "").startswith(
                                ("application/octet", "application/pdf")):
                        n = next((i for i, (x, y) in enumerate(zip(sa["bytes"], sb["bytes"]))
                                  if x != y), min(len(sa["bytes"]), len(sb["bytes"])))
                        res["diff"] += f"first differing byte offset: {n}\n"
                    res["verdict"] = "DIFF"
        if res["verdict"] == "DIFF" and name in known:
            res.update(verdict="KNOWN", reason=known[name])
        out.append(res)
    return out


def _status(step):
    if step is None:
        return None
    d = step["data"]
    return d.get("status") or d.get("error") or "-"


def report(results, verbose):
    counts = {"PASS": 0, "DIFF": 0, "KNOWN": 0}
    for r in results:
        counts[r["verdict"]] += 1
        if r["verdict"] != "PASS" or verbose:
            print(f"{r['verdict']:5}  {r['name']}" + (f"   [{r['req']}]" if r["verdict"] != "PASS" else "")
                  + (f"  ({r['reason']})" if r["reason"] else ""))
        else:
            print(f"PASS   {r['name']}   [{r['status_a']}]")
        if r["verdict"] == "DIFF":
            print("".join("      " + ln + "\n" for ln in r["diff"].rstrip("\n").split("\n")), end="")
    print(f"\n{len(results)} steps: {counts['PASS']} pass, {counts['DIFF']} diff, "
          f"{counts['KNOWN']} known")
    return counts


# --- sources of the two servers --------------------------------------------------------------


def extract_ref(ref, dest):
    p = subprocess.run(["git", "-C", str(REPO), "archive", ref, "app"], capture_output=True)
    if p.returncode:
        raise SystemExit(f"git archive {ref} failed: {p.stderr.decode(errors='replace')}")
    with tarfile.open(fileobj=io.BytesIO(p.stdout)) as tf:
        try:
            tf.extractall(dest, filter="data")
        except TypeError:                     # Python before the extraction filters
            tf.extractall(dest)


def rust_binary(arg):
    exe = "printpapi.exe" if os.name == "nt" else "printpapi"
    cands = [Path(arg)] if arg else []
    if os.environ.get("CARGO_TARGET_DIR"):
        cands.append(Path(os.environ["CARGO_TARGET_DIR"]) / "release" / exe)
    cands.append(REPO / "target" / "release" / exe)
    found = next((c for c in cands if c.is_file()), None)
    # Absolute: the server is started with its own cwd, and on POSIX a relative program path is
    # resolved against that cwd (Windows resolves it against ours), so `--rust target/...` broke CI.
    return (found.resolve() if found else None), cands


def python_cmd():
    return [sys.executable, "-u", "-m", "app.server"]


# --- db compatibility mode ----------------------------------------------------------------------


def run_db_compat(py_cwd, rust_cmd, rust_cwd, work):
    """Python -> Rust -> Python on one DB file: whatever one wrote, the next must read and extend."""
    checks = []

    def check(name, cond, detail=""):
        checks.append({"name": name, "verdict": "PASS" if cond else "DIFF", "detail": detail})
        print(f"{'PASS' if cond else 'FAIL':5}  {name}" + ("" if cond else f"   {detail}"))

    db = Path(work) / "compat.db"
    mk_py = lambda tag: Server(tag, python_cmd(), py_cwd, work, db)
    mk_rs = lambda tag: Server(tag, rust_cmd, rust_cwd, work, db)
    print("phase 1: python writes")
    s = mk_py("compat-py1").start()
    try:
        P = s.port
        call = lambda m, p, a=None, **kw: _quick(P, m, p, a, **kw)
        org = call("POST", "/orgs", TOKEN, js={"name": "Compat"}).j["id"]
        call("POST", f"/orgs/{org}/users", TOKEN, js={"email": "py@compat.test", "password": "python-pass"})
        sess_py = call("POST", "/login", js={"email": "py@compat.test", "password": "python-pass"}).j["token"]
        key = call("POST", "/apikeys", TOKEN, js={"label": "k", "org_id": org}).j["key"]
        reg = call("POST", "/agent/register", key, js={"name": "compat-agent", "printers": [
            {"name": "P1", "can_pdf": True, "capabilities": {"duplex": True, "papers": ["A4"]}}]})
        pid = reg.j["printer_ids"]["P1"]
        job = call("POST", "/jobs", key, js={"type": "pdf_base64", "printer_id": pid,
                                             "content": b64(PDF), "copies": 2, "title": "compat",
                                             "options": {"duplex": "long-edge", "paper": "A4"},
                                             "idempotency_key": "compat-1"})
        job2 = call("POST", "/jobs", key, js={"type": "raw_base64", "printer_id": pid,
                                              "content": b64(RAW)})
        check("python: setup answered 200", all(r.status == 200 for r in (reg, job, job2)))
        j1, j2 = job.j["job_id"], job2.j["job_id"]
    finally:
        s.stop()
    print("phase 2: rust reads the python DB and writes to it")
    s = mk_rs("compat-rs").start()
    try:
        P = s.port
        call = lambda m, p, a=None, **kw: _quick(P, m, p, a, **kw)
        check("rust: login with a python-made scrypt hash",
              call("POST", "/login", js={"email": "py@compat.test", "password": "python-pass"}).status == 200)
        check("rust: wrong password refused",
              call("POST", "/login", js={"email": "py@compat.test", "password": "nope"}).status == 401)
        check("rust: python session token still valid", call("GET", "/me", sess_py).status == 200)
        check("rust: python api key authenticates", call("GET", "/jobs", key).status == 200)
        jobs = call("GET", "/jobs", key).j or {}
        check("rust: sees both python jobs", [j["id"] for j in jobs.get("jobs", [])] == [j2, j1])
        pr = call("GET", "/printers", key).j or {}
        check("rust: printer capabilities readable",
              (pr.get("printers") or [{}])[0].get("capabilities") == {"duplex": True, "papers": ["A4"]})
        claim = call("GET", "/agent/jobs", key)
        check("rust: claims the python job with its options",
              claim.status == 200 and claim.j == {"job_id": j1, "printer_id": pid, "mode": "pdf",
                                                  "copies": 2, "options": {"duplex": "long-edge", "paper": "A4"}},
              repr(claim.j))
        check("rust: payload bytes intact",
              call("GET", f"/agent/jobs/{j1}/payload", key).body == PDF)
        check("rust: idempotency key from python honoured",
              call("POST", "/jobs", key, js={"type": "pdf_base64", "printer_id": pid,
                                             "content": b64(PDF), "idempotency_key": "compat-1"}).j == {"job_id": j1})
        rs_user = call("POST", f"/orgs/{org}/users", TOKEN,
                       js={"email": "rs@compat.test", "password": "rust-pass-1"})
        check("rust: creates a user", rs_user.status == 200)
        sess_rs = call("POST", "/login", js={"email": "rs@compat.test", "password": "rust-pass-1"}).j["token"]
        rs_key = call("POST", "/apikeys", TOKEN, js={"label": "rk", "org_id": org}).j["key"]
        j3 = call("POST", "/jobs", rs_key, js={"type": "raw_base64", "printer_id": pid, "content": b64(b"RUST"),
                                               "title": "from rust", "options": None}).j["job_id"]
        check("rust: submits a job", isinstance(j3, int))
    finally:
        s.stop()
    print("phase 3: python reads what rust wrote")
    s = mk_py("compat-py2").start()
    try:
        P = s.port
        call = lambda m, p, a=None, **kw: _quick(P, m, p, a, **kw)
        check("python: login with a rust-made scrypt hash",
              call("POST", "/login", js={"email": "rs@compat.test", "password": "rust-pass-1"}).status == 200)
        check("python: rust session token valid", call("GET", "/me", sess_rs).status == 200)
        check("python: rust api key authenticates", call("GET", "/jobs", rs_key).status == 200)
        jobs = (call("GET", "/jobs", key).j or {}).get("jobs", [])
        check("python: lists all three jobs", [j["id"] for j in jobs] == [j3, j2, j1])
        check("python: job claimed by rust is still claimed",
              next((j["state"] for j in jobs if j["id"] == j1), None) == "claimed")
        check("python: finishes the job rust claimed",
              call("POST", f"/agent/jobs/{j1}/result", key, js={"ok": True}).status == 200)
        c = call("GET", "/agent/jobs", key)
        check("python: claims the next queued job", c.status == 200 and (c.j or {}).get("job_id") == j2)
        c = call("GET", "/agent/jobs", key)
        check("python: claims the job rust created", c.status == 200 and (c.j or {}).get("job_id") == j3)
        check("python: rust payload intact", call("GET", f"/agent/jobs/{j3}/payload", key).body == b"RUST")
        check("python: states after", [(call("GET", f"/jobs/{i}", key).j or {}).get("state")
                                       for i in (j1, j2, j3)] == ["done", "claimed", "claimed"])
        check("python: metrics render", call("GET", "/metrics", key).status == 200)
    finally:
        s.stop()
    bad = sum(c["verdict"] != "PASS" for c in checks)
    print(f"\n{len(checks)} checks: {len(checks) - bad} pass, {bad} fail")
    return checks, bad


def _quick(port, method, path, auth=None, js=None, **kw):
    h = {}
    if auth:
        h["Authorization"] = "Bearer " + auth
    body = None
    if js is not None:
        body = json.dumps(js).encode()
        h["Content-Type"] = "application/json"
    return http_call(port, method, path, h, body, **kw)


# --- main --------------------------------------------------------------------------------------


def main(argv=None):
    ap = argparse.ArgumentParser(description="Python-vs-Rust black-box parity harness")
    ap.add_argument("--ref", default="v2.5.1", help="git ref the Python server is taken from")
    ap.add_argument("--python-dir", help="existing checkout (with app/) instead of a git ref")
    ap.add_argument("--rust", help="path of the Rust server binary (default target/release/printpapi)")
    ap.add_argument("--b", dest="b_cmd", help="custom command for server B (run in A's directory)")
    ap.add_argument("--b-python-dir", help="B = the Python server of this checkout (mutation tests)")
    ap.add_argument("--dump-steps", help="write server A's normalised steps here (determinism checks)")
    ap.add_argument("--a-only", action="store_true",
                    help="self-validation: B is a second instance of the Python server")
    ap.add_argument("--json", help="write the results to this file")
    ap.add_argument("--events", action="store_true",
                    help="also wait ~60-100 s for the agent-offline event_url delivery")
    ap.add_argument("--db-compat", action="store_true", help="python -> rust -> python on one DB")
    ap.add_argument("--serial", action="store_true", help="run A then B instead of in parallel")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args(argv)
    for s in (sys.stdout, sys.stderr):
        try:
            s.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass

    work = Path(tempfile.mkdtemp(prefix="parity-"))
    servers, stub = [], []

    def cleanup():
        for s in servers:
            s.stop()
        for s in stub:
            s.stop()
        shutil.rmtree(work, ignore_errors=True)

    atexit.register(cleanup)
    t_start = time.time()

    if args.python_dir:
        py_cwd = Path(args.python_dir).resolve()
        if not (py_cwd / "app" / "server.py").is_file():
            raise SystemExit(f"{py_cwd}/app/server.py not found")
    else:
        py_cwd = work / "python"
        py_cwd.mkdir()
        extract_ref(args.ref, py_cwd)
        print(f"server A: python server from git ref {args.ref}")

    rust, cands = rust_binary(args.rust)
    if args.b_python_dir:
        b_cmd, b_cwd, b_name = python_cmd(), Path(args.b_python_dir).resolve(), "python " + args.b_python_dir
    elif args.b_cmd:
        b_cmd, b_cwd, b_name = shlex.split(args.b_cmd, posix=os.name != "nt"), py_cwd, "custom"
    elif args.a_only:
        b_cmd, b_cwd, b_name = python_cmd(), py_cwd, "python (self-validation)"
    elif rust:
        b_cmd, b_cwd, b_name = [str(rust)], work, f"rust ({rust})"
    else:
        b_cmd = None

    if args.db_compat:
        if not rust:
            print("SKIPPED --db-compat: no Rust binary found (looked at: "
                  + ", ".join(str(c) for c in cands) + ")")
            return 0
        checks, bad = run_db_compat(py_cwd, [str(rust)], work, work)
        if args.json:
            Path(args.json).write_text(json.dumps({"mode": "db-compat", "checks": checks}, indent=1))
        return 1 if bad else 0

    if b_cmd is None:
        print("SKIPPED: no Rust binary found (looked at: " + ", ".join(str(c) for c in cands)
              + "). Build it, pass --rust, or self-validate with --a-only.")
        return 0

    st = Stub()
    stub.append(st)
    a = Server("A", python_cmd(), py_cwd, work)
    b = Server("B", b_cmd, b_cwd, work)
    servers.extend([a, b])
    print(f"server B: {b_name}")
    a.start()
    b.start()
    print(f"A on :{a.port}, B on :{b.port}; stub on :{st.port}; running the scenario"
          + (" serially" if args.serial else " on both in parallel"))
    box = {}

    def go(key, srv):
        box[key] = run_scenario(srv, st, args.events)

    if args.serial:
        go("A", a)
        go("B", b)
    else:
        ts = [threading.Thread(target=go, args=(k, s)) for k, s in (("A", a), ("B", b))]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
    for s in servers:
        s.stop()

    if args.dump_steps:
        Path(args.dump_steps).write_text(json.dumps(
            [{"name": s["name"], "data": s["data"]} for s in box["A"].steps], indent=1,
            ensure_ascii=False), encoding="utf-8")
    results = compare(box["A"].steps, box["B"].steps, KNOWN_DIFFS)
    counts = report(results, args.verbose)
    elapsed = time.time() - t_start
    print(f"took {elapsed:.1f} s")
    if args.json:
        Path(args.json).write_text(json.dumps({
            "a": f"python {args.python_dir or args.ref}", "b": b_name, "elapsed_s": round(elapsed, 1),
            "counts": counts, "steps": results}, indent=1, ensure_ascii=False))
    return 1 if counts["DIFF"] else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        sys.exit(130)
