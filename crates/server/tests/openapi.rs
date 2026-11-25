// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The OpenAPI document of the public API (`GET /openapi.json`): its structure, that every documented
//! route exists, that every route of the router is documented (a closed list, not a guess), and
//! that the served copy is the file. No mocks: the real router over an in-memory database.
mod common;

use common::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const DOC: &str = include_str!("../openapi.json");
const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

fn doc() -> Value {
    serde_json::from_str(DOC).expect("openapi.json is JSON")
}

/// Every (METHOD, path) the document describes, with its operation object.
fn operations(d: &Value) -> Vec<(String, String, &Value)> {
    let mut out = Vec::new();
    for (path, item) in d["paths"].as_object().expect("paths") {
        for (m, op) in item.as_object().expect("a path item") {
            assert!(METHODS.contains(&m.as_str()), "{path}: unexpected key {m}");
            out.push((m.to_uppercase(), path.clone(), op));
        }
    }
    out
}

/// Follows a local `$ref` (`#/components/...`), else returns the value itself.
fn resolve<'a>(d: &'a Value, v: &'a Value) -> &'a Value {
    match v.get("$ref").and_then(Value::as_str) {
        Some(r) => d
            .pointer(r.strip_prefix('#').expect("a local ref"))
            .unwrap_or_else(|| panic!("{r} does not resolve")),
        None => v,
    }
}

fn walk<'a>(v: &'a Value, f: &mut impl FnMut(&'a Value)) {
    f(v);
    match v {
        Value::Object(m) => m.values().for_each(|c| walk(c, f)),
        Value::Array(a) => a.iter().for_each(|c| walk(c, f)),
        _ => {}
    }
}

fn path_params(path: &str) -> BTreeSet<String> {
    path.split('/')
        .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
        .map(String::from)
        .collect()
}

// --- structure -----------------------------------------------------------------------------------

#[test]
fn the_document_is_a_well_formed_openapi_3_1_description() {
    let d = doc();
    assert_eq!(d["openapi"], "3.1.0");
    for k in ["title", "version"] {
        assert!(
            d["info"][k].as_str().is_some_and(|s| !s.is_empty()),
            "info.{k}"
        );
    }
    // the version of the description itself, not the server's: the unauthenticated document
    // discloses no patch level
    let v = d["info"]["version"].as_str().unwrap();
    assert!(
        v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u32>().is_ok()),
        "info.version is x.y.z, got {v}"
    );
    assert!(
        !DOC.contains(env!("CARGO_PKG_VERSION")),
        "the server version must not appear in the document"
    );
    let declared: BTreeSet<_> = d["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let mut ids = BTreeSet::new();
    for (m, path, op) in operations(&d) {
        let id = op["operationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{m} {path}: operationId"));
        assert!(ids.insert(id.to_string()), "duplicate operationId {id}");
        for t in op["tags"].as_array().expect("tags") {
            assert!(
                declared.contains(t.as_str().unwrap()),
                "{m} {path}: tag {t}"
            );
        }
        let responses = op["responses"].as_object().expect("responses");
        assert!(!responses.is_empty(), "{m} {path}: no responses");
        for (status, r) in responses {
            let r = resolve(&d, r);
            assert!(
                r["description"].as_str().is_some_and(|s| !s.is_empty()),
                "{m} {path} {status}: a response needs a description"
            );
        }
        // path parameters, both ways
        let mut declared_path = BTreeSet::new();
        for p in op
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let p = resolve(&d, p);
            let place = p["in"]
                .as_str()
                .unwrap_or_else(|| panic!("{m} {path}: parameter without in"));
            assert!(
                ["path", "query", "header"].contains(&place),
                "{m} {path}: in {place}"
            );
            if place == "path" {
                assert_eq!(
                    p["required"], true,
                    "{m} {path}: a path parameter is required"
                );
                declared_path.insert(p["name"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(
            declared_path,
            path_params(&path),
            "{m} {path}: path parameters"
        );
        // roles and rights are what the router enforces (docs/api.md, Key rights)
        let roles = op["x-roles"].as_array().filter(|r| !r.is_empty());
        for r in roles.unwrap_or_else(|| panic!("{m} {path}: x-roles")) {
            assert!(
                [
                    "none",
                    "any",
                    "client",
                    "manage",
                    "root",
                    "session",
                    "agent",
                    "code",
                    "hmac",
                    "cloudprnt",
                    "cloudprnt-key"
                ]
                .contains(&r.as_str().unwrap()),
                "{m} {path}: role {r}"
            );
        }
        // /agent/* stays 401 for a missing right (the setup app reads 403 there as "a proxy
        // refused", docs/api.md, Key rights): no agent operation documents a 403
        if op["tags"][0] == "agent" {
            assert!(
                op["responses"].get("403").is_none(),
                "{m} {path}: /agent/* never answers 403"
            );
        }
        for r in op["x-rights"].as_array().expect("x-rights") {
            assert!(
                ["Computers", "Look", "Submit", "Edit", "Agent", "CloudPrnt"]
                    .contains(&r.as_str().unwrap()),
                "{m} {path}: right {r}"
            );
        }
    }
}

#[test]
fn every_ref_resolves_and_every_security_name_is_declared() {
    let d = doc();
    let mut refs = Vec::new();
    walk(&d, &mut |v| {
        if let Some(r) = v.get("$ref").and_then(Value::as_str) {
            refs.push(r.to_string());
        }
    });
    assert!(refs.len() > 50);
    for r in refs {
        assert!(d.pointer(r.strip_prefix('#').unwrap()).is_some(), "{r}");
    }
    let schemes = d["components"]["securitySchemes"].as_object().unwrap();
    for (m, path, op) in operations(&d) {
        let sec = op["security"]
            .as_array()
            .unwrap_or_else(|| panic!("{m} {path}: security"));
        for req in sec {
            for name in req.as_object().unwrap().keys() {
                assert!(schemes.contains_key(name), "{m} {path}: scheme {name}");
            }
        }
    }
}

/// A response object is closed: a field the server starts to send fails the contract test until it
/// is documented here.
#[test]
fn response_objects_are_closed() {
    let d = doc();
    fn visit(d: &Value, s: &Value, seen: &mut BTreeSet<String>, at: &str) {
        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            if seen.insert(r.to_string()) {
                visit(d, resolve(d, s), seen, r);
            }
            return;
        }
        if s.get("properties").is_some() {
            assert_eq!(
                s["additionalProperties"], false,
                "{at}: a response object sets additionalProperties: false"
            );
        }
        for k in ["properties"] {
            for c in s
                .get(k)
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|m| m.values())
            {
                visit(d, c, seen, at);
            }
        }
        for k in ["items", "additionalProperties"] {
            if s.get(k).is_some_and(Value::is_object) {
                visit(d, &s[k], seen, at);
            }
        }
        for c in s
            .get("oneOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            visit(d, c, seen, at);
        }
    }
    let mut seen = BTreeSet::new();
    for (m, path, op) in operations(&d) {
        for (status, r) in op["responses"].as_object().unwrap() {
            for (_, media) in resolve(&d, r)["content"].as_object().into_iter().flatten() {
                visit(
                    &d,
                    &media["schema"],
                    &mut seen,
                    &format!("{m} {path} {status}"),
                );
            }
        }
    }
    assert!(seen.len() > 20);
}

