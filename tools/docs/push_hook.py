#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""git pre-push hook: the docs are checked before anything leaves the machine.

Started by tools/githooks/pre-push. It runs only once `git config core.hooksPath tools/githooks`
is set in that clone. Per https://git-scm.com/docs/githooks#_pre_push git calls the hook at the
real push with the remote name and URL as arguments and, on stdin, one line per ref to update:

    <local ref> SP <local object name> SP <remote ref> SP <remote object name> LF

The local ref is "as it was originally given" and may contain spaces (`HEAD^{/some words}`); the
other three fields never do. A deletion has all zeros as the local object name, a new remote ref
has them as the remote object name. A non-zero exit aborts the push, and stderr tells the person
why.

Only the commit at the tip of each pushed ref is checked (a tag is peeled to its commit, a
deletion is skipped), not the commits in between: pull requests are squash-merged, so the tip is
what lands. A tip that is already on a remote-tracking ref was pushed before and is skipped
entirely (an old tag or a mirror push must not fail today's rules). For every other tip:

  1. always, also for the maintainer's own pushes: the mechanical check of check_docs.py on what
     that commit contains must be clean, else the push is aborted and the findings are listed;
  2. only when the environment variable CLAUDECODE is set: the commit must be in the marker file
     `<git common dir>/docs-checked` (one sha per line), i.e. Claude compared README.md and docs/
     with the pushed commits and then ran `python tools/docs/push_hook.py --mark`. The marker
     lives inside .git, so it is never committed; the common dir is shared by all worktrees of a
     clone, and a sha is the same in each of them. A new commit has a new sha, so an old mark
     never covers it. This holds also when check_docs itself is broken.

  # ponytail: Claude Code sets CLAUDECODE=1 in the shells it runs; that is observed, not
  # documented. If it ever disappears, only the mechanical part (1) runs.

`--mark [rev]` appends the commit sha of rev (HEAD by default) to the marker, without duplicates.

Exit 3 aborts the push (the wrapper turns it into git's non-zero), 0 allows it. The wrapper
lets every other exit code through, so a crash of this file (python exits 1 on a SyntaxError)
never blocks. An unexpected error inside the hook: message on stderr, exit 0 (fail open: a
broken hook must never lock pushes). That includes check_docs.py failing to
import (it is imported from the working tree): the mechanical check is skipped, the marker is
still required under CLAUDECODE.

Python 3 stdlib only.
"""
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

MARKER_NAME = "docs-checked"
ZEROS = "0"
OID = re.compile(r"[0-9a-f]{40,64}")
SHORT = 10  # characters of a sha in messages
ABORT = 3  # the only exit code tools/githooks/pre-push turns into an aborted push

Update = tuple[str, str, str]  # (remote ref, commit, remote object name)


def parse_updates(stdin_text: str) -> list[Update]:
    """(remote ref, local object, remote object) of every stdin line that pushes something."""
    out = []
    for line in stdin_text.splitlines():
        parts = line.rsplit(" ", 3)  # the local ref comes first and may hold spaces
        if len(parts) != 4 or not OID.fullmatch(parts[1]) or not OID.fullmatch(parts[3]):
            continue
        if set(parts[1]) == {ZEROS}:
            continue
        out.append((parts[2], parts[1], parts[3]))
    return out


def decide(updates: list[Update], findings_by_commit: dict[str, list[str]], marked: set[str],
           in_claude: bool, remote: str = "origin", remote_main: bool = True) -> str | None:
    """None to allow the push, else the reason to abort it.

    `updates` holds (remote ref, commit, remote object) per pushed ref, the commit already peeled
    and the remote object all zeros for a new ref. `findings_by_commit` is check_docs per commit,
    `marked` the shas of the marker file, `remote` the remote pushed to and `remote_main` whether
    `<remote>/main` exists locally.
    """
    lines = []
    for ref, commit, _ in updates:
        if findings_by_commit.get(commit):
            lines.append(f"{ref} ({commit[:SHORT]}):")
            lines += [f"  {f}" for f in findings_by_commit[commit]]
    if lines:
        return "The docs have problems, the push is aborted:\n" + "\n".join(lines)
    if not in_claude:
        return None
    todo: list[Update] = []
    for update in updates:
        if update[1] not in marked and update[1] not in [c for _, c, _ in todo]:
            todo.append(update)
    if not todo:
        return None
    lines = ["The push is held until the docs are checked against what it contains."]
    for _, commit, remote_oid in todo:
        if set(remote_oid) != {ZEROS}:
            rng = f"{remote_oid}..{commit}"
            lines.append(f"  git log {rng}   and   git diff {rng}")
        elif remote_main:
            rng = f"{remote}/main..{commit}"
            lines.append(f"  git log {rng}   and   git diff {rng}")
        else:
            lines.append(f"  git log {commit} --not --remotes={remote}   (-p shows the diffs)")
    lines += [
        "Compare README.md and docs/ with these commits and fix what is out of date (a fix is a "
        "new commit, so commit it and push that).",
        'When nothing is left to fix, run "python tools/docs/push_hook.py --mark" (the commit at '
        'HEAD) or "python tools/docs/push_hook.py --mark <rev>", then push again.',
    ]
    return "\n".join(lines)


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True, encoding="utf-8").stdout.strip()


def git_ok(cwd: Path, *args: str) -> bool:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True).returncode == 0


def toplevel() -> Path:
    return Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))


def marker_path(top: Path) -> Path:
    path = Path(git(top, "rev-parse", "--git-common-dir")) / MARKER_NAME
    return path if path.is_absolute() else top / path


def read_marker(top: Path) -> set[str]:
    try:
        return {line.strip() for line in marker_path(top).read_text(encoding="utf-8").splitlines()}
    except OSError:
        return set()


def mark(rev: str = "HEAD") -> int:
    top = toplevel()
    sha = git(top, "rev-parse", "--verify", f"{rev}^{{commit}}")
    if sha not in read_marker(top):
        path = marker_path(top)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8", newline="\n") as f:
            f.write(sha + "\n")
    print(f"docs checked for {sha}")
    return 0


def pre_push(stdin_text: str, remote: str = "origin") -> int:
    top = toplevel()
    updates: list[Update] = []
    for ref, oid, remote_oid in parse_updates(stdin_text):
        commit = git(top, "rev-parse", "--verify", f"{oid}^{{commit}}")
        if git(top, "rev-list", "-n", "1", commit, "--not", "--remotes"):  # else pushed before
            updates.append((ref, commit, remote_oid))
    findings: dict[str, list[str]] = {}
    try:
        import check_docs  # from the working tree: whatever is wrong with it must not block pushes
        for commit in dict.fromkeys(c for _, c, _ in updates):
            findings[commit] = check_docs.check_rev(top, commit, commit[:SHORT])[0]
    except Exception as e:  # fail open for the mechanical check, the marker below still applies
        print(f"push_hook: docs check skipped, unexpected error: {e!r}", file=sys.stderr)
        findings = {}
    reason = decide(updates, findings, read_marker(top), bool(os.environ.get("CLAUDECODE")),
                    remote, git_ok(top, "rev-parse", "--verify", "-q", f"refs/remotes/{remote}/main"))
    if reason:
        print(reason, file=sys.stderr)
        return ABORT
    return 0


def main(argv: list[str], stdin_text: str | None = None) -> int:
    if argv[:1] == ["--mark"]:
        try:
            return mark(*argv[1:2])
        except (subprocess.CalledProcessError, OSError) as e:
            print(f"push_hook: --mark failed: {e}", file=sys.stderr)
            return 1
    try:
        return pre_push(sys.stdin.read() if stdin_text is None else stdin_text,
                        argv[0] if argv else "origin")
    except Exception as e:  # fail open, see the module docstring
        print(f"push_hook: skipped, unexpected error: {e!r}", file=sys.stderr)
        return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
