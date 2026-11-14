// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `printpapi` — the server binary. Configured from the environment exactly like the Python
//! server: PRINTAPI_TOKEN (required, non-empty), PRINT_DB (default printpapi.db), PRINT_PORT
//! (default 3460), PRINTAPI_SIGNUP, PUBLIC_URL, PRINTAPI_PLANS (JSON array or a path to a file
//! holding one), PRINTAPI_BILLING_SECRET, PRINTAPI_TRUSTED_PROXIES (IPs or CIDRs whose
//! `X-Forwarded-For` is believed), PRINTAPI_SIGNUP_QUOTA (monthly job quota of every self-signup
//! org), PRINTAPI_FETCHES_PER_ORG (payload downloads one org may run at once, 0 = no cap; unset:
//! 4 with open signup, else no cap), PRINTAPI_JOB_RETENTION_DAYS (days until finished jobs lose payload, title and
//! callback, and until queued jobs without a deadline are failed; their row goes after
//! max(N, 7) days if it is from before this UTC month; unset or 0 keeps everything), PRINTAPI_TERMS_VERSION (set: `POST /signup`
//! must accept that version of the terms), PRINTAPI_TERMS_URL (required with it), PRINTAPI_DPA_URL,
//! PRINTAPI_PRIVACY_URL (optional links, http(s) only), PRINTAPI_OPERATOR_TOKEN (set: the
//! `/operator/` API is on, at least 32 characters, and hosted mode with it: private URLs denied and
//! 4 fetches per org unless set), PRINTAPI_OPERATOR_FROM (required with it: IPs or CIDRs the token is
//! accepted from), LOG_REQUESTS, SMTP_*.
use printpapi_server::dispatch::{Destinations, HttpFetcher, OUTBOUND_IN_FLIGHT};
use printpapi_server::mail::{self, SmtpMailer};
use printpapi_server::server::{AppState, Config, Terms};
use printpapi_server::store::Db;
use printpapi_server::{billing, http};
use std::sync::Arc;
use std::time::Duration;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Python `str.strip()`: Unicode whitespace plus U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Empty: no quota. Otherwise a non-negative integer (0 is a real quota: nothing may be printed).
fn parse_quota(raw: &str) -> Result<Option<i64>, String> {
    let t = py_strip(raw);
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<i64>() {
        Ok(n) if n >= 0 => Ok(Some(n)),
        _ => Err(format!("must be a non-negative integer, not {t:?}")),
    }
}

/// Empty: unset (the default follows the signup mode). Otherwise a non-negative integer, 0 = no cap.
fn parse_fetch_cap(raw: &str) -> Result<Option<usize>, String> {
    let t = py_strip(raw);
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<usize>()
        .map(Some)
        .map_err(|_| format!("must be a non-negative integer, not {t:?}"))
}

/// Empty or 0: keep for ever. Otherwise a positive number of days.
fn parse_retention(raw: &str) -> Result<Option<i64>, String> {
    let t = py_strip(raw);
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<i64>() {
        Ok(0) => Ok(None),
        Ok(n) if n > 0 => Ok(Some(n)),
        _ => Err(format!("must be a positive integer, not {t:?}")),
    }
}

/// The one link rule (`web/lib/terms.ts` `isHttpUrl` is its twin): `http://` or `https://`, any
/// case, then a non-empty host. Printable ASCII only (0x21..=0x7E): Rust's whitespace and JS's
/// `\s` disagree on U+0085 and U+FEFF, so neither is asked.
fn is_terms_url(u: &str) -> bool {
    if !u.bytes().all(|b| (0x21..=0x7E).contains(&b)) {
        return false;
    }
    let lower = u.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.split(['/', '?', '#']).next().unwrap_or("").is_empty()
}

