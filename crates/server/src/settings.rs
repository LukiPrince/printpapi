// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Live settings, the pure side (spec A5): the env parsers `main` and the operator API share, the
//! effective values with their source, and the validation of a `PUT /operator/settings` body. No
//! IO: the rows come from `store`, the stderr notes are returned for `main` to print.
use crate::server::Terms;
use serde_json::{Map, Value as Json, json};

/// Python `str.strip()`: Unicode whitespace plus U+001C..U+001F.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Empty: no quota. Otherwise a non-negative integer (0 is a real quota: nothing may be printed).
pub fn parse_quota(raw: &str) -> Result<Option<i64>, String> {
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
pub fn parse_fetch_cap(raw: &str) -> Result<Option<usize>, String> {
    let t = py_strip(raw);
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<usize>()
        .map(Some)
        .map_err(|_| format!("must be a non-negative integer, not {t:?}"))
}

/// Empty or 0: keep for ever. Otherwise a positive number of days.
pub fn parse_retention(raw: &str) -> Result<Option<i64>, String> {
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
pub fn is_terms_url(u: &str) -> bool {
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

/// Longest terms version, in characters.
pub const TERMS_VERSION_MAX: usize = 100;

/// The terms a signup must accept. A version needs a terms URL and has no control character (it
/// lands in stderr and `GET /health`) and at most `TERMS_VERSION_MAX` characters. Every link that
/// is set must be an absolute `http(s)` URL with a host, version or not (`is_terms_url` already
/// refuses control characters and whitespace).
pub fn parse_terms(
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
    if version.chars().any(char::is_control) {
        return Err("PRINTAPI_TERMS_VERSION must not contain control characters".into());
    }
    if version.chars().count() > TERMS_VERSION_MAX {
        return Err(format!(
            "PRINTAPI_TERMS_VERSION must be at most {TERMS_VERSION_MAX} characters"
        ));
    }
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

/// Does this `PRINTAPI_SIGNUP` value lock signup shut (the emergency brake)? Any present, non-blank
/// value that is not exactly `open` does, so a near miss (`CLOSED`, `off`, `closd`) fails closed.
pub fn signup_env_locks(raw: &str) -> bool {
    let t = py_strip(raw);
    !t.is_empty() && t != "open"
}

/// The four keys an operator may change at runtime; everything else stays env-only.
pub const LIVE_KEYS: [&str; 4] = ["signup", "signup_quota", "job_retention_days", "terms"];

/// Where an effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An override row.
    Db,
    /// The variable is set.
    Env,
    /// `PRINTAPI_SIGNUP` set to anything but `open`: the emergency brake, no override applies.
    EnvLocked,
    /// Neither a row nor a variable.
    Default,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Db => "db",
            Source::Env => "env",
            Source::EnvLocked => "env-locked",
            Source::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
}

/// The effective live values. One of these is a snapshot; it is replaced whole, never edited.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub signup: Sourced<bool>,
    pub signup_quota: Sourced<Option<i64>>,
    pub job_retention_days: Sourced<Option<i64>>,
    pub terms: Sourced<Option<Terms>>,
    /// Keys whose stored row is not applied (env-locked, the private-URL rule, or a value that no
    /// longer validates), with the row as stored (as the API shows it, else the raw text).
    pub ignored: Vec<(&'static str, Json)>,
}

/// The values the environment gave at startup and whether each variable was set at all.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnvStart {
    pub signup_open: bool,
    pub signup_set: bool,
    /// `PRINTAPI_SIGNUP` is present and not `open` (`signup_env_locks`): unset means closed by
    /// default, not a lock.
    pub signup_explicit_closed: bool,
    pub signup_quota: Option<i64>,
    pub signup_quota_set: bool,
    pub job_retention_days: Option<i64>,
    pub job_retention_days_set: bool,
    pub terms: Option<Terms>,
    pub terms_set: bool,
}

/// Which startup variables were set at all (a default and an explicit value can be equal).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnvFlags {
    pub signup_set: bool,
    /// `PRINTAPI_SIGNUP` is present, not blank and not exactly `open` (`signup_env_locks`).
    pub signup_explicit_closed: bool,
    pub signup_quota_set: bool,
    pub job_retention_days_set: bool,
    pub terms_set: bool,
}

/// One validated override value.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Signup(bool),
    SignupQuota(i64),
    JobRetentionDays(i64),
    Terms(Terms),
}

