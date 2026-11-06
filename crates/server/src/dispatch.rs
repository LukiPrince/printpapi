// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Job-body validation and payload fetching — the port of `app/dispatch.py`. Pure except for
//! `HttpFetcher`; everything else takes an injectable `Fetcher`.
//!
//! Error messages match the Python f-strings exactly (`crate::pyfmt::repr` for `{x!r}`).
use crate::pyfmt::{repr, truthy};
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde_json::{Map, Value};
use std::io::Read;
use std::time::Duration;

/// Browser-like UA: WAF/CDN-fronted services (e.g. Cloudflare) reject a library default UA with
/// 403 (gotcha #3). Sent on every outbound request.
pub const USER_AGENT: &str = "Mozilla/5.0 (print-api)";
/// ponytail: flat cap so one job can't spool 10k prints.
pub const MAX_COPIES: i64 = 100;
pub const MAX_EXPIRE_AFTER: i64 = 30 * 24 * 3600;
pub const MAX_IDEMPOTENCY_KEY: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub enum DispatchError {
    /// Client error -> 400 with the message.
    Invalid(String),
    /// The payload URL could not be fetched -> 502 (`downstream: {msg}`).
    Fetch(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::Invalid(m) | DispatchError::Fetch(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for DispatchError {}

/// Outbound HTTP, injectable so tests never touch the network.
pub trait Fetcher: Send + Sync {
    /// GET `url`, the response body on 2xx. Err carries a urllib-style message
    /// (`HTTP Error 404: Not Found`, or the connection error text).
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
    /// POST `body` as JSON, the response body on 2xx.
    fn post_json(&self, url: &str, body: &Value) -> Result<Vec<u8>, String>;
}

// ponytail: the Python had no cap on fetched bodies; 100 MB stops a hostile URL from exhausting RAM.
const MAX_BODY: u64 = 100 * 1024 * 1024;

/// The real thing: ureq, `USER_AGENT`, a timeout per socket operation like Python's (30 s for
/// payload fetches, 10 s for the webhook dispatcher), redirects followed, system trust store.
pub struct HttpFetcher {
    agent: ureq::Agent,
    /// Requests in flight / the cap. Each one pins a thread of the server's blocking pool, so
    /// slow upstreams must not be able to take them all (deliberate deviation from the Python
    /// server, which had a thread per request and so no shared pool to exhaust).
    in_flight: std::sync::atomic::AtomicUsize,
    max_in_flight: usize,
}

/// Holds one in-flight slot of an `HttpFetcher`.
struct Slot<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Outbound payload fetches in flight at once. Each one waits on a blocking thread, so the cap is
/// what keeps a client pointing many jobs at a tarpit URL from taking every thread the server has
/// (main sizes the pool at twice this). ponytail: one global cap, not per org — a single tenant
/// can still use all of it, it just cannot stall /health, logins or agent claims any more.
pub const OUTBOUND_IN_FLIGHT: usize = 256;

/// Where outbound requests (payload URLs, job callbacks, org event URLs) may connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destinations {
    /// Anywhere, the LAN included: a self-hosted box fetching PDFs from an internal renderer.
    Any,
    /// Public addresses only. A server whose tenants are strangers needs this, or a job could
    /// make it fetch its own admin port, the cloud metadata service or the LAN and print the
    /// answer on the tenant's printer. Checked on the resolved addresses of every hop (redirects
    /// included) and the connection goes to exactly the checked address, so DNS rebinding cannot
    /// slip a private one in between.
    PublicOnly,
}

/// Whether a stranger's job may make the server connect to `ip`.
pub fn is_public(ip: std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr};
    fn v4(ip: Ipv4Addr) -> bool {
        let o = ip.octets();
        !(ip.is_private()
            || ip.is_loopback()
            || ip.is_link_local() // 169.254/16, the cloud metadata service included
            || ip.is_unspecified()
            || ip.is_broadcast()
            || ip.is_multicast()
            || ip.is_documentation()
            || o[0] == 0 // 0.0.0.0/8 "this network"
            || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64/10 carrier-grade NAT
            // 192.0.0.0/24 protocol assignments, except the two global anycast ones (PCP, TURN)
            || (o[0] == 192 && o[1] == 0 && o[2] == 0 && o[3] != 9 && o[3] != 10)
            || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18/15 benchmarking
            || o[0] >= 240) // 240/4 reserved
    }
    match ip {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return v4(mapped); // ::ffff:a.b.c.d
            }
            let s = ip.segments();
            let embedded = |hi: u16, lo: u16| v4(Ipv4Addr::from(((hi as u32) << 16) | lo as u32));
            // The IANA special-purpose registry's non-global entries, plus every form that
            // carries an IPv4 address (judged by that address) or tunnels to one.
            match s {
                [0, 0, 0, 0, 0, 0, _, _] => false, // ::, ::1 and IPv4-compatible ::a.b.c.d
                [0, 0, 0, 0, 0xffff, 0, hi, lo] => embedded(hi, lo), // SIIT ::ffff:0:a.b.c.d
                [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] => embedded(hi, lo), // NAT64 well-known
                [0x64, 0xff9b, 1, ..] => false,    // 64:ff9b:1::/48 local-use NAT64
                [0x2002, hi, lo, ..] => embedded(hi, lo), // 6to4
                [0x2001, 0, ..] => false, // 2001::/32 Teredo (tunnels via an embedded IPv4)
                [0x100, 0, 0, 0, ..] => false, // 100::/64 discard-only
                [0x2001, 2, 0, ..] => false, // 2001:2::/48 benchmarking
                [0x2001, x, ..] if (0x10..=0x2f).contains(&x) => false, // ORCHID(v2) 2001:10::/27
                [0x2001, 0x0db8, ..] => false, // 2001:db8::/32 documentation
                [x, ..] if (x & 0xfff0) == 0x3ff0 => false, // 3fff::/20 documentation
                [0x5f00, ..] => false,    // 5f00::/16 SRv6 SIDs
                _ => {
                    !(ip.is_multicast()
                        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                        || (s[0] & 0xffc0) == 0xfec0) // fec0::/10 site-local (deprecated)
                }
            }
        }
    }
}

/// The default resolver with every non-public address dropped (see `Destinations::PublicOnly`).
#[derive(Debug, Default)]
struct PublicOnly(ureq::unversioned::resolver::DefaultResolver);

impl ureq::unversioned::resolver::Resolver for PublicOnly {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let all = self.0.resolve(uri, config, timeout)?;
        let mut public = self.empty();
        for addr in all.iter().filter(|a| is_public(a.ip())) {
            public.push(*addr);
        }
        if public.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "destination not allowed: {} is not a public address",
                    uri.host().unwrap_or("")
                ),
            )));
        }
        Ok(public)
    }
}

