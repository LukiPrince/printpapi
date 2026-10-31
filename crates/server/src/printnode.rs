// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! PrintNode-compatible API layer — the port of `app/printnode.py`. Pure translation.
//!
//! PrintNode is a trademark of PrintNode Ltd. printpapi is not affiliated with, endorsed by, or
//! sponsored by PrintNode. This layer was written from the publicly documented API surface; no
//! PrintNode source code or documentation text was copied into it.
//!
//! The same server and job store, dressed in the JSON shapes PrintNode's clients expect, so an
//! existing plugin or SDK can be pointed at a printpapi base URL and keep working. It is selected
//! by auth scheme: PrintNode carries the API key as the HTTP Basic username, so a `Basic` header
//! means "answer in PrintNode's shapes" and `Bearer` keeps printpapi's own.
use crate::dispatch::MAX_COPIES;
use crate::pyfmt::{get, py_str, repr, truthy};
use crate::store::{AgentInfo, JobRow, Metrics, PrinterInfo};
use base64::Engine as _;
use base64::alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
use chrono::{DateTime, Datelike};
use serde_json::{Map, Value, json};

/// A request this layer cannot translate — the caller maps it to 400.
#[derive(Debug, Clone, PartialEq)]
pub struct CompatError(pub String);

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CompatError {}

/// The API key from an `Authorization: Basic` header (username half); "" if unusable.
/// PrintNode's clients send the key as the username with an empty password.
pub fn basic_key(header: &str) -> String {
    let Some(rest) = header.strip_prefix("Basic ") else {
        return String::new();
    };
    // Python's strict b64decode accepts non-zero trailing bits but needs canonical padding.
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    );
    let Ok(raw) = engine.decode(py_trim(rest)) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&raw);
    text.split(':').next().unwrap_or("").to_string()
}

/// Epoch seconds -> `2026-07-03T10:00:00.123Z` (Python isoformat, millisecond timespec).
///
/// Like `datetime.fromtimestamp`, the fraction is rounded half-even to microseconds first;
/// isoformat then truncates to milliseconds. Out-of-range input (Python: ValueError) gives None.
pub fn iso(ts: Option<f64>) -> Option<String> {
    let ts = ts?;
    if !ts.is_finite() || ts.abs() > 1e12 {
        return None;
    }
    let mut secs = ts.trunc();
    let mut us = ((ts - secs) * 1e6).round_ties_even();
    if us >= 1e6 {
        us -= 1e6;
        secs += 1.0;
    } else if us < 0.0 {
        us += 1e6;
        secs -= 1.0;
    }
    let dt = DateTime::from_timestamp(secs as i64, (us as u32) * 1000)?;
    if !(1..=9999).contains(&dt.year()) {
        return None;
    }
    Some(format!(
        "{}.{:03}Z",
        dt.format("%Y-%m-%dT%H:%M:%S"),
        us as u32 / 1000
    ))
}

/// Python's whitespace: Unicode White_Space plus U+001C..U+001F, which `str.strip()`/`split()`
/// treat as spaces and Rust's `trim()` does not.
pub(crate) fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub(crate) fn py_trim(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

/// Python `int(s)` for a str: surrounding whitespace, one sign, digits with single underscores
/// between them. Out-of-range values saturate to the i128 limits (callers only compare them).
pub(crate) fn py_int(s: &str) -> Option<i128> {
    let t = py_trim(s);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    // Python refuses more than 4300 digits (sys.int_info.default_max_str_digits) with a ValueError.
    if digits.chars().filter(|c| *c != '_').count() > 4300 {
        return None;
    }
    let mut n: i128 = 0;
    for c in digits.chars().filter(|c| *c != '_') {
        let d = i128::from(c.to_digit(10)?);
        n = n.saturating_mul(10).saturating_add(d);
    }
    Some(if neg { -n } else { n })
}

/// Stays clear of SQLite's bound-parameter limit (999 on builds before 3.32).
const MAX_SET: usize = 500;

/// `10`, `10,12`, `5-9` or a mix; at most 500 ids. A range is a shorthand for a handful of ids,
/// not an invitation to materialise `1-999999999`.
pub fn parse_set(spec: &str) -> Result<Vec<i64>, CompatError> {
    let bad = || CompatError(format!("bad id set: {}", repr(&Value::String(spec.into()))));
    let too_large = || {
        CompatError(format!(
            "id set too large or reversed: {}",
            repr(&Value::String(spec.into()))
        ))
    };
    let mut ids: Vec<i64> = Vec::new();
    for part in spec.split(',') {
        let (lo, hi) = py_trim(part).split_once('-').unwrap_or((py_trim(part), ""));
        let lo = py_int(lo).ok_or_else(bad)?;
        let hi = if hi.is_empty() {
            lo
        } else {
            py_int(hi).ok_or_else(bad)?
        };
        // ponytail: ids beyond i64 cannot be addressed anyway (Python returned bigints that no row
        // matches); they are refused with the too-large text.
        let (Ok(lo), Ok(hi)) = (i64::try_from(lo), i64::try_from(hi)) else {
            return Err(too_large());
        };
        if hi < lo || ids.len() as i128 + (i128::from(hi) - i128::from(lo) + 1) > MAX_SET as i128 {
            return Err(too_large());
        }
        ids.extend(lo..=hi);
    }
    if ids.is_empty() {
        return Err(bad());
    }
    Ok(ids)
}

/// `list(x)` for the values a capability list may arrive as; anything else reads as empty.
fn py_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::String(s) => s.chars().map(|c| Value::String(c.to_string())).collect(),
        Value::Object(o) => o.keys().map(|k| Value::String(k.clone())).collect(),
        _ => vec![],
    }
}

