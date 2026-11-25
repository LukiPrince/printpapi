# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""publish-client.yml: the job that holds the npm id-token runs in a protected environment.

npm trusted publishing matches the repository, the workflow file name and, only if the maintainer set
it, the environment name. Without an `environment:` on the job, the guards inside the file are the only
gate, and everybody who can push a branch can start a copy of the workflow with the guards removed
(SECURITY.md names every writer of the repository as the threat for the update key, and puts an
environment in front of it; the npm path gets the same).
"""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "publish-client.yml"


def jobs() -> dict[str, str]:
    text = WORKFLOW.read_text(encoding="utf-8").split("\njobs:\n", 1)[1]
    parts = re.split(r"(?m)^  (?=[a-z][a-z0-9-]*:\s*$)", text)
    return {p.split(":", 1)[0]: p for p in parts if p.strip()}


def test_the_job_with_the_id_token_runs_in_the_npm_publish_environment():
    holders = {n: j for n, j in jobs().items() if re.search(r"(?m)^\s*id-token:\s*write", j)}
    assert list(holders) == ["publish"], "exactly one job holds the id-token"
    assert re.search(r"(?m)^    environment:\s*npm-publish\s*$", holders["publish"]), (
        "the publish job needs `environment: npm-publish` (required reviewer, tags v* only)"
    )


def test_the_build_job_has_no_id_token_and_no_environment():
    build = jobs()["build"]
    assert "id-token" not in build
    assert "environment:" not in build
