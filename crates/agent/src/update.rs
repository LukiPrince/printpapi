// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Signed updates of the installed agent and its tray/setup program.
//!
//! One Ed25519-signed file, `printpapi-update.txt`, names the files of a release with their SHA-256
//! and size. TLS, the host and a mirror are not trusted: only a signature made with a key in
//! `update-keys.txt` counts. The updater (the installed agent, run elevated) reads every file once
//! into memory, checks those bytes, starts both new programs with `--version` from a private
//! folder, swaps two program files by rename and restarts the agent with `install::restart_plan`.
//! It never writes agent.ini, the unit, the task or the rights. A new version that does not
//! connect is replaced by the old one, which is still on disk.
//!
//! The pure parts (version, signed file, settings, `pick`, `ready`) are first; the steps that touch
//! the machine follow ([`update`]).
//!
//! Sources: ring `UnparsedPublicKey::verify` with `ED25519`,
//! https://docs.rs/ring/0.17.14/ring/signature/struct.UnparsedPublicKey.html (checked in
//! ring-0.17.14/src/signature.rs).

use std::fmt;

use ring::signature::{ED25519, UnparsedPublicKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::apply::ApplyError;
use crate::config::Config;
use crate::install::Os;

/// The signed file in a release (and in a mirror).
pub const MANIFEST_NAME: &str = "printpapi-update.txt";
/// Where releases are looked for unless `update_url` says otherwise.
pub const DEFAULT_BASE: &str = "https://github.com/LukiPrince/printpapi/releases/latest/download";
/// The signed file is at most this large.
pub const MAX_MANIFEST: usize = 64 * 1024;
/// One program file is at most this large (and at least one byte).
pub const MAX_FILE: u64 = 300 * 1024 * 1024;
/// Signature lines in the signed file: one to this many.
const MAX_SIGS: usize = 8;
/// The only schema this program understands.
const SCHEMA: i64 = 1;
/// A status file older than this, or a contact older than this, is not "connected".
const FRESH_SECS: f64 = 120.0;
/// A job that finished this recently means the queue may still have a backlog.
const JOB_QUIET_SECS: f64 = 10.0;

/// The public keys of the build: the committed file, plus the test key under the Cargo feature
/// `update-test-key` (only the CI smoke build enables it).
const KEY_FILE: &str = include_str!("../update-keys.txt");

/// The public half of the seed `printpapi test seed, not secret!` that `examples/sign_update.rs`
/// signs with. Not a secret on purpose; see `keys`.
const TEST_PUBLIC: [u8; 32] = [
    0x38, 0x71, 0xb8, 0xa0, 0xef, 0xf0, 0xb7, 0x8d, 0xce, 0xca, 0xee, 0xe7, 0x14, 0x48, 0xda, 0xe0,
    0xd2, 0xc2, 0xaa, 0x81, 0xfb, 0xbb, 0x17, 0xbf, 0x98, 0x16, 0x76, 0x7b, 0xb6, 0xad, 0x42, 0xab,
];

/// `N.N.N`, compared number by number (3.10.0 is newer than 3.9.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Exactly three numbers of plain digits separated by dots: no sign, no blanks, no suffix, no
    /// leading zero (a prerelease or a stray character is not a version).
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.');
        let mut next = || {
            let p = parts.next()?;
            let plain = !p.is_empty()
                && p.len() <= 9
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p == "0" || !p.starts_with('0'));
            if plain { p.parse::<u32>().ok() } else { None }
        };
        let v = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(v)
    }

    /// The version of this program.
    pub fn own() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// agent.ini `updates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No check, no request.
    Off,
    /// Look once a day and tell the person (the default).
    Notify,
    /// Also install at night, from a task or timer of its own.
    Auto,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Notify => "notify",
            Mode::Auto => "auto",
        }
    }
}

/// What agent.ini says about updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    /// The folder with the files, without a trailing slash. `None`: a `update_url` that is no valid
    /// folder, so nothing is looked up.
    pub base: Option<String>,
}

/// `updates` and `update_url` of `[agent]`. Any case; a value that is not one of the three words
/// counts as `off` (a typo must not switch unattended installs on).
pub fn settings(cfg: &Config) -> Settings {
    let mode = match cfg.get("updates") {
        Ok(None) => Mode::Notify,
        Ok(Some(v)) => match v.trim().to_ascii_lowercase().as_str() {
            "notify" => Mode::Notify,
            "auto" => Mode::Auto,
            _ => Mode::Off,
        },
        Err(_) => Mode::Off,
    };
    let base = match cfg.get("update_url") {
        Ok(None) => Some(DEFAULT_BASE.to_string()),
        Ok(Some(url)) => valid_base(&url),
        Err(_) => None,
    };
    Settings { mode, base }
}

/// `url` as a folder of files: http or https (the signature protects the content, so plain http is
/// fine), a host, no user info, no query, no fragment, no blanks or control characters.
pub fn valid_base(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) || url.contains(['?', '#']) {
        return None;
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(url.trim_end_matches('/').to_string())
}

/// The keys of this build, parsed from the committed file. A file that cannot be parsed gives no
/// key at all (a test keeps the committed one parseable): the build then cannot update, which is
/// the safe way to fail.
pub fn keys() -> Vec<[u8; 32]> {
    keys_from(KEY_FILE, cfg!(feature = "update-test-key")).unwrap_or_default()
}

/// `keys` for a given text, with or without the test key.
pub fn keys_from(text: &str, with_test_key: bool) -> Result<Vec<[u8; 32]>, String> {
    let mut keys = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = hex_key(line).ok_or_else(|| format!("line {} is no 64 digit hex key", i + 1))?;
        keys.push(key);
    }
    if with_test_key {
        keys.push(TEST_PUBLIC);
    }
    Ok(keys)
}

fn hex_key(text: &str) -> Option<[u8; 32]> {
    let bytes = hex_decode(text)?;
    bytes.try_into().ok()
}

/// Lower-case hex only: the key file and the signature lines have one spelling.
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks(2)
        .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
        .collect()
}

/// Whether this build can check for updates at all: it has a public key.
pub fn supported() -> bool {
    !keys().is_empty()
}

/// The SHA-256 of `bytes` as 64 lower-case hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An error of the update with the `result.json` code the window reads.
pub(crate) fn coded(code: &'static str, message: impl Into<String>) -> ApplyError {
    ApplyError::Coded(code, message.into())
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

impl FileRef {
    /// A file entry of the signed file, checked: a plain name, a size of 1 byte to 300 MiB and a
    /// SHA-256 as 64 lower-case hex digits.
    fn from_json(v: &Value) -> Option<FileRef> {
        let file = v.get("file")?.as_str()?;
        let size = v.get("size")?.as_u64()?;
        let sha256 = v.get("sha256")?.as_str()?;
        let first = file.bytes().next()?;
        let plain = first.is_ascii_alphanumeric()
            && file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            && !file.contains("..")
            && file.len() <= 200;
        let hash = sha256.len() == 64 && hex_decode(sha256).is_some();
        (plain && hash && (1..=MAX_FILE).contains(&size)).then(|| FileRef {
            file: file.to_string(),
            size,
            sha256: sha256.to_string(),
        })
    }

    /// Whether `bytes` are this file.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        bytes.len() as u64 == self.size && sha256_hex(bytes) == self.sha256
    }
}

/// The payload of a signed file whose signature was verified.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub version: Version,
    targets: Value,
}

const NOT_SIGNED: &str =
    "The update is not signed with a key this program knows. It was not installed.";

/// The signed file `bytes`: one to eight `sig <128 hex>` lines, an empty line, then the payload up
/// to the end. A signature covers exactly the payload bytes; the file is accepted when any line
/// verifies under any of `keys`. The signature is checked before the payload is parsed.
pub fn open_signed(bytes: &[u8], keys: &[[u8; 32]]) -> Result<Manifest, ApplyError> {
    if bytes.len() > MAX_MANIFEST {
        return Err(coded("bad_file", "The signed update file is too large."));
    }
    let not_signed = || coded("bad_signature", NOT_SIGNED);
    let split = bytes
        .windows(2)
        .position(|w| w == b"\n\n")
        .ok_or_else(not_signed)?;
    let (head, payload) = (&bytes[..split], &bytes[split + 2..]);
    let head = std::str::from_utf8(head).map_err(|_| not_signed())?;
    let mut sigs: Vec<Vec<u8>> = Vec::new();
    for line in head.split('\n') {
        let hex = line.strip_prefix("sig ").ok_or_else(not_signed)?;
        if hex.len() != 128 {
            return Err(not_signed());
        }
        sigs.push(hex_decode(hex).ok_or_else(not_signed)?);
    }
    if sigs.is_empty() || sigs.len() > MAX_SIGS {
        return Err(not_signed());
    }
    let verified = sigs.iter().any(|sig| {
        keys.iter().any(|key| {
            UnparsedPublicKey::new(&ED25519, key)
                .verify(payload, sig)
                .is_ok()
        })
    });
    if !verified {
        return Err(not_signed());
    }
    let bad = || coded("bad_file", "The signed update file cannot be read.");
    let json: Value = serde_json::from_slice(payload).map_err(|_| bad())?;
    match json.get("schema").and_then(Value::as_i64) {
        Some(SCHEMA) => {}
        Some(_) => {
            return Err(coded(
                "manual",
                "This update needs the new setup program. Download it from the releases page.",
            ));
        }
        None => return Err(bad()),
    }
    let version = json
        .get("version")
        .and_then(Value::as_str)
        .and_then(Version::parse)
        .ok_or_else(bad)?;
    let targets = json.get("targets").cloned().ok_or_else(bad)?;
    if !targets.is_object() {
        return Err(bad());
    }
    Ok(Manifest { version, targets })
}

/// `std::env::consts::OS-ARCH` of this build, which is how the signed file names a target.
pub fn host_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// What kind of tray/setup program is installed, which decides the file that replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayKind {
    /// No tray installed: only the agent is replaced.
    None,
    /// An executable: the Windows setup exe or a Linux ELF.
    Program,
    /// An AppImage (Linux).
    AppImage,
    /// A `.app` folder (macOS).
    App,
}

