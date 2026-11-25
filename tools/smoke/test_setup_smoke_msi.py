# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""The pure parts of the --msi mode of setup_smoke.py. The mode itself runs msiexec and needs a
throwaway Windows runner (ci.yml, release.yml); nothing here starts it."""
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import setup_smoke as smoke  # noqa: E402


def test_msiexec_is_silent_logs_verbosely_and_takes_the_properties_last():
    cmd = smoke.msiexec_cmd("/i", Path("a.msi"), Path("x.log"), "AGENT_INI=C:\\ini", "INSTALLDIR=C:\\x\\")
    assert cmd == ["msiexec", "/i", "a.msi", "/qn", "/norestart", "/l*v", "x.log",
                   "AGENT_INI=C:\\ini", "INSTALLDIR=C:\\x\\"]
    assert smoke.msiexec_cmd("/x", Path("a.msi"), Path("x.log"))[-1] == "x.log"


def test_a_log_is_read_as_utf_16_with_a_mark_or_as_ansi(tmp_path):
    text = "Action ManagedInstall: --managed-install \"C:\\Küche\""
    utf16 = tmp_path / "u.log"
    utf16.write_bytes(b"\xff\xfe" + text.encode("utf-16-le"))
    ansi = tmp_path / "a.log"
    ansi.write_bytes(text.encode("cp1252"))
    assert smoke.read_log(utf16) == text
    assert smoke.read_log(ansi) == text


def test_the_prepared_ini_is_the_file_of_this_computer_with_the_key_and_no_name(tmp_path):
    folder = tmp_path / "ini"
    smoke.write_prepared_ini(folder, "SHOP-PC", "http://127.0.0.1:1", "K3Y", "file:///a%%20b", "H")
    text = (folder / "SHOP-PC.ini").read_text(encoding="utf-8")
    assert "api_key = K3Y" in text and "server_url = http://127.0.0.1:1" in text
    assert "printers = archive = file:///a%%20b" in text and "X-Smoke = H" in text
    assert "name" not in text.replace("server_url", "")  # the install fills the name in


def test_the_smoke_locks_the_folder_with_exactly_the_icacls_line_of_the_docs():
    docs = (Path(__file__).resolve().parents[2] / "docs" / "agent.md").read_text(encoding="utf-8")
    folder = r"%SystemRoot%\printpapi-deploy"
    documented = [line.split() for line in docs.splitlines() if line.startswith("icacls " + folder)]
    # one line, and none that walks into the folder: /T and /reset follow a junction that a user put
    # in a folder they made, and /setowner leaves the access list as it is
    assert len(documented) == 1, documented
    assert [w for w in documented[0] if w.lower() in ("/t", "/reset", "/setowner")] == []
    assert documented[0][2] == "/inheritance:r"
    ours = smoke.lock_down_commands(Path("X:/f"))
    assert [[str(Path("X:/f")) if w == folder else w for w in line] for line in documented] == ours


def test_the_docs_do_not_promise_to_repair_a_folder_that_somebody_else_made():
    docs = (Path(__file__).resolve().parents[2] / "docs" / "agent.md").read_text(encoding="utf-8")
    assert "safe for a folder that exists already" not in docs
    assert "run all three lines" not in docs
    # a place that standard users cannot make folders in, not C:\ProgramData
    assert r"AGENT_INI=C:\ProgramData" not in docs
    assert r"md %SystemRoot%\printpapi-deploy" in docs


def test_a_log_tells_a_custom_action_that_ran_from_one_that_was_skipped():
    ran = "MSI (s) (A4:B0) [10:00:00:000]: Doing action: ManagedUninstall\nAction start 10:00:00: ManagedUninstall."
    skipped = "MSI (s) (A4:B0) [10:00:00:000]: Skipping action: ManagedUninstall (condition is false)"
    assert smoke.ran_action(ran, "ManagedUninstall")
    assert not smoke.ran_action(skipped, "ManagedUninstall")
    assert not smoke.ran_action("Doing action: ManagedUninstallX", "ManagedUninstall")


def test_the_reason_of_a_failure_is_found_above_the_properties():
    log = "\n".join([
        "MSI (s) (E0:F4) [17:29:38:001]: Doing action: ManagedInstall",
        "CustomAction ManagedInstall returned actual error code 1 (note this may not be 100% accurate)",
        "Action ended 17:29:40: InstallFinalize. Return value 3.",
        "Property(S): LogonUser = runneradmin",
    ])
    assert smoke.telling_lines(log) == log.splitlines()[:3]


def test_msiexec_is_started_with_a_proxy_that_must_not_reach_the_agent(monkeypatch):
    monkeypatch.setenv("PRINTPAPI_KEEP", "1")
    env = smoke.starter_env()
    assert env["http_proxy"] == env["HTTP_PROXY"] == "http://127.0.0.1:9"
    assert env["PRINTPAPI_KEEP"] == "1"


def test_an_access_list_that_names_ordinary_users_is_found():
    private = "C:\\x NT AUTHORITY\\SYSTEM:(I)(OI)(CI)(F)\n     BUILTIN\\Administrators:(I)(OI)(CI)(F)\n"
    assert smoke.open_to_users(private) == []
    assert smoke.open_to_users(private + "     BUILTIN\\Users:(I)(RX)\n") == ["BUILTIN\\Users"]
    assert smoke.open_to_users(private + "     NT AUTHORITY\\Authenticated Users:(M)\n") == [
        "Authenticated Users"]


def test_the_working_folder_is_seen_where_it_appears(tmp_path):
    (tmp_path / "theirs").mkdir()
    with smoke.StagingWatch({"system": tmp_path / "not there", "starter": tmp_path / "theirs"}) as watch:
        (tmp_path / "theirs" / "printpapi-setup-abc").mkdir()
        (tmp_path / "theirs" / "other").mkdir()
        deadline = time.monotonic() + 10
        while not watch.seen["starter"] and time.monotonic() < deadline:
            time.sleep(0.02)
    assert watch.seen == {"system": set(), "starter": {"printpapi-setup-abc"}}


def test_msi_and_msi_upgrade_come_together(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["setup_smoke.py", "--server", "s", "--msi", "a.msi"])
    with pytest.raises(SystemExit) as e:
        smoke.main()
    assert e.value.code == 2
    assert "come together" in capsys.readouterr().err


def test_the_msi_mode_runs_on_windows_only(monkeypatch, capsys):
    monkeypatch.setattr(smoke, "WIN", False)
    monkeypatch.setattr(smoke, "check_msi", lambda args: pytest.fail("must not run"))
    monkeypatch.setattr(sys, "argv", ["setup_smoke.py", "--server", "s", "--msi", "a", "--msi-upgrade", "b"])
    with pytest.raises(SystemExit) as e:
        smoke.main()
    assert e.value.code == 2
    assert "Windows only" in capsys.readouterr().err


def test_the_msi_mode_is_handed_both_files(monkeypatch):
    seen = []
    monkeypatch.setattr(smoke, "WIN", True)
    monkeypatch.setattr(smoke, "check_msi", lambda args: seen.append((args.msi, args.msi_upgrade)) or 0)
    monkeypatch.setattr(sys, "argv", ["setup_smoke.py", "--server", "s", "--msi", "a", "--msi-upgrade", "b"])
    assert smoke.main() == 0
    assert seen == [("a", "b")]


def test_the_other_modes_still_need_an_agent(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["setup_smoke.py", "--server", "s"])
    with pytest.raises(SystemExit) as e:
        smoke.main()
    assert e.value.code == 2
    assert "--agent is required" in capsys.readouterr().err
