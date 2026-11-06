// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! agent.ini: loading, the `[headers]`/`timeout` network settings, and the `printers` grammar.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::ini::Ini;
use crate::pyrepr::{fmt_g, repr_str};

pub const HTTP_DEFAULT_TIMEOUT: f64 = 60.0;
/// The server long-polls 25 s; a shorter socket timeout would cut every poll short.
pub const MIN_TIMEOUT: f64 = 30.0;
const RESERVED_HEADERS: [&str; 3] = ["authorization", "user-agent", "content-type"];

/// The parsed agent.ini; every `[agent]` read goes through `BasicInterpolation`.
#[derive(Debug, Clone)]
pub struct Config {
    ini: Ini,
}

/// Network settings from agent.ini (see [`Config::http_settings`]).
///   headers - extra request headers, e.g. an auth proxy's service-token pair in front of /agent/*
///   timeout - per-request timeout in seconds. Must outlast the server's long-poll (25 s): without
///             one, a NAT flow that is silently dropped mid-poll hangs the agent forever while
///             the server already shows it offline.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpSettings {
    pub headers: BTreeMap<String, String>,
    pub timeout: f64,
}

impl Default for HttpSettings {
    fn default() -> Self {
        HttpSettings {
            headers: BTreeMap::new(),
            timeout: HTTP_DEFAULT_TIMEOUT,
        }
    }
}

/// Windows-1252 for 0x80..=0x9F; the other bytes map to the code point of the same number.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}', '\u{0178}',
];

/// agent.ini bytes -> text. Python decoded with the locale code page (cp1252 on Windows), so an
/// ANSI file saved by an old Notepad with "Büro" in a printer name must keep working: strict UTF-8
/// first (BOM dropped), cp1252 when that fails.
// ponytail: a UTF-8 file with non-ASCII was mojibake under Python on Windows; it reads correctly
// now, so such a printer name changes. Only cp1252, not other ANSI code pages.
fn decode_ini(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(t) => t.strip_prefix('\u{feff}').unwrap_or(t).to_string(),
        Err(_) => bytes
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => CP1252_HIGH[usize::from(b - 0x80)],
                _ => char::from(b),
            })
            .collect(),
    }
}

impl Config {
    /// Read `<base_dir>/agent.ini`. A missing/unreadable file or a missing `[agent]` section gives
    /// the "missing or invalid" text; a malformed file gives the parser's complaint.
    pub fn load(base_dir: &Path) -> Result<Config, String> {
        let ini_path = base_dir.join("agent.ini");
        let missing = || {
            format!(
                "missing or invalid {}: need an [agent] section with server_url, api_key, printers",
                ini_path.display()
            )
        };
        let bytes = std::fs::read(&ini_path).map_err(|_| missing())?;
        let text = decode_ini(&bytes);
        let ini = Ini::parse(&text).map_err(|e| format!("invalid {}: {e}", ini_path.display()))?;
        if !ini.has_section("agent") {
            return Err(missing());
        }
        Ok(Config { ini })
    }

    pub fn from_text(text: &str) -> Result<Config, String> {
        Ok(Config {
            ini: Ini::parse(text)?,
        })
    }

    /// `agent_cfg.get(key)`: the interpolated `[agent]` value, `None` when absent.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.ini.get("agent", key)
    }

    /// `agent_cfg[key]`: like [`Config::get`] but a missing key is an error.
    pub fn require(&self, key: &str) -> Result<String, String> {
        self.get(key)?
            .ok_or_else(|| format!("agent.ini [agent]: missing required key {key:?}"))
    }

    /// agent.ini -> (extra headers, timeout).
    ///
    /// Headers come from an optional `[headers]` section, one `Name = value` per line. The names
    /// are lower-cased; HTTP header names are case-insensitive, so that is harmless. Values are
    /// read raw (no interpolation) so a secret containing '%' survives. `timeout` is an optional
    /// key in `[agent]`.
    pub fn http_settings(&self) -> Result<HttpSettings, String> {
        let mut headers = BTreeMap::new();
        if let Some(items) = self.ini.items_raw("headers") {
            let defaults = self.ini.defaults();
            for (name, value) in items {
                if defaults.iter().any(|(k, _)| *k == name) {
                    continue;
                }
                if RESERVED_HEADERS.contains(&name.to_lowercase().as_str()) {
                    return Err(format!(
                        "agent.ini [headers]: {name} is set by the agent itself"
                    ));
                }
                headers.insert(name, value);
            }
        }
        let timeout = match self.get("timeout")? {
            None => HTTP_DEFAULT_TIMEOUT,
            Some(raw) => raw
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("agent.ini timeout = {}: not a number", repr_str(&raw)))?,
        };
        if timeout < MIN_TIMEOUT {
            return Err(format!(
                "agent.ini timeout = {}: must be >= {} s (the server long-polls 25 s)",
                fmt_g(timeout),
                fmt_g(MIN_TIMEOUT)
            ));
        }
        // NaN compares false above, and an infinite or absurd value cannot become a Duration.
        if !timeout.is_finite() || std::time::Duration::try_from_secs_f64(timeout).is_err() {
            return Err(format!(
                "agent.ini timeout = {}: must be a finite number of seconds",
                fmt_g(timeout)
            ));
        }
        Ok(HttpSettings { headers, timeout })
    }
}

