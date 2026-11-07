# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Tests of tools/docs/check_docs.py: one pass and one fail case per rule, on fake repos."""
import subprocess

import check_docs
from check_docs import anchors_of, check, slug, tracked_set

ASSETS = {"printpapi-setup-windows-x86_64.exe", "printpapi-setup-linux-x86_64.deb"}


def repo(files: dict[str, str], tracked: set[str] | None = None) -> list[str]:
    """Check all the .md among the files; `tracked` defaults to the files (and their directories)."""
    paths = set(files) if tracked is None else set(tracked)
    md = [n for n in files if n.endswith(".md")]
    return check(md, files.__getitem__, tracked_set(paths), ASSETS)


# --- slugs ---------------------------------------------------------------------------------

def test_slug_plain_and_case():
    assert slug("Labels vs documents (the one rule)") == "labels-vs-documents-the-one-rule"


def test_slug_em_dash_leaves_two_hyphens():
    assert slug("DYMO LabelWriter — `pdf`, not raw") == "dymo-labelwriter--pdf-not-raw"


def test_slug_keeps_underscore_and_hyphen_and_digits():
    assert slug("snake_case and well-known 2x") == "snake_case-and-well-known-2x"


def test_slug_removes_emoji_and_punctuation():
    assert slug("Ready :rocket: \U0001F680 to go!") == "ready-rocket--to-go"


def test_slug_keeps_unicode_letters():
    assert slug("Über Straße") == "über-straße"


def test_slug_strips_inline_markup():
    assert slug("A **bold** and *em* [link](x.md) ![img](i.png) <b>tag</b> ~~gone~~") == (
        "a-bold-and-em-link-img-tag-gone"
    )


def test_slug_keeps_code_content_literally():
    assert slug("Use `a_b*c` here") == "use-a_bc-here"


def test_slug_file_output_heading():
    assert slug("File output (virtual print server)") == "file-output-virtual-print-server"


def test_slug_unescapes_html_entities():
    assert slug("Q&amp;A") == "qa"


def test_slug_drops_other_numbers_but_keeps_digits():
    assert slug("2¼ inch labels") == "2-inch-labels"
    assert slug("Step ① and ² and 3") == "step--and--and-3"


def test_slug_underscore_only_as_paired_emphasis():
    assert slug("snake_case name") == "snake_case-name"
    assert slug("_em_ and __strong__ and a_b_c") == "em-and-strong-and-a_b_c"
    assert slug("__init__ method") == "init-method"


def test_anchors_duplicates_follow_github_slugger():
    assert anchors_of("# echo\n\n# echo\n\n# echo 1\n") == {"echo", "echo-1", "echo-1-1"}


def test_anchors_duplicates_get_suffix():
    text = "# Setup\n\n## Setup\n\n## Setup\n"
    assert anchors_of(text) == {"setup", "setup-1", "setup-2"}


def test_anchors_ignore_headings_in_fences():
    text = "# Real\n\n```\n# fake\n```\n\n~~~sh\n# fake2\n~~~\n"
    assert anchors_of(text) == {"real"}


def test_anchors_explicit_a_name_and_id():
    text = '# T\n\n<a name="old-name"></a>\n<div id=\'box\'></div>\n'
    assert anchors_of(text) == {"t", "old-name", "box"}


def test_anchors_closing_hashes_and_indent():
    assert anchors_of("  ## Title ##\n") == {"title"}


# --- rule a: links resolve -----------------------------------------------------------------

def test_inline_link_ok():
    assert repo({"README.md": "[x](docs/a.md)\n", "docs/a.md": "# A\n"}) == []


