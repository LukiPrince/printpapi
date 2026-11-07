# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Tests of tools/docs/push_hook.py and tools/githooks/pre-push: decide() on its own, then the
real thing, a git push into a bare repository through the hook."""
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from push_hook import decide, parse_updates

REPO = Path(__file__).resolve().parents[2]
A, B, C = "a" * 40, "b" * 40, "c" * 40
ZERO = "0" * 40


# --- parse_updates and decide ----------------------------------------------------------------

def test_parse_updates_skips_deletions_and_noise():
    text = (f"refs/heads/main {A} refs/heads/main {B}\n"
            f"(delete) {ZERO} refs/heads/old {C}\n"
            f"refs/tags/v1 {C} refs/tags/v1 {ZERO}\n\nnot a line\n")
    assert parse_updates(text) == [("refs/heads/main", A, B), ("refs/tags/v1", C, ZERO)]


def test_parse_updates_takes_a_local_ref_with_spaces():
    text = f"HEAD^{{/broken docs}} {A} refs/heads/main {B}\n"
    assert parse_updates(text) == [("refs/heads/main", A, B)]


MAIN, TAG = "refs/heads/main", "refs/tags/v1"


def test_decide_allows_clean_docs_outside_claude():
    assert decide([(MAIN, A, B)], {A: []}, set(), in_claude=False) is None


def test_decide_allows_nothing_to_push():
    assert decide([], {}, set(), in_claude=True) is None


def test_decide_findings_abort_with_and_without_claude():
    for in_claude in (False, True):
        reason = decide([(MAIN, A, B)], {A: ["README.md:3: broken link x"]}, {A}, in_claude)
        assert reason and "README.md:3: broken link x" in reason


def test_decide_names_each_pushed_ref_with_its_short_sha_before_its_findings():
    reason = decide([(MAIN, A, B), (TAG, A, ZERO), ("refs/heads/x", C, B)],
                    {A: ["a:1: bad"], C: ["c:2: bad"]}, set(), False)
    assert f"{MAIN} ({A[:10]})" in reason and f"{TAG} ({A[:10]})" in reason
    assert f"refs/heads/x ({C[:10]})" in reason
    assert reason.index(f"{MAIN} ({A[:10]})") < reason.index("a:1: bad") < reason.index(
        f"refs/heads/x ({C[:10]})") < reason.index("c:2: bad")


def test_decide_leaves_out_refs_without_findings():
    reason = decide([(MAIN, A, B), (TAG, C, ZERO)], {A: ["a:1: bad"], C: []}, set(), False)
    assert TAG not in reason


def test_decide_in_claude_needs_the_marker():
    reason = decide([(MAIN, A, B)], {A: []}, set(), in_claude=True)
    assert reason
    for needle in ("README.md", "docs/", f"git log {B}..{A}", f"git diff {B}..{A}",
                   "python tools/docs/push_hook.py --mark", "--mark <rev>", "push again"):
        assert needle in reason


def test_decide_new_remote_ref_compares_with_the_remote_main():
    reason = decide([(MAIN, A, ZERO)], {A: []}, set(), in_claude=True)
    assert f"git log origin/main..{A}" in reason
    reason = decide([(MAIN, A, ZERO)], {A: []}, set(), in_claude=True, remote="upstream")
    assert f"git log upstream/main..{A}" in reason and "origin" not in reason


def test_decide_new_remote_ref_without_a_remote_main_uses_what_the_remote_lacks():
    reason = decide([(MAIN, A, ZERO)], {A: []}, set(), in_claude=True, remote="upstream",
                    remote_main=False)
    assert f"git log {A} --not --remotes=upstream" in reason and "/main" not in reason


def test_decide_marked_commit_passes_in_claude():
    assert decide([(MAIN, A, B)], {A: []}, {A}, in_claude=True) is None


def test_decide_asks_only_for_the_unmarked_commits():
    reason = decide([(MAIN, A, B), (TAG, C, ZERO)], {A: [], C: []}, {A}, in_claude=True)
    assert f"origin/main..{C}" in reason and f"{B}..{A}" not in reason


def test_decide_findings_come_before_the_marker():
    reason = decide([(MAIN, A, B)], {A: ["x:1: bad"]}, set(), in_claude=True)
    assert "x:1: bad" in reason and "--mark" not in reason


# --- end to end: real git, a bare remote, a clone with the hook on -------------------------------

GOOD_README = "# Title\n\nSee [the list](web/lib/setup-downloads.json).\n"
BAD_README = "# Title\n\nSee [nothing](missing.md).\n"
SETUP_JSON = '[{"asset": "printpapi-setup-windows-x86_64.exe"}]\n'


def make_env(claude: bool) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_") and k != "CLAUDECODE"}
    env.update(GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t",
               GIT_COMMITTER_EMAIL="t@t", GIT_TERMINAL_PROMPT="0")
    if claude:
        env["CLAUDECODE"] = "1"
    return env


class Clone:
    def __init__(self, tmp: Path):
        self.remote = tmp / "remote.git"
        self.dir = tmp / "work"
        self.dir.mkdir()
        subprocess.run(["git", "init", "--bare", "-b", "main", str(self.remote)], check=True,
                       capture_output=True, env=make_env(False))
        self.git("init", "-b", "main")
        self.git("remote", "add", "origin", str(self.remote))
        self.git("config", "core.hooksPath", "tools/githooks")  # this clone only
        (self.dir / "tools" / "docs").mkdir(parents=True)
        (self.dir / "tools" / "githooks").mkdir()
        (self.dir / "web" / "lib").mkdir(parents=True)
        for name in ("check_docs.py", "push_hook.py"):
            shutil.copy(REPO / "tools" / "docs" / name, self.dir / "tools" / "docs" / name)
        hook = self.dir / "tools" / "githooks" / "pre-push"
        shutil.copy(REPO / "tools" / "githooks" / "pre-push", hook)
        hook.chmod(0o755)
        (self.dir / "web" / "lib" / "setup-downloads.json").write_text(SETUP_JSON, newline="\n")

    def run(self, cmd: list[str], claude: bool = False, cwd: Path | None = None,
            env: dict[str, str] | None = None, **kw) -> subprocess.CompletedProcess:
        return subprocess.run(cmd, cwd=cwd or self.dir, capture_output=True, text=True,
                              encoding="utf-8", env=env or make_env(claude), **kw)

    def add_remote(self, name: str) -> Path:
        path = self.dir.parent / f"{name}.git"
        subprocess.run(["git", "init", "--bare", "-b", "main", str(path)], check=True,
                       capture_output=True, env=make_env(False))
        self.git("remote", "add", name, str(path))
        return path

    def git(self, *args: str, claude: bool = False) -> subprocess.CompletedProcess:
        out = self.run(["git", *args], claude)
        assert out.returncode == 0, out.stderr
        return out

    def commit(self, readme: str = GOOD_README, msg: str = "c") -> str:
        (self.dir / "README.md").write_text(readme, newline="\n")
        self.git("add", "-A")
        self.git("commit", "-m", msg)
        return self.git("rev-parse", "HEAD").stdout.strip()

    def push(self, *refspec: str, claude: bool = False, remote: str = "origin",
             **kw) -> subprocess.CompletedProcess:
        return self.run(["git", "push", remote, *(refspec or ("HEAD:refs/heads/main",))], claude, **kw)

    def mark(self, *rev: str) -> subprocess.CompletedProcess:
        return self.run([sys.executable, "tools/docs/push_hook.py", "--mark", *rev], claude=True)

    def remote_ref(self, ref: str) -> str | None:
        out = subprocess.run(["git", "--git-dir", str(self.remote), "rev-parse", "--verify", "-q", ref],
                             capture_output=True, text=True, env=make_env(False))
        return out.stdout.strip() or None


@pytest.fixture
def clone(tmp_path):
    return Clone(tmp_path)


@pytest.mark.parametrize("claude", [False, True])
def test_broken_link_rejects_the_push(clone, claude):
    clone.commit(BAD_README)
    out = clone.push(claude=claude)
    assert out.returncode != 0
    assert "README.md:3: broken link" in out.stderr
    assert clone.remote_ref("refs/heads/main") is None


@pytest.mark.parametrize("claude", [False, True])
def test_broken_link_rejects_even_when_marked(clone, claude):
    clone.commit(BAD_README)
    clone.mark()
    assert clone.push(claude=claude).returncode != 0


def test_clean_docs_without_claude_are_pushed(clone):
    sha = clone.commit()
    out = clone.push()
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main") == sha


def test_claude_without_marker_is_held_and_told_what_to_do(clone):
    sha = clone.commit()
    out = clone.push(claude=True)
    assert out.returncode != 0
    assert "--mark" in out.stderr and f"git log {sha} --not --remotes=origin" in out.stderr
    assert clone.remote_ref("refs/heads/main") is None


def test_claude_with_marker_is_pushed(clone):
    sha = clone.commit()
    assert clone.mark().returncode == 0
    out = clone.push(claude=True)
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main") == sha


def test_a_new_commit_needs_a_new_marker_and_names_the_remote_range(clone):
    first = clone.commit()
    clone.mark()
    assert clone.push(claude=True).returncode == 0
    second = clone.commit(GOOD_README + "\nMore.\n", "more")
    out = clone.push(claude=True)
    assert out.returncode != 0 and f"{first}..{second}" in out.stderr
    clone.mark()
    assert clone.push(claude=True).returncode == 0


def test_mark_takes_a_rev_and_does_not_duplicate(clone):
    first = clone.commit()
    clone.commit(GOOD_README + "\nMore.\n", "more")
    assert clone.mark("HEAD~1").returncode == 0
    assert clone.mark("HEAD~1").returncode == 0
    marker = Path(clone.git("rev-parse", "--absolute-git-dir").stdout.strip()) / "docs-checked"
    assert marker.read_text().split() == [first]
    assert clone.mark("no-such-rev").returncode == 1


def test_deleting_a_remote_branch_is_allowed(clone):
    sha = clone.commit()
    assert clone.push("HEAD:refs/heads/feature").returncode == 0
    assert clone.remote_ref("refs/heads/feature") == sha
    out = clone.push(":refs/heads/feature", claude=True)  # no marker, nothing to check
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/feature") is None


def test_annotated_tag_on_a_marked_commit_is_allowed(clone):
    sha = clone.commit()
    clone.mark()
    clone.git("tag", "-a", "v1", "-m", "v1")
    out = clone.push("refs/tags/v1", claude=True)
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/tags/v1^{commit}") == sha


def test_annotated_tag_on_an_unmarked_commit_is_held(clone):
    sha = clone.commit()
    clone.git("tag", "-a", "v1", "-m", "v1")
    out = clone.push("refs/tags/v1", claude=True)
    assert out.returncode != 0 and sha in out.stderr


def test_wrapper_without_the_script_lets_the_push_through(clone):
    clone.commit(BAD_README)
    (clone.dir / "tools" / "docs" / "push_hook.py").unlink()
    out = clone.push(claude=True)
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main")


def test_unexpected_error_fails_open(clone):
    clone.commit()
    line = f"refs/heads/main {'f' * 40} refs/heads/main {ZERO}\n"  # an object that does not exist
    out = clone.run([sys.executable, "tools/docs/push_hook.py", "origin", str(clone.remote)],
                    claude=True, input=line)
    assert out.returncode == 0 and "unexpected error" in out.stderr



def test_a_local_ref_with_spaces_is_checked(clone):
    clone.commit(BAD_README, "broken docs")
    out = clone.push("HEAD^{/broken docs}:refs/heads/main")
    assert out.returncode != 0 and "README.md:3: broken link" in out.stderr
    assert clone.remote_ref("refs/heads/main") is None


def test_the_abort_names_each_pushed_ref_with_its_short_sha(clone):
    sha = clone.commit(BAD_README)
    out = clone.push("HEAD:refs/heads/main", "HEAD:refs/heads/other")
    assert f"refs/heads/main ({sha[:10]})" in out.stderr and f"refs/heads/other ({sha[:10]})" in out.stderr
    assert f"not in git at {sha[:10]}" in out.stderr and "at HEAD" not in out.stderr


def test_only_the_tip_of_a_pushed_ref_is_checked(clone):
    clone.commit(BAD_README, "broken")
    sha = clone.commit(GOOD_README + "\nFixed.\n", "fixed")
    out = clone.push()
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main") == sha


# --- fail open, also at import time --------------------------------------------------------------

def break_check_docs(clone):
    (clone.dir / "tools" / "docs" / "check_docs.py").write_text("def broken(:\n", newline="\n")


def test_a_syntax_error_in_check_docs_does_not_block_a_push(clone):
    clone.commit(BAD_README)
    break_check_docs(clone)
    out = clone.push()
    assert out.returncode == 0, out.stderr
    assert "skipped" in out.stderr and clone.remote_ref("refs/heads/main")


def test_the_marker_is_still_required_when_check_docs_is_broken(clone):
    sha = clone.commit()
    break_check_docs(clone)
    out = clone.push(claude=True)
    assert out.returncode != 0 and "--mark" in out.stderr
    assert clone.mark().returncode == 0
    out = clone.push(claude=True)
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main") == sha


def test_an_invalid_setup_json_rejects_the_push_and_names_the_file(clone):
    (clone.dir / "web" / "lib" / "setup-downloads.json").write_text("{nope", newline="\n")
    clone.commit()
    out = clone.push()
    assert out.returncode != 0 and "web/lib/setup-downloads.json: not valid JSON" in out.stderr


# --- tips that were pushed before ----------------------------------------------------------------

def test_a_tip_that_is_already_on_a_remote_tracking_ref_is_not_checked(clone):
    clone.add_remote("mirror")
    old = clone.commit(BAD_README, "old")
    assert clone.push("--no-verify", "HEAD:refs/heads/main").returncode == 0  # pushed before the hook
    out = clone.push("HEAD:refs/heads/main", remote="mirror", claude=True)  # neither check applies
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main") == old
    clone.commit(BAD_README + "\nMore.\n", "new")  # a new commit with the same broken link
    out = clone.push("HEAD:refs/heads/main", remote="mirror")
    assert out.returncode != 0 and "README.md:3: broken link" in out.stderr


# --- the remote that is pushed to -----------------------------------------------------------------

def test_the_hint_uses_the_remote_that_is_pushed_to(clone):
    clone.add_remote("upstream")
    sha = clone.commit()
    out = clone.push("HEAD:refs/heads/feature", remote="upstream", claude=True)
    assert out.returncode != 0
    assert f"git log {sha} --not --remotes=upstream" in out.stderr and "origin" not in out.stderr
    clone.mark()
    assert clone.push("HEAD:refs/heads/main", remote="upstream", claude=True).returncode == 0
    new = clone.commit(GOOD_README + "\nMore.\n", "more")
    out = clone.push("HEAD:refs/heads/feature", remote="upstream", claude=True)
    assert out.returncode != 0 and f"git log upstream/main..{new}" in out.stderr


# --- the marker is shared by all worktrees -------------------------------------------------------

def test_a_mark_in_the_main_checkout_covers_a_linked_worktree(clone):
    sha = clone.commit()
    wt = clone.dir.parent / "linked"
    clone.git("worktree", "add", "--detach", str(wt), "HEAD")
    assert clone.mark().returncode == 0
    out = clone.push(claude=True, cwd=wt)
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main") == sha
    common = Path(clone.git("rev-parse", "--path-format=absolute", "--git-common-dir").stdout.strip())
    assert (common / "docs-checked").read_text().split() == [sha]


# --- the wrapper's Python probe ------------------------------------------------------------------

def stub_dir(tmp: Path, python_works: bool, python3_works: bool) -> Path:
    """A directory for the front of PATH: `python` and `python3` that exit 49 like the Microsoft
    Store alias without a Python, or that run this interpreter."""
    d = tmp / "stubs"
    d.mkdir()
    for name, works in (("python", python_works), ("python3", python3_works)):
        body = f"exec '{Path(sys.executable).as_posix()}' \"$@\"" if works else "exit 49"
        (d / name).write_text(f"#!/bin/sh\n{body}\n", newline="\n")
        (d / name).chmod(0o755)
    return d


def test_the_wrapper_skips_a_python_that_does_not_run(clone, tmp_path):
    env = make_env(False)
    env["PATH"] = str(stub_dir(tmp_path, False, True)) + os.pathsep + env["PATH"]
    clone.commit(BAD_README)
    out = clone.push(env=env)
    assert out.returncode != 0 and "README.md:3: broken link" in out.stderr


def test_the_wrapper_lets_the_push_through_without_any_working_python(clone, tmp_path):
    env = make_env(False)
    env["PATH"] = str(stub_dir(tmp_path, False, False)) + os.pathsep + env["PATH"]
    clone.commit(BAD_README)
    out = clone.push(env=env)
    assert out.returncode == 0, out.stderr
    assert clone.remote_ref("refs/heads/main")
