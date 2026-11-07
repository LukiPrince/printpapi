# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Tests of tools/docs/push_hook.py: the push regex, decide(), and the hook as a subprocess."""
import json
import subprocess
import sys
from pathlib import Path

import pytest

from push_hook import decide, is_push

HOOK = Path(__file__).with_name("push_hook.py")
SHA = "a" * 40


@pytest.mark.parametrize("command", [
    "git push",
    "git push -u origin x",
    "cd a && git push",
    "git -C /some/path push",
    "git -c core.x=1 push origin main",
    "git --no-pager push",
    "echo hi; git   push --force-with-lease",
])
def test_is_push_true(command):
    assert is_push(command)


@pytest.mark.parametrize("command", [
    "git status",
    "echo pushed",
    "git log --grep=push",
    "git pushd",
    "npm run push",
    "",
])
def test_is_push_false(command):
    assert not is_push(command)


def test_decide_allows_non_push_without_looking_at_state():
    assert decide("git status", None, None, ["x"]) is None


def test_decide_blocks_on_findings_and_lists_them():
    reason = decide("git push", SHA, SHA, ["README.md:3: broken link", "docs/a.md:1: bad"])
    assert reason and "README.md:3: broken link" in reason and "docs/a.md:1: bad" in reason


def test_decide_blocks_without_marker_and_tells_what_to_do():
    reason = decide("git push", SHA, None, [])
    assert reason
    for needle in ("README.md", "docs/", "git log origin/main..HEAD", "git diff origin/main...HEAD",
                   "python tools/docs/push_hook.py --mark"):
        assert needle in reason


def test_decide_blocks_on_stale_marker():
    assert decide("git push", SHA, "b" * 40, []) is not None


def test_decide_allows_matching_marker_ignoring_whitespace():
    assert decide("git push", SHA, SHA + "\n", []) is None


# --- the hook as a process -----------------------------------------------------------------

def git(cwd, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", *args],
        cwd=cwd, check=True, capture_output=True, text=True,
    ).stdout.strip()


@pytest.fixture
def work(tmp_path):
    git(tmp_path, "init", "-q")
    (tmp_path / "README.md").write_text("# T\n", encoding="utf-8")
    git(tmp_path, "add", "README.md")
    git(tmp_path, "commit", "-q", "-m", "init")
    return tmp_path


def run_hook(cwd, command=None, *argv):
    payload = {"hook_event_name": "PreToolUse", "tool_name": "Bash",
               "tool_input": {"command": command}, "cwd": str(cwd)}
    return subprocess.run([sys.executable, str(HOOK), *argv], input=json.dumps(payload), cwd=cwd,
                          capture_output=True, text=True)


def test_hook_non_push_exits_0_even_outside_a_repo(tmp_path):
    done = run_hook(tmp_path, "git status")
    assert done.returncode == 0 and done.stderr == ""


def test_hook_push_without_marker_blocks_then_mark_allows(work):
    done = run_hook(work, "git push")
    assert done.returncode == 2 and "--mark" in done.stderr
    marked = run_hook(work, None, "--mark")
    assert marked.returncode == 0
    marker = Path(git(work, "rev-parse", "--git-path", "docs-checked"))
    marker = marker if marker.is_absolute() else work / marker
    assert marker.read_text().strip() == git(work, "rev-parse", "HEAD")
    assert run_hook(work, "git push").returncode == 0


def test_hook_new_commit_invalidates_marker(work):
    run_hook(work, None, "--mark")
    git(work, "commit", "-q", "--allow-empty", "-m", "more")
    assert run_hook(work, "git push").returncode == 2


def test_hook_blocks_on_broken_link_even_with_marker(work):
    (work / "README.md").write_text("# T\n\n[x](nope.md)\n", encoding="utf-8")
    git(work, "commit", "-q", "-am", "bad")
    run_hook(work, None, "--mark")
    done = run_hook(work, "git push")
    assert done.returncode == 2 and "README.md:3:" in done.stderr


def test_hook_unusable_cwd_falls_back_to_the_process_cwd(work):
    payload = {"tool_name": "Bash", "tool_input": {"command": "git push"}, "cwd": "/no/such/dir"}
    done = subprocess.run([sys.executable, str(HOOK)], input=json.dumps(payload), cwd=work,
                          capture_output=True, text=True)
    assert done.returncode == 2 and "--mark" in done.stderr


def test_hook_fails_open_on_garbage_input(tmp_path):
    done = subprocess.run([sys.executable, str(HOOK)], input="not json", cwd=tmp_path,
                          capture_output=True, text=True)
    assert done.returncode == 1 and done.stderr


def test_hook_push_outside_a_repo_is_allowed(tmp_path):
    assert run_hook(tmp_path, "git push").returncode == 0