impl HttpFetcher {
    /// Payload fetcher: `timeout` per socket operation, 120 s overall, OUTBOUND_IN_FLIGHT at a time.
    pub fn new(timeout: Duration) -> Self {
        Self::with_limits(timeout, Duration::from_secs(120), OUTBOUND_IN_FLIGHT)
    }

    fn slot(&self) -> Result<Slot<'_>, String> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.in_flight.fetch_add(1, SeqCst) >= self.max_in_flight {
            self.in_flight.fetch_sub(1, SeqCst);
            return Err("too many concurrent outbound requests, try again later".into());
        }
        Ok(Slot(&self.in_flight))
    }

    /// `timeout` per socket operation, `global` for the whole request (a trickling upstream
    /// resets the per-operation one forever), at most `max_in_flight` requests at a time.
    pub fn with_limits(timeout: Duration, global: Duration, max_in_flight: usize) -> Self {
        Self::with_limits_to(timeout, global, max_in_flight, Destinations::Any)
    }

    /// `with_limits`, connecting only where `destinations` allows.
    pub fn with_limits_to(
        timeout: Duration,
        global: Duration,
        max_in_flight: usize,
        destinations: Destinations,
    ) -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(timeout))
            .timeout_per_read(Some(timeout))
            .timeout_per_write(Some(timeout))
            .timeout_global(Some(global))
            .tls_config(tls);
        let agent = match destinations {
            Destinations::Any => ureq::Agent::new_with_config(config.build()),
            // No proxy from the environment here: the proxy would resolve the target itself,
            // past the address check. Egress through a proxy is that proxy's policy to enforce.
            Destinations::PublicOnly => ureq::Agent::with_parts(
                config.proxy(None).build(),
                ureq::unversioned::transport::DefaultConnector::new(),
                PublicOnly::default(),
            ),
        };
        HttpFetcher {
            agent,
            in_flight: std::sync::atomic::AtomicUsize::new(0),
            max_in_flight,
        }
    }
}

/// urllib's `HTTPError` text for a non-2xx status.
fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(code) => {
            let reason = ureq::http::StatusCode::from_u16(code)
                .ok()
                .and_then(|s| s.canonical_reason())
                .unwrap_or("");
            if reason.is_empty() {
                format!("HTTP Error {code}")
            } else {
                format!("HTTP Error {code}: {reason}")
            }
        }
        other => other.to_string(),
    }
}

fn read_body(mut resp: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, String> {
    // urllib fails every non-2xx; ureq only errors on 4xx/5xx (a 304 would be an empty payload).
    if !resp.status().is_success() {
        return Err(describe(ureq::Error::StatusCode(resp.status().as_u16())));
    }
    let mut out = Vec::new();
    resp.body_mut()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > MAX_BODY {
        return Err("response body too large".into());
    }
    Ok(out)
}

/// What urllib does to a URL before using it (`Request.full_url`/`unwrap`): Python `str.strip()`,
/// then a `<...>` wrapper and a leading `URL:` are removed. Also percent-encodes the few ASCII
/// characters Python sends raw but `http::Uri` refuses.
fn normalize_url(url: &str) -> String {
    let mut u = url.trim_matches(py_space);
    if let Some(inner) = u.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
        u = inner.trim_matches(py_space);
    }
    let u = u.strip_prefix("URL:").unwrap_or(u).trim_matches(py_space);
    let mut out = String::with_capacity(u.len());
    for c in u.chars() {
        if matches!(c, '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^' | '"') {
            out.push_str(&format!("%{:02X}", c as u8));
        } else {
            out.push(c);
        }
    }
    out
}

/// Python's `str.isspace()` plus the `-` separators `str.strip()` also removes.
fn py_space(c: char) -> bool {
    c.is_whitespace() || (''..='').contains(&c)
}

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let _slot = self.slot()?;
        let resp = self
            .agent
            .get(normalize_url(url))
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(describe)?;
        read_body(resp)
    }
    fn post_json(&self, url: &str, body: &Value) -> Result<Vec<u8>, String> {
        let _slot = self.slot()?;
        let data = crate::pyfmt::dumps(body).into_bytes();
        let resp = self
            .agent
            .post(normalize_url(url))
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/json")
            .send(data)
            .map_err(describe)?;
        read_body(resp)
    }
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, DispatchError> {
    Err(DispatchError::Invalid(msg.into()))
}

/// The URL scheme as `urllib.parse.urlparse` sees it (lowercased; "" when there is none).
pub fn url_scheme(url: &str) -> String {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let Some(i) = url.find(':') else {
        return String::new();
    };
    let head = &url[..i];
    let mut chars = head.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return String::new(),
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
        head.to_ascii_lowercase()
    } else {
        String::new()
    }
}

fn is_http(url: &str) -> bool {
    matches!(url_scheme(url).as_str(), "http" | "https")
}

// A non-string truthy url gets the scheme error with its repr (Python crashed in urlparse).
fn checked_http_url(url: Option<&Value>) -> Result<&str, DispatchError> {
    match url {
        Some(v) if truthy(v) => match v {
            Value::String(s) if is_http(s) => Ok(s),
            other => invalid(format!("unsupported url scheme: {}", repr(other))),
        },
        _ => invalid("missing 'url'"),
    }
}

/// `base64.b64decode(validate=True)`: standard alphabet, canonical padding, no stray characters,
/// but non-zero trailing bits are accepted (Python does not check them).
const B64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

/// The `binascii.Error` text Python's strict `a2b_base64` gives for input `B64` rejected.
fn py_b64_error(s: &str) -> String {
    if !s.is_ascii() {
        return "string argument should contain only ASCII characters".into();
    }
    let b = s.as_bytes();
    if b.first() == Some(&b'=') {
        return "Leading padding not allowed".into();
    }
    let (mut quad, mut pads, mut padding_started, mut data) = (0usize, 0usize, false, 0usize);
    for (i, &c) in b.iter().enumerate() {
        if c == b'=' {
            padding_started = true;
            pads += 1;
            if quad >= 2 && quad + pads >= 4 {
                if i + 1 < b.len() {
                    return "Excess data after padding".into();
                }
                return "Excess padding not allowed".into();
            }
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/') {
            return "Only base64 data is allowed".into();
        }
        if padding_started {
            return "Discontinuous padding not allowed".into();
        }
        pads = 0;
        data += 1;
        quad = (quad + 1) % 4;
    }
    match quad {
        0 => "Excess padding not allowed".into(),
        1 => format!(
            "Invalid base64-encoded string: number of data characters ({data}) cannot be 1 more than a multiple of 4"
        ),
        _ => "Incorrect padding".into(),
    }
}