#[test]
fn a_job_request_is_one_of_ten_types() {
    let d = doc();
    let one_of = d["components"]["schemas"]["JobRequest"]["oneOf"]
        .as_array()
        .unwrap();
    let types: BTreeSet<String> = one_of
        .iter()
        .map(|r| {
            resolve(&d, r)["properties"]["type"]["enum"][0]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let want = [
        "raw_base64",
        "pdf_base64",
        "raw_uri",
        "pdf_uri",
        "raw_uri_post",
        "pdf_uri_post",
        "label_uri",
        "label_base64",
        "label_text",
        "receipt",
    ];
    assert_eq!(types, want.iter().map(|s| s.to_string()).collect());
    // raw jobs never take options; every other type may (a ZPL label printer answers 400)
    for r in one_of {
        let s = resolve(&d, r);
        let t = s["properties"]["type"]["enum"][0].as_str().unwrap();
        assert_eq!(
            s["properties"].get("options").is_some(),
            !t.starts_with("raw_"),
            "{t}"
        );
    }
}

// roles and scopes: what a reader of the document is told about who may call an operation

/// Every scope a manager can issue, by its word.
fn scopes() -> Vec<(&'static str, printpapi_server::auth::Scope)> {
    use printpapi_server::auth::Scope::*;
    vec![
        ("full", Full),
        ("agent", Agent),
        ("cloudprnt", CloudPrnt),
        ("print", Print),
        ("read", Read),
    ]
}

fn right_named(name: &str) -> printpapi_server::auth::Right {
    use printpapi_server::auth::Right::*;
    match name {
        "Computers" => Computers,
        "Look" => Look,
        "Submit" => Submit,
        "Edit" => Edit,
        "Agent" => Agent,
        "CloudPrnt" => CloudPrnt,
        other => panic!("right {other}"),
    }
}

/// The scope words of the keys that may call an operation, from `Scope::allows`: the rights of a
/// key route (client, agent, CloudPRNT), every scope for `any`, none for what no key may call
/// (root, a session, a pairing code, a signed webhook).
fn key_scopes(op: &Value) -> Vec<&'static str> {
    let roles: Vec<&str> = op["x-roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    let rights: Vec<_> = op["x-rights"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| right_named(r.as_str().unwrap()))
        .collect();
    if roles.contains(&"any") {
        return scopes().iter().map(|(w, _)| *w).collect();
    }
    let by_right = ["client", "agent", "cloudprnt", "cloudprnt-key"]
        .iter()
        .any(|r| roles.contains(r));
    if rights.is_empty() || !by_right {
        return Vec::new();
    }
    scopes()
        .into_iter()
        .filter(|(_, s)| rights.iter().all(|r| s.allows(*r)))
        .map(|(w, _)| w)
        .collect()
}

/// The first sentence of every description: who may call it.
fn who_may_call(op: &Value) -> String {
    let scopes = key_scopes(op).join(", ");
    let rights: Vec<&str> = op["x-rights"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    let key = format!("Key right {} (scopes: {scopes})", rights.join(" and "));
    match op["x-roles"][0].as_str().unwrap() {
        "none" => "No credential needed.".into(),
        "any" => "Any valid credential: the root token, a session or a key of any scope.".into(),
        "client" => format!("{key}, or the root token or a session."),
        "manage" => {
            "The root token or a session only; no API key can call it, not even a full one.".into()
        }
        "root" => "The root token only.".into(),
        "session" => "A session only.".into(),
        // register: the root token or a key an agent already holds enrolls too (server.rs enrollment_org);
        // the other four calls take the key an agent registered with, a never-issued one such as the root token
        "agent" => format!("{key}, the root token, or a key an agent has registered with."),
        "code" => "A pairing code in the body; no key.".into(),
        "hmac" => "A signed webhook call: the HMAC signature is the credential.".into(),
        "cloudprnt" => format!("{key}: the client key is the Basic user name."),
        "cloudprnt-key" => format!("{key}: the client key is in the path."),
        other => panic!("role {other}"),
    }
}

#[test]
fn every_operation_says_who_may_call_it_in_words_and_in_security() {
    let d = doc();
    for (m, path, op) in operations(&d) {
        let text = op["description"].as_str().unwrap_or_default();
        let want = who_may_call(op);
        assert!(
            text.starts_with(&want),
            "{m} {path}: the description starts with \"{want}\", got \"{text}\""
        );
        // the scope words of the keys that may call it follow the rights table (Scope::allows)
        let scopes = key_scopes(op);
        for req in op["security"].as_array().unwrap() {
            for (name, listed) in req.as_object().unwrap() {
                let listed: Vec<&str> = listed
                    .as_array()
                    .unwrap_or_else(|| panic!("{m} {path}: {name} lists scopes"))
                    .iter()
                    .map(|s| s.as_str().unwrap())
                    .collect();
                assert_eq!(listed, scopes, "{m} {path}: security {name}");
            }
        }
    }
}

/// The document says who may call each operation: the root token and a key of every scope are held
/// against it on the real routes, and what answers 401 or 403 is exactly what the document leaves out.
/// (The sentence and the security list are derived from the document's own x-roles and x-rights, so the
/// test above only compares the document with itself; this one compares it with the server.)
#[test]
fn who_may_call_an_operation_is_what_the_server_lets_in() {
    use printpapi_server::auth::Scope;
    use printpapi_server::store::{self, DEFAULT_ORG};
    use serde_json::json;
    let d = doc();
    let mut checked = 0;
    for (m, path, op) in operations(&d) {
        let roles: Vec<&str> = op["x-roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        // only what a bearer credential reaches: no sign-in, no signed webhook, no Basic user name
        let bearer = op["security"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_object().unwrap().contains_key("bearerAuth"));
        if !bearer {
            continue;
        }
        let app = App::new();
        let root = "t"; // the root token of App::new
        let mut creds: Vec<(String, String, bool)> = vec![(
            "the root token".into(),
            root.into(),
            roles
                .iter()
                .any(|r| ["any", "client", "root", "manage", "agent"].contains(r)),
        )];
        let allowed = key_scopes(op);
        for (word, scope) in [
            ("full", Scope::Full),
            ("agent", Scope::Agent),
            ("cloudprnt", Scope::CloudPrnt),
            ("print", Scope::Print),
            ("read", Scope::Read),
        ] {
            let key = format!("k-{word}");
            app.db
                .with(|c| store::add_api_key_scoped(c, word, &key, DEFAULT_ORG, scope))
                .unwrap();
            creds.push((format!("a {word} key"), key, allowed.contains(&word)));
        }
        let url = path.replace("{id}", "999999").replace("{key}", "k");
        for (who, key, may) in creds {
            // an agent that has enrolled with this key, if the key may enroll: claim, payload and result
            // are for the key of a registered agent
            let name = format!("pc-{}", key.replace(['-', ' '], ""));
            let body = json!({"name": name, "printers": [{"name": "Z"}]});
            let _ = app
                .call("POST", "/agent/register")
                .bearer(&key)
                .json(&body)
                .send();
            let r = app.call(&m, &url).bearer(&key).json(&body).send();
            let let_in = r.status != 401 && r.status != 403;
            assert_eq!(
                let_in,
                may,
                "{m} {path} with {who}: the server answered {} ({}), the document says {}",
                r.status,
                r.text(),
                who_may_call(op)
            );
            checked += 1;
        }
    }
    assert!(checked >= 6 * 25, "{checked}");
}

// --- A: every documented route exists --------------------------------------------------------------

fn concrete(path: &str) -> String {
    path.replace("{id}", "1").replace("{key}", "k")
}

#[test]
fn every_documented_route_answers_a_request_without_credentials() {
    let d = doc();
    let app = App::new();
    let mut probed = 0;
    for (m, path, op) in operations(&d) {
        let r = app.call(&m, &concrete(&path)).send();
        let status = r.status.to_string();
        let declared = op["responses"].as_object().unwrap();
        assert!(
            declared.contains_key(&status),
            "{m} {path}: answered {status}, documented {:?}\n{}",
            declared.keys().collect::<Vec<_>>(),
            r.text()
        );
        let secured = !op["security"].as_array().unwrap().is_empty();
        if secured {
            // a router miss would be 404 and a missing gate would be anything but 401
            assert_eq!(
                r.status, 401,
                "{m} {path}: a secured route answers 401 without a credential"
            );
        }
        probed += 1;
        // the same with a body that is no JSON: a route that reads it before it checks a credential
        // (login, signup, password reset, pairing) answers 400, and that is documented too
        if op.get("requestBody").is_some() {
            let r = app.call(&m, &concrete(&path)).raw(b"not json").send();
            assert!(
                declared.contains_key(&r.status.to_string()),
                "{m} {path}: a body that is no JSON answered {}, documented {:?}\n{}",
                r.status,
                declared.keys().collect::<Vec<_>>(),
                r.text()
            );
        }
    }
    assert!(probed >= 55, "{probed}");
}

#[test]
fn the_webhooks_answer_with_their_documented_statuses() {
    let d = doc();
    let app = App::new();
    // billing is not configured in the default App: 503 before the signature is read
    let r = app.call("POST", "/billing/webhook").raw(b"{}").send();
    assert_eq!(r.status, 503);
    assert!(d["paths"]["/billing/webhook"]["post"]["responses"]["503"].is_object());
    for s in ["400", "401", "404"] {
        assert!(
            d["paths"]["/billing/webhook"]["post"]["responses"][s].is_object(),
            "{s}"
        );
    }
    // Shopify: no key in the URL is a 401, before the signature
    let r = app
        .call("POST", "/integrations/shopify/orders")
        .raw(b"{}")
        .send();
    assert_eq!(r.status, 401);
}

// --- B: every route of the router is documented ------------------------------------------------------

/// The text of a source file without its test module.
fn non_test(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap()
}

/// The body of `    fn name(&self) -> R {` up to its closing `    }` (four spaces).
fn method_body<'a>(src: &'a str, name: &str) -> &'a str {
    let head = format!("\n    fn {name}(&self) -> R {{\n");
    let start = src
        .find(&head)
        .unwrap_or_else(|| panic!("fn {name} not found"))
        + head.len();
    let end = src[start..]
        .find("\n    }\n")
        .expect("the end of the function");
    &src[start..start + end]
}

/// How many parentheses a line leaves open, outside string and char literals.
fn open_parens(s: &str) -> i32 {
    let (mut n, mut in_str) = (0, false);
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' if in_str => i += 1,
            '"' => in_str = !in_str,
            '\'' if !in_str && c.get(i + 2) == Some(&'\'') => i += 2,
            '(' if !in_str => n += 1,
            ')' if !in_str => n -= 1,
            _ => {}
        }
        i += 1;
    }
    n
}

