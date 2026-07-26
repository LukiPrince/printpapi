import base64, json, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer

import pytest

from app import printnode, server, store


# --- pure translation ---------------------------------------------------------------------------

def test_basic_key_takes_the_username_half():
    hdr = "Basic " + base64.b64encode(b"secretkey:").decode()
    assert printnode.basic_key(hdr) == "secretkey"
    # a password is present but irrelevant
    assert printnode.basic_key("Basic " + base64.b64encode(b"k:pw").decode()) == "k"
    assert printnode.basic_key("Bearer secretkey") == ""
    assert printnode.basic_key("Basic !!!not-base64!!!") == ""
    assert printnode.basic_key("") == ""


def test_parse_set_handles_ids_ranges_and_mixes():
    assert printnode.parse_set("10") == [10]
    assert printnode.parse_set("10,12") == [10, 12]
    assert printnode.parse_set("5-7") == [5, 6, 7]
    assert printnode.parse_set("1,4-6") == [1, 4, 5, 6]
    for bad in ("", "abc", "-5", "9-4", "1-999999"):
        with pytest.raises(printnode.CompatError):
            printnode.parse_set(bad)


def test_job_state_mapping():
    assert printnode.job_state("queued") == "queued"
    assert printnode.job_state("claimed") == "sent"
    assert printnode.job_state("done") == "done"
    assert printnode.job_state("failed", "printer offline") == "error"
    assert printnode.job_state("failed", "expired") == "expired"
    assert printnode.job_state("cancelled") == "deleted"


def test_job_body_maps_content_url_qty_and_options():
    b = printnode.job_body({"printerId": 3, "title": "T", "contentType": "pdf_uri",
                            "content": "https://x/y.pdf", "qty": 2, "expireAfter": 60,
                            "options": {"paper": "A4", "rotate": 90, "fit_to_page": True}})
    # the URL rides in `content` on their side, in `url` on ours
    assert b == {"type": "pdf_uri", "printer_id": 3, "title": "T", "url": "https://x/y.pdf",
                 "copies": 2, "expire_after": 60, "options": {"paper": "A4"}}


def test_job_body_base64_and_raw_drops_options():
    b = printnode.job_body({"printerId": 1, "contentType": "raw_base64", "content": "QUJD",
                            "options": {"paper": "A4", "copies": 3}})
    # raw payloads carry their own layout — options would 400 in POST /jobs, so they are dropped
    assert b["content"] == "QUJD" and "options" not in b and b["copies"] == 3
    with pytest.raises(printnode.CompatError):
        printnode.job_body({"printerId": 1, "contentType": "docx", "content": "x"})
    with pytest.raises(printnode.CompatError):
        printnode.job_body("nope")


def test_capabilities_mapping():
    caps = printnode.capabilities({"papers": ["A4", "Letter"], "bins": ["Tray 1"],
                                   "duplex": True, "color": False})
    assert caps["papers"] == {"A4": None, "Letter": None}   # names known, dimensions not
    assert caps["bins"] == ["Tray 1"] and caps["duplex"] is True and caps["color"] is False
    assert printnode.capabilities(None) is None


def test_iso_timestamps():
    assert printnode._iso(0) == "1970-01-01T00:00:00.000Z"
    assert printnode._iso(None) is None


# --- over HTTP ----------------------------------------------------------------------------------

def _mem():
    conn = store.connect(":memory:")
    store.init_db(conn)
    return conn


def _serve(conn, token="t"):
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3, poll_interval=0.05)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}"