/// One entry of the agent.ini `printers` list, as registered with the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Printer {
    pub name: String,
    pub can_pdf: bool,
    pub target: String,
    pub capabilities: Option<Capabilities>,
}

impl Printer {
    pub fn new(name: &str, can_pdf: bool, target: &str) -> Printer {
        Printer {
            name: name.into(),
            can_pdf,
            target: target.into(),
            capabilities: None,
        }
    }

    /// The JSON the server's register endpoint takes (and Python's dict repr prints).
    pub fn to_json(&self) -> Value {
        let mut v = json!({"name": self.name, "can_pdf": self.can_pdf, "target": self.target});
        if let (Some(caps), Some(obj)) = (&self.capabilities, v.as_object_mut()) {
            obj.insert("capabilities".into(), caps.to_json());
        }
        v
    }
}

/// Best-effort printer capabilities: any subset of papers, bins, duplex, color.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    pub papers: Option<Vec<String>>,
    pub bins: Option<Vec<String>>,
    pub duplex: Option<bool>,
    pub color: Option<bool>,
}

impl Capabilities {
    pub fn is_empty(&self) -> bool {
        self.papers.is_none()
            && self.bins.is_none()
            && self.duplex.is_none()
            && self.color.is_none()
    }

    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(p) = &self.papers {
            m.insert("papers".into(), json!(p));
        }
        if let Some(b) = &self.bins {
            m.insert("bins".into(), json!(b));
        }
        if let Some(d) = self.duplex {
            m.insert("duplex".into(), json!(d));
        }
        if let Some(c) = self.color {
            m.insert("color".into(), json!(c));
        }
        Value::Object(m)
    }
}

/// `file:///srv/inbox` -> `/srv/inbox` (and `file:///C:/inbox` -> `C:\inbox` on Windows).
///
/// Mirrors Python's `urllib.request.url2pathname(target[len("file://"):])` (3.14): the remainder
/// is read as the rest of a `file:` URL, so `file://C:/x` also works, query/fragment are dropped,
/// escapes are decoded. A non-local authority (`file:////host/x`) is a UNC path on Windows and
/// an error elsewhere.
pub fn file_target_dir(target: &str) -> Result<String, String> {
    let rest = target
        .strip_prefix("file://")
        .unwrap_or_else(|| target.get(7..).unwrap_or(""));
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, mut path) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find('/').unwrap_or(after.len());
            (&after[..end], after[end..].to_string())
        }
        None => ("", rest.to_string()),
    };
    if cfg!(windows) {
        if authority.get(1..2) == Some(":") {
            path = format!("{authority}{path}"); // file://c:/file.txt
        } else if !is_local_authority(authority) {
            path = format!("//{authority}{path}"); // file://server/share/file.txt
        } else if path.starts_with("///") {
            path = path[1..].to_string();
        } else {
            let b = path.as_bytes();
            if b.first() == Some(&b'/') && matches!(b.get(2), Some(b':') | Some(b'|')) {
                path = path[1..].to_string(); // the extra slash before a DOS drive
            }
            if path.as_bytes().get(1) == Some(&b'|') {
                path = format!("{}:{}", &path[..1], &path[2..]); // older `C|` drive spelling
            }
        }
        path = path.replace('/', "\\");
    } else if !is_local_authority(authority) {
        return Err("file:// scheme is supported only on localhost".into());
    }
    Ok(percent_encoding::percent_decode_str(&path)
        .decode_utf8_lossy()
        .into_owned())
}

