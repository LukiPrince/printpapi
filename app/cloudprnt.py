# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Star CloudPRNT protocol — pure translation, no IO.

Star's receipt/label printers can poll an HTTP URL by themselves: a periodic `POST` carrying their
status, a `GET` to pull the job data, a `DELETE` to confirm the result. Speaking that protocol makes
the *printer* the agent — nothing is installed on any machine at the site.

The mapping onto our model is one printer = one (pseudo) agent, so a CloudPRNT job takes the same
queue, quota, history and dashboard path as an agent job: poll = `claim_job`, GET = `get_payload`,
DELETE = `finish_job`.

We never transcode. The job's bytes are served exactly as submitted, and the media type is only the
label the printer honours — which is why `media_type` picks from what the client says it accepts.

"Star", "CloudPRNT" and "Star Micronics" are trademarks of Star Micronics Co., Ltd., used here
descriptively to say which protocol this speaks. This is an independent implementation from the
published protocol documentation; no Star code or documentation text was copied.
"""

# What we are willing to label a raw job's bytes as, best first. All of these are byte streams the
# printer executes as-is — image and PDF types are absent on purpose: those would need rendering,
# and rendering is what a CloudPRNT printer has no way to do (gotcha #1).
MEDIA_TYPES = ("application/vnd.star.starprnt", "application/vnd.star.starprntcore",
               "application/vnd.star.line", "application/vnd.star.linematrix",
               "application/vnd.star.raster", "text/plain")
MEDIA_DEFAULT = MEDIA_TYPES[0]


def media_type(accept):
    """The type to serve, from the client's `Accept` header (its supported formats, with q-values).

    Falls back to Star PRNT mode: it is what the current models speak, and a plain-text receipt is
    a valid Star PRNT stream anyway."""
    offered = {p.split(";", 1)[0].strip().lower() for p in (accept or "").split(",")}
    for t in MEDIA_TYPES:
        if t in offered:
            return t
    return MEDIA_DEFAULT


def poll_response(job, media=MEDIA_DEFAULT):
    """The JSON answer to a POST poll: an offer of one job, or nothing to print."""
    if job is None:
        return {"jobReady": False}
    # jobToken is echoed back on the GET and the DELETE, which is how those two name the job.
    return {"jobReady": True, "mediaTypes": [media], "jobToken": str(job["job_id"])}


def job_ok(code):
    """Did the printer's confirmation code report a successful print?

    It sends its ASB status ("200 OK", "420 Cover open", …), and per the guide *every* code
    beginning with 2 means the printer is online and printed — 201 (output paper taken) and 211
    (paper low) are successful prints carrying a warning, not failures. The guide's own example of
    a plain success is the bare "OK". Anything else — including no code at all — is a failed print,
    because reporting a job as printed when it was not is the worse mistake here."""
    c = (code or "").strip()
    return c.upper() == "OK" or c.startswith("2")


# The documented status codes an operator can actually act on. Anything else passes through as the
# printer sent it — a half-guessed description would be worse than the bare number.
_STATUS_TEXT = {
    "410": "out of paper", "411": "paper jam", "412": "roll position error", "420": "cover open",
    "511": "image conversion failed", "520": "job download failed",
    "521": "job too large (512 KB, or 2 MB on mC-Label3/HI01X/HI02X)",
}


def status_text(code):
    """The printer's confirmation code, with a plain-language reason where we know one. This is the
    only diagnostic an operator gets for a device with no agent and no log of its own."""
    c = (code or "").strip()
    if not c:
        return "no status"
    desc = _STATUS_TEXT.get(c.split()[0])
    return f"{c} ({desc})" if desc else c


def device_name(mac):
    """The agent/printer name a device enrols under. Keyed on the MAC address, normalized, because
    that is the one identifier the printer sends on every request of all three methods."""
    return f"cloudprnt-{mac.strip().lower()}"
