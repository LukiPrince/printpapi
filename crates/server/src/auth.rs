// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Password hashing, tokens and login throttling — the port of `app/auth.py`. Pure, no IO.
//!
//! Hashes are the Python format `scrypt$n$r$p$<salt b64>$<hash b64>` (n=2^14, r=8, p=1, dklen 32,
//! 16-byte salt) and verify in both directions — existing DBs hold hashes written by
//! `hashlib.scrypt`. The stored string carries its own parameters, so raising the cost later still
//! verifies old hashes.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use subtle::ConstantTimeEq;

pub const MIN_PASSWORD_LEN: usize = 8;
/// Bounds the work an unauthenticated login can buy. Counted in characters, like Python `len`.
pub const MAX_PASSWORD_LEN: usize = 1024;
/// Fixed session lifetime; no sliding renewal.
pub const SESSION_TTL_S: f64 = 30.0 * 24.0 * 3600.0;

const LOG_N: u8 = 14;
const R: u32 = 8;
const P: u32 = 1;
const DKLEN: usize = 32;
/// OpenSSL's default `maxmem` (what `hashlib.scrypt` enforces): a stored hash must not be able to
/// make a login allocate more than this.
const MAX_MEM: u128 = 32 * 1024 * 1024;

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    // ponytail: an OS RNG failure is unrecoverable (Python's secrets raises too); not worth a Result.
    getrandom::fill(&mut buf).expect("OS random number generator unavailable");
    buf
}

fn scrypt_raw(
    password: &str,
    salt: &[u8],
    log_n: u8,
    r: u32,
    p: u32,
    dklen: usize,
) -> Option<Vec<u8>> {
    let params = scrypt::Params::new(log_n, r, p).ok()?;
    let mut dk = vec![0u8; dklen];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut dk).ok()?;
    Some(dk)
}

/// `None` (absent or not a JSON string) fails like a too-short password.
/// Err text: `password must be at least 8 characters` / `password must be at most 1024 characters`.
pub fn hash_password(password: Option<&str>) -> Result<String, String> {
    let pw = password.unwrap_or("");
    let chars = pw.chars().count();
    if password.is_none() || chars < MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    if chars > MAX_PASSWORD_LEN {
        return Err(format!(
            "password must be at most {MAX_PASSWORD_LEN} characters"
        ));
    }
    let salt = random_bytes(16);
    let dk = scrypt_raw(pw, &salt, LOG_N, R, P, DKLEN)
        .ok_or_else(|| "password hashing failed".to_string())?;
    Ok(format!(
        "scrypt${}${R}${P}${}${}",
        1u32 << LOG_N,
        STANDARD.encode(&salt),
        STANDARD.encode(&dk)
    ))
}

/// Python `base64.b64decode` (non-validating, `binascii.a2b_base64`): characters outside the
/// alphabet are dropped, `=` only counts as padding after two data characters, decoding stops once
/// padding completes a quad, and non-ASCII input is an error.
fn b64decode_lenient(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let (mut acc, mut quad_pos, mut pads) = (0u32, 0usize, 0usize);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                if quad_pos >= 2 {
                    pads += 1;
                    if quad_pos + pads >= 4 {
                        // a completed pad sequence ends the input; flush the partial quad
                        return Some(flush(out, acc, quad_pos));
                    }
                }
                continue;
            }
            _ => continue,
        };
        pads = 0;
        acc = (acc << 6) | v as u32;
        quad_pos += 1;
        if quad_pos == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            quad_pos = 0;
        }
    }
    if quad_pos != 0 {
        return None; // incorrect padding
    }
    Some(out)
}

fn flush(mut out: Vec<u8>, acc: u32, quad_pos: usize) -> Vec<u8> {
    match quad_pos {
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => {}
    }
    out
}

/// Python `int(s)` for the shapes a stored hash can hold: optional sign, digits, surrounding spaces.
fn parse_py_int(s: &str) -> Option<i128> {
    let t = s.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || digits.len() > 30 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = digits.parse().ok()?;
    Some(if t.starts_with('-') { -v } else { v })
}

