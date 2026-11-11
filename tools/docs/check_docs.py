#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Mechanical check of the Markdown docs: links, anchors, setup file names, wording.

Checks what git has at a revision (HEAD by default), not the working tree: every Markdown file in
`git ls-tree -r HEAD`, read with `git show`. A gitignored file on the maintainer's disk is not
there for CI or for a reader of the repo, so it is a finding locally exactly as in CI.

  a. a relative link (inline, image, reference definition, HTML src/href) points at a file or
     directory that is in git at that revision (relative to the Markdown file; links with a scheme
     are skipped). This also catches links to the gitignored maintainer-local files (HANDOFF.md,
     TODO.md, docs/plans/, docs/specs/), see AGENTS.md "Maintainer-local files";
  b. a #fragment on a same-file link or a link to another .md file matches a heading anchor of
     that file, computed like GitHub does (see `slug`);
  c. a literal setup file name (printpapi-setup-...) is one of the assets in
     web/lib/setup-downloads.json (at the same revision);
  d. "open source" only in a paragraph (or list item) with a "not" or "never": the project is
     "source available".

Fenced code blocks and inline code spans (which may span the lines of a paragraph) are ignored for
links and headings; file names (c) and wording (d) are checked everywhere. `check` is pure: it
gets the Markdown file list, a `read(path)` callable, the set of tracked paths and the setup
assets. `main` asks git. Prints "path:line: message" per finding and exits 1 if there are any.

Python 3 stdlib only.
"""
import html
import json
import os
import posixpath
import re
import subprocess
import sys
import unicodedata
from collections.abc import Callable
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parents[2]
SETUP_JSON = "web/lib/setup-downloads.json"
# Decimal and letter numbers (not 1/4 or a superscript two) and connector punctuation (_) stay in
# an anchor, next to every letter and mark.
KEEP_CATEGORIES = {"Nd", "Nl", "Pc"}

FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
HEADING = re.compile(r"^ {0,3}#{1,6}[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$")
CODE_SPAN = re.compile(r"(`+)(?!`)(.+?)(?<!`)\1(?!`)", re.S)
EMPHASIS_UNDERSCORE = re.compile(r"(?<!\w)_{1,3}(?=\S)(.+?)(?<=\S)_{1,3}(?!\w)")
INLINE_LINK = re.compile(r"\]\(\s*(<[^>]*>|[^\s)]*)")
REF_DEF = re.compile(r"^ {0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)")
HTML_REF = re.compile(r"""\b(?:src|href)\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.I)
HTML_ANCHOR = re.compile(r"""<[A-Za-z][^>]*?\b(?:name|id)\s*=\s*["']([^"']+)["']""", re.I)
SCHEME = re.compile(r"^(?:[A-Za-z][A-Za-z0-9+.-]*:|//)")
SETUP_NAME = re.compile(r"printpapi-setup-[A-Za-z0-9_.-]*[A-Za-z0-9_]")
OPEN_SOURCE = re.compile(r"open[\s-]+source", re.I)  # \s: the docs are hard-wrapped
NOT_WORD = re.compile(r"\b(?:not|never)\b", re.I)
LIST_ITEM = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s")


def lines_of(text: str):
    """Yield (line number, line, in_fence); the fence lines themselves count as in_fence."""
    fence = None
    for no, line in enumerate(text.split("\n"), 1):
        line = line.rstrip("\r")
        m = FENCE.match(line)
        if fence is None:
            if m and not (m.group(1)[0] == "`" and "`" in m.group(2)):
                fence = m.group(1)
                yield no, line, True
                continue
            yield no, line, False
        else:
            if m and m.group(1)[0] == fence[0] and len(m.group(1)) >= len(fence) and not m.group(2).strip():
                fence = None
            yield no, line, True


def blank_code(text: str) -> str:
    """The text with every inline code span replaced by spaces (line breaks stay)."""
    return CODE_SPAN.sub(lambda m: re.sub(r"[^\n]", " ", m.group(0)), text)


def blocks_of(text: str):
    """Yield (block, in_fence), a block being a list of (line number, line).

    A prose block is a paragraph: consecutive non-blank lines, cut at a fence and around an ATX
    heading (a heading is a block of its own). Every line of a fence is a block of its own.
    """
    block: list[tuple[int, str]] = []
    for no, line, in_fence in lines_of(text):
        if in_fence:
            if block:
                yield block, False
                block = []
            yield [(no, line)], True
        elif not line.strip():
            if block:
                yield block, False
                block = []
        elif HEADING.match(line):
            if block:
                yield block, False
            block = []
            yield [(no, line)], False
        else:
            block.append((no, line))
    if block:
        yield block, False