impl Parsed {
    pub fn key(&self) -> &'static str {
        match self {
            Parsed::Signup(_) => "signup",
            Parsed::SignupQuota(_) => "signup_quota",
            Parsed::JobRetentionDays(_) => "job_retention_days",
            Parsed::Terms(_) => "terms",
        }
    }

    /// The value as the API shows it (and the audit detail holds it).
    pub fn to_json(&self) -> Json {
        match self {
            Parsed::Signup(open) => json!(if *open { "open" } else { "closed" }),
            Parsed::SignupQuota(n) | Parsed::JobRetentionDays(n) => json!(n),
            Parsed::Terms(t) => terms_json(t),
        }
    }

    /// The text of the `settings.value` column.
    pub fn encode(&self) -> String {
        match self {
            Parsed::Signup(open) => (if *open { "open" } else { "closed" }).to_string(),
            Parsed::SignupQuota(n) | Parsed::JobRetentionDays(n) => n.to_string(),
            Parsed::Terms(t) => terms_json(t).to_string(),
        }
    }

    /// A stored value, checked by the same rules as a request.
    pub fn decode(key: &str, raw: &str) -> Result<Parsed, String> {
        match key {
            "signup" => signup_of(raw),
            "signup_quota" | "job_retention_days" => Self::from_json(
                key,
                &json!(
                    raw.trim()
                        .parse::<i64>()
                        .map_err(|_| { "must be an integer".to_string() })?
                ),
            ),
            "terms" => Self::from_json(
                key,
                &serde_json::from_str::<Json>(raw).map_err(|_| "not valid JSON".to_string())?,
            ),
            _ => Err(format!("unknown setting {key:?}")),
        }
    }

    /// A request value, checked by the env parsers' rules.
    pub fn from_json(key: &str, v: &Json) -> Result<Parsed, String> {
        let int = || {
            v.as_i64()
                .map(|n| n.to_string())
                .ok_or_else(|| "must be an integer".to_string())
        };
        match key {
            "signup" => signup_of(v.as_str().ok_or("must be \"open\" or \"closed\"")?),
            "signup_quota" => parse_quota(&int()?)?
                .map(Parsed::SignupQuota)
                .ok_or_else(|| "must be a non-negative integer".into()),
            "job_retention_days" => parse_retention(&int()?)?
                .map(Parsed::JobRetentionDays)
                .ok_or_else(|| "must be a positive integer (unset the key to go back)".into()),
            "terms" => {
                let o = v.as_object().ok_or("must be an object")?;
                if let Some(k) = o.keys().find(|k| {
                    !["version", "terms_url", "dpa_url", "privacy_url"].contains(&k.as_str())
                }) {
                    return Err(format!("unknown field {k:?}"));
                }
                let text = |name: &str| match o.get(name) {
                    None => Ok(""),
                    Some(Json::String(s)) => Ok(s.as_str()),
                    Some(_) => Err(format!("{name} must be a string")),
                };
                let version = text("version")?;
                if py_strip(version).is_empty() {
                    return Err("version must not be empty".into());
                }
                parse_terms(
                    version,
                    text("terms_url")?,
                    text("dpa_url")?,
                    text("privacy_url")?,
                )?
                .map(Parsed::Terms)
                .ok_or_else(|| "version must not be empty".into())
            }
            _ => Err(format!("unknown setting {key:?}")),
        }
    }
}

fn signup_of(s: &str) -> Result<Parsed, String> {
    match s {
        "open" => Ok(Parsed::Signup(true)),
        "closed" => Ok(Parsed::Signup(false)),
        _ => Err("must be \"open\" or \"closed\"".into()),
    }
}

/// The terms as the API shows them: absent links are left out.
pub fn terms_json(t: &Terms) -> Json {
    let mut m = Map::new();
    m.insert("version".into(), json!(t.version));
    m.insert("terms_url".into(), json!(t.terms_url));
    if let Some(u) = &t.dpa_url {
        m.insert("dpa_url".into(), json!(u));
    }
    if let Some(u) = &t.privacy_url {
        m.insert("privacy_url".into(), json!(u));
    }
    Json::Object(m)
}