/// The kind of the tray at `path`: a folder on macOS is an app, a file elsewhere is a program,
/// except a Linux file with the AppImage magic (`41 49 02` at offset 8,
/// https://raw.githubusercontent.com/AppImage/AppImageSpec/master/draft.md). Links are not
/// followed: a link is nothing we installed.
pub fn tray_kind(os: Os, path: &std::path::Path) -> TrayKind {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return TrayKind::None;
    };
    match os {
        Os::Macos if meta.is_dir() => TrayKind::App,
        Os::Macos => TrayKind::None,
        _ if !meta.file_type().is_file() => TrayKind::None,
        Os::Linux => {
            use std::io::Read;
            let mut head = [0u8; 11];
            let appimage = std::fs::File::open(path)
                .and_then(|mut f| f.read_exact(&mut head))
                .is_ok()
                && head[8..11] == [0x41, 0x49, 0x02];
            if appimage {
                TrayKind::AppImage
            } else {
                TrayKind::Program
            }
        }
        Os::Windows => TrayKind::Program,
    }
}

/// The files of `target` that replace this installation: the agent and, when a tray is installed,
/// the role that fits its kind (`tray`, `appimage`, `app`). A missing role or target is
/// `no_target`, and nothing is replaced then.
pub fn pick(
    m: &Manifest,
    target: &str,
    kind: TrayKind,
) -> Result<(FileRef, Option<FileRef>), ApplyError> {
    let none = || {
        coded(
            "no_target",
            "This release has no file for this installation. Use the setup program.",
        )
    };
    let files = m.targets.get(target).ok_or_else(none)?;
    let role = |name: &str| FileRef::from_json(files.get(name)?);
    let agent = role("agent").ok_or_else(none)?;
    let tray = match kind {
        TrayKind::None => None,
        TrayKind::Program => Some(role("tray").ok_or_else(none)?),
        TrayKind::AppImage => Some(role("appimage").ok_or_else(none)?),
        TrayKind::App => Some(role("app").ok_or_else(none)?),
    };
    Ok((agent, tray))
}

/// What reads the status file: the real [`read_status_file`], or a fake in a test.
pub type StatusFn = dyn Fn(&Path) -> Option<StatusSeen>;

/// What the status file looked like when it was read.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusSeen {
    /// Seconds since the file was last written.
    pub age: f64,
    pub text: String,
}

/// Whether the agent is quiet enough to be stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    Yes,
    /// It prints or printed a moment ago: look again.
    Printing,
    /// No fresh file, not online, no recent contact with the server.
    NotConnected,
}

/// `NotConnected` when there is no file, it is older than 120 s, the state is not `online` or the
/// last contact is older than 120 s. `Printing` when `busy`, or the last job is 0 to 10 s old (a
/// negative age, a clock set back, counts as old). A queue with a backlog therefore never looks idle:
/// the agent polls again at once after each job.
pub fn ready(seen: Option<&StatusSeen>, now: f64) -> Ready {
    let Some(v) = connected(seen, now) else {
        return Ready::NotConnected;
    };
    if is_printing(&v, now) {
        Ready::Printing
    } else {
        Ready::Yes
    }
}

/// `busy`, or a last job 0 to 10 s old, in an already parsed status file.
fn is_printing(v: &Value, now: f64) -> bool {
    if v.get("busy").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let job_age = v
        .get("last_job")
        .and_then(|j| j.get("at"))
        .and_then(Value::as_f64)
        .map(|at| now - at);
    matches!(job_age, Some(age) if (0.0..JOB_QUIET_SECS).contains(&age))
}

/// A fresh status file of an agent that prints, or of an online one that printed a moment ago,
/// whatever its last contact with the server: a long print does not refresh `last_contact`, and a
/// rollback must not stop the agent in the middle of it. `busy` counts whatever `state` says: the
/// first job after an offline phase prints while the file still says offline (`claimed` sets `busy`,
/// the state turns online only when the poll is done).
pub fn still_printing(seen: Option<&StatusSeen>, now: f64) -> bool {
    let Some(seen) = seen.filter(|s| !s.age.is_nan() && s.age <= FRESH_SECS) else {
        return false;
    };
    serde_json::from_str::<Value>(&seen.text).is_ok_and(|v| {
        v.get("busy").and_then(Value::as_bool) == Some(true)
            || (v.get("state").and_then(Value::as_str) == Some("online") && is_printing(&v, now))
    })
}

/// The status JSON when it is a fresh file of an online agent that talked to the server lately.
fn connected(seen: Option<&StatusSeen>, now: f64) -> Option<Value> {
    let seen = seen?;
    if seen.age.is_nan() || seen.age > FRESH_SECS {
        return None;
    }
    let v: Value = serde_json::from_str(&seen.text).ok()?;
    if v.get("state").and_then(Value::as_str) != Some("online") {
        return None;
    }
    let contact = v.get("last_contact").and_then(Value::as_f64)?;
    (now - contact <= FRESH_SECS).then_some(v)
}

/// The `started_at` of the process that wrote `seen` when it is the new version, online, and both
/// its start and its last contact are later than `local` (the clock of this computer right before
/// the start): proof that the new program, not the old one, reached the server.
pub fn proven(seen: Option<&StatusSeen>, version: Version, local: f64, now: f64) -> Option<f64> {
    let v = connected(seen, now)?;
    let own = v
        .get("version")
        .and_then(Value::as_str)
        .and_then(Version::parse)?;
    let started = v.get("started_at").and_then(Value::as_f64)?;
    let contact = v.get("last_contact").and_then(Value::as_f64)?;
    (own == version && started > local && contact > local).then_some(started)
}

// ---------------------------------------------------------------------------------------------
// The machine side: lock, private folder, schedule, state, and the update itself.
// ---------------------------------------------------------------------------------------------

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::apply::{
    Env, check_app_links, check_install_dir, check_staging, create_new_file, is_link, leftover_of,
    place_file, remove_tree, retry, set_mode, system,
};
use crate::client::DownloadError;
use crate::install::{self, Layout, TaskState};
use crate::marker::{self, BrandMark};
use crate::proc::Cmd;

/// The private folder inside the install folder. Nothing in it can be opened by an ordinary user.
pub const DIR_NAME: &str = ".update";
/// The lock inside it: every admin command that changes the installation holds it.
pub const LOCK_NAME: &str = "lock";
/// What the last attempt left (no secrets), next to agent.ini.
pub const STATE_NAME: &str = "update-state.json";

const LOCK_TRIES: u32 = 5;
const MANIFEST_DEADLINE: Duration = Duration::from_secs(30);
const FILE_DEADLINE: Duration = Duration::from_secs(600);
const PROBE_SECS: u64 = 60;
const DITTO_SECS: u64 = 120;
/// How long a busy agent is waited for before the update gives up.
const QUIET_WAIT_SECS: u64 = 120;
/// How long the new agent has to connect. The test key shortens it for the smoke test.
const HEALTH_SECS: u64 = if cfg!(feature = "update-test-key") {
    45
} else {
    300
};
/// The second look at a new agent: the same process, still online.
const STABLE_SECS: u64 = 15;
/// After this long without a new process, the start command is given once more (the Windows task
/// ignores a start while the old process is still ending, and says nothing).
const RESTART_AGAIN_SECS: u64 = 20;
const ROLLBACK_START_TRIES: u32 = 15;
const ROLLBACK_ONLINE_SECS: u64 = 120;
/// Unattended runs leave a version alone after this many failures after the stop.
pub const MAX_TRIES: u32 = 2;

const BUSY: &str = "An update or another change is running. Try again in a few minutes.";
const NO_KEY: &str = "This build has no update key, so it cannot install updates.";
const BRANDED: &str =
    "This installation belongs to a branded build. Its updates come from that build.";
const NOT_OURS: &str = "This agent was not installed by the setup, so the setup leaves it alone.";
const OTHER_BINARY: &str = "The update has to run from the installed agent.";
const NO_LOCK: &str =
    "This installation has no update lock yet. Run the setup once, then update from it.";
const TRY_LATER: &str =
    "A downloaded file is not the one the release lists. Try again in a few minutes.";

/// `<install folder>/.update`
pub fn private_dir(l: &Layout) -> PathBuf {
    l.dir.join(DIR_NAME)
}

fn lock_path(l: &Layout) -> PathBuf {
    private_dir(l).join(LOCK_NAME)
}

/// The held lock. Dropping it lets go (the file stays: it is never deleted or replaced while the
/// installation exists, so two holders on two inodes cannot happen).
#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] std::fs::File);

/// The lock when `.update/lock` exists: five tries, one second apart, then `busy`. `None` when
/// there is no lock file (an installation made before this release; the first install from this
/// release on creates it). `install`, `uninstall`, `restart`, `unprotect-key` take it this way.
pub fn lock_if_exists(env: &Env) -> Result<Option<Lock>, ApplyError> {
    lock_in_dir(&env.layout.dir, env.sleep)
}

/// `lock_if_exists` for the installation in `dir` (`printpapi-agent pair` runs from the install
/// folder and has no `Env`).
pub fn lock_in_dir(dir: &Path, sleep: &dyn Fn(u64)) -> Result<Option<Lock>, ApplyError> {
    let path = dir.join(DIR_NAME).join(LOCK_NAME);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(system("cannot look at the update lock", e)),
        Ok(meta) if is_link(&meta) || !meta.is_file() => {
            return Err(ApplyError::System(
                "The update lock is not a plain file. Remove the folder .update in the install folder and run the setup again."
                    .into(),
            ));
        }
        Ok(_) => {}
    }
    hold(sleep, &path).map(Some)
}

/// `lock_if_exists`, but the lock has to be there: an update of an installation without one asks
/// for the setup.
fn lock_required(env: &Env) -> Result<Lock, ApplyError> {
    lock_if_exists(env)?.ok_or_else(|| ApplyError::System(NO_LOCK.into()))
}

fn hold(sleep: &dyn Fn(u64), path: &Path) -> Result<Lock, ApplyError> {
    use std::fs::TryLockError;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| system("cannot open the update lock", e))?;
    for i in 0..LOCK_TRIES {
        match file.try_lock() {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) => {
                if i + 1 < LOCK_TRIES {
                    sleep(1);
                }
            }
            Err(TryLockError::Error(e)) => return Err(system("cannot take the update lock", e)),
        }
    }
    Err(coded("busy", BUSY))
}

/// What is looked at on an existing private folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirFacts {
    pub is_dir: bool,
    pub is_link: bool,
    /// Unix only.
    pub uid: Option<u32>,
    /// Unix only: the permission bits.
    pub mode: Option<u32>,
}