/// The terms a signup must accept. A version needs a terms URL. Every link that is set must be
/// an absolute `http(s)` URL with a host, version or not.
fn parse_terms(
    version: &str,
    terms_url: &str,
    dpa_url: &str,
    privacy_url: &str,
) -> Result<Option<Terms>, String> {
    let version = py_strip(version);
    let url = |name: &str, raw: &str| -> Result<Option<String>, String> {
        let t = py_strip(raw);
        if t.is_empty() {
            Ok(None)
        } else if is_terms_url(t) {
            Ok(Some(t.to_string()))
        } else {
            Err(format!("{name} must be an http(s) URL, not {t:?}"))
        }
    };
    let terms_url = url("PRINTAPI_TERMS_URL", terms_url)?;
    let dpa_url = url("PRINTAPI_DPA_URL", dpa_url)?;
    let privacy_url = url("PRINTAPI_PRIVACY_URL", privacy_url)?;
    if version.is_empty() {
        return Ok(None); // no consent required: the links are only checked, not used
    }
    let terms_url =
        terms_url.ok_or("PRINTAPI_TERMS_URL is required with PRINTAPI_TERMS_VERSION")?;
    Ok(Some(Terms {
        version: version.to_string(),
        terms_url,
        dpa_url,
        privacy_url,
    }))
}

/// The operator token and the peers it is accepted from. Unset or blank: the operator API is off.
/// The token is at least 32 characters, never starts with `sess_` (a session's prefix, which the
/// credential lookup treats specially) and is not the root token. The peer list is required: a
/// token that works from anywhere is not what the operator API is for.
fn parse_operator(
    token: &str,
    from: &str,
    root: &str,
) -> Result<Option<(String, printpapi_server::proxy::TrustedProxies)>, String> {
    let token = py_strip(token);
    if token.is_empty() {
        return Ok(None);
    }
    if token.chars().count() < 32 {
        return Err("PRINTAPI_OPERATOR_TOKEN must be at least 32 characters".into());
    }
    if token.starts_with("sess_") {
        return Err("PRINTAPI_OPERATOR_TOKEN must not start with sess_".into());
    }
    if token == root {
        return Err("PRINTAPI_OPERATOR_TOKEN must differ from PRINTAPI_TOKEN".into());
    }
    let from = printpapi_server::proxy::TrustedProxies::parse(from)
        .map_err(|e| format!("PRINTAPI_OPERATOR_FROM: {e}"))?;
    if from.is_empty() {
        return Err(
            "PRINTAPI_OPERATOR_FROM is required with PRINTAPI_OPERATOR_TOKEN (the console's address, e.g. 172.30.0.3)"
                .into(),
        );
    }
    Ok(Some((token.to_string(), from)))
}

/// One stderr line per hosted-beta safeguard that is off while strangers can sign up.
fn startup_warnings(cfg: &Config) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfg.signup_open && cfg.signup_quota.is_none() {
        out.push(
            "warning: PRINTAPI_SIGNUP=open without PRINTAPI_SIGNUP_QUOTA: new orgs can print without limit",
        );
    }
    if cfg.signup_open && cfg.job_retention_days.is_none() {
        out.push(
            "warning: PRINTAPI_SIGNUP=open without PRINTAPI_JOB_RETENTION_DAYS: job payloads are kept for ever",
        );
    }
    if cfg.signup_open && cfg.terms.is_none() {
        out.push(
            "warning: PRINTAPI_SIGNUP=open without PRINTAPI_TERMS_VERSION: signups do not accept any terms",
        );
    }
    out
}

fn config() -> Result<(Config, String, u16), String> {
    config_from(&env)
}