/// The effective values from the startup env, the effective destinations policy and the override
/// rows (key, stored text), plus one stderr note per applied or ignored override. A row that no
/// longer validates, a `signup = open` while private URLs are allowed, and any signup row while
/// `PRINTAPI_SIGNUP` locks signup are ignored (and listed in `Live::ignored`). Notes never hold a
/// secret and print every user-supplied value with `{:?}`.
pub fn build_live(
    env: &EnvStart,
    destinations_any: bool,
    rows: &[(String, String)],
) -> (Live, Vec<String>) {
    let src = |set: bool| if set { Source::Env } else { Source::Default };
    let mut live = Live {
        signup: Sourced {
            value: env.signup_open,
            source: if env.signup_explicit_closed {
                Source::EnvLocked
            } else {
                src(env.signup_set)
            },
        },
        signup_quota: Sourced {
            value: env.signup_quota,
            source: src(env.signup_quota_set),
        },
        job_retention_days: Sourced {
            value: env.job_retention_days,
            source: src(env.job_retention_days_set),
        },
        terms: Sourced {
            value: env.terms.clone(),
            source: src(env.terms_set),
        },
        ignored: Vec::new(),
    };
    let opt = |v: Option<i64>| v.map_or("unset".to_string(), |n| n.to_string());
    let env_says = |key: &str| match key {
        "signup" if env.signup_set => (if env.signup_open { "open" } else { "closed" }).to_string(),
        "signup_quota" => opt(env.signup_quota),
        "job_retention_days" => opt(env.job_retention_days),
        "terms" => env
            .terms
            .as_ref()
            .map_or("unset".to_string(), |t| format!("version {:?}", t.version)),
        _ => "unset".to_string(),
    };
    let mut notes = Vec::new();
    for key in LIVE_KEYS {
        let Some((_, raw)) = rows.iter().find(|(k, _)| k == key) else {
            continue;
        };
        let parsed = match Parsed::decode(key, raw) {
            Ok(p) => p,
            Err(e) => {
                notes.push(format!(
                    "setting {key}: ignoring the database value ({e}); the environment value applies"
                ));
                live.ignored.push((key, json!(raw)));
                continue;
            }
        };
        if let Parsed::Signup(open) = &parsed {
            if env.signup_explicit_closed {
                notes.push(
                    "setting signup: ignoring the database value, PRINTAPI_SIGNUP locks signup closed"
                        .to_string(),
                );
                live.ignored.push((key, parsed.to_json()));
                continue;
            }
            if *open && destinations_any {
                notes.push(
                    "setting signup: ignoring the database value open, private URLs are allowed"
                        .to_string(),
                );
                live.ignored.push((key, parsed.to_json()));
                continue;
            }
        }
        let shown = match &parsed {
            Parsed::Terms(t) => format!("version {:?}", t.version),
            other => other.encode(),
        };
        notes.push(format!(
            "setting {key} = {shown} (from the database, env says {})",
            env_says(key)
        ));
        match parsed {
            Parsed::Signup(v) => {
                live.signup = Sourced {
                    value: v,
                    source: Source::Db,
                }
            }
            Parsed::SignupQuota(n) => {
                live.signup_quota = Sourced {
                    value: Some(n),
                    source: Source::Db,
                }
            }
            Parsed::JobRetentionDays(n) => {
                live.job_retention_days = Sourced {
                    value: Some(n),
                    source: Source::Db,
                }
            }
            Parsed::Terms(t) => {
                live.terms = Sourced {
                    value: Some(t),
                    source: Source::Db,
                }
            }
        }
    }
    (live, notes)
}

/// A validated change: the values to write and the keys to remove, in `LIVE_KEYS` order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Change {
    pub set: Vec<Parsed>,
    pub unset: Vec<&'static str>,
}

/// A change that is well formed but not allowed right now (409 at the HTTP layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// The change would leave signup open while private URLs are allowed.
    OpenWithPrivateUrls,
    /// `PRINTAPI_SIGNUP` locks signup shut: only a change that closes it is accepted.
    SignupLocked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PutError {
    /// Malformed or unknown (400).
    Invalid(String),
    /// A write that cannot be undone needs a reason of at least `REASON_MIN` characters (400).
    ReasonNeeded,
    Conflict(Conflict),
}

/// Characters of reason an irreversible write needs.
pub const REASON_MIN: usize = 8;

