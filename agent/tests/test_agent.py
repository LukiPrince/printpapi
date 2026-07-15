import pytest
from agent import print_agent


class FakeHTTP:
    """Records calls; serves a scripted poll result + payload."""
    def __init__(self, poll_result, payload):
        self.poll_result, self.payload = poll_result, payload
        self.posts = []

    def get(self, url, key):            # GET /agent/jobs
        return self.poll_result

    def get_bytes(self, url, key):      # GET payload
        return self.payload

    def post(self, url, key, body):     # register / result
        self.posts.append((url, body))
        return {"ok": True}


def test_run_once_prints_and_reports_success():
    http = FakeHTTP({"job_id": 7, "printer_id": 1, "mode": "raw"}, b"ZPLDATA")
    printed = {}
    raw_fn = lambda printer, data: printed.update(printer=printer, data=data)
    pdf_fn = lambda printer, data: (_ for _ in ()).throw(AssertionError("pdf not expected"))
    handled = print_agent.run_once(
        "http://x", "k", {1: {"name": "Zebra", "can_pdf": False, "target": "Zebra"}},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=raw_fn, pdf_fn=pdf_fn)
    assert handled is True
    assert printed == {"printer": "Zebra", "data": b"ZPLDATA"}
    url, body = http.posts[-1]
    assert url.endswith("/agent/jobs/7/result") and body == {"ok": True, "error": None}


def test_run_once_reports_failure_on_print_error():
    http = FakeHTTP({"job_id": 9, "printer_id": 1, "mode": "raw"}, b"x")
    raw_fn = lambda p, d: (_ for _ in ()).throw(RuntimeError("spooler down"))
    handled = print_agent.run_once(
        "http://x", "k", {1: {"name": "Zebra", "can_pdf": False, "target": "Zebra"}},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=raw_fn, pdf_fn=lambda p, d: None)
    assert handled is True
    url, body = http.posts[-1]
    assert url.endswith("/agent/jobs/9/result") and body["ok"] is False
    assert "spooler down" in body["error"]


def test_run_once_no_job_returns_false():
    http = FakeHTTP(None, b"")
    assert print_agent.run_once(
        "http://x", "k", {}, http_get=http.get, http_get_bytes=http.get_bytes,
        http_post=http.post, raw_fn=lambda p, d: None, pdf_fn=lambda p, d: None) is False
    assert http.posts == []


def test_run_once_prints_pdf_and_reports_success():
    http = FakeHTTP({"job_id": 5, "printer_id": 1, "mode": "pdf"}, b"%PDF")
    printed = {}
    pdf_fn = lambda printer, data: printed.update(printer=printer, data=data)
    raw_fn = lambda printer, data: (_ for _ in ()).throw(AssertionError("raw not expected"))
    handled = print_agent.run_once(
        "http://x", "k", {1: {"name": "Zebra", "can_pdf": True, "target": "Zebra"}},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=raw_fn, pdf_fn=pdf_fn)
    assert handled is True
    assert printed == {"printer": "Zebra", "data": b"%PDF"}
    url, body = http.posts[-1]
    assert url.endswith("/agent/jobs/5/result") and body == {"ok": True, "error": None}


def test_print_job_bad_mode_raises():
    entry = {"name": "P", "can_pdf": False, "target": "P"}
    with pytest.raises(ValueError, match="bad mode"):
        print_agent.print_job("docx", entry, b"x")


def test_cups_raw_pipes_data_with_raw_option():
    calls = []
    print_agent.raw_to_printer_cups("Zebra", b"^XA^XZ",
                                    run=lambda argv, **kw: calls.append((argv, kw)))
    argv, kw = calls[0]
    assert argv == ["lp", "-d", "Zebra", "-o", "raw"]
    assert kw["input"] == b"^XA^XZ" and kw["check"] is True


def test_cups_pdf_pipes_data_without_raw_option():
    calls = []
    print_agent.pdf_to_printer_cups("HP", b"%PDF-1.4",
                                    run=lambda argv, **kw: calls.append((argv, kw)))
    argv, kw = calls[0]
    assert argv == ["lp", "-d", "HP"]          # CUPS renders PDF itself; no -o raw
    assert kw["input"] == b"%PDF-1.4"


def test_select_backend_by_platform():
    raw_w, _ = print_agent.select_backend(platform="win32")
    assert raw_w is print_agent.raw_to_printer          # Windows: win32print RAW
    raw_l, pdf_l = print_agent.select_backend(platform="linux")
    assert raw_l is print_agent.raw_to_printer_cups     # non-Windows: CUPS lp
    assert pdf_l is print_agent.pdf_to_printer_cups