/// An existing `.update` is trusted only as a real folder, owned by root (when this process is
/// root; `None` skips the owner), with no rights for group or others. Anything else is an error:
/// the lock inside a folder that another user can write can be held by that user for ever.
pub fn check_private_dir(f: &DirFacts, root: bool, path: &Path) -> Result<(), ApplyError> {
    let refuse = |why: &str| {
        Err(ApplyError::System(format!(
            "{} {why}. The setup does not use it. Remove it and run the setup again.",
            path.display()
        )))
    };
    if f.is_link || !f.is_dir {
        return refuse("is not a plain folder");
    }
    if root && f.uid.is_some_and(|u| u != 0) {
        return refuse("is not owned by root");
    }
    if f.mode.is_some_and(|m| m & 0o077 != 0) {
        return refuse("can be used by other users");
    }
    Ok(())
}

#[cfg(unix)]
fn dir_facts(meta: &std::fs::Metadata) -> DirFacts {
    use std::os::unix::fs::MetadataExt;
    DirFacts {
        is_dir: meta.is_dir(),
        is_link: meta.file_type().is_symlink(),
        uid: Some(meta.uid()),
        mode: Some(meta.mode() & 0o7777),
    }
}

#[cfg(not(unix))]
fn dir_facts(meta: &std::fs::Metadata) -> DirFacts {
    DirFacts {
        is_dir: meta.is_dir(),
        is_link: is_link(meta),
        uid: None,
        mode: None,
    }
}

#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no arguments and cannot fail
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn running_as_root() -> bool {
    false
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

/// Make `.update` (or check the one that is there), close it to everybody but the administrators,
/// and make the lock file if it is missing. Windows: `icacls` on the folder while it is still
/// empty, so nothing in it ever had wider rights. Elevated install only.
pub fn ensure_private_dir(env: &Env) -> Result<(), ApplyError> {
    let dir = private_dir(&env.layout);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => check_private_dir(&dir_facts(&meta), running_as_root(), &dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            create_private_dir(&dir).map_err(|e| system("cannot create the update folder", e))?;
        }
        Err(e) => return Err(system("cannot look at the update folder", e)),
    }
    for c in install::private_dir_cmds(env.os, &dir, &env.sys) {
        (env.run)(&c).map_err(|e| system("cannot protect the update folder", e))?;
    }
    // created once, never replaced: `create_new` keeps whatever is there
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(lock_path(&env.layout)) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(system("cannot create the update lock", e)),
    }
}

const SCHEDULE_FAILED: &str = "The agent runs, but the update schedule could not be changed";

/// Whether the unattended update should exist for the installed `agent.ini` text: mode `auto`, a
/// valid folder, a build with a key, a stock (not branded) installation.
pub fn wants_schedule(env: &Env, ini_text: &str) -> bool {
    let stock = marker::read(&env.layout.marker).is_some_and(|m| m.brand == BrandMark::Stock);
    let Ok(cfg) = Config::from_text(ini_text) else {
        return false;
    };
    let s = settings(&cfg);
    stock && s.mode == Mode::Auto && s.base.is_some() && !env.update_keys.is_empty()
}

/// Bring the unattended update in line with `want`. Equal bytes on disk mean nothing to do (no
/// command at all, which keeps an ordinary install exactly as it was); otherwise the files are
/// written and registered, or, when it is not wanted, unregistered and deleted. Only the
/// update's own timer, task or job is touched: never the agent's, and nothing is enabled or
/// created that an update could have switched off. macOS: `bootout` first when the job existed,
/// then `bootstrap` up to ten times, one second apart, because the label can still be in teardown.
pub fn sync_schedule(env: &Env, want: bool) -> Result<(), ApplyError> {
    sync(env, want, false)
}

/// The uninstall: the schedule goes whether or not its files are still there. A registration that
/// failed deleted its files (see `sync`), and a task or timer that an earlier run registered
/// would otherwise outlive them.
pub fn remove_schedule(env: &Env) -> Result<(), ApplyError> {
    sync(env, false, true)
}

fn sync(env: &Env, want: bool, even_without_files: bool) -> Result<(), ApplyError> {
    let plan = install::update_schedule(env.os, &env.layout, &env.sys);
    let fail = |e: String| ApplyError::System(format!("{SCHEDULE_FAILED}: {e}"));
    let present = plan
        .files
        .iter()
        .any(|(p, _)| std::fs::symlink_metadata(p).is_ok());
    if !want {
        if !present && !even_without_files {
            return Ok(());
        }
        for c in &plan.unregister {
            let _ = (env.run)(c);
        }
        for (path, _) in &plan.files {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(fail(format!("cannot delete {}: {e}", path.display()))),
            }
        }
        for c in &plan.reload {
            let _ = (env.run)(c);
        }
        return Ok(());
    }
    let same = plan
        .files
        .iter()
        .all(|(p, bytes)| std::fs::read(p).is_ok_and(|on_disk| on_disk == *bytes));
    if same {
        return Ok(());
    }
    for (path, bytes) in &plan.files {
        if let Some(parent) = path.parent() {
            crate::apply::create_dirs(parent).map_err(|e| fail(e.to_string()))?;
        }
        place_file(path, bytes, 0o644)
            .map_err(|e| fail(format!("cannot write {}: {e}", path.display())))?;
    }
    if present && env.os == Os::Macos {
        for c in &plan.unregister {
            let _ = (env.run)(c);
        }
    }
    for c in &plan.register {
        let attempts = if env.os == Os::Macos { 10 } else { 1 };
        let registered = retry(attempts, env.sleep, &mut || {
            (env.run)(c).map(|_| ()).map_err(std::io::Error::other)
        });
        if let Err(e) = registered {
            // equal bytes on disk mean "done", so files of a schedule that was never registered
            // must not stay: the next install or restart writes and registers them again. An entry
            // that an earlier run registered goes with them: with no file left, "off" and the
            // uninstall would have nothing to start from.
            for c in &plan.unregister {
                let _ = (env.run)(c);
            }
            for (path, _) in &plan.files {
                let _ = std::fs::remove_file(path);
            }
            for c in &plan.reload {
                let _ = (env.run)(c);
            }
            return Err(fail(e.to_string()));
        }
    }
    Ok(())
}

/// What an update leaves beside a program: `.<name>.new`, `.failed`, and `.old` to `.old9`.
pub fn leftovers_of(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut out = vec![
        leftover_of(path),
        path.with_file_name(format!(".{name}.failed")),
    ];
    out.push(path.with_file_name(format!(".{name}.old")));
    for i in 1..=9 {
        out.push(path.with_file_name(format!(".{name}.old{i}")));
    }
    out
}

/// When `path` is gone, the first aside copy (`.<name>.old`, `.old1` to `.old9`) takes its place.
/// An earlier rollback that could not rename the old program back leaves it only there.
fn restore_missing(env: &Env, path: &Path) {
    if std::fs::symlink_metadata(path).is_ok() {
        return;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let asides =
        std::iter::once(format!(".{name}.old")).chain((1..=9).map(|i| format!(".{name}.old{i}")));
    for aside in asides.map(|a| path.with_file_name(a)) {
        if std::fs::symlink_metadata(&aside).is_ok()
            && retry(3, env.sleep, &mut || std::fs::rename(&aside, path)).is_ok()
        {
            return;
        }
    }
}

/// `<install folder>/update-state.json`
pub fn state_path(l: &Layout) -> PathBuf {
    l.dir.join(STATE_NAME)
}

/// What an update adds to an installation and the uninstall takes out: the private folder with its
/// lock, the state file, and the leftovers beside the programs (macOS: beside the app).
pub fn paths_to_remove(l: &Layout) -> Vec<PathBuf> {
    let mut paths = vec![private_dir(l), state_path(l)];
    for program in [&l.binary, &l.tray] {
        paths.extend(leftovers_of(program));
    }
    paths
}

/// The record of the last attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub version: String,
    pub from: String,
    pub at: f64,
    pub ok: bool,
    /// Failures after the stop for this same version.
    pub tries: u32,
    pub message: String,
}

pub fn read_state(l: &Layout) -> Option<State> {
    let bytes = marker::read_text(&state_path(l))?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    Some(State {
        version: v.get("version")?.as_str()?.to_string(),
        from: v.get("from")?.as_str()?.to_string(),
        at: v.get("at")?.as_f64()?,
        ok: v.get("ok")?.as_bool()?,
        tries: u32::try_from(v.get("tries")?.as_u64()?).ok()?,
        message: v.get("message")?.as_str()?.to_string(),
    })
}

/// `detail`: the reason behind the short message, for a person who looks (not part of [`State`]:
/// nothing reads it back).
fn write_state(env: &Env, state: &State, detail: &str) {
    // the file is read back through the 4 KiB reader of the marker
    let message: String = state.message.chars().take(800).collect();
    let detail: String = detail.chars().take(400).collect();
    let text = json!({
        "version": state.version,
        "from": state.from,
        "at": state.at,
        "ok": state.ok,
        "tries": state.tries,
        "message": message,
        "detail": detail,
    })
    .to_string();
    if let Err(e) = place_file(&state_path(&env.layout), text.as_bytes(), 0o644) {
        crate::err_line(&format!("cannot write {STATE_NAME}: {e}"));
    }
}

/// The status file as it is now: a regular file (Unix: no link, no blocking on a fifo), at most
/// 64 KB, with its age from the modification time. A time in the future is an unbounded age.
pub fn read_status_file(path: &Path) -> Option<StatusSeen> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let age = std::time::SystemTime::now()
        .duration_since(meta.modified().ok()?)
        .map_or(f64::INFINITY, |d| d.as_secs_f64());
    Some(StatusSeen {
        age,
        text: String::from_utf8(bytes).ok()?,
    })
}