fn is_local_authority(authority: &str) -> bool {
    // ponytail: no DNS resolution of the authority; the machine's own name counts as local.
    authority.is_empty()
        || authority == "localhost"
        || std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .is_ok_and(|h| h == authority)
}

/// agent.ini `printers` (semicolon-separated) -> printers.
/// Grammar: `name [|pdf] [= target]`.
///   - no `=`  -> target is the name (a CUPS queue / Windows printer).
///   - `= socket://host:port` -> agent opens a raw TCP socket to it.
///   - `= file:///path/to/dir` -> agent writes the job into that directory (no paper).
///
/// Append `|pdf` to declare a document printer PDF-capable; default is raw-only so a label
/// printer is never auto-sent a PDF (gotcha #1). A socket:// target is always raw-only, a
/// file:// target always takes both (a directory cannot misrender anything).
pub fn parse_printers(spec: &str) -> Result<Vec<Printer>, String> {
    let mut out = Vec::new();
    for entry in spec.split(';') {
        let (left, target) = entry.split_once('=').unwrap_or((entry, ""));
        let (name, tag) = left.trim().split_once('|').unwrap_or((left.trim(), ""));
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let target = match target.trim() {
            "" => name,
            t => t,
        };
        let mut can_pdf = tag.trim().to_lowercase() == "pdf";
        if let Some(addr) = target.strip_prefix("socket://") {
            can_pdf = false; // gotcha #1: no renderer behind a bare socket
            let ok = addr.rsplit_once(':').is_some_and(|(host, port)| {
                !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
            });
            if !ok {
                return Err(format!(
                    "invalid socket printer target {} for {}: expected socket://host:port",
                    repr_str(target),
                    repr_str(name)
                ));
            }
        } else if target.starts_with("file://") {
            can_pdf = true; // a directory takes pdf and raw alike
            if file_target_dir(target)?.trim().is_empty() {
                return Err(format!(
                    "invalid file printer target {} for {}: expected file:///path/to/dir",
                    repr_str(target),
                    repr_str(name)
                ));
            }
        }
        out.push(Printer::new(name, can_pdf, target));
    }
    Ok(out)
}

/// Whether `printers` can spell this printer name: no `;`, `=` or `|` (they are the grammar), not
/// empty, no surrounding blanks (the parser trims them, so the round trip would change the name)
/// and no control characters (a newline would break the line).
pub fn expressible_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && !name.contains([';', '=', '|'])
        && !name.chars().any(char::is_control)
}

/// Printers -> the `printers` value, the inverse of [`parse_printers`]. Entries are joined with
/// ` ; `. `|pdf` is left out for `socket://` and `file://` targets because the parser sets it
/// itself. A name or target the grammar cannot hold is an error naming it.
pub fn format_printers(printers: &[Printer]) -> Result<String, String> {
    let mut entries = Vec::new();
    for p in printers {
        if !expressible_name(&p.name) {
            return Err(format!(
                "printer name {} cannot be written to agent.ini (no ;, = or | allowed)",
                repr_str(&p.name)
            ));
        }
        if p.target.is_empty() || p.target.contains(';') || p.target.chars().any(char::is_control) {
            return Err(format!(
                "target {} of printer {} cannot be written to agent.ini",
                repr_str(&p.target),
                repr_str(&p.name)
            ));
        }
        let implied = p.target.starts_with("socket://") || p.target.starts_with("file://");
        let mut entry = p.name.clone();
        if p.can_pdf && !implied {
            entry.push_str("|pdf");
        }
        if p.target != p.name {
            entry.push_str(" = ");
            entry.push_str(&p.target);
        }
        entries.push(entry);
    }
    Ok(entries.join(" ; "))
}

/// What [`update_ini`] sets. `None` leaves the key as it is; `printers` is always set. The values
/// are plain text: `update_ini` escapes `%` itself.
pub struct IniUpdate<'a> {
    pub server_url: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub name: Option<&'a str>,
    pub printers: &'a str,
}

