import base64, json, threading, urllib.request, urllib.error
from http.server import ThreadingHTTPServer

from app import cloudprnt, server, store


# --- pure translation ---------------------------------------------------------------------------

def test_media_type_picks_the_best_type_the_client_accepts():
    accept = ("application/vnd.star.starprnt, application/vnd.star.starconfiguration, "
              "application/vnd.star.starprntcore; q=0.9, image/png; q=0.1")
    assert cloudprnt.media_type(accept) == "application/vnd.star.starprnt"
    # an interface board that speaks line mode but not starprnt
    assert cloudprnt.media_type("application/vnd.star.line, image/png") \
        == "application/vnd.star.line"
    assert cloudprnt.media_type("text/plain") == "text/plain"
    # nothing we can label our bytes with, or no header at all -> the default
    assert cloudprnt.media_type("image/png, image/jpeg") == cloudprnt.MEDIA_DEFAULT
    assert cloudprnt.media_type(None) == cloudprnt.MEDIA_DEFAULT


def test_poll_response_shapes():
    assert cloudprnt.poll_response(None) == {"jobReady": False}
    r = cloudprnt.poll_response({"job_id": 7}, "text/plain")
    assert r == {"jobReady": True, "mediaTypes": ["text/plain"], "jobToken": "7"}


def test_job_ok_reads_the_printers_status_code():
    assert cloudprnt.job_ok("200 OK") is True
    assert cloudprnt.job_ok("OK") is True
    # every 2xx means the printer is online and printed — including the ones that carry a warning
    assert cloudprnt.job_ok("201 Output paper taken") is True
    assert cloudprnt.job_ok("211 Paper low") is True
    assert cloudprnt.job_ok("420 Cover open") is False
    assert cloudprnt.job_ok("521") is False
    assert cloudprnt.job_ok("") is False        # no status is not a success
    assert cloudprnt.job_ok(None) is False


def test_status_text_explains_the_codes_an_operator_can_act_on():
    assert cloudprnt.status_text("410 Out of paper") == "410 Out of paper (out of paper)"
    assert "too large" in cloudprnt.status_text("521")
    assert cloudprnt.status_text("999 whatever") == "999 whatever"   # unknown passes through
    assert cloudprnt.status_text("") == "no status"


def test_device_name_is_stable_across_mac_spelling():
    assert cloudprnt.device_name(" 00:11:62:0F:97:1A ") == "cloudprnt-00:11:62:0f:97:1a"


# --- store --------------------------------------------------------------------------------------

def _mem():
    conn = store.connect(":memory:")
    store.init_db(conn)
    return conn


def test_register_cloudprnt_upserts_one_raw_only_printer():
    conn = _mem()
    a = store.register_cloudprnt(conn, store.DEFAULT_ORG, "cloudprnt-mac1")
    b = store.register_cloudprnt(conn, store.DEFAULT_ORG, "cloudprnt-mac1")
    assert a == b                                   # a second poll is the same device
    printers = store.list_printers(conn, 60)
    assert len(printers) == 1
    assert printers[0]["id"] == a["printer_id"]
    assert printers[0]["can_pdf"] is False          # gotcha #1: no PDF renderer in the printer
    assert printers[0]["online"] is True            # polling is what liveness means here
    # the same MAC in another org is a different device, not a hijack
    other = store.register_cloudprnt(conn, store.create_org(conn, "other"), "cloudprnt-mac1")
    assert other["printer_id"] != a["printer_id"]


def test_claimed_job_reports_the_job_in_flight():
    conn = _mem()
    dev = store.register_cloudprnt(conn, store.DEFAULT_ORG, "cloudprnt-mac1")
    jid = store.enqueue_job(conn, dev["printer_id"], "raw_base64", "raw", b"ABC", copies=3)
    assert store.claimed_job(conn, dev["agent_id"]) is None      # still queued
    store.claim_job(conn, dev["agent_id"])
    assert store.claimed_job(conn, dev["agent_id"]) == {"job_id": jid, "mode": "raw", "copies": 3}
    store.finish_job(conn, jid, dev["agent_id"], True)
    assert store.claimed_job(conn, dev["agent_id"]) is None      # terminal again


# --- over HTTP ----------------------------------------------------------------------------------

def _serve(conn, token="t"):
    handler = server.make_handler(conn=conn, token=token, long_poll_timeout=0.3, poll_interval=0.05)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, f"http://127.0.0.1:{httpd.server_address[1]}"


def _org_with_key(conn, key="cpkey", name="acme"):
    oid = store.create_org(conn, name)
    store.add_api_key(conn, "cloudprnt", key, org_id=oid)
    return oid