/// The job payload: base64 content, a fetched URL, or a POSTed `json` body's response.
pub fn decode_payload(
    body: &Map<String, Value>,
    fetcher: &dyn Fetcher,
) -> Result<Vec<u8>, DispatchError> {
    let t = body.get("type");
    match t.and_then(Value::as_str) {
        Some("raw_base64" | "pdf_base64" | "label_base64") => match body.get("content") {
            Some(Value::String(s)) if !s.is_empty() => B64
                .decode(s)
                .or_else(|_| invalid(format!("bad base64: {}", py_b64_error(s)))),
            Some(v) if truthy(v) => {
                // Python raised TypeError here (no response); a clean 400 instead.
                invalid(format!("bad base64: expected a string, got {}", repr(v)))
            }
            _ => invalid("missing 'content'"),
        },
        Some("raw_uri" | "pdf_uri" | "label_uri") => {
            let url = checked_http_url(body.get("url"))?;
            fetcher
                .get(url)
                .map_err(|e| DispatchError::Fetch(format!("fetch failed: {e}")))
        }
        // *_uri_post: the server POSTs `json` to `url` (e.g. a PDF-rendering service) and prints
        // the response bytes. Avoids round-tripping binary through the caller.
        Some("raw_uri_post" | "pdf_uri_post") => {
            let url = checked_http_url(body.get("url"))?;
            let json = body
                .get("json")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            fetcher
                .post_json(url, &json)
                .map_err(|e| DispatchError::Fetch(format!("post fetch failed: {e}")))
        }
        _ => invalid(format!(
            "unknown type: {}",
            t.map_or_else(|| "None".to_string(), repr)
        )),
    }
}

/// Which template a `label_*` job names: a string is a name, an integer is an id.
#[derive(Debug, Clone, PartialEq)]
pub enum TemplateRef {
    Name(String),
    Id(i64),
}

impl std::fmt::Display for TemplateRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateRef::Name(n) => f.write_str(n),
            TemplateRef::Id(i) => write!(f, "{i}"),
        }
    }
}

/// The label-specific fields of a `label_uri` / `label_base64` / `label_text` job. `text` is
/// `Some` only for `label_text`; the other two carry their image in `url` / `content`, which
/// `decode_payload` reads.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelJob {
    pub template: TemplateRef,
    pub text: Option<String>,
}

/// `Ok(None)` for every other job type. Pure field checks, so a bad job 400s before the printer,
/// the template or the network are touched.
pub fn parse_label_job(body: &Map<String, Value>) -> Result<Option<LabelJob>, DispatchError> {
    let t = body.get("type").and_then(Value::as_str);
    if !matches!(t, Some("label_uri" | "label_base64" | "label_text")) {
        return Ok(None);
    }
    let template = match body.get("template") {
        None | Some(Value::Null) => return invalid("missing 'template'"),
        Some(Value::String(s)) if !s.is_empty() => TemplateRef::Name(s.clone()),
        Some(Value::Number(n)) if n.is_i64() => TemplateRef::Id(n.as_i64().unwrap_or_default()),
        Some(_) => return invalid("template must be a name or an integer id"),
    };
    let text = if t == Some("label_text") {
        match body.get("text") {
            None | Some(Value::Null) => return invalid("missing 'text'"),
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return invalid("text must be a string"),
        }
    } else {
        None
    };
    Ok(Some(LabelJob { template, text }))
}

/// "pdf" for the pdf_* types, "raw" for everything else (unknown types included).
pub fn agent_mode(type_: &Value) -> &'static str {
    match type_.as_str() {
        Some("pdf_base64" | "pdf_uri_post" | "pdf_uri") => "pdf",
        _ => "raw",
    }
}

/// Optional 'copies' -> 1..=MAX_COPIES (default 1). Only a JSON integer passes: bool, float and
/// string are rejected at the trust boundary.
pub fn parse_copies(body: &Map<String, Value>) -> Result<i64, DispatchError> {
    match body.get("copies") {
        None | Some(Value::Null) => Ok(1),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(c) if (1..=MAX_COPIES).contains(&c) => Ok(c),
            _ => invalid(format!("copies must be an integer 1..{MAX_COPIES}")),
        },
        Some(_) => invalid(format!("copies must be an integer 1..{MAX_COPIES}")),
    }
}

const OPTION_KEYS: [&str; 5] = ["duplex", "paper", "bin", "color", "pages"];
const OPTION_DUPLEX: [&str; 3] = ["long-edge", "short-edge", "one-sided"];

/// `^[\w. -]{1,64}$` — no comma: paper/bin land in Sumatra's comma-separated -print-settings
/// list, so a comma in a value would smuggle extra settings past validation.
// Python's `$` also matched before a trailing newline; here it is rejected.
pub(crate) fn valid_option_value(s: &str) -> bool {
    let n = s.chars().count();
    (1..=64).contains(&n)
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ' ' | '-'))
}

/// `^\d+(-\d+)?(,\d+(-\d+)?)*$` with ASCII digits only.
fn valid_pages(s: &str) -> bool {
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    s.split(',').all(|range| {
        let mut parts = range.split('-');
        let (a, b, c) = (parts.next(), parts.next(), parts.next());
        c.is_none() && a.is_some_and(digits) && b.is_none_or(digits)
    })
}

/// Optional 'options' -> validated map, or None if absent/empty. pdf jobs only — raw payloads
/// (ZPL/ESC-POS) carry their own layout, a renderer never sees them. The agent maps these onto
/// SumatraPDF -print-settings (Windows) or lp -o (CUPS).
pub fn parse_options(
    body: &Map<String, Value>,
    mode: &str,
) -> Result<Option<Map<String, Value>>, DispatchError> {
    let o = match body.get("options") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(o)) => o,
        Some(_) => return invalid("options must be an object"),
    };
    if o.is_empty() {
        return Ok(None);
    }
    if mode != "pdf" {
        return invalid("options are only supported on pdf jobs");
    }
    let mut unknown: Vec<&str> = o
        .keys()
        .map(String::as_str)
        .filter(|k| !OPTION_KEYS.contains(k))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        return invalid(format!("unknown option(s): {}", unknown.join(", ")));
    }
    if let Some(d) = o.get("duplex")
        && !d.as_str().is_some_and(|d| OPTION_DUPLEX.contains(&d))
    {
        return invalid(format!(
            "duplex must be one of {}",
            OPTION_DUPLEX.join(", ")
        ));
    }
    for k in ["paper", "bin"] {
        if let Some(v) = o.get(k)
            && !v.as_str().is_some_and(valid_option_value)
        {
            return invalid(format!(
                "{k} must be 1-64 chars of letters/digits/space/._-"
            ));
        }
    }
    if let Some(c) = o.get("color")
        && !c.is_boolean()
    {
        return invalid("color must be a boolean");
    }
    if let Some(p) = o.get("pages")
        && !p.as_str().is_some_and(valid_pages)
    {
        return invalid("pages must be ranges like '1-3,5'");
    }
    Ok(Some(o.clone()))
}

