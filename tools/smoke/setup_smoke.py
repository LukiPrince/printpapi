#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""CI smoke test of the agent setup: install, autostart, tray icon, a job, restart, uninstall.

On Windows it also runs the steps of "Where the key is stored" in docs/agent.md: the key is sealed,
the documented Python snippet prints it, a patch keeps it sealed, `unprotect-key` and the opt-out,
a blob that cannot be read (the agent waits, offline, with a fresh status file), the repair by hand
and by a connection patch. On every system `read-config` is checked to hand out no key.

Starts the server, installs the agent with `printpapi-agent install` (a `file://` printer, so no
paper is needed) together with a placeholder tray program, waits until the agent is online, checks
the status file and the tray autostart, sends a raw job and waits for `job-<id>.prn`, runs
`restart` and waits until the agent is online again, then runs `uninstall` and checks that
everything is gone. Runs elevated: administrator on Windows, `sudo` on Linux and macOS.

DO NOT RUN THIS ON A MACHINE YOU CARE ABOUT. It creates a SYSTEM task / systemd unit / launchd
daemon and writes to the real install folder. It is meant for the throwaway CI runners only.

Python 3 stdlib only.
"""
import argparse
import base64
import json
import os
import re
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

# The status file and the crash log of a setup-installed agent live in one folder. The places are
# those of `crates/agent/src/install.rs` (`layout_with`) and `status.rs` (`status_dir_in`).
TRAY_RUN_KEY = r"HKLM\Software\Microsoft\Windows\CurrentVersion\Run"
TRAY_RUN_VALUE = "printpapi-tray"
if WIN:
    INSTALL_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "printpapi-agent"
    AUTOSTART = INSTALL_DIR / "printpapi-agent-task.xml"
    STATUS_DIR = Path(os.environ.get("ProgramData", r"C:\ProgramData")) / "printpapi-agent"
    TRAY = INSTALL_DIR / "printpapi-tray.exe"
    TRAY_AUTOSTART = None  # the registry, see tray_registered()
    APP_ENTRY = None
    REG = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "reg.exe"
elif LINUX:
    INSTALL_DIR = Path("/opt/printpapi-agent")
    AUTOSTART = Path("/etc/systemd/system/printpapi-agent.service")
    STATUS_DIR = Path("/var/lib/printpapi-agent")
    TRAY = INSTALL_DIR / "printpapi-tray"
    TRAY_AUTOSTART = Path("/etc/xdg/autostart/printpapi-tray.desktop")
    APP_ENTRY = Path("/usr/share/applications/printpapi-agent.desktop")
else:
    INSTALL_DIR = Path("/opt/printpapi-agent")
    AUTOSTART = Path("/Library/LaunchDaemons/com.printpapi.agent.plist")
    STATUS_DIR = Path("/Library/Application Support/printpapi-agent")
    TRAY = Path("/Applications/printpapi Agent.app")
    TRAY_AUTOSTART = Path("/Library/LaunchAgents/com.printpapi.tray.plist")
    APP_ENTRY = None
STATUS_FILE = STATUS_DIR / "status.json"
SERVICE_LOG = STATUS_DIR / "print_agent-error.log"


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


def stage_tray(staging):
    """A placeholder for the tray program: install only copies it, nothing here starts it."""
    tray = staging / "tray"
    if sys.platform == "darwin":  # macOS takes a whole .app folder
        (tray / "Contents" / "MacOS").mkdir(parents=True, exist_ok=True)
        (tray / "Contents" / "MacOS" / "printpapi-setup").write_bytes(b"placeholder")
    else:
        tray.write_bytes(b"placeholder")


def tray_registered():
    """Is the tray icon's autostart entry there? Windows asks the registry, the others a file."""
    if WIN:
        q = subprocess.run([str(REG), "query", TRAY_RUN_KEY, "/v", TRAY_RUN_VALUE],
                           capture_output=True)
        return q.returncode == 0
    return TRAY_AUTOSTART.is_file()


def check_status_file(secret_values):
    """The status file the tray icon reads: JSON, online, and without any secret."""
    text = STATUS_FILE.read_text(encoding="utf-8")
    try:
        status = json.loads(text)
    except ValueError as e:
        raise Fail(f"{STATUS_FILE} is not JSON: {e}")
    if status.get("state") != "online":
        raise Fail(f"the status file does not say online: {status.get('state')!r}")
    for secret in secret_values:
        if secret in text:
            raise Fail("the status file contains a secret (key or header value)")


def plant_foreign_file():
    """A file in the install folder that the installation did not make (a server binary, say).
    The removal must leave it, and the folder with it."""
    foreign = INSTALL_DIR / "printpapi"
    if WIN:
        foreign.write_bytes(b"not the agent's")
    else:
        subprocess.run(["sudo", "sh", "-c", f"echo not the agent > '{foreign}'"], check=True)
    return foreign


def remove_foreign_file(foreign):
    if WIN:
        foreign.unlink(missing_ok=True)
        try:
            INSTALL_DIR.rmdir()
        except OSError:
            pass
    else:
        subprocess.run(["sudo", "rm", "-f", str(foreign)], check=True)
        # the folder lies in root's /opt, so the runner user may not remove it itself
        subprocess.run(["sudo", "rmdir", str(INSTALL_DIR)], check=False)


