# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""PrintNode-compatible API layer.

The same server and the same job store, dressed in the JSON shapes PrintNode's clients expect, so
an existing plugin or SDK can be pointed at a printpapi base URL and keep working.

It is selected by *auth scheme*: PrintNode carries the API key as the HTTP Basic username, so a
`Basic` header means "answer in PrintNode's shapes" and `Bearer` keeps printpapi's own. That lets
`/printers` and `/computers` serve both shapes without a URL prefix — which matters, because the
SDKs let you override the host but not the paths.

Everything here is a pure translation of data the store already has: no schema change, no agent
change, no second code path for printing.

PrintNode is a trademark of PrintNode Ltd. printpapi is not affiliated with, endorsed by, or
sponsored by PrintNode. This layer was written from the publicly documented API surface; no
PrintNode source code or documentation text was copied into it.
"""
import base64
import binascii
from datetime import datetime, timezone

from app.dispatch import _MAX_COPIES


class CompatError(Exception):
    """A request this layer cannot translate — the caller maps it to 400."""


def basic_key(header):
    """The API key out of an `Authorization: Basic` header. PrintNode's clients send the key as the
    username with an empty password, so only the username half matters. `''` if unusable."""
    if not header.startswith("Basic "):
        return ""
    try:
        raw = base64.b64decode(header[6:].strip(), validate=True)
    except (binascii.Error, ValueError):
        return ""
    return raw.decode("utf-8", "replace").split(":", 1)[0]


def _iso(ts):
    """Epoch seconds -> the ISO-8601 UTC millisecond form PrintNode timestamps use."""
    if ts is None:
        return None
    return (datetime.fromtimestamp(ts, timezone.utc)
            .isoformat(timespec="milliseconds").replace("+00:00", "Z"))


_MAX_SET = 500   # stays clear of SQLite's bound-parameter limit (999 on builds before 3.32)


def parse_set(spec):
    """PrintNode addresses collections by id set in the path: `10`, `10,12`, `5-9`, or a mix.
    Bounded: a range is a shorthand for a handful of ids, not an invitation to materialise
    `1-999999999`."""
    ids = []
    for part in spec.split(","):
        lo, _, hi = part.strip().partition("-")
        try:
            lo = int(lo)
            hi = int(hi) if hi else lo
        except ValueError:
            raise CompatError(f"bad id set: {spec!r}")
        if hi < lo or len(ids) + (hi - lo + 1) > _MAX_SET:
            raise CompatError(f"id set too large or reversed: {spec!r}")
        ids.extend(range(lo, hi + 1))
    if not ids:
        raise CompatError(f"bad id set: {spec!r}")
    return ids


def capabilities(caps):
    """Agent-reported capabilities -> PrintNode's capability object. What the agent cannot discover
    is reported empty instead of invented — paper *dimensions* in particular: we know the names, not
    the micrometre extents, so every paper maps to null."""
    if not caps:
        return None
    return {
        "bins": list(caps.get("bins") or []),
        "collate": False,
        "color": bool(caps.get("color")),
        "copies": _MAX_COPIES,
        "dpis": [],
        "duplex": bool(caps.get("duplex")),
        "extent": [],
        "medias": [],
        "nup": [],
        "papers": {name: None for name in caps.get("papers") or []},
        "printrate": None,
        "supports_custom_paper_size": False,
    }


def computer(agent):
    """A `store.list_agents()` row -> PrintNode computer object. The network fields are null: our
    agent reports its name and its printers, not its interfaces."""
    return {"id": agent["id"], "name": agent["name"], "inet": None, "inet6": None,
            "hostname": agent["name"], "version": "", "jre": None,
            "createTimestamp": _iso(agent["created_at"]),
            "state": "connected" if agent["online"] else "disconnected"}


def printer(p, comp=None):
    """A `store.list_printers()` row (plus its computer object) -> PrintNode printer object."""
    return {"id": p["id"], "name": p["name"], "description": p["name"],
            "computer": comp or {"id": p["agent_id"], "name": p["agent_name"],
                                 "state": "connected" if p["online"] else "disconnected"},
            "capabilities": capabilities(p.get("capabilities")),
            "default": False, "createTimestamp": _iso(p.get("created_at")),
            "state": "online" if p["online"] else "offline"}


_STATES = {"queued": "queued", "claimed": "sent", "done": "done", "cancelled": "deleted"}


def job_state(state, error=None):
    """Our job state -> PrintNode's. `failed` splits in two over there (a missed deadline is
    `expired`), and a cancelled job is `deleted` — they have no separate cancelled state."""
    if state == "failed":
        return "expired" if error == "expired" else "error"
    return _STATES.get(state, state)


# We do not persist the client's `source` string, so every job reports its printer of origin.
_SOURCE = "printpapi"


def printjob(job, printer_obj=None):
    """A `store.recent_jobs()` row -> PrintNode print job object."""
    return {"id": job["id"],
            "printer": printer_obj or {"id": job["printer_id"], "name": job["printer_name"]},
            "title": job["title"] or "", "contentType": job["type"], "source": _SOURCE,
            "expireAt": None, "createTimestamp": _iso(job["created_at"]),
            "state": job_state(job["state"], job["error"])}


def printjob_states(job):
    """The per-job state *history* PrintNode reports. We keep only the current state, so this is a
    single entry — enough for the clients that poll it waiting for a terminal state."""
    return [{"printJobId": job["id"], "state": job_state(job["state"], job["error"]),
             "message": job["error"] or "", "clientVersion": "",
             "createTimestamp": _iso(job["finished_at"] or job["created_at"])}]


def whoami(org_id, metrics, online_names):
    """The account object clients fetch first to check their credentials. Counts come from the
    metrics snapshot; the billing fields are null because there is no billing here."""
    return {"id": org_id or 0, "firstname": "printpapi", "lastname": "", "email": "",
            "canCreateSubAccounts": False, "creatorEmail": "", "childAccounts": [],
            "credits": None, "numComputers": metrics["agents_total"],
            "totalPrints": metrics["jobs"].get("done", 0), "versions": {},
            "connected": online_names, "Tags": {}, "ActiveSubscriptions": [], "state": "active"}


_CONTENT_TYPES = ("pdf_uri", "pdf_base64", "raw_uri", "raw_base64")
_OPTIONS = ("paper", "bin", "color", "duplex", "pages")   # the subset the agent can actually apply


def job_body(pn):
    """A PrintNode `POST /printjobs` body -> a printpapi `POST /jobs` body.

    Unknown option keys are dropped rather than rejected, and options on a raw job are dropped
    whole: a plugin sends its entire option set (rotate, dpi, fit_to_page, …) with every job, and a
    400 over one we do not implement would defeat the point of the layer. What we *do* map is
    validated by the normal `POST /jobs` path — this function invents no leniency about values."""
    if not isinstance(pn, dict):
        raise CompatError("body must be an object")
    ct = pn.get("contentType")
    if ct not in _CONTENT_TYPES:
        raise CompatError(f"unsupported contentType: {ct!r} "
                          f"(supported: {', '.join(_CONTENT_TYPES)})")
    body = {"type": ct, "printer_id": pn.get("printerId"), "title": pn.get("title")}
    # The URL rides in `content` on their side; ours takes it in `url`.
    body["url" if ct.endswith("_uri") else "content"] = pn.get("content")
    opts = pn.get("options") if isinstance(pn.get("options"), dict) else {}
    qty = pn.get("qty", opts.get("copies"))
    if qty is not None:
        body["copies"] = qty
    if pn.get("expireAfter") is not None:
        body["expire_after"] = pn["expireAfter"]
    if ct.startswith("pdf"):
        keep = {k: opts[k] for k in _OPTIONS if k in opts}
        if keep:
            body["options"] = keep
    return body
