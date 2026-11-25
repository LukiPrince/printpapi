# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""crates/setup/wix/managed.wxs and crates/setup/src/managed.rs must agree on the flag file.

The two custom actions that remove and refresh the agent run only when AppSearch found the flag of
managed.rs, so a PC that has the plain .msi never starts the unsigned setup exe on a removal (WDAC
or an antivirus that blocks it would otherwise fail msiexec /x with 1603). The .msi itself cannot
be run here; the real behaviour is the removal steps of tools/smoke/setup_smoke.py --msi.
"""
import re
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WXS = ROOT / "crates" / "setup" / "wix" / "managed.wxs"
MANAGED_RS = ROOT / "crates" / "setup" / "src" / "managed.rs"
NS = {"w": "http://schemas.microsoft.com/wix/2006/wi"}
PROPERTY = "PRINTPAPI_MANAGED_FLAG"


def root() -> ET.Element:
    return ET.parse(WXS).getroot()


def conditions() -> dict[str, str]:
    """The text of each `<Custom Action=...>condition</Custom>` of the execute sequence."""
    seq = root().find(".//w:InstallExecuteSequence", NS)
    return {c.get("Action"): (c.text or "").strip() for c in seq.findall("w:Custom", NS)}


def test_the_flag_file_is_searched_where_the_exe_is_installed_under_the_name_managed_rs_writes():
    prop = next(p for p in root().iter(f"{{{NS['w']}}}Property") if p.get("Id") == PROPERTY)
    folder = prop.find("w:DirectorySearch", NS)
    assert folder is not None and folder.get("Path") == "[ProgramFiles64Folder]printpapi Agent Setup"
    # the exe's own folder is the one that PinInstallDir pins
    pin = next(c for c in root().iter(f"{{{NS['w']}}}CustomAction") if c.get("Id") == "PinInstallDir")
    assert pin.get("Value") == folder.get("Path")
    assert folder.get("Depth") == "0"
    name = folder.find("w:FileSearch", NS).get("Name")
    flag = re.search(r'pub const FLAG: &str = "([^"]+)";', MANAGED_RS.read_text(encoding="utf-8"))
    assert flag and name == flag.group(1)


def test_the_flag_property_is_secure_so_a_double_clicked_run_hands_it_to_the_elevated_server():
    """AppSearch runs once: in the UI sequence of a double-clicked .msi, where it is NOT elevated.
    The execute sequence then skips it and sees the value only when the property is secure, like
    Tauri's own DESKTOPSHORTCUTEXISTS search (main.wxs of tauri-bundler); without it a double-clicked
    Remove or upgrade skips ManagedUninstall and ManagedRefresh. AppSearch:
    https://learn.microsoft.com/en-us/windows/win32/msi/appsearch-action"""
    prop = next(p for p in root().iter(f"{{{NS['w']}}}Property") if p.get("Id") == PROPERTY)
    assert prop.get("Secure") == "yes"


def test_removal_and_refresh_need_the_flag_and_the_install_does_not():
    c = conditions()
    assert PROPERTY in c["ManagedUninstall"] and 'REMOVE="ALL"' in c["ManagedUninstall"]
    assert "NOT UPGRADINGPRODUCTCODE" in c["ManagedUninstall"]
    assert PROPERTY in c["ManagedRefresh"] and "WIX_UPGRADE_DETECTED" in c["ManagedRefresh"]
    # the first install has no flag yet: it is the one that writes it
    assert PROPERTY not in c["ManagedInstall"] and "AGENT_INI" in c["ManagedInstall"]
