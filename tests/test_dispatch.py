import base64
import pytest
from app.dispatch import (decode_payload, agent_mode, parse_copies, parse_callback_url,
                          parse_options, DispatchError)


def test_raw_base64_decodes():
    body = {"type": "raw_base64", "content": base64.b64encode(b"^XA^XZ").decode()}
    assert decode_payload(body) == b"^XA^XZ"


def test_pdf_base64_decodes():
    body = {"type": "pdf_base64", "content": base64.b64encode(b"%PDF-1.4").decode()}
    assert decode_payload(body) == b"%PDF-1.4"


def test_raw_uri_uses_injected_fetcher():
    body = {"type": "raw_uri", "url": "https://x/y.zpl"}
    seen = {}
    def fake_fetch(url):
        seen["url"] = url
        return b"ZPLBYTES"
    assert decode_payload(body, fetch_url=fake_fetch) == b"ZPLBYTES"
    assert seen["url"] == "https://x/y.zpl"


def test_unknown_type_raises():
    with pytest.raises(DispatchError):
        decode_payload({"type": "nope"})


def test_missing_content_raises():
    with pytest.raises(DispatchError):
        decode_payload({"type": "raw_base64"})


def test_bad_base64_raises():
    with pytest.raises(DispatchError):
        decode_payload({"type": "raw_base64", "content": "!!!notbase64!!!"})


def test_agent_mode():
    assert agent_mode("pdf_base64") == "pdf"
    assert agent_mode("raw_base64") == "raw"
    assert agent_mode("raw_uri") == "raw"


def test_missing_url_raises():
    with pytest.raises(DispatchError):
        decode_payload({"type": "raw_uri"})


def test_fetch_failure_becomes_dispatcherror():
    def boom(url):
        raise OSError("unreachable")
    with pytest.raises(DispatchError):
        decode_payload({"type": "raw_uri", "url": "https://x"}, fetch_url=boom)


def test_fetch_failure_is_fetcherror():
    from app.dispatch import FetchError
    def boom(url):
        raise OSError("render-svc down")
    with pytest.raises(FetchError):
        decode_payload({"type": "raw_uri", "url": "https://x"}, fetch_url=boom)


def test_bad_scheme_raises_dispatcherror():
    with pytest.raises(DispatchError):
        decode_payload({"type": "raw_uri", "url": "file:///etc/passwd"})


def test_http_get_sets_browser_user_agent(monkeypatch):
    # Cloudflare in front of render-svc 403s the default Python-urllib UA.
    import app.dispatch as d
    captured = {}

    class _R:
        def read(self):
            return b"ZPL"
        def __enter__(self):
            return self
        def __exit__(self, *a):
            return False

    def fake_urlopen(req, timeout=30):
        captured["ua"] = req.get_header("User-agent")
        return _R()

    monkeypatch.setattr(d.urllib.request, "urlopen", fake_urlopen)
    assert d._http_get("https://x") == b"ZPL"
    assert "Mozilla/5.0" in captured["ua"]


def test_raw_uri_post_calls_post_fetch_with_url_and_json():
    seen = {}
    def fake_post(url, json_body):
        seen["url"] = url; seen["json"] = json_body
        return b"%PDF-bytes"
    body = {"type": "raw_uri_post", "url": "https://render-svc/convert", "json": {"url": "https://label"}}
    assert decode_payload(body, post_fetch=fake_post) == b"%PDF-bytes"
    assert seen["url"] == "https://render-svc/convert"
    assert seen["json"] == {"url": "https://label"}


def test_pdf_uri_post_calls_post_fetch():
    body = {"type": "pdf_uri_post", "url": "https://render-svc/convert", "json": {}}
    assert decode_payload(body, post_fetch=lambda u, j: b"PDF") == b"PDF"


def test_uri_post_missing_url_raises():
    with pytest.raises(DispatchError):
        decode_payload({"type": "pdf_uri_post", "json": {}})


def test_uri_post_fetch_failure_is_fetcherror():
    from app.dispatch import FetchError
    def boom(url, json_body):
        raise OSError("convert down")
    with pytest.raises(FetchError):
        decode_payload({"type": "raw_uri_post", "url": "https://x", "json": {}}, post_fetch=boom)


def test_agent_mode_pdf_uri_post():
    assert agent_mode("pdf_uri_post") == "pdf"
    assert agent_mode("raw_uri_post") == "raw"


def test_parse_copies_defaults_to_one_when_absent_or_null():
    assert parse_copies({}) == 1
    assert parse_copies({"copies": None}) == 1


