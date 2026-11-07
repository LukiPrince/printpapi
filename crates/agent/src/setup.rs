// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The logic behind the setup window's steps: checking the server address and key without
//! registering anything, the test print, and the proof that the installed agent really came up.

use std::fmt;

use serde_json::Value;

use crate::client::{Http, HttpError, http_status};
use crate::config::Printer;
use crate::print::{Fns, SendResult, print_job};
use crate::proc::Runner;
use crate::{Platform, select_backend};

/// The one-page PDF of `web/lib/testdoc.ts`.
pub const TEST_PDF: &[u8] = include_bytes!("testpage.pdf");
/// The one-line label of `web/lib/testdoc.ts`.
pub const TEST_ZPL: &[u8] = b"^XA^FO40,40^ADN,36,20^FDprintpapi test^FS^XZ";

/// Why the window cannot go on from step 1.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectError {
    BadUrl,
    Unreachable(String),
    BadKey,
}

impl ConnectError {
    /// A stable word for the window to branch on.
    pub fn code(&self) -> &'static str {
        match self {
            ConnectError::BadUrl => "bad_url",
            ConnectError::Unreachable(_) => "unreachable",
            ConnectError::BadKey => "bad_key",
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::BadUrl => {
                f.write_str("The server address must start with http:// or https://.")
            }
            ConnectError::Unreachable(why) => write!(f, "Could not reach the server: {why}"),
            ConnectError::BadKey => {
                f.write_str("The server does not accept this key. It is wrong or was revoked.")
            }
        }
    }
}

/// What the person typed -> the base URL the agent will use: trimmed, no trailing slash.
/// Only `http(s)` with a host; whitespace inside is refused because the value goes into agent.ini,
/// where a line break would start a new setting.
pub fn normalize_server_url(raw: &str) -> Result<String, ConnectError> {
    let url = raw.trim().trim_end_matches('/');
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ConnectError::BadUrl);
    }
    let (_, rest) = split_scheme(url).ok_or(ConnectError::BadUrl)?;
    if host_of(rest).is_empty() {
        return Err(ConnectError::BadUrl);
    }
    Ok(url.to_string())
}

/// `(is_https, rest after "://")` for an `http(s)` URL, `None` for any other scheme.
fn split_scheme(url: &str) -> Option<(bool, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.eq_ignore_ascii_case("https") {
        Some((true, rest))
    } else if scheme.eq_ignore_ascii_case("http") {
        Some((false, rest))
    } else {
        None
    }
}

/// The host of `authority/path`: userinfo, port and IPv6 brackets removed.
fn host_of(rest: &str) -> &str {
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or(v6);
    }
    auth.split(':').next().unwrap_or(auth)
}

/// Plain `http://` sends the key readable. That is fine on this machine or in a private network,
/// anywhere else the person has to confirm.
pub fn needs_insecure_confirm(url: &str) -> bool {
    let Some((false, rest)) = split_scheme(url.trim()) else {
        return false; // https, or not a URL at all (normalize_server_url refuses that)
    };
    let host = host_of(rest);
    if host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !(ip.is_loopback() || ip.is_private()),
        Ok(std::net::IpAddr::V6(ip)) => !(ip.is_loopback() || ip.is_unique_local()),
        Err(_) => true,
    }
}

fn unreachable_or_key(e: &HttpError) -> ConnectError {
    if http_status(e) == Some(401) {
        ConnectError::BadKey
    } else {
        ConnectError::Unreachable(e.to_string())
    }
}

