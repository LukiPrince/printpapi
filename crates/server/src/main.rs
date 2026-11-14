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
use printpapi_server::server::{AppState, Config};
use printpapi_server::settings::{
    EnvFlags, Live, parse_fetch_cap, parse_quota, parse_retention, parse_terms, py_strip,
    signup_env_locks,
};
use printpapi_server::store::Db;
use printpapi_server::{billing, http};
use std::sync::Arc;
use std::time::Duration;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
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

/// One stderr line per hosted-beta safeguard that is off while strangers can sign up, judged on
/// the effective values (an override from the database counts).
fn startup_warnings(live: &Live) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !live.signup.value {
        return out;
    }
    if live.signup_quota.value.is_none() {
        out.push(
            "warning: signup is open (PRINTAPI_SIGNUP or the operator console) without PRINTAPI_SIGNUP_QUOTA: new orgs can print without limit",
        );
    }
    if live.job_retention_days.value.is_none() {
        out.push(
            "warning: signup is open (PRINTAPI_SIGNUP or the operator console) without PRINTAPI_JOB_RETENTION_DAYS: job payloads are kept for ever",
        );
    }
    if live.terms.value.is_none() {
        out.push(
            "warning: signup is open (PRINTAPI_SIGNUP or the operator console) without PRINTAPI_TERMS_VERSION: signups do not accept any terms",
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
    // Which of the live settings' variables are set at all: it decides a value's source and
    // whether `PRINTAPI_SIGNUP` locks signup shut (any value but `open`).
    let set = |k: &str| env(k).is_some_and(|v| !py_strip(&v).is_empty());
    cfg.env_flags = EnvFlags {
        signup_set: env("PRINTAPI_SIGNUP").is_some(),
        signup_explicit_closed: env("PRINTAPI_SIGNUP").is_some_and(|v| signup_env_locks(&v)),
        signup_quota_set: set("PRINTAPI_SIGNUP_QUOTA"),
        job_retention_days_set: set("PRINTAPI_JOB_RETENTION_DAYS"),
        terms_set: set("PRINTAPI_TERMS_VERSION"),
    };
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
    let hosted_locks_enforced = cfg.operator_token.is_some();
    if cfg.operator_token.is_some() {
        eprintln!("operator API on (hosted mode: private URLs denied unless allowed explicitly)");
    }
    let db = Db::open(&db_path).map_err(|e| format!("cannot open database {db_path:?}: {e}"))?;
    if !hosted_locks_enforced
        && let Ok(n) = db.with(printpapi_server::store::count_org_locks)
        && n > 0
    {
        eprintln!("{n} org locks are not enforced: PRINTAPI_OPERATOR_TOKEN is unset");
    }
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
    for line in startup_warnings(&state.live()) {
        eprintln!("{line}");
    }
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

#[cfg(test)]
mod live_tests {
    use super::{config_from, startup_warnings};
    use printpapi_server::server::Config;
    use printpapi_server::settings::{Source, Sourced};

    fn cfg(extra: &[(&str, &str)]) -> Config {
        let mut vars: Vec<(String, String)> = vec![("PRINTAPI_TOKEN".into(), "root".into())];
        vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        config_from(&|k| vars.iter().rfind(|(n, _)| n == k).map(|(_, v)| v.clone()))
            .map(|(c, _, _)| c)
            .unwrap()
    }

    #[test]
    fn nothing_set_means_no_flag_is_set() {
        let f = cfg(&[]).env_flags;
        assert!(
            !f.signup_set
                && !f.signup_explicit_closed
                && !f.signup_quota_set
                && !f.job_retention_days_set
                && !f.terms_set,
            "{f:?}"
        );
        // blank is unset, as the parsers read it
        let f = cfg(&[
            ("PRINTAPI_SIGNUP_QUOTA", "  "),
            ("PRINTAPI_JOB_RETENTION_DAYS", ""),
        ])
        .env_flags;
        assert!(!f.signup_quota_set && !f.job_retention_days_set, "{f:?}");
    }

    #[test]
    fn the_flags_follow_the_variables() {
        let f = cfg(&[("PRINTAPI_SIGNUP", "closed")]).env_flags;
        assert!(f.signup_set && f.signup_explicit_closed);
        let f = cfg(&[("PRINTAPI_SIGNUP", "open")]).env_flags;
        assert!(f.signup_set && !f.signup_explicit_closed);
        let f = cfg(&[
            ("PRINTAPI_SIGNUP_QUOTA", "5"),
            ("PRINTAPI_JOB_RETENTION_DAYS", "0"),
            ("PRINTAPI_TERMS_VERSION", "v1"),
            ("PRINTAPI_TERMS_URL", "https://x.test/t"),
        ])
        .env_flags;
        assert!(f.signup_quota_set && f.job_retention_days_set && f.terms_set);
    }

    #[test]
    fn any_signup_value_but_open_locks_signup_shut() {
        for v in [
            "closed",
            "CLOSED",
            "Closed",
            "off",
            "0",
            "closd",
            "\"closed\"",
            "no",
        ] {
            let c = cfg(&[("PRINTAPI_SIGNUP", v)]);
            assert!(
                c.env_flags.signup_set && c.env_flags.signup_explicit_closed,
                "{v:?}"
            );
            assert!(!c.signup_open, "{v:?} never opens signup");
        }
        // padding does not matter, blank is unset, `open` opens and never locks
        let f = cfg(&[("PRINTAPI_SIGNUP", "  closed ")]).env_flags;
        assert!(f.signup_explicit_closed);
        for v in ["", "   "] {
            let f = cfg(&[("PRINTAPI_SIGNUP", v)]).env_flags;
            assert!(!f.signup_explicit_closed, "{v:?}");
        }
        let c = cfg(&[("PRINTAPI_SIGNUP", "open")]);
        assert!(c.signup_open && !c.env_flags.signup_explicit_closed);
    }

    #[test]
    fn a_terms_version_with_a_control_character_stops_the_server() {
        let vars = [
            ("PRINTAPI_TERMS_VERSION", "v1\u{1b}[2J"),
            ("PRINTAPI_TERMS_URL", "https://x.test/t"),
        ];
        let mut all: Vec<(String, String)> = vec![("PRINTAPI_TOKEN".into(), "root".into())];
        all.extend(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let e = config_from(&|k| all.iter().rfind(|(n, _)| n == k).map(|(_, v)| v.clone()))
            .unwrap_err();
        assert!(e.contains("PRINTAPI_TERMS_VERSION"), "{e:?}");
        assert!(!e.contains('\u{1b}'), "{e:?}");
    }

    #[test]
    fn the_open_signup_warnings_read_the_effective_values() {
        let c = cfg(&[]);
        let mut live = printpapi_server::settings::build_live(&c.env_start(), false, &[]).0;
        assert!(startup_warnings(&live).is_empty());
        // the database opened signup: all three safeguards are missing
        live.signup = Sourced {
            value: true,
            source: Source::Db,
        };
        assert_eq!(startup_warnings(&live).len(), 3);
        // the cause may be the console, so no warning blames the variable alone
        for w in startup_warnings(&live) {
            assert!(w.starts_with("warning: signup is open"), "{w}");
            assert!(!w.contains("PRINTAPI_SIGNUP=open"), "{w}");
            assert!(w.contains("operator console"), "{w}");
        }
        // an override that supplies them silences the warning
        live.signup_quota.value = Some(5);
        live.job_retention_days.value = Some(30);
        assert_eq!(startup_warnings(&live).len(), 1);
        // a database close beats an env open: nothing to warn about
        live.signup.value = false;
        assert!(startup_warnings(&live).is_empty());
    }
}