/// A dict key the way `json.dumps` coerces it: `True` -> "true", `None` -> "null".
fn paper_key(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        other => py_str(other),
    }
}

/// Agent-reported capabilities -> PrintNode's capability object. What the agent cannot discover is
/// reported empty instead of invented — paper dimensions in particular: we know the names, not the
/// micrometre extents, so every paper maps to null. Empty or unusable input gives null.
pub fn capabilities(caps: Option<&Value>) -> Value {
    // ponytail: Python crashed on a truthy non-object (AttributeError); that reads as "no
    // capabilities" here.
    let Some(caps @ Value::Object(_)) = caps else {
        return Value::Null;
    };
    if !truthy(caps) {
        return Value::Null;
    }
    let papers: Map<String, Value> = py_list(get(caps, "papers"))
        .iter()
        .map(|n| (paper_key(n), Value::Null))
        .collect();
    json!({
        "bins": py_list(get(caps, "bins")),
        "collate": false,
        "color": truthy(get(caps, "color")),
        "copies": MAX_COPIES,
        "dpis": [],
        "duplex": truthy(get(caps, "duplex")),
        "extent": [],
        "medias": [],
        "nup": [],
        "papers": papers,
        "printrate": null,
        "supports_custom_paper_size": false,
    })
}

/// A `store.list_agents()` row -> PrintNode computer object. The network fields are null: our
/// agent reports its name and its printers, not its interfaces.
pub fn computer(agent: &AgentInfo) -> Value {
    json!({
        "id": agent.id, "name": agent.name, "inet": null, "inet6": null,
        "hostname": agent.name, "version": "", "jre": null,
        "createTimestamp": iso(Some(agent.created_at)),
        "state": if agent.online { "connected" } else { "disconnected" },
    })
}

/// A `store.list_printers()` row (plus its computer object) -> PrintNode printer object.
pub fn printer(p: &PrinterInfo, comp: Option<&Value>) -> Value {
    let computer = match comp {
        Some(c) if truthy(c) => c.clone(),
        _ => json!({
            "id": p.agent_id, "name": p.agent_name,
            "state": if p.online { "connected" } else { "disconnected" },
        }),
    };
    json!({
        "id": p.id, "name": p.name, "description": p.name,
        "computer": computer,
        "capabilities": capabilities(p.capabilities.as_ref()),
        "default": false, "createTimestamp": iso(Some(p.created_at)),
        "state": if p.online { "online" } else { "offline" },
    })
}

/// Our job state -> PrintNode's. `failed` splits in two over there (a missed deadline is
/// `expired`), and a cancelled job is `deleted` — they have no separate cancelled state.
pub fn job_state(state: &str, error: Option<&str>) -> String {
    match state {
        "failed" => if error == Some("expired") {
            "expired"
        } else {
            "error"
        }
        .into(),
        "queued" => "queued".into(),
        "claimed" => "sent".into(),
        "done" => "done".into(),
        "cancelled" => "deleted".into(),
        other => other.into(),
    }
}

