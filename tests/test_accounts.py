"""Org accounts: e-mail/password login, browser sessions, and what a session may manage."""
import json, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer
from app import store, server

PW = "hunter2hunter2"


def _serve(conn, token="t", **kw):
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3,
                                  poll_interval=0.05, **kw)
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
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def _login(base, email, password=PW):
    return _req("POST", base + "/login", body={"email": email, "password": password})


def _account(base, org_id=store.DEFAULT_ORG, email="ops@shop.test"):
    """Root creates the org's first user, that user logs in. Returns the session token."""
    code, _ = _req("POST", base + f"/orgs/{org_id}/users", token="t",
                   body={"email": email, "password": PW})
    assert code == 200
    code, body = _login(base, email)
    assert code == 200
    return body["token"]


def test_login_returns_a_session_that_acts_inside_its_own_org():
    conn = _mem()
    store.register_agent(conn, "pc", "agentkey", [{"name": "Z", "can_pdf": False}])
    httpd, base = _serve(conn)
    try:
        code, body = _req("POST", base + "/orgs/1/users", token="t",
                          body={"email": "Ops@Shop.test", "password": PW})
        assert code == 200 and body["email"] == "ops@shop.test"
        code, login = _login(base, "ops@shop.test")
        assert code == 200
        assert login["token"].startswith("sess_") and login["org_id"] == store.DEFAULT_ORG
        assert login["expires_at"] > 0 and "password" not in json.dumps(login)
        sess = login["token"]
        code, printers = _req("GET", base + "/printers", token=sess)
        assert code == 200 and printers["printers"][0]["name"] == "Z"
        code, me = _req("GET", base + "/me", token=sess)
        assert code == 200 and me == {"kind": "session", "org_id": store.DEFAULT_ORG,
                                      "email": "ops@shop.test", "user_id": login["user_id"]}
    finally:
        httpd.shutdown()


def test_a_bad_password_unknown_user_and_junk_body_are_all_401_or_400():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        _account(base)
        assert _login(base, "ops@shop.test", "wrong password")[0] == 401
        assert _login(base, "nobody@shop.test")[0] == 401       # no user-enumeration difference
        assert _req("POST", base + "/login", body={"email": "ops@shop.test"})[0] == 401
        assert _req("POST", base + "/login", body={})[0] == 401
    finally:
        httpd.shutdown()


def test_repeated_failures_are_throttled_per_email():
    conn = _mem()
    httpd, base = _serve(conn, max_login_fails=3)
    try:
        _account(base)
        _account(base, email="other@shop.test")
        for _ in range(3):
            assert _login(base, "ops@shop.test", "wrong password")[0] == 401
        assert _login(base, "ops@shop.test", "wrong password")[0] == 429
        assert _login(base, "ops@shop.test")[0] == 429           # even the right password waits
        assert _login(base, "other@shop.test")[0] == 200         # other accounts unaffected
    finally:
        httpd.shutdown()


def test_a_successful_login_clears_the_failure_count():
    conn = _mem()
    httpd, base = _serve(conn, max_login_fails=3)
    try:
        _account(base)
        assert _login(base, "ops@shop.test", "wrong password")[0] == 401
        assert _login(base, "ops@shop.test", "wrong password")[0] == 401
        assert _login(base, "ops@shop.test")[0] == 200
        assert _login(base, "ops@shop.test", "wrong password")[0] == 401
        assert _login(base, "ops@shop.test")[0] == 200           # counter reset by the success
    finally:
        httpd.shutdown()


def test_an_expired_session_stops_authorizing():
    conn = _mem()
    httpd, base = _serve(conn, session_ttl_s=0)
    try:
        sess = _account(base)
        assert _req("GET", base + "/printers", token=sess)[0] == 401
    finally:
        httpd.shutdown()


def test_logout_kills_the_session():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        sess = _account(base)
        assert _req("POST", base + "/logout", token=sess) == (200, {"ok": True})
        assert _req("GET", base + "/printers", token=sess)[0] == 401
        assert _req("POST", base + "/logout", token=sess)[0] == 401
    finally:
        httpd.shutdown()


