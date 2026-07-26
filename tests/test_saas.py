"""The hosted-service plumbing: self-signup, password reset by e-mail, removing an account,
reading/writing org settings from a session, and monthly job quotas."""
import base64, json, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer
from app import store, server

PW = "hunter2hunter2"


def _serve(conn, token="t", **kw):
    kw.setdefault("send_mail", _Mailbox())
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3,
                                  poll_interval=0.05, **kw)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}", kw["send_mail"]


class _Mailbox:
    """Stands in for app.mail.send — same (to, subject, body) call, kept in a list."""

    def __init__(self):
        self.sent = []

    def __call__(self, to, subject, body):
        self.sent.append({"to": to, "subject": subject, "body": body})
        return True


def _mem():
    conn = store.connect(":memory:")
    store.init_db(conn)
    return conn


def _req(method, url, token=None, body=None, basic=None, raw=None):
    data = json.dumps(body).encode() if body is not None else raw
    r = urllib.request.Request(url, data=data, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    if basic:
        r.add_header("Authorization",
                     "Basic " + base64.b64encode(f"{basic}:".encode()).decode())
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def _account(base, org_id=store.DEFAULT_ORG, email="ops@shop.test"):
    assert _req("POST", base + f"/orgs/{org_id}/users", token="t",
                body={"email": email, "password": PW})[0] == 200
    code, body = _req("POST", base + "/login", body={"email": email, "password": PW})
    assert code == 200
    return body["token"]


# --- self-signup ------------------------------------------------------------------------------


def test_signup_is_closed_unless_the_operator_opens_it():
    conn = _mem()
    httpd, base, _ = _serve(conn)
    try:
        code, body = _req("POST", base + "/signup",
                          body={"email": "new@shop.test", "password": PW})
        assert code == 403 and "disabled" in body["error"]
        assert store.get_user_by_email(conn, "new@shop.test") is None
        assert _req("GET", base + "/health")[1]["signup"] == "closed"
    finally:
        httpd.shutdown()


def test_open_signup_creates_an_org_and_signs_the_new_owner_in():
    conn = _mem()
    httpd, base, _ = _serve(conn, signup="open")
    try:
        assert _req("GET", base + "/health")[1]["signup"] == "open"
        code, body = _req("POST", base + "/signup",
                          body={"email": "Owner@Shop.test", "password": PW,
                                "org_name": "Shop GmbH"})
        assert code == 200 and body["email"] == "owner@shop.test"
        assert body["org_id"] != store.DEFAULT_ORG and body["token"].startswith("sess_")
        code, me = _req("GET", base + "/me", token=body["token"])
        assert code == 200 and me["org_id"] == body["org_id"] and me["kind"] == "session"
        # The new org starts empty — it sees none of the default org's things.
        assert _req("GET", base + "/printers", token=body["token"])[1]["printers"] == []
        assert store.get_org(conn, body["org_id"])["name"] == "Shop GmbH"
    finally:
        httpd.shutdown()


def test_signup_rejects_a_taken_address_a_bad_address_and_a_short_password():
    conn = _mem()
    httpd, base, _ = _serve(conn, signup="open")
    try:
        assert _req("POST", base + "/signup", body={"email": "a@shop.test", "password": PW})[0] == 200
        orgs = len(store.list_orgs(conn))
        assert _req("POST", base + "/signup",
                    body={"email": "a@shop.test", "password": PW})[0] == 409
        assert _req("POST", base + "/signup", body={"email": "nope", "password": PW})[0] == 400
        assert _req("POST", base + "/signup",
                    body={"email": "b@shop.test", "password": "short"})[0] == 400
        assert len(store.list_orgs(conn)) == orgs        # no half-made orgs left behind
    finally:
        httpd.shutdown()


def test_signup_is_throttled_per_client_so_one_host_cannot_spray_orgs():
    conn = _mem()
    httpd, base, _ = _serve(conn, signup="open", max_login_fails=2)
    try:
        for n in range(2):
            assert _req("POST", base + "/signup",
                        body={"email": f"u{n}@shop.test", "password": PW})[0] == 200
        code, body = _req("POST", base + "/signup",
                          body={"email": "u3@shop.test", "password": PW})
        assert code == 429 and store.get_user_by_email(conn, "u3@shop.test") is None
    finally:
        httpd.shutdown()


# --- password reset ---------------------------------------------------------------------------


def test_a_reset_mails_a_token_that_sets_a_new_password_exactly_once():
    conn = _mem()
    httpd, base, box = _serve(conn)
    try:
        session = _account(base)
        assert _req("POST", base + "/password/reset", body={"email": "ops@shop.test"})[0] == 200
        assert box.sent[0]["to"] == "ops@shop.test"
        token = box.sent[0]["body"].split("token: ")[1].split()[0]
        code, _ = _req("POST", base + "/password/reset/confirm",
                       body={"token": token, "password": "brandnewpassword"})
        assert code == 200
        assert _req("GET", base + "/me", token=session)[0] == 401      # old session is dead
        assert _req("POST", base + "/login",
                    body={"email": "ops@shop.test", "password": PW})[0] == 401
        assert _req("POST", base + "/login",
                    body={"email": "ops@shop.test", "password": "brandnewpassword"})[0] == 200
        # Spent — a leaked mail cannot be replayed.
        assert _req("POST", base + "/password/reset/confirm",
                    body={"token": token, "password": "yetanotherpassword"})[0] == 400
    finally:
        httpd.shutdown()


def test_a_reset_for_an_unknown_address_answers_the_same_and_mails_nothing():
    conn = _mem()
    httpd, base, box = _serve(conn)
    try:
        _account(base)
        known = _req("POST", base + "/password/reset", body={"email": "ops@shop.test"})
        unknown = _req("POST", base + "/password/reset", body={"email": "nobody@shop.test"})
        assert known == unknown == (200, {"ok": True})   # no account enumeration
        assert [m["to"] for m in box.sent] == ["ops@shop.test"]
        assert _req("POST", base + "/password/reset/confirm",
                    body={"token": "sess_invented", "password": PW})[0] == 400
    finally:
        httpd.shutdown()


def test_the_reset_mail_only_links_when_the_operator_declared_a_public_url():
    conn = _mem()
    httpd, base, box = _serve(conn)
    try:
        _account(base)
        _req("POST", base + "/password/reset", body={"email": "ops@shop.test"})
        # No PUBLIC_URL: the Host header is attacker-controlled, so the mail carries the bare
        # token instead of a link built from it.
        assert "http" not in box.sent[0]["body"]
    finally:
        httpd.shutdown()
    httpd, base, box = _serve(conn, public_url="https://print.shop.test")
    try:
        _req("POST", base + "/password/reset", body={"email": "ops@shop.test"})
        assert "https://print.shop.test/?reset=" in box.sent[-1]["body"]
    finally:
        httpd.shutdown()


def test_reset_requests_are_throttled_per_address():
    conn = _mem()
    httpd, base, box = _serve(conn, max_login_fails=2)
    try:
        _account(base)
        for _ in range(2):
            assert _req("POST", base + "/password/reset",
                        body={"email": "ops@shop.test"})[0] == 200
        assert _req("POST", base + "/password/reset",
                    body={"email": "ops@shop.test"})[0] == 429      # no mail-bombing an inbox
        assert len(box.sent) == 2
    finally:
        httpd.shutdown()


# --- removing an account ----------------------------------------------------------------------


def test_a_session_removes_a_colleague_but_not_itself_and_not_a_foreign_user():
    conn = _mem()
    other = store.create_org(conn, "other")
    httpd, base, _ = _serve(conn)
    try:
        session = _account(base)
        code, mate = _req("POST", base + "/users", token=session,
                          body={"email": "mate@shop.test", "password": PW})
        assert code == 200
        code, stranger = _req("POST", base + f"/orgs/{other}/users", token="t",
                              body={"email": "x@other.test", "password": PW})
        assert code == 200
        me = _req("GET", base + "/me", token=session)[1]["user_id"]
        assert _req("DELETE", base + f"/users/{stranger['id']}", token=session)[0] == 404
        assert _req("DELETE", base + f"/users/{me}", token=session)[0] == 400
        assert _req("DELETE", base + f"/users/{mate['id']}", token=session)[0] == 200
        assert [u["email"] for u in _req("GET", base + "/users", token=session)[1]["users"]] \
            == ["ops@shop.test"]
        assert _req("POST", base + "/login",
                    body={"email": "mate@shop.test", "password": PW})[0] == 401
    finally:
        httpd.shutdown()


def test_the_last_account_of_an_org_cannot_be_removed_and_a_machine_key_may_not_remove_anyone():
    conn = _mem()
    httpd, base, _ = _serve(conn)
    try:
        session = _account(base)
        me = _req("GET", base + "/me", token=session)[1]["user_id"]
        code, body = _req("DELETE", base + f"/users/{me}", token="t")   # root, no self-rule
        assert code == 400 and "at least one account" in body["error"]
        key = _req("POST", base + "/apikeys", token="t", body={"label": "shop"})[1]["key"]
        assert _req("DELETE", base + f"/users/{me}", token=key)[0] == 401
    finally:
        httpd.shutdown()


# --- org settings from a session --------------------------------------------------------------


def test_a_session_reads_and_writes_its_own_org_settings_but_no_other():
    conn = _mem()
    other = store.create_org(conn, "other")
    httpd, base, _ = _serve(conn)
    try:
        session = _account(base)
        code, body = _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token=session,
                          body={"event_url": "https://hooks.shop.test/agents",
                                "shopify_secret": "shhh"})
        assert code == 200
        code, org = _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token=session)
        assert code == 200
        assert org["event_url"] == "https://hooks.shop.test/agents"
        assert org["shopify_secret_set"] is True and "shhh" not in json.dumps(org)
        assert org["job_quota"] is None and org["jobs_this_month"] == 0
        assert _req("GET", base + f"/orgs/{other}", token=session)[0] == 404
        assert _req("GET", base + f"/orgs/{other}", token="t")[0] == 200      # root sees every org
        assert _req("GET", base + "/orgs/9999", token="t")[0] == 404
    finally:
        httpd.shutdown()