def item_blocks(text: str):
    """`blocks_of`, with every prose block also cut where a list item starts."""
    for block, in_fence in blocks_of(text):
        if in_fence:
            yield block, in_fence
            continue
        part: list[tuple[int, str]] = []
        for no, line in block:
            if part and LIST_ITEM.match(line):
                yield part, False
                part = []
            part.append((no, line))
        yield part, False


def prose_lines(text: str) -> dict[int, str]:
    """Line number -> the line with inline code blanked, for the lines outside fences.

    A code span can cross a line break inside a hard-wrapped paragraph, so the code is blanked per
    prose block (see `blocks_of`).
    """
    out: dict[int, str] = {}
    for block, in_fence in blocks_of(text):
        if not in_fence:
            blanked = blank_code("\n".join(line for _, line in block)).split("\n")
            out.update((no, line) for (no, _), line in zip(block, blanked))
    return out


def wording_findings(text: str) -> list[tuple[int, str]]:
    """(line number, message) for every "open source" in a block without a "not" or "never".

    The docs are hard-wrapped, so the rule works per paragraph, not per line. A list item is a
    block of its own (a "not" in one bullet must not excuse the next one).
    """
    out = []
    for block, _ in item_blocks(text):
        joined = "\n".join(line for _, line in block)
        # ponytail: a paragraph with "not" or "never" anywhere passes, even if it says something
        # else ("not only open source"). A word list per paragraph, not a parse.
        if NOT_WORD.search(joined):
            continue
        for m in OPEN_SOURCE.finditer(joined):
            no = block[joined.count("\n", 0, m.start())][0]
            out.append((no, '"open source": the project is source available (say "not" or "never")'))
    return out


def strip_markup(text: str) -> str:
    """Heading text as GitHub reads it for the anchor: inline markup gone, code content kept."""
    codes: list[str] = []

    def stash(m):
        codes.append(m.group(2))
        return f"\x00{len(codes) - 1}\x00"

    text = CODE_SPAN.sub(stash, text)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)  # images and links -> their text
    text = re.sub(r"\[([^\]]*)\]\[[^\]]*\]", r"\1", text)  # reference links
    text = re.sub(r"<[^>]+>", "", text)
    # `_em_` and `__strong__` lose their underscores, snake_case keeps them. `*em*` needs no rule:
    # `*` is punctuation, which `slug` drops anyway.
    text = EMPHASIS_UNDERSCORE.sub(r"\1", text)
    text = html.unescape(text)  # &amp; -> &, dropped like any punctuation (code stays literal)
    return re.sub(r"\x00(\d+)\x00", lambda m: codes[int(m.group(1))], text)


def slug(heading: str) -> str:
    """GitHub's heading anchor (github-slugger): lowercase, keep letters, marks, Nd, Nl, Pc, space
    and -, then space -> -."""
    text = strip_markup(heading.strip()).lower()
    kept = "".join(
        c for c in text
        if c in " -" or (cat := unicodedata.category(c)) in KEEP_CATEGORIES or cat[0] in "LM"
    )
    return kept.replace(" ", "-")


def anchors_of(text: str) -> set[str]:
    """Every anchor a Markdown file offers: its headings and <a name>/id."""
    found: set[str] = set()
    seen: dict[str, int] = {}  # github-slugger's occurrence counter
    # ponytail: ATX headings (# Title) only; a setext heading (Title over ====) is not an anchor
    # here, so a link to one is reported as broken. Nothing in the docs uses them.
    prose = prose_lines(text)
    for no, line, in_fence in lines_of(text):
        if in_fence:
            continue
        m = HEADING.match(line)
        if m:
            base = result = slug(m.group(1))
            while result in seen:
                seen[base] += 1
                result = f"{base}-{seen[base]}"
            seen[result] = 0
            found.add(result)
        found.update(HTML_ANCHOR.findall(prose.get(no, "")))
    return found


def link_targets(line: str) -> list[str]:
    """Raw link targets on one line (already free of code spans)."""
    out = [m.group(1) for m in INLINE_LINK.finditer(line)]
    m = REF_DEF.match(line)
    if m:
        out.append(m.group(1))
    out += [a or b for a, b in HTML_REF.findall(line)]
    return [t[1:-1] if t.startswith("<") and t.endswith(">") else t for t in out]


