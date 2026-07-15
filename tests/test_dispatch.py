import base64
import pytest
from app.dispatch import decode_payload, agent_mode, DispatchError


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


def test_pdf_uri_uses_injected_fetcher_and_is_pdf_mode():
    seen = {}
    def fake(url):
        seen["url"] = url
        return b"%PDF-bytes"
    assert decode_payload({"type": "pdf_uri", "url": "https://x/a.pdf"}, fetch_url=fake) == b"%PDF-bytes"
    assert seen["url"] == "https://x/a.pdf"
    assert agent_mode("pdf_uri") == "pdf"