def _pn(method, url, key="t", body=None):
    """A PrintNode-style call: the API key is the HTTP Basic username."""
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Authorization", "Basic " + base64.b64encode(f"{key}:".encode()).decode())
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def _bearer(method, url, key="t", body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Authorization", f"Bearer {key}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def test_whoami_and_bad_key():
    conn = _mem()
    httpd, base = _serve(conn)
    try:
        code, body = _pn("GET", base + "/whoami")
        assert code == 200 and body["id"] == 0 and body["credits"] is None
        assert body["numComputers"] == 0 and body["totalPrints"] == 0
        code, body = _pn("GET", base + "/whoami", key="nope")
        assert code == 401 and body["code"] and body["message"]
    finally:
        httpd.shutdown()


def test_computers_and_printers_shapes():
    conn = _mem()
    store.register_agent(conn, "win-1", "ak", [
        {"name": "Zebra", "can_pdf": False},
        {"name": "Office", "can_pdf": True,
         "capabilities": {"papers": ["A4"], "bins": ["Tray 1"], "duplex": True, "color": True}}])
    httpd, base = _serve(conn)
    try:
        code, comps = _pn("GET", base + "/computers")
        assert code == 200 and len(comps) == 1
        assert comps[0]["name"] == "win-1" and comps[0]["state"] == "connected"
        assert comps[0]["hostname"] == "win-1" and comps[0]["inet"] is None

        code, printers = _pn("GET", base + "/printers")
        assert code == 200 and [p["name"] for p in printers] == ["Zebra", "Office"]
        assert printers[0]["computer"]["id"] == comps[0]["id"]
        assert printers[0]["state"] == "online" and printers[0]["capabilities"] is None
        assert printers[1]["capabilities"]["papers"] == {"A4": None}

        # id sets, and the computer-scoped printer list
        one = printers[1]["id"]
        assert [p["id"] for p in _pn("GET", base + f"/printers/{one}")[1]] == [one]
        assert len(_pn("GET", base + f"/computers/{comps[0]['id']}/printers")[1]) == 2
        assert _pn("GET", base + "/printers/9-1")[0] == 400

        # Bearer on the same path still gets printpapi's own shape
        code, own = _bearer("GET", base + "/printers")
        assert code == 200 and own["printers"][0]["can_pdf"] is False
    finally:
        httpd.shutdown()


def test_printjob_submit_list_states_and_delete():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "ak", [{"name": "Zebra", "can_pdf": False}])
    pid = reg["printer_ids"]["Zebra"]
    httpd, base = _serve(conn)
    try:
        code, jid = _pn("POST", base + "/printjobs",
                        body={"printerId": pid, "title": "Label", "contentType": "raw_base64",
                              "content": "QUJD", "source": "my plugin"})
        assert code == 201 and isinstance(jid, int)      # PrintNode answers with the bare id
        assert store.get_job(conn, jid)["state"] == "queued"

        code, jobs = _pn("GET", base + "/printjobs")
        assert code == 200 and jobs[0]["id"] == jid and jobs[0]["state"] == "queued"
        assert jobs[0]["title"] == "Label" and jobs[0]["contentType"] == "raw_base64"
        assert jobs[0]["printer"]["id"] == pid

        code, states = _pn("GET", base + f"/printjobs/{jid}/states")
        assert code == 200 and states[0][0]["printJobId"] == jid
        assert states[0][0]["state"] == "queued"

        # a claimed job reads as `sent`, a finished one as `done`
        store.claim_job(conn, reg["computer_id"])
        assert _pn("GET", base + f"/printjobs/{jid}")[1][0]["state"] == "sent"
        store.finish_job(conn, jid, reg["computer_id"], True)
        assert _pn("GET", base + f"/printjobs/{jid}/states")[1][0][0]["state"] == "done"

        # delete cancels what is still queued
        code, jid2 = _pn("POST", base + "/printjobs",
                         body={"printerId": pid, "contentType": "raw_base64", "content": "QUJD"})
        assert code == 201
        assert _pn("DELETE", base + f"/printjobs/{jid2}") == (200, 1)
        assert store.get_job(conn, jid2)["state"] == "cancelled"
        assert _pn("GET", base + f"/printjobs/{jid2}")[1][0]["state"] == "deleted"

        # their clients page the list with ?limit=
        assert len(_pn("GET", base + "/printjobs")[1]) == 2
        assert len(_pn("GET", base + "/printjobs?limit=1")[1]) == 1
        assert len(_pn("GET", base + "/printjobs?limit=junk")[1]) == 2

        assert _pn("POST", base + "/printjobs",
                   body={"printerId": 999, "contentType": "raw_base64", "content": "QUJD"})[0] == 400
    finally:
        httpd.shutdown()


def test_compat_layer_is_org_scoped():
    conn = _mem()
    other = store.create_org(conn, "other")
    key_a, key_b = "ka", "kb"
    store.add_api_key(conn, "a", key_a)                       # default org
    store.add_api_key(conn, "b", key_b, org_id=other)
    reg = store.register_agent(conn, "win-1", "ak", [{"name": "Zebra", "can_pdf": False}])
    pid = reg["printer_ids"]["Zebra"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"ABC")
    httpd, base = _serve(conn)
    try:
        assert len(_pn("GET", base + "/printers", key=key_a)[1]) == 1
        assert _pn("GET", base + "/printers", key=key_b)[1] == []
        assert _pn("GET", base + "/computers", key=key_b)[1] == []
        assert _pn("GET", base + f"/printjobs/{jid}", key=key_b)[1] == []
        # a foreign printer is simply unknown
        assert _pn("POST", base + "/printjobs", key=key_b,
                   body={"printerId": pid, "contentType": "raw_base64", "content": "QUJD"})[0] == 400
    finally:
        httpd.shutdown()


def test_pdf_options_reach_the_job_and_unknown_ones_are_ignored():
    conn = _mem()
    reg = store.register_agent(conn, "win-1", "ak", [{"name": "Office", "can_pdf": True}])
    pid = reg["printer_ids"]["Office"]
    httpd, base = _serve(conn)
    try:
        code, jid = _pn("POST", base + "/printjobs",
                        body={"printerId": pid, "contentType": "pdf_base64", "content": "QUJD",
                              "qty": 2, "options": {"paper": "A4", "duplex": "long-edge",
                                                    "rotate": 90, "dpi": "300x300"}})
        assert code == 201
        job = store.claim_job(conn, reg["computer_id"])
        assert job["copies"] == 2 and job["options"] == {"paper": "A4", "duplex": "long-edge"}
    finally:
        httpd.shutdown()
