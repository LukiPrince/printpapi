import base64
import hashlib
import hmac
import json
import urllib.error
import urllib.request

from app import store
from tests.test_orders import SHOPIFY
from tests.test_server import _mem, _req, _serve

ORDER = {"number": "1001", "customer": "Jane Doe", "address": ["Musterweg 1", "12345 Berlin"],
         "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80"}],
         "totals": [["Total", "19.80"]]}


def _printers(conn, org_id=store.DEFAULT_ORG):
    """A PDF-capable and a raw-only printer on one agent, returned as (pdf_id, raw_id)."""
    reg = store.register_agent(conn, f"pc-{org_id}", f"agent-{org_id}",
                               [{"name": "HP", "can_pdf": True}, {"name": "Zebra"}],
                               org_id=org_id)
    return reg["printer_ids"]["HP"], reg["printer_ids"]["Zebra"], reg["computer_id"]


def _payload_of(conn, job_id, agent_id):
    store.claim_job(conn, agent_id)
    return store.get_payload(conn, job_id, agent_id)


def test_post_orders_renders_a_packing_slip_and_queues_it():
    conn = _mem()
    pdf_printer, _, agent_id = _printers(conn)
    httpd, base = _serve(conn)
    try:
        code, raw = _req("POST", base + "/orders", token="t",
                         body={"printer_id": pdf_printer, "order": ORDER})
        assert code == 200
        jid = json.loads(raw)["job_id"]
        assert _payload_of(conn, jid, agent_id).startswith(b"%PDF")
        job = store.recent_jobs(conn)[0]
        assert (job["mode"], job["type"], job["title"]) == ("pdf", "order", "Packing slip 1001")
    finally:
        httpd.shutdown()


def test_post_orders_maps_a_store_payload_when_a_format_is_given():
    conn = _mem()
    pdf_printer, _, agent_id = _printers(conn)
    httpd, base = _serve(conn)
    try:
        code, raw = _req("POST", base + "/orders", token="t",
                         body={"printer_id": pdf_printer, "format": "shopify", "order": SHOPIFY})
        assert code == 200
        payload = _payload_of(conn, json.loads(raw)["job_id"], agent_id)
        assert rb"Widget \(blue\)" in payload and b"#1001" in payload   # parens are PDF-escaped
        assert _req("POST", base + "/orders", token="t",
                    body={"printer_id": pdf_printer, "format": "magento",
                          "order": SHOPIFY})[0] == 400
    finally:
        httpd.shutdown()


def test_post_orders_rejects_bad_input():
    conn = _mem()
    pdf_printer, raw_printer, _ = _printers(conn)
    httpd, base = _serve(conn)
    try:
        assert _req("POST", base + "/orders",
                    body={"printer_id": pdf_printer, "order": ORDER})[0] == 401
        # a label printer cannot render a PDF (gotcha #1) — refuse before queueing blanks
        code, raw = _req("POST", base + "/orders", token="t",
                         body={"printer_id": raw_printer, "order": ORDER})
        assert code == 400 and b"raw-only" in raw
        assert _req("POST", base + "/orders", token="t",
                    body={"printer_id": 999, "order": ORDER})[0] == 400
        assert _req("POST", base + "/orders", token="t",
                    body={"printer_id": pdf_printer, "order": {"lines": []}})[0] == 400
    finally:
        httpd.shutdown()


def test_orders_are_org_scoped():
    conn = _mem()
    other = store.create_org(conn, "other")
    store.add_api_key(conn, "other-key", "k-other", org_id=other)
    mine, _, _ = _printers(conn)
    httpd, base = _serve(conn)
    try:
        assert _req("POST", base + "/orders", token="k-other",
                    body={"printer_id": mine, "order": ORDER})[0] == 400   # foreign printer
    finally:
        httpd.shutdown()


def _shopify_post(url, body, secret, header=None, topic="orders/create"):
    data = json.dumps(body).encode()
    digest = base64.b64encode(hmac.new(secret.encode(), data, hashlib.sha256).digest()).decode()
    r = urllib.request.Request(url, data=data, method="POST")
    r.add_header("Content-Type", "application/json")
    r.add_header("X-Shopify-Hmac-Sha256", header or digest)
    r.add_header("X-Shopify-Topic", topic)
    r.add_header("X-Shopify-Shop-Domain", "acme.myshopify.com")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def test_shopify_webhook_verifies_hmac_and_prints_once_per_order():
    conn = _mem()
    store.add_api_key(conn, "shop", "k-shop")
    store.set_org_shopify_secret(conn, store.DEFAULT_ORG, "s3cret")
    pdf_printer, _, agent_id = _printers(conn)
    httpd, base = _serve(conn)
    hook = base + f"/integrations/shopify/orders?key=k-shop&printer_id={pdf_printer}"
    try:
        code, raw = _shopify_post(hook, SHOPIFY, "s3cret")
        assert code == 200
        jid = json.loads(raw)["job_id"]
        payload = _payload_of(conn, jid, agent_id)
        assert payload.startswith(b"%PDF") and b"acme.myshopify.com" in payload  # shop domain
        # Shopify redelivers webhooks; the same order must never print twice
        code, raw = _shopify_post(hook, SHOPIFY, "s3cret")
        assert code == 200 and json.loads(raw)["job_id"] == jid
        assert len(store.recent_jobs(conn)) == 1
        # a forged/altered payload is rejected
        assert _shopify_post(hook, SHOPIFY, "wrong-secret")[0] == 401
        assert _shopify_post(hook, SHOPIFY, "s3cret", header="not-base64")[0] == 401
        assert _shopify_post(base + f"/integrations/shopify/orders?key=nope&printer_id={pdf_printer}",
                             SHOPIFY, "s3cret")[0] == 401
    finally:
        httpd.shutdown()


def test_shopify_webhook_without_a_configured_secret_is_refused():
    conn = _mem()
    store.add_api_key(conn, "shop", "k-shop")
    pdf_printer, _, _ = _printers(conn)
    httpd, base = _serve(conn)
    try:
        code, raw = _shopify_post(
            base + f"/integrations/shopify/orders?key=k-shop&printer_id={pdf_printer}",
            SHOPIFY, "s3cret")
        assert code == 400 and b"shopify_secret" in raw
        # ...and a missing printer_id is a client error, not a traceback
        store.set_org_shopify_secret(conn, store.DEFAULT_ORG, "s3cret")
        assert _shopify_post(base + "/integrations/shopify/orders?key=k-shop",
                             SHOPIFY, "s3cret")[0] == 400
    finally:
        httpd.shutdown()


def test_root_sets_the_shopify_secret_and_it_is_never_echoed_back():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}",
                    body={"shopify_secret": "s3cret"})[0] == 401          # root only
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"shopify_secret": "s3cret"})[0] == 200
        code, raw = _req("GET", base + "/orgs", token="t")
        org = json.loads(raw)["orgs"][0]
        assert org["shopify_secret_set"] is True and "shopify_secret" not in org
        assert store.get_org(conn, store.DEFAULT_ORG)["shopify_secret"] == "s3cret"
        assert _req("PUT", base + f"/orgs/{store.DEFAULT_ORG}", token="t",
                    body={"shopify_secret": None})[0] == 200
        assert store.get_org(conn, store.DEFAULT_ORG)["shopify_secret"] is None
    finally:
        httpd.shutdown()
