# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Tests of tools/docs/check_docs.py: one pass and one fail case per rule, on fake repos."""
from pathlib import Path

import check_docs
from check_docs import anchors_of, check, slug

ASSETS = {"printpapi-setup-windows-x86_64.exe", "printpapi-setup-linux-x86_64.deb"}


def repo(tmp_path: Path, files: dict[str, str]) -> list[str]:
    """Write files, check all the .md among them."""
    for name, text in files.items():
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return check(tmp_path, [n for n in files if n.endswith(".md")], ASSETS)


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

def test_inline_link_ok(tmp_path):
    assert repo(tmp_path, {"README.md": "[x](docs/a.md)\n", "docs/a.md": "# A\n"}) == []


def test_inline_link_broken_reports_path_line(tmp_path):
    out = repo(tmp_path, {"README.md": "ok\n\n[x](docs/missing.md)\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")
    assert "docs/missing.md" in out[0]


def test_link_relative_to_the_markdown_file(tmp_path):
    files = {"docs/a.md": "[up](../README.md) [sib](b.md)\n", "README.md": "# R\n", "docs/b.md": ""}
    assert repo(tmp_path, files) == []
    out = repo(tmp_path, {"docs/c.md": "[bad](docs/b.md)\n"})
    assert len(out) == 1 and out[0].startswith("docs/c.md:1: ")


def test_image_and_directory_target(tmp_path):
    (tmp_path / "img").mkdir()
    (tmp_path / "img" / "a.png").write_bytes(b"x")
    assert repo(tmp_path, {"README.md": "![alt](img/a.png) [dir](img/) [dir2](img)\n"}) == []
    assert len(repo(tmp_path, {"README.md": "![alt](img/b.png)\n"})) == 1


def test_reference_definition(tmp_path):
    assert repo(tmp_path, {"README.md": "[id]: docs/a.md\n", "docs/a.md": ""}) == []
    out = repo(tmp_path, {"README.md": "see [id]\n\n[id]: docs/none.md \"Title\"\n"})
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


def test_inline_html_src_and_href(tmp_path):
    assert repo(tmp_path, {"README.md": '<img src="a.png"> <a href=\'b.md\'>x</a>\n',
                           "a.png": "x", "b.md": ""}) == []
    out = repo(tmp_path, {"README.md": '<img src="nope.png"> <a href="nope.md">x</a>\n'})
    assert len(out) == 2


def test_title_angle_brackets_and_percent_20(tmp_path):
    files = {
        "README.md": '[a](docs/a.md "A title") [b](<docs/my file.md>) [c](docs/my%20file.md) '
                     "[d](docs/a.md 'single')\n",
        "docs/a.md": "",
        "docs/my file.md": "",
    }
    assert repo(tmp_path, files) == []


def test_urls_with_scheme_are_skipped(tmp_path):
    text = "[a](https://x.example/nope) [b](mailto:a@b.c) [c](http://x) [d](tel:1) [e](//cdn.x/y)\n"
    assert repo(tmp_path, {"README.md": text}) == []


def test_code_fence_and_code_span_ignored(tmp_path):
    text = (
        "```\n[x](nope.md)\n```\n\n"
        "~~~\n[y](nope2.md)\n~~~\n\n"
        "`[z](nope3.md)` and ``[w](nope4.md)``\n"
    )
    assert repo(tmp_path, {"README.md": text}) == []


def test_longer_fence_needs_matching_close(tmp_path):
    text = "````\n```\n[x](nope.md)\n```\n````\n\n[after](nope-after.md)\n"
    out = repo(tmp_path, {"README.md": text})
    assert len(out) == 1 and out[0].startswith("README.md:7: ")


def test_root_relative_link(tmp_path):
    assert repo(tmp_path, {"docs/a.md": "[r](/README.md)\n", "README.md": ""}) == []


def test_query_string_is_dropped(tmp_path):
    assert repo(tmp_path, {"README.md": "[r](docs/a.md?plain=1)\n", "docs/a.md": ""}) == []


# --- rule b: fragments ---------------------------------------------------------------------

def test_same_file_fragment(tmp_path):
    assert repo(tmp_path, {"README.md": "# Install\n\n[i](#install)\n"}) == []
    out = repo(tmp_path, {"README.md": "# Install\n\n[i](#instal)\n"})
    assert len(out) == 1 and "#instal" in out[0] and out[0].startswith("README.md:3: ")


def test_fragment_on_other_md_file(tmp_path):
    files = {"README.md": "[a](docs/a.md#the-one-rule)\n", "docs/a.md": "## The one rule\n"}
    assert repo(tmp_path, files) == []
    files["docs/a.md"] = "## Other\n"
    assert len(repo(tmp_path, files)) == 1


def test_fragment_with_em_dash_heading(tmp_path):
    files = {
        "README.md": "[d](docs/agent.md#dymo-labelwriter--pdf-not-raw)\n",
        "docs/agent.md": "### DYMO LabelWriter — `pdf`, not raw\n",
    }
    assert repo(tmp_path, files) == []


def test_fragment_duplicate_heading_suffix(tmp_path):
    files = {"README.md": "[a](#x-1)\n\n# X\n\n# X\n"}
    assert repo(tmp_path, files) == []


def test_fragment_is_case_insensitive_and_unquoted(tmp_path):
    assert repo(tmp_path, {"README.md": "# My Title\n\n[a](#My-Title) [b](#my%2Dtitle)\n"}) == []


def test_fragment_on_non_markdown_checks_file_only(tmp_path):
    files = {"README.md": "[a](src/main.rs#L10) [b](img/)\n[c](src/main.rs#whatever)\n",
             "src/main.rs": "fn main() {}\n"}
    (tmp_path / "img").mkdir()
    assert repo(tmp_path, files) == []
    assert len(repo(tmp_path, {"README.md": "[a](src/none.rs#L1)\n"})) == 1


def test_fragment_inside_fence_heading_does_not_count(tmp_path):
    out = repo(tmp_path, {"README.md": "```\n# Hidden\n```\n\n[a](#hidden)\n"})
    assert len(out) == 1


# --- rule c: maintainer-local files --------------------------------------------------------

def test_link_to_maintainer_local_file_is_a_finding_even_if_it_exists(tmp_path):
    files = {
        "README.md": "[h](HANDOFF.md) [t](TODO.md) [p](docs/superpowers/plan.md)\n",
        "HANDOFF.md": "# H\n",
        "TODO.md": "# T\n",
        "docs/superpowers/plan.md": "# P\n",
    }
    out = repo(tmp_path, files)
    assert len(out) == 3 and all("maintainer-local" in line for line in out)


def test_maintainer_local_via_relative_path(tmp_path):
    out = repo(tmp_path, {"docs/a.md": "[h](../HANDOFF.md#x)\n"})
    assert len(out) == 1 and "maintainer-local" in out[0]


def test_similarly_named_file_is_fine(tmp_path):
    assert repo(tmp_path, {"README.md": "[x](docs/TODO.md)\n", "docs/TODO.md": ""}) == []


# --- rule d: setup file names --------------------------------------------------------------

def test_known_setup_asset_ok(tmp_path):
    text = "Get printpapi-setup-windows-x86_64.exe, or `printpapi-setup-linux-x86_64.deb`.\n"
    assert repo(tmp_path, {"README.md": text}) == []


def test_unknown_setup_asset_found_with_line(tmp_path):
    out = repo(tmp_path, {"README.md": "x\n`printpapi-setup-windows-arm64.exe` here\n"})
    assert len(out) == 1 and out[0].startswith("README.md:2: ")
    assert "printpapi-setup-windows-arm64.exe" in out[0]


def test_setup_asset_in_url_and_fence_is_checked(tmp_path):
    text = "[d](https://x/releases/download/printpapi-setup-bogus.exe)\n```\nprintpapi-setup-bogus2.msi\n```\n"
    assert len(repo(tmp_path, {"README.md": text})) == 2


def test_setup_placeholder_and_trailing_dot(tmp_path):
    text = "Use `printpapi-setup-<platform>` or printpapi-setup-windows-x86_64.exe.\nThe printpapi-setup app.\n"
    assert repo(tmp_path, {"README.md": text}) == []


# --- rule e: open source -------------------------------------------------------------------

def test_open_source_claim_is_found(tmp_path):
    out = repo(tmp_path, {"README.md": "ok\nprintpapi is Open Source.\nan open-source tool\n"})
    assert len(out) == 2 and out[0].startswith("README.md:2: ") and out[1].startswith("README.md:3: ")


def test_open_source_with_not_or_never_is_fine(tmp_path):
    text = "It is **source available**, not OSI open source.\nNever call it \"open-source\".\n"
    assert repo(tmp_path, {"README.md": text}) == []


def test_not_must_be_a_word(tmp_path):
    assert len(repo(tmp_path, {"README.md": "We note this open source thing\n"})) == 1


# --- helpers -------------------------------------------------------------------------------

def test_crlf_line_endings(tmp_path):
    (tmp_path / "README.md").write_bytes(b"# T\r\n\r\n[a](#t) [b](nope.md)\r\n")
    out = check(tmp_path, ["README.md"], ASSETS)
    assert len(out) == 1 and out[0].startswith("README.md:3: ")


def test_setup_assets_from_json(tmp_path):
    (tmp_path / "web" / "lib").mkdir(parents=True)
    (tmp_path / "web" / "lib" / "setup-downloads.json").write_text(
        '[{"platform": "windows", "asset": "printpapi-setup-a.exe"}, {"asset": "printpapi-setup-b.dmg"}]'
    )
    assert check_docs.load_setup_assets(tmp_path) == {"printpapi-setup-a.exe", "printpapi-setup-b.dmg"}
    assert check_docs.load_setup_assets(tmp_path / "none") == set()
