#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""git pre-push hook: the docs are checked before anything leaves the machine.

Started by tools/githooks/pre-push (turn it on once per clone: `git config core.hooksPath
tools/githooks`). Per https://git-scm.com/docs/githooks#_pre_push git calls the hook at the real
push with the remote name and URL as arguments and, on stdin, one line per ref to update:

    <local ref> SP <local object name> SP <remote ref> SP <remote object name> LF

A deletion has all zeros as the local object name, a new remote ref has them as the remote object
name. A non-zero exit aborts the push, and stderr tells the person why.

For every pushed commit (a tag is peeled to its commit, a deletion is skipped):

  1. always, also for the maintainer's own pushes: the mechanical check of check_docs.py on what
     that commit contains must be clean, else the push is aborted and the findings are listed;
  2. only when the environment variable CLAUDECODE is set: the commit must be in the marker file
     `<git dir>/docs-checked` (one sha per line), i.e. Claude compared README.md and docs/ with
     the pushed commits and then ran `python tools/docs/push_hook.py --mark`. The marker lives
     inside .git, so it is never committed and works in worktrees; a new commit has a new sha, so
     an old mark never covers it.

  # ponytail: Claude Code sets CLAUDECODE=1 in the shells it runs; that is observed, not
  # documented. If it ever disappears, only the mechanical part (1) runs.

`--mark [rev]` appends the commit sha of rev (HEAD by default) to the marker, without duplicates.

Exit 1 aborts the push, 0 allows it. An unexpected error inside the hook: message on stderr,
exit 0 (fail open: a broken hook must never lock pushes).

Python 3 stdlib only.
"""
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_docs  # noqa: E402

MARKER_NAME = "docs-checked"
ZEROS = "0"
DEFAULT_BASE = "origin/main"


def parse_updates(stdin_text: str) -> list[tuple[str, str]]:
    """(local object, remote object) of every stdin line that pushes something (no deletions)."""
    out = []
    for line in stdin_text.splitlines():
        parts = line.split()
        if len(parts) != 4 or set(parts[1]) == {ZEROS}:
            continue
        out.append((parts[1], parts[3]))
    return out


def decide(updates: list[tuple[str, str]], findings_by_commit: dict[str, list[str]],
           marked: set[str], in_claude: bool) -> str | None:
    """None to allow the push, else the reason to abort it.

    `updates` holds (commit, remote object) per pushed ref, the commit already peeled and the
    remote object all zeros for a new ref. `findings_by_commit` is check_docs per commit,
    `marked` the shas of the marker file.
    """
    commits = list(dict.fromkeys(commit for commit, _ in updates))
    findings = [f for commit in commits for f in findings_by_commit.get(commit, [])]
    if findings:
        return "The docs have problems, the push is aborted:\n" + "\n".join(findings)
    if not in_claude:
        return None
    todo: list[tuple[str, str]] = []
    for commit, remote in updates:
        if commit not in marked and commit not in [c for c, _ in todo]:
            todo.append((commit, remote))
    if not todo:
        return None
    ranges = [f"{remote}..{commit}" if set(remote) != {ZEROS} else f"{DEFAULT_BASE}..{commit}"
              for commit, remote in todo]
    lines = ["The push is held until the docs are checked against what it contains."]
    for rng in ranges:
        lines.append(f"  git log {rng}   and   git diff {rng}")
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


def toplevel() -> Path:
    return Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))


def marker_path(top: Path) -> Path:
    path = Path(git(top, "rev-parse", "--git-path", MARKER_NAME))
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


def pre_push(stdin_text: str) -> int:
    top = toplevel()
    updates = [(git(top, "rev-parse", "--verify", f"{oid}^{{commit}}"), remote)
               for oid, remote in parse_updates(stdin_text)]
    findings = {commit: check_docs.check_rev(top, commit)[0] for commit, _ in updates}
    reason = decide(updates, findings, read_marker(top), bool(os.environ.get("CLAUDECODE")))
    if reason:
        print(reason, file=sys.stderr)
        return 1
    return 0


def main(argv: list[str], stdin_text: str | None = None) -> int:
    if argv[:1] == ["--mark"]:
        try:
            return mark(*argv[1:2])
        except (subprocess.CalledProcessError, OSError) as e:
            print(f"push_hook: --mark failed: {e}", file=sys.stderr)
            return 1
    try:
        return pre_push(sys.stdin.read() if stdin_text is None else stdin_text)
    except Exception as e:  # fail open, see the module docstring
        print(f"push_hook: skipped, unexpected error: {e!r}", file=sys.stderr)
        return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
