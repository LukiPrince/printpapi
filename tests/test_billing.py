"""Billing: a plan catalogue the operator configures, a signed webhook that moves an org onto a
plan (and its quota with it), and org deletion for the tenant that leaves."""
import hashlib, hmac, json, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer
import pytest
from app import billing, server, store

PW = "hunter2hunter2"
SECRET = "whsec_test"
PLANS_JSON = json.dumps([
    {"id": "free", "name": "Free", "jobs": 50, "price": "0 EUR"},
    {"id": "pro", "name": "Pro", "jobs": 5000, "price": "9 EUR/mo",
     "checkout_url": "https://pay.example/pro?client_reference_id={org}"},
    {"id": "unlimited", "name": "Unlimited", "jobs": None, "price": "29 EUR/mo"},
])


def _mem():
    conn = store.connect(":memory:")
    store.init_db(conn)
    return conn


def _serve(conn, token="t", plans=PLANS_JSON, billing_secret=SECRET, **kw):
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3, poll_interval=0.05,
                                  plans=billing.load_plans(plans), billing_secret=billing_secret,
                                  **kw)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}"


def _req(method, url, token=None, body=None, raw=None, headers=None):
    data = json.dumps(body).encode() if body is not None else raw
    r = urllib.request.Request(url, data=data, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    for k, v in (headers or {}).items():
        r.add_header(k, v)
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def _account(base, org_id=store.DEFAULT_ORG, email="ops@shop.test"):
    assert _req("POST", base + f"/orgs/{org_id}/users", token="t",
                body={"email": email, "password": PW})[0] == 200
    return _req("POST", base + "/login", body={"email": email, "password": PW})[1]["token"]


def _signed(base, event, secret=SECRET, path="/billing/webhook"):
    raw = json.dumps(event).encode()
    sig = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return _req("POST", base + path, raw=raw, headers={"X-Signature": f"sha256={sig}"})


# --- the catalogue (pure) ---------------------------------------------------------------------


def test_plans_parse_with_defaults_and_reject_junk():
    plans = billing.load_plans(PLANS_JSON)
    assert [p["id"] for p in plans] == ["free", "pro", "unlimited"]
    assert plans[0]["jobs"] == 50 and plans[2]["jobs"] is None       # null = unlimited
    assert billing.default_plan(plans)["id"] == "free"               # first = what a new org gets
    assert billing.find(plans, "pro")["name"] == "Pro"
    assert billing.find(plans, "gold") is None
    assert billing.load_plans("") == [] and billing.load_plans(None) == []
    for bad in ("{", "[]", '[{"name": "no id"}]', '[{"id": "a"}, {"id": "a"}]',
                '[{"id": "a", "jobs": -1}]', '[{"id": "a", "jobs": "many"}]', '"nope"'):
        with pytest.raises(billing.BillingError):
            billing.load_plans(bad)


def test_the_checkout_link_carries_the_org_so_the_webhook_can_name_it():
    pro = billing.find(billing.load_plans(PLANS_JSON), "pro")
    assert billing.checkout_url(pro, 7) == "https://pay.example/pro?client_reference_id=7"
    assert billing.checkout_url(pro, None).endswith("{org}")         # root has no org to fill in
    assert billing.checkout_url({"checkout_url": None}, 7) is None


def test_a_signature_only_verifies_over_the_exact_body():
    raw = b'{"org_id": 1, "plan": "pro"}'
    sig = hmac.new(SECRET.encode(), raw, hashlib.sha256).hexdigest()
    assert billing.verify(SECRET, raw, sig)
    assert billing.verify(SECRET, raw, f"sha256={sig.upper()}")      # providers differ on both
    assert not billing.verify(SECRET, raw + b" ", sig)
    assert not billing.verify("other", raw, sig)
    assert not billing.verify(SECRET, raw, "")
    assert not billing.verify("", raw, sig)                          # no secret, no trust


# --- the webhook -------------------------------------------------------------------------------


def test_a_signed_event_moves_the_org_onto_the_plan_and_its_quota():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        session = _account(base)
        code, body = _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "pro",
                                    "status": "active"})
        assert code == 200 and body["plan"] == "pro" and body["job_quota"] == 5000
        code, org = _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token=session)
        assert org["plan"] == "pro" and org["job_quota"] == 5000     # the quota is what bites
    finally:
        httpd.shutdown()


def test_an_event_may_name_the_org_by_the_owner_e_mail():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        session = _account(base)
        assert _signed(base, {"email": "OPS@shop.test", "plan": "unlimited"})[0] == 200
        org = _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token=session)[1]
        assert org["plan"] == "unlimited" and org["job_quota"] is None
        assert _signed(base, {"email": "nobody@shop.test", "plan": "pro"})[0] == 404
        assert _signed(base, {"plan": "pro"})[0] == 400              # neither selector
    finally:
        httpd.shutdown()


