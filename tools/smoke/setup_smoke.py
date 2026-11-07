#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""CI smoke test of the agent setup: install, autostart, a job that arrives, uninstall.

Starts the server, installs the agent with `printpapi-agent install` (a `file://` printer, so no
paper is needed), waits until the agent is online, sends a raw job and waits for `job-<id>.prn`,
then runs `uninstall`. Runs elevated: administrator on Windows, `sudo` on Linux and macOS.

DO NOT RUN THIS ON A MACHINE YOU CARE ABOUT. It creates a SYSTEM task / systemd unit / launchd
daemon and writes to the real install folder. It is meant for the throwaway CI runners only.

Python 3 stdlib only.
"""
import argparse
import base64
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

WIN = os.name == "nt"
LINUX = sys.platform.startswith("linux")
WAIT_S = 60

if WIN:
    INSTALL_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "printpapi"
    AUTOSTART = INSTALL_DIR / "printpapi-agent-task.xml"
    SERVICE_LOG = (Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "config"
                   / "systemprofile" / "AppData" / "Local" / "print_agent-error.log")
elif LINUX:
    INSTALL_DIR = Path("/opt/printpapi")
    AUTOSTART = Path("/etc/systemd/system/printpapi-agent.service")
    SERVICE_LOG = Path("/tmp/print_agent-error.log")
else:
    INSTALL_DIR = Path("/opt/printpapi")
    AUTOSTART = Path("/Library/LaunchDaemons/com.printpapi.agent.plist")
    SERVICE_LOG = Path("/var/log/printpapi-agent.log")


class Fail(Exception):
    pass


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def call(base, token, method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raise Fail(f"{method} {path} answered {e.code}: {e.read()[:300]!r}")


def wait_for(what, check, timeout=WAIT_S):
    end = time.time() + timeout
    while time.time() < end:
        try:
            value = check()
        except (OSError, urllib.error.URLError):
            value = None  # the server or a file is not there yet
        if value:
            return value
        time.sleep(1)
    raise Fail(f"timed out after {timeout} s waiting for {what}")


def open_dir(path, mode):
    # chmod explicitly: mkdir's mode is cut by the umask, and the service user (Linux) or root
    # (macOS) has to get in whatever the runner's umask is
    path.mkdir(parents=True, exist_ok=True)
    if not WIN:
        os.chmod(path, mode)


def agent_cmd(agent, sub, staging):
    cmd = [str(agent), sub, str(staging)]
    return cmd if WIN else ["sudo", *cmd]


def run_agent(agent, sub, staging):
    p = subprocess.run(agent_cmd(agent, sub, staging), capture_output=True, text=True,
                       timeout=300)
    print(f"--- {sub}: exit {p.returncode}\n{p.stdout}{p.stderr}".rstrip(), flush=True)
    try:
        result = json.loads((staging / "result.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise Fail(f"{sub} left no readable result.json: {e}")
    if p.returncode != 0 or not result.get("ok"):
        raise Fail(f"{sub} failed: {result}")
    return result


def dump_logs(server_log):
    print("\n===== server log =====", flush=True)
    print(server_log.read_text(errors="replace") if server_log.exists() else "(none)")
    print("===== agent crash log =====", flush=True)
    try:
        print(SERVICE_LOG.read_text(errors="replace"))
    except OSError as e:
        print(f"({SERVICE_LOG}: {e})")
    if LINUX:
        subprocess.run(["sudo", "journalctl", "-u", "printpapi-agent", "--no-pager", "-n", "50"])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--server", required=True, help="path of the printpapi server binary")
    ap.add_argument("--agent", required=True, help="path of the printpapi-agent binary")
    args = ap.parse_args()
    agent = Path(args.agent).resolve()

    work = Path(tempfile.mkdtemp(prefix="printpapi-smoke-"))
    # every folder the service touches sits under one parent it can walk through
    open_dir(work, 0o755)
    staging, archive = work / "staging", work / "archive"
    open_dir(staging, 0o777)
    open_dir(archive, 0o777)
    server_log = work / "server.log"
    token, port = secrets.token_hex(16), free_port()
    base = f"http://127.0.0.1:{port}"
    env = dict(os.environ, PRINTAPI_TOKEN=token, PRINT_DB=str(work / "smoke.db"),
               PRINT_PORT=str(port))
    server = None
    installed = False
    try:
        with server_log.open("wb") as log:
            server = subprocess.Popen([args.server], env=env, stdout=log, stderr=log)
        wait_for("the server", lambda: urllib.request.urlopen(base + "/health", timeout=2),
                 timeout=30)

        key = call(base, token, "POST", "/apikeys", {"label": "setup smoke"})["key"]
        name = "smoke-" + secrets.token_hex(3)
        # the ini parser reads `%` as an interpolation, so a percent-escape in the URI needs %%
        target = archive.resolve().as_uri().replace("%", "%%")
        (staging / "agent.ini").write_text(
            f"[agent]\nserver_url = {base}\napi_key = {key}\nname = {name}\n"
            f"printers = archive = {target}\n", encoding="utf-8")

        # twice: a second run over a finished (or half-finished) install has to work as well
        installed = True
        run_agent(agent, "install", staging)
        result = run_agent(agent, "install", staging)
        baseline = result["baseline_last_seen"]
        if not INSTALL_DIR.is_dir() or not AUTOSTART.is_file():
            raise Fail(f"install left no {INSTALL_DIR} / {AUTOSTART}")
        if not WIN:
            # the service runs the binary as root (macOS) or as a user that must not own it: a copy
            # that kept the starting user's ownership would let that user replace what root runs
            for p in (INSTALL_DIR, INSTALL_DIR / "printpapi-agent"):
                if p.stat().st_uid != 0:
                    raise Fail(f"{p} is not owned by root")

        def online():
            for c in call(base, token, "GET", "/computers")["computers"]:
                if c["name"] == name and (c.get("last_seen_at") or 0) > baseline:
                    return c
        wait_for("the agent to come online", online)

        def printer_id():
            for p in call(base, token, "GET", "/printers")["printers"]:
                if p["name"] == "archive" and p["agent_name"] == name:
                    return p["id"]
        pid = wait_for("the printer to be listed", printer_id)
        zpl = b"^XA^FDsetup smoke^FS^XZ"
        job = call(base, token, "POST", "/jobs", {
            "printer_id": pid, "type": "raw_base64", "title": "setup smoke",
            "content": base64.b64encode(zpl).decode()})["job_id"]
        out = archive / f"job-{job}.prn"
        wait_for(f"{out.name} to arrive", out.is_file)
        if out.read_bytes() != zpl:
            raise Fail(f"{out.name} has the wrong content")

        run_agent(agent, "uninstall", staging)
        installed = False
        left = [p for p in (INSTALL_DIR, AUTOSTART) if p.exists()]
        if left:
            raise Fail(f"uninstall left {left}")
        if WIN:
            q = subprocess.run(["schtasks", "/Query", "/TN", "printpapi-agent"],
                               capture_output=True)
            if q.returncode == 0:
                raise Fail("uninstall left the scheduled task")
        print("setup smoke: ok", flush=True)
        return 0
    except (Fail, subprocess.SubprocessError) as e:
        print(f"setup smoke FAILED: {e}", file=sys.stderr, flush=True)
        dump_logs(server_log)
        return 1
    finally:
        if installed:  # leave the runner as it was found, whatever happened
            try:
                subprocess.run(agent_cmd(agent, "uninstall", staging), capture_output=True,
                               timeout=300)
            except (OSError, subprocess.SubprocessError):
                pass
        if server:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