/// The `computers` list of `GET /computers`; a 204 reads as an empty list.
fn computers(http: &dyn Http, base: &str, key: &str) -> Result<Vec<Value>, ConnectError> {
    let reply = http
        .get(&format!("{base}/computers"), key)
        .map_err(|e| unreachable_or_key(&e))?;
    Ok(reply
        .as_ref()
        .and_then(|v| v.get("computers"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Is the server there and does it accept the key? Registers nothing. `Ok(true)` when a device of
/// this name is already listed (the same machine set up again, which is allowed).
pub fn check_connection(
    http: &dyn Http,
    base: &str,
    key: &str,
    name: &str,
) -> Result<bool, ConnectError> {
    http.get(&format!("{base}/health"), key)
        .map_err(|e| ConnectError::Unreachable(e.to_string()))?;
    Ok(computers(http, base, key)?
        .iter()
        .any(|c| c["name"].as_str() == Some(name)))
}

/// Has the device of this name been in touch since `baseline` (its `last_seen_at` read right after
/// registering)? The `online` flag would not prove it, see the spec.
pub fn agent_seen_after(
    http: &dyn Http,
    base: &str,
    key: &str,
    name: &str,
    baseline: f64,
) -> Result<bool, ConnectError> {
    Ok(computers(http, base, key)?
        .iter()
        .find(|c| c["name"].as_str() == Some(name))
        .and_then(|c| c["last_seen_at"].as_f64())
        .is_some_and(|seen| seen > baseline))
}

/// One copy of the built-in test page: a PDF only to a printer marked `|pdf`, else the ZPL label.
pub fn test_print_with(entry: &Printer, fns: &Fns) -> SendResult {
    let (mode, data) = if entry.can_pdf {
        ("pdf", TEST_PDF)
    } else {
        ("raw", TEST_ZPL)
    };
    print_job(mode, entry, data, 1, None, fns, None)
}

/// [`test_print_with`] through the same backend the running agent uses.
pub fn test_print(entry: &Printer, sumatra: &str, run: Runner) -> SendResult {
    let (raw, pdf) = select_backend(Platform::host(), sumatra, run);
    test_print_with(entry, &Fns::native(raw, pdf))
}

fn device_name_from(get: impl Fn(&str) -> Option<String>) -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "agent".to_string())
}

/// The suggested device name: the computer's name, else `agent`.
pub fn default_device_name() -> String {
    device_name_from(|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::HttpError;
    use serde_json::{Value, json};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Reply = Result<Option<Value>, HttpError>;

    /// `/health` and `/computers` answered with canned replies; every URL asked is recorded.
    struct FakeHttp {
        health: Reply,
        computers: Reply,
        asked: RefCell<Vec<(String, String)>>,
    }

    impl FakeHttp {
        fn new(health: Reply, computers: Reply) -> FakeHttp {
            FakeHttp {
                health,
                computers,
                asked: RefCell::new(Vec::new()),
            }
        }
        fn with_computers(list: Value) -> FakeHttp {
            FakeHttp::new(
                Ok(Some(json!({"ok": true}))),
                Ok(Some(json!({ "computers": list }))),
            )
        }
    }

    impl Http for FakeHttp {
        fn get(&self, url: &str, key: &str) -> Reply {
            self.asked.borrow_mut().push((url.into(), key.into()));
            if url.ends_with("/health") {
                self.health.clone()
            } else {
                self.computers.clone()
            }
        }
        fn get_bytes(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, HttpError> {
            unreachable!("the setup downloads nothing here")
        }
        fn post(&self, _: &str, _: &str, _: &Value) -> Reply {
            unreachable!("the check must not register anything")
        }
    }

    fn status(code: u16) -> Reply {
        Err(HttpError::Os(format!("server returned {code}")))
    }

    #[test]
    fn normalize_trims_and_drops_the_trailing_slash() {
        assert_eq!(
            normalize_server_url("  https://x/print/ ").unwrap(),
            "https://x/print"
        );
        assert_eq!(
            normalize_server_url("HTTP://192.168.1.5:3460///").unwrap(),
            "HTTP://192.168.1.5:3460"
        );
    }

    #[test]
    fn normalize_refuses_anything_but_http_and_https() {
        for bad in [
            "",
            "   ",
            "ftp://x",
            "x.example.de",
            "https://",
            "http:///path",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                normalize_server_url(bad),
                Err(ConnectError::BadUrl),
                "{bad}"
            );
        }
    }

    #[test]
    fn normalize_refuses_a_line_break_that_would_start_a_new_ini_line() {
        assert_eq!(
            normalize_server_url("https://x\napi_key=evil"),
            Err(ConnectError::BadUrl)
        );
        assert_eq!(
            normalize_server_url("https://x y"),
            Err(ConnectError::BadUrl)
        );
    }

    #[test]
    fn only_plain_http_to_a_public_host_needs_the_confirmation() {
        for risky in ["http://print.example.de", "http://8.8.8.8", "http://nas"] {
            assert!(needs_insecure_confirm(risky), "{risky}");
        }
        for fine in [
            "https://x",
            "https://8.8.8.8",
            "http://127.0.0.1:3460",
            "http://localhost",
            "http://LOCALHOST:3460/print",
            "http://192.168.1.5",
            "http://10.0.0.1",
            "http://172.16.0.1",
            "http://[::1]:3460",
        ] {
            assert!(!needs_insecure_confirm(fine), "{fine}");
        }
    }

    #[test]
    fn a_public_address_hiding_behind_a_private_looking_name_still_asks() {
        // userinfo must not fool the host check; 172.32 is outside 172.16/12
        assert!(needs_insecure_confirm("http://192.168.1.5@evil.example.de"));
        assert!(needs_insecure_confirm("http://172.32.0.1"));
        assert!(needs_insecure_confirm("http://10.0.0.1.example.de"));
    }

    #[test]
    fn error_codes_and_messages_are_plain_words() {
        assert_eq!(ConnectError::BadUrl.code(), "bad_url");
        assert_eq!(ConnectError::BadKey.code(), "bad_key");
        let e = ConnectError::Unreachable("boom".into());
        assert_eq!(e.code(), "unreachable");
        assert_eq!(e.to_string(), "Could not reach the server: boom");
        assert!(!ConnectError::BadUrl.to_string().is_empty());
        assert!(ConnectError::BadKey.to_string().contains("key"));
    }

    #[test]
    fn a_revoked_or_wrong_key_is_bad_key() {
        let http = FakeHttp::new(Ok(Some(json!({"ok": true}))), status(401));
        assert_eq!(
            check_connection(&http, "https://x", "k", "pc"),
            Err(ConnectError::BadKey)
        );
    }

    #[test]
    fn the_name_in_the_list_is_reported_but_not_an_error() {
        let http = FakeHttp::with_computers(json!([{"id": 1, "name": "shop-pc"}]));
        assert_eq!(
            check_connection(&http, "https://x", "k", "shop-pc"),
            Ok(true)
        );
        assert_eq!(
            check_connection(&http, "https://x", "k", "other"),
            Ok(false)
        );
        let asked = http.asked.borrow();
        assert_eq!(asked[0], ("https://x/health".to_string(), "k".to_string()));
        assert_eq!(
            asked[1],
            ("https://x/computers".to_string(), "k".to_string())
        );
    }

    #[test]
    fn a_tls_failure_reads_as_unreachable_in_words() {
        let tls = HttpError::Os("connection failed: invalid peer certificate".into());
        let http = FakeHttp::new(Err(tls), Ok(None));
        let e = check_connection(&http, "https://x", "k", "pc").unwrap_err();
        assert!(matches!(e, ConnectError::Unreachable(_)));
        assert!(e.to_string().starts_with("Could not reach the server:"));
        assert!(e.to_string().contains("invalid peer certificate"));
    }

    #[test]
    fn a_server_error_on_computers_is_unreachable_and_never_leaks_the_key() {
        let http = FakeHttp::new(Ok(Some(json!({"ok": true}))), status(502));
        let e = check_connection(&http, "https://x", "sekret", "pc").unwrap_err();
        assert_eq!(
            e,
            ConnectError::Unreachable("server returned 502".to_string())
        );
        assert!(!e.to_string().contains("sekret"));
    }

    #[test]
    fn an_empty_computers_reply_means_the_name_is_free() {
        let http = FakeHttp::new(Ok(Some(json!({"ok": true}))), Ok(None));
        assert_eq!(check_connection(&http, "https://x", "k", "pc"), Ok(false));
    }

    #[test]
    fn the_agent_counts_as_seen_only_after_the_baseline() {
        let list = |seen: f64| json!([{"id": 1, "name": "pc", "last_seen_at": seen}]);
        let same = FakeHttp::with_computers(list(10.0));
        assert_eq!(
            agent_seen_after(&same, "https://x", "k", "pc", 10.0),
            Ok(false)
        );
        let later = FakeHttp::with_computers(list(12.0));
        assert_eq!(
            agent_seen_after(&later, "https://x", "k", "pc", 10.0),
            Ok(true)
        );
        let other = FakeHttp::with_computers(json!([{"id": 2, "name": "x", "last_seen_at": 99.0}]));
        assert_eq!(
            agent_seen_after(&other, "https://x", "k", "pc", 10.0),
            Ok(false)
        );
        let never =
            FakeHttp::with_computers(json!([{"id": 1, "name": "pc", "last_seen_at": null}]));
        assert_eq!(
            agent_seen_after(&never, "https://x", "k", "pc", 10.0),
            Ok(false)
        );
    }

    #[test]
    fn the_online_check_tells_a_bad_key_from_a_dead_server() {
        let revoked = FakeHttp::new(Ok(None), status(401));
        assert_eq!(
            agent_seen_after(&revoked, "https://x", "k", "pc", 1.0),
            Err(ConnectError::BadKey)
        );
        let down = FakeHttp::new(Ok(None), status(503));
        assert!(matches!(
            agent_seen_after(&down, "https://x", "k", "pc", 1.0),
            Err(ConnectError::Unreachable(_))
        ));
    }

    type SendLog = Rc<RefCell<Vec<(&'static str, Vec<u8>)>>>;

    /// Fns that record which sender got which bytes; every other sender fails the test.
    fn recording(log: &SendLog) -> Fns {
        let (raw_log, pdf_log) = (log.clone(), log.clone());
        let mut fns = Fns::refusing();
        fns.raw = Box::new(move |_, d| {
            raw_log.borrow_mut().push(("raw", d.to_vec()));
            Ok(())
        });
        fns.pdf = Box::new(move |_, d, _| {
            pdf_log.borrow_mut().push(("pdf", d.to_vec()));
            Ok(())
        });
        fns
    }

    #[test]
    fn test_print_never_sends_pdf_to_a_raw_printer() {
        let log: SendLog = Rc::new(RefCell::new(Vec::new()));
        let fns = recording(&log);
        test_print_with(&Printer::new("Zebra", false, "Zebra"), &fns).unwrap();
        test_print_with(&Printer::new("HP", true, "HP"), &fns).unwrap();
        let log = log.borrow();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], ("raw", TEST_ZPL.to_vec()));
        assert_eq!(log[1], ("pdf", TEST_PDF.to_vec()));
    }

    #[test]
    fn a_failing_test_print_returns_the_reason() {
        let mut fns = Fns::refusing();
        fns.raw = Box::new(|_, _| Err("printer offline".into()));
        assert_eq!(
            test_print_with(&Printer::new("Z", false, "Z"), &fns),
            Err("printer offline".to_string())
        );
    }

    #[test]
    fn the_test_page_is_the_pdf_of_the_dashboard_and_the_label_is_its_zpl() {
        assert!(TEST_PDF.starts_with(b"%PDF-1.4"));
        assert!(TEST_PDF.ends_with(b"%%EOF"));
        assert_eq!(TEST_ZPL, b"^XA^FO40,40^ADN,36,20^FDprintpapi test^FS^XZ");
    }

    #[cfg(not(windows))]
    #[test]
    fn test_print_goes_through_the_real_backend_choice() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        let run: Runner = Rc::new(move |c| {
            s.borrow_mut().push(c.argv.clone());
            Ok(Vec::new())
        });
        test_print(&Printer::new("Zebra", false, "Zebra"), "S.exe", run).unwrap();
        assert_eq!(seen.borrow()[0], ["lp", "-d", "Zebra", "-o", "raw"]);
    }

    #[test]
    fn the_device_name_prefers_the_computer_name_and_falls_back_to_agent() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            device_name_from(env(&[("COMPUTERNAME", "SHOP-PC"), ("HOSTNAME", "h")])),
            "SHOP-PC"
        );
        assert_eq!(device_name_from(env(&[("HOSTNAME", "host")])), "host");
        assert_eq!(device_name_from(env(&[("COMPUTERNAME", "  ")])), "agent");
        assert_eq!(device_name_from(env(&[])), "agent");
        assert!(!default_device_name().is_empty());
    }
}