fn config_from(env: &dyn Fn(&str) -> Option<String>) -> Result<(Config, String, u16), String> {
    let token = env("PRINTAPI_TOKEN").ok_or("PRINTAPI_TOKEN is required")?;
    if py_strip(&token).is_empty() {
        // An empty bootstrap token would compare-equal every missing Authorization header,
        // making the whole API root-writable.
        return Err("PRINTAPI_TOKEN must not be empty".into());
    }
    let db_path = env("PRINT_DB").unwrap_or_else(|| "printpapi.db".into());
    let port = match env("PRINT_PORT") {
        None => 3460,
        Some(raw) => {
            let t = py_strip(&raw);
            t.strip_prefix('+')
                .unwrap_or(t)
                .parse::<u16>()
                .map_err(|_| format!("invalid PRINT_PORT: {raw:?}"))?
        }
    };
    // An HTTP header value cannot carry leading/trailing whitespace (hyper trims it), so a padded
    // token could never match: use it trimmed (deliberate deviation, Python compared it raw).
    let mut cfg = Config::new(py_strip(&token));
    cfg.signup_open = env("PRINTAPI_SIGNUP").as_deref() == Some("open");
    cfg.public_url = env("PUBLIC_URL")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    cfg.reset_enabled = mail::configured(&|k| env(k));
    // PRINTAPI_PLANS is the catalogue itself (a JSON array) or a path to a file holding one, so a
    // compose file can mount it instead of squeezing JSON into an env var.
    let mut raw_plans = py_strip(&env("PRINTAPI_PLANS").unwrap_or_default()).to_string();
    if !raw_plans.is_empty() && !raw_plans.starts_with('[') {
        raw_plans = std::fs::read_to_string(&raw_plans)
            .map_err(|e| format!("cannot read PRINTAPI_PLANS file {raw_plans:?}: {e}"))?;
    }
    cfg.plans = billing::load_plans(&raw_plans).map_err(|e| e.to_string())?;
    cfg.billing_secret = env("PRINTAPI_BILLING_SECRET").filter(|s| !s.is_empty());
    cfg.log_requests = env("LOG_REQUESTS").is_some_and(|s| !s.is_empty());
    if let Some((token, from)) = parse_operator(
        &env("PRINTAPI_OPERATOR_TOKEN").unwrap_or_default(),
        &env("PRINTAPI_OPERATOR_FROM").unwrap_or_default(),
        &cfg.token,
    )? {
        cfg.operator_token = Some(token);
        cfg.operator_from = from;
    }
    // Hosted mode comes with the operator token: strangers' jobs run here, so the safe defaults
    // hold whatever signup says, fixed at startup (a later live setting never moves them).
    let hosted = cfg.operator_token.is_some();
    // Private destinations are refused by default exactly when strangers get accounts (open
    // signup, or hosted mode); a self-hosted box keeps fetching from its LAN.
    let private = env("PRINTAPI_PRIVATE_URLS").unwrap_or_default();
    cfg.destinations = match py_strip(&private) {
        "" if cfg.signup_open || hosted => Destinations::PublicOnly,
        "" | "allow" => Destinations::Any,
        "deny" => Destinations::PublicOnly,
        other => {
            return Err(format!(
                "PRINTAPI_PRIVATE_URLS must be allow or deny, not {other:?}"
            ));
        }
    };
    cfg.fetches_per_org = parse_fetch_cap(&env("PRINTAPI_FETCHES_PER_ORG").unwrap_or_default())
        .map_err(|e| format!("PRINTAPI_FETCHES_PER_ORG: {e}"))?;
    if hosted && cfg.fetches_per_org.is_none() {
        cfg.fetches_per_org = Some(printpapi_server::server::MAX_FETCHES_PER_ORG);
    }
    cfg.trusted_proxies = printpapi_server::proxy::TrustedProxies::parse(
        &env("PRINTAPI_TRUSTED_PROXIES").unwrap_or_default(),
    )
    .map_err(|e| format!("PRINTAPI_TRUSTED_PROXIES: {e}"))?;
    cfg.signup_quota = parse_quota(&env("PRINTAPI_SIGNUP_QUOTA").unwrap_or_default())
        .map_err(|e| format!("PRINTAPI_SIGNUP_QUOTA: {e}"))?;
    cfg.job_retention_days =
        parse_retention(&env("PRINTAPI_JOB_RETENTION_DAYS").unwrap_or_default())
            .map_err(|e| format!("PRINTAPI_JOB_RETENTION_DAYS: {e}"))?;

    cfg.terms = parse_terms(
        &env("PRINTAPI_TERMS_VERSION").unwrap_or_default(),
        &env("PRINTAPI_TERMS_URL").unwrap_or_default(),
        &env("PRINTAPI_DPA_URL").unwrap_or_default(),
        &env("PRINTAPI_PRIVACY_URL").unwrap_or_default(),
    )?;
    cfg.open_enrollment = env("PRINTAPI_OPEN_ENROLLMENT").is_some_and(|s| py_strip(&s) == "1");
    Ok((cfg, db_path, port))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn run() -> Result<(), String> {
    let (cfg, db_path, port) = config()?;
    let destinations = cfg.destinations;
    for line in startup_warnings(&cfg) {
        eprintln!("{line}");
    }
    if cfg.operator_token.is_some() {
        eprintln!("operator API on (hosted mode: private URLs denied unless allowed explicitly)");
    }
    let db = Db::open(&db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    let state = Arc::new(AppState::new(
        cfg,
        db,
        Arc::new(SmtpMailer),
        Arc::new(HttpFetcher::with_limits_to(
            Duration::from_secs(30),
            Duration::from_secs(120),
            OUTBOUND_IN_FLIGHT,
            destinations,
        )),
    ));
    http::start_background(state.clone());
    // The heavy lifting (SQLite, scrypt, outbound HTTP) runs on the blocking pool, so a few
    // workers are plenty and idle ones would only cost RSS.
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(4);
    // Blocking threads are only spawned under load and exit after 10 s idle. 512 (tokio's
    // default) leaves room for the slow outbound fetches HttpFetcher admits (at most
    // OUTBOUND_IN_FLIGHT) with as many threads again for everything else — the Python server
    // took a thread per request without any cap.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(2 * printpapi_server::dispatch::OUTBOUND_IN_FLIGHT)
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start runtime: {e}"))?;
    let result = rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .map_err(|e| format!("cannot bind :{port}: {e}"))?;
        println!("printpapi listening on :{port}");
        http::serve(listener, state, shutdown_signal())
            .await
            .map_err(|e| e.to_string())
    });
    // Dropping the runtime would wait for every blocking task, i.e. for an in-flight outbound
    // fetch; docker stop would then end in SIGKILL. SQLite's WAL makes abandoning them safe.
    rt.shutdown_timeout(Duration::from_secs(1));
    result
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("printpapi {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Err(msg) = run() {
        eprintln!("{msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod terms_tests {
    use super::parse_terms;

    #[test]
    fn urls_need_scheme_slashes_and_a_host() {
        for bad in [
            "https:example.com/terms",
            "http:/terms",
            "https:",
            "http:dpa.example",
            "javascript:x",
            "https://",
            "https:///path",
        ] {
            assert!(parse_terms("v1", bad, "", "").is_err(), "terms {bad}");
            assert!(
                parse_terms("v1", "https://x.test/t", bad, "").is_err(),
                "dpa {bad}"
            );
            assert!(
                parse_terms("v1", "https://x.test/t", "", bad).is_err(),
                "privacy {bad}"
            );
        }
        let t = parse_terms("v1", "HTTPS://EXAMPLE.COM/t", "", "")
            .unwrap()
            .unwrap();
        assert_eq!(t.terms_url, "HTTPS://EXAMPLE.COM/t");
    }

    #[test]
    fn urls_are_printable_ascii_only() {
        // U+FEFF is not whitespace to Rust but is to JS `\s`; U+0085 the other way round.
        for bad in [
            "https://x.test/t\u{feff}",
            "https://x.test/\u{85}t",
            "https://x.test/täglich",
            "https://x.test/a b",
            "https://x.test/\u{7f}",
        ] {
            assert!(parse_terms("v1", bad, "", "").is_err(), "{bad:?}");
        }
        assert!(parse_terms("v1", "https://x.test/a%20b?q=1#f", "", "").is_ok());
    }

    #[test]
    fn urls_are_checked_even_without_a_version() {
        assert!(parse_terms("", "https:example.com", "", "").is_err());
        assert!(parse_terms("", "", "ftp://x/d", "").is_err());
        assert!(parse_terms("", "", "", "/privacy").is_err());
        assert!(parse_terms("", "", "", "").unwrap().is_none());
        assert!(
            parse_terms("", "https://x.test/t", "", "")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_version_still_needs_a_terms_url() {
        assert!(parse_terms("v1", "", "", "").is_err());
    }
}

#[cfg(test)]
mod operator_tests {
    use super::config_from;
    use printpapi_server::dispatch::Destinations;
    use printpapi_server::server::Config;

    const ROOT: &str = "root-token";
    const OP: &str = "operator-token-0123456789abcdef-xyz";

    fn cfg(extra: &[(&str, &str)]) -> Result<Config, String> {
        let mut vars: Vec<(String, String)> = vec![("PRINTAPI_TOKEN".into(), ROOT.into())];
        vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        config_from(&|k| vars.iter().rfind(|(n, _)| n == k).map(|(_, v)| v.clone()))
            .map(|(c, _, _)| c)
    }

    /// The operator variables plus `extra`; an entry of `extra` replaces the default of its name.
    fn with_op(extra: &[(&str, &str)]) -> Result<Config, String> {
        let mut v = vec![
            ("PRINTAPI_OPERATOR_TOKEN", OP),
            ("PRINTAPI_OPERATOR_FROM", "10.9.0.2"),
        ];
        for (k, val) in extra {
            v.retain(|(n, _)| n != k);
            v.push((k, val));
        }
        cfg(&v)
    }

    #[test]
    fn unset_the_operator_api_is_off_and_nothing_else_changes() {
        let c = cfg(&[]).unwrap();
        assert!(c.operator_token.is_none());
        assert_eq!(c.destinations, Destinations::Any);
        assert_eq!(c.fetches_per_org, None);
    }

    #[test]
    fn a_token_needs_from() {
        let e = cfg(&[("PRINTAPI_OPERATOR_TOKEN", OP)]).unwrap_err();
        assert!(e.contains("PRINTAPI_OPERATOR_FROM"), "{e}");
        let e = cfg(&[
            ("PRINTAPI_OPERATOR_TOKEN", OP),
            ("PRINTAPI_OPERATOR_FROM", "  "),
        ])
        .unwrap_err();
        assert!(e.contains("PRINTAPI_OPERATOR_FROM"), "{e}");
        let c = with_op(&[]).unwrap();
        assert_eq!(c.operator_token.as_deref(), Some(OP));
        assert!(!c.operator_from.is_empty());
    }

    #[test]
    fn the_token_must_be_long_and_not_look_like_a_session_or_the_root_token() {
        let short = "a".repeat(31);
        let e = with_op(&[("PRINTAPI_OPERATOR_TOKEN", &short)]).unwrap_err();
        assert!(e.contains("PRINTAPI_OPERATOR_TOKEN"), "{e}");
        let ok = "a".repeat(32);
        assert!(with_op(&[("PRINTAPI_OPERATOR_TOKEN", &ok)]).is_ok());
        let sess = format!("sess_{}", "a".repeat(40));
        assert!(with_op(&[("PRINTAPI_OPERATOR_TOKEN", &sess)]).is_err());
        let e = with_op(&[("PRINTAPI_TOKEN", OP)]).unwrap_err();
        assert!(e.contains("PRINTAPI_TOKEN"), "{e}");
        // surrounding whitespace never counts towards the length
        let padded = format!(" {} ", "a".repeat(31));
        assert!(with_op(&[("PRINTAPI_OPERATOR_TOKEN", &padded)]).is_err());
    }

    #[test]
    fn an_invalid_from_entry_stops_the_server() {
        let e = with_op(&[("PRINTAPI_OPERATOR_FROM", "10.9.0.2,not-an-ip")]).unwrap_err();
        assert!(e.contains("PRINTAPI_OPERATOR_FROM"), "{e}");
    }

    #[test]
    fn hosted_mode_comes_with_the_token() {
        let c = with_op(&[]).unwrap();
        assert_eq!(c.destinations, Destinations::PublicOnly);
        assert_eq!(c.fetches_per_org, Some(4));
        assert_eq!(c.fetch_cap(), 4);
        // whatever signup says
        let c = with_op(&[("PRINTAPI_SIGNUP", "closed")]).unwrap();
        assert_eq!(c.destinations, Destinations::PublicOnly);
        assert_eq!(c.fetch_cap(), 4);
    }

    #[test]
    fn explicit_settings_beat_the_hosted_defaults() {
        let c = with_op(&[
            ("PRINTAPI_PRIVATE_URLS", "allow"),
            ("PRINTAPI_FETCHES_PER_ORG", "0"),
        ])
        .unwrap();
        assert_eq!(c.destinations, Destinations::Any);
        assert_eq!(c.fetch_cap(), 0);
        let c = with_op(&[("PRINTAPI_FETCHES_PER_ORG", "9")]).unwrap();
        assert_eq!(c.fetch_cap(), 9);
    }
}