/// We do not persist the client's `source` string, so every job reports its printer of origin.
const SOURCE: &str = "printpapi";

/// A `store.recent_jobs()` row -> PrintNode print job object.
pub fn printjob(job: &JobRow, printer_obj: Option<&Value>) -> Value {
    let printer = match printer_obj {
        Some(p) if truthy(p) => p.clone(),
        _ => json!({"id": job.printer_id, "name": job.printer_name}),
    };
    json!({
        "id": job.id, "printer": printer,
        "title": job.title.as_deref().unwrap_or(""), "contentType": job.type_,
        "source": SOURCE, "expireAt": null, "createTimestamp": iso(Some(job.created_at)),
        "state": job_state(&job.state, job.error.as_deref()),
    })
}

/// The per-job state history PrintNode reports. We keep only the current state, so this is a
/// single entry — enough for the clients that poll it waiting for a terminal state.
pub fn printjob_states(job: &JobRow) -> Value {
    let when = job
        .finished_at
        .filter(|t| *t != 0.0)
        .unwrap_or(job.created_at);
    json!([{
        "printJobId": job.id, "state": job_state(&job.state, job.error.as_deref()),
        "message": job.error.as_deref().unwrap_or(""), "clientVersion": "",
        "createTimestamp": iso(Some(when)),
    }])
}

/// The account object clients fetch first to check their credentials. Counts come from the metrics
/// snapshot; the billing fields are null because there is no billing here.
pub fn whoami(org_id: Option<i64>, metrics: &Metrics, online_names: &[String]) -> Value {
    json!({
        "id": org_id.unwrap_or(0), "firstname": "printpapi", "lastname": "", "email": "",
        "canCreateSubAccounts": false, "creatorEmail": "", "childAccounts": [],
        "credits": null, "numComputers": metrics.agents_total,
        "totalPrints": metrics.jobs.get("done").copied().unwrap_or(0), "versions": {},
        "connected": online_names, "Tags": {}, "ActiveSubscriptions": [], "state": "active",
    })
}

const CONTENT_TYPES: [&str; 4] = ["pdf_uri", "pdf_base64", "raw_uri", "raw_base64"];
/// The subset of PrintNode's options the agent can actually apply.
const OPTIONS: [&str; 5] = ["paper", "bin", "color", "duplex", "pages"];

