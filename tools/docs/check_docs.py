#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Mechanical check of the Markdown docs: links, anchors, setup file names, wording.

Checks every Markdown file git tracks at the repo root:

  a. a relative link (inline, image, reference definition, HTML src/href) points at a file or
     directory that exists, relative to the Markdown file; links with a scheme are skipped;
  b. a #fragment on a same-file link or a link to another .md file matches a heading anchor of
     that file, computed like GitHub does (see `slug`);
  c. nothing links to a maintainer-local file (HANDOFF.md, TODO.md, docs/superpowers/): they are
     gitignored, see AGENTS.md "Maintainer-local files";
  d. a literal setup file name (printpapi-setup-...) is one of the assets in
     web/lib/setup-downloads.json;
  e. "open source" only next to a "not" or "never": the project is "source available".

Fenced code blocks and inline code spans are ignored for links and headings; file names (d) and
wording (e) are checked everywhere. `check` is pure (it only reads files under `root`); `main`
asks git for the file list. Prints "path:line: message" per finding and exits 1 if there are any.

Python 3 stdlib only.
"""
import json
import os
import re
import subprocess
import sys
import unicodedata
from pathlib import Path
from urllib.parse import unquote

REPO = Path(__file__).resolve().parents[2]

FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
HEADING = re.compile(r"^ {0,3}#{1,6}[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$")
CODE_SPAN = re.compile(r"(`+)(?!`)(.+?)(?<!`)\1(?!`)")
INLINE_LINK = re.compile(r"\]\(\s*(<[^>]*>|[^\s)]*)")
REF_DEF = re.compile(r"^ {0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)")
HTML_REF = re.compile(r"""\b(?:src|href)\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.I)
HTML_ANCHOR = re.compile(r"""<[A-Za-z][^>]*?\b(?:name|id)\s*=\s*["']([^"']+)["']""", re.I)
SCHEME = re.compile(r"^(?:[A-Za-z][A-Za-z0-9+.-]*:|//)")
SETUP_NAME = re.compile(r"printpapi-setup-[A-Za-z0-9_.-]*[A-Za-z0-9_]")
OPEN_SOURCE = re.compile(r"open[ -]source", re.I)
NOT_WORD = re.compile(r"\b(?:not|never)\b", re.I)


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


def blank_code(line: str) -> str:
    """The line with every inline code span replaced by spaces."""
    return CODE_SPAN.sub(lambda m: " " * len(m.group(0)), line)


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
    text = re.sub(r"\*+|~~", "", text)
    text = re.sub(r"(?<!\w)_+|_+(?!\w)", "", text)  # _em_, but not snake_case
    return re.sub(r"\x00(\d+)\x00", lambda m: codes[int(m.group(1))], text)


def slug(heading: str) -> str:
    """GitHub's heading anchor: lowercase, keep letters, digits, marks, space, - and _, space -> -."""
    text = strip_markup(heading.strip()).lower()
    kept = "".join(c for c in text if c in " -_" or unicodedata.category(c)[0] in "LNM")
    return kept.replace(" ", "-")


def anchors_of(text: str) -> set[str]:
    """Every anchor a Markdown file offers: its headings (duplicates -1, -2, ...) and <a name>/id."""
    found: set[str] = set()
    seen: dict[str, int] = {}
    # ponytail: ATX headings (# Title) only; a setext heading (Title over ====) is not an anchor
    # here, so a link to one is reported as broken. Nothing in the docs uses them.
    for _, line, in_fence in lines_of(text):
        if in_fence:
            continue
        m = HEADING.match(line)
        if m:
            base = slug(m.group(1))
            n = seen.get(base, 0)
            seen[base] = n + 1
            found.add(base if n == 0 else f"{base}-{n}")
        found.update(HTML_ANCHOR.findall(blank_code(line)))
    return found


def link_targets(line: str) -> list[str]:
    """Raw link targets on one line (already free of code spans)."""
    out = [m.group(1) for m in INLINE_LINK.finditer(line)]
    m = REF_DEF.match(line)
    if m:
        out.append(m.group(1))
    out += [a or b for a, b in HTML_REF.findall(line)]
    return [t[1:-1] if t.startswith("<") and t.endswith(">") else t for t in out]


def is_maintainer_local(rel: str) -> bool:
    rel = rel.lower()
    return rel in ("handoff.md", "todo.md") or rel == "docs/superpowers" or rel.startswith("docs/superpowers/")


def check(root: Path, md_files: list[str], setup_assets: set[str]) -> list[str]:
    """Findings ("path:line: message") for the Markdown files (paths relative to root)."""
    findings: list[str] = []
    anchor_cache: dict[str, set[str]] = {}

    def anchors(rel: str) -> set[str]:
        if rel not in anchor_cache:
            anchor_cache[rel] = anchors_of((root / rel).read_text(encoding="utf-8", errors="replace"))
        return anchor_cache[rel]

    for md in md_files:
        text = (root / md).read_text(encoding="utf-8", errors="replace")
        for no, line, in_fence in lines_of(text):
            where = f"{md}:{no}: "
            for name in SETUP_NAME.findall(line):
                if name not in setup_assets:
                    findings.append(f"{where}setup file name {name} is not in web/lib/setup-downloads.json")
            # ponytail: a line with "not" or "never" next to "open source" passes, even if it says
            # something else ("not only open source"). A word list, not a parse.
            if OPEN_SOURCE.search(line) and not NOT_WORD.search(line):
                findings.append(f'{where}"open source": the project is source available (say "not" or "never")')
            if in_fence:
                continue
            for target in link_targets(blank_code(line)):
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
                if is_maintainer_local(rel):
                    findings.append(f"{where}link to {target}: maintainer-local file, gitignored, not in the repo")
                    continue
                dest = root / rel
                if not dest.exists():
                    findings.append(f"{where}broken link {target}: {rel} does not exist")
                    continue
                if fragment and dest.is_file() and rel.lower().endswith(".md") and fragment not in anchors(rel):
                    findings.append(f"{where}broken anchor {target}: no heading #{fragment} in {rel}")
    return findings


def load_setup_assets(root: Path) -> set[str]:
    """The "asset" file names of web/lib/setup-downloads.json (empty if the file is missing)."""
    try:
        rows = json.loads((root / "web" / "lib" / "setup-downloads.json").read_text(encoding="utf-8"))
    except FileNotFoundError:
        return set()
    return {row["asset"] for row in rows if "asset" in row}


def tracked_markdown(root: Path) -> list[str]:
    """Markdown files git tracks at the repo root (those still on disk)."""
    out = subprocess.run(["git", "ls-files", "-z", "--", "*.md"], cwd=root, check=True,
                         capture_output=True).stdout.decode("utf-8")
    return [f for f in out.split("\0") if f and (root / f).is_file()]


def main() -> int:
    files = tracked_markdown(REPO)
    findings = check(REPO, files, load_setup_assets(REPO))
    for f in findings:
        print(f)
    if findings:
        print(f"{len(findings)} problem(s) in the docs", file=sys.stderr)
        return 1
    print(f"OK: {len(files)} Markdown files, links, anchors, setup file names and wording are fine")
    return 0


if __name__ == "__main__":
    sys.exit(main())
