# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Command lines that the packaging files and the docs give must parse on the real tool.

zypper takes --allow-unsigned-rpm as an option of the install command, not as a global one: put in
front of the command it answers "The flag --allow-unsigned-rpm is not known." and exits 2
(zypper 1.14.102 in opensuse/tumbleweed; zypper.8, https://manpages.opensuse.org/Tumbleweed/zypper/zypper.8.en.html,
lists it under "install"). The spec's own text has it in front, so the files are checked here.
"""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FILES = [
    ROOT / "tools" / "packaging" / "rpm_install_check.sh",
    ROOT / "docs" / "agent.md",
    ROOT / "README.md",
]
# zypper ... --allow-unsigned-rpm ... install: the option stands before the command
BEFORE = re.compile(r"zypper\b[^\n`]*--allow-unsigned-rpm[^\n`]*\binstall\b")
AFTER = re.compile(r"zypper\b[^\n`]*\binstall\b[^\n`]*--allow-unsigned-rpm")


def test_the_zypper_option_stands_behind_the_install_command_everywhere():
    for path in FILES:
        text = path.read_text(encoding="utf-8")
        assert not BEFORE.search(text), f"{path.name}: --allow-unsigned-rpm stands before install"


def test_the_script_and_the_docs_do_use_the_option():
    for path in FILES[:2]:
        assert AFTER.search(path.read_text(encoding="utf-8")), path.name


def test_the_checker_catches_the_wrong_order():
    assert BEFORE.search("sudo zypper --allow-unsigned-rpm install ./x.rpm")
    assert not BEFORE.search("sudo zypper install --allow-unsigned-rpm ./x.rpm")