/// `<os>-<arch>` as the signed file names a target.
pub fn target(os: Os) -> String {
    let os = match os {
        Os::Windows => "windows",
        Os::Linux => "linux",
        Os::Macos => "macos",
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// How an update is started.
pub enum Source<'a> {
    /// `restart --update-from <staging>`: the window downloaded and verified the files into a
    /// staging folder (which any user can write, so everything is checked again).
    Staged(&'a Path),
    /// `restart --update now`: fetch it, quietly leaving when there is nothing to do.
    Fetch,
}

/// The files of a release as bytes, before any check.
struct Fetched {
    /// The folder the files came from.
    base: String,
    manifest: Vec<u8>,
    files: Vec<(String, Vec<u8>)>,
}

/// A file by name and size limit.
type Getter<'a> = dyn FnMut(&str, u64) -> Result<Vec<u8>, ApplyError> + 'a;

/// A release that passed every check.
struct Release {
    version: Version,
    agent: Vec<u8>,
    tray: Option<Vec<u8>>,
}

fn file_error(what: &str, e: std::io::Error) -> ApplyError {
    coded("bad_file", format!("{what}: {e}"))
}

/// A regular file, at most `limit` bytes, never through a link: root reads it, and the folder
/// belongs to the user who started the window. Windows opens with `FILE_FLAG_OPEN_REPARSE_POINT`
/// (a junction or link is then opened as itself and refused), Unix with `O_NOFOLLOW`.
fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>, ApplyError> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let meta = std::fs::symlink_metadata(path).map_err(|e| file_error(&name, e))?;
    if is_link(&meta) || !meta.is_file() {
        return Err(coded("bad_file", format!("{name} is not a plain file.")));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = opts.open(path).map_err(|e| file_error(&name, e))?;
    let opened = file.metadata().map_err(|e| file_error(&name, e))?;
    if is_link(&opened) || !opened.is_file() || opened.len() > limit {
        return Err(coded(
            "bad_file",
            format!("{name} is not a plain file of the listed size."),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| file_error(&name, e))?;
    if bytes.len() as u64 > limit {
        return Err(coded("bad_file", format!("{name} is larger than listed.")));
    }
    Ok(bytes)
}

/// Check a release: the signature first, then the version (strictly newer than `own`), the files
/// for this system, size and SHA-256 of each. `get` yields a file by name and size limit.
fn check_release(
    env: &Env,
    own: Version,
    kind: TrayKind,
    get: &mut Getter,
) -> Result<Release, ApplyError> {
    let manifest = open_signed(&get(MANIFEST_NAME, MAX_MANIFEST as u64)?, env.update_keys)?;
    if manifest.version <= own {
        return Err(coded(
            "not_newer",
            format!(
                "Version {} is already installed, or a newer one.",
                own.max(manifest.version)
            ),
        ));
    }
    let (agent_ref, tray_ref) = pick(&manifest, &target(env.os), kind)?;
    let agent = get(&agent_ref.file, agent_ref.size)?;
    if !agent_ref.matches(&agent) {
        return Err(coded("bad_file", TRY_LATER));
    }
    let tray = match tray_ref {
        Some(r) => {
            let bytes = get(&r.file, r.size)?;
            if !r.matches(&bytes) {
                return Err(coded("bad_file", TRY_LATER));
            }
            Some(bytes)
        }
        None => None,
    };
    Ok(Release {
        version: manifest.version,
        agent,
        tray,
    })
}

/// The release folder an unattended run may use now: `agent.ini` says `updates = auto` with a
/// valid folder and the marker names a stock installation. Read again after the lock, because
/// the download takes minutes and a person may switch updates off meanwhile.
fn unattended_base(env: &Env) -> Option<String> {
    let l = &env.layout;
    if !marker::read(&l.marker).is_some_and(|m| m.brand == BrandMark::Stock) {
        return None;
    }
    let ini = std::fs::read(&l.ini).ok()?;
    let cfg = Config::from_text(&crate::config::decode_ini(&ini)).ok()?;
    let s = settings(&cfg);
    s.base.filter(|_| s.mode == Mode::Auto)
}

/// The unattended side of step 0: everything that decides whether to fetch at all. `Ok(None)` is a
/// quiet leave (exit 0).
fn fetch(env: &Env, own: Version, kind: TrayKind) -> Result<Option<Fetched>, ApplyError> {
    let l = &env.layout;
    let Some(base) = unattended_base(env) else {
        return Ok(None);
    };
    let status = (env.read_status)(&l.status_dir.join("status.json"));
    if ready(status.as_ref(), (env.now)()) == Ready::NotConnected {
        return Ok(None);
    }
    let get =
        |url: String, limit: u64, deadline: Duration| match (env.download)(&url, limit, deadline) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(DownloadError::NotFound) => Ok(None),
            Err(DownloadError::Other(e)) => Err(ApplyError::Unreachable(e)),
        };
    let Some(manifest) = get(
        format!("{base}/{MANIFEST_NAME}"),
        MAX_MANIFEST as u64,
        MANIFEST_DEADLINE,
    )?
    else {
        return Ok(None);
    };
    let signed = open_signed(&manifest, env.update_keys)?;
    if signed.version <= own {
        return Ok(None);
    }
    if read_state(l)
        .is_some_and(|st| st.version == signed.version.to_string() && st.tries >= MAX_TRIES)
    {
        return Ok(None);
    }
    let Ok((agent_ref, tray_ref)) = pick(&signed, &target(env.os), kind) else {
        return Ok(None);
    };
    let mut files = Vec::new();
    for r in std::iter::once(&agent_ref).chain(tray_ref.as_ref()) {
        match get(format!("{base}/{}", r.file), r.size, FILE_DEADLINE)? {
            Some(bytes) => files.push((r.file.clone(), bytes)),
            None => return Err(coded("bad_file", TRY_LATER)),
        }
    }
    Ok(Some(Fetched {
        base,
        manifest,
        files,
    }))
}

/// Write `bytes` as a new file only this process has open, executable, and close it again, so a
/// program started from it is not held open (Windows share mode 0 while the handle lives).
fn write_program(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = create_new_file(path, 0o755)?;
    file.write_all(bytes)?;
    drop(file);
    set_mode(path, 0o755)
}

/// `--version` of a program that is about to be installed: the output must be exactly `expected`.
fn probe(env: &Env, program: &Path, expected: &str) -> Result<(), String> {
    let mut c = Cmd::new(
        vec![program.to_string_lossy().into_owned(), "--version".into()],
        PROBE_SECS,
    );
    c.capture = true;
    let out = (env.run)(&c)?;
    let text = String::from_utf8_lossy(&out);
    if text.trim_end_matches(['\r', '\n']) == expected {
        Ok(())
    } else {
        Err("it reports another version".to_string())
    }
}

/// What step 3 has made: the programs from the private folder that were started once, and the
/// folder a macOS app was unpacked to.
#[derive(Default)]
struct Probed {
    /// macOS: the unpacked `.app`.
    app: Option<PathBuf>,
}

const WONT_START: &str =
    "The new version cannot be started on this computer (it may be blocked). Nothing was changed.";

/// Steps 3 and 4: the new programs are started once from the private folder, then written next to
/// their targets as `.<name>.new`. Nothing has been stopped yet.
fn prepare(env: &Env, release: &Release, kind: TrayKind) -> Result<Probed, ApplyError> {
    let l = &env.layout;
    let dir = private_dir(l);
    let ext = if env.os == Os::Windows { ".exe" } else { "" };
    let version = release.version;
    let wont =
        |what: &str, why: String| coded("wont_start", format!("{WONT_START} ({what}: {why})"));
    let mut probed = Probed::default();

    let agent_probe = dir.join(format!("agent-probe{ext}"));
    write_program(&agent_probe, &release.agent)
        .map_err(|e| system("cannot write the new agent", e))?;
    probe(env, &agent_probe, &format!("printpapi-agent {version}"))
        .map_err(|e| wont("agent", e))?;

    match (kind, &release.tray) {
        (TrayKind::Program, Some(bytes)) => {
            let path = dir.join(format!("tray-probe{ext}"));
            write_program(&path, bytes)
                .map_err(|e| system("cannot write the new tray program", e))?;
            probe(env, &path, &format!("printpapi-setup {version}"))
                .map_err(|e| wont("tray", e))?;
        }
        (TrayKind::App, Some(zip)) => {
            let zip_path = dir.join("app.zip");
            let unpacked = dir.join("app");
            std::fs::write(&zip_path, zip).map_err(|e| system("cannot write the new app", e))?;
            let ditto = Cmd::new(
                vec![
                    install::ditto_program(env.os, &env.sys),
                    "-x".into(),
                    "-k".into(),
                    zip_path.to_string_lossy().into_owned(),
                    unpacked.to_string_lossy().into_owned(),
                ],
                DITTO_SECS,
            );
            (env.run)(&ditto).map_err(|e| wont("app", format!("it cannot be unpacked: {e}")))?;
            let mut apps = std::fs::read_dir(&unpacked)
                .map_err(|e| wont("app", format!("it cannot be unpacked: {e}")))?
                .flatten()
                .map(|e| e.path())
                .collect::<Vec<_>>();
            apps.retain(|p| p.extension().is_some_and(|x| x == "app") && p.is_dir());
            let [app] = apps.as_slice() else {
                return Err(wont("app", "the archive has no single .app folder".into()));
            };
            check_app_links(app).map_err(|e| wont("app", e))?;
            let inner = app.join("Contents").join("MacOS").join("printpapi-setup");
            probe(env, &inner, &format!("printpapi-setup {version}"))
                .map_err(|e| wont("app", e))?;
            probed.app = Some(app.clone());
        }
        // an AppImage needs FUSE as root: it is not started (ponytail, documented)
        _ => {}
    }

    // step 4: the files next to their targets, all disk space used before anything stops
    let new_agent = leftover_of(&l.binary);
    write_program(&new_agent, &release.agent)
        .map_err(|e| system("cannot write the new agent", e))?;
    match (&release.tray, &probed.app) {
        (Some(bytes), None) => {
            write_program(&leftover_of(&l.tray), bytes)
                .map_err(|e| system("cannot write the new tray program", e))?;
        }
        (Some(_), Some(app)) => {
            let to = leftover_of(&l.tray);
            for c in install::tray_copy_cmds_to(env.os, app, &to, &env.sys) {
                (env.run)(&c).map_err(|e| system("cannot copy the new app", e))?;
            }
            check_app_links(&to).map_err(ApplyError::System)?;
        }
        _ => {}
    }
    Ok(probed)
}

/// Remove what `prepare` and a failed run left: probes, unpacked app, the `.new` files.
fn clean_prepared(env: &Env) {
    let l = &env.layout;
    // a program that has just ended can still be locked for a moment on Windows
    let remove = |path: &Path| {
        let _ = retry(5, env.sleep, &mut || remove_tree(path));
    };
    if let Ok(entries) = std::fs::read_dir(private_dir(l)) {
        for e in entries.flatten() {
            if e.file_name() != LOCK_NAME {
                remove(&e.path());
            }
        }
    }
    for p in [&l.binary, &l.tray] {
        remove(&leftover_of(p));
    }
}

/// Step 5: wait until the agent is quiet: two looks one second apart that both say `Yes`. Not
/// connected is `offline` at once; still printing after two minutes is `printing`.
fn wait_quiet(env: &Env) -> Result<(), ApplyError> {
    let path = env.layout.status_dir.join("status.json");
    let mut waited = 0;
    let mut yes_in_a_row = 0;
    loop {
        let seen = (env.read_status)(&path);
        let now = (env.now)();
        // a long print does not refresh the last contact: a fresh file of an online agent that is
        // busy is printing, and "run the setup" would stop it in the middle of the job
        let look = match ready(seen.as_ref(), now) {
            Ready::NotConnected if still_printing(seen.as_ref(), now) => Ready::Printing,
            other => other,
        };
        match look {
            Ready::NotConnected => {
                return Err(coded(
                    "offline",
                    "The agent has to be connected, so that the update can check the new version. Otherwise run the setup of the new version.",
                ));
            }
            Ready::Yes => {
                yes_in_a_row += 1;
                if yes_in_a_row == 2 {
                    return Ok(());
                }
            }
            Ready::Printing => {
                yes_in_a_row = 0;
                if waited >= QUIET_WAIT_SECS {
                    return Err(coded(
                        "printing",
                        "The agent is printing. Try again when the queue is empty.",
                    ));
                }
            }
        }
        (env.sleep)(1);
        waited += 1;
    }
}

/// Windows: `schtasks /end` and wait until the task is no longer running. A look that fails counts
/// as unknown and is waited for the whole time (the existing restart gives up at the first failing
/// look, which an update may not: it would rename a running exe). The last state is returned.
fn stop_and_wait(env: &Env, plan: &install::RestartPlan) -> TaskState {
    for c in &plan.stop {
        let _ = (env.run)(c);
    }
    let Some(look) = &plan.wait else {
        return TaskState::Stopped;
    };
    let mut state = TaskState::Unknown;
    for _ in 0..install::RESTART_WAIT_SECS {
        state = match (env.run)(look) {
            Ok(out) => install::parse_task_state(&out),
            Err(_) => TaskState::Unknown,
        };
        if state == TaskState::Stopped {
            return state;
        }
        (env.sleep)(1);
    }
    state
}

/// One program swapped: `to` now holds the new file, `old` holds what was there.
struct Swap {
    to: PathBuf,
    old: PathBuf,
}

/// Rename `to` aside to the first free `.<name>.old` (`.old1` to `.old9` when a locked one is still
/// there), then `.<name>.new` to `to`. A running exe cannot be renamed over or deleted on
/// Windows (error 5) but it can be renamed (measured, see the spec).
fn swap(env: &Env, to: &Path) -> Result<Swap, Cause> {
    let name = to
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let free = (0..10)
        .map(|i| {
            let suffix = if i == 0 {
                "old".to_string()
            } else {
                format!("old{i}")
            };
            to.with_file_name(format!(".{name}.{suffix}"))
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .ok_or_else(|| Cause::Step(format!("no free name for the old {name}")))?;
    // ponytail: two renames, not one transaction. A power loss between them leaves the program
    // missing until the setup is run again; a Windows exe cannot be replaced atomically while it
    // runs, and the window is the time of two renames in a stopped agent.
    retry(10, env.sleep, &mut || std::fs::rename(to, &free))
        .map_err(|e| Cause::Step(format!("cannot move the old {name} aside: {e}")))?;
    if let Err(e) = retry(10, env.sleep, &mut || std::fs::rename(leftover_of(to), to)) {
        let why = format!("cannot put the new {name} in place: {e}");
        // put the old one back at once: nothing may be left without a program
        return Err(
            match retry(10, env.sleep, &mut || std::fs::rename(&free, to)) {
                Ok(()) => Cause::Step(why),
                Err(back) => Cause::Stuck(
                    why,
                    format!(
                        "{name} could not be put back: {back} (the old program is {})",
                        free.display()
                    ),
                ),
            },
        );
    }
    Ok(Swap {
        to: to.to_path_buf(),
        old: free,
    })
}

/// Delete what an update moved aside. A file that cannot be deleted (the running updater is that
/// very exe) goes at the next restart on Windows.
fn discard(env: &Env, path: &Path) {
    if remove_tree(path).is_err()
        && std::fs::symlink_metadata(path).is_ok()
        && env.os == Os::Windows
    {
        let _ = (env.delete_at_reboot)(path);
    }
}

/// Why an update is taken back.
#[derive(Debug)]
enum Cause {
    /// The new agent did not connect, or did not stay connected.
    Connect(String),
    /// A step failed: a rename, a start command.
    Step(String),
    /// A step failed and the old program could not be moved back either: what failed, and what is
    /// left (the text names the file that holds the old program).
    Stuck(String, String),
}

/// The result of `update` for the one who called it.
pub type Outcome = Result<Option<Version>, ApplyError>;

/// Update the installed agent and its tray/setup program from a signed release. `Ok(Some(v))` is
/// done and online; `Ok(None)` is a quiet leave of an unattended run. See the module text for the
/// steps; the limits of each are the constants above.
pub fn update(source: Source, env: &Env) -> Outcome {
    let l = &env.layout;
    let attended = matches!(source, Source::Staged(_));
    // an unattended run leaves quietly where an attended one says why
    let refuse = |e: ApplyError| if attended { Err(e) } else { Ok(None) };

    // step 0: nothing is changed and nothing is locked
    check_install_dir(l)?;
    let Some(mark) = marker::read(&l.marker) else {
        return refuse(ApplyError::System(NOT_OURS.into()));
    };
    if mark.brand != BrandMark::Stock {
        return refuse(coded("manual", BRANDED));
    }
    if env.update_keys.is_empty() {
        return refuse(coded("no_key", NO_KEY));
    }
    let own = Version::parse(env.version).unwrap_or(Version(0, 0, 0));
    let installed_here = match (
        std::fs::canonicalize(&env.self_exe),
        std::fs::canonicalize(&l.binary),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !installed_here {
        return refuse(ApplyError::System(OTHER_BINARY.into()));
    }
    let mine =
        std::fs::read(&l.binary).map_err(|e| system("cannot read the installed agent", e))?;
    let kind = tray_kind(env.os, &l.tray);
    let fetched = match source {
        Source::Staged(_) => None,
        Source::Fetch => match fetch(env, own, kind)? {
            Some(f) => Some(f),
            None => return Ok(None),
        },
    };

    // step 1: the lock, and the proof that the program is the one that started
    let _lock = match lock_required(env) {
        Ok(lock) => lock,
        Err(e) => return refuse(e),
    };
    if std::fs::read(&l.binary).ok().as_deref() != Some(mine.as_slice()) {
        return refuse(coded("busy", BUSY));
    }
    // the download ran without the lock: what allowed it must still hold
    if let Some(f) = &fetched
        && unattended_base(env).as_deref() != Some(f.base.as_str())
    {
        return Ok(None);
    }
    clean_prepared(env);
    // a program that is missing (an earlier rollback could not rename it back) has its only copy in
    // an aside name: put that back before the leftovers are cleared, never delete it
    for p in [&l.binary, &l.tray] {
        restore_missing(env, p);
    }
    for p in [&l.binary, &l.tray] {
        for old in leftovers_of(p) {
            let _ = remove_tree(&old);
        }
    }

    // step 2: every check, on the bytes that will be installed
    let release = match (&source, fetched) {
        (Source::Staged(dir), _) => {
            check_staging(dir)?;
            let mut get = |name: &str, limit: u64| read_regular(&dir.join(name), limit);
            check_release(env, own, kind, &mut get)?
        }
        (Source::Fetch, Some(f)) => {
            let mut get = |name: &str, _limit: u64| {
                if name == MANIFEST_NAME {
                    return Ok(f.manifest.clone());
                }
                f.files
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, b)| b.clone())
                    .ok_or_else(|| coded("bad_file", TRY_LATER))
            };
            check_release(env, own, kind, &mut get)?
        }
        (Source::Fetch, None) => return Ok(None),
    };

    // steps 3 and 4: both programs start once, then sit next to their targets
    let result = (|| {
        let probed = prepare(env, &release, kind)?;
        wait_quiet(env)?;
        Ok::<_, ApplyError>(probed)
    })();
    if let Err(e) = result {
        clean_prepared(env);
        return Err(e);
    }
    let outcome = replace(env, &release, own);
    clean_prepared(env);
    outcome.map(Some)
}

/// Steps 6 to 8 and the rollback: the stop, the swap, the health check.
fn replace(env: &Env, release: &Release, own: Version) -> Result<Version, ApplyError> {
    let l = &env.layout;
    let version = release.version;
    let plan = install::restart_plan(env.os, l, &env.sys);

    // step 6
    if stop_and_wait(env, &plan) == TaskState::Running {
        for c in &plan.start {
            let _ = (env.run)(c);
        }
        return Err(ApplyError::System(
            "The agent did not stop, so nothing was changed. Try again, or run the setup.".into(),
        ));
    }
    let mut swaps: Vec<Swap> = Vec::new();
    let failed = |swaps: &[Swap], env: &Env, why: Cause, online: bool| -> ApplyError {
        let (e, detail) = rollback(env, &plan, swaps, version, own, &why, online);
        record(env, version, own, false, &e.to_string(), &detail);
        e
    };
    match swap(env, &l.binary) {
        Ok(s) => swaps.push(s),
        Err(why) => return Err(failed(&swaps, env, why, false)),
    }
    let local = (env.now)();
    for c in &plan.start {
        if let Err(e) = (env.run)(c) {
            return Err(failed(
                &swaps,
                env,
                Cause::Step(format!("the new agent did not start: {e}")),
                false,
            ));
        }
    }

    // step 7: the new version, online, the same process twice
    let path = l.status_dir.join("status.json");
    let mut waited = 0;
    let mut again = false;
    let started = loop {
        if let Some(at) = proven(
            (env.read_status)(&path).as_ref(),
            version,
            local,
            (env.now)(),
        ) {
            break at;
        }
        if waited >= HEALTH_SECS {
            return Err(failed(
                &swaps,
                env,
                Cause::Connect(format!("it did not connect within {HEALTH_SECS} seconds")),
                false,
            ));
        }
        // only where a start can be ignored (the Windows task, IgnoreNew): `systemctl restart` and
        // `kickstart -k` would end a new agent that is merely slow to register
        if !again && waited >= RESTART_AGAIN_SECS && plan.wait.is_some() {
            again = true;
            for c in &plan.start {
                let _ = (env.run)(c);
            }
        }
        (env.sleep)(1);
        waited += 1;
    };
    (env.sleep)(STABLE_SECS);
    let stable = proven(
        (env.read_status)(&path).as_ref(),
        version,
        local,
        (env.now)(),
    );
    if stable != Some(started) {
        return Err(failed(
            &swaps,
            env,
            Cause::Connect("it did not stay connected".into()),
            true,
        ));
    }

    // step 8: the tray, then the record
    if release.tray.is_some() {
        match swap(env, &l.tray) {
            Ok(s) => swaps.push(s),
            Err(why) => return Err(failed(&swaps, env, why, true)),
        }
    }
    let marker_text = marker::read_text(&l.marker)
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|t| marker::parse(t.as_bytes()).brand == BrandMark::Stock)
        .unwrap_or_default();
    if let Err(e) = place_file(
        &l.marker,
        marker::with_version(&marker_text, &version.to_string()).as_bytes(),
        0o644,
    ) {
        crate::err_line(&format!("cannot write the marker: {e}"));
    }
    record(env, version, own, true, "", "");
    for s in &swaps {
        discard(env, &s.old);
    }
    Ok(version)
}

/// `update-state.json` after an attempt: a failure after the stop counts toward the two tries.
fn record(env: &Env, version: Version, own: Version, ok: bool, message: &str, detail: &str) {
    let previous = read_state(&env.layout).filter(|s| s.version == version.to_string());
    write_state(
        env,
        &State {
            version: version.to_string(),
            from: own.to_string(),
            at: (env.now)(),
            ok,
            tries: if ok {
                0
            } else {
                previous.map_or(1, |s| s.tries + 1)
            },
            message: message.to_string(),
        },
        detail,
    );
}

/// Put the old programs back and bring the old agent up. Nothing needs the network. `online`: the
/// new agent was seen online, so it is given time to finish a job first. The error it returns says
/// what happened in words the window can show, and the detail behind it for the state file.
fn rollback(
    env: &Env,
    plan: &install::RestartPlan,
    swaps: &[Swap],
    new: Version,
    old: Version,
    why: &Cause,
    online: bool,
) -> (ApplyError, String) {
    let l = &env.layout;
    let path = l.status_dir.join("status.json");
    if online {
        let mut waited = 0;
        while waited < QUIET_WAIT_SECS
            && still_printing((env.read_status)(&path).as_ref(), (env.now)())
        {
            (env.sleep)(1);
            waited += 1;
        }
    }
    // Windows: the process of the new agent has to end before its file can be renamed
    let _ = stop_and_wait(env, plan);
    let mut failed_names: Vec<PathBuf> = Vec::new();
    // a swap that was stuck half way is already a rename that failed. Only the program that is
    // started decides: the agent swap is the first one, so when it is among `swaps` the stuck one
    // was the tray, and the old agent can be put back and started as usual.
    let mut tray_left: Option<String> = None;
    let mut rename_error: Option<String> = match why {
        Cause::Stuck(_, left) if swaps.iter().any(|s| s.to == l.binary) => {
            tray_left = Some(left.clone());
            None
        }
        Cause::Stuck(_, left) => Some(left.clone()),
        _ => None,
    };
    for s in swaps.iter().rev() {
        let name =
            s.to.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        let failed = s.to.with_file_name(format!(".{name}.failed"));
        let _ = remove_tree(&failed);
        let step = retry(10, env.sleep, &mut || std::fs::rename(&s.to, &failed))
            .and_then(|()| retry(10, env.sleep, &mut || std::fs::rename(&s.old, &s.to)));
        match step {
            Ok(()) => failed_names.push(failed),
            Err(e) => {
                // better some program in its place than none
                if !s.to.exists() {
                    let _ = std::fs::rename(&failed, &s.to);
                }
                rename_error = Some(format!("{name} could not be put back: {e}"));
                break;
            }
        }
    }
    let local = (env.now)();
    let mut started = false;
    let mut last_error = String::new();
    // with no agent program to put back there is nothing worth starting
    let tries = if rename_error.is_some() {
        0
    } else {
        ROLLBACK_START_TRIES
    };
    for i in 0..tries {
        let result: Result<Vec<_>, String> = plan.start.iter().map(|c| (env.run)(c)).collect();
        match result {
            Ok(_) => {
                started = true;
                break;
            }
            Err(e) => last_error = e,
        }
        if i + 1 < tries {
            (env.sleep)(2);
        }
    }
    let mut connected = false;
    if started && rename_error.is_none() {
        let mut again = false;
        for waited in 0..ROLLBACK_ONLINE_SECS {
            if proven((env.read_status)(&path).as_ref(), old, local, (env.now)()).is_some() {
                connected = true;
                break;
            }
            // as in the health wait: a start the Windows task ignored is given once more
            if !again && waited >= RESTART_AGAIN_SECS && plan.wait.is_some() {
                again = true;
                for c in &plan.start {
                    let _ = (env.run)(c);
                }
            }
            (env.sleep)(1);
        }
    }
    for f in &failed_names {
        discard(env, f);
    }
    // what the short sentence leaves out: stderr (the Linux and macOS nightly runs keep it) and the
    // state file (all of them: the Windows task has no console and a click runs hidden)
    let mut detail: Vec<String> = Vec::new();
    let head = match (&rename_error, why) {
        (Some(e), Cause::Connect(w) | Cause::Step(w) | Cause::Stuck(w, _)) => format!(
            "Version {new} could not be installed ({w}), and putting {old} back failed: {e}."
        ),
        (None, Cause::Connect(w)) => {
            detail.push(format!("version {new} did not connect: {w}"));
            format!("Version {new} did not connect, so {old} was put back.")
        }
        (None, Cause::Step(w)) => {
            format!("Version {new} could not be installed ({w}), so {old} was put back.")
        }
        (None, Cause::Stuck(w, _)) => {
            let left = tray_left.as_deref().unwrap_or_default();
            format!(
                "Version {new} could not be installed ({w}), so {old} was put back, but {left}."
            )
        }
    };
    let tail = if rename_error.is_some() || !started {
        if !started && rename_error.is_none() {
            detail.push(format!("the old agent could not be started: {last_error}"));
        }
        "The agent is not running. Run the setup again.".to_string()
    } else {
        let running = if connected {
            "It is running again."
        } else {
            "It is running but has not connected yet; see the log."
        };
        // the tray and the settings window are gone until the setup puts them back
        if tray_left.is_some() {
            format!("{running} Run the setup again to put the tray program back.")
        } else {
            running.to_string()
        }
    };
    for line in &detail {
        crate::err_line(line);
    }
    (
        coded("rolled_back", format!("{head} {tail}")),
        detail.join("; "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const SEED: &[u8; 32] = b"printpapi test seed, not secret!";

    fn pair(seed: &[u8; 32]) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
    }

    fn public(seed: &[u8; 32]) -> [u8; 32] {
        pair(seed).public_key().as_ref().try_into().unwrap()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A signed file over `payload` with one signature line per seed.
    fn signed(payload: &str, seeds: &[&[u8; 32]]) -> Vec<u8> {
        let mut out = String::new();
        for s in seeds {
            out += &format!("sig {}\n", hex(pair(s).sign(payload.as_bytes()).as_ref()));
        }
        out += "\n";
        out += payload;
        out.into_bytes()
    }

    fn payload(version: &str) -> String {
        format!(
            r#"{{"schema":1,"version":"{version}","targets":{{"windows-x86_64":{{"agent":{{"file":"printpapi-update-agent-windows-x86_64.exe","size":10,"sha256":"{h}"}},"tray":{{"file":"printpapi-update-tray-windows-x86_64.exe","size":11,"sha256":"{h}"}}}},"linux-x86_64":{{"agent":{{"file":"a","size":1,"sha256":"{h}"}},"tray":{{"file":"t","size":1,"sha256":"{h}"}},"appimage":{{"file":"i.AppImage","size":1,"sha256":"{h}"}}}},"macos-aarch64":{{"agent":{{"file":"a","size":1,"sha256":"{h}"}},"app":{{"file":"x.zip","size":1,"sha256":"{h}"}}}}}}}}"#,
            h = "ab".repeat(32)
        )
    }

    const OTHER: &[u8; 32] = b"another seed, 32 bytes long....!";

    #[test]
    fn the_test_key_is_the_public_half_of_the_committed_seed() {
        assert_eq!(public(SEED), TEST_PUBLIC);
        // the example signs with the same seed
        let example = include_str!("../examples/sign_update.rs");
        assert!(example.contains("b\"printpapi test seed, not secret!\""));
    }

    #[test]
    fn versions_parse_only_as_n_n_n_and_compare_by_number() {
        assert_eq!(Version::parse("3.7.0"), Some(Version(3, 7, 0)));
        assert_eq!(Version::parse("0.0.0"), Some(Version(0, 0, 0)));
        for bad in [
            "",
            "3",
            "3.7",
            "3.7.0.1",
            "3.7.x",
            "v3.7.0",
            "3.7.0-rc1",
            "3.07.0",
            "+3.7.0",
            " 3.7.0",
            "3..0",
            "99999999999.0.0",
            "-1.0.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert!(Version::parse("3.10.0") > Version::parse("3.9.9"));
        assert!(Version::parse("10.0.0") > Version::parse("9.99.99"));
        assert_eq!(Version(3, 7, 0).to_string(), "3.7.0");
        assert_eq!(Version::own().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_committed_key_file_parses_and_never_holds_the_test_key() {
        // it parses today and on the day the owner adds a real key
        let parsed = keys_from(KEY_FILE, false).expect("the committed file parses");
        assert!(
            !parsed.contains(&TEST_PUBLIC),
            "the public half of the test seed is not a release key"
        );
        // the keys of the build are these, plus the test key only behind the feature
        assert_eq!(
            keys(),
            keys_from(KEY_FILE, cfg!(feature = "update-test-key")).unwrap()
        );
        assert_eq!(supported(), !keys().is_empty());
        // comments only means unsupported, and the test key is there only when asked for
        let comments = "# comments only\n\n";
        assert!(keys_from(comments, false).unwrap().is_empty());
        assert_eq!(keys_from(comments, true).unwrap(), vec![TEST_PUBLIC]);
    }

    /// Every 32 byte string literal `b"..."` in the Rust sources of the repository (a seed is any 32
    /// bytes, `from_seed_unchecked`): the seeds of the tests and of `examples/sign_update.rs`, which
    /// anybody who reads the repository has.
    fn committed_seeds() -> Vec<[u8; 32]> {
        fn walk(dir: &Path, out: &mut Vec<[u8; 32]>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if path.is_dir() {
                    if !["target", "node_modules", "web", ".git"].contains(&name.to_str().unwrap())
                    {
                        walk(&path, out);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let bytes = text.as_bytes();
                    let mut i = 0;
                    while let Some(at) = text[i..].find("b\"") {
                        let start = i + at + 2;
                        let end = bytes[start..]
                            .iter()
                            .position(|&b| b == b'"' || b == b'\\' || b == b'\n')
                            .map_or(bytes.len(), |p| start + p);
                        if end - start == 32 && bytes[end] == b'"' {
                            out.push(bytes[start..end].try_into().unwrap());
                        }
                        i = start;
                    }
                }
            }
        }
        let mut seeds = Vec::new();
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        walk(&crates, &mut seeds);
        seeds
    }

    /// The keys among `keys` that belong to a seed anybody can read in the repository.
    fn keys_with_a_public_seed(keys: &[[u8; 32]]) -> Vec<[u8; 32]> {
        let seeds = committed_seeds();
        keys.iter()
            .copied()
            .filter(|k| seeds.iter().any(|s| public(s) == *k))
            .collect()
    }

    #[test]
    fn a_default_build_accepts_no_key_whose_seed_is_in_the_repository() {
        // the scan is alive: it finds the seed of the test key, and flags a build that has the key
        assert!(committed_seeds().contains(SEED));
        assert_eq!(
            keys_with_a_public_seed(&keys_from(KEY_FILE, true).unwrap()),
            vec![TEST_PUBLIC]
        );
        // the committed file holds no such key, and a build without the feature trusts nothing else
        assert!(keys_with_a_public_seed(&keys_from(KEY_FILE, false).unwrap()).is_empty());
        if !cfg!(feature = "update-test-key") {
            assert!(keys_with_a_public_seed(&keys()).is_empty());
        }
    }

    #[test]
    fn a_key_file_is_parsed_strictly() {
        let k = hex(&public(OTHER));
        let text = format!("# comment\n\n  {k}  \n{k}\n");
        assert_eq!(keys_from(&text, false).unwrap().len(), 2);
        for bad in [
            "abcd".to_string(),
            k.to_uppercase(),
            format!("{k}00"),
            format!("{}g", &k[..63]),
            format!("{k} # trailing comment"),
        ] {
            assert!(keys_from(&bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_signed_file_is_accepted_with_a_listed_key() {
        let k = public(SEED);
        let m = open_signed(&signed(&payload("3.7.0"), &[SEED]), &[k]).unwrap();
        assert_eq!(m.version, Version(3, 7, 0));
        // the second of two keys, and a first signature line nobody knows next to a valid one
        let k2 = public(OTHER);
        assert!(open_signed(&signed(&payload("3.7.0"), &[SEED]), &[k2, k]).is_ok());
        assert!(open_signed(&signed(&payload("3.7.0"), &[OTHER, SEED]), &[k]).is_ok());
        // rotation: signed with old and new, either key alone is enough
        let both = signed(&payload("3.7.0"), &[SEED, OTHER]);
        assert!(open_signed(&both, &[k]).is_ok());
        assert!(open_signed(&both, &[k2]).is_ok());
    }

    fn code(r: Result<Manifest, ApplyError>) -> &'static str {
        r.expect_err("must be refused").code()
    }

    #[test]
    fn a_signed_file_is_refused_when_the_signature_does_not_hold() {
        let k = public(SEED);
        let good = signed(&payload("3.7.0"), &[SEED]);
        assert_eq!(code(open_signed(&good, &[public(OTHER)])), "bad_signature");
        assert_eq!(code(open_signed(&good, &[])), "bad_signature");
        // a flipped byte in the payload, in the signature
        let mut flipped = good.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(code(open_signed(&flipped, &[k])), "bad_signature");
        let mut flipped = good.clone();
        flipped[10] = if flipped[10] == b'0' { b'1' } else { b'0' };
        assert_eq!(code(open_signed(&flipped, &[k])), "bad_signature");
        // short, non-hex and upper-case signatures
        let text = String::from_utf8(good.clone()).unwrap();
        let sig_end = text.find('\n').unwrap();
        let rest = &text[sig_end..];
        for bad in [
            format!("{}{rest}", &text[..sig_end - 2]),
            format!("sig g{}{rest}", &text[5..sig_end]),
            format!("sig {}{rest}", text[4..sig_end].to_uppercase()),
        ] {
            assert_eq!(
                code(open_signed(bad.as_bytes(), &[k])),
                "bad_signature",
                "{bad}"
            );
        }
    }

    #[test]
    fn the_number_of_signature_lines_and_the_empty_line_are_checked() {
        let k = public(SEED);
        let p = payload("3.7.0");
        // no signature line at all
        assert_eq!(
            code(open_signed(format!("\n{p}").as_bytes(), &[k])),
            "bad_signature"
        );
        assert_eq!(code(open_signed(p.as_bytes(), &[k])), "bad_signature");
        // nine lines are one too many, even when each is valid
        let nine = signed(&p, &[SEED; 9]);
        assert_eq!(code(open_signed(&nine, &[k])), "bad_signature");
        assert!(open_signed(&signed(&p, &[SEED; 8]), &[k]).is_ok());
        // no empty line between the signatures and the payload
        let text = String::from_utf8(signed(&p, &[SEED]))
            .unwrap()
            .replacen("\n\n", "\n", 1);
        assert_eq!(code(open_signed(text.as_bytes(), &[k])), "bad_signature");
        // a line that is not a signature line before the empty line
        let text = String::from_utf8(signed(&p, &[SEED])).unwrap();
        let text = format!("note x\n{text}");
        assert_eq!(code(open_signed(text.as_bytes(), &[k])), "bad_signature");
    }

    #[test]
    fn a_file_over_64_kb_is_refused_before_anything_is_read() {
        let k = public(SEED);
        let mut big = signed(&payload("3.7.0"), &[SEED]);
        big.extend(std::iter::repeat_n(b' ', MAX_MANIFEST));
        assert_eq!(code(open_signed(&big, &[k])), "bad_file");
    }

    #[test]
    fn the_payload_is_not_parsed_before_its_signature_was_checked() {
        // a payload that is no JSON with a wrong signature is "not signed", never "cannot be read"
        let bytes = format!("sig {}\n\nnot json", "0".repeat(128));
        assert_eq!(
            code(open_signed(bytes.as_bytes(), &[public(SEED)])),
            "bad_signature"
        );
        // the same, correctly signed, is a file that cannot be read
        let ok = signed("not json", &[SEED]);
        assert_eq!(code(open_signed(&ok, &[public(SEED)])), "bad_file");
    }

    #[test]
    fn schema_version_and_targets_are_checked_after_the_signature() {
        let k = public(SEED);
        let check = |p: &str| code(open_signed(&signed(p, &[SEED]), &[k]));
        assert_eq!(
            check(r#"{"schema":2,"version":"3.7.0","targets":{}}"#),
            "manual"
        );
        assert_eq!(check(r#"{"version":"3.7.0","targets":{}}"#), "bad_file");
        assert_eq!(
            check(r#"{"schema":1,"version":"3.7","targets":{}}"#),
            "bad_file"
        );
        assert_eq!(check(r#"{"schema":1,"version":"3.7.0"}"#), "bad_file");
        assert_eq!(
            check(r#"{"schema":1,"version":"3.7.0","targets":[]}"#),
            "bad_file"
        );
        // unknown fields are ignored
        let p = r#"{"schema":1,"version":"3.7.0","targets":{},"channel":"x"}"#;
        assert!(open_signed(&signed(p, &[SEED]), &[k]).is_ok());
    }

    #[test]
    fn an_openssl_made_file_verifies() {
        // made with: openssl genpkey -algorithm ed25519; openssl pkeyutl -sign -rawin (OpenSSL 3.5.4);
        // the private key was thrown away, only the public half, the payload and the signature stay
        let key = hex_key(include_str!("../testdata/openssl-update-key.txt").trim()).unwrap();
        let file = include_bytes!("../testdata/openssl-update.txt");
        let m = open_signed(file, &[key]).expect("ring accepts an OpenSSL signature");
        assert_eq!(m.version, Version(3, 7, 0));
    }

    fn manifest() -> Manifest {
        open_signed(&signed(&payload("3.7.0"), &[SEED]), &[public(SEED)]).unwrap()
    }

    #[test]
    fn pick_chooses_the_files_by_target_and_tray_kind() {
        let m = manifest();
        let (a, t) = pick(&m, "windows-x86_64", TrayKind::Program).unwrap();
        assert_eq!(a.file, "printpapi-update-agent-windows-x86_64.exe");
        assert_eq!(t.unwrap().file, "printpapi-update-tray-windows-x86_64.exe");
        assert_eq!(
            pick(&m, "linux-x86_64", TrayKind::Program)
                .unwrap()
                .1
                .unwrap()
                .file,
            "t"
        );
        assert_eq!(
            pick(&m, "linux-x86_64", TrayKind::AppImage)
                .unwrap()
                .1
                .unwrap()
                .file,
            "i.AppImage"
        );
        assert_eq!(
            pick(&m, "macos-aarch64", TrayKind::App)
                .unwrap()
                .1
                .unwrap()
                .file,
            "x.zip"
        );
        // no tray installed: the agent alone
        assert_eq!(pick(&m, "linux-x86_64", TrayKind::None).unwrap().1, None);
    }

    #[test]
    fn pick_refuses_what_the_release_does_not_have() {
        let m = manifest();
        for (target, kind) in [
            ("linux-aarch64", TrayKind::None),
            ("windows-x86_64", TrayKind::AppImage),
            ("macos-aarch64", TrayKind::Program),
            ("linux-x86_64", TrayKind::App),
        ] {
            let e = pick(&m, target, kind).unwrap_err();
            assert_eq!(e.code(), "no_target", "{target} {kind:?}");
        }
    }

    #[test]
    fn a_file_entry_is_checked() {
        let h = "ab".repeat(32);
        let entry = |file: &str, size: u64, sha: &str| {
            FileRef::from_json(&serde_json::json!({"file": file, "size": size, "sha256": sha}))
        };
        assert!(entry("a-b_c.1.exe", 1, &h).is_some());
        assert!(entry("a", MAX_FILE, &h).is_some());
        for file in [
            "",
            "a/b",
            "a\\b",
            "../a",
            "a..b",
            ".hidden",
            "-a",
            "a b",
            "a:b",
            &"a".repeat(201),
        ] {
            assert!(entry(file, 1, &h).is_none(), "{file:?}");
        }
        assert!(entry("a", 0, &h).is_none());
        assert!(entry("a", MAX_FILE + 1, &h).is_none());
        for sha in [
            "",
            "ab",
            &"AB".repeat(32),
            &"zz".repeat(32),
            &"ab".repeat(33),
        ] {
            assert!(entry("a", 1, sha).is_none(), "{sha:?}");
        }
        // a role with a bad entry is a missing role
        let p = payload("3.7.0").replace("printpapi-update-agent-windows-x86_64.exe", "../x.exe");
        let m = open_signed(&signed(&p, &[SEED]), &[public(SEED)]).unwrap();
        assert_eq!(
            pick(&m, "windows-x86_64", TrayKind::Program)
                .unwrap_err()
                .code(),
            "no_target"
        );
    }

    #[test]
    fn a_file_entry_matches_exactly_its_bytes() {
        let bytes = b"hello";
        let f = FileRef {
            file: "a".into(),
            size: 5,
            sha256: sha256_hex(bytes),
        };
        assert!(f.matches(bytes));
        assert!(!f.matches(b"hellp"));
        assert!(!f.matches(b"hello!"));
    }

    #[test]
    fn tray_kind_reads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        assert_eq!(tray_kind(Os::Linux, &p("missing")), TrayKind::None);
        std::fs::write(p("elf"), b"\x7fELF....more bytes here").unwrap();
        assert_eq!(tray_kind(Os::Linux, &p("elf")), TrayKind::Program);
        let mut image = b"\x7fELF\x02\x01\x01\x00".to_vec();
        image.extend([0x41, 0x49, 0x02, 0, 0]);
        std::fs::write(p("image"), &image).unwrap();
        assert_eq!(tray_kind(Os::Linux, &p("image")), TrayKind::AppImage);
        // a short file is a program, never a crash
        std::fs::write(p("short"), b"x").unwrap();
        assert_eq!(tray_kind(Os::Linux, &p("short")), TrayKind::Program);
        // the magic only counts on Linux
        assert_eq!(tray_kind(Os::Windows, &p("image")), TrayKind::Program);
        std::fs::create_dir(p("folder")).unwrap();
        assert_eq!(tray_kind(Os::Linux, &p("folder")), TrayKind::None);
        assert_eq!(tray_kind(Os::Windows, &p("folder")), TrayKind::None);
        assert_eq!(tray_kind(Os::Macos, &p("folder")), TrayKind::App);
        assert_eq!(tray_kind(Os::Macos, &p("elf")), TrayKind::None);
    }

    fn cfg(extra: &str) -> Config {
        Config::from_text(&format!(
            "[agent]\nserver_url=https://x\napi_key=k\nprinters=p\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn settings_default_to_notify_and_the_release_folder() {
        let s = settings(&cfg(""));
        assert_eq!(s.mode, Mode::Notify);
        assert_eq!(s.base.as_deref(), Some(DEFAULT_BASE));
    }

    #[test]
    fn the_mode_is_read_in_any_case_and_unknown_means_off() {
        for (text, mode) in [
            ("notify", Mode::Notify),
            ("AUTO", Mode::Auto),
            (" Off ", Mode::Off),
            ("yes", Mode::Off),
            ("", Mode::Off),
            ("autoo", Mode::Off),
            ("%(x)s", Mode::Off),
        ] {
            assert_eq!(
                settings(&cfg(&format!("updates = {text}\n"))).mode,
                mode,
                "{text:?}"
            );
        }
    }

    #[test]
    fn update_url_is_a_folder_or_it_disables_checks() {
        let base = |url: &str| settings(&cfg(&format!("update_url = {url}\n"))).base;
        assert_eq!(
            base("https://mirror.example/p/").as_deref(),
            Some("https://mirror.example/p")
        );
        assert_eq!(
            base("http://10.0.0.5:8080/u").as_deref(),
            Some("http://10.0.0.5:8080/u")
        );
        for bad in [
            "https://user:pw@mirror.example/p",
            "https://mirror.example/p?x=1",
            "https://mirror.example/p#frag",
            "ftp://mirror.example/p",
            "file:///c:/x",
            "mirror.example/p",
            "https://",
            "https:///p",
            "https://mirror.example/a b",
        ] {
            assert_eq!(base(bad), None, "{bad}");
        }
    }

    fn seen(age: f64, text: &str) -> StatusSeen {
        StatusSeen {
            age,
            text: text.to_string(),
        }
    }

    fn status(extra: &str) -> String {
        format!(
            r#"{{"version":"3.7.0","state":"online","started_at":1000.0,"last_contact":1990.0{extra}}}"#
        )
    }

    #[test]
    fn ready_needs_a_fresh_online_agent_that_talked_to_the_server() {
        let now = 2000.0;
        assert_eq!(ready(None, now), Ready::NotConnected);
        assert_eq!(
            ready(Some(&seen(121.0, &status(""))), now),
            Ready::NotConnected
        );
        assert_eq!(
            ready(Some(&seen(0.0, "not json")), now),
            Ready::NotConnected
        );
        assert_eq!(
            ready(
                Some(&seen(1.0, &status("").replace("online", "offline"))),
                now
            ),
            Ready::NotConnected
        );
        // contact older than 120 s
        let stale = status("").replace("1990.0", "1800.0");
        assert_eq!(ready(Some(&seen(1.0, &stale)), now), Ready::NotConnected);
        let no_contact = status("").replace(r#""last_contact":1990.0"#, r#""last_contact":null"#);
        assert_eq!(
            ready(Some(&seen(1.0, &no_contact)), now),
            Ready::NotConnected
        );
        assert_eq!(ready(Some(&seen(1.0, &status(""))), now), Ready::Yes);
    }

    #[test]
    fn ready_waits_for_a_busy_agent_and_a_job_that_just_finished() {
        let now = 2000.0;
        let r = |extra: &str| ready(Some(&seen(1.0, &status(extra))), now);
        assert_eq!(r(r#","busy":true"#), Ready::Printing);
        assert_eq!(r(r#","busy":false"#), Ready::Yes);
        // a status file from an older agent has no busy field
        assert_eq!(r(""), Ready::Yes);
        assert_eq!(
            r(r#","last_job":{"id":1,"ok":true,"at":1997.0}"#),
            Ready::Printing
        );
        assert_eq!(
            r(r#","last_job":{"id":1,"ok":true,"at":1988.0}"#),
            Ready::Yes
        );
        // exactly 10 s old counts as quiet; a job "in the future" (clock set back) counts as old
        assert_eq!(
            r(r#","last_job":{"id":1,"ok":true,"at":1990.0}"#),
            Ready::Yes
        );
        assert_eq!(
            r(r#","last_job":{"id":1,"ok":true,"at":2500.0}"#),
            Ready::Yes
        );
        assert_eq!(r(r#","last_job":null"#), Ready::Yes);
    }

    #[test]
    fn printing_needs_no_recent_contact_with_the_server() {
        // a long print: the file is fresh (heartbeat) but the last contact is old
        let now = 2000.0;
        let stale = status(r#","busy":true"#).replace("1990.0", "1500.0");
        assert_eq!(ready(Some(&seen(1.0, &stale)), now), Ready::NotConnected);
        assert!(still_printing(Some(&seen(1.0, &stale)), now));
        assert!(still_printing(
            Some(&seen(1.0, &status(r#","busy":true"#))),
            now
        ));
        assert!(!still_printing(
            Some(&seen(1.0, &status(r#","busy":false"#))),
            now
        ));
        assert!(!still_printing(Some(&seen(121.0, &stale)), now));
        // the first job after an offline phase: claimed() sets busy, the state turns online only
        // after the print (polled), so the file says offline while the job prints
        let first = status(r#","busy":true"#).replace("online", "offline");
        assert!(still_printing(Some(&seen(1.0, &first)), now));
        assert!(
            !still_printing(Some(&seen(121.0, &first)), now),
            "an old file is nobody's"
        );
        let idle = status(r#","busy":false"#).replace("online", "offline");
        assert!(!still_printing(Some(&seen(1.0, &idle)), now));
        assert!(!still_printing(Some(&seen(1.0, "not json")), now));
        assert!(!still_printing(None, now));
        let done = r#","last_job":{"id":1,"ok":true,"at":1997.0}"#;
        assert!(still_printing(Some(&seen(1.0, &status(done))), now));
    }

    #[test]
    fn proven_needs_the_new_version_online_and_started_after_the_swap() {
        let v = Version(3, 7, 0);
        let now = 2000.0;
        let p = |text: &str| proven(Some(&seen(1.0, text)), v, 900.0, now);
        assert_eq!(p(&status("")), Some(1000.0));
        // the old version wrote it, the process started before the swap, no contact since
        assert_eq!(p(&status("").replace("3.7.0", "3.6.0")), None);
        assert_eq!(p(&status("").replace("1000.0", "800.0")), None);
        assert_eq!(p(&status("").replace("1990.0", "850.0")), None);
        assert_eq!(p(&status("").replace("online", "offline")), None);
        assert_eq!(proven(None, v, 900.0, now), None);
    }
}

// a simulated machine is a list of shared cells and closures
#[cfg(test)]
#[allow(clippy::type_complexity)]
#[path = "update_machine_tests.rs"]
mod machine;

#[cfg(test)]
#[path = "update_scenarios.rs"]
mod scenarios;
