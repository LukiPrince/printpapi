# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""The setup downloads listed in web/lib/setup-downloads.json and the files release.yml builds.

A file in the JSON that no step builds is a 404 baked into the server binary (the downloads page links
to releases/latest/download/<asset>), so the JSON must be a SUBSET of what the workflow can build.
The other direction is allowed only for the files named in OPTIONAL_UNLISTED, which are the ones that
may fail to build (steps with continue-on-error, the aarch64 AppImage leg, the Windows ARM64 leg
until it has passed once). When one of them cannot
be built, RELEASE_CHECKLIST step 7 deletes its JSON entry, and this check stays green. The job `docs`
then also wants the docs to stop naming the file (tools/docs/check_docs.py: README.md and docs/ name
only files of the JSON), so step 7 is: the JSON entry, the lines of README.md and docs/agent.md that
name the file, then `cd web && npm run build:app`.

What the workflow builds is derived from its text without a YAML parser (the docs job installs
nothing but pytest): the names of the setup job's matrix rows, and, per system, the file extensions
that a step names as `printpapi-setup-${{ matrix.name }}.<ext>`. The update files
(printpapi-update-*, printpapi-update.txt) are not setup downloads and are not looked at.
"""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"
DOWNLOADS = ROOT / "web" / "lib" / "setup-downloads.json"

# What each system's setup files can be called; a workflow that never names the extension for the
# matrix name does not build that file.
EXTENSIONS = {
    "windows": ("exe", "msi"),
    "macos": ("dmg",),
    "linux": ("deb", "rpm", "AppImage"),
}

# Setup files that release.yml builds but web/lib/setup-downloads.json may not list: the ones whose
# build is allowed to fail. A built file nobody can find on the page is a mistake, unless it is named
# here (and a file that is not built at all must not stay in the JSON: that is the other test).
OPTIONAL_UNLISTED: set[str] = {
    # the windows-11-arm leg is new and has not run in a release yet: it can fail as a whole (the
    # matrix has fail-fast: false and publish goes on), so its .exe may be missing as well. Take this
    # line out when that leg has passed once.
    "printpapi-setup-windows-aarch64.exe",
    "printpapi-setup-windows-x86_64.msi",
    "printpapi-setup-windows-aarch64.msi",
    "printpapi-setup-linux-x86_64.rpm",
    "printpapi-setup-linux-aarch64.rpm",
    "printpapi-setup-linux-aarch64.AppImage",
}


def setup_job(text: str) -> str:
    """The text of the `setup:` job (two-space indent) up to the next job."""
    m = re.search(r"^  setup:\n(.*?)(?=^  [a-z][\w-]*:\n|\Z)", text, flags=re.S | re.M)
    if not m:
        raise AssertionError("release.yml has no setup job")
    return m.group(1)


def matrix_names(job: str) -> list[str]:
    return re.findall(r"^\s*- \{[^}\n]*\bname: ([\w-]+)[^}\n]*\}", job, flags=re.M)


def buildable_assets(text: str) -> set[str]:
    """Every `printpapi-setup-<name>.<ext>` that a matrix row and a step of the setup job give."""
    job = setup_job(text)
    assets = set()
    for name in matrix_names(job):
        system = name.split("-")[0]
        for ext in EXTENSIONS.get(system, ()):
            named = rf"printpapi-setup-(\$\{{\{{ matrix\.name \}}\}}|{re.escape(name)})\.{ext}\b"
            if re.search(named, job):
                assets.add(f"printpapi-setup-{name}.{ext}")
    return assets


def problems(listed: set[str], text: str, optional: set[str]) -> list[str]:
    built = buildable_assets(text)
    out = [f"{a} is listed in setup-downloads.json but release.yml builds no such file"
           for a in sorted(listed - built)]
    out += [f"{a} is built by release.yml but not listed in setup-downloads.json (and not in OPTIONAL_UNLISTED)"
            for a in sorted(built - listed - optional)]
    return out


def listed_assets() -> set[str]:
    return {entry["asset"] for entry in json.loads(DOWNLOADS.read_text(encoding="utf-8"))}


def test_the_downloads_are_a_subset_of_what_the_workflow_builds():
    built = buildable_assets(WORKFLOW.read_text(encoding="utf-8"))
    missing = sorted(listed_assets() - built)
    assert not missing, f"listed but not built: {missing}"


def test_what_is_built_is_listed_unless_it_is_named_optional():
    text = WORKFLOW.read_text(encoding="utf-8")
    extra = sorted(buildable_assets(text) - listed_assets() - OPTIONAL_UNLISTED)
    assert not extra, (
        f"built but not listed: {extra}. List them in web/lib/setup-downloads.json, or, for a file that "
        "may fail to build, name them in OPTIONAL_UNLISTED in tools/docs/test_release_assets.py"
    )


def test_the_workflow_builds_the_new_files():
    built = buildable_assets(WORKFLOW.read_text(encoding="utf-8"))
    for asset in (
        "printpapi-setup-windows-aarch64.exe",
        "printpapi-setup-windows-x86_64.msi",
        "printpapi-setup-windows-aarch64.msi",
        "printpapi-setup-linux-x86_64.rpm",
        "printpapi-setup-linux-aarch64.rpm",
    ):
        assert asset in built, asset


def test_the_owner_can_drop_the_entry_of_any_optional_file_and_the_check_stays_green():
    """RELEASE_CHECKLIST step 7: a file that cannot be built loses its JSON entry, and the job `docs`
    must not turn red because of that alone."""
    text = WORKFLOW.read_text(encoding="utf-8")
    for asset in sorted(OPTIONAL_UNLISTED):
        assert problems(listed_assets() - {asset}, text, OPTIONAL_UNLISTED) == [], asset
    assert OPTIONAL_UNLISTED <= buildable_assets(text), "OPTIONAL_UNLISTED names a file no step builds"


def test_every_file_that_the_workflow_may_fail_to_build_is_optional():
    """The files of steps with continue-on-error (the .msi, the .rpm) and of the matrix leg that is
    allowed to fail (linux-aarch64 AppImage) are named in OPTIONAL_UNLISTED."""
    assert {
        "printpapi-setup-windows-x86_64.msi",
        "printpapi-setup-windows-aarch64.msi",
        "printpapi-setup-linux-x86_64.rpm",
        "printpapi-setup-linux-aarch64.rpm",
        "printpapi-setup-linux-aarch64.AppImage",
        # the windows-11-arm leg has not run yet: it can fail as a whole, and publish goes on
        "printpapi-setup-windows-aarch64.exe",
    } <= OPTIONAL_UNLISTED


def test_the_update_files_are_not_setup_files():
    built = buildable_assets(WORKFLOW.read_text(encoding="utf-8"))
    assert built and all(a.startswith("printpapi-setup-") for a in built)


WORKFLOW_SAMPLE = """
jobs:
  build:
    strategy:
      matrix:
        include:
          - { os: x, target: t, name: windows-x86_64, exe: .exe }
  setup:
    strategy:
      matrix:
        include:
          - { os: windows-latest, target: t, name: windows-x86_64, exe: .exe }
          - { os: ubuntu-22.04, target: u, name: linux-x86_64, exe: "" }
    steps:
      - run: |
          cp a dist/printpapi-setup-${{ matrix.name }}.exe
          cp b dist/printpapi-setup-${{ matrix.name }}.deb
          cp c dist/printpapi-update-tray-${{ matrix.name }}.exe
          cp d dist-extra/printpapi-setup-linux-x86_64.rpm
  publish:
    steps:
      - run: echo printpapi-setup-windows-x86_64.msi