/// The statements of a dispatcher body: comments are dropped, a line that starts with `|` joins the
/// one before (a wrapped or-pattern), and a line with an open parenthesis runs on to the closing one
/// (rustfmt wraps a long `id_of(` call over several lines).
fn logical_lines(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in body.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with("//") {
            continue;
        }
        match out.last_mut() {
            Some(last) if open_parens(last) > 0 || l.starts_with('|') => {
                if !last.ends_with('(') {
                    last.push(' ');
                }
                last.push_str(l);
            }
            _ => out.push(l.to_string()),
        }
    }
    out
}

/// True when `word` stands in `line` outside string literals as a whole identifier (not after `.`, `::`
/// or inside a longer name).
fn has_word(line: &str, word: char) -> bool {
    let c: Vec<char> = line.chars().collect();
    let ident = |x: char| x.is_alphanumeric() || x == '_';
    let mut in_str = false;
    for i in 0..c.len() {
        match c[i] {
            '"' if i == 0 || c[i - 1] != '\\' => in_str = !in_str,
            x if x == word && !in_str => {
                let before = i.checked_sub(1).map(|j| c[j]);
                let after = c.get(i + 1).copied();
                if !before.is_some_and(|b| ident(b) || b == '.' || b == ':' || b == '\'')
                    && !after.is_some_and(|a| ident(a) || a == '\'')
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The statements of a dispatcher that use its request target `t` in a form `routes_in` does not read:
/// `let t = self.target();`, `match t {`, `id_of(t, ..` and `t == "..` are read, everything else (a call
/// of a helper dispatcher with `t`, `matches!(t, ..)`, `t.split('/')`) is not.
fn unread_uses_of_t(body: &str) -> Vec<String> {
    logical_lines(body)
        .into_iter()
        .filter(|l| {
            let rest = strip_id_of(
                &l.replace("let t = self.target();", "")
                    .replace("match t {", "")
                    .replace("t == \"", "\""),
            );
            has_word(&rest, 't')
        })
        .collect()
}

const ID_OF: &str = "id_of(t, ";

/// `("prefix", "suffix")` of an `id_of(t, "prefix", "suffix")` call whose text after `id_of(t, ` is `rest`,
/// and how many bytes the arguments and the closing parenthesis take. `None` for any other argument (a
/// constant, a variable): the route it reads is not visible.
fn id_of_args(rest: &str) -> Option<(String, String, usize)> {
    fn literal(s: &str) -> Option<(String, usize)> {
        let s0 = s.strip_prefix('"')?;
        let end = s0.find('"')?;
        (!s0[..end].contains('\\')).then(|| (s0[..end].to_string(), end + 2))
    }
    let (p, n1) = literal(rest)?;
    let after = rest[n1..].trim_start().strip_prefix(',')?.trim_start();
    let (s, n2) = literal(after)?;
    let tail = after[n2..].trim_start();
    let tail = tail.strip_prefix(',').map_or(tail, str::trim_start);
    let tail = tail.strip_prefix(')')?;
    Some((p, s, rest.len() - tail.len()))
}

/// `line` without its well-formed `id_of(t, "..", "..")` calls.
fn strip_id_of(line: &str) -> String {
    let (mut out, mut rest) = (String::new(), line);
    while let Some(i) = rest.find(ID_OF) {
        let args = &rest[i + ID_OF.len()..];
        match id_of_args(args) {
            Some((_, _, used)) => {
                out.push_str(&rest[..i]);
                rest = &args[used..];
            }
            None => {
                out.push_str(&rest[..i + ID_OF.len()]);
                rest = args;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The patterns of the arms of every `match t {` in a dispatcher body, also one nested in an arm.
fn arm_patterns(body: &str) -> Vec<String> {
    let text = without_comments(body);
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[char_to_byte(&text, from)..].find("match t {") {
        let start =
            from + text[char_to_byte(&text, from)..][..at].chars().count() + "match t {".len();
        let mut i = start;
        loop {
            while c.get(i).is_some_and(|x| x.is_whitespace()) {
                i += 1;
            }
            if i >= c.len() || c[i] == '}' {
                break;
            }
            let pat_start = i;
            while i < c.len() && !(c[i] == '=' && c.get(i + 1) == Some(&'>')) {
                i = skip_token(&c, i);
            }
            let pattern: String = c[pat_start..i.min(c.len())].iter().collect();
            out.push(pattern.split_whitespace().collect::<Vec<_>>().join(" "));
            i += 2;
            while c.get(i).is_some_and(|x| x.is_whitespace()) {
                i += 1;
            }
            if c.get(i) == Some(&'{') {
                i = skip_token(&c, i);
                if c.get(i) == Some(&',') {
                    i += 1;
                }
            } else {
                while i < c.len() && c[i] != ',' && c[i] != '}' {
                    i = skip_token(&c, i);
                }
                if c.get(i) == Some(&',') {
                    i += 1;
                }
            }
        }
        // on from just after the opening, so a `match t` inside an arm is walked as well
        from = start;
    }
    out
}

/// The arms of `match t` that are not string literals joined by `|`, or `_`: a constant, a binding (which
/// hands the target to a helper), a guard (rustfmt may put it on the next line). The route scan reads
/// literal arms only, so nothing else may route.
fn opaque_arms(body: &str) -> Vec<String> {
    arm_patterns(body)
        .into_iter()
        .filter(|p| !literal_pattern(p))
        .collect()
}

fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(b, _)| b)
}

/// `_`, or string literals joined by `|`.
fn literal_pattern(p: &str) -> bool {
    let mut alts = Vec::new();
    let (mut in_str, mut esc, mut cur) = (false, false, String::new());
    for ch in p.chars() {
        match ch {
            _ if esc => {
                esc = false;
                cur.push(ch);
            }
            '\\' if in_str => {
                esc = true;
                cur.push(ch);
            }
            '"' => {
                in_str = !in_str;
                cur.push(ch);
            }
            '|' if !in_str => alts.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    alts.push(cur);
    alts.iter().all(|a| {
        let a = a.trim();
        a == "_"
            || (a.len() >= 2
                && a.starts_with('"')
                && a.ends_with('"')
                && !a[1..a.len() - 1].contains(['"', '\\']))
    })
}

/// The text without `//` comments (outside string literals).
fn without_comments(s: &str) -> String {
    let mut out = String::new();
    for line in s.lines() {
        let (mut in_str, mut esc) = (false, false);
        let mut cut = line.len();
        let b: Vec<(usize, char)> = line.char_indices().collect();
        for (k, &(pos, ch)) in b.iter().enumerate() {
            if esc {
                esc = false;
            } else if in_str && ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = !in_str;
            } else if !in_str && ch == '/' && b.get(k + 1).is_some_and(|n| n.1 == '/') {
                cut = pos;
                break;
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The index after the token at `i`: a string or char literal, a bracketed group with everything in it,
/// or one character.
fn skip_token(c: &[char], i: usize) -> usize {
    match c[i] {
        '"' => {
            let mut j = i + 1;
            while j < c.len() && c[j] != '"' {
                j += if c[j] == '\\' { 2 } else { 1 };
            }
            j + 1
        }
        '\'' if c.get(i + 2) == Some(&'\'') => i + 3,
        '(' | '[' | '{' => {
            let mut j = i + 1;
            while j < c.len() && !matches!(c[j], ')' | ']' | '}') {
                j = skip_token(c, j);
            }
            j + 1
        }
        _ => i + 1,
    }
}

/// The routes one dispatcher body names: `"/x" =>` arms (also several, joined by `|`), `t == "/x"`,
/// `path() == "/x"`, `id_of(t, "/p/", "/s")` (a number, written `{id}`) and `cloudprnt_route(`.
/// ponytail: a path built by `format!` is invisible here, and so is any comparison that does not name
/// `t` (a helper that reads `self.target()` itself is a pinned path-reading line, see PINNED); the
/// dispatchers may use `t` only as `t == "/x"`, `id_of(t, "/p/", "/s")` and `match t` with literal arms
/// (`unread_uses_of_t`, `opaque_arms`). Test A still covers every documented route.
fn routes_in(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in logical_lines(body) {
        let l = l.as_str();
        if l.starts_with('"') && l.contains("=>") {
            let pat = l.split("=>").next().unwrap();
            out.extend(literals(pat).into_iter().filter(|s| s.starts_with('/')));
        }
        // every use in a statement, not the first only: `t == "/a" || t == "/b"`, two id_of calls
        for marker in ["t == \"", "path() == \"", "path() != \"", "target() == \""] {
            for (i, _) in l.match_indices(marker) {
                out.extend(
                    literals(&l[i..])
                        .into_iter()
                        .take(1)
                        .filter(|s| s.starts_with('/')),
                );
            }
        }
        for (i, _) in l.match_indices(ID_OF) {
            // a call with anything but two literals is not read here: unread_uses_of_t fails it
            if let Some((p, s, _)) = id_of_args(&l[i + ID_OF.len()..]) {
                out.insert(format!("{p}{{id}}{s}"));
            }
        }
        if l.contains("cloudprnt_route(") && !l.starts_with("fn ") {
            out.insert("/cloudprnt".into());
            out.insert("/cloudprnt/{key}".into());
        }
    }
    out
}

/// The double-quoted literals of a line, in order.
fn literals(s: &str) -> Vec<String> {
    s.split('"').skip(1).step_by(2).map(String::from).collect()
}

/// Routes the router answers that the document does not describe, with the reason.
const EXCLUDED: [(&str, &str, &str); 2] = [
    ("GET", "/", "the dashboard bundle"),
    ("GET", "/index.html", "the dashboard bundle"),
];

fn router_routes(src: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (m, f) in [
        ("GET", "get"),
        ("POST", "post"),
        ("PUT", "put"),
        ("DELETE", "delete"),
    ] {
        for p in routes_in(method_body(src, f)) {
            out.insert((m.to_string(), p));
        }
    }
    out
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/").to_string() + rel)
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn the_scanner_reads_arms_comparisons_and_id_helpers() {
    let src = "\nimpl X {\n    fn get(&self) -> R {\n        let t = self.target();\n        match t {\n            \"/\" | \"/index.html\" => return a(),\n            \"/health\" => {\n            }\n            _ => {}\n        }\n        if self.req.path() == \"/support-access\" {\n        }\n        if let Some(id) = id_of(t, \"/jobs/\", \"/preview\") {\n        }\n        if let Some(key) = cloudprnt_route(self.req.path()) {\n        }\n        // \"/commented\" => x\n        if t == \"/agent/jobs\" {\n        }\n    }\n\n    fn post(&self) -> R {\n    }\n}\n";
    let got = routes_in(method_body(src, "get"));
    let want: BTreeSet<String> = [
        "/",
        "/index.html",
        "/health",
        "/support-access",
        "/jobs/{id}/preview",
        "/cloudprnt",
        "/cloudprnt/{key}",
        "/agent/jobs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(got, want);
}

#[test]
fn the_scanner_sees_wrapped_calls_wrapped_patterns_and_other_uses_of_the_target() {
    // rustfmt wraps a long id_of call and a long or-pattern
    let src = "\nimpl X {\n    fn get(&self) -> R {\n        let t = self.target();\n        if let Some(id) = id_of(\n            t,\n            \"/jobs/\",\n            \"/secret\",\n        ) {\n        }\n        match t {\n            \"/a\"\n            | \"/b\" => x(),\n            _ => {}\n        }\n    }\n}\n";
    let body = method_body(src, "get");
    let got = routes_in(body);
    let want: BTreeSet<String> = ["/jobs/{id}/secret", "/a", "/b"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, want);
    assert!(
        unread_uses_of_t(body).is_empty(),
        "{:?}",
        unread_uses_of_t(body)
    );
    // a path the router answers through a helper, or in a form the scanner does not read
    for hidden in [
        "if let Some(r) = self.extra_get(t)? { return Ok(r); }",
        "if matches!(t, \"/hidden\") { return a(); }",
        "let parts: Vec<&str> = t.split('/').collect();",
        "if t.ends_with(\"/export\") { return a(); }",
        "if let Some(id) = parse_id(t) { return a(); }",
    ] {
        let src = format!(
            "\nimpl X {{\n    fn get(&self) -> R {{\n        let t = self.target();\n        {hidden}\n    }}\n}}\n"
        );
        assert_eq!(
            unread_uses_of_t(method_body(&src, "get")),
            [hidden],
            "{hidden}"
        );
    }
    // and what is no use of the target
    for fine in [
        "let at = 1;",
        "let s = \"t\";",
        "foo.t(1);",
        "a::t(1);",
        "let x = it(1);",
    ] {
        let src = format!("\nimpl X {{\n    fn get(&self) -> R {{\n        {fine}\n    }}\n}}\n");
        assert!(
            unread_uses_of_t(method_body(&src, "get")).is_empty(),
            "{fine}"
        );
    }
}

/// A dispatcher around `stmt`, for the self-tests of the scanner.
fn dispatcher(stmt: &str) -> String {
    format!(
        "\nimpl X {{\n    fn get(&self) -> R {{\n        let t = self.target();\n{stmt}\n    }}\n}}\n"
    )
}

#[test]
fn the_scanner_reads_every_comparison_and_id_helper_of_a_statement() {
    // two comparisons in one condition, one wrapped by rustfmt, two id_of calls in one statement
    let src = dispatcher(
        "        if t == \"/agent/jobs\" || t == \"/agent/hidden\" {\n        }\n        if t == \"/a\"\n            || t == \"/b\"\n        {\n        }\n        let r = id_of(t, \"/p/\", \"\").or_else(|| id_of(t, \"/hidden/\", \"/s\"));",
    );
    let body = method_body(&src, "get");
    let want: BTreeSet<String> = [
        "/agent/jobs",
        "/agent/hidden",
        "/a",
        "/b",
        "/p/{id}",
        "/hidden/{id}/s",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(routes_in(body), want);
    assert!(
        unread_uses_of_t(body).is_empty(),
        "{:?}",
        unread_uses_of_t(body)
    );
}

#[test]
fn the_scanner_refuses_a_target_that_is_not_compared_with_literals() {
    for hidden in [
        "if let Some(id) = id_of(t, HIDDEN, \"\") { return a(); }",
        "if let Some(id) = id_of(t, \"/jobs/\", suffix) { return a(); }",
        "if t == HIDDEN { return a(); }",
    ] {
        let src = dispatcher(&format!("        {hidden}"));
        assert_eq!(
            unread_uses_of_t(method_body(&src, "get")),
            [hidden],
            "{hidden}"
        );
    }
}

#[test]
fn a_match_on_the_target_has_only_literal_arms() {
    let fine = dispatcher(
        "        match t {\n            \"/a\" => return x(),\n            \"/b\"\n            | \"/c\" => {\n                let s = \"}\";\n                y(s, \"=>\")\n            }\n            // \"/commented\" if x => z(),\n            _ => {}\n        }",
    );
    assert!(opaque_arms(method_body(&fine, "get")).is_empty());
    assert_eq!(opaque_arms(method_body(&fine, "get")), Vec::<String>::new());
    for (arm, what) in [
        ("HIDDEN => return x(),", "HIDDEN"),
        (
            "other => { if let Some(r) = self.extra_get(other)? { return Ok(r); } }",
            "other",
        ),
        (
            "\"/hidden/export\" | \"/hidden/import\"\n                if cond => return x(),",
            "a guard on the next line",
        ),
        ("\"/g\" if cond => return x(),", "a guard"),
        (
            "p if p.ends_with(\"/export\") => return x(),",
            "a guard on a binding",
        ),
        ("Some(\"/x\") => return x(),", "a wrapped literal"),
    ] {
        let src = dispatcher(&format!(
            "        match t {{\n            \"/a\" => return x(),\n            {arm}\n            _ => {{}}\n        }}"
        ));
        assert_eq!(opaque_arms(method_body(&src, "get")).len(), 1, "{what}");
    }
    // a match on the target inside an arm of another one is walked too
    let nested = dispatcher(
        "        match t {\n            \"/a\" => return x(),\n            _ => {\n                match t {\n                    HIDDEN => return y(),\n                    _ => {}\n                }\n            }\n        }",
    );
    assert_eq!(opaque_arms(method_body(&nested, "get")), ["HIDDEN"]);
    // another match is none of its business
    let other = dispatcher(
        "        match self.db() {\n            Ok(x) => x,\n            e => return e,\n        }",
    );
    assert!(opaque_arms(method_body(&other, "get")).is_empty());
}

/// The lines of the dispatchers that use `t` as something other than the target: a binding of the same
/// name in an inner scope of get(). Pinned so that a new use is a new line and fails.
const T_SHADOWS: [&str; 2] = [
    "if let Some(t) = &self.live.terms.value {",
    "h[\"terms\"] = json!({ \"version\": t.version, \"terms_url\": t.terms_url, \"dpa_url\": t.dpa_url, \"privacy_url\": t.privacy_url });",
];

#[test]
fn the_dispatchers_read_the_request_target_only_in_forms_the_scanner_reads() {
    let src = read("src/server.rs");
    let src = non_test(&src);
    let mut shadows = BTreeSet::new();
    for f in ["get", "post", "put", "delete"] {
        let opaque = opaque_arms(method_body(src, f));
        // the walker reaches every literal arm the route scan reads (it did not stop early)
        let walked = arm_patterns(method_body(src, f)).join(" ");
        for l in logical_lines(method_body(src, f)) {
            if l.starts_with('"') && l.contains("=>") {
                for lit in literals(l.split("=>").next().unwrap()) {
                    assert!(
                        walked.contains(&format!("\"{lit}\"")),
                        "{f}(): the arm walk missed {lit}"
                    );
                }
            }
        }
        assert!(
            opaque.is_empty(),
            "{f}(): an arm of `match t` is not a string literal, so the route scan cannot read it: {opaque:?}"
        );
        for l in unread_uses_of_t(method_body(src, f)) {
            if T_SHADOWS.contains(&l.as_str()) {
                shadows.insert(l);
            } else {
                panic!(
                    "{f}(): a line uses the request target in a form the route scan does not read: {l}\nDocument the route it adds and teach routes_in the form, or pin it"
                );
            }
        }
    }
    assert_eq!(
        shadows.len(),
        T_SHADOWS.len(),
        "a pin in T_SHADOWS is stale"
    );
}

#[test]
fn every_route_of_the_router_is_documented_and_every_documented_route_is_routed() {
    let d = doc();
    let documented: BTreeSet<(String, String)> =
        operations(&d).into_iter().map(|(m, p, _)| (m, p)).collect();
    let routed = router_routes(non_test(&read("src/server.rs")));
    let excluded: BTreeSet<(String, String)> = EXCLUDED
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    // (3) an exclusion that no longer occurs is stale
    for e in &excluded {
        assert!(routed.contains(e), "stale exclusion {e:?}");
    }
    let undocumented: Vec<_> = routed
        .difference(&excluded)
        .filter(|r| !documented.contains(*r))
        .collect();
    assert!(
        undocumented.is_empty(),
        "routed but not in openapi.json: {undocumented:?}"
    );
    let unrouted: Vec<_> = documented.iter().filter(|r| !routed.contains(*r)).collect();
    assert!(
        unrouted.is_empty(),
        "in openapi.json but not routed: {unrouted:?}"
    );
}

/// Every line that reads the request path outside the four dispatchers' arms, pinned with a reason:
/// (file, trimmed line, how often, why it needs no entry in the document). A new dispatcher, helper
/// or http.rs route is a new line and fails until somebody documents it or pins it here.
const PINNED: &[(&str, &str, usize, &str)] = &[
    (
        "src/http.rs",
        ".path_and_query()",
        1,
        "the request target, taken from the URI before routing",
    ),
    (
        "src/http.rs",
        "let resp = if method == \"GET\" && target == \"/agent/jobs\" {",
        1,
        "GET /agent/jobs is the async long-poll, documented as the agent claim (the sync twin is the arm in get())",
    ),
    (
        "src/http.rs",
        "let target = if target.starts_with(\"/operator/\") {",
        1,
        "the access log drops the query of an operator path",
    ),
    (
        "src/http.rs",
        "target.split('?').next().unwrap_or(target)",
        1,
        "the same log line",
    ),
    (
        "src/server.rs",
        "&self.req.target",
        1,
        "target(): the request target",
    ),
    (
        "src/server.rs",
        "fn cloudprnt_route(path: &str) -> Option<Option<&str>> {",
        1,
        "the CloudPRNT matcher: documented as /cloudprnt and /cloudprnt/{key}",
    ),
    (
        "src/server.rs",
        "fn pn_set(path: &str) -> Option<PnSet<'_>> {",
        1,
        "PrintNode id sets, only reached with a Basic header: not part of this document",
    ),
    (
        "src/server.rs",
        "if let Some(key) = cloudprnt_route(self.req.path()) {",
        3,
        "CloudPRNT in get, post and delete: documented",
    ),
    (
        "src/server.rs",
        "if path == \"/operator\" || path.starts_with(\"/operator/\") {",
        1,
        "/operator/** has its own gate and is no part of this document",
    ),
    (
        "src/server.rs",
        "if self.req.path() != \"/printjobs\" {",
        1,
        "PrintNode POST /printjobs, Basic only (docs/printnode-compat.md)",
    ),
    (
        "src/server.rs",
        "if self.req.path() == \"/integrations/shopify/orders\" {",
        1,
        "the Shopify webhook in post(): documented",
    ),
    (
        "src/server.rs",
        "if self.req.path() == \"/support-access\" {",
        1,
        "GET /support-access in get(): documented",
    ),
    (
        "src/server.rs",
        "if self.target() == \"/health\" {",
        1,
        "HEAD /health: the twin of a documented GET",
    ),
    (
        "src/server.rs",
        "if self.target() == \"/openapi.json\" {",
        1,
        "HEAD /openapi.json: the twin of a documented GET",
    ),
    (
        "src/server.rs",
        "let (kind, rest) = path.strip_prefix('/')?.split_once('/')?;",
        1,
        "pn_set: PrintNode only",
    ),
    (
        "src/server.rs",
        "let Some(m) = pn_set(self.req.path()).filter(|m| m.kind == \"printjobs\" && m.sub.is_empty())",
        1,
        "PrintNode DELETE /printjobs/{set}, Basic only",
    ),
    (
        "src/server.rs",
        "let asked = qfirst(&query_params(self.target()), \"org\").map(str::parse::<i64>);",
        1,
        "the ?org= of GET /support-access: documented as a query parameter",
    ),
    (
        "src/server.rs",
        "let body: Value = if path == \"/whoami\" {",
        1,
        "PrintNode /whoami, Basic only",
    ),
    (
        "src/server.rs",
        "let d = target.strip_prefix(prefix)?.strip_suffix(suffix)?;",
        1,
        "id_of: digits only; the dispatchers' id_of calls are scanned as {id}",
    ),
    (
        "src/server.rs",
        "let key = rest.strip_prefix('/')?;",
        1,
        "cloudprnt_route",
    ),
    (
        "src/server.rs",
        "let m = pn_set(path);",
        1,
        "PrintNode GET shapes, Basic only",
    ),
    (
        "src/server.rs",
        "let path = req.path();",
        1,
        "handle(): the operator split",
    ),
    (
        "src/server.rs",
        "let path = self.req.path();",
        1,
        "printnode_get: Basic only",
    ),
    (
        "src/server.rs",
        "let q = query_params(self.target());",
        4,
        "the query of the routes that document one: Shopify (key, printer_id), CloudPRNT (mac, type, token, code), PrintNode limit",
    ),
    (
        "src/server.rs",
        "let rest = path.strip_prefix(\"/cloudprnt\")?;",
        1,
        "cloudprnt_route",
    ),
    (
        "src/server.rs",
        "let t = self.target();",
        4,
        "the dispatchers' own target: their arms are scanned",
    ),
    (
        "src/server.rs",
        "let target = self.target();",
        1,
        "dashboard(): the static bundle, not an API route",
    ),
    (
        "src/server.rs",
        "self.target.split('?').next().unwrap_or(\"\")",
        1,
        "Req::path()",
    ),
];

fn path_lines(src: &str) -> Vec<String> {
    let tokens = [
        "req.path()",
        "self.target()",
        "req.target",
        "cloudprnt_route(",
        "pn_set(",
        "path_and_query",
        "target ==",
        "path ==",
        "path !=",
        "target.starts_with(",
        "target.split(",
    ];
    non_test(src)
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter(|l| {
            tokens.iter().any(|t| l.contains(t))
                || ((l.contains("strip_prefix(") || l.contains("starts_with("))
                    && path_literal_arg(l))
        })
        .map(String::from)
        .collect()
}

/// True when the first argument of a strip_prefix/starts_with on the line is not a literal that
/// is clearly no path (a header scheme, a key prefix).
fn path_literal_arg(l: &str) -> bool {
    for f in ["strip_prefix(", "starts_with("] {
        if let Some(i) = l.find(f) {
            let arg = &l[i + f.len()..];
            // a literal that starts with a slash, or a variable (`prefix`): not a constant like
            // NAME_PREFIX, a header scheme or a byte-order mark
            let arg_text = arg.split(')').next().unwrap_or("");
            return arg.starts_with("\"/")
                || arg.starts_with("'/")
                || (arg.starts_with(|c: char| c.is_ascii_lowercase()) && !arg_text.contains("::"));
        }
    }
    false
}

#[test]
fn every_line_that_reads_the_path_is_pinned() {
    let mut files = vec!["src/server.rs".to_string(), "src/http.rs".to_string()];
    for e in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/server")).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        // the operator API has its own document and its own guard (http_operator.rs)
        if name != "operator.rs" {
            files.push(format!("src/server/{name}"));
        }
    }
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for f in &files {
        for l in path_lines(&read(f)) {
            *seen.entry((f.clone(), l)).or_default() += 1;
        }
    }
    let mut problems = Vec::new();
    for ((f, l), n) in &seen {
        match PINNED.iter().find(|(pf, pl, _, _)| pf == f && pl == l) {
            None => problems.push(format!("not pinned: {f}: {l}")),
            Some((_, _, want, _)) if want != n => {
                problems.push(format!("{f}: {l}: pinned {want}x, found {n}x"));
            }
            Some(_) => {}
        }
    }
    for (f, l, _, why) in PINNED {
        assert!(!why.is_empty());
        if !seen.contains_key(&(f.to_string(), l.to_string())) {
            problems.push(format!("stale pin: {f}: {l}"));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// The methods route() dispatches, pinned: a new one needs documenting.
#[test]
fn route_dispatches_exactly_these_methods() {
    let src = read("src/server.rs");
    let start = src.find("fn route(state: &AppState").unwrap();
    let body = &src[start..start + src[start..].find("\n}\n").unwrap()];
    let arms: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('"') && l.contains("=>"))
        .map(|l| literals(l.split("=>").next().unwrap()).concat())
        .collect();
    assert_eq!(arms, ["GET", "HEAD", "POST", "PUT", "DELETE"]);
}

// --- C: the served copy --------------------------------------------------------------------------------

#[test]
fn the_server_serves_the_file_without_a_credential() {
    let app = App::new();
    let r = app.call("GET", "/openapi.json").send();
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert_eq!(r.header("cache-control"), Some("no-cache"));
    assert_eq!(r.body, DOC.as_bytes());
    // no credential of any kind is needed, and a wrong one changes nothing
    assert_eq!(
        app.call("GET", "/openapi.json").bearer("nope").send().body,
        r.body
    );
    // the PrintNode layer's Basic header gets the same document
    assert_eq!(
        app.call("GET", "/openapi.json").basic("nope").send().body,
        r.body
    );
    let head = app.call("HEAD", "/openapi.json").send();
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.header("content-type"), Some("application/json"));
    assert_eq!(
        head.header("content-length"),
        Some(DOC.len().to_string().as_str())
    );
    assert_eq!(app.call("POST", "/openapi.json").send().status, 404);
    assert_eq!(app.call("PUT", "/openapi.json").send().status, 404);
    assert_eq!(app.call("DELETE", "/openapi.json").send().status, 404);
}

#[test]
fn the_document_carries_no_credential_and_no_deployment_data() {
    for needle in ["sess_", "Bearer ey", "localhost", "127.0.0.1"] {
        assert!(!DOC.contains(needle), "{needle}");
    }
    assert!(
        doc().get("servers").is_none(),
        "no servers: the document is the same on every host"
    );
}

// --- D: the client package ---------------------------------------------------------------------------

fn client_file(rel: &str) -> String {
    std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../clients/js/").to_string() + rel,
    )
    .unwrap_or_else(|e| panic!("clients/js/{rel}: {e}"))
}

#[test]
fn the_client_package_follows_the_workspace_version_and_ships_only_what_it_should() {
    let pkg: Value = serde_json::from_str(&client_file("package.json")).unwrap();
    assert_eq!(
        pkg["version"],
        env!("CARGO_PKG_VERSION"),
        "clients/js/package.json is in the release bump list (docs: AGENTS.md, Client)"
    );
    assert!(
        pkg.get("dependencies").is_none(),
        "the client has no runtime dependency"
    );
    assert_eq!(
        pkg["files"],
        serde_json::json!(["build/src", "README.md", "LICENSE"])
    );
    assert_eq!(pkg["license"], "SEE LICENSE IN LICENSE");
    // npm trusted publishing refuses a package whose repository.url is not the GitHub repository the
    // workflow runs in: https://github.com/npm/documentation/blob/main/content/packages-and-modules/securing-your-code/trusted-publishers.mdx
    // ("repository.url field in package.json must exactly match your GitHub repository"). The dry run
    // does not talk to the registry and would not show it.
    assert_eq!(
        pkg["repository"],
        serde_json::json!({
            "type": "git",
            "url": "git+https://github.com/LukiPrince/printpapi.git",
            "directory": "clients/js"
        })
    );
    assert_eq!(pkg["type"], "module");
    let root =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../LICENSE")).unwrap();
    assert_eq!(
        client_file("LICENSE"),
        root,
        "the client's LICENSE is the repository's, byte for byte"
    );
    let dev: BTreeSet<_> = pkg["devDependencies"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        dev,
        ["@types/node", "typescript"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
}

// --- E: the docs name it ---------------------------------------------------------------------------------

#[test]
fn docs_api_md_mentions_the_document() {
    let api =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api.md")).unwrap();
    assert!(api.contains("/openapi.json"));
}
