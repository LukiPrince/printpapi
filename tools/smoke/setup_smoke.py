#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""CI smoke test of the agent setup: install, autostart, tray icon, a job, restart, uninstall.

On Windows it also runs the steps of "Where the key is stored" in docs/agent.md: the key is sealed,
the documented Python snippet prints it, a patch keeps it sealed, `unprotect-key` and the opt-out,
a blob that cannot be read (the agent waits, offline, with a fresh status file), the repair by hand
and by a connection patch. On every system `read-config` is checked to hand out no key.

With --update-agent, --sign-update and --fake-tray it also tries the signed update (see
`check_updates`): a build that is version 99.0.0 and has the test key is signed with the committed test
seed (`crates/agent/examples/sign_update.rs`), put back when the server is gone (`rolled_back`), installed,
refused a second time (`not_newer`), refused when a file or a program is not what the signed file
says (`bad_file`, `wont_start`), and the nightly schedule of `updates = auto` exists and is removed again
by the uninstall. The installed agent must be a build with the Cargo feature `update-test-key`.

Starts the server, installs the agent with `printpapi-agent install` (a `file://` printer, so no
paper is needed) together with a placeholder tray program, waits until the agent is online, checks
the status file and the tray autostart, sends a raw job and waits for `job-<id>.prn`, runs
`restart` and waits until the agent is online again, then runs `uninstall` and checks that
everything is gone. Runs elevated: administrator on Windows, `sudo` on Linux and macOS.

DO NOT RUN THIS ON A MACHINE YOU CARE ABOUT. It creates a SYSTEM task / systemd unit / launchd
daemon and writes to the real install folder. It is meant for the throwaway CI runners only.

With --msi A.msi --msi-upgrade B.msi (Windows only, two builds of the same .msi, see `check_msi`) it
tests the managed install instead: msiexec installs A with AGENT_INI (a folder with
<COMPUTERNAME>.ini), runs it again, repairs it with another ini, upgrades it with B without
AGENT_INI, removes it, installs without AGENT_INI and with INSTALLDIR=C:/x. msiexec needs
administrator rights and changes the machine: throwaway runners only.