def agent_ini_path():
    return INSTALL_DIR / "agent.ini"


def read_installed_ini():
    return agent_ini_path().read_bytes().decode("utf-8")


def write_installed_ini(text):
    # in place: the rights of the file stay as the install made them
    agent_ini_path().write_bytes(text.encode("utf-8"))


def install_patch(agent, staging, patch):
    """Change the installed agent with a patch. A staged agent.ini wins over it, so it goes first."""
    (staging / "agent.ini").unlink(missing_ok=True)
    (staging / "patch.json").write_text(json.dumps(patch), encoding="utf-8")
    try:
        return run_agent(agent, "install", staging)
    finally:
        (staging / "patch.json").unlink(missing_ok=True)


def read_status():
    return json.loads(STATUS_FILE.read_text(encoding="utf-8"))


def online_after(local_time):
    """Online means: the status file says online with a contact after `local_time`."""
    def check():
        status = read_status()
        if status.get("state") == "online" and (status.get("last_contact") or 0) > local_time:
            return status
    return check


def documented_key_snippet():
    """The Python block of docs/agent.md that reads the stored key, found by its marker comment, so
    that the text that is documented is the text that runs."""
    docs = (Path(__file__).resolve().parents[2] / "docs" / "agent.md").read_text(encoding="utf-8")
    for block in re.findall(r"```python\n(.*?)```", docs, flags=re.S):
        if "# printpapi: show the stored agent key" in block:
            return block
    raise Fail("docs/agent.md has no python block with the marker comment")