def test_a_cancelled_subscription_falls_back_to_the_default_plan():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        session = _account(base)
        assert _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "unlimited"})[0] == 200
        # Any non-active status downgrades — the plan field of a cancellation is the *old* plan.
        code, body = _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "unlimited",
                                    "status": "cancelled"})
        assert code == 200 and body["plan"] == "free"
        org = _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token=session)[1]
        assert org["plan"] == "free" and org["job_quota"] == 50
    finally:
        httpd.shutdown()


def test_a_bad_signature_changes_nothing():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        event = {"org_id": store.DEFAULT_ORG, "plan": "pro"}
        assert _signed(base, event, secret="guessed")[0] == 401
        assert _req("POST", base + "/billing/webhook", body=event)[0] == 401   # unsigned
        assert store.get_org(conn, store.DEFAULT_ORG)["plan"] is None
        assert store.get_org(conn, store.DEFAULT_ORG)["job_quota"] is None
        assert _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "gold"})[0] == 400
        assert _signed(base, {"org_id": 9999, "plan": "pro"})[0] == 404
    finally:
        httpd.shutdown()


def test_billing_is_off_unless_the_operator_configured_it():
    conn = _mem()
    httpd, base = _serve(conn, plans="", billing_secret=None)
    try:
        session = _account(base)
        code, body = _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "pro"})
        assert code == 503 and "not configured" in body["error"]
        assert _req("GET", base + "/plans", token=session)[1]["plans"] == []
    finally:
        httpd.shutdown()


def test_a_plan_quota_is_enforced_on_the_next_job():
    conn = _mem()
    pid = store.register_agent(conn, "pc", "agentkey",
                               [{"name": "HP", "can_pdf": True}])["printer_ids"]["HP"]
    httpd, base = _serve(conn, plans=json.dumps([{"id": "free", "jobs": 0},
                                                 {"id": "pro", "jobs": 10}]))
    try:
        key = _req("POST", base + "/apikeys", token="t", body={"label": "shop"})[1]["key"]
        job = {"printer_id": pid, "type": "raw_base64", "content": "eA=="}
        assert _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "free"})[0] == 200
        assert _req("POST", base + "/jobs", token=key, body=job)[0] == 402
        assert _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "pro"})[0] == 200
        assert _req("POST", base + "/jobs", token=key, body=job)[0] == 200
    finally:
        httpd.shutdown()


# --- the catalogue over HTTP -------------------------------------------------------------------


def test_plans_are_listed_with_the_callers_own_checkout_link():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        session = _account(base)
        assert _req("GET", base + "/plans")[0] == 401           # not a public price list
        code, body = _req("GET", base + "/plans", token=session)
        assert code == 200 and body["current"] is None
        pro = [p for p in body["plans"] if p["id"] == "pro"][0]
        assert pro["checkout_url"].endswith(f"client_reference_id={store.DEFAULT_ORG}")
        assert pro["price"] == "9 EUR/mo"
        assert _signed(base, {"org_id": store.DEFAULT_ORG, "plan": "pro"})[0] == 200
        assert _req("GET", base + "/plans", token=session)[1]["current"] == "pro"
    finally:
        httpd.shutdown()


def test_only_the_operator_moves_an_org_between_plans_by_hand():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        session = _account(base)
        code, body = _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token=session,
                          body={"plan": "unlimited"})
        assert code == 403 and "billing" in body["error"]        # no self-upgrade
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"plan": "gold"})[0] == 400
        code, body = _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                          body={"plan": "pro"})
        assert code == 200 and body["job_quota"] == 5000
        assert _req("GET", base + f"/orgs/{store.DEFAULT_ORG}", token=session)[1]["plan"] == "pro"
    finally:
        httpd.shutdown()


# --- the tenant that leaves --------------------------------------------------------------------


def test_root_deletes_an_org_with_everything_in_it_but_never_the_default_one():
    conn = _mem()
    gone = store.create_org(conn, "leaving")
    store.register_agent(conn, "pc", "agentkey", [{"name": "HP", "can_pdf": True}], org_id=gone)
    httpd, base = _serve(conn)
    try:
        session = _account(base, org_id=gone, email="bye@shop.test")
        key = _req("POST", base + "/apikeys", token="t",
                   body={"label": "shop", "org_id": gone})[1]["key"]
        assert _req("DELETE", base + f"/orgs/{gone}", token=session)[0] == 401   # not the tenant's
        assert _req("DELETE", base + f"/orgs/{store.DEFAULT_ORG}", token="t")[0] == 400
        assert _req("DELETE", base + f"/orgs/{gone}", token="t")[0] == 200
        assert _req("DELETE", base + f"/orgs/{gone}", token="t")[0] == 404       # only once
        assert _req("GET", base + "/me", token=session)[0] == 401                # session died
        assert _req("GET", base + "/printers", token=key)[0] == 401              # key died
        assert [o["id"] for o in store.list_orgs(conn)] == [store.DEFAULT_ORG]
        assert store.get_user_by_email(conn, "bye@shop.test") is None
    finally:
        httpd.shutdown()
