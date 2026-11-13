// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `printpapi` — the server binary. Configured from the environment exactly like the Python
//! server: PRINTAPI_TOKEN (required, non-empty), PRINT_DB (default printpapi.db), PRINT_PORT
//! (default 3460), PRINTAPI_SIGNUP, PUBLIC_URL, PRINTAPI_PLANS (JSON array or a path to a file
//! holding one), PRINTAPI_BILLING_SECRET, PRINTAPI_TRUSTED_PROXIES (IPs or CIDRs whose
//! `X-Forwarded-For` is believed), PRINTAPI_SIGNUP_QUOTA (monthly job quota of every self-signup
//! org), LOG_REQUESTS, SMTP_*.
use printpapi_server::dispatch::{Destinations, HttpFetcher, OUTBOUND_IN_FLIGHT};
use printpapi_server::mail::{self, SmtpMailer};
use printpapi_server::server::{AppState, Config};
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

/// One stderr line per hosted-beta safeguard that is off while strangers can sign up.
fn startup_warnings(cfg: &Config) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfg.signup_open && cfg.signup_quota.is_none() {
        out.push(
            "warning: PRINTAPI_SIGNUP=open without PRINTAPI_SIGNUP_QUOTA: new orgs can print without limit",
        );
    }
    out
}

fn config() -> Result<(Config, String, u16), String> {
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
    // Private destinations are refused by default exactly when strangers get accounts (open
    // signup); a self-hosted box keeps fetching from its LAN.
    let private = env("PRINTAPI_PRIVATE_URLS").unwrap_or_default();
    cfg.destinations = match py_strip(&private) {
        "" if cfg.signup_open => Destinations::PublicOnly,
        "" | "allow" => Destinations::Any,
        "deny" => Destinations::PublicOnly,
        other => {
            return Err(format!(
                "PRINTAPI_PRIVATE_URLS must be allow or deny, not {other:?}"
            ));
        }
    };
    cfg.trusted_proxies = printpapi_server::proxy::TrustedProxies::parse(
        &env("PRINTAPI_TRUSTED_PROXIES").unwrap_or_default(),
    )
    .map_err(|e| format!("PRINTAPI_TRUSTED_PROXIES: {e}"))?;
    cfg.signup_quota = parse_quota(&env("PRINTAPI_SIGNUP_QUOTA").unwrap_or_default())
        .map_err(|e| format!("PRINTAPI_SIGNUP_QUOTA: {e}"))?;
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
