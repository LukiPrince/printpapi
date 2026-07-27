# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import base64
import hashlib
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
from urllib.parse import parse_qs, unquote, urlparse

from app import auth, billing, cloudprnt, mail, printnode, store
from app.dispatch import (decode_payload, agent_mode, parse_copies, parse_callback_url,
                          parse_options, parse_expire_after, parse_idempotency_key,
                          DispatchError, FetchError, _http_get, _http_post)
from app.orders import OrderError, normalize_order
from app.packing_slip import render_packing_slip

_MAX_BODY = 32 * 1024 * 1024  # ponytail: flat 32 MB body cap; make env-tunable if someone needs bigger

_JOB_ID = re.compile(r"^/jobs/(\d+)$")
_AGENT_PAYLOAD = re.compile(r"^/agent/jobs/(\d+)/payload$")
_AGENT_RESULT = re.compile(r"^/agent/jobs/(\d+)/result$")
_APIKEY_ID = re.compile(r"^/apikeys/(\d+)$")
_ORG_ID = re.compile(r"^/orgs/(\d+)$")
_ORG_USERS = re.compile(r"^/orgs/(\d+)/users$")
_USER_ID = re.compile(r"^/users/(\d+)$")

# Compared against when no user matches the e-mail, so a wrong address and a wrong password cost
# the same ~50 ms — the login answer alone must not reveal which accounts exist.
_DUMMY_HASH = auth.hash_password("no such user, no such password")
# PrintNode-compat paths: a collection addressed by id set ("5", "5,7", "5-9"), optionally with a
# sub-resource. Only reachable with HTTP Basic auth — see _printnode_get.
_PN_SET = re.compile(r"^/(computers|printers|printjobs)/([\d,\- ]+)(/printers|/states)?$")
# Star CloudPRNT: one URL answers all three of its methods. The client key rides in the path
# because the printer appends its own query string to whatever URL it was configured with; a
# printer that can fill in its "User Name" setting instead sends the key as HTTP Basic.
_CLOUDPRNT = re.compile(r"^/cloudprnt(?:/([A-Za-z0-9_-]+))?$")
_PN_COLLECTIONS = ("/whoami", "/computers", "/printers", "/printjobs")

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
                 long_poll_timeout=25.0, poll_interval=1.0, online_window_s=60,
                 session_ttl_s=auth.SESSION_TTL_S, max_login_fails=10, login_window_s=900,
                 signup="closed", send_mail=None, public_url=None, reset_ttl_s=3600,
                 reset_enabled=None, plans=(), billing_secret=None):
    fetch = fetch_url or _http_get
    limiter = auth.LoginLimiter(max_fails=max_login_fails, window_s=login_window_s)
    # Signup is off unless the operator turns it on: a self-hosted box on the open internet must
    # not hand an org to whoever finds it. Only the hosted deployment sets PRINTAPI_SIGNUP=open.
    signup_open = signup == "open"
    send_mail = send_mail or mail.send
    reset_enabled = mail.configured() if reset_enabled is None else bool(reset_enabled)
    public_url = (public_url or "").rstrip("/")
    # Billing needs both halves: a catalogue to sell and a secret to trust the provider's callback.
    # A self-hosted box configures neither, and every billing route answers 503.
    plans = list(plans)

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

        def _principal(self):
            """Who is calling, resolved from the one Bearer header. None = no valid credential.

            kind='root'     the bootstrap token: no org filter, may act in and manage every org.
            kind='session'  a browser login (POST /login): one org, and may manage it — keys,
                            users, org settings.
            kind='key'      a machine credential: one org, print and read only. A leaked
                            integration key must not be able to issue itself a successor.

            Which table the credential resolves in *is* the permission — no role column.
            """
            key = self._presented_key()
            if hmac.compare_digest(key, token):
                return {"kind": "root", "org_id": None, "user_id": None, "manage": True}
            if key.startswith("sess_"):             # never looked up as a machine key
                s = store.authenticate_session(conn, key)
                return None if s is None else {
                    "kind": "session", "org_id": s["org_id"], "user_id": s["user_id"],
                    "email": s["email"], "manage": True}
            row = store.authenticate_client(conn, key)
            return None if row is None else {
                "kind": "key", "org_id": row["org_id"], "user_id": None, "manage": False}

        def _client_org(self):
            """The print/read gate: (ok, org_id) — org_id None means root, no filter."""
            p = self._principal()
            return (True, p["org_id"]) if p else (False, None)

        def _manager(self):
            """The gate for key/user/org administration: root or a browser session, never a
            machine key. Answers 401 itself and returns None if the caller may not manage."""
            p = self._principal()
            if p is None or not p["manage"]:
                self._json(401, {"error": "unauthorized"})
                return None
            return p

        def _foreign_org(self, p, org_id):
            """True if this principal may not touch `org_id` (root may touch every one)."""
            return p["org_id"] is not None and org_id != p["org_id"]

        def _create_user(self, org_id, body):
            email = (body.get("email") or "").strip().lower()
            if "@" not in email:
                return self._json(400, {"error": "email required"})
            try:
                pw_hash = auth.hash_password(body.get("password"))
            except ValueError as e:
                return self._json(400, {"error": str(e)})
            try:
                uid = store.create_user(conn, org_id, email, pw_hash)
            except sqlite3.IntegrityError:
                return self._json(409, {"error": "email already registered"})
            return self._json(200, {"id": uid, "email": email, "org_id": org_id})

        def _login(self):
            try:
                body = self._read_json()
            except ValueError:
                return self._json(400, {"error": "bad json"})
            email = (body.get("email") or "").strip().lower()
            if not limiter.allow(email):
                return self._json(429, {"error": "too many failed logins, try again later"})
            user = store.get_user_by_email(conn, email)
            # Verify even when there is no such user (against a dummy hash): same work, same
            # timing, and one shared "invalid credentials" answer for both cases.
            ok = auth.verify_password(body.get("password"),
                                      user["password_hash"] if user else _DUMMY_HASH)
            if user is None or not ok:
                limiter.fail(email)
                return self._json(401, {"error": "invalid credentials"})
            limiter.succeed(email)
            tok = auth.new_session_token()
            expires_at = store.create_session(conn, user["id"], tok, session_ttl_s)
            return self._json(200, {"token": tok, "expires_at": expires_at,
                                    "org_id": user["org_id"], "user_id": user["id"],
                                    "email": user["email"]})

        def _signup(self):
            """Self-serve: a new org, its first user, and a session in one call — the hosted
            deployment's front door. Off by default (see signup_open)."""
            if not signup_open:
                return self._json(403, {"error": "signup is disabled on this server"})
            try:
                body = self._read_json()
            except ValueError:
                return self._json(400, {"error": "bad json"})
            # Throttled by client address, not by e-mail: an org sprayer picks a fresh address
            # every time, so counting addresses would bound nothing.
            # ponytail: the address is the socket peer — behind a reverse proxy every signup shares
            # one key. Read X-Forwarded-For (and pin the trusted proxy) if that becomes the setup.
            slot = f"signup:{self.client_address[0]}"
            if not limiter.allow(slot):
                return self._json(429, {"error": "too many signups, try again later"})
            email = (body.get("email") or "").strip().lower()
            if "@" not in email:
                return self._json(400, {"error": "email required"})
            try:
                pw_hash = auth.hash_password(body.get("password"))
            except ValueError as e:
                return self._json(400, {"error": str(e)})
            name = (body.get("org_name") or "").strip() or email.split("@", 1)[1]
            limiter.fail(slot)          # a *successful* signup spends a slot too — that is the cap
            try:
                new = store.signup(conn, name, email, pw_hash)
            except sqlite3.IntegrityError:
                return self._json(409, {"error": "email already registered"})
            tok = auth.new_session_token()
            expires_at = store.create_session(conn, new["user_id"], tok, session_ttl_s)
            return self._json(200, {"token": tok, "expires_at": expires_at,
                                    "org_id": new["org_id"], "user_id": new["user_id"],
                                    "email": email})

        def _password_reset(self):
            """Always 200, whether or not the address exists — the answer must not enumerate
            accounts. A mail goes out only for a real one."""
            try:
                body = self._read_json()
            except ValueError:
                return self._json(400, {"error": "bad json"})
            email = (body.get("email") or "").strip().lower()
            slot = f"reset:{email}"
            if not limiter.allow(slot):
                return self._json(429, {"error": "too many reset requests, try again later"})
            limiter.fail(slot)          # counted for every address, so a 429 leaks nothing either
            user = store.get_user_by_email(conn, email)
            if user is not None:
                tok = auth.new_session_token()
                store.create_password_reset(conn, user["id"], tok, reset_ttl_s)
                # The link is built from PUBLIC_URL only. Deriving it from the Host header would
                # let anyone who can send this server a request mail a *valid* token pointing at
                # their own host; without the env var the mail carries the bare token instead.
                link = (f"\nOpen {public_url}/?reset={tok}\n" if public_url else "")
                try:
                    send_mail(email, "Reset your printpapi password",
                              f"Someone asked to reset the printpapi password for {email}.\n"
                              f"{link}\ntoken: {tok}\n\n"
                              f"It is valid for {reset_ttl_s // 60} minutes and works once. "
                              f"If this was not you, ignore this message.\n")
                except Exception as e:      # a broken mail server must not answer differently
                    print(f"password reset mail to {email} failed: {e}", file=sys.stderr)
            return self._json(200, {"ok": True})

        def _password_reset_confirm(self):
            try:
                body = self._read_json()
            except ValueError:
                return self._json(400, {"error": "bad json"})
            try:
                pw_hash = auth.hash_password(body.get("password"))
            except ValueError as e:
                return self._json(400, {"error": str(e)})
            if store.consume_password_reset(conn, body.get("token"), pw_hash) is None:
                return self._json(400, {"error": "invalid or expired token"})
            return self._json(200, {"ok": True})

        def _read_body(self):
            length = int(self.headers.get("Content-Length", 0))   # ValueError -> caller's 400
            if length < 0 or length > _MAX_BODY:
                raise ValueError("bad content-length")
            return self.rfile.read(length)

        def _read_json(self):
            return json.loads(self._read_body() or b"{}")

        def _agent_id(self):
            key = self._presented_key()
            return agent_auth(conn, key) if key else None

        def _submit_job(self, body, org, user_id=None):
            """The POST /jobs core: validate, fetch the payload, enqueue. Returns the job id and
            raises DispatchError / FetchError / store.UnknownPrinter for the caller to map — the
            PrintNode compat layer submits through here too, so there is one validation path."""
            mode = agent_mode(body.get("type"))
            # Validate before decode_payload: a bad field must 400 without first fetching a URL
            # (and a retried submit must not re-fetch it either).
            copies = parse_copies(body)
            callback_url = parse_callback_url(body)
            options = parse_options(body, mode)
            idem = parse_idempotency_key(body)
            expire_after = parse_expire_after(body)
            data = decode_payload(body, fetch_url=fetch)
            return store.enqueue_job(conn, body.get("printer_id"), body.get("type"), mode, data,
                                     user_id=user_id or store.DEFAULT_USER,
                                     title=body.get("title"), copies=copies,
                                     callback_url=callback_url, options=options, org_id=org,
                                     idempotency_key=idem, expire_after=expire_after)

        def _enqueue_order(self, payload, fmt, opts, org, *, idem=None, shop=None, user_id=None):
            """Render a store order as a packing slip and queue it as a pdf job. `opts` carries the
            same job knobs POST /jobs takes (printer_id, copies, title, expire_after, …)."""
            try:
                order = normalize_order(payload, fmt)
                copies = parse_copies(opts)
                callback_url = parse_callback_url(opts)
                expire_after = parse_expire_after(opts)
                idem = idem or parse_idempotency_key(opts)
            except (OrderError, DispatchError) as e:
                return self._json(400, {"error": str(e)})
            if shop and not order.get("shop"):
                order["shop"] = shop
            printer = store.get_printer(conn, opts.get("printer_id"), org_id=org)
            if printer is None:
                return self._json(400, {"error": f"unknown printer: {opts.get('printer_id')}"})
            if not printer["can_pdf"]:
                # gotcha #1: a label printer form-feeds blanks on a PDF. Refuse before printing.
                return self._json(400, {"error": "printer is raw-only; a packing slip needs a "
                                                 "PDF-capable printer"})
            try:
                jid = store.enqueue_job(conn, printer["id"], "order", "pdf",
                                        render_packing_slip(order),
                                        user_id=user_id or store.DEFAULT_USER,
                                        title=opts.get("title") or f"Packing slip {order['number']}",
                                        copies=copies, callback_url=callback_url, org_id=org,
                                        idempotency_key=idem, expire_after=expire_after)
            except store.QuotaExceeded as e:
                return self._json(402, {"error": str(e)})
            return self._json(200, {"job_id": jid})

        def _shopify_webhook(self):
            """Shopify's order webhook. It cannot send an Authorization header, so the org comes
            from `key` in the URL and *authenticity* from Shopify's HMAC over the raw body — the
            key alone is never enough to print here."""
            q = parse_qs(urlparse(self.path).query)
            try:
                raw = self._read_body()          # read first: an unread body poisons keep-alive
            except ValueError:
                return self._json(400, {"error": "bad content-length"})
            row = store.authenticate_client(conn, (q.get("key") or [""])[0])
            if row is None:                      # an issued client key only — root has no org
                return self._json(401, {"error": "unauthorized"})
            org = row["org_id"]
            secret = (store.get_org(conn, org) or {}).get("shopify_secret")
            if not secret:
                return self._json(400, {"error": "shopify_secret not configured for this org "
                                                 "(PUT /orgs/{id})"})
            digest = base64.b64encode(
                hmac.new(secret.encode(), raw, hashlib.sha256).digest()).decode()
            if not hmac.compare_digest(digest, self.headers.get("X-Shopify-Hmac-Sha256", "")):
                return self._json(401, {"error": "bad signature"})
            try:
                payload = json.loads(raw or b"{}")
                printer_id = int((q.get("printer_id") or [""])[0])
            except ValueError:
                return self._json(400, {"error": "bad json or missing printer_id"})
            # Shopify retries a webhook it considers undelivered, so dedupe on the order id:
            # a redelivery returns the original job instead of printing a second slip.
            sid = payload.get("id") or payload.get("name") if isinstance(payload, dict) else None
            return self._enqueue_order(payload, "shopify", {"printer_id": printer_id}, org,
                                       idem=f"shopify-{sid}" if sid else None,
                                       shop=self.headers.get("X-Shopify-Shop-Domain"))

        def _billing_webhook(self):
            """The payment provider's callback: "this org is on that plan now". Signed with a
            shared secret over the raw body — the body names an org, so an unsigned one would let
            anybody grant themselves the top plan."""
            try:
                raw = self._read_body()          # read first: an unread body poisons keep-alive
            except ValueError:
                return self._json(400, {"error": "bad content-length"})
            if not plans or not billing_secret:
                return self._json(503, {"error": "billing is not configured on this server"})
            if not billing.verify(billing_secret, raw, self.headers.get("X-Signature", "")):
                return self._json(401, {"error": "bad signature"})
            try:
                ev = billing.parse_event(json.loads(raw or b"{}"), plans)
            except ValueError as e:              # bad JSON and billing.BillingError alike
                return self._json(400, {"error": str(e)})
            org_id = ev["org_id"]
            if org_id is None:                   # named by its owner's address instead
                user = store.get_user_by_email(conn, ev["email"])
                org_id = user["org_id"] if user else None
            plan = ev["plan"]
            if org_id is None or not store.set_org_plan(conn, org_id, plan["id"], plan["jobs"]):
                return self._json(404, {"error": "unknown org"})
            return self._json(200, {"ok": True, "org_id": org_id, "plan": plan["id"],
                                    "job_quota": plan["jobs"]})

        # --- PrintNode-compatible layer -------------------------------------------------------
        # Selected by auth scheme, not by URL: PrintNode carries the API key as the HTTP Basic
        # username, so a Basic header means "answer in their shapes" while Bearer keeps ours. The
        # shapes themselves live in app/printnode.py; these methods are only routing + auth.
        def _is_basic(self):
            return self.headers.get("Authorization", "").startswith("Basic ")

        def _pn_error(self, code, name, message):
            return self._json(code, {"code": name, "message": message})

        def _pn_org(self):
            """Same credentials as the Bearer API — any issued client key, or the root token."""
            key = printnode.basic_key(self.headers.get("Authorization", ""))
            if key and hmac.compare_digest(key, token):
                return True, None
            row = store.authenticate_client(conn, key)
            return (True, row["org_id"]) if row else (False, None)

        def _pn_printers(self, org, ids=None, agent_ids=None):
            comps = {c["id"]: printnode.computer(c)
                     for c in store.list_agents(conn, online_window_s, org_id=org)}
            return [printnode.printer(p, comps.get(p["agent_id"]))
                    for p in store.list_printers(conn, online_window_s, org_id=org)
                    if (ids is None or p["id"] in ids)
                    and (agent_ids is None or p["agent_id"] in agent_ids)]

        def _pn_jobs(self, org, ids):
            if ids:
                return store.recent_jobs(conn, limit=len(ids), org_id=org, ids=ids)
            # `?limit=` is how their clients page the job list; silently capping every caller at our
            # own default would be a surprising truncation. Junk falls back to the default.
            q = parse_qs(urlparse(self.path).query)
            try:
                limit = min(max(int((q.get("limit") or ["50"])[0]), 1), 500)
            except ValueError:
                limit = 50
            return store.recent_jobs(conn, limit=limit, org_id=org)

        def _printnode_get(self):
            """PrintNode-shaped GETs. False if the path is not part of the compat surface."""
            path = self.path.split("?", 1)[0]
            m = _PN_SET.match(path)
            if path not in _PN_COLLECTIONS and not m:
                return False
            kind = m.group(1) if m else path.lstrip("/")
            sub = (m.group(3) or "") if m else ""
            if ((sub == "/printers" and kind != "computers")
                    or (sub == "/states" and kind != "printjobs")):
                return False                    # e.g. /printers/1/states — not a route of theirs
            ok, org = self._pn_org()
            if not ok:
                self._pn_error(401, "Unauthorized", "invalid API key")
                return True
            try:
                ids = printnode.parse_set(m.group(2)) if m else None
            except printnode.CompatError as e:
                self._pn_error(400, "BadRequest", str(e))
                return True
            if path == "/whoami":
                agents = store.list_agents(conn, online_window_s, org_id=org)
                self._json(200, printnode.whoami(
                    org, store.metrics(conn, online_window_s, org_id=org),
                    [a["name"] for a in agents if a["online"]]))
            elif kind == "computers" and sub != "/printers":
                self._json(200, [printnode.computer(a)
                                 for a in store.list_agents(conn, online_window_s, org_id=org)
                                 if ids is None or a["id"] in ids])
            elif kind == "printers" or sub == "/printers":
                self._json(200, self._pn_printers(
                    org, ids=ids if kind == "printers" else None,
                    agent_ids=ids if kind == "computers" else None))
            elif sub == "/states":
                self._json(200, [printnode.printjob_states(j) for j in self._pn_jobs(org, ids)])
            else:
                pmap = {p["id"]: p for p in self._pn_printers(org)}
                self._json(200, [printnode.printjob(j, pmap.get(j["printer_id"]))
                                 for j in self._pn_jobs(org, ids)])
            return True

        def _printnode_post(self):
            if self.path.split("?", 1)[0] != "/printjobs":
                return False
            ok, org = self._pn_org()
            if not ok:
                self._pn_error(401, "Unauthorized", "invalid API key")
                return True
            try:
                body = self._read_json()
            except ValueError:
                self._pn_error(400, "BadRequest", "malformed json body")
                return True
            try:
                jid = self._submit_job(printnode.job_body(body), org)
            except FetchError as e:
                self._pn_error(502, "DownstreamError", str(e))
            except store.QuotaExceeded as e:
                self._pn_error(402, "QuotaExceeded", str(e))
            except (printnode.CompatError, DispatchError, store.UnknownPrinter) as e:
                self._pn_error(400, "BadRequest", str(e))
            else:
                self._json(201, jid)      # they answer a create with the bare print job id
            return True

        def _printnode_delete(self):
            """Their DELETE /printjobs/{set} drops queued jobs; ours cancels them (a printed job
            stays in the history) and answers with the number affected, as they do."""
            m = _PN_SET.match(self.path.split("?", 1)[0])
            if not m or m.group(1) != "printjobs" or m.group(3):
                return False
            ok, org = self._pn_org()
            if not ok:
                self._pn_error(401, "Unauthorized", "invalid API key")
                return True
            try:
                ids = printnode.parse_set(m.group(2))
            except printnode.CompatError as e:
                self._pn_error(400, "BadRequest", str(e))
                return True
            self._json(200, sum(store.cancel_job(conn, i, org_id=org) == "cancelled" for i in ids))
            return True

        # --- Star CloudPRNT ------------------------------------------------------------------
        # The printer itself is the agent here: it POSTs its status on a timer, GETs the job data
        # when we offer one, and DELETEs with the result once it has printed. That maps onto the
        # ordinary queue — poll = claim_job, GET = get_payload, DELETE = finish_job — so these jobs
        # carry the same quota, history and dashboard as an agent's. Shapes live in app/cloudprnt.py.
        def _cloudprnt_device(self, path_key, mac):
            """Resolve a request to its enrolled device, answering 401/400 itself and returning
            None when it cannot. Every request re-touches the device, which is its liveness."""
            key = path_key or printnode.basic_key(self.headers.get("Authorization", ""))
            row = store.authenticate_client(conn, key)
            if row is None:
                # An issued client key only: the root token belongs to no org, so it has no place
                # to enrol a printer into.
                self._json(401, {"error": "unauthorized"})
                return None
            if not (mac or "").strip():
                self._json(400, {"error": "mac required"})
                return None
            return store.register_cloudprnt(conn, row["org_id"], cloudprnt.device_name(mac))

        def _cloudprnt_poll(self, path_key):
            try:
                body = self._read_json()
            except ValueError:
                return self._json(400, {"error": "bad json"})
            if not isinstance(body, dict):
                body = {}
            dev = self._cloudprnt_device(path_key, body.get("printerMAC"))
            if dev is None:
                return
            job = None
            if not body.get("printingInProgress"):
                # A printer that is mid-print gets offered nothing — it has not confirmed the job it
                # holds yet, and re-offering it invites a second copy. Otherwise re-offer what it
                # already holds before claiming anything new: a poll response lost on the way must
                # not burn the next job in the queue.
                job = (store.claimed_job(conn, dev["agent_id"])
                       or store.claim_job(conn, dev["agent_id"]))
            if job is not None and job["mode"] != "raw":
                # gotcha #1: no renderer in the printer, so a PDF would come out as blank feed.
                # Fail it here — the dashboard then says why — instead of handing it over.
                # ponytail: one such job per poll; the next poll takes the next one.
                store.finish_job(conn, job["job_id"], dev["agent_id"], False,
                                 "CloudPRNT printers cannot render PDF — send raw Star commands")
                job = None
            return self._json(200, cloudprnt.poll_response(
                job, cloudprnt.media_type(self.headers.get("Accept"))))

        def _cloudprnt_payload(self, path_key):
            q = parse_qs(urlparse(self.path).query)
            dev = self._cloudprnt_device(path_key, (q.get("mac") or [""])[0])
            if dev is None:
                return
            media = (q.get("type") or [""])[0] or cloudprnt.media_type(self.headers.get("Accept"))
            if media not in cloudprnt.MEDIA_TYPES:
                # We hand over the submitted bytes unchanged, so a type we cannot honestly label
                # them with is their 415, not a silent mislabel.
                return self._json(415, {"error": f"cannot serve {media}"})
            job = store.claimed_job(conn, dev["agent_id"])
            token = (q.get("token") or [""])[0]
            if job is None or (token and token != str(job["job_id"])):
                return self._json(404, {"error": "no job"})     # their "no data available"
            data = (store.get_payload(conn, job["job_id"], dev["agent_id"]) or b"")
            copies = job["copies"]       # nothing in this protocol counts copies — repeat the stream
            self.send_response(200)
            self.send_header("Content-Type", media)
            self.send_header("Content-Length", str(len(data) * copies))
            self.end_headers()
            for _ in range(copies):      # written in passes, so 100 copies of a 32 MB job is not 3 GB of RAM
                self.wfile.write(data)

        def _cloudprnt_confirm(self, path_key):
            q = parse_qs(urlparse(self.path).query)
            dev = self._cloudprnt_device(path_key, (q.get("mac") or [""])[0])
            if dev is None:
                return
            job = store.claimed_job(conn, dev["agent_id"])
            if job is not None:
                code = (q.get("code") or [""])[0]
                ok = cloudprnt.job_ok(code)
                store.finish_job(conn, job["job_id"], dev["agent_id"], ok,
                                 None if ok else f"printer reported {cloudprnt.status_text(code)}")
            # ponytail: no `deleteMethod` — the printer confirms with DELETE, which is the default.
            # Offer the GET form if a deployment ever sits behind a proxy that blocks DELETE.
            return self._empty(200)                             # their DELETE answer: 200, no body

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
            if self._is_basic() and self._printnode_get():
                return
            mc = _CLOUDPRNT.match(self.path.split("?", 1)[0])
            if mc:
                return self._cloudprnt_payload(mc.group(1))
            if self.path in ("/", "/index.html"):
                return None if self._serve_dashboard() else self._json(404, {"error": "not found"})
            if self.path == "/health":
                # Unauthenticated on purpose: the sign-in screen reads it to decide whether to
                # offer "create an account" and "forgot password" at all. Both are booleans about
                # the server's configuration, not about any account.
                return self._json(200, {"ok": True,
                                        "signup": "open" if signup_open else "closed",
                                        "password_reset": reset_enabled})
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
            if self.path == "/me":
                p = self._principal()
                if p is None:
                    return self._json(401, {"error": "unauthorized"})
                me = {"kind": p["kind"], "org_id": p["org_id"]}
                if p["kind"] == "session":
                    me.update(email=p["email"], user_id=p["user_id"])
                return self._json(200, me)
            if self.path == "/plans":
                # The catalogue, with each checkout link already pointed at the caller's own org
                # so the provider hands that id back in its webhook. [] = billing not configured.
                p = self._principal()
                if p is None:
                    return self._json(401, {"error": "unauthorized"})
                org = store.get_org(conn, p["org_id"]) if p["org_id"] else None
                return self._json(200, {
                    "plans": [dict(pl, checkout_url=billing.checkout_url(pl, p["org_id"]))
                              for pl in plans],
                    "current": org["plan"] if org else None})
            if self.path == "/apikeys":
                p = self._manager()
                return p and self._json(200, {"keys": store.list_api_keys(conn,
                                                                          org_id=p["org_id"])})
            if self.path == "/users":
                p = self._manager()
                return p and self._json(200, {"users": store.list_users(conn, org_id=p["org_id"])})
            if self.path == "/orgs":
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                return self._json(200, {"orgs": store.list_orgs(conn)})
            mo = _ORG_ID.match(self.path)
            if mo:
                # One org's own settings, readable by a session so the dashboard can show them —
                # the root-only GET /orgs above stays the cross-org listing.
                p = self._manager()
                if p is None:
                    return
                oid = int(mo.group(1))
                org = None if self._foreign_org(p, oid) else store.get_org(conn, oid)
                if org is None:
                    return self._json(404, {"error": "not found"})
                return self._json(200, {
                    "id": org["id"], "name": org["name"], "event_url": org["event_url"],
                    "shopify_secret_set": bool(org["shopify_secret"]),   # never echo the secret
                    "job_quota": org["job_quota"], "plan": org["plan"],
                    "jobs_this_month": store.org_usage(conn, oid),
                    "created_at": org["created_at"]})
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
            if self._is_basic() and self._printnode_post():
                return
            mc = _CLOUDPRNT.match(self.path.split("?", 1)[0])
            if mc:
                return self._cloudprnt_poll(mc.group(1))
            if self.path == "/jobs":
                p = self._principal()
                if p is None:
                    return self._json(401, {"error": "unauthorized"})
                org = p["org_id"]
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                try:
                    jid = self._submit_job(body, org, user_id=p["user_id"])
                except FetchError as e:
                    return self._json(502, {"error": f"downstream: {e}"})
                except store.QuotaExceeded as e:
                    return self._json(402, {"error": str(e)})
                except (DispatchError, store.UnknownPrinter) as e:
                    return self._json(400, {"error": str(e)})
                return self._json(200, {"job_id": jid})
            if self.path == "/orders":
                p = self._principal()
                if p is None:
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                return self._enqueue_order(body.get("order"), body.get("format"), body,
                                           p["org_id"], user_id=p["user_id"])
            if self.path.split("?", 1)[0] == "/integrations/shopify/orders":
                return self._shopify_webhook()
            if self.path == "/billing/webhook":
                return self._billing_webhook()
            if self.path == "/login":
                return self._login()
            if self.path == "/signup":
                return self._signup()
            if self.path == "/password/reset":
                return self._password_reset()
            if self.path == "/password/reset/confirm":
                return self._password_reset_confirm()
            if self.path == "/logout":
                p = self._principal()
                if p is None or p["kind"] != "session":
                    return self._json(401, {"error": "unauthorized"})
                store.delete_session(conn, self._presented_key())
                return self._json(200, {"ok": True})
            if self.path == "/users":
                p = self._manager()
                if p is None:
                    return
                if p["org_id"] is None:      # root belongs to no org — it must name one
                    return self._json(400, {"error": "root has no org; use POST /orgs/{id}/users"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                return self._create_user(p["org_id"], body)
            mu = _ORG_USERS.match(self.path)
            if mu:
                if not self._admin_ok():     # seeding an org's first user is root's job
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                oid = int(mu.group(1))
                if not store.org_exists(conn, oid):
                    return self._json(404, {"error": "not found"})
                return self._create_user(oid, body)
            if self.path == "/apikeys":
                p = self._manager()
                if p is None:
                    return
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                label = (body.get("label") or "client").strip() or "client"
                org_id = body.get("org_id") or p["org_id"] or store.DEFAULT_ORG
                if self._foreign_org(p, org_id):
                    return self._json(400, {"error": "org_id not allowed"})
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
            if self.path == "/me/password":
                p = self._principal()
                if p is None or p["kind"] != "session":
                    return self._json(401, {"error": "unauthorized"})
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                user = store.get_user_by_email(conn, p["email"])
                if not auth.verify_password(body.get("current"), user["password_hash"]):
                    return self._json(401, {"error": "current password does not match"})
                try:
                    pw_hash = auth.hash_password(body.get("new"))
                except ValueError as e:
                    return self._json(400, {"error": str(e)})
                # Drops every session of this user, including the one making the call — a stolen
                # session dies with the password it was minted from.
                store.set_user_password(conn, p["user_id"], pw_hash)
                return self._json(200, {"ok": True})
            mo = _ORG_ID.match(self.path)
            if mo:
                p = self._manager()
                if p is None:
                    return
                try:
                    body = self._read_json()
                except ValueError:
                    return self._json(400, {"error": "bad json"})
                oid, applied = int(mo.group(1)), {}
                if self._foreign_org(p, oid):
                    return self._json(404, {"error": "not found"})
                if "event_url" in body:
                    try:
                        # Same http(s) check as a job's callback_url; null/"" clears the URL.
                        url = parse_callback_url({"callback_url": body.get("event_url")})
                    except DispatchError as e:
                        return self._json(400,
                                          {"error": str(e).replace("callback_url", "event_url")})
                    if not store.set_org_event_url(conn, oid, url):
                        return self._json(404, {"error": "not found"})
                    applied["event_url"] = url
                if "job_quota" in body:
                    # The one org field a session may *not* set: a tenant that could raise its own
                    # cap has no cap. Only the operator's bootstrap token writes it.
                    if p["kind"] != "root":
                        return self._json(403,
                                          {"error": "job_quota is set by the server operator"})
                    quota = body.get("job_quota")
                    if quota is not None and (isinstance(quota, bool)
                                              or not isinstance(quota, int) or quota < 0):
                        return self._json(
                            400, {"error": "job_quota must be a non-negative integer or null"})
                    if not store.set_org_quota(conn, oid, quota):
                        return self._json(404, {"error": "not found"})
                    applied["job_quota"] = quota
                if "plan" in body:
                    # Same rule as the quota it grants: the operator (or a signed billing event)
                    # moves an org between plans, never the org itself.
                    if p["kind"] != "root":
                        return self._json(403, {"error": "plan is set by billing, not by the org"})
                    plan = billing.find(plans, body.get("plan"))
                    if plan is None:
                        return self._json(400, {"error": f"unknown plan: {body.get('plan')!r}"})
                    if not store.set_org_plan(conn, oid, plan["id"], plan["jobs"]):
                        return self._json(404, {"error": "not found"})
                    applied["plan"], applied["job_quota"] = plan["id"], plan["jobs"]
                if "shopify_secret" in body:
                    secret = body.get("shopify_secret")     # null clears it
                    if secret is not None and not (isinstance(secret, str) and secret.strip()):
                        return self._json(400, {"error": "shopify_secret must be a string or null"})
                    if not store.set_org_shopify_secret(conn, oid, secret):
                        return self._json(404, {"error": "not found"})
                    applied["shopify_secret_set"] = secret is not None   # never echo the secret
                if not applied:
                    if not store.org_exists(conn, oid):
                        return self._json(404, {"error": "not found"})
                    return self._json(400, {"error": "nothing to update"})
                return self._json(200, {"ok": True, **applied})
            self._json(404, {"error": "not found"})

        def do_DELETE(self):
            if self._is_basic() and self._printnode_delete():
                return
            mc = _CLOUDPRNT.match(self.path.split("?", 1)[0])
            if mc:
                return self._cloudprnt_confirm(mc.group(1))
            m = _APIKEY_ID.match(self.path)
            if m:
                p = self._manager()
                if p is None:
                    return
                ok = store.revoke_api_key(conn, int(m.group(1)), org_id=p["org_id"])
                return self._json(200, {"ok": True}) if ok else self._json(404, {"error": "not found"})
            mu = _USER_ID.match(self.path)
            if mu:
                p = self._manager()
                if p is None:
                    return
                uid = int(mu.group(1))
                if p["user_id"] == uid:
                    # Removing yourself would sign you out mid-request and, in a two-admin org,
                    # is the mistake nobody can undo from the dashboard. Someone else does it.
                    return self._json(400, {"error": "cannot remove your own account"})
                res = store.delete_user(conn, uid, org_id=p["org_id"])
                if res == "deleted":
                    return self._json(200, {"ok": True})
                if res == "last_user":
                    return self._json(400, {"error": "an org must keep at least one account"})
                return self._json(404, {"error": "not found"})
            mo = _ORG_ID.match(self.path)
            if mo:
                # Root only, and never DEFAULT_ORG: an org holds other people's print history, and
                # the default one is where an agent with an unknown key still lands.
                if not self._admin_ok():
                    return self._json(401, {"error": "unauthorized"})
                oid = int(mo.group(1))
                if oid == store.DEFAULT_ORG:
                    return self._json(400, {"error": "the default org cannot be removed"})
                if not store.delete_org(conn, oid):
                    return self._json(404, {"error": "not found"})
                return self._json(200, {"ok": True})
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
                store.expire_jobs(conn)
                store.requeue_stale(conn, timeout_s, max_retries)
                store.purge_expired_sessions(conn)
                store.purge_expired_resets(conn)
            except Exception as e:
                print(f"reaper error: {e}", file=sys.stderr)
            time.sleep(interval_s)
    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def main():
    token = os.environ["PRINTAPI_TOKEN"]
    if not token.strip():
        # An empty bootstrap token would compare_digest-equal every missing Authorization header,
        # making the whole API root-writable.
        raise SystemExit("PRINTAPI_TOKEN must not be empty")
    db_path = os.environ.get("PRINT_DB", "printpapi.db")
    port = int(os.environ.get("PRINT_PORT", "3460"))
    conn = store.connect(db_path)
    store.init_db(conn)
    start_reaper(conn)
    start_webhook_dispatcher(conn)
    # PRINTAPI_PLANS is the catalogue itself (a JSON array) or a path to a file holding one, so a
    # compose file can mount it instead of squeezing JSON into an env var.
    raw_plans = os.environ.get("PRINTAPI_PLANS", "").strip()
    if raw_plans and not raw_plans.startswith("["):
        raw_plans = Path(raw_plans).read_text(encoding="utf-8")
    httpd = create_server(conn, token, host="0.0.0.0", port=port,
                          signup=os.environ.get("PRINTAPI_SIGNUP", "closed"),
                          public_url=os.environ.get("PUBLIC_URL"),
                          plans=billing.load_plans(raw_plans),
                          billing_secret=os.environ.get("PRINTAPI_BILLING_SECRET"))
    print(f"printpapi listening on :{port}")
    httpd.serve_forever()


if __name__ == "__main__":
    main()
