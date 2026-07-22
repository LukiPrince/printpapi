# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
import base64
import binascii
import json
import urllib.request
from urllib.parse import urlparse


class DispatchError(Exception):
    pass


class FetchError(DispatchError):
    pass


# Browser-like UA: WAF/CDN-fronted services (e.g. Cloudflare) reject the default
# "Python-urllib/x.y" agent with 403. Mozilla/5.0 passes.
_USER_AGENT = "Mozilla/5.0 (print-api)"


def _http_get(url, timeout=30):
    req = urllib.request.Request(url, headers={"User-Agent": _USER_AGENT})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def _http_post(url, json_body, timeout=30):
    data = json.dumps(json_body).encode()
    req = urllib.request.Request(
        url, data=data, method="POST",
        headers={"User-Agent": _USER_AGENT, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def _checked_http_url(url):
    if not url:
        raise DispatchError("missing 'url'")
    if urlparse(url).scheme not in ("http", "https"):
        raise DispatchError(f"unsupported url scheme: {url!r}")
    return url


def decode_payload(body, fetch_url=_http_get, post_fetch=_http_post):
    t = body.get("type")
    if t in ("raw_base64", "pdf_base64"):
        content = body.get("content")
        if not content:
            raise DispatchError("missing 'content'")
        try:
            return base64.b64decode(content, validate=True)
        except (binascii.Error, ValueError) as e:
            raise DispatchError(f"bad base64: {e}")
    if t in ("raw_uri", "pdf_uri"):
        url = _checked_http_url(body.get("url"))
        try:
            return fetch_url(url)
        except Exception as e:
            raise FetchError(f"fetch failed: {e}") from e
    # *_uri_post: the server POSTs `json` to `url` (e.g. a PDF-rendering service)
    # and prints the response bytes. Avoids round-tripping binary through the caller.
    if t in ("raw_uri_post", "pdf_uri_post"):
        url = _checked_http_url(body.get("url"))
        try:
            return post_fetch(url, body.get("json", {}))
        except Exception as e:
            raise FetchError(f"post fetch failed: {e}") from e
    raise DispatchError(f"unknown type: {t!r}")


def agent_mode(type_):
    return "pdf" if type_ in ("pdf_base64", "pdf_uri_post", "pdf_uri") else "raw"


_MAX_COPIES = 100  # ponytail: flat cap so one job can't spool 10k prints; env-tunable if ever needed


def parse_copies(body):
    """Optional 'copies' from a job body -> int in 1.._MAX_COPIES (default 1).
    Absent/null -> 1. type(c) is int rejects bool/float/str at the trust boundary."""
    c = body.get("copies")
    if c is None:
        return 1
    if type(c) is not int or not (1 <= c <= _MAX_COPIES):
        raise DispatchError(f"copies must be an integer 1..{_MAX_COPIES}")
    return c