def test_me_tells_root_session_and_machine_key_apart():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, key = _req("POST", base + "/apikeys", token="t", body={"label": "n8n"})
        assert code == 200
        assert _req("GET", base + "/me", token="t")[1] == {"kind": "root", "org_id": None}
        assert _req("GET", base + "/me", token=key["key"])[1] == {
            "kind": "key", "org_id": store.DEFAULT_ORG}
        assert _req("GET", base + "/me", token="nope")[0] == 401
    finally:
        httpd.shutdown()


def test_a_session_manages_only_its_own_orgs_keys():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, other = _req("POST", base + "/orgs", token="t", body={"name": "other"})
        assert code == 200
        _, foreign = _req("POST", base + "/apikeys", token="t",
                          body={"label": "theirs", "org_id": other["id"]})
        sess = _account(base)
        code, mine = _req("POST", base + "/apikeys", token=sess, body={"label": "mine"})
        assert code == 200 and mine["org_id"] == store.DEFAULT_ORG and mine["key"]
        code, listing = _req("GET", base + "/apikeys", token=sess)
        assert code == 200 and [k["label"] for k in listing["keys"]] == ["mine"]
        # a foreign key is invisible and unrevokable — 404, never 403
        assert _req("DELETE", base + f"/apikeys/{foreign['id']}", token=sess)[0] == 404
        assert _req("DELETE", base + f"/apikeys/{mine['id']}", token=sess)[0] == 200
        assert _req("POST", base + "/apikeys", token=sess,
                    body={"label": "x", "org_id": other["id"]})[0] == 400
        # root still sees every org
        assert len(_req("GET", base + "/apikeys", token="t")[1]["keys"]) == 2
    finally:
        httpd.shutdown()


def test_a_machine_key_can_print_but_cannot_manage_the_org():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        _, key = _req("POST", base + "/apikeys", token="t", body={"label": "n8n"})
        k = key["key"]
        assert _req("GET", base + "/printers", token=k)[0] == 200          # printing path intact
        assert _req("GET", base + "/apikeys", token=k)[0] == 401
        assert _req("POST", base + "/apikeys", token=k, body={"label": "self-issued"})[0] == 401
        assert _req("DELETE", base + f"/apikeys/{key['id']}", token=k)[0] == 401
        assert _req("GET", base + "/users", token=k)[0] == 401
        assert _req("POST", base + "/users", token=k,
                    body={"email": "x@y.z", "password": PW})[0] == 401
        assert _req("PUT", base + "/orgs/1", token=k,
                    body={"event_url": "https://h.example/x"})[0] == 401
        assert _req("PUT", base + "/me/password", token=k,
                    body={"current": PW, "new": "another password"})[0] == 401
    finally:
        httpd.shutdown()


def test_a_session_invites_further_users_into_its_own_org():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        sess = _account(base)
        code, u = _req("POST", base + "/users", token=sess,
                       body={"email": "packer@shop.test", "password": PW})
        assert code == 200 and u["org_id"] == store.DEFAULT_ORG
        code, listing = _req("GET", base + "/users", token=sess)
        assert code == 200 and [x["email"] for x in listing["users"]] == [
            "ops@shop.test", "packer@shop.test"]
        assert "password_hash" not in json.dumps(listing)
        assert _login(base, "packer@shop.test")[0] == 200
        # taken e-mail, weak password, missing fields
        assert _req("POST", base + "/users", token=sess,
                    body={"email": "packer@shop.test", "password": PW})[0] == 409
        assert _req("POST", base + "/users", token=sess,
                    body={"email": "new@shop.test", "password": "short"})[0] == 400
        assert _req("POST", base + "/users", token=sess, body={"email": "new@shop.test"})[0] == 400
        assert _req("POST", base + "/users", token=sess, body={"password": PW})[0] == 400
    finally:
        httpd.shutdown()


