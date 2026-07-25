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


def test_print_job_copies_repeats_raw_send():
    sends = []
    entry = {"name": "Zebra", "can_pdf": False, "target": "Zebra"}
    print_agent.print_job("raw", entry, b"ZPL", copies=3,
                          raw_fn=lambda t, d: sends.append(d), pdf_fn=lambda *a: None)
    assert sends == [b"ZPL", b"ZPL", b"ZPL"]


def test_print_job_copies_repeats_pdf_and_socket():
    pdfs, socks = [], []
    print_agent.print_job("pdf", {"name": "HP", "can_pdf": True, "target": "HP"}, b"%PDF",
                          copies=2, raw_fn=lambda *a: None, pdf_fn=lambda t, d: pdfs.append(d))
    assert pdfs == [b"%PDF", b"%PDF"]
    print_agent.print_job("raw", {"name": "n", "target": "socket://10.0.0.5:9100"}, b"Z",
                          copies=2, socket_fn=lambda t, d: socks.append(d),
                          raw_fn=lambda *a: None, pdf_fn=lambda *a: None)
    assert socks == [b"Z", b"Z"]


def test_run_once_applies_job_copies():
    http = FakeHTTP({"job_id": 7, "printer_id": 1, "mode": "raw", "copies": 3}, b"D")
    sends = []
    print_agent.run_once(
        "http://x", "k", {1: {"name": "Z", "can_pdf": False, "target": "Z"}},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=lambda t, d: sends.append(d), pdf_fn=lambda *a: None)
    assert sends == [b"D", b"D", b"D"]


def test_run_once_defaults_copies_to_one_for_old_server():
    # a server without the copies field (older) -> exactly one print
    http = FakeHTTP({"job_id": 8, "printer_id": 1, "mode": "raw"}, b"D")
    sends = []
    print_agent.run_once(
        "http://x", "k", {1: {"name": "Z", "can_pdf": False, "target": "Z"}},
        http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
        raw_fn=lambda t, d: sends.append(d), pdf_fn=lambda *a: None)
    assert sends == [b"D"]


def test_sumatra_settings_maps_all_options():
    s = print_agent._sumatra_settings({"duplex": "long-edge", "paper": "A4", "bin": "Tray 1",
                                       "color": False, "pages": "1-3,5"})
    assert s == "1-3,5,duplexlong,paper=A4,bin=Tray 1,monochrome"
    assert print_agent._sumatra_settings({"duplex": "short-edge"}) == "duplexshort"
    assert print_agent._sumatra_settings({"duplex": "one-sided", "color": True}) == "simplex,color"


def test_pdf_to_printer_passes_print_settings():
    calls = []
    print_agent.pdf_to_printer("HP", b"%PDF", options={"duplex": "short-edge"},
                               run=lambda argv, **kw: calls.append(argv))
    argv = calls[0]
    assert argv[:3] == ["SumatraPDF.exe", "-print-to", "HP"]
    assert argv[argv.index("-print-settings") + 1] == "duplexshort"


def test_pdf_to_printer_no_options_no_print_settings():
    calls = []
    print_agent.pdf_to_printer("HP", b"%PDF", run=lambda argv, **kw: calls.append(argv))
    assert "-print-settings" not in calls[0]


def test_cups_pdf_maps_options_to_lp_o():
    calls = []
    print_agent.pdf_to_printer_cups(
        "HP", b"%PDF",
        options={"duplex": "long-edge", "paper": "A4", "bin": "Tray1",
                 "color": True, "pages": "1-2"},
        run=lambda argv, **kw: calls.append(argv))
    argv = calls[0]
    assert argv[:3] == ["lp", "-d", "HP"]
    for o in ("sides=two-sided-long-edge", "media=A4", "InputSlot=Tray1",
              "print-color-mode=color", "page-ranges=1-2"):
        assert o in argv


def test_cups_pdf_rejects_whitespace_option_values():
    # lp parses one -o value space-separated: "A4 raw" would smuggle an extra option in
    with pytest.raises(ValueError, match="whitespace"):
        print_agent.pdf_to_printer_cups("HP", b"%PDF", options={"paper": "A4 raw"},
                                        run=lambda argv, **kw: None)


def test_print_job_passes_options_to_pdf_fn():
    seen = {}
    entry = {"name": "HP", "can_pdf": True, "target": "HP"}
    print_agent.print_job("pdf", entry, b"%PDF", options={"duplex": "long-edge"},
                          raw_fn=lambda *a: None,
                          pdf_fn=lambda t, d, o: seen.update(t=t, o=o))
    assert seen == {"t": "HP", "o": {"duplex": "long-edge"}}