def test_parse_copies_accepts_valid_count():
    assert parse_copies({"copies": 3}) == 3
    assert parse_copies({"copies": 1}) == 1
    assert parse_copies({"copies": 100}) == 100   # inclusive upper cap


def test_parse_copies_rejects_zero_and_negative():
    for n in (0, -1, -100):
        with pytest.raises(DispatchError):
            parse_copies({"copies": n})


def test_parse_copies_rejects_over_cap():
    with pytest.raises(DispatchError):
        parse_copies({"copies": 101})


def test_parse_copies_rejects_non_int_types():
    # trust boundary: "3", 2.5, and bool are not a valid copy count
    for v in ("3", 2.5, True):
        with pytest.raises(DispatchError):
            parse_copies({"copies": v})


def test_parse_callback_url_absent_or_empty_is_none():
    assert parse_callback_url({}) is None
    assert parse_callback_url({"callback_url": ""}) is None
    assert parse_callback_url({"callback_url": None}) is None


def test_parse_callback_url_accepts_http_and_https():
    assert parse_callback_url({"callback_url": "https://hook.example/x"}) == "https://hook.example/x"
    assert parse_callback_url({"callback_url": "http://hook/x"}) == "http://hook/x"


def test_parse_callback_url_rejects_non_http_scheme():
    for u in ("file:///etc/passwd", "ftp://x", "gopher://x", "javascript:alert(1)"):
        with pytest.raises(DispatchError):
            parse_callback_url({"callback_url": u})


def test_parse_callback_url_rejects_non_string():
    # a truthy non-string must be a clean DispatchError (-> 400), not an uncaught crash
    for v in (123, 1.5, True, ["https://x"], {"u": "https://x"}):
        with pytest.raises(DispatchError):
            parse_callback_url({"callback_url": v})


def test_parse_options_absent_null_or_empty_is_none():
    assert parse_options({}, "pdf") is None
    assert parse_options({"options": None}, "pdf") is None
    assert parse_options({"options": {}}, "pdf") is None
    assert parse_options({"options": {}}, "raw") is None   # empty options never error


def test_parse_options_valid_full_set_roundtrips():
    o = {"duplex": "long-edge", "paper": "A4", "bin": "Tray 1", "color": False, "pages": "1-3,5"}
    assert parse_options({"options": o}, "pdf") == o
    assert parse_options({"options": {"duplex": "short-edge"}}, "pdf") == {"duplex": "short-edge"}
    assert parse_options({"options": {"duplex": "one-sided"}}, "pdf") == {"duplex": "one-sided"}


def test_parse_options_rejected_on_raw_jobs():
    # ZPL/ESC-POS carries its own layout; duplex/paper/bin only mean something to a renderer
    with pytest.raises(DispatchError):
        parse_options({"options": {"duplex": "long-edge"}}, "raw")


def test_parse_options_rejects_non_dict():
    for v in ("duplex", 1, ["duplex"], True, ""):
        with pytest.raises(DispatchError):
            parse_options({"options": v}, "pdf")


def test_parse_options_rejects_unknown_keys():
    with pytest.raises(DispatchError):
        parse_options({"options": {"stapler": True}}, "pdf")


def test_parse_options_rejects_bad_duplex():
    for v in ("both", 1, None, "duplexlong"):
        with pytest.raises(DispatchError):
            parse_options({"options": {"duplex": v}}, "pdf")


def test_parse_options_rejects_bad_paper_and_bin_values():
    # a comma would smuggle extra entries into Sumatra's comma-separated -print-settings
    for v in ("A4,monochrome", "", 4, None, "x" * 65):
        with pytest.raises(DispatchError):
            parse_options({"options": {"paper": v}}, "pdf")
        with pytest.raises(DispatchError):
            parse_options({"options": {"bin": v}}, "pdf")


def test_parse_options_rejects_non_bool_color():
    for v in ("yes", 1, 0):
        with pytest.raises(DispatchError):
            parse_options({"options": {"color": v}}, "pdf")


def test_parse_options_rejects_bad_pages():
    for v in ("", "abc", "1-", "1,,2", "1-3,", 5, True):
        with pytest.raises(DispatchError):
            parse_options({"options": {"pages": v}}, "pdf")


def test_pdf_uri_uses_injected_fetcher_and_is_pdf_mode():
    seen = {}
    def fake(url):
        seen["url"] = url
        return b"%PDF-bytes"
    assert decode_payload({"type": "pdf_uri", "url": "https://x/a.pdf"}, fetch_url=fake) == b"%PDF-bytes"
    assert seen["url"] == "https://x/a.pdf"
    assert agent_mode("pdf_uri") == "pdf"