def tracked_set(paths) -> set[str]:
    """The tracked file paths plus every directory above them ("." is the root), so a link to a
    directory resolves."""
    out = {"."}
    for path in paths:
        out.add(path)
        parent = posixpath.dirname(path)
        while parent and parent not in out:
            out.add(parent)
            parent = posixpath.dirname(parent)
    return out


def check(md_files: list[str], read: Callable[[str], str], tracked: set[str],
          setup_assets: set[str] | None, rev: str = "HEAD") -> list[str]:
    """Findings ("path:line: message") for the Markdown files (repo-relative, '/'-separated).

    `read(path)` returns a tracked file's text; `tracked` holds the tracked files and directories;
    `rev` names the checked revision in the messages. `setup_assets` None skips rule c (the list
    itself is unreadable, which is a finding of its own).
    """
    findings: list[str] = []
    anchor_cache: dict[str, set[str]] = {}

    def anchors(rel: str) -> set[str]:
        if rel not in anchor_cache:
            anchor_cache[rel] = anchors_of(read(rel))
        return anchor_cache[rel]

    for md in md_files:
        text = read(md)
        prose = prose_lines(text)
        findings += [f"{md}:{no}: {msg}" for no, msg in wording_findings(text)]
        for no, line, in_fence in lines_of(text):
            where = f"{md}:{no}: "
            for name in SETUP_NAME.findall(line):
                if setup_assets is not None and name not in setup_assets:
                    findings.append(f"{where}setup file name {name} is not in {SETUP_JSON}")
            if in_fence:
                continue
            for target in link_targets(prose.get(no, "")):
                if not target or SCHEME.match(target):
                    continue
                path, _, fragment = target.partition("#")
                path = unquote(path.partition("?")[0])
                fragment = unquote(fragment).lower()
                if path.startswith("/"):
                    rel = os.path.normpath(path.lstrip("/"))
                elif path:
                    rel = os.path.normpath(os.path.join(os.path.dirname(md), path))
                else:
                    rel = md
                rel = rel.replace("\\", "/")
                if rel not in tracked:
                    findings.append(
                        f"{where}broken link {target}: {rel} is not in git at {rev} (missing or gitignored)")
                    continue
                if fragment and rel.lower().endswith(".md") and fragment not in anchors(rel):
                    findings.append(f"{where}broken anchor {target}: no heading #{fragment} in {rel}")
    return findings


def setup_assets_at(read: Callable[[str], str], tracked: set[str]) -> set[str]:
    """The "asset" file names of web/lib/setup-downloads.json (empty if it is not tracked)."""
    if SETUP_JSON not in tracked:
        return set()
    return {row["asset"] for row in json.loads(read(SETUP_JSON)) if "asset" in row}


def git_out(root: Path, *args: str) -> bytes:
    return subprocess.run(["git", *args], cwd=root, check=True, capture_output=True).stdout


def check_rev(root: Path, rev: str = "HEAD", label: str | None = None) -> tuple[list[str], int]:
    """(findings, number of Markdown files) for what git has at `rev` in the repo at `root`.

    `label` (default `rev`) is how the messages name the revision.
    """
    listing = git_out(root, "ls-tree", "-r", "-z", "--name-only", rev).decode("utf-8")
    files = [f for f in listing.split("\0") if f]
    tracked = tracked_set(files)
    cache: dict[str, str] = {}

    def read(path: str) -> str:
        if path not in cache:
            cache[path] = git_out(root, "show", f"{rev}:{path}").decode("utf-8", errors="replace")
        return cache[path]

    md = [f for f in files if f.lower().endswith(".md")]
    extra: list[str] = []
    assets: set[str] | None
    try:
        assets = setup_assets_at(read, tracked)
    except (ValueError, TypeError, AttributeError) as e:  # bad JSON, or not a list of objects
        extra.append(f"{SETUP_JSON}: not valid JSON: {e}")
        assets = None
    except subprocess.CalledProcessError as e:  # in the tree but not readable (a submodule entry)
        extra.append(f"{SETUP_JSON}: not readable at {label or rev}: {e}")
        assets = None
    return extra + check(md, read, tracked, assets, label or rev), len(md)


def main() -> int:
    findings, count = check_rev(REPO)
    for f in findings:
        print(f)
    if findings:
        print(f"{len(findings)} problem(s) in the docs", file=sys.stderr)
        return 1
    print(f"OK: {count} Markdown files at HEAD, links, anchors, setup file names and wording are fine")
    return 0


if __name__ == "__main__":
    sys.exit(main())