/// Optional 'expire_after' seconds, or None (never expires). A job past its deadline is failed
/// instead of printed — a stale shipping label must not come out of the printer hours later when
/// an offline agent reconnects.
pub fn parse_expire_after(body: &Map<String, Value>) -> Result<Option<i64>, DispatchError> {
    match body.get("expire_after") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(v) if (1..=MAX_EXPIRE_AFTER).contains(&v) => Ok(Some(v)),
            _ => invalid(format!(
                "expire_after must be an integer 1..{MAX_EXPIRE_AFTER} (seconds)"
            )),
        },
        Some(_) => invalid(format!(
            "expire_after must be an integer 1..{MAX_EXPIRE_AFTER} (seconds)"
        )),
    }
}

/// Optional 'idempotency_key' -> trimmed string, or None. Resubmitting the same key in the same
/// org returns the original job instead of printing again (safe POST /jobs retries).
pub fn parse_idempotency_key(body: &Map<String, Value>) -> Result<Option<String>, DispatchError> {
    match body.get("idempotency_key") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= MAX_IDEMPOTENCY_KEY => {
            // Python's str.strip() also strips the \x1c-\x1f separators.
            let t = s.trim_matches(py_space);
            Ok((!t.is_empty()).then(|| t.to_string()))
        }
        Some(_) => invalid(format!(
            "idempotency_key must be a string of at most {MAX_IDEMPOTENCY_KEY} chars"
        )),
    }
}

/// Optional 'callback_url' -> validated http(s) URL, or None if absent/empty. The server POSTs
/// job-state changes here (see the webhook dispatcher).
// ponytail: no private-IP/SSRF block — same authenticated-client trust as raw_uri/pdf_uri_post;
// add an IP-range denylist here if untrusted clients ever get keys.
pub fn parse_callback_url(body: &Map<String, Value>) -> Result<Option<String>, DispatchError> {
    let Some(v) = body.get("callback_url").filter(|v| truthy(v)) else {
        return Ok(None);
    };
    let Value::String(url) = v else {
        return invalid(format!("callback_url must be a string: {}", repr(v)));
    };
    if !is_http(url) {
        return invalid(format!("callback_url must be http(s): {}", repr(v)));
    }
    check_netloc_brackets(url)?;
    Ok(Some(url.clone()))
}