/// Check a `PUT /operator/settings` body (`{set, unset, reason?}`) against the effective values.
/// `reason` is the request's reason, already length-checked by the caller. Nothing is decided
/// from the body beyond the checks here; the caller writes `Change` and rebuilds `Live` from the
/// rows afterwards.
pub fn validate_put(
    body: &Map<String, Json>,
    reason: Option<&str>,
    live: &Live,
    env: &EnvStart,
    destinations_any: bool,
) -> Result<Change, PutError> {
    let bad = |m: String| PutError::Invalid(m);
    if let Some(k) = body
        .keys()
        .find(|k| !["set", "unset", "reason"].contains(&k.as_str()))
    {
        return Err(bad(format!("unknown field {k:?}")));
    }
    let empty = Map::new();
    let set = match body.get("set") {
        None => &empty,
        Some(Json::Object(m)) => m,
        Some(_) => return Err(bad("set must be an object".into())),
    };
    if let Some(k) = set.keys().find(|k| !LIVE_KEYS.contains(&k.as_str())) {
        return Err(bad(format!("unknown setting {k:?}")));
    }
    let mut unset: Vec<&'static str> = Vec::new();
    match body.get("unset") {
        None => {}
        Some(Json::Array(a)) => {
            for v in a {
                let name = v
                    .as_str()
                    .ok_or_else(|| bad("unset must be a list of keys".into()))?;
                let key = LIVE_KEYS
                    .iter()
                    .find(|k| **k == name)
                    .ok_or_else(|| bad(format!("unknown setting {name:?}")))?;
                unset.push(key);
            }
        }
        Some(_) => return Err(bad("unset must be a list of keys".into())),
    }
    if set.is_empty() && unset.is_empty() {
        return Err(bad("nothing to change: give set or unset".into()));
    }
    let mut parsed = Vec::new();
    for key in LIVE_KEYS {
        let Some(v) = set.get(key) else { continue };
        if unset.contains(&key) {
            return Err(bad(format!("{key} is both in set and in unset")));
        }
        if v.is_null() {
            return Err(bad(format!(
                "{key}: null is not a value, unset the key to go back to the environment"
            )));
        }
        parsed.push(Parsed::from_json(key, v).map_err(|e| bad(format!("{key}: {e}")))?);
    }
    unset.sort_by_key(|k| LIVE_KEYS.iter().position(|l| l == k));
    unset.dedup();
    // well formed; now what the current state allows
    // What signup ends up as, worked out as startup would (build_live): a set value, or with
    // `unset` the env one. A PUT that does not touch signup is never refused here.
    let new_signup = parsed.iter().find_map(|p| match p {
        Parsed::Signup(open) => Some(*open),
        _ => None,
    });
    let touches_signup = new_signup.is_some() || unset.contains(&"signup");
    if touches_signup {
        if live.signup.source == Source::EnvLocked {
            // the brake: only changes that can close signup get through
            if new_signup == Some(true) {
                return Err(PutError::Conflict(Conflict::SignupLocked));
            }
        } else if new_signup.unwrap_or(env.signup_open) && destinations_any {
            return Err(PutError::Conflict(Conflict::OpenWithPrivateUrls));
        }
    }
    // the next retention pass scrubs payloads and titles for good: say why
    let irreversible = parsed.iter().any(|p| match p {
        Parsed::JobRetentionDays(n) => live.job_retention_days.value.is_none_or(|cur| *n < cur),
        _ => false,
    });
    if irreversible && !reason_is_enough(reason) {
        return Err(PutError::ReasonNeeded);
    }
    Ok(Change { set: parsed, unset })
}

/// Would dropping the override of `job_retention_days` (`change.unset`) lower the effective
/// retention, or enable it over keep-for-ever? The same irreversible write as a `set` that does,
/// so it needs the same reason. `env` is the value it falls back to.
pub fn unset_needs_reason(change: &Change, live: &Live, env: &EnvStart) -> bool {
    change.unset.contains(&"job_retention_days")
        && live.job_retention_days.source == Source::Db
        && env
            .job_retention_days
            .is_some_and(|e| live.job_retention_days.value.is_none_or(|cur| e < cur))
}