fn verify_inner(password: &str, stored: &str) -> Option<bool> {
    let parts: Vec<&str> = stored.split('$').collect();
    let [scheme, n, r, p, salt_b64, hash_b64] = parts[..] else {
        return None;
    };
    if scheme != "scrypt" {
        return None;
    }
    let salt = b64decode_lenient(salt_b64)?;
    let expected = b64decode_lenient(hash_b64)?;
    let (n, r, p) = (parse_py_int(n)?, parse_py_int(r)?, parse_py_int(p)?);
    // hashlib.scrypt: n a power of two > 1, r and p positive, memory under OpenSSL's maxmem.
    if n <= 1 || n & (n - 1) != 0 || r < 1 || p < 1 || expected.is_empty() {
        return None;
    }
    let (n, r, p) = (n as u128, r as u128, p as u128);
    if r > u32::MAX as u128 || p > u32::MAX as u128 || n > 1 << 40 {
        return None;
    }
    // OpenSSL also requires N < 2^(16r) (the scrypt crate omits this bound on purpose).
    if r <= 3 && n >= 1u128 << (16 * r) {
        return None;
    }
    if p * 128 * r + 32 * r * (n + 2) * 4 > MAX_MEM {
        return None;
    }
    let log_n = n.trailing_zeros() as u8;
    let dk = scrypt_raw(password, &salt, log_n, r as u32, p as u32, expected.len())?;
    Some(bool::from(dk.ct_eq(&expected)))
}

/// False for a wrong password, an empty/oversized one, and a NULL/garbage stored hash. Callers map
/// a non-string password to `None` (clean 401; Python raised TypeError/AttributeError there, and
/// OverflowError for n/r/p >= 2^32 on Windows — both deliberately not reproduced).
pub fn verify_password(password: Option<&str>, stored: Option<&str>) -> bool {
    let (Some(password), Some(stored)) = (password, stored) else {
        return false;
    };
    if password.is_empty() || password.chars().count() > MAX_PASSWORD_LEN {
        return false;
    }
    verify_inner(password, stored).unwrap_or(false)
}

/// `secrets.token_urlsafe(n)`: n random bytes, URL-safe base64 without padding.
pub fn token_urlsafe(nbytes: usize) -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(nbytes))
}

