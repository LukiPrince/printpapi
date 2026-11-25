# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_update_fixture as fixture  # noqa: E402

CARGO = '[workspace]\nmembers = ["a"]\n\n[workspace.package]\nversion = "3.6.0"\nedition = "2024"\n'


def test_only_the_workspace_version_changes():
    new = fixture.set_version(CARGO, "99.0.0")
    assert new == CARGO.replace('"3.6.0"', '"99.0.0"')


def test_a_file_without_a_version_line_is_refused():
    try:
        fixture.set_version("[workspace]\n", "99.0.0")
    except SystemExit:
        return
    raise AssertionError("must refuse")


def test_the_real_cargo_toml_has_exactly_one_line_to_change():
    text = (fixture.ROOT / "Cargo.toml").read_text(encoding="utf-8")
    new = fixture.set_version(text, fixture.VERSION)
    changed = [a for a, b in zip(text.splitlines(), new.splitlines()) if a != b]
    assert len(changed) == 1 and changed[0].startswith("version")