/// Does this reason satisfy `REASON_MIN`?
pub fn reason_is_enough(reason: Option<&str>) -> bool {
    reason.is_some_and(|r| r.trim().chars().count() >= REASON_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(v: &str) -> Terms {
        Terms {
            version: v.into(),
            terms_url: "https://x.test/t".into(),
            dpa_url: None,
            privacy_url: None,
        }
    }

    fn rows(r: &[(&str, &str)]) -> Vec<(String, String)> {
        r.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn env() -> EnvStart {
        EnvStart::default()
    }

    // ---- build_live ----

    #[test]
    fn no_rows_means_the_env_values_with_env_or_default_as_source() {
        let (live, notes) = build_live(&env(), false, &[]);
        assert!(notes.is_empty());
        assert_eq!(live.signup.source, Source::Default);
        assert_eq!(live.signup_quota.source, Source::Default);
        let e = EnvStart {
            signup_open: true,
            signup_set: true,
            signup_quota: Some(50),
            signup_quota_set: true,
            job_retention_days: Some(30),
            job_retention_days_set: true,
            terms: Some(terms("v1")),
            terms_set: true,
            ..env()
        };
        let (live, _) = build_live(&e, false, &[]);
        assert_eq!(
            live.signup,
            Sourced {
                value: true,
                source: Source::Env
            }
        );
        assert_eq!(
            live.signup_quota,
            Sourced {
                value: Some(50),
                source: Source::Env
            }
        );
        assert_eq!(live.job_retention_days.source, Source::Env);
        assert_eq!(live.terms.value, Some(terms("v1")));
        assert_eq!(live.terms.source, Source::Env);
    }

    #[test]
    fn a_row_overrides_the_env_value_and_says_so() {
        let e = EnvStart {
            signup_quota: Some(50),
            signup_quota_set: true,
            ..env()
        };
        let t = r#"{"version":"v2","terms_url":"https://x.test/t2","dpa_url":"https://x.test/d"}"#;
        let (live, notes) = build_live(
            &e,
            false,
            &rows(&[
                ("signup", "open"),
                ("signup_quota", "5"),
                ("job_retention_days", "14"),
                ("terms", t),
            ]),
        );
        assert_eq!(
            live.signup,
            Sourced {
                value: true,
                source: Source::Db
            }
        );
        assert_eq!(
            live.signup_quota,
            Sourced {
                value: Some(5),
                source: Source::Db
            }
        );
        assert_eq!(
            live.job_retention_days,
            Sourced {
                value: Some(14),
                source: Source::Db
            }
        );
        let tv = live.terms.value.clone().unwrap();
        assert_eq!(
            (tv.version.as_str(), tv.dpa_url.as_deref()),
            ("v2", Some("https://x.test/d"))
        );
        assert_eq!(live.terms.source, Source::Db);
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(
            notes
                .contains(&"setting signup = open (from the database, env says unset)".to_string()),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("signup_quota = 5") && n.contains("env says 50"))
        );
    }

    #[test]
    fn an_explicit_env_closed_beats_a_db_open() {
        let e = EnvStart {
            signup_set: true,
            signup_explicit_closed: true,
            ..env()
        };
        let (live, notes) = build_live(&e, false, &rows(&[("signup", "open")]));
        assert_eq!(
            live.signup,
            Sourced {
                value: false,
                source: Source::EnvLocked
            }
        );
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("ignor") && notes[0].contains("PRINTAPI_SIGNUP"),
            "{notes:?}"
        );
        // locked without any row, too
        let (live, notes) = build_live(&e, false, &[]);
        assert_eq!(live.signup.source, Source::EnvLocked);
        assert!(notes.is_empty());
    }

    #[test]
    fn an_unset_env_signup_is_closed_by_default_and_not_a_lock() {
        let (live, _) = build_live(&env(), false, &rows(&[("signup", "open")]));
        assert_eq!(
            live.signup,
            Sourced {
                value: true,
                source: Source::Db
            }
        );
    }

    #[test]
    fn a_db_open_is_ignored_while_private_urls_are_allowed() {
        let (live, notes) = build_live(&env(), true, &rows(&[("signup", "open")]));
        assert_eq!(
            live.signup,
            Sourced {
                value: false,
                source: Source::Default
            }
        );
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("ignor") && notes[0].contains("private"),
            "{notes:?}"
        );
        // closing is always fine
        let e = EnvStart {
            signup_open: true,
            signup_set: true,
            ..env()
        };
        let (live, _) = build_live(&e, true, &rows(&[("signup", "closed")]));
        assert_eq!(
            live.signup,
            Sourced {
                value: false,
                source: Source::Db
            }
        );
    }

    #[test]
    fn a_row_that_no_longer_validates_is_ignored_with_a_note() {
        let e = EnvStart {
            signup_quota: Some(9),
            signup_quota_set: true,
            ..env()
        };
        let (live, notes) = build_live(
            &e,
            false,
            &rows(&[
                ("signup", "maybe"),
                ("signup_quota", "-3"),
                ("job_retention_days", "0"),
                ("terms", r#"{"version":"v1","terms_url":"ftp://x"}"#),
            ]),
        );
        assert_eq!(live.signup.source, Source::Default);
        assert_eq!(
            live.signup_quota,
            Sourced {
                value: Some(9),
                source: Source::Env
            }
        );
        assert_eq!(live.job_retention_days.source, Source::Default);
        assert_eq!(live.terms.source, Source::Default);
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes.iter().all(|n| n.contains("ignor")));
    }

    #[test]
    fn unknown_keys_in_the_rows_change_nothing() {
        let (live, notes) = build_live(&env(), false, &rows(&[("fetches_per_org", "9")]));
        assert_eq!(live, build_live(&env(), false, &[]).0);
        assert!(notes.is_empty());
    }

    #[test]
    fn ignored_rows_are_listed_with_their_stored_value() {
        let locked = EnvStart {
            signup_set: true,
            signup_explicit_closed: true,
            ..env()
        };
        let l = build_live(&locked, false, &rows(&[("signup", "open")])).0;
        assert_eq!(l.ignored, vec![("signup", json!("open"))]);
        let l = build_live(&env(), true, &rows(&[("signup", "open")])).0;
        assert_eq!(l.ignored, vec![("signup", json!("open"))]);
        let l = build_live(&env(), false, &rows(&[("signup_quota", "-3")])).0;
        assert_eq!(l.ignored, vec![("signup_quota", json!("-3"))]);
        // an applied row is not ignored
        let l = build_live(&env(), false, &rows(&[("signup", "open")])).0;
        assert!(l.ignored.is_empty());
    }

    #[test]
    fn the_terms_version_has_no_control_character_and_at_most_100_characters() {
        let ok = |v: &str| parse_terms(v, "https://x.test/t", "", "");
        assert!(ok("v1").unwrap().is_some());
        assert!(ok(&"a".repeat(100)).is_ok());
        assert!(ok(&"a".repeat(101)).is_err());
        assert!(ok(&"é".repeat(100)).is_ok(), "characters, not bytes");
        for v in ["v1\u{1b}[2J", "v1\nx", "a\tb", "a\u{7f}b", "a\u{85}b"] {
            let e = ok(v).unwrap_err();
            assert!(e.contains("control"), "{v:?}: {e}");
        }
        // padding is stripped before the check, links with a control character are refused
        assert!(ok(" v1\n").is_ok());
        assert!(parse_terms("v1", "https://x.test/\u{1b}", "", "").is_err());
        assert!(parse_terms("", "", "https://x.test/a\nb", "").is_err());
    }

    // ---- stored text and request values ----

    #[test]
    fn values_round_trip_through_the_stored_text() {
        let t = Terms {
            version: "v3".into(),
            terms_url: "https://x.test/t".into(),
            dpa_url: Some("https://x.test/d".into()),
            privacy_url: Some("http://x.test/p".into()),
        };
        for p in [
            Parsed::Signup(true),
            Parsed::Signup(false),
            Parsed::SignupQuota(0),
            Parsed::JobRetentionDays(7),
            Parsed::Terms(t),
        ] {
            assert_eq!(Parsed::decode(p.key(), &p.encode()), Ok(p.clone()), "{p:?}");
            assert_eq!(
                Parsed::from_json(p.key(), &p.to_json()),
                Ok(p.clone()),
                "{p:?}"
            );
        }
    }

    #[test]
    fn request_values_follow_the_env_parsers_rules() {
        let ok = |k: &str, v: Json| Parsed::from_json(k, &v);
        assert_eq!(ok("signup", json!("open")), Ok(Parsed::Signup(true)));
        assert!(ok("signup", json!("Open")).is_err());
        assert!(ok("signup", json!(true)).is_err());
        assert_eq!(ok("signup_quota", json!(0)), Ok(Parsed::SignupQuota(0)));
        assert!(ok("signup_quota", json!(-1)).is_err());
        assert!(ok("signup_quota", json!("5")).is_err());
        assert!(ok("signup_quota", json!(1.5)).is_err());
        assert_eq!(
            ok("job_retention_days", json!(30)),
            Ok(Parsed::JobRetentionDays(30))
        );
        assert!(
            ok("job_retention_days", json!(0)).is_err(),
            "0 is unset, not a value"
        );
        assert!(ok("job_retention_days", json!(-2)).is_err());
        let t = |v: Json| ok("terms", v);
        assert!(t(json!({"version":"v1","terms_url":"https://x.test/t"})).is_ok());
        assert!(t(json!({"version":"","terms_url":"https://x.test/t"})).is_err());
        assert!(t(json!({"version":"  ","terms_url":"https://x.test/t"})).is_err());
        assert!(t(json!({"version":"v1"})).is_err(), "terms_url is required");
        assert!(t(json!({"version":"v1","terms_url":"ftp://x/t"})).is_err());
        assert!(
            t(json!({"version":"v1","terms_url":"https://x.test/t","dpa_url":"javascript:x"}))
                .is_err()
        );
        assert!(t(json!({"version":"v1","terms_url":"https://x.test/t","extra":1})).is_err());
        assert!(t(json!({"version":1,"terms_url":"https://x.test/t"})).is_err());
        assert!(t(json!("v1")).is_err());
    }

    // ---- validate_put ----

    fn put(body: Json, reason: Option<&str>, live: &Live, any: bool) -> Result<Change, PutError> {
        put_env(body, reason, live, &env(), any)
    }

    fn put_env(
        body: Json,
        reason: Option<&str>,
        live: &Live,
        env: &EnvStart,
        any: bool,
    ) -> Result<Change, PutError> {
        validate_put(body.as_object().unwrap(), reason, live, env, any)
    }

    fn base() -> Live {
        build_live(&env(), false, &[]).0
    }

    fn invalid(r: Result<Change, PutError>) -> bool {
        matches!(r, Err(PutError::Invalid(_)))
    }

    #[test]
    fn a_valid_change_comes_back_in_key_order() {
        let c = put(
            json!({"set": {"signup_quota": 10, "signup": "open"}, "unset": ["terms"]}),
            None,
            &base(),
            false,
        )
        .unwrap();
        assert_eq!(c.set, vec![Parsed::Signup(true), Parsed::SignupQuota(10)]);
        assert_eq!(c.unset, vec!["terms"]);
    }

    #[test]
    fn malformed_requests_are_invalid() {
        let l = base();
        assert!(invalid(put(json!({}), None, &l, false)), "empty");
        assert!(
            invalid(put(json!({"set": {}, "unset": []}), None, &l, false)),
            "empty lists"
        );
        assert!(
            invalid(put(json!({"set": {"fetches_per_org": 3}}), None, &l, false)),
            "unknown key"
        );
        assert!(
            invalid(put(json!({"unset": ["nope"]}), None, &l, false)),
            "unknown unset key"
        );
        assert!(
            invalid(put(json!({"set": {"signup": null}}), None, &l, false)),
            "null"
        );
        assert!(
            invalid(put(json!({"set": {"signup_quota": "5"}}), None, &l, false)),
            "wrong type"
        );
        assert!(
            invalid(put(json!({"set": [1]}), None, &l, false)),
            "set not an object"
        );
        assert!(
            invalid(put(json!({"unset": "signup"}), None, &l, false)),
            "unset not a list"
        );
        assert!(
            invalid(put(json!({"unset": [1]}), None, &l, false)),
            "unset entry not a string"
        );
        assert!(
            invalid(put(
                json!({"unset": ["terms"], "extra": 1}),
                None,
                &l,
                false
            )),
            "unknown top key"
        );
        assert!(
            invalid(put(
                json!({"set": {"signup_quota": 1}, "unset": ["signup_quota"]}),
                None,
                &l,
                false
            )),
            "both set and unset"
        );
    }

    #[test]
    fn open_signup_is_refused_while_private_urls_are_allowed() {
        let l = base();
        assert_eq!(
            put(json!({"set": {"signup": "open"}}), None, &l, true),
            Err(PutError::Conflict(Conflict::OpenWithPrivateUrls))
        );
        assert!(put(json!({"set": {"signup": "closed"}}), None, &l, true).is_ok());
        assert!(put(json!({"set": {"signup": "open"}}), None, &l, false).is_ok());
    }

    #[test]
    fn unset_cannot_reopen_signup_while_private_urls_are_allowed() {
        let e = EnvStart {
            signup_open: true,
            signup_set: true,
            ..env()
        };
        let rows = rows(&[("signup", "closed")]);
        let l = build_live(&e, true, &rows).0;
        assert!(!l.signup.value);
        assert_eq!(
            put_env(json!({"unset": ["signup"]}), None, &l, &e, true),
            Err(PutError::Conflict(Conflict::OpenWithPrivateUrls))
        );
        // not touching signup, closing it, or an env that says closed: all fine
        assert!(put_env(json!({"set": {"signup_quota": 2}}), None, &l, &e, true).is_ok());
        assert!(put_env(json!({"set": {"signup": "closed"}}), None, &l, &e, true).is_ok());
        assert!(put_env(json!({"unset": ["signup"]}), None, &l, &env(), true).is_ok());
        assert!(put_env(json!({"unset": ["signup"]}), None, &l, &e, false).is_ok());
    }

    #[test]
    fn an_env_locked_signup_accepts_only_changes_that_close_it() {
        let e = EnvStart {
            signup_set: true,
            signup_explicit_closed: true,
            ..env()
        };
        let l = build_live(&e, false, &[]).0;
        for body in [
            json!({"set": {"signup": "open"}}),
            json!({"set": {"signup": "open", "signup_quota": 1}}),
        ] {
            assert_eq!(
                put(body.clone(), None, &l, false),
                Err(PutError::Conflict(Conflict::SignupLocked)),
                "{body}"
            );
        }
        assert!(put(json!({"set": {"signup": "closed"}}), None, &l, false).is_ok());
        assert!(put(json!({"unset": ["signup"]}), None, &l, false).is_ok());
        // the other keys stay writable
        assert!(put(json!({"set": {"signup_quota": 1}}), None, &l, false).is_ok());
    }

    #[test]
    fn malformed_input_is_reported_before_a_conflict() {
        let e = EnvStart {
            signup_set: true,
            signup_explicit_closed: true,
            ..env()
        };
        let l = build_live(&e, false, &[]).0;
        assert!(invalid(put(
            json!({"set": {"signup": "open", "terms": 3}}),
            None,
            &l,
            false
        )));
    }

    #[test]
    fn enabling_retention_over_keep_for_ever_needs_a_reason() {
        let l = base(); // retention None
        let body = json!({"set": {"job_retention_days": 30}});
        assert_eq!(
            put(body.clone(), None, &l, false),
            Err(PutError::ReasonNeeded)
        );
        assert_eq!(
            put(body.clone(), Some("short"), &l, false),
            Err(PutError::ReasonNeeded)
        );
        assert_eq!(
            put(body.clone(), Some("  1234567  "), &l, false),
            Err(PutError::ReasonNeeded)
        );
        assert!(put(body, Some("12345678"), &l, false).is_ok());
    }

    #[test]
    fn lowering_retention_needs_a_reason_raising_or_keeping_does_not() {
        let e = EnvStart {
            job_retention_days: Some(30),
            job_retention_days_set: true,
            ..env()
        };
        let l = build_live(&e, false, &[]).0;
        let set = |n: i64| json!({"set": {"job_retention_days": n}});
        assert_eq!(put(set(29), None, &l, false), Err(PutError::ReasonNeeded));
        assert!(put(set(29), Some("ticket 4711"), &l, false).is_ok());
        assert!(put(set(30), None, &l, false).is_ok());
        assert!(put(set(90), None, &l, false).is_ok());
        // other keys never need one
        assert!(
            put(
                json!({"set": {"signup_quota": 0}, "unset": ["job_retention_days"]}),
                None,
                &l,
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn the_reason_rule_reads_the_effective_value_not_the_env_one() {
        let rows = rows(&[("job_retention_days", "10")]);
        let l = build_live(&env(), false, &rows).0;
        assert_eq!(l.job_retention_days.value, Some(10));
        assert_eq!(
            put(json!({"set": {"job_retention_days": 5}}), None, &l, false),
            Err(PutError::ReasonNeeded)
        );
        assert!(put(json!({"set": {"job_retention_days": 20}}), None, &l, false).is_ok());
    }
}