"""


def test_the_parser_reads_the_matrix_and_the_steps_of_the_setup_job_only():
    assert matrix_names(setup_job(WORKFLOW_SAMPLE)) == ["windows-x86_64", "linux-x86_64"]
    assert buildable_assets(WORKFLOW_SAMPLE) == {
        "printpapi-setup-windows-x86_64.exe",
        "printpapi-setup-linux-x86_64.deb",
        "printpapi-setup-linux-x86_64.rpm",
    }


def test_a_listed_file_without_a_build_and_a_built_file_without_an_entry_are_problems():
    built = buildable_assets(WORKFLOW_SAMPLE)
    assert problems(built, WORKFLOW_SAMPLE, set()) == []
    ghost = built | {"printpapi-setup-windows-x86_64.msi"}
    assert problems(ghost, WORKFLOW_SAMPLE, set()) == [
        "printpapi-setup-windows-x86_64.msi is listed in setup-downloads.json but release.yml builds no such file"
    ]
    # deleting the entry of a file that cannot be built keeps the check green, as the checklist says
    assert problems(built - {"printpapi-setup-linux-x86_64.rpm"}, WORKFLOW_SAMPLE, {"printpapi-setup-linux-x86_64.rpm"}) == []
    assert len(problems(built - {"printpapi-setup-linux-x86_64.rpm"}, WORKFLOW_SAMPLE, set())) == 1
