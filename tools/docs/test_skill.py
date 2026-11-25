# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Staleness test of skills/printpapi/SKILL.md: every fact the skill states must still be true.

The skill is copied out of the repository by "npx skills add", so nothing in it may point at a
relative path, and nothing in it may drift from the server. Standard library only (the frontmatter
values are JSON strings, which are valid YAML, so json.loads reads them without a YAML parser).
"""
import json
import re
from pathlib import Path

import check_docs

ROOT = Path(__file__).resolve().parents[2]
SKILL = ROOT / "skills" / "printpapi" / "SKILL.md"
# The OpenAPI description the server compiles in (docs/api.md, "Machine-readable API").
OPENAPI = ROOT / "crates" / "server" / "openapi.json"
RAW = "https://raw.githubusercontent.com/LukiPrince/printpapi/main/"
INSTALL = "npx skills add LukiPrince/printpapi --skill printpapi"

VERB = r"(?:GET|POST|PUT|PATCH|DELETE)"
# No "." in the path class: "POST /orders." in a sentence yields "/orders".
ENDPOINT = re.compile(r"\b(" + VERB + r")\s+(/[A-Za-z0-9_\-/{}]*)")
MUST_NAME = {
    ("POST", "/jobs"), ("GET", "/jobs/{}"), ("GET", "/jobs"), ("DELETE", "/jobs/{}"),
    ("GET", "/printers"), ("GET", "/computers"),
    ("GET", "/labels"), ("POST", "/labels"), ("GET", "/labels/{}"), ("PUT", "/labels/{}"),
    ("DELETE", "/labels/{}"), ("POST", "/orders"), ("POST", "/apikeys"),
}
STATES = {"queued", "claimed", "done", "failed", "cancelled"}


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def skill_text() -> str:
    assert SKILL.is_file(), f"{SKILL} does not exist"
    return SKILL.read_text(encoding="utf-8").replace("\r\n", "\n")  # a Windows checkout may use CRLF


# --- helpers (tested on their own below, so a broken check cannot pass silently) ------------

def frontmatter(text: str) -> tuple[dict[str, str], list[str]]:
    """(values, problems). Each value must be a double-quoted string that json.loads reads."""
    lines = text.split("\n")
    if not lines or lines[0] != "---":
        return {}, ["the file must start with a --- line"]
    try:
        end = lines.index("---", 1)
    except ValueError:
        return {}, ["the frontmatter is not closed by a --- line"]
    values: dict[str, str] = {}
    problems: list[str] = []
    for line in lines[1:end]:
        key, sep, raw = line.partition(": ")
        if not sep or not re.fullmatch(r"[a-z]+", key):
            problems.append(f"not a key: value line: {line!r}")
            continue
        if len(raw) < 2 or not (raw.startswith('"') and raw.endswith('"')):
            problems.append(f"{key} must be a double-quoted string (a bare colon breaks the YAML)")
            continue
        try:
            value = json.loads(raw)
        except ValueError as e:
            problems.append(f"{key} is not a valid double-quoted string: {e}")
            continue
        if key in values:
            problems.append(f"{key} twice")
        values[key] = value
    return values, problems


def frontmatter_problems(text: str, folder: str = "printpapi") -> list[str]:
    values, problems = frontmatter(text)
    if problems:
        return problems
    if set(values) != {"name", "description", "license"}:
        problems.append(f"keys must be exactly name, description, license, not {sorted(values)}")
        return problems
    name, desc = values["name"], values["description"]
    if name != folder:
        problems.append(f"name {name!r} must equal its folder {folder!r}")
    if not re.fullmatch(r"[a-z0-9]+(-[a-z0-9]+)*", name) or len(name) > 64:
        problems.append("name must be kebab-case, at most 64 characters")
    if not desc or len(desc) > 1024:
        problems.append("description must be 1 to 1024 characters")
    if "<" in desc or ">" in desc:
        problems.append("description must not contain angle brackets")
    if not desc.startswith("Use this skill"):
        problems.append('description must start with "Use this skill"')
    if ": " in desc:
        problems.append("description must not contain a colon and a space")
    if not values["license"].strip():
        problems.append("license must not be empty")
    return problems


def normalise(path: str) -> str:
    path = re.sub(r"\{[^}]*\}", "{}", path)
    return path.rstrip("/") or "/"


def endpoints_named(text: str) -> set[tuple[str, str]]:
    return {(m.group(1), normalise(m.group(2))) for m in ENDPOINT.finditer(text)}


def openapi_operations(doc: dict) -> set[tuple[str, str]]:
    return {
        (method.upper(), normalise(path))
        for path, item in doc["paths"].items()
        for method in item
        if method.upper() in {"GET", "POST", "PUT", "PATCH", "DELETE"}
    }


def api_doc_operations(md: str) -> set[tuple[str, str]]:
    """Operations of the endpoint table of docs/api.md; a cell with GET, a pipe, POST is expanded."""
    ops: set[tuple[str, str]] = set()
    section = md.split("\n## Endpoints", 1)[1].split("\n## ", 1)[0]
    for row in section.splitlines():
        if not row.startswith("|"):
            continue
        # a literal \| inside a cell is the escape of a pipe, so split the row on the unescaped ones
        cells = re.split(r"(?<!\\)\|", row)
        first = cells[1] if len(cells) > 1 else ""
        for span in re.findall(r"`([^`]+)`", first):
            m = re.fullmatch(r"((?:GET|POST|PUT|PATCH|DELETE)(?:\\?\|(?:GET|POST|PUT|PATCH|DELETE))*)\s+(/[A-Za-z0-9_\-/{}]*)", span)
            if not m:
                continue
            for verb in re.split(r"\\?\|", m.group(1)):
                ops.add((verb, normalise(m.group(2))))
    return ops


def table_after(md: str, heading: str) -> list[str]:
    """The first table at or below the first line that starts with heading (header, separator, rows)."""
    lines = md.splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith(heading))
    rows: list[str] = []
    for line in lines[start:]:
        if line.startswith("|"):
            rows.append(line)
        elif rows:
            break
    return rows


def first_column_tokens(rows: list[str]) -> set[str]:
    """Backticked [a-z0-9_]+ tokens of the first column, header and separator skipped."""
    out: set[str] = set()
    for row in rows[2:]:
        first = re.split(r"(?<!\\)\|", row)[1]
        out.update(re.findall(r"`([a-z0-9_]+)`", first))
    return out


def fn_body(src: str, name: str) -> str:
    m = re.search(r"^pub fn " + name + r"\b.*?^}", src, re.S | re.M)
    assert m, f"fn {name} not found"
    return m.group(0)


def type_literals(body: str) -> set[str]:
    """The string literals of every Some("a" | "b") in a body."""
    out: set[str] = set()
    for m in re.finditer(r"Some\(\s*((?:\"[a-z0-9_]+\"\s*\|?\s*)+)\)", body):
        out.update(re.findall(r"\"([a-z0-9_]+)\"", m.group(1)))
    return out


def code_job_types() -> set[str]:
    src = read("crates/server/src/dispatch.rs")
    found: set[str] = set()
    for name in ("decode_payload", "parse_label_job", "parse_receipt_job"):
        found |= type_literals(fn_body(src, name))
    return found


def section(text: str, heading: str) -> str:
    """The body of a "## heading" section of the skill."""
    m = re.search(r"^## " + re.escape(heading) + r"\s*$(.*?)(?=^## |\Z)", text, re.S | re.M)
    assert m, f"no section ## {heading}"
    return m.group(1)


def error_codes(text: str) -> set[str]:
    return set(re.findall(r"^\|\s*(\d{3})\s*\|", section(text, "Errors"), re.M))


# --- 1. the file --------------------------------------------------------------------------

def test_skill_file_exists():
    assert SKILL.is_file()


# --- 2. frontmatter -----------------------------------------------------------------------

def test_frontmatter_follows_the_skills_rules():
    assert frontmatter_problems(skill_text()) == []


def test_frontmatter_unquoted_description_with_a_colon_fails():
    # Everything but the description is quoted, so the one problem can only be the description.
    head = '---\nname: "printpapi"\ndescription: '
    tail = '\nlicense: "x"\n---\n'
    bad = head + "Use this skill when: code prints" + tail
    assert frontmatter_problems(bad) == [
        "description must be a double-quoted string (a bare colon breaks the YAML)"
    ]
    # the twin with a quoted description (colon-free, as the rules ask) is clean
    assert frontmatter_problems(head + '"Use this skill when code prints"' + tail) == []
    # a quoted colon is valid YAML for the parser but the house rule still refuses it
    assert frontmatter_problems(head + '"Use this skill when: code prints"' + tail) == [
        "description must not contain a colon and a space"
    ]


def test_frontmatter_rejects_extra_keys_a_wrong_name_and_angle_brackets():
    good = '---\nname: "printpapi"\ndescription: "Use this skill to print"\nlicense: "L"\n---\n'
    assert frontmatter_problems(good) == []
    extra = good.replace('license: "L"\n', 'license: "L"\nversion: "1"\n')
    assert any("exactly" in p for p in frontmatter_problems(extra))
    assert any("folder" in p for p in frontmatter_problems(good.replace('"printpapi"', '"other"')))
    angled = good.replace("to print", "to print <b>")
    assert any("angle" in p for p in frontmatter_problems(angled))
    assert any("start with" in p for p in frontmatter_problems(good.replace("Use this", "This")))
    assert any("not closed" in p for p in frontmatter_problems("---\nname: \"x\"\n"))


# --- 3. length ----------------------------------------------------------------------------

def test_skill_stays_short():
    assert len(skill_text().splitlines()) <= 130


# --- 4 and 5. endpoints -------------------------------------------------------------------

def test_every_endpoint_it_names_exists_in_the_openapi_document():
    assert OPENAPI.is_file(), f"{OPENAPI} is missing: the skill is checked against it, never skipped"
    ops = openapi_operations(json.loads(OPENAPI.read_text(encoding="utf-8")))
    named = endpoints_named(skill_text())
    assert len(named) >= 10
    assert named <= ops, sorted(named - ops)
    assert MUST_NAME <= named, sorted(MUST_NAME - named)


def test_every_endpoint_it_names_is_in_the_api_doc_table():
    table = api_doc_operations(read("docs/api.md"))
    named = endpoints_named(skill_text())
    assert len(named) >= 10
    assert named <= table, sorted(named - table)


def test_endpoint_checks_catch_an_invented_route_and_a_removed_operation():
    ops = openapi_operations(json.loads(OPENAPI.read_text(encoding="utf-8")))
    table = api_doc_operations(read("docs/api.md"))
    invented = endpoints_named(skill_text() + "\nGET /nope\n")
    assert not invented <= ops and not invented <= table
    assert ("POST", "/orders") in endpoints_named("Use POST /orders. Then GET /jobs/{id}.")
    assert endpoints_named("GET /jobs/{job_id}/") == {("GET", "/jobs/{}")}
    assert not endpoints_named("PATCH /nope") <= ops  # PATCH is read as a verb, so an invented one fails
    without = {op for op in ops if op != ("POST", "/orders")}
    assert not endpoints_named(skill_text()) <= without
    assert {("GET", "/labels"), ("POST", "/labels")} <= api_doc_operations(
        "\n## Endpoints\n\n| a | b |\n|---|---|\n| " + r"`GET\|POST /labels`" + " | x |\n"
    )


# --- 6. job types -------------------------------------------------------------------------

def test_job_types_three_way_equal():
    skill = first_column_tokens(table_after(skill_text(), "| type |"))
    docs = first_column_tokens(table_after(read("docs/api.md"), "## Content types"))
    code = code_job_types()
    assert len(skill) >= 9 and len(docs) >= 9 and len(code) >= 9
    assert skill == docs == code, (sorted(skill ^ docs), sorted(skill ^ code))
    doc = json.loads(OPENAPI.read_text(encoding="utf-8"))
    enum: set[str] = set()
    for ref in doc["components"]["schemas"]["JobRequest"]["oneOf"]:
        schema = doc["components"]["schemas"][ref["$ref"].rsplit("/", 1)[1]]
        enum.update(schema["properties"]["type"]["enum"])
    assert enum == skill


# --- 7. numbers ---------------------------------------------------------------------------

def server_limits() -> dict[str, int]:
    src = read("crates/server/src/dispatch.rs")
    a, b, c = re.search(r"const MAX_EXPIRE_AFTER: i64 = (\d+) \* (\d+) \* (\d+);", src).groups()
    body = re.search(r"const MAX_BODY: usize = (\d+) \* 1024 \* 1024;", read("crates/server/src/http.rs"))
    return {
        "copies": int(re.search(r"const MAX_COPIES: i64 = (\d+);", src).group(1)),
        "expire": int(a) * int(b) * int(c),
        "key": int(re.search(r"const MAX_IDEMPOTENCY_KEY: usize = (\d+);", src).group(1)),
        "body": int(body.group(1)),
        "body_400": int(body.group(1)),
    }


def skill_limits(text: str) -> dict[str, list[int]]:
    """Each number read from its own phrase, so a number inside a longer one never matches."""
    def nums(pattern: str) -> list[int]:
        return [int(n) for n in re.findall(pattern, text)]

    return {
        "copies": nums(r"`copies` \(1-(\d+)\)"),
        "expire": nums(r"`expire_after` \(1-(\d+) seconds"),
        "key": nums(r"`idempotency_key` \(at most (\d+) characters\)"),
        "body": nums(r"request body is at most (\d+) MB\b"),
        "body_400": nums(r"body over (\d+) MB\b"),
    }


def test_numbers_match_the_server_constants():
    server = server_limits()
    assert skill_limits(skill_text()) == {k: [v] for k, v in server.items()}
    # the CloudPRNT numbers come from docs/cloudprnt.md
    cp = read("docs/cloudprnt.md")
    text = skill_text()
    for fact in ("512 KB", "2 MB", "521"):
        assert fact in text and fact in cp, fact


def test_number_check_catches_drift_in_either_side():
    text = skill_text()
    server = {k: [v] for k, v in server_limits().items()}
    assert skill_limits(text) == server
    # a number that is only a part of the right one, or the right one inside a longer one
    for old, new in [
        ("(1-100)", "(1-10)"), ("(1-100)", "(1-1000)"),
        ("(1-2592000 seconds", "(1-259200 seconds"), ("(1-2592000 seconds", "(1-25920000 seconds"),
        ("at most 128 characters", "at most 512 characters"),
        ("at most 128 characters", "at most 100 characters"),
        ("request body is at most 32 MB", "request body is at most 2 MB"),
        ("request body is at most 32 MB", "request body is at most 132 MB"),
        ("body over 32 MB", "body over 2 MB"),
    ]:
        assert old in text, old
        assert skill_limits(text.replace(old, new)) != server, (old, new)
    # the same server value changed: the comparison sees it
    assert skill_limits(text) != {k: [v + 1] for k, v in server_limits().items()}


# --- 8. job states ------------------------------------------------------------------------

def states_listed(text: str) -> set[str]:
    """The backticked words of the "`state` is ..." sentence of the section "Did it print"."""
    m = re.search(r"`state` is ([^.]*)\.", section(text, "Did it print"))
    assert m, 'no "`state` is ..." sentence'
    return set(re.findall(r"`([a-z]+)`", m.group(1)))


def test_job_states_it_lists_exist():
    text, api = skill_text(), read("docs/api.md")
    assert states_listed(text) == STATES
    doc = json.loads(OPENAPI.read_text(encoding="utf-8"))
    assert set(doc["components"]["schemas"]["JobState"]["enum"]) == STATES
    for state in STATES:
        assert re.search(rf"\b{state}\b", api), state


def test_state_check_catches_a_changed_list():
    text = skill_text()
    assert "`claimed`" in section(text, "Did it print")
    assert states_listed(text.replace("`claimed`", "`taken`", 1)) != STATES
    assert states_listed(text.replace("`failed` or `cancelled`", "`failed`, `cancelled` or `paused`", 1)) != STATES


# --- 9. links -----------------------------------------------------------------------------

def test_links_resolve_to_files_in_the_repo():
    text = skill_text()
    body = text.split("\n---\n", 1)[1]
    targets = re.findall(r"\]\(([^)\s]+)\)", body)
    assert targets
    for t in targets:
        assert t.startswith(RAW), f"only absolute raw links survive a copy of the file: {t}"
        assert "#" not in t, f"no anchors: {t}"
        assert (ROOT / t[len(RAW):]).is_file(), t
    for raw in re.findall(re.escape(RAW) + r"[^\s)>\"]+", text):
        assert (ROOT / raw[len(RAW):]).is_file(), raw


# --- 10. the label rule -------------------------------------------------------------------

def test_label_rule_is_stated():
    text = skill_text()
    for word in ("can_pdf", "%PDF", "label_*", "cloudprnt-", "raw_*"):
        assert word in text, word
    assert "can_pdf" in read("crates/server/src/store.rs")
    assert '"cloudprnt-"' in read("crates/server/src/cloudprnt.rs")


# --- 11. errors ---------------------------------------------------------------------------

def test_error_codes_are_documented_and_no_429():
    text = skill_text()
    codes = error_codes(text)
    assert {"400", "401", "402", "403", "404", "409", "502", "503"} <= codes
    documented = read("docs/api.md") + read("docs/labels.md")
    for code in codes:
        assert re.search(rf"\b{code}\b", documented), code
    assert "429" not in text


# --- 12. pointers -------------------------------------------------------------------------

def test_pointers_exist():
    readme = read("README.md")
    assert "skills/printpapi/SKILL.md" in readme
    assert INSTALL in readme
    assert RAW + "skills/printpapi/SKILL.md" in read("web/demo/static/llms.txt")
    assert "skills/printpapi/SKILL.md" in read("docs/recipes.md")
    assert "tools/docs/test_skill.py" in read("AGENTS.md")


# --- 12b. the rules that lose prints or hide a failure ----------------------------------------

def test_rules_that_cost_prints_are_stated():
    text = skill_text()
    # a receipt on a ZPL printer is accepted, sent as ESC/POS and reported done
    assert "never on a ZPL" in section(text, "Receipts")
    assert "never `receipt`" in section(text, "Which payload for which printer")
    # the key space is the whole org's, a print key may not use shopify-, a 404 after a 200 is a clue
    retry = section(text, "Retry-safe submits")
    for word in ("whole org", "prefix", "shopify-", "404"):
        assert word in retry, word
    # a callback is a hint: best effort, unsigned, poll as the fallback
    done = section(text, "Did it print")
    for word in ("best effort", "unsigned", "Keep polling `GET /jobs/{id}`", "operator token"):
        assert word in done, word
    # an older server ignores the scope: the answer must say it
    assert '"scope": "print"' in section(text, "Auth")
    # the body goes through stdin: a big label as one argument fails with "Argument list too long"
    submit = section(text, "Submit a job")
    assert "--data-binary @-" in submit and 'printf' in submit
    assert '-d "{' not in submit
    assert "closed connection" in section(text, "Errors")
    assert "receipt" in section(text, "Common mistakes")


def test_recipes_give_one_key_per_document():
    recipes = read("docs/recipes.md")
    assert "stable key (the order id)" not in recipes
    assert '"order-{{ $json.id }}"' not in recipes and '"order-{{order_id}}"' not in recipes


def test_roadmap_names_the_skill():
    assert "skills/printpapi/SKILL.md" in read("docs/roadmap.md")


# --- 13. wording --------------------------------------------------------------------------

def test_wording():
    text = skill_text()
    assert check_docs.wording_findings(text) == []
    assert "source-available" in text
    assert check_docs.wording_findings("It is open source.") != []