def test_inline_link_broken_reports_path_line():
    out = repo({"README.md": "ok\n\n[x](docs/missing.md)\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")
    assert "docs/missing.md" in out[0]


def test_link_relative_to_the_markdown_file():
    files = {"docs/a.md": "[up](../README.md) [sib](b.md)\n", "README.md": "# R\n", "docs/b.md": ""}
    assert repo(files) == []
    out = repo({"docs/c.md": "[bad](docs/b.md)\n"})
    assert len(out) == 1 and out[0].startswith("docs/c.md:1: ")


def test_image_and_directory_target():
    files = {"README.md": "![alt](img/a.png) [dir](img/) [dir2](img) [root](./) [root2](.)\n", "img/a.png": "x"}
    assert repo(files) == []
    assert len(repo({"README.md": "![alt](img/b.png)\n"})) == 1


def test_reference_definition():
    assert repo({"README.md": "[id]: docs/a.md\n", "docs/a.md": ""}) == []
    out = repo({"README.md": "see [id]\n\n[id]: docs/none.md \"Title\"\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


def test_inline_html_src_and_href():
    assert repo({"README.md": '<img src="a.png"> <a href=\'b.md\'>x</a>\n',
                           "a.png": "x", "b.md": ""}) == []
    out = repo({"README.md": '<img src="nope.png"> <a href="nope.md">x</a>\n'})
    assert len(out) == 2


def test_title_angle_brackets_and_percent_20():
    files = {
        "README.md": '[a](docs/a.md "A title") [b](<docs/my file.md>) [c](docs/my%20file.md) '
                     "[d](docs/a.md 'single')\n",
        "docs/a.md": "",
        "docs/my file.md": "",
    }
    assert repo(files) == []


def test_urls_with_scheme_are_skipped():
    text = "[a](https://x.example/nope) [b](mailto:a@b.c) [c](http://x) [d](tel:1) [e](//cdn.x/y)\n"
    assert repo({"README.md": text}) == []


def test_code_fence_and_code_span_ignored():
    text = (
        "```\n[x](nope.md)\n```\n\n"
        "~~~\n[y](nope2.md)\n~~~\n\n"
        "`[z](nope3.md)` and ``[w](nope4.md)``\n"
    )
    assert repo({"README.md": text}) == []


def test_longer_fence_needs_matching_close():
    text = "````\n```\n[x](nope.md)\n```\n````\n\n[after](nope-after.md)\n"
    out = repo({"README.md": text})
    assert len(out) == 1 and out[0].startswith("README.md:7: ")


def test_root_relative_link():
    assert repo({"docs/a.md": "[r](/README.md)\n", "README.md": ""}) == []


def test_query_string_is_dropped():
    assert repo({"README.md": "[r](docs/a.md?plain=1)\n", "docs/a.md": ""}) == []


# --- rule b: fragments ---------------------------------------------------------------------

def test_same_file_fragment():
    assert repo({"README.md": "# Install\n\n[i](#install)\n"}) == []
    out = repo({"README.md": "# Install\n\n[i](#instal)\n"})
    assert len(out) == 1 and "#instal" in out[0] and out[0].startswith("README.md:3: ")


def test_fragment_on_other_md_file():
    files = {"README.md": "[a](docs/a.md#the-one-rule)\n", "docs/a.md": "## The one rule\n"}
    assert repo(files) == []
    files["docs/a.md"] = "## Other\n"
    assert len(repo(files)) == 1


def test_fragment_with_em_dash_heading():
    files = {
        "README.md": "[d](docs/agent.md#dymo-labelwriter--pdf-not-raw)\n",
        "docs/agent.md": "### DYMO LabelWriter — `pdf`, not raw\n",
    }
    assert repo(files) == []


def test_fragment_duplicate_heading_suffix():
    files = {"README.md": "[a](#x-1)\n\n# X\n\n# X\n"}
    assert repo(files) == []


def test_fragment_is_case_insensitive_and_unquoted():
    assert repo({"README.md": "# My Title\n\n[a](#My-Title) [b](#my%2Dtitle)\n"}) == []


def test_fragment_on_non_markdown_checks_file_only():
    files = {"README.md": "[a](src/main.rs#L10) [b](img/)\n[c](src/main.rs#whatever)\n",
             "src/main.rs": "fn main() {}\n"}
    files["img/x.png"] = ""
    assert repo(files) == []
    assert len(repo({"README.md": "[a](src/none.rs#L1)\n"})) == 1


def test_fragment_inside_fence_heading_does_not_count():
    out = repo({"README.md": "```\n# Hidden\n```\n\n[a](#hidden)\n"})
    assert len(out) == 1


# --- rule c: only what git has at HEAD -----------------------------------------------------

def test_target_not_in_git_is_a_finding_even_if_the_file_exists_on_disk():
    files = {"README.md": "[a](RELEASE_CHECKLIST.md) [b](docs/design.md) [c](target/)\n",
             "RELEASE_CHECKLIST.md": "", "docs/design.md": "", "target/x": ""}
    out = repo(files, tracked={"README.md"})
    assert len(out) == 3 and all("not in git at HEAD (missing or gitignored)" in line for line in out)


def test_maintainer_local_files_are_just_untracked():
    out = repo({"README.md": "[h](HANDOFF.md) [t](TODO.md) [p](docs/superpowers/plan.md)\n"})
    assert len(out) == 3 and all("not in git at HEAD" in line for line in out)


def test_directory_link_works_through_tracked_files():
    assert repo({"README.md": "[d](docs/sub/) [e](docs)\n", "docs/sub/a.md": ""}) == []


def test_tracked_set_adds_every_parent_directory():
    assert tracked_set({"a/b/c.md", "d.md"}) == {"a/b/c.md", "d.md", "a/b", "a", "."}


def test_link_to_other_md_reads_that_file_through_read():
    files = {"README.md": "[a](docs/a.md#there)\n", "docs/a.md": "# There\n"}
    assert repo(files) == []
    files["docs/a.md"] = "# Elsewhere\n"
    assert len(repo(files)) == 1


def test_code_span_crosses_a_line_break():
    out = repo({"README.md": "Run `cargo\ntest` then [x](nope.md) and `y`\n"})
    assert len(out) == 1 and "nope.md" in out[0] and out[0].startswith("README.md:2: ")


def test_code_span_does_not_cross_a_blank_line():
    out = repo({"README.md": "a ` b\n\n[x](nope.md) `\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


def test_link_inside_multiline_code_span_is_ignored():
    assert repo({"README.md": "see `a\n[x](nope.md)\nb` end\n"}) == []


# --- rule d: setup file names --------------------------------------------------------------

def test_known_setup_asset_ok():
    text = "Get printpapi-setup-windows-x86_64.exe, or `printpapi-setup-linux-x86_64.deb`.\n"
    assert repo({"README.md": text}) == []


def test_unknown_setup_asset_found_with_line():
    out = repo({"README.md": "x\n`printpapi-setup-windows-arm64.exe` here\n"})
    assert len(out) == 1 and out[0].startswith("README.md:2: ")
    assert "printpapi-setup-windows-arm64.exe" in out[0]


def test_setup_asset_in_url_and_fence_is_checked():
    text = "[d](https://x/releases/download/printpapi-setup-bogus.exe)\n```\nprintpapi-setup-bogus2.msi\n```\n"
    assert len(repo({"README.md": text})) == 2


def test_setup_placeholder_and_trailing_dot():
    text = "Use `printpapi-setup-<platform>` or printpapi-setup-windows-x86_64.exe.\nThe printpapi-setup app.\n"
    assert repo({"README.md": text}) == []


# --- rule e: open source -------------------------------------------------------------------

def test_open_source_claim_is_found():
    out = repo({"README.md": "ok\nprintpapi is Open Source.\nan open-source tool\n"})
    assert len(out) == 2 and out[0].startswith("README.md:2: ") and out[1].startswith("README.md:3: ")


def test_open_source_with_not_or_never_is_fine():
    text = "It is **source available**, not OSI open source.\nNever call it \"open-source\".\n"
    assert repo({"README.md": text}) == []


def test_not_must_be_a_word():
    assert len(repo({"README.md": "We note this open source thing\n"})) == 1


def test_wording_works_per_paragraph_for_hard_wrapped_docs():
    text = 'The project is source available and never\n"open source" in the OSI sense.\n'
    assert repo({"README.md": text}) == []
    assert repo({"README.md": "Sadly not a\nsurprise: it is open\nsource.\n"}) == []


def test_wrapped_open_source_without_not_fails_at_the_match_line():
    out = repo({"README.md": "# T\n\nA tool that\nis open\nsource, truly.\n"})
    assert len(out) == 1 and out[0].startswith("README.md:4: ")


def test_not_in_another_paragraph_does_not_excuse():
    out = repo({"README.md": "It is not a toy.\n\nWe are open source.\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


def test_open_source_in_a_fence_is_checked_per_line():
    out = repo({"README.md": "```\nnot this\nopen source\n```\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


# --- helpers -------------------------------------------------------------------------------

def test_crlf_line_endings():
    out = repo({"README.md": "# T\r\n\r\n[a](#t) [b](nope.md)\r\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


def test_setup_assets_from_json():
    text = '[{"platform": "windows", "asset": "printpapi-setup-a.exe"}, {"asset": "printpapi-setup-b.dmg"}]'
    files = {"web/lib/setup-downloads.json": text}
    read, tracked = files.__getitem__, tracked_set(set(files))
    assert check_docs.setup_assets_at(read, tracked) == {"printpapi-setup-a.exe", "printpapi-setup-b.dmg"}
    assert check_docs.setup_assets_at(read, set()) == set()


# --- the revision, not the working tree ---------------------------------------------------

def git(cwd, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", *args],
        cwd=cwd, check=True, capture_output=True, text=True,
    ).stdout.strip()


def test_check_rev_uses_head_content_and_tracked_files(tmp_path):
    git(tmp_path, "init", "-q")
    (tmp_path / ".gitignore").write_text("secret.md\n")
    (tmp_path / "web" / "lib").mkdir(parents=True)
    (tmp_path / "web" / "lib" / "setup-downloads.json").write_text('[{"asset": "printpapi-setup-ok.exe"}]')
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "a.md").write_text("# A\n")
    (tmp_path / "README.md").write_text(
        "[a](docs/a.md#a) [s](secret.md) printpapi-setup-ok.exe\n", encoding="utf-8")
    (tmp_path / "secret.md").write_text("# S\n")
    git(tmp_path, "add", "-A")
    git(tmp_path, "commit", "-q", "-m", "init")
    findings, count = check_docs.check_rev(tmp_path, "HEAD")
    assert count == 2
    assert len(findings) == 1 and "secret.md" in findings[0] and "not in git at HEAD" in findings[0]
    # an edit that is not committed is not what gets pushed
    (tmp_path / "README.md").write_text("[bad](nope.md)\n")
    (tmp_path / "docs" / "new.md").write_text("[bad](nope.md)\n")
    assert check_docs.check_rev(tmp_path, "HEAD") == (findings, 2)
