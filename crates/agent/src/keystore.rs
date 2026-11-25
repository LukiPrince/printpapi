// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The agent key at rest. On Windows the setup stores it in agent.ini as `api_key_dpapi`: the key
//! (with a SHA-256 in front of it) encrypted by DPAPI with the machine scope, so a copy of the file
//! is useless on another Windows installation. The ACL of the file stays. Everywhere else the file
//! keeps a plain `api_key` (docs/agent.md, "Where the key is stored").
//!
//! Everything here works on the ini *text*: [`seal`] turns `api_key` into `api_key_dpapi`,
//! [`open`] turns it back in memory. Downstream code sees a plain `api_key` and needs no change.
//! Reading order: a non-empty plain `api_key` wins, then `api_key_dpapi`.

use sha2::{Digest, Sha256};

use crate::config::{Config, rewrite};

/// The line above `api_key_dpapi`. Files are matched by this exact text (after trimming) when the
/// note has to go again.
pub const NOTE: &str = "; The key is stored encrypted for this Windows installation only: https://github.com/LukiPrince/printpapi/blob/main/docs/agent.md#where-the-key-is-stored";

/// What [`open`] says when the key cannot be read here. No key, no blob.
pub const UNREADABLE: &str = "The key stored in agent.ini cannot be read on this Windows installation. Open printpapi Agent, Connection, and enter the server and the key again, or add api_key to agent.ini as an administrator.";
/// What [`open`] says without a store (not Windows): the file is a copy.
pub const NOT_ON_THIS_SYSTEM: &str = "agent.ini holds a key that only the Windows installation that stored it can read (api_key_dpapi). Put the key in as api_key.";
/// What [`seal`] says when the key could not be encrypted.
pub const SEAL_FAILED: &str = "The key could not be encrypted on this Windows installation.";

/// An operating-system store for secrets: machine-scope DPAPI on Windows. Plain function pointers,
/// so a test hands in a reversible fake.
#[derive(Clone, Copy)]
pub struct KeyStore {
    pub protect: fn(&[u8]) -> std::io::Result<Vec<u8>>,
    pub unprotect: fn(&[u8]) -> std::io::Result<Vec<u8>>,
}