def _cp(method, url, body=None, accept=None, basic=None):
    """One CloudPRNT client request -> (status, body bytes, Content-Type)."""
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if accept:
        r.add_header("Accept", accept)
    if basic:
        r.add_header("Authorization", "Basic " + base64.b64encode(f"{basic}:".encode()).decode())
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, resp.read(), resp.headers.get("Content-Type")
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers.get("Content-Type")


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


MAC = "00:11:62:0f:97:1a"
POLL = {"status": "23 6 0 0 0 0 0 0 0 ", "printerMAC": MAC, "statusCode": "200%20OK",
        "clientAction": None}


def test_cloudprnt_roundtrip_poll_get_delete():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    url = base + "/cloudprnt/cpkey"
    try:
        # first poll: nothing to print, and it enrols the printer
        code, raw, _ = _cp("POST", url, POLL, accept="application/vnd.star.starprnt")
        assert code == 200 and json.loads(raw) == {"jobReady": False}
        code, raw = _req("GET", base + "/printers", token="cpkey")
        printers = json.loads(raw)["printers"]
        assert len(printers) == 1 and printers[0]["name"] == f"cloudprnt-{MAC}"
        pid = printers[0]["id"]

        # a client queues a raw job the ordinary way
        code, raw = _req("POST", base + "/jobs", token="cpkey",
                         body={"printer_id": pid, "type": "raw_base64", "content": "QUJD"})
        assert code == 200
        jid = json.loads(raw)["job_id"]

        # the next poll offers it
        code, raw, _ = _cp("POST", url, POLL, accept="application/vnd.star.starprnt")
        body = json.loads(raw)
        assert body == {"jobReady": True, "mediaTypes": ["application/vnd.star.starprnt"],
                        "jobToken": str(jid)}

        # the printer pulls the data
        code, data, ctype = _cp("GET", url + f"?mac={MAC}&type=application/vnd.star.starprnt"
                                            f"&token={jid}")
        assert code == 200 and data == b"ABC" and ctype == "application/vnd.star.starprnt"

        # ... prints it, and confirms
        assert _cp("DELETE", url + f"?mac={MAC}&code=200%20OK&token={jid}")[0] == 200
        code, raw = _req("GET", base + f"/jobs/{jid}", token="cpkey")
        assert json.loads(raw)["state"] == "done"
    finally:
        httpd.shutdown()


def test_printer_error_code_fails_the_job():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    url = base + "/cloudprnt/cpkey"
    try:
        _cp("POST", url, POLL)
        pid = json.loads(_req("GET", base + "/printers", token="cpkey")[1])["printers"][0]["id"]
        jid = json.loads(_req("POST", base + "/jobs", token="cpkey",
                              body={"printer_id": pid, "type": "raw_base64",
                                    "content": "QUJD"})[1])["job_id"]
        _cp("POST", url, POLL)
        _cp("GET", url + f"?mac={MAC}")
        assert _cp("DELETE", url + f"?mac={MAC}&code=420%20Cover%20open")[0] == 200
        job = json.loads(_req("GET", base + f"/jobs/{jid}", token="cpkey")[1])
        assert job["state"] == "failed" and "420" in job["error"] and "cover open" in job["error"]
    finally:
        httpd.shutdown()


def test_an_unconfirmed_job_is_re_offered_not_replaced():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    url = base + "/cloudprnt/cpkey"
    try:
        _cp("POST", url, POLL)
        pid = json.loads(_req("GET", base + "/printers", token="cpkey")[1])["printers"][0]["id"]
        first = json.loads(_req("POST", base + "/jobs", token="cpkey",
                                body={"printer_id": pid, "type": "raw_base64",
                                      "content": "QUJD"})[1])["job_id"]
        second = json.loads(_req("POST", base + "/jobs", token="cpkey",
                                 body={"printer_id": pid, "type": "raw_base64",
                                       "content": "REVG"})[1])["job_id"]
        tok1 = json.loads(_cp("POST", url, POLL)[1])["jobToken"]
        tok2 = json.loads(_cp("POST", url, POLL)[1])["jobToken"]
        assert tok1 == tok2 == str(first)       # the lost response does not burn the second job
        assert json.loads(_req("GET", base + f"/jobs/{second}", token="cpkey")[1])["state"] \
            == "queued"
        # a busy printer is offered nothing at all
        busy = json.loads(_cp("POST", url, dict(POLL, printingInProgress=True))[1])
        assert busy == {"jobReady": False}
    finally:
        httpd.shutdown()


