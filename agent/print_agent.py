# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
import configparser
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

_UA = "printpapi-agent"


def raw_to_printer(printer, data):
    import win32print  # only on Windows; injected in tests
    h = win32print.OpenPrinter(printer)
    try:
        win32print.StartDocPrinter(h, 1, ("print-agent", None, "RAW"))
        win32print.StartPagePrinter(h)
        win32print.WritePrinter(h, data)
        win32print.EndPagePrinter(h)
        win32print.EndDocPrinter(h)
    finally:
        win32print.ClosePrinter(h)


def pdf_to_printer(printer, data, sumatra="SumatraPDF.exe"):
    fd, path = tempfile.mkstemp(suffix=".pdf")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        subprocess.run([sumatra, "-print-to", printer, "-silent", path], check=True, timeout=60)
    finally:
        try:
            os.remove(path)
        except OSError:
            pass


def raw_to_printer_cups(printer, data, run=subprocess.run):
    # Already-rendered bytes (ZPL/ESC-POS) straight to the queue — CUPS must NOT re-render them.
    run(["lp", "-d", printer, "-o", "raw"], input=data, check=True, timeout=60)


def pdf_to_printer_cups(printer, data, run=subprocess.run):
    # CUPS renders PDF through its own filter chain (gotcha #1: never send PDF as raw).
    run(["lp", "-d", printer], input=data, check=True, timeout=60)


def raw_to_socket(target, data, connect=socket.create_connection, timeout=30):
    # Raw bytes straight to a network printer's socket (e.g. Zebra :9100). Already-rendered only
    # (ZPL/ESC-POS) — gotcha #1: a bare socket has no renderer, so PDF must never reach here.
    addr = target[len("socket://"):] if target.startswith("socket://") else target
    host, _, port = addr.rpartition(":")  # ponytail: IPv4 host:port; no IPv6-in-brackets support
    with connect((host, int(port)), timeout=timeout) as s:
        s.sendall(data)


def select_backend(platform=sys.platform, sumatra="SumatraPDF.exe"):
    """(raw_fn, pdf_fn) for the host OS. Windows: win32print + SumatraPDF; else CUPS lp."""
    if platform.startswith("win"):
        return raw_to_printer, lambda p, d: pdf_to_printer(p, d, sumatra=sumatra)
    return raw_to_printer_cups, pdf_to_printer_cups


def parse_printers(spec):
    """agent.ini 'printers' (semicolon-separated) -> [{name, can_pdf, target}].
    Grammar: name [|pdf] [= target].
      - no '='  -> target is the name (a CUPS queue / Windows printer).
      - '= socket://host:port' -> agent opens a raw TCP socket to it.
    Append '|pdf' to declare a document printer PDF-capable; default is raw-only so a label
    printer is never auto-sent a PDF (gotcha #1). A socket:// target is always raw-only."""
    out = []
    for entry in spec.split(";"):
        left, _, target = entry.partition("=")
        name, _, tag = left.strip().partition("|")
        name = name.strip()
        if not name:
            continue
        target = target.strip() or name
        can_pdf = tag.strip().lower() == "pdf"
        if target.startswith("socket://"):
            can_pdf = False  # gotcha #1: no renderer behind a bare socket
            addr = target[len("socket://"):]
            host, sep, port = addr.rpartition(":")
            if not host or not sep or not port.isdigit():
                raise ValueError(
                    f"invalid socket printer target {target!r} for {name!r}: expected socket://host:port")
        out.append({"name": name, "can_pdf": can_pdf, "target": target})
    return out