def test_root_creates_the_first_user_of_any_org_but_has_no_org_of_its_own():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        _, other = _req("POST", base + "/orgs", token="t", body={"name": "other"})
        code, u = _req("POST", base + f"/orgs/{other['id']}/users", token="t",
                       body={"email": "them@other.test", "password": PW})
        assert code == 200 and u["org_id"] == other["id"]
        assert _req("POST", base + "/orgs/4242/users", token="t",
                    body={"email": "x@y.z", "password": PW})[0] == 404
        assert _req("POST", base + "/users", token="t",
                    body={"email": "x@y.z", "password": PW})[0] == 400   # root belongs to no org
        assert len(_req("GET", base + "/users", token="t")[1]["users"]) == 1   # every org
        code, login = _login(base, "them@other.test")
        assert code == 200 and login["org_id"] == other["id"]
    finally:
        httpd.shutdown()


def test_a_session_of_one_org_cannot_touch_another_orgs_data():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        _, other = _req("POST", base + "/orgs", token="t", body={"name": "other"})
        reg = store.register_agent(conn, "theirs", "ak", [{"name": "Z", "can_pdf": False}],
                                   org_id=other["id"])
        foreign_job = store.enqueue_job(conn, reg["printer_ids"]["Z"], "raw_base64", "raw", b"x",
                                        org_id=other["id"])
        _req("POST", base + f"/orgs/{other['id']}/users", token="t",
             body={"email": "them@other.test", "password": PW})
        sess = _account(base)
        assert _req("GET", base + f"/jobs/{foreign_job}", token=sess)[0] == 404
        assert _req("GET", base + "/printers", token=sess)[1]["printers"] == []
        assert _req("GET", base + "/computers", token=sess)[1]["computers"] == []
        assert _req("PUT", base + f"/orgs/{other['id']}", token=sess,
                    body={"event_url": "https://evil.example/x"})[0] == 404
        assert [u["email"] for u in _req("GET", base + "/users", token=sess)[1]["users"]] == [
            "ops@shop.test"]                          # the other org's user stays invisible
    finally:
        httpd.shutdown()


def test_a_session_configures_its_own_org():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        sess = _account(base)
        code, body = _req("PUT", base + "/orgs/1", token=sess,
                          body={"event_url": "https://hooks.example/x", "shopify_secret": "s3cr3t"})
        assert code == 200 and body["event_url"] == "https://hooks.example/x"
        assert body["shopify_secret_set"] is True and "s3cr3t" not in json.dumps(body)
        assert store.get_org(conn, store.DEFAULT_ORG)["shopify_secret"] == "s3cr3t"
    finally:
        httpd.shutdown()


def test_a_password_change_needs_the_old_one_and_logs_the_browsers_out():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        sess = _account(base)
        assert _req("PUT", base + "/me/password", token=sess,
                    body={"current": "wrong password", "new": "brand new password"})[0] == 401
        assert _req("PUT", base + "/me/password", token=sess,
                    body={"current": PW, "new": "short"})[0] == 400
        assert _req("PUT", base + "/me/password", token=sess,
                    body={"current": PW, "new": "brand new password"}) == (200, {"ok": True})
        assert _req("GET", base + "/printers", token=sess)[0] == 401     # old session invalidated
        assert _login(base, "ops@shop.test")[0] == 401                   # old password gone
        assert _login(base, "ops@shop.test", "brand new password")[0] == 200
    finally:
        httpd.shutdown()


def test_a_job_submitted_from_a_session_records_who_printed_it():
    conn = _mem()
    reg = store.register_agent(conn, "pc", "ak", [{"name": "Z", "can_pdf": False}])
    httpd, base = _serve(conn)
    try:
        sess = _account(base)
        code, login = _login(base, "ops@shop.test")
        code, job = _req("POST", base + "/jobs", token=sess,
                         body={"printer_id": reg["printer_ids"]["Z"], "type": "raw_base64",
                               "content": "QUJD"})
        assert code == 200
        row = conn.execute("SELECT user_id FROM jobs WHERE id=?", (job["job_id"],)).fetchone()
        assert row["user_id"] == login["user_id"]
    finally:
        httpd.shutdown()