def test_only_the_operator_sets_a_quota():
    conn = _mem()
    httpd, base, _ = _serve(conn)
    try:
        session = _account(base)
        code, body = _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token=session,
                          body={"job_quota": 100000})
        assert code == 403 and "operator" in body["error"]
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"job_quota": "lots"})[0] == 400
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"job_quota": 5})[0] == 200
        assert _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token=session)[1]["job_quota"] == 5
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"job_quota": None})[0] == 200
        assert _req("GET", base + f"/orgs/{store.DEFAULT_ORG}",
                    token=session)[1]["job_quota"] is None
    finally:
        httpd.shutdown()


# --- quotas -----------------------------------------------------------------------------------


def test_a_spent_quota_answers_402_on_every_submit_path():
    conn = _mem()
    pid = store.register_agent(conn, "pc", "agentkey",
                               [{"name": "HP", "can_pdf": True}])["printer_ids"]["HP"]
    httpd, base, _ = _serve(conn)
    try:
        key = _req("POST", base + "/apikeys", token="t", body={"label": "shop"})[1]["key"]
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"job_quota": 1})[0] == 200
        job = {"printer_id": pid, "type": "raw_base64", "content": "eA=="}
        assert _req("POST", base + "/jobs", token=key, body=job)[0] == 200
        code, body = _req("POST", base + "/jobs", token=key, body=job)
        assert code == 402 and "quota" in body["error"]
        order = {"printer_id": pid, "order": {"number": "1001", "items": []}}
        assert _req("POST", base + "/orders", token=key, body=order)[0] == 402
        code, body = _req("POST", base + "/printjobs", basic=key,
                          body={"printerId": pid, "contentType": "raw_base64",
                                "content": "eA==", "source": "t"})
        assert code == 402 and body["code"] == "QuotaExceeded"
        # The counter is per org: another org's budget is untouched.
        assert _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token="t")[1]["jobs_this_month"] == 1
    finally:
        httpd.shutdown()