/// A PrintNode `POST /printjobs` body -> a printpapi `POST /jobs` body.
///
/// Unknown option keys are dropped rather than rejected, and options on a raw job are dropped
/// whole: a plugin sends its entire option set (rotate, dpi, fit_to_page, ...) with every job, and
/// a 400 over one we do not implement would defeat the point of the layer. What we do map is
/// validated by the normal `POST /jobs` path — this function invents no leniency about values.
pub fn job_body(pn: &Value) -> Result<Map<String, Value>, CompatError> {
    let Value::Object(pn_map) = pn else {
        return Err(CompatError("body must be an object".into()));
    };
    let ct = match get(pn, "contentType") {
        Value::String(s) if CONTENT_TYPES.contains(&s.as_str()) => s.as_str(),
        other => {
            return Err(CompatError(format!(
                "unsupported contentType: {} (supported: {})",
                repr(other),
                CONTENT_TYPES.join(", ")
            )));
        }
    };
    let mut body = Map::new();
    body.insert("type".into(), json!(ct));
    body.insert("printer_id".into(), get(pn, "printerId").clone());
    body.insert("title".into(), get(pn, "title").clone());
    // The URL rides in `content` on their side; ours takes it in `url`.
    let content_key = if ct.ends_with("_uri") {
        "url"
    } else {
        "content"
    };
    body.insert(content_key.into(), get(pn, "content").clone());
    let empty = Map::new();
    let opts = match pn_map.get("options") {
        Some(Value::Object(o)) => o,
        _ => &empty,
    };
    let qty = pn_map
        .get("qty")
        .or_else(|| opts.get("copies"))
        .unwrap_or(&Value::Null);
    if !qty.is_null() {
        body.insert("copies".into(), qty.clone());
    }
    let expire = get(pn, "expireAfter");
    if !expire.is_null() {
        body.insert("expire_after".into(), expire.clone());
    }
    if ct.starts_with("pdf") {
        let keep: Map<String, Value> = OPTIONS
            .iter()
            .filter_map(|k| opts.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect();
        if !keep.is_empty() {
            body.insert("options".into(), Value::Object(keep));
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn b64(s: &[u8]) -> String {
        STANDARD.encode(s)
    }

    fn msg<T: std::fmt::Debug>(r: Result<T, CompatError>) -> String {
        r.unwrap_err().0
    }

    #[test]
    fn basic_key_takes_the_username_half() {
        let hdr = format!("Basic {}", b64(b"secretkey:"));
        assert_eq!(basic_key(&hdr), "secretkey");
        // a password is present but irrelevant
        assert_eq!(basic_key(&format!("Basic {}", b64(b"k:pw"))), "k");
        assert_eq!(basic_key(&format!("Basic {}", b64(b"k:p:q"))), "k");
        assert_eq!(basic_key("Bearer secretkey"), "");
        assert_eq!(basic_key("Basic !!!not-base64!!!"), "");
        assert_eq!(basic_key(""), "");
    }

    #[test]
    fn basic_key_is_strict_like_python_b64decode_validate() {
        // reference: app.printnode.basic_key on Python 3.14
        assert_eq!(basic_key("Basic YQ=="), "a");
        assert_eq!(basic_key("Basic YR=="), "a"); // non-zero trailing bits are fine
        assert_eq!(basic_key("Basic  YQ== "), "a");
        assert_eq!(basic_key("Basic YQ==\n"), "a");
        assert_eq!(basic_key("Basic YQ"), ""); // missing padding
        assert_eq!(basic_key("Basic Y Q=="), ""); // inner whitespace
        assert_eq!(basic_key("Basic ===="), "");
        assert_eq!(basic_key("Basic =YQ="), "");
        assert_eq!(basic_key("Basic YQ==YQ=="), "");
        assert_eq!(basic_key("Basic "), "");
        assert_eq!(basic_key("basic YQ=="), ""); // the scheme match is case-sensitive
        assert_eq!(basic_key("Basic /+8="), "\u{fffd}\u{fffd}"); // lossy decode, no crash
    }

    #[test]
    fn parse_set_handles_ids_ranges_and_mixes() {
        assert_eq!(parse_set("10").unwrap(), [10]);
        assert_eq!(parse_set("10,12").unwrap(), [10, 12]);
        assert_eq!(parse_set("5-7").unwrap(), [5, 6, 7]);
        assert_eq!(parse_set("1,4-6").unwrap(), [1, 4, 5, 6]);
        for bad in ["", "abc", "-5", "9-4", "1-999999"] {
            assert!(parse_set(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parse_set_matches_python_int_and_error_texts() {
        // reference: app.printnode.parse_set on Python 3.14
        assert_eq!(parse_set("1_0").unwrap(), [10]);
        assert_eq!(parse_set("+5").unwrap(), [5]);
        assert_eq!(parse_set("  7 ").unwrap(), [7]);
        assert_eq!(parse_set("5-").unwrap(), [5]);
        assert_eq!(parse_set("1 - 3").unwrap(), [1, 2, 3]);
        assert_eq!(parse_set("1,1").unwrap(), [1, 1]);
        assert_eq!(parse_set("0-499").unwrap().len(), 500);
        assert_eq!(
            msg(parse_set("0-500")),
            "id set too large or reversed: '0-500'"
        );
        assert_eq!(
            msg(parse_set("5--3")),
            "id set too large or reversed: '5--3'"
        );
        assert_eq!(msg(parse_set("9-4")), "id set too large or reversed: '9-4'");
        assert_eq!(msg(parse_set("1-2-3")), "bad id set: '1-2-3'");
        assert_eq!(msg(parse_set("-")), "bad id set: '-'");
        assert_eq!(msg(parse_set(",")), "bad id set: ','");
        assert_eq!(msg(parse_set("")), "bad id set: ''");
        assert_eq!(msg(parse_set("a'b")), "bad id set: \"a'b\"");
        assert_eq!(msg(parse_set("1__0")), "bad id set: '1__0'");
        assert_eq!(msg(parse_set("_1")), "bad id set: '_1'");
        assert_eq!(
            msg(parse_set("0-99999999999999999999999")),
            "id set too large or reversed: '0-99999999999999999999999'"
        );
        assert_eq!(
            msg(parse_set("99999999999999999999999")),
            "id set too large or reversed: '99999999999999999999999'"
        );
        // the cap counts across parts
        assert_eq!(
            msg(parse_set("1-300,1-201")),
            "id set too large or reversed: '1-300,1-201'"
        );
        assert_eq!(
            parse_set("-5-").map_err(|e| e.0),
            Err("bad id set: '-5-'".to_string())
        );
        assert_eq!(
            parse_set(&format!("{0}-{0}", i64::MAX)).unwrap(),
            [i64::MAX]
        );
    }

    #[test]
    fn job_state_mapping() {
        assert_eq!(job_state("queued", None), "queued");
        assert_eq!(job_state("claimed", None), "sent");
        assert_eq!(job_state("done", None), "done");
        assert_eq!(job_state("failed", Some("printer offline")), "error");
        assert_eq!(job_state("failed", Some("expired")), "expired");
        assert_eq!(job_state("failed", None), "error");
        assert_eq!(job_state("cancelled", None), "deleted");
        assert_eq!(job_state("weird", None), "weird");
    }

    fn body(v: Value) -> String {
        serde_json::to_string(&job_body(&v).unwrap()).unwrap()
    }

    #[test]
    fn job_body_maps_content_url_qty_and_options() {
        let b = job_body(
            &json!({"printerId": 3, "title": "T", "contentType": "pdf_uri",
            "content": "https://x/y.pdf", "qty": 2, "expireAfter": 60,
            "options": {"paper": "A4", "rotate": 90, "fit_to_page": true}}),
        )
        .unwrap();
        // the URL rides in `content` on their side, in `url` on ours
        assert_eq!(
            Value::Object(b),
            json!({"type": "pdf_uri", "printer_id": 3, "title": "T", "url": "https://x/y.pdf",
                "copies": 2, "expire_after": 60, "options": {"paper": "A4"}})
        );
    }

    #[test]
    fn job_body_base64_and_raw_drops_options() {
        let b = job_body(
            &json!({"printerId": 1, "contentType": "raw_base64", "content": "QUJD",
            "options": {"paper": "A4", "copies": 3}}),
        )
        .unwrap();
        // raw payloads carry their own layout — options would 400 in POST /jobs, so they are dropped
        assert_eq!(b["content"], "QUJD");
        assert!(!b.contains_key("options"));
        assert_eq!(b["copies"], 3);
        assert!(job_body(&json!({"printerId": 1, "contentType": "docx", "content": "x"})).is_err());
        assert_eq!(msg(job_body(&json!("nope"))), "body must be an object");
    }

    #[test]
    fn job_body_matches_python_reference_outputs() {
        // reference: app.printnode.job_body on Python 3.14 (json.dumps, key order included)
        assert_eq!(
            body(
                json!({"printerId": "7", "contentType": "pdf_base64", "content": "x",
                "title": null, "qty": null, "expireAfter": 0,
                "options": {"duplex": null, "paper": "A4", "pages": "1-2", "bin": "t",
                            "color": true, "copies": 9, "rotate": 1}})
            ),
            r#"{"type":"pdf_base64","printer_id":"7","title":null,"content":"x","expire_after":0,"options":{"paper":"A4","bin":"t","color":true,"duplex":null,"pages":"1-2"}}"#
        );
        assert_eq!(
            body(json!({"contentType": "raw_uri", "options": [1], "content": "u"})),
            r#"{"type":"raw_uri","printer_id":null,"title":null,"url":"u"}"#
        );
        assert_eq!(
            body(json!({"contentType": "pdf_uri", "options": {"copies": 4}})),
            r#"{"type":"pdf_uri","printer_id":null,"title":null,"url":null,"copies":4}"#
        );
        // an explicit null qty wins over options.copies
        assert_eq!(
            body(json!({"contentType": "raw_uri", "qty": null, "options": {"copies": 4}})),
            r#"{"type":"raw_uri","printer_id":null,"title":null,"url":null}"#
        );
        let ct = |v: Value| msg(job_body(&json!({"contentType": v})));
        let tail = "(supported: pdf_uri, pdf_base64, raw_uri, raw_base64)";
        assert_eq!(
            ct(json!(null)),
            format!("unsupported contentType: None {tail}")
        );
        assert_eq!(
            ct(json!("docx")),
            format!("unsupported contentType: 'docx' {tail}")
        );
        assert_eq!(ct(json!(5)), format!("unsupported contentType: 5 {tail}"));
        assert_eq!(
            ct(json!(["a"])),
            format!("unsupported contentType: ['a'] {tail}")
        );
        assert_eq!(
            msg(job_body(&json!({}))),
            format!("unsupported contentType: None {tail}")
        );
    }

    #[test]
    fn capabilities_mapping() {
        let caps = capabilities(Some(
            &json!({"papers": ["A4", "Letter"], "bins": ["Tray 1"],
            "duplex": true, "color": false}),
        ));
        assert_eq!(caps["papers"], json!({"A4": null, "Letter": null})); // names known, dimensions not
        assert_eq!(caps["bins"], json!(["Tray 1"]));
        assert_eq!(caps["duplex"], true);
        assert_eq!(caps["color"], false);
        assert_eq!(caps["copies"], 100);
        assert_eq!(capabilities(None), Value::Null);
        assert_eq!(capabilities(Some(&json!({}))), Value::Null);
        assert_eq!(capabilities(Some(&json!([1]))), Value::Null);
    }

    #[test]
    fn capabilities_match_python_reference_key_order() {
        // reference: app.printnode.capabilities on Python 3.14
        let got = capabilities(Some(&json!({"bins": "ab", "papers": ["A4", "A4", 5],
            "color": "yes", "duplex": 0})));
        assert_eq!(
            serde_json::to_string(&got).unwrap(),
            r#"{"bins":["a","b"],"collate":false,"color":true,"copies":100,"dpis":[],"duplex":false,"extent":[],"medias":[],"nup":[],"papers":{"A4":null,"5":null},"printrate":null,"supports_custom_paper_size":false}"#
        );
        assert_eq!(
            serde_json::to_string(&capabilities(Some(&json!({"x": 1})))).unwrap(),
            r#"{"bins":[],"collate":false,"color":false,"copies":100,"dpis":[],"duplex":false,"extent":[],"medias":[],"nup":[],"papers":{},"printrate":null,"supports_custom_paper_size":false}"#
        );
    }

    #[test]
    fn iso_timestamps() {
        assert_eq!(iso(Some(0.0)).unwrap(), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(None), None);
    }

    #[test]
    fn iso_matches_python_rounding_and_truncation() {
        // reference: app.printnode._iso on Python 3.14 (Linux semantics for negatives)
        let cases: [(f64, &str); 14] = [
            (1.5, "1970-01-01T00:00:01.500Z"),
            (1783072800.123, "2026-07-03T10:00:00.123Z"),
            (1783072800.1239998, "2026-07-03T10:00:00.124Z"),
            (1783072800.0005, "2026-07-03T10:00:00.000Z"),
            (0.0005, "1970-01-01T00:00:00.000Z"),
            (0.0015, "1970-01-01T00:00:00.001Z"),
            (0.0025, "1970-01-01T00:00:00.002Z"),
            (-0.0005, "1969-12-31T23:59:59.999Z"),
            (-1.5, "1969-12-31T23:59:58.500Z"),
            (-0.001, "1969-12-31T23:59:59.999Z"),
            (1000000000.9999996, "2001-09-09T01:46:41.000Z"),
            (1.9999999, "1970-01-01T00:00:02.000Z"),
            (-0.9999995, "1969-12-31T23:59:59.000Z"),
            (1700000000.9995, "2023-11-14T22:13:20.999Z"),
        ];
        for (ts, want) in cases {
            assert_eq!(iso(Some(ts)).as_deref(), Some(want), "{ts}");
        }
        assert_eq!(
            iso(Some(253402300799.0)).as_deref(),
            Some("9999-12-31T23:59:59.000Z")
        );
        assert_eq!(iso(Some(253402300800.0)), None);
        assert_eq!(
            iso(Some(-62135596800.0)).as_deref(),
            Some("0001-01-01T00:00:00.000Z")
        );
        assert_eq!(iso(Some(-62135596801.0)), None);
        assert_eq!(iso(Some(f64::NAN)), None);
        assert_eq!(iso(Some(f64::INFINITY)), None);
        assert_eq!(iso(Some(1e300)), None);
    }

    fn agent() -> AgentInfo {
        AgentInfo {
            id: 3,
            name: "pc".into(),
            last_seen_at: None,
            created_at: 1783072800.123,
            printers: 1,
            online: true,
        }
    }

    fn printer_info() -> PrinterInfo {
        PrinterInfo {
            id: 1,
            name: "Z".into(),
            agent_id: 3,
            agent_name: "pc".into(),
            can_pdf: false,
            created_at: 0.5,
            capabilities: None,
            online: false,
        }
    }

    fn job() -> JobRow {
        JobRow {
            id: 9,
            printer_id: 1,
            printer_name: "Z".into(),
            agent_name: "pc".into(),
            title: None,
            state: "failed".into(),
            type_: "pdf_uri".into(),
            mode: "pdf".into(),
            error: Some("expired".into()),
            created_at: 10.25,
            finished_at: Some(0.0),
        }
    }

    fn s(v: &Value) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn shapes_match_python_reference_key_order() {
        // reference: app.printnode on Python 3.14, json.dumps
        assert_eq!(
            s(&computer(&agent())),
            r#"{"id":3,"name":"pc","inet":null,"inet6":null,"hostname":"pc","version":"","jre":null,"createTimestamp":"2026-07-03T10:00:00.123Z","state":"connected"}"#
        );
        assert_eq!(
            s(&printer(&printer_info(), None)),
            r#"{"id":1,"name":"Z","description":"Z","computer":{"id":3,"name":"pc","state":"disconnected"},"capabilities":null,"default":false,"createTimestamp":"1970-01-01T00:00:00.500Z","state":"offline"}"#
        );
        assert_eq!(
            s(&printer(&printer_info(), Some(&json!({"id": 3})))),
            r#"{"id":1,"name":"Z","description":"Z","computer":{"id":3},"capabilities":null,"default":false,"createTimestamp":"1970-01-01T00:00:00.500Z","state":"offline"}"#
        );
        // an empty computer object is falsy in Python and falls back to the built one
        assert_eq!(
            printer(&printer_info(), Some(&json!({}))),
            printer(&printer_info(), None)
        );
        // finished_at 0.0 is falsy: the created_at is reported
        assert_eq!(
            s(&printjob(&job(), None)),
            r#"{"id":9,"printer":{"id":1,"name":"Z"},"title":"","contentType":"pdf_uri","source":"printpapi","expireAt":null,"createTimestamp":"1970-01-01T00:00:10.250Z","state":"expired"}"#
        );
        assert_eq!(
            s(&printjob_states(&job())),
            r#"[{"printJobId":9,"state":"expired","message":"expired","clientVersion":"","createTimestamp":"1970-01-01T00:00:10.250Z"}]"#
        );
        let mut done = job();
        done.finished_at = Some(11.5);
        done.error = None;
        done.state = "done".into();
        done.title = Some("Label".into());
        assert_eq!(
            s(&printjob_states(&done)),
            r#"[{"printJobId":9,"state":"done","message":"","clientVersion":"","createTimestamp":"1970-01-01T00:00:11.500Z"}]"#
        );
        assert_eq!(printjob(&done, None)["title"], "Label");
        assert_eq!(
            printjob(&done, Some(&json!({"id": 1})))["printer"],
            json!({"id": 1})
        );
    }

    #[test]
    fn whoami_shape() {
        let mut m = Metrics {
            agents_total: 2,
            ..Default::default()
        };
        m.jobs.insert("done".into(), 5);
        assert_eq!(
            s(&whoami(None, &m, &["a".to_string()])),
            r#"{"id":0,"firstname":"printpapi","lastname":"","email":"","canCreateSubAccounts":false,"creatorEmail":"","childAccounts":[],"credits":null,"numComputers":2,"totalPrints":5,"versions":{},"connected":["a"],"Tags":{},"ActiveSubscriptions":[],"state":"active"}"#
        );
        assert_eq!(whoami(Some(4), &Metrics::default(), &[])["id"], 4);
        assert_eq!(whoami(Some(4), &Metrics::default(), &[])["totalPrints"], 0);
    }

    #[test]
    fn capabilities_paper_keys_are_spelled_like_json_dumps() {
        let c = capabilities(Some(&json!({"papers": [true, null, false, 1, "A4"]})));
        let keys: Vec<_> = c["papers"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["true", "null", "false", "1", "A4"]);
    }

    #[test]
    fn py_int_rejects_more_than_4300_digits_like_python() {
        assert!(msg(parse_set(&"1".repeat(4301))).starts_with("bad id set: "));
        assert!(py_int(&"1".repeat(4300)).is_some());
        assert_eq!(py_int("\u{1c}7\u{1f}"), Some(7));
    }
}