Python 3 stdlib only.
"""
import argparse
import base64
from hashlib import sha256
import json
import os
import platform
import re
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
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


def root_read(path):
    """The bytes of a file the install made readable for root (and the service user) only: the
    runner user is not root off Windows, the agent commands get sudo."""
    if WIN:
        return Path(path).read_bytes()
    try:
        return subprocess.run(["sudo", "cat", str(path)], check=True, capture_output=True,
                              timeout=60).stdout
    except subprocess.CalledProcessError as e:
        raise Fail(f"cannot read {path}: {e.stderr.decode(errors='replace').strip()}")


def root_list(path):
    """The sorted names in a folder of the install that only root may list (`.update` is 0700)."""
    if WIN:
        return sorted(p.name for p in Path(path).iterdir())
    try:
        out = subprocess.run(["sudo", "ls", "-A", str(path)], check=True, capture_output=True,
                             text=True, timeout=60).stdout
    except subprocess.CalledProcessError as e:
        raise Fail(f"cannot list {path}: {e.stderr.strip()}")
    return sorted(out.splitlines())


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


def update_target():
    """`<os>-<arch>` as the signed file names a target (Rust's std::env::consts)."""
    system = {"win32": "windows", "darwin": "macos"}.get(sys.platform, "linux")
    arch = "aarch64" if platform.machine().lower() in ("arm64", "aarch64") else "x86_64"
    return f"{system}-{arch}"


INSTALLED_AGENT = INSTALL_DIR / ("printpapi-agent.exe" if WIN else "printpapi-agent")
MARKER = INSTALL_DIR / ".installed-by-setup"


def make_release(dest, version, agent_bytes, fake_tray, sign):
    """The signed file and the files it names, into `dest` (the staging folder). The tray is the
    placeholder program that prints its version; on macOS it is zipped as an .app."""
    t = update_target()
    ext = ".exe" if WIN else ""
    agent_name = f"printpapi-update-agent-{t}{ext}"
    (dest / agent_name).write_bytes(agent_bytes)
    roles = {"agent": agent_name}
    if sys.platform == "darwin":
        app = dest / "build" / "printpapi Agent.app"
        (app / "Contents" / "MacOS").mkdir(parents=True, exist_ok=True)
        shutil.copy(fake_tray, app / "Contents" / "MacOS" / "printpapi-setup")
        os.chmod(app / "Contents" / "MacOS" / "printpapi-setup", 0o755)
        tray_name = f"printpapi-update-app-{t}.zip"
        subprocess.run(["ditto", "-c", "-k", "--keepParent", str(app), str(dest / tray_name)],
                       check=True)
        shutil.rmtree(dest / "build")
        roles["app"] = tray_name
    else:
        tray_name = f"printpapi-update-tray-{t}{ext}"
        shutil.copy(fake_tray, dest / tray_name)
        roles["tray"] = tray_name
    targets = {}
    for role, name in roles.items():
        data = (dest / name).read_bytes()
        targets[role] = {"file": name, "size": len(data), "sha256": sha256(data).hexdigest()}
    payload = dest / "payload.json"
    payload.write_text(json.dumps({"schema": 1, "version": version, "targets": {t: targets}},
                                  separators=(",", ":")), encoding="utf-8")
    subprocess.run([str(sign), str(payload), str(dest / "printpapi-update.txt")], check=True)
    payload.unlink()


def update_result(staging, command=None):
    """Run `restart --update-from <staging>` of the INSTALLED agent and read what it reports."""
    cmd = [str(INSTALLED_AGENT), "restart", "--update-from", str(staging)]
    p = subprocess.run(cmd if WIN else ["sudo", *cmd], capture_output=True, text=True,
                       timeout=1200)
    print(f"--- update: exit {p.returncode}\n{p.stdout}{p.stderr}".rstrip(), flush=True)
    try:
        return json.loads((staging / "result.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise Fail(f"the update left no readable result.json: {e}")


def clear_release(staging):
    for p in staging.iterdir():
        if p.name != "result.json":
            shutil.rmtree(p, ignore_errors=True) if p.is_dir() else p.unlink()


def schedule_exists():
    """Is the nightly update registered with the system?"""
    if WIN:
        return subprocess.run(["schtasks", "/Query", "/TN", "printpapi-agent-update"],
                              capture_output=True).returncode == 0
    if LINUX:
        return Path("/etc/systemd/system/printpapi-agent-update.timer").is_file()
    return Path("/Library/LaunchDaemons/com.printpapi.agent-update.plist").is_file()


def check_updates(agent, new_agent, sign, fake_tray, staging, base, token, name, work,
                  stop_server, start_server, print_a_job):
    """The signed update on this system, step by step (the five cases of the spec)."""
    old_bytes = INSTALLED_AGENT.read_bytes()
    new_bytes = Path(new_agent).read_bytes()
    ini_before = root_read(agent_ini_path())
    if not root_read(MARKER).decode("utf-8").startswith("version="):
        raise Fail("the marker holds no version")

    # (1) the server goes away as soon as the new agent is in place: the new version cannot
    # connect, so the old one is put back by the updater itself, with no network
    make_release(staging, "99.0.0", new_bytes, fake_tray, sign)
    cmd = [str(INSTALLED_AGENT), "restart", "--update-from", str(staging)]
    proc = subprocess.Popen(cmd if WIN else ["sudo", *cmd], stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)
    try:
        wait_for("the new agent to be put in place",
                 lambda: INSTALLED_AGENT.read_bytes() == new_bytes, timeout=180)
        stop_server()
        wait_for("the old agent to be put back",
                 lambda: INSTALLED_AGENT.read_bytes() == old_bytes, timeout=300)
        start_server()
        out, _ = proc.communicate(timeout=600)
    except BaseException:
        proc.kill()
        raise
    print(f"--- update (server gone): exit {proc.returncode}\n{out}".rstrip(), flush=True)
    result = json.loads((staging / "result.json").read_text(encoding="utf-8"))
    if result.get("ok") or result.get("code") != "rolled_back":
        raise Fail(f"the update without a server must be rolled back: {result}")
    if INSTALLED_AGENT.read_bytes() != old_bytes or root_read(MARKER).decode("utf-8").startswith(
            "version=99"):
        raise Fail("the old agent and the old marker must be back")
    wait_for("the old agent online again",
             lambda: read_status().get("state") == "online" and read_status().get("version") != "99.0.0",
             timeout=120)

    # (2) the same update with the server there
    clear_release(staging)
    make_release(staging, "99.0.0", new_bytes, fake_tray, sign)
    result = update_result(staging)
    if not result.get("ok") or result.get("version") != "99.0.0":
        raise Fail(f"the update failed: {result}")
    wait_for("the new version online",
             lambda: read_status().get("state") == "online" and read_status().get("version") == "99.0.0")
    if INSTALLED_AGENT.read_bytes() != new_bytes:
        raise Fail("the installed agent is not the new one")
    marker = root_read(MARKER).decode("utf-8")
    if marker != "version=99.0.0\n":
        raise Fail(f"the marker is {marker!r}")
    tray_file = TRAY / "Contents" / "MacOS" / "printpapi-setup" if sys.platform == "darwin" else TRAY
    if tray_file.read_bytes() != Path(fake_tray).read_bytes():
        raise Fail("the tray program was not replaced")
    if root_read(agent_ini_path()) != ini_before:
        raise Fail("an update must leave agent.ini byte for byte as it was")
    leftovers = root_list(INSTALL_DIR / ".update")
    if leftovers != ["lock"]:
        raise Fail(f"the private folder holds {leftovers}")
    print_a_job("after the update")

    # (3) again: nothing newer
    clear_release(staging)
    make_release(staging, "99.0.0", new_bytes, fake_tray, sign)
    result = update_result(staging)
    if result.get("code") != "not_newer":
        raise Fail(f"a second update must say not_newer: {result}")

    # (4) a file that is not what the signed file says, and a program that is another version
    clear_release(staging)
    make_release(staging, "99.9.9", new_bytes, fake_tray, sign)
    flipped = bytearray(new_bytes)
    flipped[len(flipped) // 2] ^= 1
    (staging / f"printpapi-update-agent-{update_target()}{'.exe' if WIN else ''}").write_bytes(
        bytes(flipped))
    result = update_result(staging)
    if result.get("code") != "bad_file":
        raise Fail(f"a flipped byte must be bad_file: {result}")
    clear_release(staging)
    make_release(staging, "99.9.9", new_bytes, fake_tray, sign)  # the 99.0.0 program says 99.0.0
    result = update_result(staging)
    if result.get("code") != "wont_start":
        raise Fail(f"a program of another version must be wont_start: {result}")
    if INSTALLED_AGENT.read_bytes() != new_bytes or read_status().get("version") != "99.0.0":
        raise Fail("a refused update changed the installation")
    clear_release(staging)

    # (5) the nightly schedule follows the mode
    result = install_patch(agent, staging, {"updates": "auto"})
    wait_for("the agent online after the change", online_after(result["baseline_local"]), timeout=90)
    if not schedule_exists():
        raise Fail("updates = auto left no schedule")
    return True


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


# ---- the .msi (--msi): the managed install of the setup program, see docs/agent.md

MSI_DIR = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "printpapi Agent Setup"
MSI_EXE = MSI_DIR / "printpapi-setup.exe"
MSI_FLAG = MSI_DIR / ".managed-install"
MSI_REG_KEY = r"HKLM\SOFTWARE\printpapi\Setup"
# msiexec: 0 is done, 3010 is done and a restart is wanted (nothing here needs one)
MSI_OK = (0, 3010)


def read_log(path):
    """A Windows Installer log as text: UTF-16 with a byte order mark when msiexec wrote one, else
    the ANSI text it usually writes."""
    data = Path(path).read_bytes()
    if data[:2] in (b"\xff\xfe", b"\xfe\xff"):
        return data.decode("utf-16", errors="replace")
    return data.decode("cp1252", errors="replace")


def msiexec_cmd(verb, msi, log, *properties):
    """The msiexec command line: silent, no restart, a verbose log, then PROPERTY=value words."""
    return ["msiexec", verb, str(msi), "/qn", "/norestart", "/l*v", str(log), *properties]


def starter_env():
    """The environment msiexec is started with: a proxy that does not exist. The headless modes run as
    LocalSystem and must not see it (crates/setup/src/winenv.rs), or no server is reached."""
    return dict(os.environ, http_proxy="http://127.0.0.1:9", HTTP_PROXY="http://127.0.0.1:9")


def msiexec(verb, msi, log, *properties):
    p = subprocess.run(msiexec_cmd(verb, msi, log, *properties), capture_output=True, text=True,
                       env=starter_env(),
                       timeout=900)
    return p.returncode


def msiexec_ok(what, verb, msi, log, *properties):
    code = msiexec(verb, msi, log, *properties)
    print(f"--- msiexec {verb} ({what}): exit {code}", flush=True)
    if code not in MSI_OK:
        raise Fail(f"msiexec {verb} ({what}) exited {code}; see {log}")


def write_prepared_ini(folder, computer, base, key, target, header_value):
    """<COMPUTERNAME>.ini for one PC: no name (the install fills in the computer's name)."""
    folder.mkdir(parents=True, exist_ok=True)
    (folder / f"{computer}.ini").write_text(
        f"[agent]\nserver_url = {base}\napi_key = {key}\nprinters = archive = {target}\n\n"
        f"[headers]\nX-Smoke = {header_value}\n", encoding="utf-8")


def lock_down_commands(folder):
    """The icacls line of docs/agent.md, "Managed Windows install (.msi)", "Protect the file", for
    one folder, word for word (test_setup_smoke_msi.py compares it with the docs): only SYSTEM and
    the Administrators may use it. A folder that somebody else made is not repaired (no /setowner,
    no /reset /T: they follow a junction in it); the docs put it where users cannot make one."""
    return [["icacls", str(folder), "/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F",
             "*S-1-5-32-544:(OI)(CI)F"]]


def lock_down(folder):
    # an elevated Command Prompt or PowerShell makes files of the Administrators; Git Bash, which runs
    # this in CI, makes them the user's own. Safe here: the folder was made a moment ago by this script
    subprocess.run(["icacls", str(folder), "/setowner", "*S-1-5-32-544", "/T"], check=True,
                   capture_output=True)
    for cmd in lock_down_commands(folder):
        subprocess.run(cmd, check=True, capture_output=True)


def ran_action(log_text, action):
    """Whether a Windows Installer verbose log shows that the custom action was run (and not
    skipped because its condition was false)."""
    return re.search(rf"Doing action: {re.escape(action)}\b", log_text) is not None


def telling_lines(log_text):
    """What a verbose Windows Installer log says about the custom actions and the reason of a
    failure: the reason stands far above the end of the log, which is a list of properties."""
    words = re.compile(r"Managed(Install|Refresh|Uninstall)|--managed-|CustomAction|Return value 3"
                       r"|Error 1[0-9]{3}\b|PinInstallDir|INSTALLDIR = ")
    return [line for line in log_text.splitlines() if words.search(line)]


def managed_log():
    """Where the headless modes write when msiexec runs them: LocalSystem's profile."""
    return (Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "config" / "systemprofile"
            / "AppData" / "Local" / "printpapi-managed.log")


# the names icacls prints for groups a standard user is in (the runners speak English)
EVERYBODY = ("BUILTIN\\Users", "Everyone", "Authenticated Users", "INTERACTIVE")


def open_to_users(icacls_text):
    """The groups of ordinary users that an access list names."""
    return [name for name in EVERYBODY if name.lower() in icacls_text.lower()]


class StagingWatch(threading.Thread):
    """Where the private working folder (printpapi-setup-*, it holds the agent that SYSTEM starts and
    the agent.ini with the key) appears while msiexec runs."""

    def __init__(self, roots):
        super().__init__(daemon=True)
        self.roots, self.seen, self.done = roots, {name: set() for name in roots}, threading.Event()

    def look(self):
        for name, root in self.roots.items():
            try:
                self.seen[name].update(p.name for p in Path(root).glob("printpapi-setup-*"))
            except OSError:
                pass

    def run(self):
        while not self.done.wait(0.05):
            self.look()

    def __exit__(self, *_):
        self.done.set()
        self.join(timeout=10)

    def __enter__(self):
        self.start()
        return self


def run_key(base, token, label):
    return call(base, token, "POST", "/apikeys", {"label": label, "scope": "agent"})["key"]


def reg_value(key, value):
    q = subprocess.run([str(REG), "query", key, "/v", value], capture_output=True, text=True)
    return q.stdout if q.returncode == 0 else None


def sha(path):
    return sha256(Path(path).read_bytes()).hexdigest()


def check_msi(args):
    """Install A with AGENT_INI, run it again, repair it, upgrade it to B without AGENT_INI, remove
    it; then install without AGENT_INI and with INSTALLDIR on the command line (docs/agent.md,
    "Managed Windows install (.msi)"). Needs a server binary, and A and B: the same .msi built twice,
    which gives B another ProductCode and so makes it a real major upgrade of A."""
    msi_a, msi_b = Path(args.msi).resolve(), Path(args.msi_upgrade).resolve()
    server_bin = args.server
    computer = os.environ["COMPUTERNAME"]
    work = Path(tempfile.mkdtemp(prefix="printpapi-msi-smoke-"))
    open_dir(work, 0o755)
    staging, archive, logs = work / "staging", work / "archive", work / "logs"
    for d in (staging, archive, logs):
        open_dir(d, 0o777)
    server_log = work / "server.log"
    token, port = secrets.token_hex(16), free_port()
    base = f"http://127.0.0.1:{port}"
    env = dict(os.environ, PRINTAPI_TOKEN=token, PRINT_DB=str(work / "smoke.db"),
               PRINT_PORT=str(port))
    server = None
    installed = False
    try:
        with server_log.open("ab") as log:
            server = subprocess.Popen([server_bin], env=env, stdout=log, stderr=log)
        wait_for("the server", lambda: urllib.request.urlopen(base + "/health", timeout=2),
                 timeout=30)
        target = archive.resolve().as_uri().replace("%", "%%")
        header_value = "msi-header-" + secrets.token_hex(8)
        key = run_key(base, token, "msi smoke")
        ini_dir = work / "ini"
        write_prepared_ini(ini_dir, computer, base, key, target, header_value)
        lock_down(ini_dir)

        def last_seen():
            for c in call(base, token, "GET", "/computers")["computers"]:
                if c["name"] == computer:
                    return c.get("last_seen_at") or 0
            return 0

        def online_since(baseline):
            return lambda: last_seen() > baseline

        def started_at():
            return json.loads(STATUS_FILE.read_text(encoding="utf-8")).get("started_at") or 0

        def print_a_job(what):
            body = b"^XA^FDmsi smoke " + what.encode() + b"^FS^XZ"
            pid = wait_for("the printer to be listed", lambda: next(
                (p["id"] for p in call(base, token, "GET", "/printers")["printers"]
                 if p["name"] == "archive" and p["agent_name"] == computer), None))
            job = call(base, token, "POST", "/jobs", {
                "printer_id": pid, "type": "raw_base64", "title": what,
                "content": base64.b64encode(body).decode()})["job_id"]
            out = archive / f"job-{job}.prn"
            wait_for(f"{out.name} to arrive ({what})", out.is_file)
            if out.read_bytes() != body:
                raise Fail(f"{out.name} has the wrong content")

        # (0) a folder that is not there: the install fails with one line of LocalSystem's own log, and
        # nothing is written to the profile of whoever started it
        missing = work / "missing"
        if msiexec("/i", msi_a, logs / "0-missing.log", f"AGENT_INI={missing}") in MSI_OK:
            raise Fail("an AGENT_INI that does not exist was accepted")
        if str(missing) not in managed_log().read_text(errors="replace"):
            raise Fail(f"{managed_log()} does not name {missing}")
        rights = subprocess.run(["icacls", str(managed_log().parent)], capture_output=True, text=True,
                                check=True).stdout
        if open_to_users(rights):
            raise Fail(f"{managed_log().parent} is open to {open_to_users(rights)}:\n{rights}")
        theirs = sorted(p.name for p in Path(os.environ["LOCALAPPDATA"]).glob("printpapi-*"))
        if theirs:
            raise Fail(f"LocalSystem wrote to the starter's profile: {theirs}")

        # (1) A with AGENT_INI as a folder: the file of this computer is found, the agent is
        # installed, online and prints. The baseline is taken AFTER msiexec has returned: register
        # itself writes last_seen_at, so an earlier baseline would pass without a running agent.
        installed = True
        with StagingWatch({"system": managed_log().parent,
                           "starter": os.environ["LOCALAPPDATA"]}) as watch:
            msiexec_ok("A, AGENT_INI", "/i", msi_a, logs / "1-install.log", f"AGENT_INI={ini_dir}")
        if not watch.seen["system"] or watch.seen["starter"]:
            raise Fail(f"the working folder of the install was seen in {watch.seen}")
        if not MSI_EXE.is_file() or not MSI_FLAG.is_file():
            raise Fail(f"the install left no {MSI_EXE} or no {MSI_FLAG}")
        if not INSTALL_DIR.is_dir() or not AUTOSTART.is_file() or not TRAY.exists():
            raise Fail(f"the install left no agent in {INSTALL_DIR}")
        if not tray_registered():
            raise Fail("the install left no tray autostart")
        baseline = last_seen()
        wait_for("the managed agent to come online", online_since(baseline))
        wait_for("the status file", STATUS_FILE.is_file)
        check_status_file([key, header_value])
        text = read_installed_ini()
        if not re.search(rf"(?m)^name *= *{re.escape(computer)} *$", text):
            raise Fail("the device name is not the name of this computer")
        if not re.search(r"(?m)^updates *= *off *$", text):
            raise Fail("a managed install must write updates = off")
        if key in text or not re.search(r"(?m)^api_key_dpapi *=", text):
            raise Fail("the key is not stored sealed")
        print_a_job("after the install")
        # the resolved path is in the log; the key is in no log
        log1 = read_log(logs / "1-install.log")
        if not any("--managed-install" in l and str(ini_dir) in l for l in log1.splitlines()):
            raise Fail("the install log does not show the resolved AGENT_INI path")
        if key in log1:
            raise Fail("the key is in the Windows Installer log")

        # (2) the same file again is maintenance mode: a no-op, the agent is not restarted
        ini_hash, started = sha(agent_ini_path()), started_at()
        msiexec_ok("A again", "/i", msi_a, logs / "2-again.log", f"AGENT_INI={ini_dir}")
        if sha(agent_ini_path()) != ini_hash or started_at() != started:
            raise Fail("running the same .msi again changed the agent (it must be a no-op)")

        # (3) a repair with another AGENT_INI must not replace the configuration
        other = work / "ini-other"
        write_prepared_ini(other, computer, base, run_key(base, token, "msi smoke other"),
                           target, header_value)
        lock_down(other)
        msiexec_ok("A, repair", "/fa", msi_a, logs / "3-repair.log", f"AGENT_INI={other}")
        if sha(agent_ini_path()) != ini_hash:
            raise Fail("a repair with another AGENT_INI replaced agent.ini")

        # (4) B without AGENT_INI is a major upgrade: the agent is refreshed, the settings stay
        # a copy runs the change: `install` replaces the installed program, which a running copy of
        # itself cannot be replaced by
        runner = work / "printpapi-agent-copy.exe"
        shutil.copy(INSTALLED_AGENT, runner)
        result = install_patch(runner, staging, {"timeout": 77})
        wait_for("the agent online after a change", online_after(result["baseline_local"]), timeout=90)
        before = started_at()
        # the refresh exists to replace the programs: the agent is the same file in A and B, so what
        # shows that it was replaced is its modification time, and a marker in place of the tray
        agent_mtime = INSTALLED_AGENT.stat().st_mtime_ns
        stale_tray = b"stale tray, to be replaced by the refresh"
        TRAY.write_bytes(stale_tray)
        msiexec_ok("B, upgrade", "/i", msi_b, logs / "4-upgrade.log")
        if INSTALLED_AGENT.stat().st_mtime_ns <= agent_mtime:
            raise Fail("the upgrade did not replace the agent program")
        if TRAY.read_bytes() == stale_tray:
            raise Fail("the upgrade did not replace the tray program")
        wait_for("the refreshed agent to start", lambda: started_at() > before, timeout=90)
        baseline = last_seen()
        wait_for("the refreshed agent to come online", online_since(baseline))
        text = read_installed_ini()
        if (not re.search(r"(?m)^timeout *= *77", text) or not re.search(r"(?m)^updates *= *off *$", text)
                or not re.search(rf"(?m)^name *= *{re.escape(computer)} *$", text)):
            raise Fail("the upgrade did not keep the settings")
        if key in text or not re.search(r"(?m)^api_key_dpapi *=", text):
            raise Fail("the upgrade did not keep the key sealed")
        if not MSI_FLAG.is_file() or not MSI_EXE.is_file():
            raise Fail("the upgrade lost the setup program or the flag")
        print_a_job("after the upgrade")

        # (4) and (5) run with /qn, which has no UI sequence. A double-clicked upgrade and a
        # double-clicked Remove (AppSearch ran unelevated in the UI sequence; the flag property is
        # secure for that) cannot run here: do both by hand on a test PC before a release.

        # (5) removing B takes the agent, its task, its status folder, its own folder and the flag
        msiexec_ok("B, remove", "/x", msi_b, logs / "5-remove.log")
        installed = False
        if not ran_action(read_log(logs / "5-remove.log"), "ManagedUninstall"):
            raise Fail("the removal of a PC with the flag did not run ManagedUninstall")
        left = [p for p in (INSTALL_DIR, AUTOSTART, TRAY, STATUS_DIR, MSI_DIR, MSI_FLAG) if p.exists()]
        if left:
            raise Fail(f"the removal left {left}")
        if tray_registered():
            raise Fail("the removal left the tray autostart")
        if subprocess.run(["schtasks", "/Query", "/TN", "printpapi-agent"],
                          capture_output=True).returncode == 0:
            raise Fail("the removal left the scheduled task")

        # (6) without AGENT_INI only the setup program is installed
        installed = True
        msiexec_ok("B, no AGENT_INI", "/i", msi_b, logs / "6-plain.log")
        if not MSI_EXE.is_file() or INSTALL_DIR.exists() or MSI_FLAG.exists():
            raise Fail("an install without AGENT_INI must install the setup program only")
        msiexec_ok("B, remove", "/x", msi_b, logs / "6-remove.log")
        installed = False
        # no flag: the .msi never installed an agent here, so the unsigned exe is not started at all
        if ran_action(read_log(logs / "6-remove.log"), "ManagedUninstall"):
            raise Fail("the removal of a plain install ran ManagedUninstall")
        if MSI_DIR.exists():
            raise Fail("the removal left the setup program's folder")

        # (7) INSTALLDIR on the command line cannot move the exe out of Program Files
        installed = True
        elsewhere = Path(r"C:\x")
        msiexec_ok("B, INSTALLDIR", "/i", msi_b, logs / "7-installdir.log", "INSTALLDIR=C:\\x\\")
        if not MSI_EXE.is_file() or elsewhere.exists():
            raise Fail("INSTALLDIR on the command line moved the setup program")
        shown = reg_value(MSI_REG_KEY, "InstallDir") or ""
        if str(MSI_DIR).lower() not in shown.lower():
            raise Fail(f"the registry does not name {MSI_DIR}: {shown!r}")
        msiexec_ok("B, remove", "/x", msi_b, logs / "7-remove.log")
        installed = False
        print("setup smoke (msi): ok", flush=True)
        return 0
    except (Fail, OSError, subprocess.SubprocessError) as e:
        print(f"setup smoke (msi) FAILED: {e}", file=sys.stderr, flush=True)
        dump_logs(server_log)
        print("===== printpapi-managed.log (LocalSystem) =====", flush=True)
        try:
            print(managed_log().read_text(errors="replace"))
        except OSError as e2:
            print(f"({e2})")
        for log in sorted(logs.glob("*.log")):
            text = read_log(log)
            print(f"===== {log.name} (custom actions and errors) =====", flush=True)
            print("\n".join(telling_lines(text)[-60:]))
            print(f"===== {log.name} (tail) =====", flush=True)
            print("\n".join(text.splitlines()[-25:]))
        return 1
    finally:
        if installed:  # leave the runner as it was found, whatever happened
            for msi in (msi_b, msi_a):
                try:
                    msiexec("/x", msi, logs / "cleanup.log")
                except (OSError, subprocess.SubprocessError):
                    pass
            if INSTALLED_AGENT.exists():
                try:
                    # a copy again: the installed program cannot be replaced while it runs
                    runner = work / "printpapi-agent-cleanup.exe"
                    shutil.copy(INSTALLED_AGENT, runner)
                    subprocess.run(agent_cmd(runner, "uninstall", staging),
                                   capture_output=True, timeout=300)
                except (OSError, subprocess.SubprocessError):
                    pass
        if server is not None and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
        shutil.rmtree(work, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--server", required=True, help="path of the printpapi server binary")
    ap.add_argument("--agent", help="path of the printpapi-agent binary (not with --msi)")
    ap.add_argument("--msi", help="the .msi (build A): test the managed install of the .msi")
    ap.add_argument("--msi-upgrade", help="the same .msi built a second time (build B), with --msi")
    ap.add_argument("--update-agent", help="the agent built as version 99.0.0 (with the test key)")
    ap.add_argument("--sign-update", help="the sign_update example binary")
    ap.add_argument("--fake-tray", help="the fake_tray example binary")
    args = ap.parse_args()
    if bool(args.msi) != bool(args.msi_upgrade):
        ap.error("--msi and --msi-upgrade come together")
    if args.msi:
        if not WIN:
            ap.error("--msi tests the Windows installer and runs on Windows only")
        return check_msi(args)
    if not args.agent:
        ap.error("--agent is required")
    agent = Path(args.agent).resolve()
    update_files = [args.update_agent, args.sign_update, args.fake_tray]
    if any(update_files) and not all(update_files):
        ap.error("--update-agent, --sign-update and --fake-tray come together")

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
    servers = []
    installed = False
    foreign = None
    try:
        def start_server():
            nonlocal server
            with server_log.open("ab") as log:
                server = subprocess.Popen([args.server], env=env, stdout=log, stderr=log)
            servers.append(server)
            wait_for("the server", lambda: urllib.request.urlopen(base + "/health", timeout=2),
                     timeout=30)

        def stop_server():
            nonlocal server
            if server:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                server = None

        start_server()

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

        if args.update_agent:
            def print_a_job(what):
                body = b"^XA^FDupdate smoke " + what.encode() + b"^FS^XZ"
                job = call(base, token, "POST", "/jobs", {
                    "printer_id": pid, "type": "raw_base64", "title": what,
                    "content": base64.b64encode(body).decode()})["job_id"]
                out = archive / f"job-{job}.prn"
                wait_for(f"{out.name} to arrive {what}", out.is_file)
                if out.read_bytes() != body:
                    raise Fail(f"{out.name} has the wrong content")

            check_updates(agent, args.update_agent, args.sign_update, args.fake_tray, staging,
                          base, token, name, work, stop_server, start_server, print_a_job)
        else:
            print("setup smoke: no update program given, the update steps are skipped",
                  flush=True)

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
        if schedule_exists():
            raise Fail("uninstall left the nightly update schedule")
        if WIN:
            q = subprocess.run(["schtasks", "/Query", "/TN", "printpapi-agent"],
                               capture_output=True)
            if q.returncode == 0:
                raise Fail("uninstall left the scheduled task")
        print("setup smoke: ok", flush=True)
        return 0
    except (Fail, OSError, subprocess.SubprocessError) as e:
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
        for s in servers:
            if s.poll() is None:
                s.terminate()
                try:
                    s.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    s.kill()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