/// `secrets.token_hex(n)`: n random bytes as lowercase hex.
pub fn token_hex(nbytes: usize) -> String {
    random_bytes(nbytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `sess_` + token_urlsafe(32): recognizable as a browser session in a log.
pub fn new_session_token() -> String {
    format!("sess_{}", token_urlsafe(32))
}

/// Constant-time string equality (`hmac.compare_digest`). Unlike Python, non-ASCII strings are
/// compared byte-wise instead of raising, so a header with such a byte is a plain 401.
pub fn ct_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Failed-login throttle keyed by an arbitrary string (`login:<email>`, `signup:<ip>`, `reset:<email>`).
/// ponytail: per-process and in-memory (a restart forgets, several server processes each keep
/// their own count), no IP dimension, and bounded — a big enough spray of invented addresses
/// evicts counters rather than growing memory. scrypt already caps a single process at ~20
/// guesses/s; move the counters into SQLite if a deployment ever runs more than one process.
pub struct LoginLimiter {
    pub max_fails: u32,
    pub window_s: f64,
    pub max_tracked: usize,
    fails: Mutex<HashMap<String, (u32, f64)>>,
}

impl LoginLimiter {
    pub fn new(max_fails: u32, window_s: f64) -> Self {
        Self::with_max_tracked(max_fails, window_s, 10_000)
    }

    pub fn with_max_tracked(max_fails: u32, window_s: f64, max_tracked: usize) -> Self {
        LoginLimiter {
            max_fails,
            window_s,
            max_tracked,
            fails: Default::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, (u32, f64)>> {
        self.fails.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn tracked(&self) -> usize {
        self.lock().len()
    }

    /// `now: None` = `crate::now()`.
    pub fn allow(&self, key: &str, now: Option<f64>) -> bool {
        let now = now.unwrap_or_else(crate::now);
        let (count, start) = self.lock().get(key).copied().unwrap_or((0, now));
        now - start >= self.window_s || count < self.max_fails
    }

    pub fn fail(&self, key: &str, now: Option<f64>) {
        let now = now.unwrap_or_else(crate::now);
        let mut fails = self.lock();
        self.bump(&mut fails, key, now);
    }

    /// Count one use of `key` (rolling an expired window over) and keep the map bounded.
    fn bump(&self, fails: &mut HashMap<String, (u32, f64)>, key: &str, now: f64) {
        let (mut count, mut start) = fails.get(key).copied().unwrap_or((0, now));
        if now - start >= self.window_s {
            (count, start) = (0, now); // window rolled over — start counting again
        }
        fails.insert(key.to_string(), (count.saturating_add(1), start));
        if fails.len() > self.max_tracked {
            // Spraying invented addresses must not grow this map without bound. Rolled-over
            // entries are worthless (they no longer block anything), so they go first; if every
            // entry is live, the oldest windows go — those expire soonest anyway.
            let mut victims: Vec<String> = fails
                .iter()
                .filter(|(_, (_, s))| now - s >= self.window_s)
                .map(|(k, _)| k.clone())
                .collect();
            if victims.is_empty() {
                let mut all: Vec<(&String, f64)> = fails.iter().map(|(k, v)| (k, v.1)).collect();
                all.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(b.0)));
                let n = fails.len() - self.max_tracked + 1;
                victims = all.into_iter().take(n).map(|(k, _)| k.clone()).collect();
            }
            for k in victims {
                if k != key {
                    fails.remove(&k);
                }
            }
        }
    }

    /// Atomic check-and-take: under one lock, roll an expired window over, then count one use and
    /// return true only while the window still has room. Unlike `allow` + `fail` (two lock
    /// acquisitions), parallel callers can never all pass the check before any of them counts.
    pub fn try_take(&self, key: &str, now: Option<f64>) -> bool {
        let now = now.unwrap_or_else(crate::now);
        let mut fails = self.lock();
        let (count, start) = fails.get(key).copied().unwrap_or((0, now));
        if now - start < self.window_s && count >= self.max_fails {
            return false;
        }
        self.bump(&mut fails, key, now);
        true
    }

    pub fn succeed(&self, key: &str) {
        self.lock().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Written by Python's hashlib.scrypt (app.auth.hash_password); production DBs hold these.
    const PY_HASH_1: &str =
        "scrypt$16384$8$1$OHCucIOfL9DiYllK7yGMfg==$cr+AlllKsaBHbtIuEYG7Bxlp7hp5GPix+kB1UNtgzHs=";
    const PY_HASH_2: &str =
        "scrypt$16384$8$1$sTTFtB+GcWkDm//n2R5aSw==$YT7+Iy8Xghz1RPYN2jVV0CL6QMmYtg7Kyunq99rWRd4=";
    const PY_HASH_UNICODE: &str =
        "scrypt$16384$8$1$fjHnr4w3smH7nUHqEtIzTQ==$aTW0+/spg/s9kilPxF7HMz1oFaEWSEEny13pi4bf5Yw=";

    #[test]
    fn verifies_hashes_written_by_python() {
        assert!(verify_password(
            Some("correct horse battery"),
            Some(PY_HASH_1)
        ));
        assert!(verify_password(
            Some("pässwörd日本語123"),
            Some(PY_HASH_UNICODE)
        ));
        assert!(!verify_password(
            Some("wrong horse battery"),
            Some(PY_HASH_1)
        ));
        assert!(!verify_password(
            Some("correct horse battery"),
            Some(PY_HASH_2)
        ));
    }

    #[test]
    fn hash_verify_roundtrip_and_wrong_password() {
        let stored = hash_password(Some("correct horse battery")).unwrap();
        assert!(stored.starts_with("scrypt$16384$8$1$"));
        assert!(verify_password(
            Some("correct horse battery"),
            Some(&stored)
        ));
        assert!(!verify_password(Some("wrong horse battery"), Some(&stored)));
    }

    #[test]
    fn hash_is_salted_so_the_same_password_hashes_differently() {
        let a = hash_password(Some("same password")).unwrap();
        let b = hash_password(Some("same password")).unwrap();
        assert_ne!(a, b);
        assert!(verify_password(Some("same password"), Some(&a)));
        assert!(verify_password(Some("same password"), Some(&b)));
    }

    #[test]
    fn verify_rejects_missing_or_malformed_stored_hash() {
        // A legacy user row has password_hash NULL — it must never authenticate.
        assert!(!verify_password(Some("x"), None));
        assert!(!verify_password(Some("x"), Some("")));
        assert!(!verify_password(Some("x"), Some("scrypt$junk")));
        assert!(!verify_password(Some("x"), Some("bcrypt$2$3$4$5")));
        assert!(!verify_password(None, Some(PY_HASH_1)));
        assert!(!verify_password(Some(""), Some(PY_HASH_1)));
    }

    #[test]
    fn verify_rejects_hostile_parameters_and_encodings() {
        let pw = Some("correct horse battery");
        let tail = "OHCucIOfL9DiYllK7yGMfg==$cr+AlllKsaBHbtIuEYG7Bxlp7hp5GPix+kB1UNtgzHs=";
        for n in [
            "0",
            "1",
            "3",
            "-16384",
            "abc",
            "",
            "1099511627776",
            "16777216",
        ] {
            let s = format!("scrypt${n}$8$1${tail}");
            assert!(!verify_password(pw, Some(&s)), "n={n}");
        }
        // huge r / p must be refused, not allocated
        assert!(!verify_password(
            pw,
            Some(&format!("scrypt$16384$4294967295$1${tail}"))
        ));
        assert!(!verify_password(
            pw,
            Some(&format!("scrypt$16384$8$0${tail}"))
        ));
        assert!(!verify_password(
            pw,
            Some(&format!("scrypt$16384$8$4294967295${tail}"))
        ));
        // bad padding, empty expected hash, extra fields
        let bad_pad =
            "scrypt$16384$8$1$OHCucIOfL9DiYllK7yGMf$cr+AlllKsaBHbtIuEYG7Bxlp7hp5GPix+kB1UNtgzHs=";
        assert!(!verify_password(pw, Some(bad_pad)));
        assert!(!verify_password(
            pw,
            Some("scrypt$16384$8$1$OHCucIOfL9DiYllK7yGMfg==$")
        ));
        assert!(!verify_password(pw, Some(&format!("{PY_HASH_1}$x"))));
    }

    #[test]
    fn base64_decoding_is_as_lenient_as_pythons() {
        // junk characters are dropped, like base64.b64decode without validate=True
        assert_eq!(b64decode_lenient("a G\nk =").as_deref(), Some(&b"hi"[..]));
        assert_eq!(b64decode_lenient("aGk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(b64decode_lenient("aGk").as_deref(), None);
        assert_eq!(b64decode_lenient("a").as_deref(), None);
        assert_eq!(
            b64decode_lenient("aGk=trailing").as_deref(),
            Some(&b"hi"[..])
        );
        assert_eq!(b64decode_lenient("").as_deref(), Some(&b""[..]));
    }

    #[test]
    fn base64_decoding_matches_binascii_edge_cases() {
        assert_eq!(b64decode_lenient("=aG=kA==").as_deref(), Some(&b"hi\0"[..]));
        assert_eq!(b64decode_lenient("=aG="), None);
        assert_eq!(b64decode_lenient("aG\u{e9}k="), None);
        let tail = "$myNuW/dnZrkDUrJ1CIheRKHv2vcgAwblz51VNlNIwUw=";
        let pw = Some("correct horse battery");
        assert!(verify_password(
            pw,
            Some(&format!("scrypt$16384$8$1$=aG=kA=={tail}"))
        ));
        let other = "$DxDh+kNTiKRu4XyVz+h5Nys4PAERoB2pnrc55lU7HtM=";
        assert!(!verify_password(
            pw,
            Some(&format!("scrypt$16384$8$1$=aG=kA=={other}"))
        ));
        assert!(!verify_password(
            pw,
            Some(
                "scrypt$16384$8$1$OHCucIOf\u{e9}L9DiYllK7yGMfg==$cr+AlllKsaBHbtIuEYG7Bxlp7hp5GPix+kB1UNtgzHs="
            )
        ));
    }

    #[test]
    fn verify_rejects_n_at_or_above_2_pow_16r_like_openssl() {
        let pw = Some("correct horse battery");
        for n in ["65536", "131072"] {
            let s = format!(
                "scrypt${n}$1$1$MDEyMzQ1Njc4OWFiY2RlZg==$CCqpR40htmFYCcqVCFLQ3WYzKf8hBt98AXayIrA8j2g="
            );
            assert!(!verify_password(pw, Some(&s)), "n={n}");
        }
    }

    #[test]
    fn short_password_is_refused_at_hash_time() {
        let short = "password must be at least 8 characters";
        assert_eq!(hash_password(Some("short")).unwrap_err(), short);
        assert_eq!(hash_password(None).unwrap_err(), short);
        // characters, not bytes: 8 multi-byte characters are enough
        assert!(hash_password(Some("日本語日本語日本")).is_ok());
        assert!(hash_password(Some("日本語日本語日")).is_err());
    }

    #[test]
    fn session_tokens_are_prefixed_and_unique() {
        let (a, b) = (new_session_token(), new_session_token());
        assert!(a.starts_with("sess_") && b.starts_with("sess_"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 5 + 43);
    }

    #[test]
    fn token_helpers_have_the_python_shapes() {
        let h = token_hex(16);
        assert_eq!(h.len(), 32);
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let u = token_urlsafe(32);
        assert!(
            u.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(token_urlsafe(0), "");
    }

    #[test]
    fn ct_eq_compares_content_and_length() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(ct_eq("", ""));
    }

    #[test]
    fn try_take_hands_out_max_slots_per_window_then_resets() {
        let lim = LoginLimiter::new(3, 900.0);
        for i in 0..3 {
            assert!(lim.try_take("k", Some(100.0 + i as f64)));
        }
        assert!(!lim.try_take("k", Some(110.0)));
        assert!(!lim.try_take("k", Some(110.0))); // a refusal takes nothing
        assert!(lim.try_take("other", Some(110.0))); // per key
        assert!(!lim.allow("k", Some(110.0)));
        assert!(lim.try_take("k", Some(100.0 + 900.0))); // window rolled over
        assert!(lim.try_take("k", Some(1001.0)));
        assert!(lim.try_take("k", Some(1002.0)));
        assert!(!lim.try_take("k", Some(1003.0)));
    }

    #[test]
    fn limiter_blocks_after_max_failures_and_forgets_after_the_window() {
        let lim = LoginLimiter::new(3, 900.0);
        for i in 0..3 {
            assert!(lim.allow("a@b.c", Some(100.0 + i as f64)));
            lim.fail("a@b.c", Some(100.0 + i as f64));
        }
        assert!(!lim.allow("a@b.c", Some(110.0)));
        assert!(lim.allow("other@b.c", Some(110.0))); // per-identity, not global
        assert!(lim.allow("a@b.c", Some(100.0 + 900.0))); // window rolled over
    }

    #[test]
    fn limiter_eviction_breaks_window_ties_by_key() {
        let lim = LoginLimiter::with_max_tracked(3, 900.0, 3);
        for k in ["c", "b", "a"] {
            lim.fail(k, Some(100.0));
        }
        lim.fail("d", Some(100.0)); // all four share a window start: "a" and "b" go first
        assert_eq!(lim.tracked(), 2);
        lim.fail("a", Some(100.0));
        lim.fail("a", Some(100.0));
        assert!(lim.allow("a", Some(101.0))); // "a" restarted at 1 failure
    }

    #[test]
    fn limiter_clears_the_count_on_a_successful_login() {
        let lim = LoginLimiter::new(2, 900.0);
        lim.fail("a@b.c", Some(100.0));
        lim.succeed("a@b.c");
        lim.fail("a@b.c", Some(101.0));
        assert!(lim.allow("a@b.c", Some(102.0))); // only one failure since the success
    }

    #[test]
    fn an_absurdly_long_password_is_refused_instead_of_hashed() {
        // Bounded work per login attempt: a multi-megabyte "password" must not buy CPU time.
        let huge = "x".repeat(MAX_PASSWORD_LEN + 1);
        let err = hash_password(Some(&huge)).unwrap_err();
        assert_eq!(err, "password must be at most 1024 characters");
        assert!(!verify_password(Some(&huge), Some(PY_HASH_1)));
    }

    #[test]
    fn the_limiter_forgets_stale_identities_instead_of_growing_forever() {
        let lim = LoginLimiter::with_max_tracked(3, 60.0, 10);
        for i in 0..50 {
            lim.fail(&format!("spray-{i}@b.c"), Some(100.0));
        }
        assert!(lim.tracked() <= 10);
        lim.fail("victim@b.c", Some(100.0));
        lim.fail("victim@b.c", Some(1000.0)); // window rolled over -> counting restarts
        assert!(lim.allow("victim@b.c", Some(1000.0)));
    }

    #[test]
    fn the_limiter_never_evicts_the_key_being_failed_and_drops_oldest_live_windows() {
        let lim = LoginLimiter::with_max_tracked(3, 900.0, 3);
        for (i, k) in ["a", "b", "c"].iter().enumerate() {
            lim.fail(k, Some(100.0 + i as f64));
        }
        lim.fail("d", Some(110.0)); // 4 live entries -> as in Python the slice drops len-max+1 = 2 oldest
        assert_eq!(lim.tracked(), 2);
        for _ in 0..3 {
            lim.fail("d", Some(111.0));
        }
        assert!(!lim.allow("d", Some(112.0)));
        assert!(lim.allow("a", Some(112.0)));
    }
}
