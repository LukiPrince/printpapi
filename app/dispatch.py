# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
import base64
import binascii
import json
import re
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


_OPTION_KEYS = ("duplex", "paper", "bin", "color", "pages")
_OPTION_DUPLEX = ("long-edge", "short-edge", "one-sided")
# No comma: paper/bin land in Sumatra's comma-separated -print-settings list, so a comma in a
# value would smuggle extra settings past validation. \w covers letters/digits/_.
_OPTION_VALUE = re.compile(r"^[\w. -]{1,64}$")
_OPTION_PAGES = re.compile(r"^\d+(-\d+)?(,\d+(-\d+)?)*$")


def parse_options(body, mode):
    """Optional 'options' from a job body -> validated dict, or None if absent/empty.
    pdf jobs only — raw payloads (ZPL/ESC-POS) carry their own layout, a renderer never sees them.
    Keys: duplex ('long-edge'|'short-edge'|'one-sided'), paper (e.g. 'A4'), bin (tray name),
    color (bool), pages (e.g. '1-3,5'). The agent maps these onto SumatraPDF -print-settings
    (Windows) or lp -o (CUPS)."""
    o = body.get("options")
    if o is None:
        return None
    if type(o) is not dict:
        raise DispatchError("options must be an object")
    if not o:
        return None
    if mode != "pdf":
        raise DispatchError("options are only supported on pdf jobs")
    unknown = sorted(set(o) - set(_OPTION_KEYS))
    if unknown:
        raise DispatchError(f"unknown option(s): {', '.join(unknown)}")
    if "duplex" in o and o["duplex"] not in _OPTION_DUPLEX:
        raise DispatchError(f"duplex must be one of {', '.join(_OPTION_DUPLEX)}")
    for k in ("paper", "bin"):
        if k in o and (type(o[k]) is not str or not _OPTION_VALUE.match(o[k])):
            raise DispatchError(f"{k} must be 1-64 chars of letters/digits/space/._-")
    if "color" in o and type(o["color"]) is not bool:
        raise DispatchError("color must be a boolean")
    if "pages" in o and (type(o["pages"]) is not str or not _OPTION_PAGES.match(o["pages"])):
        raise DispatchError("pages must be ranges like '1-3,5'")
    return dict(o)


def parse_callback_url(body):
    """Optional 'callback_url' -> validated http(s) URL, or None if absent/empty.
    The server POSTs job-state changes here (see the webhook dispatcher). http(s)-only.
    # ponytail: no private-IP/SSRF block — same authenticated-client trust as raw_uri/pdf_uri_post;
    # add an IP-range denylist here if untrusted clients ever get keys."""
    url = body.get("callback_url")
    if not url:
        return None
    if type(url) is not str:                     # a truthy non-string must 400, not crash urlparse
        raise DispatchError(f"callback_url must be a string: {url!r}")
    if urlparse(url).scheme not in ("http", "https"):
        raise DispatchError(f"callback_url must be http(s): {url!r}")
    return url