def test_print_job_without_options_calls_two_arg_pdf_fn():
    # no options -> old-style 2-arg pdf_fn keeps working (old server, plain jobs)
    seen = {}
    print_agent.print_job("pdf", {"name": "HP", "can_pdf": True, "target": "HP"}, b"%PDF",
                          raw_fn=lambda *a: None, pdf_fn=lambda t, d: seen.update(t=t))
    assert seen == {"t": "HP"}


def test_run_once_passes_job_options_to_pdf():
    http = FakeHTTP({"job_id": 3, "printer_id": 1, "mode": "pdf",
                     "options": {"paper": "A4"}}, b"%PDF")
    seen = []
    print_agent.run_once("http://x", "k", {1: {"name": "HP", "can_pdf": True, "target": "HP"}},
                         http_get=http.get, http_get_bytes=http.get_bytes, http_post=http.post,
                         raw_fn=lambda *a: None, pdf_fn=lambda t, d, o: seen.append(o))
    assert seen == [{"paper": "A4"}]
    assert http.posts[-1][1] == {"ok": True, "error": None}


def test_select_backend_windows_pdf_forwards_options(monkeypatch):
    seen = {}
    monkeypatch.setattr(
        print_agent, "pdf_to_printer",
        lambda p, d, options=None, sumatra=None, run=None: seen.update(
            p=p, options=options, sumatra=sumatra))
    _, pdf_w = print_agent.select_backend(platform="win32", sumatra="S.exe")
    pdf_w("HP", b"%PDF", {"paper": "A4"})
    assert seen == {"p": "HP", "options": {"paper": "A4"}, "sumatra": "S.exe"}


def test_caps_from_lpoptions_parses_pagesize_inputslot_duplex_color():
    text = ("PageSize/Media Size: *A4 Letter Legal Custom.WIDTHxHEIGHT\n"
            "InputSlot/Media Source: *Tray1 Tray2 Manual\n"
            "Duplex/2-Sided Printing: *None DuplexNoTumble DuplexTumble\n"
            "ColorModel/Color Model: *Gray RGB\n")
    assert print_agent._caps_from_lpoptions(text) == {
        "papers": ["A4", "Letter", "Legal", "Custom.WIDTHxHEIGHT"],
        "bins": ["Tray1", "Tray2", "Manual"],
        "duplex": True, "color": True}


def test_caps_from_lpoptions_mono_no_duplex_and_empty():
    text = "Duplex/2-Sided Printing: *None\nColorModel/Output Mode: *Gray\n"
    assert print_agent._caps_from_lpoptions(text) == {"duplex": False, "color": False}
    assert print_agent._caps_from_lpoptions("") is None


def test_collect_capabilities_cups_runs_lpoptions_and_survives_errors():
    def fake_run(argv, **kw):
        assert argv == ["lpoptions", "-p", "HP", "-l"]

        class R:
            stdout = b"PageSize/Media Size: *A4\n"
        return R()
    assert print_agent.collect_capabilities_cups("HP", run=fake_run) == {"papers": ["A4"]}

    def boom(argv, **kw):
        raise OSError("no lpoptions")
    assert print_agent.collect_capabilities_cups("HP", run=boom) is None


def test_collect_capabilities_windows_via_fake_win32print():
    class FakeWP:
        def OpenPrinter(self, name): return "h"
        def GetPrinter(self, h, level): return {"pPortName": "USB001"}
        def ClosePrinter(self, h): pass
        def DeviceCapabilities(self, dev, port, cap):
            return {16: ["A4\x00", "Letter"], 12: ["Tray 1"], 7: 1, 32: 0}[cap]
    assert print_agent.collect_capabilities_windows("HP", wp=FakeWP()) == {
        "papers": ["A4", "Letter"], "bins": ["Tray 1"], "duplex": True, "color": False}


def test_collect_capabilities_windows_errors_return_none():
    class Boom:
        def OpenPrinter(self, name): raise RuntimeError("no driver")
    assert print_agent.collect_capabilities_windows("HP", wp=Boom()) is None


def test_add_capabilities_skips_socket_targets_and_failures():
    printers = [{"name": "HP", "can_pdf": True, "target": "HP"},
                {"name": "netz", "can_pdf": False, "target": "socket://10.0.0.5:9100"},
                {"name": "Z", "can_pdf": False, "target": "Z"}]
    out = print_agent.add_capabilities(
        printers, lambda t: {"papers": ["A4"]} if t == "HP" else None)
    assert out[0]["capabilities"] == {"papers": ["A4"]}
    assert "capabilities" not in out[1] and "capabilities" not in out[2]


def test_select_caps_collector_by_platform():
    assert print_agent.select_caps_collector("win32") is print_agent.collect_capabilities_windows
    assert print_agent.select_caps_collector("linux") is print_agent.collect_capabilities_cups


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