def check_key_at_rest(agent, base, token, staging, name, key, header_value):
    """Windows: the key is stored encrypted, survives changes, and the way back and the recovery
    work (docs/agent.md, "Where the key is stored")."""
    def computer_id():
        for c in call(base, token, "GET", "/computers")["computers"]:
            if c["name"] == name:
                return c["id"]
    same_id = computer_id()

    def key_line():
        return re.search(r"(?m)^api_key *=", read_installed_ini())

    def blob_line():
        return re.search(r"(?m)^api_key_dpapi *= *([0-9a-f]+)", read_installed_ini())

    # 1. sealed by the install, and the SYSTEM agent reads it
    if key in read_installed_ini() or key_line() or not blob_line():
        raise Fail("agent.ini does not hold the key as api_key_dpapi only")
    blob = blob_line().group(1)

    # 2. the documented snippet prints the key
    shown = subprocess.run([sys.executable, "-I", "-"], input=documented_key_snippet(),
                           capture_output=True, text=True, timeout=60)
    if shown.returncode != 0 or shown.stdout.strip() != key:
        raise Fail(f"the documented snippet did not print the key: {shown.stderr[-300:]}")

    # 3. a change keeps it sealed, the device the same
    result = install_patch(agent, staging, {"timeout": 45})
    wait_for("the agent online after a patch", online_after(result["baseline_local"]), timeout=90)
    if result.get("key_sealed") is not True or not blob_line() or computer_id() != same_id:
        raise Fail(f"a patch did not keep the key sealed: {result}")

    # 4. unprotect-key: plain, opted out, and later changes leave it plain
    done = subprocess.run([str(agent), "unprotect-key"], capture_output=True, text=True,
                          timeout=120)
    print(f"--- unprotect-key: exit {done.returncode}\n{done.stdout}{done.stderr}".rstrip(),
          flush=True)
    text = read_installed_ini()
    if (done.returncode != 0 or key in done.stdout + done.stderr or f"api_key = {key}" not in text
            or not re.search(r"(?m)^seal_key *= *no", text) or blob_line()):
        raise Fail("unprotect-key did not leave the key as plain text with seal_key = no")
    result = install_patch(agent, staging, {"timeout": 60})
    wait_for("the agent online after a patch", online_after(result["baseline_local"]), timeout=90)
    if result.get("key_sealed") is not False or f"api_key = {key}" not in read_installed_ini():
        raise Fail(f"a patch sealed a file that opted out: {result}")

    # 5. the opt-out is a line: without it the next change seals again
    write_installed_ini(re.sub(r"(?m)^seal_key *=.*\r?\n", "", read_installed_ini()))
    result = install_patch(agent, staging, {"timeout": 45})
    wait_for("the agent online after a patch", online_after(result["baseline_local"]), timeout=90)
    if result.get("key_sealed") is not True or not blob_line():
        raise Fail(f"deleting seal_key did not seal again: {result}")

    # 6. a blob that cannot be read: the agent stays up, offline, with a fresh status file. Every
    # seal makes a new blob, so the secrets to look for are the one in the file now and the zeros
    # that replace it.
    good = read_installed_ini()
    blob = blob_line().group(1)
    zeros = "00" * 8
    write_installed_ini(re.sub(r"(?m)^(api_key_dpapi *= *)[0-9a-f]+", lambda m: m.group(1) + zeros,
                               good))
    run_agent(agent, "restart", staging)
    time.sleep(130)
    status = read_status()
    if time.time() - STATUS_FILE.stat().st_mtime > 60:
        raise Fail("the status file went stale while the key cannot be read")
    if (status.get("state") != "offline" or status.get("key_unreadable") is not True
            or not status.get("printers")):
        raise Fail(f"the status does not show an unreadable key with the printers: {status}")
    raw = STATUS_FILE.read_text(encoding="utf-8")
    if any(secret in raw for secret in (key, header_value, blob, zeros)):
        raise Fail("the status file contains a secret")

    # 7. the old line back, no restart: the agent notices within two minutes
    put_back = time.time()
    write_installed_ini(good)
    wait_for("the agent to read the key again", online_after(put_back), timeout=120)
    if read_status().get("key_unreadable"):
        raise Fail("the status still says the key is unreadable")

    # 8. damaged again: entering the same connection repairs it, same device
    write_installed_ini(re.sub(r"(?m)^(api_key_dpapi *= *)[0-9a-f]+", lambda m: m.group(1) + "00" * 8,
                               good))
    result = install_patch(agent, staging, {"server_url": base, "api_key": key, "name": name})
    wait_for("the agent online after the repair", online_after(result["baseline_local"]),
             timeout=90)
    if result.get("key_sealed") is not True or computer_id() != same_id:
        raise Fail(f"the connection patch did not repair the key: {result}")


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
    foreign = None
    try:
        with server_log.open("wb") as log:
            server = subprocess.Popen([args.server], env=env, stdout=log, stderr=log)
        wait_for("the server", lambda: urllib.request.urlopen(base + "/health", timeout=2),
                 timeout=30)

        key = call(base, token, "POST", "/apikeys", {"label": "setup smoke"})["key"]
        name = "smoke-" + secrets.token_hex(3)
        # the ini parser reads `%` as an interpolation, so a percent-escape in the URI needs %%
        target = archive.resolve().as_uri().replace("%", "%%")
        # a header value is a secret as well: it must not reach the status file
        header_value = "smoke-header-" + secrets.token_hex(8)
        (staging / "agent.ini").write_text(
            f"[agent]\nserver_url = {base}\napi_key = {key}\nname = {name}\n"
            f"printers = archive = {target}\n\n[headers]\nX-Smoke = {header_value}\n",
            encoding="utf-8")

        # twice: a second run over a finished (or half-finished) install has to work as well
        installed = True
        stage_tray(staging)
        run_agent(agent, "install", staging)
        stage_tray(staging)  # the first run may have used up what is staged
        result = run_agent(agent, "install", staging)
        baseline = result["baseline_last_seen"]
        if not INSTALL_DIR.is_dir() or not AUTOSTART.is_file():
            raise Fail(f"install left no {INSTALL_DIR} / {AUTOSTART}")
        if not TRAY.exists() or not tray_registered():
            raise Fail(f"install left no tray program {TRAY} or no tray autostart")
        if APP_ENTRY is not None and not APP_ENTRY.is_file():
            raise Fail(f"install left no {APP_ENTRY}")
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
        wait_for("the status file", STATUS_FILE.is_file)
        check_status_file([key, header_value])

        # every system: read-config hands out the headers and never the key
        run_agent(agent, "read-config", staging)
        copy = (staging / "agent.ini").read_text(encoding="utf-8")
        (staging / "agent.ini").unlink()
        if header_value not in copy or key in copy:
            raise Fail("the read-config copy must hold the header value and not the key")

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

        if WIN:
            if result.get("key_sealed") is not True:
                raise Fail(f"install did not report key_sealed: {result}")
            check_key_at_rest(agent, base, token, staging, name, key, header_value)

        # restart: a new process says so itself. Its status file holds the start time of the
        # process, which the old process cannot have written (a newer last_seen_at on the server
        # could still come from the old one, which long-polls until it is stopped)
        def started_at():
            return json.loads(STATUS_FILE.read_text(encoding="utf-8")).get("started_at") or 0
        before = started_at()
        if not before:
            raise Fail("the status file has no started_at")
        result = run_agent(agent, "restart", staging)
        if not result.get("baseline_local"):
            raise Fail(f"restart reported no baseline_local: {result}")
        wait_for("the restarted agent to write a new started_at", lambda: started_at() > before)
        if started_at() < result["baseline_local"] - 1:
            raise Fail("the new started_at is older than the restart")

        foreign = plant_foreign_file()
        run_agent(agent, "uninstall", staging)
        installed = False
        # only what the install made is deleted: the foreign file and its folder stay
        if sorted(p.name for p in INSTALL_DIR.iterdir()) != [foreign.name]:
            raise Fail(f"uninstall left {sorted(INSTALL_DIR.iterdir())} in {INSTALL_DIR}")
        remove_foreign_file(foreign)
        foreign = None
        left = [p for p in (INSTALL_DIR, AUTOSTART, TRAY, STATUS_DIR, TRAY_AUTOSTART, APP_ENTRY)
                if p is not None and p.exists()]
        if left:
            raise Fail(f"uninstall left {left}")
        if tray_registered():
            raise Fail("uninstall left the tray autostart")
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
        if foreign is not None:
            try:
                remove_foreign_file(foreign)
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