def test_copies_are_repeated_in_the_stream():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    url = base + "/cloudprnt/cpkey"
    try:
        _cp("POST", url, POLL)
        pid = json.loads(_req("GET", base + "/printers", token="cpkey")[1])["printers"][0]["id"]
        _req("POST", base + "/jobs", token="cpkey",
             body={"printer_id": pid, "type": "raw_base64", "content": "QUJD", "copies": 3})
        _cp("POST", url, POLL)
        # the printer has no copy count of its own — the stream carries them
        assert _cp("GET", url + f"?mac={MAC}")[1] == b"ABCABCABC"
    finally:
        httpd.shutdown()


def test_pdf_job_fails_instead_of_printing_blanks():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    url = base + "/cloudprnt/cpkey"
    try:
        _cp("POST", url, POLL)
        pid = json.loads(_req("GET", base + "/printers", token="cpkey")[1])["printers"][0]["id"]
        jid = json.loads(_req("POST", base + "/jobs", token="cpkey",
                              body={"printer_id": pid, "type": "pdf_base64",
                                    "content": "QUJD"})[1])["job_id"]
        assert json.loads(_cp("POST", url, POLL)[1]) == {"jobReady": False}
        job = json.loads(_req("GET", base + f"/jobs/{jid}", token="cpkey")[1])
        assert job["state"] == "failed" and "PDF" in job["error"]
    finally:
        httpd.shutdown()


def test_auth_mac_and_media_type_errors():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    url = base + "/cloudprnt/cpkey"
    try:
        # unknown key on every method
        assert _cp("POST", base + "/cloudprnt/nope", POLL)[0] == 401
        assert _cp("GET", base + f"/cloudprnt/nope?mac={MAC}")[0] == 401
        assert _cp("DELETE", base + f"/cloudprnt/nope?mac={MAC}&code=OK")[0] == 401
        # the root token is not an org, so it cannot enrol a printer either
        assert _cp("POST", base + "/cloudprnt/t", POLL)[0] == 401
        # a device that does not say who it is
        assert _cp("POST", url, dict(POLL, printerMAC=""))[0] == 400
        assert _cp("GET", url)[0] == 400
        # enrolled, nothing queued -> the protocol's "no data available"
        _cp("POST", url, POLL)
        assert _cp("GET", url + f"?mac={MAC}")[0] == 404
        pid = json.loads(_req("GET", base + "/printers", token="cpkey")[1])["printers"][0]["id"]
        jid = json.loads(_req("POST", base + "/jobs", token="cpkey",
                              body={"printer_id": pid, "type": "raw_base64",
                                    "content": "QUJD"})[1])["job_id"]
        _cp("POST", url, POLL)
        # a type we cannot label these bytes with (we never transcode)
        assert _cp("GET", url + f"?mac={MAC}&type=image/png")[0] == 415
        # someone else's token
        assert _cp("GET", url + f"?mac={MAC}&token={jid + 99}")[0] == 404
        # confirming without a code is not a success
        assert _cp("DELETE", url + f"?mac={MAC}")[0] == 200
        assert json.loads(_req("GET", base + f"/jobs/{jid}", token="cpkey")[1])["state"] == "failed"
    finally:
        httpd.shutdown()


def test_server_setting_request_is_a_404_so_the_printer_falls_back_to_http():
    # An MQTT-capable printer asks for <cloudprnt path>/cloudprnt-setting.json once at power-on.
    # A server that speaks only CloudPRNT HTTP answers 404 to it — and then the printer polls.
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    try:
        assert _cp("GET", base + "/cloudprnt/cloudprnt-setting.json"
                          f"?mac={MAC}&replaced_path=cpkey")[0] == 404
        assert _cp("GET", base + f"/cloudprnt-setting.json?mac={MAC}", basic="cpkey")[0] == 404
        # ... and nothing was enrolled by it
        assert json.loads(_req("GET", base + "/printers", token="cpkey")[1])["printers"] == []
    finally:
        httpd.shutdown()


def test_basic_auth_is_accepted_and_does_not_collide_with_the_printnode_layer():
    conn = _mem()
    _org_with_key(conn)
    httpd, base = _serve(conn)
    try:
        # the printer's own "User Name / Password" settings, so the key stays out of the URL
        code, raw, _ = _cp("POST", base + "/cloudprnt", POLL, basic="cpkey")
        assert code == 200 and json.loads(raw) == {"jobReady": False}
        assert _cp("GET", base + f"/cloudprnt?mac={MAC}", basic="cpkey")[0] == 404
        assert _cp("DELETE", base + f"/cloudprnt?mac={MAC}&code=OK", basic="cpkey")[0] == 200
        # ... and the PrintNode compat layer still answers its own paths under Basic auth
        code, raw, _ = _cp("GET", base + "/whoami", basic="cpkey")
        assert code == 200 and "id" in json.loads(raw)
    finally:
        httpd.shutdown()
