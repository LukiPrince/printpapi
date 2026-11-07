#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Claude Code PreToolUse hook: hold a `git push` until the docs are checked.

Wired up in .claude/settings.json for the Bash and PowerShell tools. Per
https://code.claude.com/docs/en/hooks.md the hook gets JSON on stdin (tool_name,
tool_input.command, cwd; the same command field for both tools). Exit 2 blocks the tool call and
stderr goes to Claude; exit 0 lets it run; any other exit code, or a crash, does NOT block.

A command that is not a `git push` exits 0 at once, with no git call. For a push, in the repo of
cwd: (1) the mechanical check of check_docs.py must be clean; (2) the marker file
`<git dir>/docs-checked` must hold the sha of HEAD, i.e. Claude compared README.md and docs/ with
what the push contains and then ran `python tools/docs/push_hook.py --mark`. The marker lives
inside .git, so it is never committed and works in worktrees. Any new commit makes it stale.

`--mark` writes the sha of HEAD into the marker (run it in the repo).

An unexpected error inside the hook: message on stderr, exit 1. That does not block: a broken hook
must never lock pushes.

Python 3 stdlib only.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_docs  # noqa: E402

# ponytail: only HEAD is checked. A push of another ref is checked against HEAD, and `git -C other
# push` against the repo of cwd.
PUSH = re.compile(r"\bgit\s+(?:(?:-[Cc]\s+\S+|--[\w-]+)\s+)*push\b")
MARKER_NAME = "docs-checked"


def is_push(command: str) -> bool:
    return bool(PUSH.search(command or ""))


def decide(command: str, head: str | None, marker: str | None, findings: list[str]) -> str | None:
    """None to allow the command, else the reason to block it."""
    if not is_push(command):
        return None
    if findings:
        return ("git push held: the docs check found problems. Fix them, commit, and push again.\n"
                + "\n".join(findings))
    if (marker or "").strip() != head:
        return (
            "git push held: the docs were not compared with this push yet.\n"
            "Compare README.md and docs/ with what the push contains (git log origin/main..HEAD, "
            "git diff origin/main...HEAD) and fix what is out of date (commit the fix first). Then run "
            "`python tools/docs/push_hook.py --mark` and push again. A new commit asks for this again."
        )
    return None


def git(cwd: str, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True).stdout.strip()


def marker_path(top: str) -> Path:
    path = Path(git(top, "rev-parse", "--git-path", MARKER_NAME))
    return path if path.is_absolute() else Path(top) / path


def main() -> int:
    for stream in (sys.stderr, sys.stdout):
        stream.reconfigure(encoding="utf-8", errors="replace")
    if "--mark" in sys.argv[1:]:
        top = git(".", "rev-parse", "--show-toplevel")
        head = git(top, "rev-parse", "HEAD")
        marker_path(top).write_text(head + "\n", encoding="utf-8")
        print(f"docs marked as checked for {head[:12]}")
        return 0
    data = json.loads(sys.stdin.buffer.read().decode("utf-8"))
    command = (data.get("tool_input") or {}).get("command") or ""
    if not is_push(command):
        return 0
    cwd = data.get("cwd")
    if not cwd or not Path(cwd).is_dir():
        cwd = "."  # an unusable cwd (a POSIX path on Windows Python): the process cwd is the project
    try:
        top = git(cwd, "rev-parse", "--show-toplevel")
    except (subprocess.CalledProcessError, FileNotFoundError, NotADirectoryError):
        return 0  # not in a repo: nothing to check
    root = Path(top)
    head = git(top, "rev-parse", "HEAD")
    try:
        marker = marker_path(top).read_text(encoding="utf-8")
    except FileNotFoundError:
        marker = None
    findings = check_docs.check(root, check_docs.tracked_markdown(root), check_docs.load_setup_assets(root))
    reason = decide(command, head, marker, findings)
    if reason:
        print(reason, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    try:
        code = main()
    except Exception as exc:  # fail open: exit 1 never blocks
        print(f"push_hook.py: unexpected error, not blocking: {exc!r}", file=sys.stderr)
        code = 1
    sys.exit(code)