def _req(url, key, *, data=None, method="GET", as_bytes=False):
    r = urllib.request.Request(url, data=data, method=method)
    r.add_header("Authorization", f"Bearer {key}")
    r.add_header("User-Agent", _UA)
    if data is not None:
        r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r) as resp:
            raw = resp.read()
            if resp.status == 204:
                return None
            return raw if as_bytes else (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raise OSError(f"server returned {e.code}") from e
    except urllib.error.URLError as e:
        raise OSError(f"connection failed: {e.reason}") from e


def _get(url, key):
    return _req(url, key, method="GET")


def _get_bytes(url, key):
    return _req(url, key, method="GET", as_bytes=True)


def _post(url, key, body):
    return _req(url, key, data=json.dumps(body).encode(), method="POST")


def register(base, key, name, printers, http_post=_post):
    return http_post(base + "/agent/register", key, {"name": name, "printers": printers})


def poll_job(base, key, http_get=_get):
    return http_get(base + "/agent/jobs", key)


def download_payload(base, key, job_id, http_get_bytes=_get_bytes):
    return http_get_bytes(base + f"/agent/jobs/{job_id}/payload", key)


def report_result(base, key, job_id, ok, error=None, http_post=_post):
    http_post(base + f"/agent/jobs/{job_id}/result", key, {"ok": ok, "error": error})


def _report_with_retry(base, key, job_id, ok, error=None, *, http_post=_post,
                       attempts=5, sleep=time.sleep):
    # A lost result makes the server's reaper requeue the job -> duplicate print. Retry hard.
    for i in range(attempts):
        try:
            report_result(base, key, job_id, ok, error, http_post=http_post)
            return True
        except OSError as e:
            print(f"report_result failed (try {i + 1}/{attempts}): {e}", file=sys.stderr)
            if i + 1 < attempts:
                sleep(min(2 ** i, 30))
    return False  # ponytail: after 5 tries we drop the result; reaper requeues -> possible dup


def print_job(mode, entry, data, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer,
              socket_fn=raw_to_socket):
    target = entry["target"]
    if target.startswith("socket://"):
        if mode != "raw":
            raise ValueError("network socket printer is raw-only (cannot render PDF)")
        socket_fn(target, data)
        return
    if mode == "raw":
        raw_fn(target, data)
    elif mode == "pdf":
        pdf_fn(target, data)
    else:
        raise ValueError(f"bad mode: {mode}")


def run_once(base, key, printer_by_id, *, http_get=_get, http_get_bytes=_get_bytes,
             http_post=_post, raw_fn=raw_to_printer, pdf_fn=pdf_to_printer,
             report_sleep=time.sleep):
    job = poll_job(base, key, http_get=http_get)
    if job is None:
        return False
    job_id = job["job_id"]
    printer = printer_by_id.get(job["printer_id"])
    try:
        data = download_payload(base, key, job_id, http_get_bytes=http_get_bytes)
        if printer is None:
            raise ValueError(f"unknown printer id: {job['printer_id']}")
        print_job(job["mode"], printer, data, raw_fn=raw_fn, pdf_fn=pdf_fn)
    except Exception as e:
        _report_with_retry(base, key, job_id, False, str(e), http_post=http_post,
                           sleep=report_sleep)
        return True
    _report_with_retry(base, key, job_id, True, None, http_post=http_post, sleep=report_sleep)
    return True


def load_config(base_dir):
    ini = os.path.join(base_dir, "agent.ini")
    cfg = configparser.ConfigParser()
    if not cfg.read(ini) or "agent" not in cfg:
        raise SystemExit(
            f"missing or invalid {ini}: need an [agent] section with server_url, api_key, printers")
    return cfg["agent"]


def main():
    base_dir = os.path.dirname(sys.executable) if getattr(sys, "frozen", False) \
        else os.path.dirname(os.path.abspath(__file__))
    agent_cfg = load_config(base_dir)
    base = agent_cfg["server_url"].rstrip("/")
    key = agent_cfg["api_key"]
    name = agent_cfg.get("name", "agent")
    bundled = os.path.join(base_dir, "SumatraPDF.exe")
    sumatra = bundled if os.path.exists(bundled) else "SumatraPDF.exe"
    raw_fn, pdf_fn = select_backend(sumatra=sumatra)
    printers = parse_printers(agent_cfg["printers"])
    reg = register(base, key, name, printers)
    entry_by_name = {p["name"]: p for p in printers}
    printer_by_id = {pid: entry_by_name[pname] for pname, pid in reg["printer_ids"].items()}
    print(f"print-agent registered as computer {reg['computer_id']}, printers={printer_by_id}")
    while True:
        try:
            run_once(base, key, printer_by_id, raw_fn=raw_fn, pdf_fn=pdf_fn)
        except Exception as e:
            print(f"poll error: {e}")
            time.sleep(2)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        import traceback
        logdir = os.environ.get("LOCALAPPDATA") or tempfile.gettempdir()
        with open(os.path.join(logdir, "print_agent-error.log"), "a", encoding="utf-8") as f:
            f.write(traceback.format_exc())
        raise
