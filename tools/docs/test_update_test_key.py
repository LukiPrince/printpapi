# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""The agent's test key for updates must never reach a release.

The agent has a Cargo feature `update-test-key` that adds the public half of a seed which is in the
repository (crates/agent/examples/sign_update.rs) to the keys an update is accepted under. Anybody who
reads the repository can sign an update with it, so only the CI smoke build may enable it. The Rust
test `a_default_build_accepts_no_key_whose_seed_is_in_the_repository` (crates/agent/src/update.rs)
checks the other half: the keys of a build without the feature.

The text of the workflows, the Dockerfile and the Cargo files is read without a YAML or TOML parser
(the docs job installs nothing but pytest).
"""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FEATURE = "update-test-key"
WORKFLOWS = ROOT / ".github" / "workflows"


def workflow_files(folder: Path = WORKFLOWS) -> list[Path]:
    """Every workflow file GitHub runs: it reads both endings."""
    return sorted(p for p in folder.iterdir() if p.suffix in (".yml", ".yaml"))


def code_lines(path: Path) -> list[str]:
    """The lines of a file that are not comments (YAML, Dockerfile and TOML all use #)."""
    return [l for l in path.read_text(encoding="utf-8").splitlines() if not l.lstrip().startswith("#")]


def test_the_feature_is_named_by_nothing_that_builds_a_release():
    for name in ("release.yml", "docker.yml", "demo.yml", "publish-client.yml"):
        path = WORKFLOWS / name
        assert path.is_file(), name
        text = path.read_text(encoding="utf-8")
        assert FEATURE not in text, f"{name} must never enable {FEATURE}"
        # tools/smoke/build_update_fixture.py builds with the feature itself
        assert "build_update_fixture" not in text, f"{name} must not run the fixture build"
    for path in (ROOT / "Dockerfile", ROOT / "docker-compose.yml"):
        assert FEATURE not in path.read_text(encoding="utf-8"), f"{path.name} must never enable {FEATURE}"


def test_a_yaml_workflow_is_looked_at_too(tmp_path):
    (tmp_path / "a.yml").write_text("on: push\n")
    (tmp_path / "nightly.yaml").write_text("on: push\n")
    (tmp_path / "notes.txt").write_text("x\n")
    assert [p.name for p in workflow_files(tmp_path)] == ["a.yml", "nightly.yaml"]


def test_no_workflow_turns_every_feature_on():
    # --all-features would switch the test key on in whatever that step builds
    for path in workflow_files():
        for line in code_lines(path):
            assert "--all-features" not in line, f"{path.name}: {line.strip()}"


def test_only_the_ci_smoke_build_enables_it_and_publishes_nothing():
    # every workflow file is either looked at above or named here, so a new one cannot slip past
    known = {"ci.yml", "release.yml", "docker.yml", "demo.yml", "publish-client.yml"}
    assert {p.name for p in workflow_files()} <= known, "a new workflow: decide if it may name the feature"
    ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    jobs = re.split(r"(?m)^  (?=[a-z][a-z0-9-]*:\s*$)", ci.split("\njobs:\n", 1)[1])
    enabling = [j for j in jobs if any(FEATURE in l for l in code_lines_of(j))]
    assert len(enabling) == 1, "exactly one CI job (the setup smoke test) builds with the test key"
    job = enabling[0]
    assert job.startswith("setup-smoke:"), job.split("\n", 1)[0]
    for forbidden in ("upload-artifact", "gh release", "docker push", "docker/build-push-action", "npm publish"):
        assert forbidden not in job, f"the job with the test key must not use {forbidden}"


def code_lines_of(text: str) -> list[str]:
    return [l for l in text.splitlines() if not l.lstrip().startswith("#")]


def test_cargo_never_turns_it_on_by_default_or_through_a_dependency():
    agent = code_lines(ROOT / "crates" / "agent" / "Cargo.toml")
    assert any(l.strip() == f"{FEATURE} = []" for l in agent), "the feature is declared, and empty"
    assert not any(re.match(r"\s*default\s*=", l) for l in agent), "the agent has no default features"
    others = [
        ROOT / "Cargo.toml",
        ROOT / "crates" / "server" / "Cargo.toml",
        ROOT / "crates" / "setup" / "Cargo.toml",
    ]
    for path in others:
        for line in code_lines(path):
            assert FEATURE not in line, f"{path.relative_to(ROOT)}: {line.strip()}"
    # a Cargo config or a tauri config could turn it on too
    for path in list((ROOT / ".cargo").glob("*")) + [ROOT / "crates" / "setup" / "tauri.conf.json",
                                                      ROOT / "crates" / "setup" / "tauri.msi.conf.json"]:
        if path.is_file():
            assert FEATURE not in path.read_text(encoding="utf-8"), path.name
