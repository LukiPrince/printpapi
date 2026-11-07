#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Claude Code PreToolUse hook: hold a `git push` until the docs are checked.

Wired up in .claude/settings.json for the Bash and PowerShell tools. Per
https://code.claude.com/docs/en/hooks.md the hook gets JSON on stdin (tool_name,
tool_input.command, cwd; the same command field for both tools). Exit 2 blocks the tool call and
stderr goes to Claude; exit 0 lets it run; any other exit code (1, a crash) does not block.

The settings command only starts this script if it exists (python exits 2 on a missing file, which
would block every Bash call) and sets `onFailure: "continue"`, for Windows without Git Bash where
the shell form runs in PowerShell and fails to parse. The hook acts only in a repo that has this
file at tools/docs/push_hook.py.

A command that is not a `git push` exits 0 at once, with no git call. For a push, in the repo of
cwd: (1) the mechanical check of check_docs.py on what HEAD contains must be clean; (2) the marker
file `<git dir>/docs-checked` must hold the sha of HEAD, i.e. Claude compared README.md and docs/
with what the push contains and then ran `python tools/docs/push_hook.py --mark` as a command of
its own. The marker lives inside .git, so it is never committed and works in worktrees. Any new
commit makes it stale. The hook runs before the command, so a push chained behind another git
command (a commit, say) is blocked: HEAD would change under it.

`--mark` writes the sha of HEAD into the marker (run it in the repo).

An unexpected error inside the hook: message on stderr, exit 1. That does not block, so a broken
hook does not lock pushes.

Python 3 stdlib only.
"""
import json
import re
import shlex
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_docs  # noqa: E402

MARKER_NAME = "docs-checked"
HOOK_FILE = Path("tools") / "docs" / "push_hook.py"

NONE, PUSH, CHAINED = "none", "push", "chained"

# Separators of simple commands: ; && || | & ( ) line breaks (and < > for redirections, ` for
# command substitution). Anything quoted stays inside its word.
SEPARATORS = ";()<>|&\n`"
ENV_WORD = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
GIT_NAMES = {"git", "git.exe"}
# git's global options that take a value, as the next word (or as --opt=value).
GIT_VALUE_OPTIONS = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--super-prefix",
                     "--config-env", "--attr-source"}


def tokens(command: str) -> list[str]:
    """The words of a command line, with every run of separator characters as a word of its own."""
    command = re.sub(r"\\\r?\n", " ", command)  # a backslash line continuation is a space

    def lex(text: str) -> list[str]:
        lexer = shlex.shlex(text, posix=True, punctuation_chars=SEPARATORS)
        lexer.whitespace = " \t\r"  # a line break separates commands
        lexer.whitespace_split = True
        lexer.commenters = ""
        return list(lexer)

    try:
        return lex(command)
    except ValueError:
        # Unbalanced quote: cannot tell what is quoted, so read every word as code. That holds a
        # push that might be quoted text rather than letting a real one through.
        return lex(re.sub(r"[\"'\\]", " ", command))


def simple_commands(command: str) -> list[list[str]]:
    out: list[list[str]] = [[]]
    for word in tokens(command):
        if all(c in SEPARATORS for c in word):
            out.append([])
        else:
            out[-1].append(word)
    return [argv for argv in out if argv]


def git_subcommand(argv: list[str]) -> tuple[str | None, list[str]]:
    """(subcommand, its arguments) of a simple command whose argv[0] is git; (None, []) if it
    is not git or has no subcommand."""
    while argv and ENV_WORD.match(argv[0]):
        argv = argv[1:]
    if not argv or re.split(r"[\\/]", argv[0])[-1].lower() not in GIT_NAMES:
        return None, []
    i = 1
    while i < len(argv):
        word = argv[i]
        if word in GIT_VALUE_OPTIONS:
            i += 2
        elif word.startswith("-"):  # a flag, or --opt=value
            i += 1
        else:
            return word, argv[i + 1:]
    return None, []


def classify(command: str) -> str:
    """NONE (not a push), PUSH (a push), or CHAINED (a push next to another git command).

    ponytail: no look into `bash -c "..."`, `eval`, aliases or `xargs`; a heredoc body line that
    starts with "git push" is read as a command. This is a guard rail for Claude, not a sandbox.
    """
    pushes = others = 0
    for argv in simple_commands(command or ""):
        sub, args = git_subcommand(argv)
        if sub is None:
            continue
        if sub != "push":
            others += 1
        elif "--dry-run" not in args and "-n" not in args:  # a dry run pushes nothing
            pushes += 1
    if not pushes:
        return NONE
    return CHAINED if others else PUSH


def is_push(command: str) -> bool:
    return classify(command) != NONE


def decide(command: str, head: str | None, marker: str | None, findings: list[str]) -> str | None:
    """None to allow the command, else the reason to block it."""
    kind = classify(command)
    if kind == NONE:
        return None
    if kind == CHAINED:
        return (
            "git push held: run git push as a command of its own, because the hook compares HEAD "
            "before the command runs (a commit chained in front of the push changes what is "
            "pushed). Run the other git commands first, then `git push` alone."
        )
    if findings:
        return ("git push held: the docs check found problems. Fix them, commit, and push again.\n"
                + "\n".join(findings))
    if (marker or "").strip() != head:
        return (
            "git push held: the docs were not compared with this push yet.\n"
            "Compare README.md and docs/ with what the push contains (git log origin/main..HEAD, "
            "git diff origin/main...HEAD) and fix what is out of date (commit the fix first). Then "
            "run `python tools/docs/push_hook.py --mark` as a command of its own, and after it the "
            "push alone. `--mark && git push` in one command can never pass: the hook checks the "
            "marker before the command runs. A new commit asks for this again."
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
    if not (root / HOOK_FILE).is_file():
        return 0  # another project: this hook is not its business
    # ponytail: only HEAD is checked. A push of another ref is checked against HEAD, and
    # `git -C other push` against the repo of cwd.
    head = git(top, "rev-parse", "HEAD")
    try:
        marker = marker_path(top).read_text(encoding="utf-8")
    except FileNotFoundError:
        marker = None
    findings, _ = check_docs.check_rev(root, head)
    reason = decide(command, head, marker, findings)
    if reason:
        print(reason, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    try:
        code = main()
    except Exception as exc:  # fail open: exit 1 does not block
        print(f"push_hook.py: unexpected error, not blocking: {exc!r}", file=sys.stderr)
        code = 1
    sys.exit(code)
