# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Tests of tools/docs/push_hook.py: command classification, decide(), and the hook as a process."""
import json
import subprocess
import sys
from pathlib import Path

import pytest

from push_hook import CHAINED, NONE, PUSH, classify, decide, is_push

HOOK = Path(__file__).with_name("push_hook.py")
SHA = "a" * 40


@pytest.mark.parametrize("command", [
    "git push",
    "git push -u origin x",
    "cd a && git push",
    "git -C /p push",
    r'git -C "C:\Program Files\repo" push',
    "git -c 'a=b c' push",
    "git -c core.x=1 push origin main",
    "git -p push",
    "git --no-pager push",
    "git --git-dir=/x/.git --work-tree /x push",
    "git --namespace n --bare push",
    "git.exe push",
    "GIT.EXE push",
    r'"C:\Program Files\Git\cmd\git.exe" push',
    "/usr/bin/git push",
    "FOO=1 git push",
    "FOO=1 BAR=2 git push",
    "git push origin HEAD",
    "echo hi; git   push --force-with-lease",
    "true || git push",
    "(git push)",
    "ls | git push",
    "sleep 1 &\ngit push",
    "echo hi\ngit push",
    "git push 2>&1",
    "git \\\n push",
    "echo $(git push)",
    "cat <<EOF\ngit push\nEOF",  # ponytail: a heredoc body line is read as a command
])
def test_is_push_true(command):
    assert is_push(command)
    assert classify(command) in (PUSH, CHAINED)


@pytest.mark.parametrize("command", [
    "git status",
    "echo pushed",
    "git log --grep=push",
    'grep -rn "git push" AGENTS.md',
    'git commit -m "explain git push hook"',
    "echo 'git push'",
    "man git push",
    """curl -X POST -d '{"cmd": "git push"}' https://x.example/hook""",
    "git stash push",
    "gitpush",
    "git pushd",
    "git push --dry-run",
    "git push --dry-run origin main",
    "git push -n",
    "npm run push",
    "echo hi # git push",
    "",
])
def test_is_push_false(command):
    assert not is_push(command)
    assert classify(command) == NONE


def test_unbalanced_quotes_decide_conservatively():
    assert is_push("git push 'origin")
    assert is_push('echo "x && git push')  # cannot tell what is quoted: hold it
    assert not is_push('git commit -m "explain git push hook')
    assert not is_push("echo 'oops")


@pytest.mark.parametrize("command", [
    "git add -A && git commit -m x && git push",
    "git push && git status",
    "git fetch; git push",
    "git -C /p commit -m x\ngit push",
    "git commit -m x || git push",
])
def test_push_chained_with_another_git_command(command):
    assert classify(command) == CHAINED


@pytest.mark.parametrize("command", [
    "git push && git push --tags",
    "cd a && git push",
    "git --version && git push",  # no subcommand is not "another git command"
    "git push --dry-run && git push",
    "echo a && git push && echo b",
])
def test_push_not_chained(command):
    assert classify(command) == PUSH


def test_decide_allows_non_push_without_looking_at_state():
    assert decide("git status", None, None, ["x"]) is None


def test_decide_blocks_on_findings_and_lists_them():
    reason = decide("git push", SHA, SHA, ["README.md:3: broken link", "docs/a.md:1: bad"])
    assert reason and "README.md:3: broken link" in reason and "docs/a.md:1: bad" in reason


def test_decide_blocks_without_marker_and_tells_what_to_do():
    reason = decide("git push", SHA, None, [])
    assert reason
    for needle in ("README.md", "docs/", "git log origin/main..HEAD", "git diff origin/main...HEAD",
                   "python tools/docs/push_hook.py --mark", "command of its own", "--mark && git push"):
        assert needle in reason


def test_decide_blocks_on_stale_marker():
    assert decide("git push", SHA, "b" * 40, []) is not None


def test_decide_allows_matching_marker_ignoring_whitespace():
    assert decide("git push", SHA, SHA + "\n", []) is None


def test_decide_blocks_a_chained_push_even_with_a_matching_marker():
    reason = decide("git add -A && git commit -m x && git push", SHA, SHA, [])
    assert reason and "run git push as a command of its own" in reason
    assert "HEAD before the command runs" in reason


# --- the hook as a process -----------------------------------------------------------------

def git(cwd, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", *args],
        cwd=cwd, check=True, capture_output=True, text=True,
    ).stdout.strip()


@pytest.fixture
def work(tmp_path):
    """A repo that has the hook (the hook only acts in a repo that does)."""
    git(tmp_path, "init", "-q")
    (tmp_path / "tools" / "docs").mkdir(parents=True)
    (tmp_path / "tools" / "docs" / "push_hook.py").write_text("# stand-in\n")
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


def test_hook_mark_and_push_in_one_command_can_never_pass(work):
    done = run_hook(work, "python tools/docs/push_hook.py --mark && git push")
    assert done.returncode == 2 and "command of its own" in done.stderr


def test_hook_chained_push_is_blocked_with_its_reason(work):
    run_hook(work, None, "--mark")
    done = run_hook(work, "git add -A && git commit -m x && git push")
    assert done.returncode == 2 and "own" in done.stderr


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


def test_hook_checks_head_not_the_working_tree(work):
    run_hook(work, None, "--mark")
    (work / "README.md").write_text("# T\n\n[x](nope.md)\n", encoding="utf-8")  # not committed
    assert run_hook(work, "git push").returncode == 0
    (work / "README.md").write_text("# T\n", encoding="utf-8")
    (work / "local.md").write_text("# L\n")  # untracked: not pushed, not checked
    (work / "README.md").write_text("# T\n\n[x](local.md)\n", encoding="utf-8")
    git(work, "commit", "-q", "-am", "link to a file git does not have")
    run_hook(work, None, "--mark")
    done = run_hook(work, "git push")
    assert done.returncode == 2 and "not in git at HEAD" in done.stderr


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


def test_hook_only_acts_in_a_repo_that_has_it(work):
    (work / "tools" / "docs" / "push_hook.py").unlink()
    done = run_hook(work, "git push")
    assert done.returncode == 0 and done.stderr == ""