def test_parse_printers_pdf_is_opt_in_default_raw():
    ps = print_agent.parse_printers("Zebra GK420d; HP LaserJet|pdf ; Office|PDF; ")
    # default is raw-only so a label printer is never auto-sent a PDF (gotcha #1);
    # a document printer opts into PDF with a '|pdf' tag (case-insensitive).
    assert ps == [
        {"name": "Zebra GK420d", "can_pdf": False, "target": "Zebra GK420d"},
        {"name": "HP LaserJet", "can_pdf": True, "target": "HP LaserJet"},
        {"name": "Office", "can_pdf": True, "target": "Office"},
    ]


def test_parse_printers_socket_target():
    ps = print_agent.parse_printers("Bixolon ; netz = socket://10.0.0.5:9100")
    assert ps == [
        {"name": "Bixolon", "can_pdf": False, "target": "Bixolon"},
        {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"},
    ]


def test_parse_printers_socket_is_forced_raw_only():
    # |pdf on a socket target is ignored — no renderer behind a bare socket (gotcha #1)
    ps = print_agent.parse_printers("lbl|pdf = socket://10.0.0.5:9100")
    assert ps == [{"name": "lbl", "can_pdf": False, "target": "socket://10.0.0.5:9100"}]


def test_parse_printers_socket_missing_port_raises():
    # a bad socket:// target should fail loudly at parse time, not later in raw_to_socket
    with pytest.raises(ValueError, match="socket"):
        print_agent.parse_printers("netz = socket://10.0.0.5")


def test_parse_printers_socket_valid_host_port_ok():
    ps = print_agent.parse_printers("netz = socket://10.0.0.5:9100")
    assert ps == [{"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"}]


def test_raw_to_socket_parses_addr_and_sends_bytes():
    seen = {}

    class FakeSock:
        def __enter__(self): return self
        def __exit__(self, *a): return False
        def sendall(self, d): seen["data"] = d

    def fake_connect(addr, timeout=None):
        seen["addr"] = addr
        return FakeSock()

    print_agent.raw_to_socket("socket://10.0.0.5:9100", b"^XA^XZ", connect=fake_connect)
    assert seen["addr"] == ("10.0.0.5", 9100)
    assert seen["data"] == b"^XA^XZ"


def test_print_job_socket_raw_routes_to_socket_fn():
    seen = {}
    entry = {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"}
    print_agent.print_job(
        "raw", entry, b"Z",
        socket_fn=lambda t, d: seen.update(t=t, d=d),
        raw_fn=lambda *a: (_ for _ in ()).throw(AssertionError("local raw not expected")),
        pdf_fn=lambda *a: None)
    assert seen == {"t": "socket://10.0.0.5:9100", "d": b"Z"}


def test_print_job_socket_pdf_is_refused():
    entry = {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"}
    with pytest.raises(ValueError, match="raw-only"):
        print_agent.print_job("pdf", entry, b"%PDF",
                              socket_fn=lambda t, d: None,
                              raw_fn=lambda *a: None, pdf_fn=lambda *a: None)


def test_print_job_local_raw_uses_target_name():
    seen = {}
    entry = {"name": "Zebra", "can_pdf": False, "target": "Zebra"}
    print_agent.print_job("raw", entry, b"x",
                          raw_fn=lambda t, d: seen.update(t=t, d=d), pdf_fn=lambda *a: None)
    assert seen == {"t": "Zebra", "d": b"x"}


def test_req_maps_urlerror_to_oserror(monkeypatch):
    import urllib.error, urllib.request

    def refuse(req, **kw):
        raise urllib.error.URLError("connection refused")
    monkeypatch.setattr(urllib.request, "urlopen", refuse)
    with pytest.raises(OSError, match="connection failed"):
        print_agent._req("http://127.0.0.1:1/x", "k")


def test_report_retries_then_succeeds():
    calls = []

    def flaky(url, key, body):
        calls.append(body)
        if len(calls) < 3:
            raise OSError("net down")
    ok = print_agent._report_with_retry("http://x", "k", 1, True, None,
                                        http_post=flaky, sleep=lambda s: None)
    assert ok and len(calls) == 3


def test_run_once_survives_report_failure():
    def dead(url, key, body):
        raise OSError("net down")
    job = {"job_id": 1, "printer_id": 5, "mode": "raw"}
    printed = []
    ret = print_agent.run_once(
        "http://x", "k", {5: {"name": "p", "can_pdf": False, "target": "p"}},
        http_get=lambda u, k: job, http_get_bytes=lambda u, k: b"DATA",
        http_post=dead, raw_fn=lambda t, d: printed.append(d),
        report_sleep=lambda s: None)
    assert ret is True and printed == [b"DATA"]   # printed, and no exception escaped


def test_load_config_missing_file(tmp_path):
    with pytest.raises(SystemExit, match="agent.ini"):
        print_agent.load_config(str(tmp_path))


def test_load_config_ok(tmp_path):
    (tmp_path / "agent.ini").write_text(
        "[agent]\nserver_url=http://x\napi_key=k\nprinters=p\n")
    cfg = print_agent.load_config(str(tmp_path))
    assert cfg["server_url"] == "http://x"