/// `existing` with the `[agent]` keys of `update` set, as text. Works line by line: a replaced
/// key loses its indented continuation lines, a missing key is added at the end of `[agent]`
/// (the section is added when absent), and every other line - comments, `[headers]`, `timeout` -
/// stays as it is. `%` is doubled because `[agent]` reads through `BasicInterpolation`; line
/// breaks in a value become blanks so one value never becomes two lines.
pub fn update_ini(existing: Option<&str>, update: &IniUpdate) -> String {
    let mut wanted: Vec<(&str, String)> = Vec::new();
    for (key, value) in [
        ("server_url", update.server_url),
        ("api_key", update.api_key),
        ("name", update.name),
        ("printers", Some(update.printers)),
    ] {
        if let Some(v) = value {
            wanted.push((key, v.replace(['\r', '\n'], " ").replace('%', "%%")));
        }
    }
    let Some(existing) = existing else {
        let mut out = String::from("[agent]\n");
        for (key, v) in &wanted {
            out.push_str(&format!("{key} = {v}\n"));
        }
        return out;
    };

    let section_of = |line: &str| {
        let t = line.trim();
        t.strip_prefix('[')
            .and_then(|r| r.rsplit_once(']'))
            .map(|(name, _)| name.to_string())
    };
    let indent = |line: &str| line.chars().take_while(|c| c.is_whitespace()).count();
    let is_comment = |line: &str| line.trim().starts_with([';', '#']);
    let is_skippable = |line: &str| line.trim().is_empty() || is_comment(line);

    let lines: Vec<&str> = existing.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut done: Vec<&str> = Vec::new();
    // Index in `out` just after the last content line of `[agent]`, where new keys go.
    let mut agent_end: Option<usize> = None;
    let mut in_agent = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(section) = section_of(line) {
            in_agent = section == "agent";
            out.push(line.to_string());
            if in_agent {
                agent_end = Some(out.len());
            }
            i += 1;
            continue;
        }
        let Some(key_pos) = line
            .find(['=', ':'])
            .filter(|_| in_agent && !is_skippable(line))
        else {
            out.push(line.to_string());
            if in_agent && !is_skippable(line) {
                agent_end = Some(out.len());
            }
            i += 1;
            continue;
        };
        let key = line[..key_pos].trim().to_lowercase();
        let key_indent = indent(line);
        // The key line plus its continuation lines (more indented than the key). Blank and
        // comment lines inside the run belong to it only when a continuation follows them.
        let mut end = i + 1;
        let mut last = i;
        while end < lines.len() {
            if is_skippable(lines[end]) {
                end += 1;
            } else if indent(lines[end]) > key_indent {
                last = end;
                end += 1;
            } else {
                break;
            }
        }
        let run = &lines[i..=last];
        if let Some((k, v)) = wanted.iter().find(|(k, _)| *k == key) {
            out.push(format!("{k} = {v}"));
            done.push(k);
            // comments between the dropped lines survive
            out.extend(
                run[1..]
                    .iter()
                    .filter(|l| is_comment(l))
                    .map(|l| l.to_string()),
            );
        } else {
            out.extend(run.iter().map(|l| l.to_string()));
        }
        agent_end = Some(out.len());
        i = last + 1;
    }
    let missing: Vec<String> = wanted
        .iter()
        .filter(|(k, _)| !done.contains(k))
        .map(|(k, v)| format!("{k} = {v}"))
        .collect();
    match agent_end {
        Some(at) => {
            out.splice(at..at, missing);
        }
        None => {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push("[agent]".into());
            out.extend(missing);
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(extra: &str) -> Config {
        Config::from_text(&format!(
            "[agent]\nserver_url=https://x\napi_key=k\nprinters=p\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn test_parse_printers_pdf_is_opt_in_default_raw() {
        let ps = parse_printers("Zebra GK420d; HP LaserJet|pdf ; Office|PDF; ").unwrap();
        assert_eq!(
            ps,
            vec![
                Printer::new("Zebra GK420d", false, "Zebra GK420d"),
                Printer::new("HP LaserJet", true, "HP LaserJet"),
                Printer::new("Office", true, "Office"),
            ]
        );
    }

    #[test]
    fn test_parse_printers_socket_target() {
        let ps = parse_printers("Bixolon ; netz = socket://10.0.0.5:9100").unwrap();
        assert_eq!(
            ps,
            vec![
                Printer::new("Bixolon", false, "Bixolon"),
                Printer::new("netz", false, "socket://10.0.0.5:9100"),
            ]
        );
    }

    #[test]
    fn test_parse_printers_socket_is_forced_raw_only() {
        let ps = parse_printers("lbl|pdf = socket://10.0.0.5:9100").unwrap();
        assert_eq!(
            ps,
            vec![Printer::new("lbl", false, "socket://10.0.0.5:9100")]
        );
    }

    #[test]
    fn test_parse_printers_socket_missing_port_raises() {
        let e = parse_printers("netz = socket://10.0.0.5").unwrap_err();
        assert!(e.contains("socket"));
        assert_eq!(
            e,
            "invalid socket printer target 'socket://10.0.0.5' for 'netz': expected socket://host:port"
        );
        assert!(parse_printers("n = socket://:9100").is_err());
        assert!(parse_printers("n = socket://h:").is_err());
        assert!(parse_printers("n = socket://h:12x").is_err());
    }

    #[test]
    fn test_parse_printers_socket_valid_host_port_ok() {
        let ps = parse_printers("netz = socket://10.0.0.5:9100").unwrap();
        assert_eq!(
            ps,
            vec![Printer::new("netz", false, "socket://10.0.0.5:9100")]
        );
    }

    #[test]
    fn test_parse_printers_file_target_is_pdf_capable() {
        let ps = parse_printers("archive = file:///srv/paperless/inbox").unwrap();
        assert_eq!(
            ps,
            vec![Printer::new("archive", true, "file:///srv/paperless/inbox")]
        );
    }

    #[test]
    fn test_parse_printers_file_target_requires_a_path() {
        let e = parse_printers("archive = file://").unwrap_err();
        assert!(e.contains("file"));
        assert_eq!(
            e,
            "invalid file printer target 'file://' for 'archive': expected file:///path/to/dir"
        );
    }

    #[test]
    fn test_file_target_dir_strips_scheme_and_decodes_escapes() {
        assert!(
            file_target_dir("file:///srv/my%20inbox")
                .unwrap()
                .ends_with("my inbox")
        );
    }

    #[cfg(windows)]
    #[test]
    fn file_target_dir_matches_python_url2pathname_on_windows() {
        let cases = [
            ("file:///C:/inbox", "C:\\inbox"),
            ("file://C:/x", "C:\\x"),
            ("file:///srv/my%20inbox", "\\srv\\my inbox"),
            ("file:///", "\\"),
            ("file://host/share/x", "host\\share\\x"),
            ("file:///C|/x", "C:\\x"),
            ("file://localhost/C:/x", "localhost\\C:\\x"),
            ("file:////srv/x", "\\\\srv\\x"),
            ("file:///C:/in%20box/", "C:\\in box\\"),
            ("file://c|/x", "c:\\x"),
            ("file:///C:/x?y#z", "C:\\x"),
            ("file://///srv/share/x", "\\srv\\share\\x"),
            ("file:///a%2Fb", "\\a/b"),
            ("file://c:", "c:"),
            ("file:///C:", "C:"),
        ];
        for (url, want) in cases {
            assert_eq!(file_target_dir(url).unwrap(), want, "{url}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn file_target_dir_posix_decodes_and_refuses_remote_hosts() {
        assert_eq!(
            file_target_dir("file:///srv/my%20inbox").unwrap(),
            "/srv/my inbox"
        );
        assert!(file_target_dir("file:////host/x").is_err());
    }

    #[test]
    fn test_load_config_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let e = Config::load(dir.path()).unwrap_err();
        assert!(e.contains("agent.ini"));
        assert!(e.starts_with("missing or invalid "));
        assert!(e.ends_with(": need an [agent] section with server_url, api_key, printers"));
    }

    #[test]
    fn load_config_without_agent_section_is_the_same_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.ini"), "[other]\nk=v\n").unwrap();
        assert!(
            Config::load(dir.path())
                .unwrap_err()
                .starts_with("missing or invalid ")
        );
    }

    #[test]
    fn test_load_config_ok() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.ini"),
            "[agent]\nserver_url=http://x\napi_key=k\nprinters=p\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.require("server_url").unwrap(), "http://x");
        assert!(cfg.require("name").is_err());
    }

    #[test]
    fn load_config_reads_an_ansi_file_and_a_utf8_file() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("agent.ini");
        std::fs::write(&ini, b"[agent]\nprinters = B\xfcro|pdf\n").unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.require("printers").unwrap(), "B\u{fc}ro|pdf");
        std::fs::write(&ini, "[agent]\nprinters = B\u{fc}ro|pdf\n").unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.require("printers").unwrap(), "B\u{fc}ro|pdf");
        assert_eq!(decode_ini(b"\x80\x93"), "\u{20ac}\u{201c}");
    }

    #[test]
    fn load_config_tolerates_a_bom() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.ini"), "\u{feff}[agent]\nk=v\n").unwrap();
        assert!(Config::load(dir.path()).is_ok());
    }

    #[test]
    fn test_http_settings_reads_headers_section_and_timeout() {
        let cfg =
            cfg("timeout=45\n[headers]\nCF-Access-Client-Id = id1\nCF-Access-Client-Secret = s3\n");
        let s = cfg.http_settings().unwrap();
        let want: BTreeMap<String, String> = [
            ("cf-access-client-id".to_string(), "id1".to_string()),
            ("cf-access-client-secret".to_string(), "s3".to_string()),
        ]
        .into();
        assert_eq!(s.headers, want);
        assert_eq!(s.timeout, 45.0);
    }

    #[test]
    fn test_http_settings_defaults_without_section() {
        assert_eq!(
            cfg("").http_settings().unwrap(),
            HttpSettings {
                headers: BTreeMap::new(),
                timeout: 60.0
            }
        );
    }

    #[test]
    fn test_http_settings_rejects_timeout_below_long_poll() {
        let e = cfg("timeout=10\n").http_settings().unwrap_err();
        assert!(e.contains("timeout"));
        assert_eq!(
            e,
            "agent.ini timeout = 10: must be >= 30 s (the server long-polls 25 s)"
        );
    }

    #[test]
    fn http_settings_rejects_garbage_timeouts() {
        assert!(cfg("timeout=abc\n").http_settings().is_err());
        assert!(cfg("timeout=nan\n").http_settings().is_err());
        assert!(cfg("timeout=inf\n").http_settings().is_err());
        assert!(cfg("timeout=1e300\n").http_settings().is_err());
        assert_eq!(cfg("timeout=30\n").http_settings().unwrap().timeout, 30.0);
    }

    #[test]
    fn test_http_settings_refuses_to_override_the_agent_bearer() {
        let e = cfg("[headers]\nAuthorization = Bearer x\n")
            .http_settings()
            .unwrap_err();
        assert!(e.contains("set by the agent"));
        assert_eq!(
            e,
            "agent.ini [headers]: authorization is set by the agent itself"
        );
        assert!(cfg("[headers]\nUser-Agent = x\n").http_settings().is_err());
        assert!(
            cfg("[headers]\nContent-Type = x\n")
                .http_settings()
                .is_err()
        );
    }

    #[test]
    fn test_http_settings_keeps_percent_signs_in_secrets() {
        let s = cfg("[headers]\nX-Token = a%b\n").http_settings().unwrap();
        assert_eq!(s.headers.get("x-token").map(String::as_str), Some("a%b"));
    }

    #[test]
    fn http_settings_skips_default_section_keys_in_headers() {
        let c = Config::from_text(
            "[DEFAULT]\ntimeout = 50\nx-d = 1\n[agent]\nk=v\n[headers]\nx-h = 2\n",
        )
        .unwrap();
        let s = c.http_settings().unwrap();
        assert_eq!(s.headers.len(), 1);
        assert_eq!(s.timeout, 50.0);
    }

    #[test]
    fn agent_values_are_interpolated() {
        let c = Config::from_text("[agent]\nname = pc%%1\n").unwrap();
        assert_eq!(c.get("name").unwrap().as_deref(), Some("pc%1"));
        let bad = Config::from_text("[agent]\nname = pc%1\n").unwrap();
        assert!(bad.get("name").is_err());
    }

    #[test]
    fn printer_json_carries_capabilities_only_when_present() {
        let mut p = Printer::new("HP", true, "HP");
        assert_eq!(
            p.to_json(),
            json!({"name": "HP", "can_pdf": true, "target": "HP"})
        );
        p.capabilities = Some(Capabilities {
            papers: Some(vec!["A4".into()]),
            ..Default::default()
        });
        assert_eq!(p.to_json()["capabilities"], json!({"papers": ["A4"]}));
    }

    #[test]
    fn format_printers_round_trips() {
        let list = vec![
            Printer::new("Zebra GK420d", false, "Zebra GK420d"),
            Printer::new("HP LaserJet", true, "HP LaserJet"),
            Printer::new("Büro Drucker", true, "Büro Drucker"),
            Printer::new("warehouse", false, "socket://192.168.1.50:9100"),
            Printer::new("archive", true, "file:///srv/out"),
        ];
        assert_eq!(
            parse_printers(&format_printers(&list).unwrap()).unwrap(),
            list
        );
    }

    #[test]
    fn format_printers_refuses_what_the_grammar_cannot_hold() {
        assert!(
            format_printers(&[Printer::new("a;b", false, "a;b")])
                .unwrap_err()
                .contains("a;b")
        );
        assert!(
            !expressible_name("x=y") && !expressible_name("x|y") && expressible_name("Büro (EG)")
        );
        assert!(!expressible_name("") && !expressible_name(" x") && !expressible_name("x\ny"));
        assert!(format_printers(&[Printer::new("ok", false, "a;b")]).is_err());
    }

    #[test]
    fn format_printers_spells_the_documented_forms() {
        let list = vec![
            Printer::new("a", false, "a"),
            Printer::new("b", true, "b"),
            Printer::new("c", true, "queue"),
            Printer::new("d", false, "socket://h:9100"),
        ];
        assert_eq!(
            format_printers(&list).unwrap(),
            "a ; b|pdf ; c|pdf = queue ; d = socket://h:9100"
        );
    }

    #[test]
    fn update_ini_escapes_percent() {
        let text = update_ini(
            None,
            &IniUpdate {
                server_url: Some("https://x"),
                api_key: Some("ab%cd"),
                name: Some("pc"),
                printers: "50% Drucker",
            },
        );
        let cfg = Config::from_text(&text).unwrap();
        assert_eq!(cfg.require("api_key").unwrap(), "ab%cd");
        assert_eq!(cfg.require("printers").unwrap(), "50% Drucker");
    }

    #[test]
    fn update_ini_keeps_everything_else() {
        let old = "; note\n[agent]\nserver_url = https://x\napi_key = k\nname = pc\nprinters = a ;\n  b\ntimeout = 90\n\n[headers]\nCF-Access-Client-Id = a%b\n";
        let text = update_ini(
            Some(old),
            &IniUpdate {
                server_url: None,
                api_key: None,
                name: None,
                printers: "c",
            },
        );
        assert!(
            text.contains("; note")
                && text.contains("timeout = 90")
                && text.contains("CF-Access-Client-Id = a%b")
        );
        assert!(!text.contains("  b"));
        assert_eq!(
            Config::from_text(&text)
                .unwrap()
                .require("printers")
                .unwrap(),
            "c"
        );
    }

    #[test]
    fn update_ini_adds_missing_keys_and_a_missing_section() {
        let up = IniUpdate {
            server_url: Some("https://x"),
            api_key: None,
            name: None,
            printers: "p",
        };
        let text = update_ini(Some("[agent]\nnote = 1\n\n[headers]\nA = b\n"), &up);
        let cfg = Config::from_text(&text).unwrap();
        assert_eq!(cfg.require("server_url").unwrap(), "https://x");
        assert_eq!(cfg.require("printers").unwrap(), "p");
        assert!(text.find("printers = p").unwrap() < text.find("[headers]").unwrap());
        assert!(text.contains("A = b"));
        let text = update_ini(Some("[other]\nk = v\n"), &up);
        let cfg = Config::from_text(&text).unwrap();
        assert_eq!(cfg.require("printers").unwrap(), "p");
        assert!(text.contains("[other]\nk = v"));
    }

    #[test]
    fn update_ini_leaves_keys_of_other_sections_and_continuations_alone() {
        let old = "[agent]\nname = pc\nprinters = a ;\n  name = b\n[headers]\nprinters = keep\n";
        let text = update_ini(
            Some(old),
            &IniUpdate {
                server_url: None,
                api_key: None,
                name: Some("new"),
                printers: "c",
            },
        );
        assert!(text.contains("[headers]\nprinters = keep"));
        let cfg = Config::from_text(&text).unwrap();
        assert_eq!(cfg.require("name").unwrap(), "new");
        assert_eq!(cfg.require("printers").unwrap(), "c");
    }

    #[test]
    fn update_ini_keeps_a_value_on_one_line() {
        let text = update_ini(
            None,
            &IniUpdate {
                server_url: None,
                api_key: Some("a\nprinters = evil"),
                name: None,
                printers: "p",
            },
        );
        assert_eq!(
            Config::from_text(&text)
                .unwrap()
                .require("printers")
                .unwrap(),
            "p"
        );
    }
}