/// `urlparse` raises ValueError for a malformed bracketed host; Python crashed on it (no job),
/// so a 400 here. ponytail: the NFKC-unsafe-netloc check (fullwidth `#` etc.) is not ported.
fn check_netloc_brackets(url: &str) -> Result<(), DispatchError> {
    let u: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let Some(rest) = u.split_once("//").map(|x| x.1) else {
        return Ok(());
    };
    let netloc = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (open, close) = (netloc.contains('['), netloc.contains(']'));
    if open != close {
        return invalid("Invalid IPv6 URL");
    }
    if open {
        // host must be `[...]` followed by nothing or `:port`; userinfo may precede it
        let host_part = netloc.rsplit_once('@').map_or(netloc, |x| x.1);
        let (Some(a), Some(z)) = (host_part.find('['), host_part.find(']')) else {
            return invalid("Invalid IPv6 URL");
        };
        if a != 0 || z < a || (host_part.len() > z + 1 && !host_part[z + 1..].starts_with(':')) {
            return invalid("Invalid IPv6 URL");
        }
        let inner = &host_part[1..z];
        if inner.parse::<std::net::Ipv4Addr>().is_ok() {
            return invalid("An IPv4 address cannot be in brackets");
        }
        if inner.parse::<std::net::Ipv6Addr>().is_err() && !inner.starts_with(['v', 'V']) {
            return invalid("Invalid IPv6 URL");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().expect("test body is an object")
    }

    /// Records calls, answers with canned bytes or a canned error.
    struct Fake {
        calls: Mutex<Vec<(String, Option<Value>)>>,
        reply: Result<Vec<u8>, String>,
    }
    impl Fake {
        fn ok(b: &[u8]) -> Self {
            Fake {
                calls: Mutex::new(vec![]),
                reply: Ok(b.to_vec()),
            }
        }
        fn err(e: &str) -> Self {
            Fake {
                calls: Mutex::new(vec![]),
                reply: Err(e.into()),
            }
        }
    }
    impl Fetcher for Fake {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.calls.lock().unwrap().push((url.into(), None));
            self.reply.clone()
        }
        fn post_json(&self, url: &str, body: &Value) -> Result<Vec<u8>, String> {
            self.calls
                .lock()
                .unwrap()
                .push((url.into(), Some(body.clone())));
            self.reply.clone()
        }
    }

    fn dp(v: Value) -> Result<Vec<u8>, DispatchError> {
        decode_payload(&obj(v), &Fake::ok(b""))
    }
    fn is_invalid<T: std::fmt::Debug>(r: Result<T, DispatchError>) -> String {
        match r {
            Err(DispatchError::Invalid(m)) => m,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn raw_base64_decodes() {
        assert_eq!(
            dp(json!({"type": "raw_base64", "content": "XlhBXlha"})).unwrap(),
            b"^XA^XZ"
        );
    }

    #[test]
    fn pdf_base64_decodes() {
        assert_eq!(
            dp(json!({"type": "pdf_base64", "content": "JVBERi0xLjQ="})).unwrap(),
            b"%PDF-1.4"
        );
    }

    #[test]
    fn raw_uri_uses_injected_fetcher() {
        let f = Fake::ok(b"ZPLBYTES");
        let r = decode_payload(
            &obj(json!({"type": "raw_uri", "url": "https://x/y.zpl"})),
            &f,
        );
        assert_eq!(r.unwrap(), b"ZPLBYTES");
        assert_eq!(
            f.calls.lock().unwrap()[0],
            ("https://x/y.zpl".to_string(), None)
        );
    }

    #[test]
    fn unknown_type_raises() {
        assert_eq!(
            is_invalid(dp(json!({"type": "nope"}))),
            "unknown type: 'nope'"
        );
        assert_eq!(is_invalid(dp(json!({}))), "unknown type: None");
        assert_eq!(is_invalid(dp(json!({"type": 5}))), "unknown type: 5");
    }

    #[test]
    fn missing_content_raises() {
        assert_eq!(
            is_invalid(dp(json!({"type": "raw_base64"}))),
            "missing 'content'"
        );
        assert_eq!(
            is_invalid(dp(json!({"type": "raw_base64", "content": ""}))),
            "missing 'content'"
        );
    }

    #[test]
    fn bad_base64_raises() {
        let m = is_invalid(dp(
            json!({"type": "raw_base64", "content": "!!!notbase64!!!"}),
        ));
        assert!(m.starts_with("bad base64: "), "{m}");
    }

    #[test]
    fn base64_matches_python_validate_true() {
        let ok = |c: &str| dp(json!({"type": "raw_base64", "content": c}));
        // whitespace and missing/extra padding are rejected under validate=True
        for bad in [
            "YQ==\n",
            "Y Q==",
            "YQ",
            "YQ=",
            "YQ===",
            "YQ==YQ==",
            "YWJj\nZA==",
        ] {
            assert!(ok(bad).is_err(), "{bad:?}");
        }
        // non-zero trailing bits are accepted (Python does not check them)
        assert_eq!(ok("YR==").unwrap(), b"a");
        assert_eq!(ok("YQ==").unwrap(), b"a");
    }

    #[test]
    fn non_string_content_is_a_clean_error() {
        let m = is_invalid(dp(json!({"type": "raw_base64", "content": 5})));
        assert!(m.starts_with("bad base64: "), "{m}");
        assert!(dp(json!({"type": "raw_base64", "content": [1]})).is_err());
    }

    // --- label jobs -------------------------------------------------------------------------

    #[test]
    fn label_uri_and_base64_decode_like_their_raw_twins() {
        let f = Fake::ok(b"IMG");
        let r = decode_payload(
            &obj(json!({"type": "label_uri", "url": "https://x/i.png"})),
            &f,
        );
        assert_eq!(r.unwrap(), b"IMG");
        assert_eq!(f.calls.lock().unwrap()[0].0, "https://x/i.png");
        assert_eq!(
            dp(json!({"type": "label_base64", "content": "XlhBXlha"})).unwrap(),
            b"^XA^XZ"
        );
        assert_eq!(
            is_invalid(dp(
                json!({"type": "label_uri", "url": "file:///etc/passwd"})
            )),
            "unsupported url scheme: 'file:///etc/passwd'"
        );
        assert_eq!(
            is_invalid(dp(json!({"type": "label_uri"}))),
            "missing 'url'"
        );
        assert_eq!(
            is_invalid(dp(json!({"type": "label_base64"}))),
            "missing 'content'"
        );
        assert!(
            is_invalid(dp(json!({"type": "label_base64", "content": "!!!"})))
                .starts_with("bad base64: ")
        );
        let down = Fake::err("HTTP Error 404: Not Found");
        let r = decode_payload(
            &obj(json!({"type": "label_uri", "url": "https://x"})),
            &down,
        );
        assert_eq!(
            r,
            Err(DispatchError::Fetch(
                "fetch failed: HTTP Error 404: Not Found".into()
            ))
        );
    }

    #[test]
    fn label_text_has_no_payload_to_decode() {
        // label_text is rendered from the body itself; decode_payload is not its path
        assert!(dp(json!({"type": "label_text", "text": "x", "template": "t"})).is_err());
    }

    #[test]
    fn label_job_fields() {
        let job = |v: Value| parse_label_job(&obj(v));
        assert_eq!(job(json!({"type": "raw_uri"})), Ok(None));
        assert_eq!(job(json!({})), Ok(None));
        let j = job(json!({"type": "label_uri", "template": "ship", "url": "https://x"}))
            .unwrap()
            .unwrap();
        assert_eq!(
            (j.template, j.text),
            (TemplateRef::Name("ship".into()), None)
        );
        let j = job(json!({"type": "label_base64", "template": 7}))
            .unwrap()
            .unwrap();
        assert_eq!(j.template, TemplateRef::Id(7));
        let j = job(json!({"type": "label_text", "template": "t", "text": "Hi ^"}))
            .unwrap()
            .unwrap();
        assert_eq!(j.text.as_deref(), Some("Hi ^"));
        assert_eq!(j.template.to_string(), "t");
        assert_eq!(TemplateRef::Id(7).to_string(), "7");
        for bad in [
            json!({"type": "label_uri"}),
            json!({"type": "label_uri", "template": ""}),
            json!({"type": "label_uri", "template": 1.5}),
            json!({"type": "label_uri", "template": true}),
            json!({"type": "label_uri", "template": ["a"]}),
            json!({"type": "label_text", "template": "t"}),
            json!({"type": "label_text", "template": "t", "text": 5}),
        ] {
            assert!(
                matches!(job(bad.clone()), Err(DispatchError::Invalid(_))),
                "{bad}"
            );
        }
        assert_eq!(
            is_invalid(job(json!({"type": "label_uri"}))),
            "missing 'template'"
        );
        assert_eq!(
            is_invalid(job(json!({"type": "label_text", "template": "t"}))),
            "missing 'text'"
        );
    }

    #[test]
    fn agent_mode_by_type() {
        assert_eq!(agent_mode(&json!("pdf_base64")), "pdf");
        assert_eq!(agent_mode(&json!("raw_base64")), "raw");
        assert_eq!(agent_mode(&json!("raw_uri")), "raw");
        assert_eq!(agent_mode(&json!(null)), "raw");
        assert_eq!(agent_mode(&json!(["pdf_uri"])), "raw");
    }

    #[test]
    fn missing_url_raises() {
        for t in ["raw_uri", "pdf_uri", "raw_uri_post", "pdf_uri_post"] {
            assert_eq!(is_invalid(dp(json!({"type": t}))), "missing 'url'");
            assert_eq!(
                is_invalid(dp(json!({"type": t, "url": ""}))),
                "missing 'url'"
            );
            assert_eq!(
                is_invalid(dp(json!({"type": t, "url": 0}))),
                "missing 'url'"
            );
        }
    }

    #[test]
    fn fetch_failure_is_fetcherror() {
        let f = Fake::err("unreachable");
        let r = decode_payload(&obj(json!({"type": "raw_uri", "url": "https://x"})), &f);
        assert_eq!(
            r,
            Err(DispatchError::Fetch("fetch failed: unreachable".into()))
        );
    }

    #[test]
    fn bad_scheme_raises_dispatcherror() {
        let m = is_invalid(dp(json!({"type": "raw_uri", "url": "file:///etc/passwd"})));
        assert_eq!(m, "unsupported url scheme: 'file:///etc/passwd'");
        // truthy non-string urls: clean error with the repr (Python crashed)
        assert_eq!(
            is_invalid(dp(json!({"type": "raw_uri", "url": 5}))),
            "unsupported url scheme: 5"
        );
        assert_eq!(
            is_invalid(dp(json!({"type": "raw_uri", "url": ["http://x"]}))),
            "unsupported url scheme: ['http://x']"
        );
    }

    #[test]
    fn url_scheme_emulates_urlsplit() {
        assert_eq!(url_scheme("HTTPS://x"), "https");
        assert_eq!(url_scheme("  \x01http://x"), "http");
        assert_eq!(url_scheme("ht\ttp://x"), "http");
        assert_eq!(url_scheme("h\ntt\rp://x"), "http");
        assert_eq!(url_scheme("a+b.c-d:x"), "a+b.c-d");
        assert_eq!(url_scheme("/path:x"), "");
        assert_eq!(url_scheme("1http://x"), "");
        assert_eq!(url_scheme("ht tp://x"), "");
        assert_eq!(url_scheme(":x"), "");
        assert_eq!(url_scheme("nocolon"), "");
        assert_eq!(url_scheme(""), "");
        assert_eq!(url_scheme("é:x"), "");
        assert_eq!(url_scheme("javascript:alert(1)"), "javascript");
    }

    #[test]
    fn raw_uri_post_calls_post_fetch_with_url_and_json() {
        let f = Fake::ok(b"%PDF-bytes");
        let body = obj(
            json!({"type": "raw_uri_post", "url": "https://render-svc/convert", "json": {"url": "https://label"}}),
        );
        assert_eq!(decode_payload(&body, &f).unwrap(), b"%PDF-bytes");
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls[0].0, "https://render-svc/convert");
        assert_eq!(calls[0].1, Some(json!({"url": "https://label"})));
    }

    #[test]
    fn pdf_uri_post_calls_post_fetch() {
        let f = Fake::ok(b"PDF");
        let body =
            obj(json!({"type": "pdf_uri_post", "url": "https://render-svc/convert", "json": {}}));
        assert_eq!(decode_payload(&body, &f).unwrap(), b"PDF");
    }

    #[test]
    fn uri_post_json_defaults_to_empty_object() {
        let f = Fake::ok(b"x");
        decode_payload(&obj(json!({"type": "pdf_uri_post", "url": "http://x"})), &f).unwrap();
        assert_eq!(f.calls.lock().unwrap()[0].1, Some(json!({})));
    }

    #[test]
    fn uri_post_missing_url_raises() {
        assert_eq!(
            is_invalid(dp(json!({"type": "pdf_uri_post", "json": {}}))),
            "missing 'url'"
        );
    }

    #[test]
    fn uri_post_fetch_failure_is_fetcherror() {
        let f = Fake::err("convert down");
        let body = obj(json!({"type": "raw_uri_post", "url": "https://x", "json": {}}));
        assert_eq!(
            decode_payload(&body, &f),
            Err(DispatchError::Fetch(
                "post fetch failed: convert down".into()
            ))
        );
    }

    #[test]
    fn agent_mode_pdf_uri_post() {
        assert_eq!(agent_mode(&json!("pdf_uri_post")), "pdf");
        assert_eq!(agent_mode(&json!("raw_uri_post")), "raw");
    }

    #[test]
    fn pdf_uri_uses_injected_fetcher_and_is_pdf_mode() {
        let f = Fake::ok(b"%PDF-bytes");
        let r = decode_payload(
            &obj(json!({"type": "pdf_uri", "url": "https://x/a.pdf"})),
            &f,
        );
        assert_eq!(r.unwrap(), b"%PDF-bytes");
        assert_eq!(agent_mode(&json!("pdf_uri")), "pdf");
    }

    #[test]
    fn parse_copies_defaults_to_one_when_absent_or_null() {
        assert_eq!(parse_copies(&obj(json!({}))), Ok(1));
        assert_eq!(parse_copies(&obj(json!({"copies": null}))), Ok(1));
    }

    #[test]
    fn parse_copies_accepts_valid_count() {
        for n in [1, 3, 100] {
            assert_eq!(parse_copies(&obj(json!({"copies": n}))), Ok(n));
        }
    }

    #[test]
    fn parse_copies_rejects_zero_and_negative() {
        for n in [0, -1, -100] {
            assert_eq!(
                is_invalid(parse_copies(&obj(json!({"copies": n})))),
                "copies must be an integer 1..100"
            );
        }
    }

    #[test]
    fn parse_copies_rejects_over_cap() {
        assert!(parse_copies(&obj(json!({"copies": 101}))).is_err());
        assert!(parse_copies(&obj(json!({"copies": u64::MAX}))).is_err());
    }

    #[test]
    fn parse_copies_rejects_non_int_types() {
        for v in [json!("3"), json!(2.5), json!(true), json!(3.0), json!([3])] {
            assert!(parse_copies(&obj(json!({"copies": v}))).is_err(), "{v}");
        }
    }

    #[test]
    fn parse_callback_url_absent_or_empty_is_none() {
        assert_eq!(parse_callback_url(&obj(json!({}))), Ok(None));
        for v in [
            json!(""),
            json!(null),
            json!(0),
            json!(false),
            json!([]),
            json!({}),
        ] {
            assert_eq!(
                parse_callback_url(&obj(json!({"callback_url": v}))),
                Ok(None)
            );
        }
    }

    #[test]
    fn parse_callback_url_accepts_http_and_https() {
        for u in ["https://hook.example/x", "http://hook/x"] {
            assert_eq!(
                parse_callback_url(&obj(json!({"callback_url": u}))),
                Ok(Some(u.into()))
            );
        }
    }

    #[test]
    fn parse_callback_url_rejects_non_http_scheme() {
        for u in [
            "file:///etc/passwd",
            "ftp://x",
            "gopher://x",
            "javascript:alert(1)",
        ] {
            let m = is_invalid(parse_callback_url(&obj(json!({"callback_url": u}))));
            assert_eq!(m, format!("callback_url must be http(s): '{u}'"));
        }
    }

    #[test]
    fn parse_callback_url_rejects_non_string() {
        for v in [
            json!(123),
            json!(1.5),
            json!(true),
            json!(["https://x"]),
            json!({"u": "https://x"}),
        ] {
            let m = is_invalid(parse_callback_url(&obj(json!({"callback_url": v.clone()}))));
            assert_eq!(m, format!("callback_url must be a string: {}", repr(&v)));
        }
    }

    fn po(o: Value, mode: &str) -> Result<Option<Map<String, Value>>, DispatchError> {
        parse_options(&obj(json!({"options": o})), mode)
    }

    #[test]
    fn parse_options_absent_null_or_empty_is_none() {
        assert_eq!(parse_options(&obj(json!({})), "pdf"), Ok(None));
        assert_eq!(po(json!(null), "pdf"), Ok(None));
        assert_eq!(po(json!({}), "pdf"), Ok(None));
        assert_eq!(po(json!({}), "raw"), Ok(None));
    }

    #[test]
    fn parse_options_valid_full_set_roundtrips() {
        let o = json!({"duplex": "long-edge", "paper": "A4", "bin": "Tray 1", "color": false, "pages": "1-3,5"});
        let got = po(o.clone(), "pdf").unwrap().unwrap();
        assert_eq!(Value::Object(got.clone()), o);
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            ["duplex", "paper", "bin", "color", "pages"]
        );
        assert!(
            po(json!({"duplex": "short-edge"}), "pdf")
                .unwrap()
                .is_some()
        );
        assert!(po(json!({"duplex": "one-sided"}), "pdf").unwrap().is_some());
    }

    #[test]
    fn parse_options_rejected_on_raw_jobs() {
        assert_eq!(
            is_invalid(po(json!({"duplex": "long-edge"}), "raw")),
            "options are only supported on pdf jobs"
        );
    }

    #[test]
    fn parse_options_rejects_non_dict() {
        for v in [
            json!("duplex"),
            json!(1),
            json!(["duplex"]),
            json!(true),
            json!(""),
        ] {
            assert_eq!(is_invalid(po(v, "pdf")), "options must be an object");
        }
    }

    #[test]
    fn parse_options_rejects_unknown_keys() {
        assert_eq!(
            is_invalid(po(json!({"stapler": true}), "pdf")),
            "unknown option(s): stapler"
        );
        assert_eq!(
            is_invalid(po(json!({"zeta": 1, "alpha": 2, "paper": "A4"}), "pdf")),
            "unknown option(s): alpha, zeta"
        );
    }

    #[test]
    fn parse_options_rejects_bad_duplex() {
        for v in [json!("both"), json!(1), json!(null), json!("duplexlong")] {
            assert_eq!(
                is_invalid(po(json!({"duplex": v}), "pdf")),
                "duplex must be one of long-edge, short-edge, one-sided"
            );
        }
    }

    #[test]
    fn parse_options_rejects_bad_paper_and_bin_values() {
        let long = "x".repeat(65);
        for v in [
            json!("A4,monochrome"),
            json!(""),
            json!(4),
            json!(null),
            json!(long),
            json!("A4\n"),
            json!("a/b"),
        ] {
            for k in ["paper", "bin"] {
                let m = is_invalid(po(json!({ k: v.clone() }), "pdf"));
                assert_eq!(
                    m,
                    format!("{k} must be 1-64 chars of letters/digits/space/._-")
                );
            }
        }
        for ok in ["x".repeat(64), "Größe_A4.1 -x".into(), "Zähler".into()] {
            assert!(po(json!({"paper": ok}), "pdf").is_ok());
        }
    }

    #[test]
    fn parse_options_rejects_non_bool_color() {
        for v in [json!("yes"), json!(1), json!(0)] {
            assert_eq!(
                is_invalid(po(json!({"color": v}), "pdf")),
                "color must be a boolean"
            );
        }
    }

    #[test]
    fn parse_options_rejects_bad_pages() {
        for v in [
            json!(""),
            json!("abc"),
            json!("1-"),
            json!("1,,2"),
            json!("1-3,"),
            json!(5),
            json!(true),
            json!("1-2-3"),
            json!("-3"),
            json!("١"),
            json!("1\n"),
        ] {
            assert_eq!(
                is_invalid(po(json!({"pages": v}), "pdf")),
                "pages must be ranges like '1-3,5'"
            );
        }
        for ok in ["1", "1-3", "1-3,5", "10,2-4,7"] {
            assert!(po(json!({"pages": ok}), "pdf").is_ok(), "{ok}");
        }
    }

    #[test]
    fn parse_expire_after_absent_or_null_is_none() {
        assert_eq!(parse_expire_after(&obj(json!({}))), Ok(None));
        assert_eq!(
            parse_expire_after(&obj(json!({"expire_after": null}))),
            Ok(None)
        );
    }

    #[test]
    fn parse_expire_after_accepts_a_positive_second_count() {
        assert_eq!(
            parse_expire_after(&obj(json!({"expire_after": 1}))),
            Ok(Some(1))
        );
        assert_eq!(
            parse_expire_after(&obj(json!({"expire_after": 2592000}))),
            Ok(Some(2592000))
        );
    }

    #[test]
    fn parse_expire_after_rejects_zero_negative_over_cap_and_non_int() {
        for v in [
            json!(0),
            json!(-5),
            json!(2592001),
            json!("60"),
            json!(60.0),
            json!(true),
            json!(u64::MAX),
        ] {
            let m = is_invalid(parse_expire_after(&obj(json!({"expire_after": v}))));
            assert_eq!(m, "expire_after must be an integer 1..2592000 (seconds)");
        }
    }

    #[test]
    fn parse_idempotency_key_absent_or_empty_is_none() {
        assert_eq!(parse_idempotency_key(&obj(json!({}))), Ok(None));
        assert_eq!(
            parse_idempotency_key(&obj(json!({"idempotency_key": null}))),
            Ok(None)
        );
        assert_eq!(
            parse_idempotency_key(&obj(json!({"idempotency_key": ""}))),
            Ok(None)
        );
        assert_eq!(
            parse_idempotency_key(&obj(json!({"idempotency_key": "  \x1f "}))),
            Ok(None)
        );
    }

    #[test]
    fn parse_idempotency_key_accepts_a_bounded_string() {
        assert_eq!(
            parse_idempotency_key(&obj(json!({"idempotency_key": " order-42 "}))),
            Ok(Some("order-42".into()))
        );
        let k = "x".repeat(128);
        assert_eq!(
            parse_idempotency_key(&obj(json!({"idempotency_key": k.clone()}))),
            Ok(Some(k))
        );
        // 128 characters, not bytes
        let k = "ä".repeat(128);
        assert_eq!(
            parse_idempotency_key(&obj(json!({"idempotency_key": k.clone()}))),
            Ok(Some(k))
        );
    }

    #[test]
    fn parse_idempotency_key_rejects_non_string_and_over_long() {
        for v in [
            json!(42),
            json!(["a"]),
            json!("x".repeat(129)),
            json!("ä".repeat(129)),
            json!(0),
            json!(false),
        ] {
            let m = is_invalid(parse_idempotency_key(&obj(json!({"idempotency_key": v}))));
            assert_eq!(m, "idempotency_key must be a string of at most 128 chars");
        }
    }

    /// One-shot-per-connection HTTP stub: answers each request with the next canned response and
    /// sends the raw request text (head + body) down the channel.
    fn stub(responses: Vec<&'static str>) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, want) = loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break (buf.len(), 0);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (p + 4, len);
                    }
                };
                while buf.len() < head_end + want {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}/p"), rx)
    }

    const OK_ZPL: &str = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nZPL";

    #[test]
    fn http_get_sets_browser_user_agent_and_returns_body() {
        let (url, rx) = stub(vec![OK_ZPL]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(f.get(&url).unwrap(), b"ZPL");
        let req = rx.recv().unwrap().to_lowercase();
        assert!(req.starts_with("get /p "), "{req}");
        assert!(req.contains("user-agent: mozilla/5.0 (print-api)"), "{req}");
    }

    #[test]
    fn only_public_addresses_count_as_public() {
        let private = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.50",
            "169.254.169.254",
            "0.0.0.0",
            "0.1.2.3",
            "100.64.0.1",
            "192.0.0.8",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "::127.0.0.1",
            "64:ff9b:1:a00:0:100::",
            "2001:0:a00:1:0:f227:fefd:fcfb",
            "::ffff:0:a00:1",
            "100::1",
            "2001:2::1",
            "2001:10::1",
            "2001:20::1",
            "3fff::1",
            "5f00::1",
            "192.0.0.1",
            "192.0.0.170",
        ];
        for ip in private {
            assert!(!is_public(ip.parse().unwrap()), "{ip} must not be public");
        }
        let public = [
            "1.1.1.1",
            "8.8.8.8",
            "100.128.0.1",
            "172.32.0.1",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
            "64:ff9b::808:808",
            "2002:808:808::",
            "192.0.0.9",
            "192.0.0.10",
            "::ffff:0:808:808",
            "2001:4860:4860::8888",
            "2a00:1450::1",
        ];
        for ip in public {
            assert!(is_public(ip.parse().unwrap()), "{ip} must be public");
        }
    }

    #[test]
    fn a_public_only_fetcher_refuses_private_destinations() {
        let (url, _rx) = stub(vec![OK_ZPL]);
        let f = HttpFetcher::with_limits_to(
            Duration::from_secs(5),
            Duration::from_secs(10),
            4,
            Destinations::PublicOnly,
        );
        let err = f.get(&url).unwrap_err();
        assert!(err.contains("destination not allowed: 127.0.0.1"), "{err}");
        let err = f.post_json(&url, &serde_json::json!({})).unwrap_err();
        assert!(err.contains("destination not allowed"), "{err}");
        // a hostname resolving to loopback is refused too (checked after DNS, not on the text)
        let named = url.replace("127.0.0.1", "localhost");
        let err = f.get(&named).unwrap_err();
        assert!(err.contains("destination not allowed: localhost"), "{err}");
        // the same URL is fine for a self-hosted box
        assert_eq!(
            HttpFetcher::new(Duration::from_secs(5)).get(&url).unwrap(),
            b"ZPL"
        );
    }

    #[test]
    fn http_get_404_is_urllib_style() {
        let (url, _rx) = stub(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(f.get(&url).unwrap_err(), "HTTP Error 404: Not Found");
    }

    #[test]
    fn http_get_follows_redirects() {
        let (url, _rx) = stub(vec![
            "HTTP/1.1 302 Found\r\nLocation: /q\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            OK_ZPL,
        ]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(f.get(&url).unwrap(), b"ZPL");
    }

    #[test]
    fn http_post_carries_json_body_and_content_type() {
        let (url, rx) = stub(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n%PDF",
        ]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(
            f.post_json(&url, &json!({"url": "https://label"})).unwrap(),
            b"%PDF"
        );
        let req = rx.recv().unwrap();
        let lower = req.to_lowercase();
        assert!(lower.starts_with("post /p "), "{req}");
        assert!(lower.contains("content-type: application/json"), "{req}");
        assert!(
            lower.contains("user-agent: mozilla/5.0 (print-api)"),
            "{req}"
        );
        assert!(req.ends_with("{\"url\": \"https://label\"}"), "{req}");
    }

    #[test]
    fn http_connection_refused_is_an_error_text() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let f = HttpFetcher::new(Duration::from_secs(2));
        assert!(
            !f.get(&format!("http://127.0.0.1:{port}/"))
                .unwrap_err()
                .is_empty()
        );
    }

    #[test]
    fn bad_base64_messages_match_python() {
        let many = "Invalid base64-encoded string: number of data characters";
        let cases = [
            ("!!!notbase64!!!", "Only base64 data is allowed".to_string()),
            ("YQ", "Incorrect padding".into()),
            ("YQ===", "Excess data after padding".into()),
            ("=", "Leading padding not allowed".into()),
            ("YWJj=", "Excess padding not allowed".into()),
            ("YW=j", "Discontinuous padding not allowed".into()),
            (
                "x",
                format!("{many} (1) cannot be 1 more than a multiple of 4"),
            ),
            (" k ", "Only base64 data is allowed".into()),
            (
                "\u{e4}bcd",
                "string argument should contain only ASCII characters".into(),
            ),
            ("YQ==Y", "Excess data after padding".into()),
            ("YQ=", "Incorrect padding".into()),
            ("YQ==\n", "Excess data after padding".into()),
            (
                "AAAAA",
                format!("{many} (5) cannot be 1 more than a multiple of 4"),
            ),
            ("AAAA=", "Excess padding not allowed".into()),
        ];
        for (input, msg) in cases {
            let b = json!({"type": "raw_base64", "content": input});
            assert_eq!(is_invalid(dp(b)), format!("bad base64: {msg}"), "{input:?}");
        }
    }

    #[test]
    fn parse_callback_url_rejects_malformed_brackets() {
        for u in [
            "http://[::1",
            "http://[1.2.3.4]/",
            "http://a]/",
            "http://x[::1]/",
        ] {
            let r = parse_callback_url(&obj(json!({"callback_url": u})));
            assert!(matches!(r, Err(DispatchError::Invalid(_))), "{u}");
        }
        assert!(parse_callback_url(&obj(json!({"callback_url": "http://[::1]:80/x"}))).is_ok());
    }

    #[test]
    fn normalize_url_strips_like_urllib() {
        assert_eq!(normalize_url(" http://h/a\n"), "http://h/a");
        assert_eq!(normalize_url("\thttp://h/a "), "http://h/a");
        assert_eq!(normalize_url("<URL:http://h/a>"), "http://h/a");
        assert_eq!(normalize_url("http://h/a`b<c>"), "http://h/a%60b%3Cc%3E");
    }

    #[test]
    fn http_get_trailing_whitespace_url_is_fetched() {
        let (url, rx) = stub(vec![OK_ZPL]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(f.get(&format!("{url}\n")).unwrap(), b"ZPL");
        assert!(rx.recv().unwrap().starts_with("GET /p "));
    }

    #[test]
    fn http_post_trailing_whitespace_url_is_posted() {
        let (url, _rx) = stub(vec![OK_ZPL]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(
            f.post_json(&format!(" {url} "), &json!({})).unwrap(),
            b"ZPL"
        );
    }

    #[test]
    fn http_get_304_is_an_error() {
        let (url, _rx) = stub(vec![
            "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(f.get(&url).unwrap_err(), "HTTP Error 304: Not Modified");
    }

    #[test]
    fn http_unknown_status_has_no_trailing_colon() {
        let (url, _rx) = stub(vec![
            "HTTP/1.1 499 Closed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]);
        let f = HttpFetcher::new(Duration::from_secs(5));
        assert_eq!(f.get(&url).unwrap_err(), "HTTP Error 499");
    }

    #[test]
    fn http_timeout_is_per_socket_operation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n");
            for b in b"SLOW" {
                std::thread::sleep(Duration::from_millis(700));
                let _ = s.write_all(&[*b]);
                let _ = s.flush();
            }
        });
        let f = HttpFetcher::new(Duration::from_secs(1));
        assert_eq!(f.get(&format!("http://{addr}/")).unwrap(), b"SLOW");
    }
}