/// Some on Windows (DPAPI, machine scope), None elsewhere.
pub fn host() -> Option<KeyStore> {
    #[cfg(windows)]
    {
        Some(KeyStore {
            protect: dpapi::protect,
            unprotect: dpapi::unprotect,
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Lower- or upper-case hex, an even number of digits, at least one byte.
fn from_hex(text: &str) -> Option<Vec<u8>> {
    let t = text.trim().as_bytes();
    if t.is_empty() || !t.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| char::from(c).to_digit(16);
    t.chunks(2)
        .map(|p| u8::try_from(digit(p[0])? * 16 + digit(p[1])?).ok())
        .collect()
}

fn opted_out(cfg: &Config) -> bool {
    cfg.get("seal_key").ok().flatten().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "no" | "false" | "off" | "0"
        )
    })
}

/// api_key -> api_key_dpapi. `Ok(None)`: there is nothing to seal, the text stays (no store, text
/// that does not parse, `seal_key` opts out, no own non-empty `api_key` in `[agent]`, a key
/// with a control character). `Err` is the fixed [`SEAL_FAILED`]: the store refused, or the
/// proof (opening the result gives the same key) failed.
// ponytail: a key in [DEFAULT] stays as it is; the setup never writes one there.
pub fn seal(text: &str, store: Option<&KeyStore>) -> Result<Option<String>, String> {
    let Some(store) = store else {
        return Ok(None);
    };
    let Ok(cfg) = Config::from_text(text) else {
        return Ok(None);
    };
    // a non-empty key in [DEFAULT] would stay in charge once the own one is gone
    if opted_out(&cfg)
        || cfg.own_raw("api_key").is_none_or(str::is_empty)
        || cfg.default_raw("api_key").is_some_and(|k| !k.is_empty())
    {
        return Ok(None);
    }
    let Ok(Some(key)) = cfg.get("api_key") else {
        return Ok(None);
    };
    if key.is_empty() || key.chars().any(char::is_control) {
        return Ok(None);
    }
    let mut plain = Sha256::digest(key.as_bytes()).to_vec();
    plain.extend_from_slice(key.as_bytes());
    let blob = (store.protect)(&plain).map_err(|_| SEAL_FAILED.to_string())?;
    let sealed = rewrite(
        Some(text),
        [
            vec![
                (
                    "api_key".to_string(),
                    Some(format!("{NOTE}\napi_key_dpapi = {}", to_hex(&blob))),
                ),
                ("api_key_dpapi".to_string(), None),
            ],
            Vec::new(),
        ],
    );
    // the proof: what was stored comes back as the same key
    let back = open(&sealed, Some(store))
        .ok()
        .flatten()
        .and_then(|t| Config::from_text(&t).ok())
        .and_then(|c| c.get("api_key").ok().flatten());
    if back.as_deref() == Some(key.as_str()) {
        Ok(Some(sealed))
    } else {
        Err(SEAL_FAILED.to_string())
    }
}

/// api_key_dpapi -> api_key, in memory. `Ok(None)`: no sealed key is in use (the text does not
/// parse, a non-empty `api_key` is there, or `[agent]` itself has no `api_key_dpapi`), the text
/// stays. `Err` is a fixed text that holds neither the key nor the blob.
pub fn open(text: &str, store: Option<&KeyStore>) -> Result<Option<String>, String> {
    let Ok(cfg) = Config::from_text(text) else {
        return Ok(None);
    };
    let Some(blob_hex) = cfg.own_raw("api_key_dpapi") else {
        return Ok(None);
    };
    // plain wins, and a stale blob is never looked at; a bad %-reference is for the loader to report
    match cfg.get("api_key") {
        Ok(Some(k)) if !k.is_empty() => return Ok(None),
        Err(_) => return Ok(None),
        _ => {}
    }
    let Some(store) = store else {
        return Err(NOT_ON_THIS_SYSTEM.to_string());
    };
    let key = decrypt(store, blob_hex).ok_or_else(|| UNREADABLE.to_string())?;
    let opened = rewrite(
        Some(text),
        [
            vec![
                (
                    "api_key_dpapi".to_string(),
                    Some(format!("api_key = {}", key.replace('%', "%%"))),
                ),
                ("api_key".to_string(), None),
            ],
            Vec::new(),
        ],
    );
    // the proof, as in `seal`: what the file now says is the key, and not a key with its
    // neighbours swallowed or its edges trimmed away
    let back = Config::from_text(&opened)
        .ok()
        .and_then(|c| c.get("api_key").ok().flatten());
    if back.as_deref() == Some(key.as_str()) {
        Ok(Some(opened))
    } else {
        Err(UNREADABLE.to_string())
    }
}

/// The key inside a blob: valid hex, the store accepts it, a SHA-256 and at least one more byte, the
/// hash matches, UTF-8 without control characters. The hash is there because Microsoft documents
/// that `CryptUnprotectData` "in some cases may succeed with corrupted output"
/// (https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptunprotectdata).
fn decrypt(store: &KeyStore, blob_hex: &str) -> Option<String> {
    let blob = from_hex(blob_hex)?;
    let plain = (store.unprotect)(&blob).ok()?;
    if plain.len() < 33 {
        return None;
    }
    let (hash, key) = plain.split_at(32);
    if Sha256::digest(key).as_slice() != hash {
        return None;
    }
    let key = String::from_utf8(key.to_vec()).ok()?;
    (!key.chars().any(char::is_control)).then_some(key)
}

/// `text` with `seal_key = no` set (replacing another value of it): later changes leave the
/// key as plain text.
pub fn keep_plain(text: &str) -> String {
    rewrite(
        Some(text),
        [
            vec![("seal_key".to_string(), Some("seal_key = no".to_string()))],
            Vec::new(),
        ],
    )
}

/// DPAPI, machine scope. The elevated administrator who installs writes the key and needs it
/// again for a change of the printers, the agent reads it as SYSTEM: a SYSTEM-scope blob would be
/// unreadable to the administrator.
/// https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata
/// https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptunprotectdata
/// (the output buffer is allocated by the call and released with `LocalFree`). No description and
/// no entropy: a constant in public source adds nothing.
#[cfg(windows)]
mod dpapi {
    use std::io;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN,
        CryptProtectData, CryptUnprotectData,
    };

    fn blob_of(bytes: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
        let len = u32::try_from(bytes.len()).map_err(|_| io::Error::other("data too long"))?;
        Ok(CRYPT_INTEGER_BLOB {
            cbData: len,
            pbData: bytes.as_ptr().cast_mut(),
        })
    }

    /// Copy the buffer the call allocated and release it.
    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        // SAFETY: on success pbData points to cbData bytes that the call allocated; they are read
        // once, then freed with LocalFree as the documentation asks
        unsafe {
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData.cast());
            bytes
        }
    }

    pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob_of(plain)?;
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: input points into `plain`, which outlives the call; description, entropy,
        // reserved and prompt are NULL; out is valid for writes
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(take(out))
    }

    pub fn unprotect(blob: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob_of(blob)?;
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`; the description out-pointer is NULL (not wanted)
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xor(b: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(b.iter().map(|x| x ^ 0x5a).collect())
    }
    fn refuse(_: &[u8]) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::other("refused"))
    }
    fn never(_: &[u8]) -> std::io::Result<Vec<u8>> {
        panic!("the store must not be asked");
    }
    fn short(_: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(vec![1; 10])
    }
    fn wrong_hash(_: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok([vec![0; 32], b"key".to_vec()].concat())
    }
    fn not_utf8(_: &[u8]) -> std::io::Result<Vec<u8>> {
        let key = [0xff, 0xfe, 0xfd];
        Ok([Sha256::digest(key).to_vec(), key.to_vec()].concat())
    }
    fn control(_: &[u8]) -> std::io::Result<Vec<u8>> {
        let key = b"ab\ncd";
        Ok([Sha256::digest(key).to_vec(), key.to_vec()].concat())
    }
    const STORE: KeyStore = KeyStore {
        protect: xor,
        unprotect: xor,
    };
    fn store_with(unprotect: fn(&[u8]) -> std::io::Result<Vec<u8>>) -> KeyStore {
        KeyStore {
            protect: xor,
            unprotect,
        }
    }

    const KEY: &str = "KEY%x-secret-1234567890";
    const TEXT: &str = "; mine\n[agent]\nserver_url = https://print.example.com\napi_key = KEY%%x-secret-1234567890\nname = shop-pc\nprinters = Büro Drucker|pdf = file:///tmp\ntimeout = 45\n\n[headers]\nCF-Access = tok%en\n";

    fn sealed() -> String {
        seal(TEXT, Some(&STORE)).unwrap().unwrap()
    }

    fn settings(text: &str) -> (Vec<Option<String>>, crate::config::HttpSettings) {
        let cfg = Config::from_text(text).unwrap();
        let values = ["server_url", "api_key", "name", "printers", "timeout"]
            .iter()
            .map(|k| cfg.get(k).unwrap())
            .collect();
        (values, cfg.http_settings().unwrap())
    }

    #[test]
    fn seal_then_open_gives_the_same_settings() {
        let s = sealed();
        let opened = open(&s, Some(&STORE)).unwrap().unwrap();
        assert_eq!(settings(&opened), settings(TEXT));
        assert_eq!(
            Config::from_text(&opened)
                .unwrap()
                .require("api_key")
                .unwrap(),
            KEY
        );
    }

    #[test]
    fn the_sealed_text_has_neither_the_key_nor_a_plain_line() {
        let s = sealed();
        assert!(!s.contains("KEY%"), "{s}");
        assert!(!s.contains("secret-1234567890"), "{s}");
        assert!(!s.lines().any(|l| l.trim_start().starts_with("api_key =")));
        assert_eq!(s.matches("api_key_dpapi").count(), 1, "{s}");
        assert_eq!(s.lines().filter(|l| l.trim() == NOTE).count(), 1, "{s}");
        // the line the documented Python snippet looks for
        let line = s
            .lines()
            .find(|l| l.starts_with("api_key_dpapi = "))
            .unwrap();
        assert!(
            line[16..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        // what was there stays
        assert!(
            s.contains("; mine") && s.contains("CF-Access = tok%en") && s.contains("timeout = 45")
        );
    }

    #[test]
    fn sealing_opening_and_sealing_again_piles_up_nothing() {
        let s = sealed();
        let opened = open(&s, Some(&STORE)).unwrap().unwrap();
        assert!(
            !opened.contains(NOTE) && !opened.contains("api_key_dpapi"),
            "{opened}"
        );
        let again = seal(&opened, Some(&STORE)).unwrap().unwrap();
        assert_eq!(again.matches("api_key_dpapi").count(), 1, "{again}");
        assert_eq!(again.lines().filter(|l| l.trim() == NOTE).count(), 1);
        // sealing a sealed text with a stale plain key leaves one blob and one note
        let stale = s.replace("[headers]", "[headers]\n").replacen(
            "name = shop-pc",
            "name = shop-pc\napi_key = other-key-1234",
            1,
        );
        let fresh = seal(&stale, Some(&STORE)).unwrap().unwrap();
        assert_eq!(fresh.matches("api_key_dpapi").count(), 1, "{fresh}");
        let back = open(&fresh, Some(&STORE)).unwrap().unwrap();
        assert_eq!(
            Config::from_text(&back)
                .unwrap()
                .require("api_key")
                .unwrap(),
            "other-key-1234"
        );
    }

    #[test]
    fn seal_leaves_the_text_alone_when_there_is_nothing_to_seal() {
        assert_eq!(seal(TEXT, None), Ok(None));
        let no_key = "[agent]\nserver_url = https://x\nprinters = p\n";
        assert_eq!(seal(no_key, Some(&STORE)), Ok(None));
        let in_default = "[DEFAULT]\napi_key = k\n[agent]\nserver_url = https://x\nprinters = p\n";
        assert_eq!(seal(in_default, Some(&STORE)), Ok(None));
        // own key and a key in [DEFAULT]: sealing the own one would leave the other in charge
        let both = "[DEFAULT]\napi_key = d\n[agent]\napi_key = own\nprinters = p\n";
        assert_eq!(seal(both, Some(&STORE)), Ok(None));
        let empty = "[agent]\nserver_url = https://x\napi_key =\nprinters = p\n";
        assert_eq!(seal(empty, Some(&STORE)), Ok(None));
        let ctl = "[agent]\napi_key = ab\u{7}cd\nprinters = p\n";
        assert_eq!(seal(ctl, Some(&STORE)), Ok(None));
        assert_eq!(seal("not an ini", Some(&STORE)), Ok(None));
        assert_eq!(seal("[agent\napi_key", Some(&STORE)), Ok(None));
    }

    #[test]
    fn a_hand_indented_agent_section_seals_and_opens_with_every_setting_kept() {
        for indent in ["  ", "\t", "    "] {
            let text = TEXT
                .lines()
                .map(|l| {
                    if l.contains(" = ") && !l.starts_with(';') && !l.starts_with("CF-") {
                        format!("{indent}{l}")
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(&format!("{indent}api_key = ")), "{text}");
            let sealed = seal(&text, Some(&STORE)).unwrap().unwrap();
            // the lines after the blob are still their own settings
            assert_eq!(
                settings(&sealed).0[2].as_deref(),
                Some("shop-pc"),
                "{sealed}"
            );
            let opened = open(&sealed, Some(&STORE)).unwrap().unwrap();
            assert_eq!(settings(&opened), settings(&text), "{opened}");
            assert_eq!(
                Config::from_text(&opened)
                    .unwrap()
                    .require("api_key")
                    .unwrap(),
                KEY
            );
            // and the way back to a sealed file, from the opened one
            assert!(seal(&opened, Some(&STORE)).unwrap().is_some());
        }
    }

    #[test]
    fn a_key_that_would_not_read_back_the_same_is_not_handed_out() {
        // trailing Unicode white space is not a control character, but the parser trims it
        fn spaced(_: &[u8]) -> std::io::Result<Vec<u8>> {
            let key = "abcdefgh1234\u{2003}".as_bytes();
            Ok([Sha256::digest(key).to_vec(), key.to_vec()].concat())
        }
        let s = sealed();
        assert_eq!(
            open(&s, Some(&store_with(spaced))),
            Err(UNREADABLE.to_string())
        );
    }

    #[test]
    fn a_plain_key_in_default_is_left_alone_without_asking_the_store() {
        let never_store = KeyStore {
            protect: never,
            unprotect: never,
        };
        let both = "[DEFAULT]\napi_key = from-default\n[agent]\nserver_url = https://x\napi_key = own-key\nprinters = p\n";
        assert_eq!(seal(both, Some(&never_store)), Ok(None));
    }

    #[test]
    fn seal_key_opts_out_in_any_spelling_and_the_store_is_never_asked() {
        let never_store = KeyStore {
            protect: never,
            unprotect: never,
        };
        for value in ["no", "No", "FALSE", "off", "0", " nO "] {
            let text = format!("{TEXT}\n[extra]\nx = 1\n").replace(
                "name = shop-pc",
                &format!("name = shop-pc\nseal_key = {value}"),
            );
            assert_eq!(seal(&text, Some(&never_store)), Ok(None), "{value}");
        }
        let yes = TEXT.replace("name = shop-pc", "name = shop-pc\nseal_key = yes");
        assert!(seal(&yes, Some(&STORE)).unwrap().is_some());
    }

    #[test]
    fn keep_plain_sets_seal_key_once_replacing_another_value() {
        let once = keep_plain(TEXT);
        assert_eq!(once.matches("seal_key").count(), 1, "{once}");
        assert!(once.lines().any(|l| l == "seal_key = no"));
        let again = keep_plain(&once);
        assert_eq!(again, once);
        let yes = TEXT.replace("name = shop-pc", "name = shop-pc\nseal_key = yes");
        let replaced = keep_plain(&yes);
        assert_eq!(replaced.matches("seal_key").count(), 1, "{replaced}");
        assert!(replaced.contains("seal_key = no"));
        assert_eq!(settings(&replaced), settings(TEXT));
    }

    #[test]
    fn a_plain_key_wins_and_the_blob_is_never_decrypted() {
        let both = sealed().replace(
            "api_key_dpapi =",
            "api_key = plain-key-1234567\napi_key_dpapi =",
        );
        let never_store = KeyStore {
            protect: never,
            unprotect: never,
        };
        assert_eq!(open(&both, Some(&never_store)), Ok(None));
        // and without a store at all
        assert_eq!(open(&both, None), Ok(None));
        // a key in [DEFAULT] counts too
        let in_default = format!("[DEFAULT]\napi_key = d-key-1234567\n{}", sealed());
        assert_eq!(open(&in_default, Some(&never_store)), Ok(None));
    }

    #[test]
    fn an_empty_api_key_next_to_the_blob_opens_the_blob_without_a_duplicate() {
        let with_empty = sealed().replace("api_key_dpapi =", "api_key =\napi_key_dpapi =");
        let opened = open(&with_empty, Some(&STORE)).unwrap().unwrap();
        let cfg = Config::from_text(&opened).unwrap();
        assert_eq!(cfg.require("api_key").unwrap(), KEY);
        assert_eq!(opened.matches("api_key").count(), 1, "{opened}");
    }

    #[test]
    fn open_leaves_text_that_does_not_parse_and_text_without_a_blob_alone() {
        assert_eq!(open("garbage", Some(&STORE)), Ok(None));
        assert_eq!(open(TEXT, Some(&STORE)), Ok(None));
        // a blob in [DEFAULT] is not "in use": [agent] itself has none
        let default_blob =
            "[DEFAULT]\napi_key_dpapi = 00\n[agent]\nserver_url = https://x\nprinters = p\n";
        assert_eq!(open(default_blob, Some(&STORE)), Ok(None));
    }

    fn blob_of(text: &str) -> String {
        text.lines()
            .find_map(|l| l.strip_prefix("api_key_dpapi = "))
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_blob_that_cannot_be_opened_gives_the_fixed_text_without_key_or_blob() {
        let good = sealed();
        let blob = blob_of(&good);
        let odd = good.replace(&blob, &blob[..blob.len() - 1]);
        let empty = good.replace(&blob, "");
        let not_hex = good.replace(&blob, "zz");
        let cases: Vec<(String, KeyStore)> = vec![
            (odd, STORE),
            (empty, STORE),
            (not_hex, STORE),
            (good.clone(), store_with(refuse)),
            (good.clone(), store_with(short)),
            (good.clone(), store_with(wrong_hash)),
            (good.clone(), store_with(not_utf8)),
            (good.clone(), store_with(control)),
        ];
        for (i, (text, store)) in cases.iter().enumerate() {
            let e = open(text, Some(store)).unwrap_err();
            assert_eq!(e, UNREADABLE, "case {i}");
            assert!(!e.contains(&blob) && !e.contains("secret"), "case {i}");
        }
    }

    #[test]
    fn a_blob_without_a_store_says_that_it_belongs_to_another_system() {
        assert_eq!(open(&sealed(), None), Err(NOT_ON_THIS_SYSTEM.to_string()));
        assert!(NOT_ON_THIS_SYSTEM.contains("api_key_dpapi"));
    }

    #[test]
    fn a_reference_to_the_key_in_another_value_still_resolves_after_open() {
        let text = "[agent]\nserver_url = https://x\napi_key = abcdefgh1234\nname = pc-%(api_key)s\nprinters = p\n";
        let s = seal(text, Some(&STORE)).unwrap().unwrap();
        let opened = open(&s, Some(&STORE)).unwrap().unwrap();
        let cfg = Config::from_text(&opened).unwrap();
        assert_eq!(cfg.require("name").unwrap(), "pc-abcdefgh1234");
    }

    #[test]
    fn seal_fails_when_the_store_refuses_or_the_proof_does_not_match() {
        let refusing = KeyStore {
            protect: refuse,
            unprotect: xor,
        };
        assert_eq!(seal(TEXT, Some(&refusing)), Err(SEAL_FAILED.to_string()));
        // protect and unprotect that do not match: the proof catches it
        let skewed = KeyStore {
            protect: xor,
            unprotect: wrong_hash,
        };
        assert_eq!(seal(TEXT, Some(&skewed)), Err(SEAL_FAILED.to_string()));
        assert!(!SEAL_FAILED.contains("secret"));
    }

    #[test]
    fn hex_round_trips_and_is_strict() {
        assert_eq!(to_hex(&[0, 15, 255]), "000fff");
        assert_eq!(from_hex("000FfF"), Some(vec![0, 15, 255]));
        assert_eq!(from_hex(""), None);
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex("+1"), None);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trips_the_key_on_this_machine() {
        let store = host().unwrap();
        let s = seal(TEXT, Some(&store)).unwrap().unwrap();
        assert!(!s.contains("secret-1234567890"));
        let opened = open(&s, Some(&store)).unwrap().unwrap();
        assert_eq!(settings(&opened), settings(TEXT));
    }

    #[cfg(windows)]
    #[test]
    fn a_damaged_dpapi_blob_never_gives_a_wrong_key() {
        let store = host().unwrap();
        let s = seal(TEXT, Some(&store)).unwrap().unwrap();
        let blob = blob_of(&s);
        let bytes = from_hex(&blob).unwrap();
        for byte in 0..bytes.len() {
            for bit in 0..8 {
                let mut flipped = bytes.clone();
                flipped[byte] ^= 1 << bit;
                let text = s.replace(&blob, &to_hex(&flipped));
                match open(&text, Some(&store)) {
                    Err(e) => assert_eq!(e, UNREADABLE),
                    Ok(Some(t)) => assert_eq!(
                        Config::from_text(&t).unwrap().require("api_key").unwrap(),
                        KEY,
                        "byte {byte} bit {bit}"
                    ),
                    Ok(None) => panic!("a blob is there"),
                }
            }
        }
        let zeros = s.replace(&blob, &"00".repeat(8));
        assert_eq!(open(&zeros, Some(&store)), Err(UNREADABLE.to_string()));
        let zeros = s.replace(&blob, &"00".repeat(blob.len() / 2));
        assert_eq!(open(&zeros, Some(&store)), Err(UNREADABLE.to_string()));
    }

    /// The Python block of docs/agent.md (found by its marker comment) reads a real DPAPI blob on a
    /// machine whose exe is blocked. Skipped where `python` does not run (none on the PATH, or the
    /// Microsoft Store stub that Windows 11 puts there, which exits with 9009).
    #[cfg(windows)]
    #[test]
    fn the_documented_python_snippet_reads_a_real_blob() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let docs =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/agent.md"))
                .unwrap();
        let block = docs
            .split("```python\n")
            .skip(1)
            .filter_map(|b| b.split("```").next())
            .find(|b| b.contains("# printpapi: show the stored agent key"))
            .expect("docs/agent.md has the snippet with its marker comment");
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("agent.ini");
        let store = host().unwrap();
        // a hand-edited file may be cp1252 (here: a lone 0xFC): the snippet reads bytes
        let mut text = b"; B\xfcro\n".to_vec();
        text.extend_from_slice(seal(TEXT, Some(&store)).unwrap().unwrap().as_bytes());
        std::fs::write(&ini, &text).unwrap();
        let code = block.replace(
            r"C:\Program Files\printpapi-agent\agent.ini",
            &ini.display().to_string(),
        );
        let probe = Command::new("python")
            .args(["-I", "-c", "print(1)"])
            .output();
        if !probe
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "1")
        {
            eprintln!("skipped: no working python on this machine");
            return;
        }
        let mut child = Command::new("python")
            .args(["-I", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // a snippet that ends early closes the pipe: its own output says why
        let _ = child.stdin.take().unwrap().write_all(code.as_bytes());
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), KEY);
    }

    #[test]
    fn host_has_a_store_only_on_windows() {
        assert_eq!(host().is_some(), cfg!(windows));
    }
}
