// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The four subcommands that run with administrator rights: `install <dir>`, `uninstall <dir>`,
//! `read-config <dir>` and `restart <dir>`. They get one argument, the path of a private staging
//! folder, and treat everything in it as untrusted because the user who started the setup can
//! write there.
//!
//! `install` also takes an optional `tray` (the tray icon program, on macOS an `.app` folder) and,
//! instead of a whole `agent.ini`, an optional `patch.json` that it merges into the installed
//! `agent.ini` itself, so a change needs the key neither in the window nor in a second prompt.
//!
//! The key lives in `<dir>/agent.ini`, never in argv. The outcome goes back as `<dir>/result.json`
//! (the elevation prompt separates the output from the calling window).
//!
//! Never call `install::detect` from here: this process is itself called `printpapi-agent` and
//! would find itself on Linux and macOS.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::client::{Client, Http, HttpError, http_status};
use crate::config::{
    Config, HttpSettings, Printer, apply_patch, decode_ini, parse_patch, parse_printers,
};
use crate::install::{self, Layout, Os};
use crate::proc::{Runner, real_runner};

/// SumatraPDF 3.6.1 (64 bit), pinned: 3.7 and later need a DLL next to the exe, and `orientation`
/// has to be retested with newer versions (roadmap 15).
pub const SUMATRA_URL: &str =
    "https://www.sumatrapdfreader.org/dl/rel/3.6.1/SumatraPDF-3.6.1-64.zip";
/// SHA-256 of the ZIP, measured on 2026-11-06 from the official server.
pub const SUMATRA_ZIP_SHA256: &str =
    "98b33a518d42986856d225064b0cd2d3643ecf78cbf84ab873d26cc51877a544";
/// SHA-256 of the `SumatraPDF-3.6.1-64.exe` inside that ZIP.
pub const SUMATRA_EXE_SHA256: &str =
    "719f689b34f47be8ca105ce8484948474dafde0e106bab599e4a89326070c3d0";

/// How often a locked file is tried (a running exe stays locked for a moment on Windows).
const FILE_ATTEMPTS: u32 = 10;

/// Why an admin subcommand stopped. `code` is what `result.json` carries for the window.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyError {
    /// The staging folder is not what the setup would have written.
    BadStaging(String),
    /// `409` from register: the key belongs to another device.
    KeyInUse,
    /// `401` from register. The server sends it for a refused (revoked) key and for a name that
    /// belongs to another key, and the setup cannot tell the two apart.
    NameTaken,
    /// `403` from register: a proxy or WAF in front of `/agent/*`.
    Proxy,
    Unreachable(String),
    System(String),
}

impl ApplyError {
    pub fn code(&self) -> &'static str {
        match self {
            ApplyError::BadStaging(_) => "bad_staging",
            ApplyError::KeyInUse => "key_in_use",
            ApplyError::NameTaken => "name_taken",
            ApplyError::Proxy => "proxy",
            ApplyError::Unreachable(_) => "unreachable",
            ApplyError::System(_) => "system",
        }
    }
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApplyError::BadStaging(m) => write!(f, "The setup's working folder is not usable: {m}"),
            ApplyError::KeyInUse => f.write_str(
                "This key already belongs to another device. Create a new key in the dashboard.",
            ),
            ApplyError::NameTaken => f.write_str(
                "The server refused this key. Either the key was revoked, or this device name \
                 belongs to a different key. Check the key on the Keys page of your dashboard, \
                 or pick another name.",
            ),
            ApplyError::Proxy => f.write_str(
                "A proxy or firewall in front of the server refused the agent's request. Add the \
                 headers it expects under [headers] in agent.ini (see docs/agent.md).",
            ),
            ApplyError::Unreachable(m) => write!(f, "The server could not be reached: {m}"),
            ApplyError::System(m) => f.write_str(m),
        }
    }
}

/// Everything the subcommands touch outside the staging folder, injectable for tests.
pub struct Env<'a> {
    pub os: Os,
    pub layout: Layout,
    pub sys: PathBuf,
    pub run: Runner,
    pub http_for: &'a dyn Fn(&HttpSettings) -> Box<dyn Http>,
    /// The running binary, which is what gets installed.
    pub self_exe: PathBuf,
    pub sleep: &'a dyn Fn(u64),
}

fn bad(msg: impl Into<String>) -> ApplyError {
    ApplyError::BadStaging(msg.into())
}

fn system(what: &str, e: impl fmt::Display) -> ApplyError {
    ApplyError::System(format!("{what}: {e}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn verify_bytes(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(format!("SHA-256 is {actual}, expected {expected_hex}"))
    }
}

/// Does the file at `path` have the SHA-256 `expected_hex` (any case)?
pub fn verify_sha256(path: &Path, expected_hex: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    verify_bytes(&bytes, expected_hex)
}

/// A symlink, or on Windows any reparse point (a junction is not a symlink to Rust).
fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// Refuse the folder if it or anything directly in it is a link.
// ponytail: a same-user attacker can still swap a file between this check and the read. The spec
// accepts that gap for an unsigned setup (roadmap 12 closes it), since the same user can swap the
// setup exe itself before the elevation prompt.
fn check_staging(staging: &Path) -> Result<(), ApplyError> {
    let meta = std::fs::symlink_metadata(staging).map_err(|e| bad(format!("{e}")))?;
    if is_link(&meta) || !meta.is_dir() {
        return Err(bad("the folder is a link or not a folder"));
    }
    let entries = std::fs::read_dir(staging).map_err(|e| bad(format!("{e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| bad(format!("{e}")))?;
        // `DirEntry::metadata` does not follow links
        let meta = entry.metadata().map_err(|e| bad(format!("{e}")))?;
        if is_link(&meta) {
            return Err(bad(format!(
                "{} is a link",
                entry.file_name().to_string_lossy()
            )));
        }
    }
    Ok(())
}

/// The checked contents of the staging folder.
struct Staged {
    /// `agent.ini` as UTF-8 text.
    text: String,
    base: String,
    key: String,
    name: String,
    settings: HttpSettings,
    printers: Vec<Printer>,
    sumatra: Option<Vec<u8>>,
    tray: Option<TrayStaged>,
}

/// The tray icon program as staged.
enum TrayStaged {
    /// Windows and Linux: the program file.
    File(Vec<u8>),
    /// macOS: the `.app` folder, copied by `ditto`.
    App(PathBuf),
}

/// An `[agent]` value. The parser's texts quote lines and raw values, and the key sits in those, so
/// an error here becomes a fixed text that names only the setting.
fn value(cfg: &Config, key: &str) -> Result<Option<String>, ApplyError> {
    cfg.get(key)
        .map_err(|_| bad(format!("agent.ini: {key} is not valid")))
}

fn required(cfg: &Config, key: &str) -> Result<String, ApplyError> {
    value(cfg, key)?.ok_or_else(|| bad(format!("agent.ini: {key} is missing")))
}

/// The `agent.ini` text to install: the staged file, or, when only a `patch.json` is staged, the
/// installed file with the patch merged in. The installed file is looked at only after the install
/// folder passed its checks, because this runs as root and would follow a planted link.
fn staged_ini_text(staging: &Path, env: &Env) -> Result<String, ApplyError> {
    match std::fs::read(staging.join("agent.ini")) {
        Ok(bytes) => Ok(decode_ini(&bytes)),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(staging.join("patch.json")).is_ok() =>
        {
            let json = std::fs::read(staging.join("patch.json"))
                .map_err(|e| bad(format!("patch.json cannot be read: {e}")))?;
            let json = String::from_utf8(json).map_err(|_| bad("patch.json is not UTF-8"))?;
            let patch = parse_patch(&json).map_err(bad)?;
            check_install_dir(&env.layout)?;
            let installed = std::fs::read(&env.layout.ini)
                .map_err(|e| system("cannot read the installed agent.ini", e))?;
            // the messages of `apply_patch` never quote the file: it holds the key
            apply_patch(&decode_ini(&installed), &patch).map_err(bad)
        }
        Err(e) => Err(bad(format!("agent.ini cannot be read: {e}"))),
    }
}

/// The staged tray program, if any. It must be what this system installs: a file, on macOS an
/// `.app` folder (a link was refused by `check_staging`).
fn staged_tray(staging: &Path, os: Os) -> Result<Option<TrayStaged>, ApplyError> {
    let path = staging.join("tray");
    match std::fs::symlink_metadata(&path) {
        Err(_) => Ok(None),
        Ok(meta) if os == Os::Macos && meta.is_dir() => Ok(Some(TrayStaged::App(path))),
        Ok(meta) if os != Os::Macos && meta.is_file() => {
            let bytes =
                std::fs::read(&path).map_err(|e| bad(format!("tray cannot be read: {e}")))?;
            Ok(Some(TrayStaged::File(bytes)))
        }
        Ok(_) => Err(bad(
            "tray is not what this system installs (a program file, on macOS an .app folder)",
        )),
    }
}

fn read_staged(staging: &Path, env: &Env, sumatra_sha: &str) -> Result<Staged, ApplyError> {
    check_staging(staging)?;
    let text = staged_ini_text(staging, env)?;
    // No parser text goes into an error from here on: the key never shows up in an output, and
    // the parsers quote the lines and values they choke on.
    let cfg = Config::from_text(&text).map_err(|_| {
        bad("agent.ini is not valid (a line that cannot be read, or a bad %-reference)")
    })?;
    let base = required(&cfg, "server_url")?
        .trim_end_matches('/')
        .to_string();
    let lower = base.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(bad(
            "agent.ini: server_url must start with http:// or https://",
        ));
    }
    let key = required(&cfg, "api_key")?;
    if !key.is_empty() && base.contains(&key) {
        return Err(bad("agent.ini: server_url must not contain the key"));
    }
    let name = value(&cfg, "name")?.unwrap_or_else(|| "agent".into());
    let printers = parse_printers(&required(&cfg, "printers")?)
        .map_err(|_| bad("agent.ini: printers is not valid"))?;
    let settings = cfg
        .http_settings()
        .map_err(|_| bad("agent.ini: timeout or [headers] is not valid"))?;
    let sumatra = match std::fs::symlink_metadata(staging.join("SumatraPDF.exe")) {
        Ok(_) => {
            let exe = std::fs::read(staging.join("SumatraPDF.exe"))
                .map_err(|e| bad(format!("SumatraPDF.exe cannot be read: {e}")))?;
            verify_bytes(&exe, sumatra_sha)
                .map_err(|e| bad(format!("SumatraPDF.exe is not the expected version ({e})")))?;
            Some(exe)
        }
        Err(_) => None,
    };
    let tray = staged_tray(staging, env.os)?;
    Ok(Staged {
        text,
        base,
        key,
        name,
        settings,
        printers,
        sumatra,
        tray,
    })
}

/// `f` until it works, at most `attempts` times, with a second between tries.
fn retry<T>(
    attempts: u32,
    sleep: &dyn Fn(u64),
    f: &mut dyn FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut tries = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if tries >= attempts => return Err(e),
            Err(_) => {
                sleep(1);
                tries += 1;
            }
        }
    }
}

/// Rights come from an explicit mode, never from the source file or the umask.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> std::io::Result<()> {
    Ok(())
}

/// A new file at `path` that only this process has open. On Unix it is created with `mode` (the
/// umask of the caller can only narrow it), so it is never wider than that, not even for a moment.
fn create_new_file(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    // a leftover of a run that died keeps its owner and rights when written into again
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    opts.open(path)
}

/// Put `bytes` at `to` as a new file: written next to it, given exactly `mode` (Unix; the umask
/// of the caller, which pkexec does not reset, must not decide), then renamed over `to`. A reader
/// never sees half a file, and on macOS a new inode matters, because the kernel caches the code
/// signature per vnode and kills a binary that was overwritten in place.
fn place_file(to: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let tmp = to.with_file_name(format!(".{name}.new"));
    let result = create_new_file(&tmp, mode)
        .and_then(|mut f| f.write_all(bytes))
        .and_then(|()| set_mode(&tmp, mode))
        .and_then(|()| std::fs::rename(&tmp, to));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Put a copy of `from` at `to`, with the rights the service user needs to run it.
///
/// The bytes are written, not `fs::copy`'d: as root, macOS's copy (`fclonefileat`, `fcopyfile`)
/// keeps the source's owner, which is the user who started the setup. The LaunchDaemon runs as
/// root, so that user could then rewrite what root runs. A file this process creates belongs to
/// this process (root, elevated).
fn install_binary(from: &Path, to: &Path) -> std::io::Result<()> {
    place_file(to, &std::fs::read(from)?, 0o755)
}

/// Replace `agent.ini` with `text` without ever having an empty or half written one: the new file
/// is made next to it, gets its restrictive rights while it is still empty (the key comes only
/// after), and is renamed over the old one. A run that dies in between leaves the installed
/// `agent.ini`, and with it the key, as it was.
fn write_ini(env: &Env, text: &str) -> Result<(), ApplyError> {
    let tmp = env.layout.ini.with_file_name(".agent.ini.new");
    let result = (|| {
        create_new_file(&tmp, 0o600).map_err(|e| system("cannot create agent.ini", e))?;
        for c in install::ini_acl_cmds(env.os, &tmp, &env.sys) {
            (env.run)(&c).map_err(|e| system("cannot restrict agent.ini", e))?;
        }
        std::fs::write(&tmp, text.as_bytes()).map_err(|e| system("cannot write agent.ini", e))?;
        std::fs::rename(&tmp, &env.layout.ini).map_err(|e| system("cannot replace agent.ini", e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Delete everything in `dir` but `keep`, entries that are already gone included.
fn clear_dir_except(dir: &Path, keep: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path == keep {
            continue;
        }
        let gone = if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match gone {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// What an earlier run left at the install folder and `agent.ini` is looked at before anything is
/// done with it. The commands run elevated: a link there would send root's reads, writes and
/// deletes wherever it points, and a folder that another user owns lets that user swap what root
/// runs. `root` is whether this process is root, a parameter so a test can ask both ways.
#[cfg(unix)]
fn check_install_dir_as(l: &Layout, root: bool) -> Result<(), ApplyError> {
    use std::os::unix::fs::MetadataExt;
    let refuse = |what: String| Err(ApplyError::System(what));
    match std::fs::symlink_metadata(&l.dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return refuse(format!(
                "{} is a symbolic link, and the setup does not follow it. Remove the link and try again.",
                l.dir.display()
            ));
        }
        Ok(meta) if !meta.is_dir() => {
            return refuse(format!("{} is not a folder.", l.dir.display()));
        }
        Ok(meta) if root && meta.uid() != 0 => {
            return refuse(format!(
                "{} is not owned by root, and the setup does not use a folder that another user controls. Check it, remove it and try again.",
                l.dir.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(system("cannot look at the install folder", e)),
    }
    match std::fs::symlink_metadata(&l.ini) {
        Ok(meta) if meta.file_type().is_symlink() => refuse(format!(
            "{} is a symbolic link, and the setup does not follow it. Remove the link and try again.",
            l.ini.display()
        )),
        _ => Ok(()),
    }
}

/// `check_install_dir_as` for the real user id (`std` has no `geteuid`, hence the `libc` crate).
#[cfg(unix)]
fn check_install_dir(l: &Layout) -> Result<(), ApplyError> {
    check_install_dir_as(l, unsafe { libc::geteuid() } == 0)
}

// ponytail: Windows has no check. The folder is under `%ProgramFiles%`, which only administrators
// can write to, so nobody else can plant a link there.
#[cfg(not(unix))]
fn check_install_dir(_: &Layout) -> Result<(), ApplyError> {
    Ok(())
}

/// Install. Returns `last_seen_at` of this device as read right after registering, which the
/// window compares against to know the agent really started.
pub fn apply(staging: &Path, env: &Env) -> Result<f64, ApplyError> {
    apply_with(staging, env, SUMATRA_EXE_SHA256)
}

fn apply_with(staging: &Path, env: &Env, sumatra_sha: &str) -> Result<f64, ApplyError> {
    let staged = read_staged(staging, env, sumatra_sha)?;
    let l = &env.layout;
    // before the server or the machine is touched
    check_install_dir(l)?;

    // Registering first: it is where a wrong key, a taken name or a proxy show up, and until it
    // worked nothing on the machine or the server has changed.
    let http = (env.http_for)(&staged.settings);
    let reg = crate::run::register(
        &*http,
        &staged.base,
        &staged.key,
        &staged.name,
        &staged.printers,
    )
    .map_err(|e| match http_status(&e) {
        Some(409) => ApplyError::KeyInUse,
        Some(401) => ApplyError::NameTaken,
        Some(403) => ApplyError::Proxy,
        _ => ApplyError::Unreachable(e.to_string()),
    })?;
    let computer_id = reg
        .as_ref()
        .and_then(|r| r.get("computer_id"))
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            ApplyError::System("the server's register reply has no computer_id".into())
        })?;
    // This first read is the check that the server lists the device. The value that counts is read
    // again after the old agent is stopped (see below).
    let first_seen = last_seen(&*http, &staged, computer_id)?;

    if let Some(check) = install::identity_check(env.os)
        && (env.run)(&check).is_err()
    {
        for c in install::identity_cmds(env.os) {
            (env.run)(&c).map_err(|e| system("creating the service user failed", e))?;
        }
    }
    // there is nothing to stop on a first install; on a reinstall ("change printers") the old agent
    // is the one to bring back if a later step fails
    let was_installed = l.marker.is_file();
    for c in install::stop_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    // The icons of all sessions go before their program is replaced: a running exe is locked on
    // Windows. Only when a new one comes; a change of printers leaves the icons alone.
    if staged.tray.is_some() {
        for c in install::tray_stop_cmds(env.os, l, &env.sys) {
            let _ = (env.run)(&c);
        }
    }
    replace_install(env, &staged, &*http, computer_id, first_seen).inspect_err(|_| {
        if was_installed {
            // best effort: the original error is the one the window shows
            for c in install::start_cmds(env.os, l, &env.sys) {
                let _ = (env.run)(&c);
            }
        }
    })
}

/// Everything after the old agent is stopped: files, key, marker, autostart, start. Returns the
/// baseline `last_seen_at`.
fn replace_install(
    env: &Env,
    staged: &Staged,
    http: &dyn Http,
    computer_id: i64,
    first_seen: f64,
) -> Result<f64, ApplyError> {
    let l = &env.layout;

    std::fs::create_dir_all(&l.dir).map_err(|e| system("cannot create the install folder", e))?;
    // the service user has to get into the folder whatever the umask of the caller is
    set_mode(&l.dir, 0o755).map_err(|e| system("cannot set the install folder's rights", e))?;
    retry(FILE_ATTEMPTS, env.sleep, &mut || {
        install_binary(&env.self_exe, &l.binary)
    })
    .map_err(|e| system("cannot copy the agent", e))?;
    if let Some(exe) = &staged.sumatra {
        retry(FILE_ATTEMPTS, env.sleep, &mut || {
            place_file(&l.sumatra, exe, 0o755)
        })
        .map_err(|e| system("cannot copy SumatraPDF", e))?;
    }
    write_ini(env, &staged.text)?;
    // The marker before the autostart file: a run that dies in between is found as ours next time.
    place_file(&l.marker, b"", 0o644).map_err(|e| system("cannot write the marker", e))?;
    if let Some(parent) = l.autostart.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| system("cannot create the autostart folder", e))?;
    }
    place_file(&l.autostart, &install::autostart_file(env.os, l), 0o644)
        .map_err(|e| system("cannot write the autostart file", e))?;
    // the agent writes its status and log there from its first second
    prepare_status_dir(env)?;
    if let Some(tray) = &staged.tray {
        install_tray(env, tray)?;
    }
    // The old agent is stopped by now. Its long-poll bumped last_seen_at until then, so the value
    // the window compares against is read here, not before. A failing read keeps the first one.
    let baseline = last_seen(http, staged, computer_id).unwrap_or(first_seen);
    start_agent(env)?;
    Ok(baseline)
}

/// Register the autostart and start the agent.
fn start_agent(env: &Env) -> Result<(), ApplyError> {
    for (i, c) in install::start_cmds(env.os, &env.layout, &env.sys)
        .iter()
        .enumerate()
    {
        // the first macOS command removes the quarantine attribute, which a local file may lack
        let optional = env.os == Os::Macos && i == 0;
        if let Err(e) = (env.run)(c)
            && !optional
        {
            return Err(system("cannot start the agent", e));
        }
    }
    Ok(())
}

/// The user ids that may own the status folder: whoever runs this (root, elevated) and, on Linux,
/// the service user the agent runs as.
#[cfg(unix)]
fn status_dir_owners(os: Os) -> Vec<u32> {
    // SAFETY: geteuid has no arguments and cannot fail
    let mut owners = vec![unsafe { libc::geteuid() }];
    if os == Os::Linux
        && let Ok(name) = std::ffi::CString::new(install::SERVICE_USER)
    {
        // SAFETY: `name` is a valid C string; the returned record is read at once, before any
        // other call could reuse it
        let entry = unsafe { libc::getpwnam(name.as_ptr()) };
        if !entry.is_null() {
            owners.push(unsafe { (*entry).pw_uid });
        }
    }
    owners
}

// ponytail: Windows has no owner check here. `status_dir_cmds` takes the folder's owner and ACL over
// with icacls, but a file that a user planted inside keeps its own owner; the worst it can do is
// show a false status for itself, because the tray only reads, and the agent replaces the file by
// renaming.
#[cfg(not(unix))]
fn status_dir_owners(_: Os) -> Vec<u32> {
    Vec::new()
}

/// Look at the status folder before it is used: this runs elevated, and the folder is in a place
/// where a user may have made it first. A link is refused (root would write where it points), and
/// on Unix a folder that none of `owners` owns is deleted with what is in it and made again, so
/// the install never keeps a folder another user controls.
// ponytail: a user can still swap the folder between this check and the next step; the same gap
// as in `check_staging`, closed together with it by roadmap 12.
fn vet_status_dir(dir: &Path, owners: &[u32]) -> Result<(), ApplyError> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(system("cannot look at the status folder", e)),
    };
    if is_link(&meta) {
        return Err(ApplyError::System(format!(
            "{} is a symbolic link, and the setup does not follow it. Remove the link and try again.",
            dir.display()
        )));
    }
    if !meta.is_dir() {
        return Err(ApplyError::System(format!(
            "{} is not a folder.",
            dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !owners.contains(&meta.uid()) {
            std::fs::remove_dir_all(dir)
                .map_err(|e| system("cannot replace the status folder of another user", e))?;
        }
    }
    #[cfg(not(unix))]
    let _ = owners;
    Ok(())
}

/// Make the status folder (readable for everybody, writable for the agent only).
fn prepare_status_dir(env: &Env) -> Result<(), ApplyError> {
    let dir = &env.layout.status_dir;
    vet_status_dir(dir, &status_dir_owners(env.os))?;
    std::fs::create_dir_all(dir).map_err(|e| system("cannot create the status folder", e))?;
    set_mode(dir, 0o755).map_err(|e| system("cannot set the status folder's rights", e))?;
    for c in install::status_dir_cmds(env.os, &env.layout, &env.sys) {
        (env.run)(&c).map_err(|e| system("cannot set the status folder's rights", e))?;
    }
    Ok(())
}

/// Delete `path` whatever it is, without following a link. A missing path is fine.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(meta) if meta.is_dir() && !is_link(&meta) => std::fs::remove_dir_all(path),
        // a file or a link; a Windows junction is removed as a folder
        Ok(_) => std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path)),
    }
}

/// The tray icon: the program, its autostart for all users and, on Linux, the menu entry.
fn install_tray(env: &Env, tray: &TrayStaged) -> Result<(), ApplyError> {
    let l = &env.layout;
    match tray {
        TrayStaged::File(bytes) => {
            retry(FILE_ATTEMPTS, env.sleep, &mut || {
                place_file(&l.tray, bytes, 0o755)
            })
            .map_err(|e| system("cannot copy the tray icon", e))?;
        }
        TrayStaged::App(from) => {
            // the old app goes first, so nothing of it stays inside the new one
            retry(FILE_ATTEMPTS, env.sleep, &mut || remove_tree(&l.tray))
                .map_err(|e| system("cannot remove the old tray icon", e))?;
            for (i, c) in install::tray_copy_cmds(env.os, from, l, &env.sys)
                .iter()
                .enumerate()
            {
                // the second command removes the quarantine flag, which a local copy may lack
                if let Err(e) = (env.run)(c)
                    && i != 1
                {
                    return Err(system("cannot copy the tray icon", e));
                }
            }
        }
    }
    let files = [
        (
            l.tray_autostart.as_ref(),
            install::tray_autostart_file(env.os, l),
        ),
        (l.app_entry.as_ref(), Some(install::app_entry_file(l))),
    ];
    for (path, bytes) in files {
        let (Some(path), Some(bytes)) = (path, bytes) else {
            continue;
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| system("cannot create the folder of the tray autostart", e))?;
        }
        place_file(path, &bytes, 0o644)
            .map_err(|e| system("cannot write the tray autostart file", e))?;
    }
    for c in install::tray_register_cmds(env.os, l, &env.sys) {
        (env.run)(&c).map_err(|e| system("cannot register the tray icon", e))?;
    }
    Ok(())
}

/// Restart: stop the agent and start it again, so a changed `agent.ini` takes effect. Only for an
/// installation this setup made.
pub fn restart(env: &Env) -> Result<(), ApplyError> {
    let l = &env.layout;
    check_install_dir(l)?;
    if !l.marker.is_file() {
        return Err(ApplyError::System(
            "This agent was not installed by the setup, so the setup leaves it alone.".into(),
        ));
    }
    for c in install::stop_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    start_agent(env)
}

/// `last_seen_at` of this device on the server.
fn last_seen(http: &dyn Http, staged: &Staged, computer_id: i64) -> Result<f64, ApplyError> {
    let computers = http
        .get(&format!("{}/computers", staged.base), &staged.key)
        .map_err(|e: HttpError| ApplyError::Unreachable(e.to_string()))?;
    computers
        .as_ref()
        .and_then(|v| v.get("computers"))
        .and_then(Value::as_array)
        .and_then(|list| list.iter().find(|c| c["id"].as_i64() == Some(computer_id)))
        .map(|c| c["last_seen_at"].as_f64().unwrap_or(0.0))
        .ok_or_else(|| {
            ApplyError::System("the server does not list this device after registering".into())
        })
}

/// Uninstall: stop the agent, take out the autostart, delete the folder. Only for an installation
/// this setup made.
pub fn remove(env: &Env) -> Result<(), ApplyError> {
    let l = &env.layout;
    check_install_dir(l)?;
    if !l.marker.is_file() {
        return Err(ApplyError::System(
            "This agent was not installed by the setup, so the setup leaves it alone.".into(),
        ));
    }
    for c in install::stop_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    // the icons go too: on Windows their exe is locked, and "Quit" in a menu is not the removal
    for c in install::tray_stop_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    // A stop that failed leaves the process alive, and deleting its files would let it print on
    // without anything left to remove it. Nothing is deleted yet, so a second try stays possible.
    // A failing check means "not running".
    if let Some(check) = install::running_check(env.os)
        && (env.run)(&check).is_ok()
    {
        return Err(ApplyError::System(
            "The agent could not be stopped and is still running, so nothing was removed. Stop it (or restart the computer) and try again."
                .into(),
        ));
    }
    // ponytail: a failing delete (task or unit already gone from an earlier half run) is not told
    // apart from a stuck one. The folder goes next, so a leftover entry has nothing to start. A
    // stuck one stays registered with nothing behind it, and `install::detect` then reports it as
    // foreign until somebody removes it by hand.
    for c in install::remove_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    for c in install::tray_unregister_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    for file in [
        Some(&l.autostart),
        l.tray_autostart.as_ref(),
        l.app_entry.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        match std::fs::remove_file(file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(system("cannot delete the autostart file", e)),
        }
    }
    // the tray program (outside the install folder on macOS) and the status folder
    for path in [&l.tray, &l.status_dir] {
        retry(FILE_ATTEMPTS, env.sleep, &mut || remove_tree(path))
            .map_err(|e| system("cannot delete the tray icon and the status folder", e))?;
    }
    // The marker goes last: a locked file (a running exe, a hung SumatraPDF) stops the removal, and
    // a second try must still find the marker instead of leaving the folder behind as "foreign".
    retry(FILE_ATTEMPTS, env.sleep, &mut || {
        clear_dir_except(&l.dir, &l.marker)
    })
    .map_err(|e| system("cannot delete the install folder", e))?;
    retry(FILE_ATTEMPTS, env.sleep, &mut || {
        match std::fs::remove_file(&l.marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::remove_dir(&l.dir)
    })
    .map_err(|e| system("cannot delete the install folder", e))
}

/// Copy the protected `agent.ini` into the staging folder for "change printers", owned by whoever
/// owns that folder.
pub fn export_config(staging: &Path, env: &Env) -> Result<(), ApplyError> {
    check_install_dir(&env.layout)?;
    let ini = std::fs::read(&env.layout.ini)
        .map_err(|e| system("cannot read the installed agent.ini", e))?;
    write_fresh(staging, "agent.ini", &ini).map_err(|e| system("cannot write the copy", e))
}

/// `result.json` in the staging folder.
pub fn write_result(staging: &Path, result: &Value) -> Result<(), String> {
    write_fresh(staging, "result.json", result.to_string().as_bytes()).map_err(|e| e.to_string())
}

/// Write `<dir>/<name>` as a new file in a folder that is no link: an entry that is already there (a planted link included) is
/// removed, never followed, and the file is created with `create_new`, which refuses to open
/// anything that appeared meanwhile. On Unix the file goes to the owner of `dir`, because this
/// process runs as root and the setup, running as the user, has to read it.
fn write_fresh(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    // The folder itself must not be a link either: root would write into wherever it points.
    // ponytail: a same-user swap between this check and the write stays possible, see `check_staging`.
    let meta = std::fs::symlink_metadata(dir)?;
    if is_link(&meta) || !meta.is_dir() {
        return Err(std::io::Error::other(
            "the folder is a link or not a folder",
        ));
    }
    let path = dir.join(name);
    if std::fs::symlink_metadata(&path).is_ok() {
        std::fs::remove_file(&path)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, lchown};
        let owner = std::fs::metadata(dir)?;
        lchown(&path, Some(owner.uid()), Some(owner.gid()))?;
    }
    Ok(())
}

/// The admin subcommands. `None` when `args` (without the program name) is not one of them, so
/// `main` runs the agent as before.
pub fn cli(args: &[String]) -> Option<ExitCode> {
    let os = Os::host();
    let http_for = |s: &HttpSettings| -> Box<dyn Http> { Box::new(Client::new(s)) };
    let sleep = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));
    let env = std::env::current_exe()
        .map(|self_exe| Env {
            os,
            layout: install::layout(os, &install::program_files()),
            sys: install::system_dir(),
            run: real_runner(),
            http_for: &http_for,
            self_exe,
            sleep: &sleep,
        })
        .map_err(|e| system("cannot find the running program", e));
    run_args(args, env.as_ref().map_err(Clone::clone)).map(ExitCode::from)
}

/// `cli` with the machine handed in, so tests never touch the real one. The exit code is 0 for
/// success, 1 for a failure and 2 for a wrong call.
fn run_args(args: &[String], env: Result<&Env, ApplyError>) -> Option<u8> {
    let command = match args.first().map(String::as_str) {
        Some(c @ ("install" | "uninstall" | "read-config" | "restart")) => c,
        _ => return None,
    };
    let [_, dir] = args else {
        crate::err_line(
            "usage: printpapi-agent install|uninstall|read-config|restart <staging folder>",
        );
        return Some(2);
    };
    let staging = Path::new(dir);
    let outcome = env.and_then(|env| match command {
        "install" => apply(staging, env).map(|b| json!({"ok": true, "baseline_last_seen": b})),
        "uninstall" => remove(env).map(|()| json!({"ok": true})),
        "restart" => restart(env).map(|()| json!({"ok": true})),
        _ => export_config(staging, env).map(|()| json!({"ok": true})),
    });
    let (result, code) = match outcome {
        Ok(v) => (v, 0),
        Err(e) => {
            crate::err_line(&e.to_string());
            (
                json!({"ok": false, "code": e.code(), "message": e.to_string()}),
                1,
            )
        }
    };
    if let Err(e) = write_result(staging, &result) {
        crate::err_line(&format!("cannot write result.json: {e}"));
        return Some(1);
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// sha256("abc"), the FIPS 180-2 example
    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    type Events = Rc<RefCell<Vec<String>>>;
    type HttpFor = Box<dyn Fn(&HttpSettings) -> Box<dyn Http>>;
    type Reply = Rc<RefCell<Result<Option<Value>, HttpError>>>;
    type Later = Rc<RefCell<Option<Result<Option<Value>, HttpError>>>>;

    struct FakeHttp {
        events: Events,
        register: Reply,
        computers: Reply,
        /// what every GET /computers after the first one answers, when a test sets it
        later: Later,
        seen: Rc<RefCell<Vec<(String, String)>>>,
    }

    impl Http for FakeHttp {
        fn get(&self, url: &str, key: &str) -> Result<Option<Value>, HttpError> {
            let first = !self.events.borrow().iter().any(|e| e == "GET /computers");
            self.events.borrow_mut().push("GET /computers".into());
            self.seen.borrow_mut().push((url.into(), key.into()));
            match (first, self.later.borrow().clone()) {
                (false, Some(later)) => later,
                _ => self.computers.borrow().clone(),
            }
        }
        fn get_bytes(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, HttpError> {
            unreachable!("apply downloads nothing")
        }
        fn post(&self, url: &str, key: &str, _: &Value) -> Result<Option<Value>, HttpError> {
            self.events.borrow_mut().push("POST /agent/register".into());
            self.seen.borrow_mut().push((url.into(), key.into()));
            self.register.borrow().clone()
        }
    }

    struct Fixture {
        tmp: tempfile::TempDir,
        staging: PathBuf,
        layout: Layout,
        os: Os,
        events: Events,
        register: Reply,
        computers: Reply,
        later: Later,
        seen: Rc<RefCell<Vec<(String, String)>>>,
        /// the settings each http client was made with
        settings: Rc<RefCell<Vec<HttpSettings>>>,
        /// commands whose joined argv contains one of these fail
        failing: Rc<RefCell<Vec<String>>>,
        /// what the check "is the agent still running?" answers
        running: Rc<std::cell::Cell<bool>>,
        run: Runner,
        http_for: HttpFor,
        sleep: Box<dyn Fn(u64)>,
    }

    const INI: &str = "[agent]\nserver_url = https://print.example.com/\napi_key = SECRET-KEY\nname = shop-pc\nprinters = Büro Drucker|pdf = file:///tmp\n";

    impl Fixture {
        fn new(os: Os) -> Fixture {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("opt");
            let name = if os == Os::Windows {
                "printpapi-agent.exe"
            } else {
                "printpapi-agent"
            };
            let layout = Layout {
                binary: dir.join(name),
                ini: dir.join("agent.ini"),
                marker: dir.join(".installed-by-setup"),
                sumatra: dir.join("SumatraPDF.exe"),
                autostart: tmp.path().join("etc").join("autostart-file"),
                status_dir: tmp.path().join("status"),
                tray: if os == Os::Macos {
                    tmp.path().join("Applications").join("printpapi Agent.app")
                } else {
                    dir.join(if os == Os::Windows {
                        "printpapi-tray.exe"
                    } else {
                        "printpapi-tray"
                    })
                },
                tray_autostart: (os != Os::Windows)
                    .then(|| tmp.path().join("xdg").join("printpapi-tray-autostart")),
                app_entry: (os == Os::Linux)
                    .then(|| tmp.path().join("apps").join("printpapi-agent.desktop")),
                dir,
            };
            let staging = tmp.path().join("staging");
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("agent.ini"), INI).unwrap();
            std::fs::write(tmp.path().join("self.exe"), b"the running binary").unwrap();
            let events: Events = Rc::default();
            let register: Reply = Rc::new(RefCell::new(Ok(Some(
                json!({"computer_id": 7, "printer_ids": {}}),
            ))));
            let computers: Reply = Rc::new(RefCell::new(Ok(Some(json!({"computers": [
                {"id": 3, "name": "other", "last_seen_at": 1.0},
                {"id": 7, "name": "shop-pc", "last_seen_at": 123.5}
            ]})))));
            let later: Later = Rc::default();
            let seen: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
            let settings: Rc<RefCell<Vec<HttpSettings>>> = Rc::default();
            let failing: Rc<RefCell<Vec<String>>> = Rc::default();
            let running: Rc<std::cell::Cell<bool>> = Rc::default();
            let run: Runner = {
                let (events, failing, running) = (events.clone(), failing.clone(), running.clone());
                let layout = layout.clone();
                Rc::new(move |c| {
                    let line = c.argv.join(" ");
                    let mut note = format!("run {line}");
                    // what the disk looks like at the moment this command runs
                    if line.contains("chmod") || line.contains("icacls") {
                        // the file the command is about: it is empty while its rights are set
                        // (the status folder is not in the install folder and has no note)
                        let dir = layout.dir.display().to_string();
                        if let Some(file) = c.argv.iter().find(|a| a.starts_with(&dir)) {
                            let len = std::fs::metadata(file).map(|m| m.len());
                            note.push_str(&format!(" [ini len {len:?}]"));
                        }
                    }
                    if line.contains("enable") || line.contains("/create") {
                        note.push_str(&format!(
                            " [marker {} autostart {}]",
                            layout.marker.exists(),
                            layout.autostart.exists()
                        ));
                    }
                    events.borrow_mut().push(note);
                    // success of the check means "still running"; the agent is stopped by default
                    if install::running_check(os).as_ref() == Some(c) {
                        return if running.get() {
                            Ok(Vec::new())
                        } else {
                            Err(format!("not running: {line}"))
                        };
                    }
                    if failing.borrow().iter().any(|f| line.contains(f.as_str())) {
                        Err(format!("failed: {line}"))
                    } else {
                        Ok(Vec::new())
                    }
                })
            };
            let http_for: HttpFor = {
                let (events, register, computers, later, seen, settings) = (
                    events.clone(),
                    register.clone(),
                    computers.clone(),
                    later.clone(),
                    seen.clone(),
                    settings.clone(),
                );
                Box::new(move |s| {
                    settings.borrow_mut().push(s.clone());
                    Box::new(FakeHttp {
                        events: events.clone(),
                        register: register.clone(),
                        computers: computers.clone(),
                        later: later.clone(),
                        seen: seen.clone(),
                    })
                })
            };
            Fixture {
                tmp,
                staging,
                layout,
                os,
                events,
                register,
                computers,
                later,
                seen,
                settings,
                failing,
                running,
                run,
                http_for,
                sleep: Box::new(|_| {}),
            }
        }

        fn env(&self) -> Env<'_> {
            Env {
                os: self.os,
                layout: self.layout.clone(),
                sys: PathBuf::from(if self.os == Os::Windows {
                    "C:\\Windows\\System32"
                } else {
                    "/"
                }),
                run: self.run.clone(),
                http_for: &*self.http_for,
                self_exe: self.tmp.path().join("self.exe"),
                sleep: &*self.sleep,
            }
        }

        fn events(&self) -> Vec<String> {
            self.events.borrow().clone()
        }
    }

    fn refuse(fx: &Fixture, status: u16) {
        *fx.register.borrow_mut() = Err(HttpError::Os(format!("server returned {status}")));
    }

    #[test]
    fn a_refused_register_touches_nothing() {
        for (status, expected) in [
            (409, ApplyError::KeyInUse),
            (401, ApplyError::NameTaken),
            (403, ApplyError::Proxy),
        ] {
            let fx = Fixture::new(Os::Linux);
            refuse(&fx, status);
            assert_eq!(apply(&fx.staging, &fx.env()), Err(expected), "{status}");
            assert_eq!(fx.events(), vec!["POST /agent/register"], "{status}");
            assert!(!fx.layout.dir.exists(), "{status}");
            assert!(!fx.layout.autostart.exists(), "{status}");
        }
    }

    #[test]
    fn a_401_names_both_causes() {
        // the server answers 401 to a refused (revoked) key as well as to a name that belongs to
        // another key, and the setup cannot tell them apart
        let text = ApplyError::NameTaken.to_string();
        assert!(text.contains("revoked"), "{text}");
        assert!(text.contains("name"), "{text}");
        assert_eq!(ApplyError::NameTaken.code(), "name_taken");
    }

    #[test]
    fn no_message_has_a_run_of_spaces() {
        let all = [
            ApplyError::BadStaging("x".into()),
            ApplyError::KeyInUse,
            ApplyError::NameTaken,
            ApplyError::Proxy,
            ApplyError::Unreachable("x".into()),
            ApplyError::System("x".into()),
        ];
        for e in all {
            let text = e.to_string();
            assert!(!text.contains("  "), "{text}");
        }
    }

    #[test]
    fn any_other_register_failure_is_unreachable() {
        let fx = Fixture::new(Os::Linux);
        *fx.register.borrow_mut() = Err(HttpError::Os("connection failed: refused".into()));
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "unreachable");
        assert!(e.to_string().contains("connection failed"), "{e}");
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn the_key_is_sent_as_the_key_and_the_url_is_normalised() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let seen = fx.seen.borrow();
        assert_eq!(seen[0].0, "https://print.example.com/agent/register");
        assert_eq!(seen[1].0, "https://print.example.com/computers");
        assert!(seen.iter().all(|(_, k)| k == "SECRET-KEY"));
    }

    #[test]
    fn steps_run_in_order() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        // the rights are set on the new file, which replaces agent.ini only once it is complete
        let ini = fx.layout.dir.join(".agent.ini.new");
        let ini = ini.display();
        let status = fx.layout.status_dir.display();
        assert_eq!(
            fx.events(),
            vec![
                "POST /agent/register".to_string(),
                "GET /computers".to_string(),
                "run id -u printpapi".to_string(),
                "run systemctl stop printpapi-agent".to_string(),
                // the new file is empty while the ACL is set, the key comes after
                format!("run chown root:printpapi {ini}"),
                format!("run chmod 640 {ini} [ini len Ok(0)]"),
                // the agent writes its status and log there: the folder and its rights come first
                format!("run chown printpapi {status}"),
                format!("run chmod 755 {status}"),
                // read again once the old agent is stopped, right before the new one starts
                "GET /computers".to_string(),
                "run systemctl daemon-reload".to_string(),
                "run systemctl enable --now printpapi-agent [marker true autostart true]"
                    .to_string(),
            ]
        );
        let ini = std::fs::read_to_string(&fx.layout.ini).unwrap();
        assert!(ini.contains("api_key = SECRET-KEY"), "{ini}");
        assert!(ini.contains("Büro Drucker"), "{ini}");
        assert!(fx.layout.marker.exists());
        let unit = std::fs::read_to_string(&fx.layout.autostart).unwrap();
        assert!(unit.contains("User=printpapi"), "{unit}");
    }

    #[test]
    fn the_service_user_is_created_only_when_missing() {
        let fx = Fixture::new(Os::Linux);
        fx.failing.borrow_mut().push("id -u".into());
        apply(&fx.staging, &fx.env()).unwrap();
        let ev = fx.events();
        let id = ev.iter().position(|e| e.starts_with("run id -u")).unwrap();
        assert!(
            ev[id + 1].starts_with("run useradd --system --no-create-home"),
            "{ev:?}"
        );
        assert!(ev[id + 2].starts_with("run systemctl stop"), "{ev:?}");
    }

    #[test]
    fn baseline_is_read_before_the_agent_starts() {
        let fx = Fixture::new(Os::Linux);
        assert_eq!(apply(&fx.staging, &fx.env()), Ok(123.5));
        let ev = fx.events();
        let get = ev.iter().rposition(|e| e == "GET /computers").unwrap();
        let start = ev.iter().position(|e| e.contains("enable")).unwrap();
        assert!(get < start, "{ev:?}");
    }

    #[test]
    fn the_baseline_is_read_after_the_old_agent_is_stopped() {
        // an old agent still long-polling bumps last_seen_at until it is stopped: a value read
        // before that would let the window report success for an agent that never started
        let fx = Fixture::new(Os::Linux);
        *fx.later.borrow_mut() = Some(Ok(Some(json!({"computers": [
            {"id": 7, "name": "shop-pc", "last_seen_at": 130.25}
        ]}))));
        assert_eq!(apply(&fx.staging, &fx.env()), Ok(130.25));
        let ev = fx.events();
        let stop = ev
            .iter()
            .position(|e| e.contains("systemctl stop"))
            .unwrap();
        let last_get = ev.iter().rposition(|e| e == "GET /computers").unwrap();
        assert!(stop < last_get, "{ev:?}");
    }

    #[test]
    fn a_failing_second_read_keeps_the_first_value() {
        let fx = Fixture::new(Os::Linux);
        *fx.later.borrow_mut() = Some(Err(HttpError::Os("connection failed".into())));
        assert_eq!(apply(&fx.staging, &fx.env()), Ok(123.5));
    }

    #[test]
    fn a_device_that_is_missing_after_register_is_a_system_error() {
        let fx = Fixture::new(Os::Linux);
        *fx.computers.borrow_mut() =
            Ok(Some(json!({"computers": [{"id": 3, "last_seen_at": 1.0}]})));
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn installs_its_own_binary() {
        let fx = Fixture::new(Os::Windows);
        std::fs::write(fx.staging.join("printpapi-agent.exe"), b"planted").unwrap();
        std::fs::write(fx.staging.join("printpapi-agent"), b"planted").unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        assert_eq!(
            std::fs::read(&fx.layout.binary).unwrap(),
            b"the running binary"
        );
    }

    #[test]
    fn a_second_run_finishes_a_broken_install() {
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        std::fs::write(&fx.layout.binary, b"half copied").unwrap();
        assert!(!fx.layout.marker.exists());
        apply(&fx.staging, &fx.env()).unwrap();
        assert!(fx.layout.marker.exists());
        assert_eq!(
            std::fs::read(&fx.layout.binary).unwrap(),
            b"the running binary"
        );
        // and once more over a finished install
        apply(&fx.staging, &fx.env()).unwrap();
        assert!(fx.layout.marker.exists());
    }

    #[test]
    fn wrong_sumatra_is_refused_before_anything_happens() {
        let fx = Fixture::new(Os::Windows);
        std::fs::write(fx.staging.join("SumatraPDF.exe"), b"not the real one").unwrap();
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "bad_staging");
        assert!(fx.events().is_empty(), "{:?}", fx.events());
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn the_pinned_sumatra_is_installed() {
        let fx = Fixture::new(Os::Windows);
        std::fs::write(fx.staging.join("SumatraPDF.exe"), b"abc").unwrap();
        apply_with(&fx.staging, &fx.env(), ABC_SHA).unwrap();
        assert_eq!(std::fs::read(&fx.layout.sumatra).unwrap(), b"abc");
    }

    #[test]
    fn without_sumatra_nothing_is_installed_for_it() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        assert!(!fx.layout.sumatra.exists());
    }

    #[test]
    fn an_unparseable_ini_is_refused_without_leaking_the_key() {
        let ok = "server_url = https://x\napi_key = SECRET\nprinters = a | raw | file:///t\n";
        let cases = [
            "this is [not an ini".to_string(),
            "[agent]\nserver_url = https://x\nprinters = a | raw | file:///t\n".to_string(), // no key
            "[agent]\nserver_url = https://x\napi_key = SECRET\nprinters = a = socket://nohost\n"
                .to_string(),
            "[agent]\nserver_url = ftp://x\napi_key = SECRET\nprinters = a | raw | file:///t\n"
                .to_string(),
            format!("[agent]\n{ok}[headers]\nAuthorization = x\n"),
            // places where a parser quotes what it chokes on, with the key right there
            "[agent]\nserver_url = https://x\napi_key = SECRET%(nope)s\nprinters = a | raw | file:///t\n"
                .to_string(),
            "[agent]\nserver_url = https://x\napi_key = SECRET%KEY\nprinters = a | raw | file:///t\n"
                .to_string(),
            format!("SECRET-KEY\n[agent]\n{ok}"),
            format!("[agent]\n{ok}SECRET-KEY\n"),
            format!("[agent]\n{ok}timeout = %(api_key)s\n"),
            "[agent]\nserver_url = https://x\napi_key = SECRET\nprinters = a = socket://%(api_key)s\n"
                .to_string(),
            "[agent]\nserver_url = https://x/%(api_key)s\napi_key = SECRET\nprinters = a | raw | file:///t\n"
                .to_string(),
        ];
        for bad in cases {
            let fx = Fixture::new(Os::Linux);
            std::fs::write(fx.staging.join("agent.ini"), &bad).unwrap();
            crate::ERR_LINES.with(|l| l.borrow_mut().clear());
            let e = apply(&fx.staging, &fx.env()).unwrap_err();
            assert_eq!(e.code(), "bad_staging", "{bad}");
            assert!(fx.events().is_empty(), "{bad}");
            assert!(!e.to_string().contains("SECRET"), "{bad}: {e}");
            // and on the way out through the entry point: result.json and the console
            let args = ["install".to_string(), fx.staging.display().to_string()];
            assert_eq!(run_args(&args, Ok(&fx.env())), Some(1), "{bad}");
            let result = std::fs::read_to_string(fx.staging.join("result.json")).unwrap();
            assert!(result.contains("bad_staging"), "{result}");
            assert!(!result.contains("SECRET"), "{bad}: {result}");
            crate::ERR_LINES.with(|l| {
                assert!(
                    l.borrow().iter().all(|line| !line.contains("SECRET")),
                    "{bad}: {:?}",
                    l.borrow()
                )
            });
        }
    }

    #[test]
    fn the_ini_settings_reach_the_http_client() {
        let fx = Fixture::new(Os::Linux);
        let ini = format!("{INI}timeout = 45\n[headers]\nX-Via = proxy 100%\n");
        std::fs::write(fx.staging.join("agent.ini"), ini).unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        let settings = fx.settings.borrow();
        assert_eq!(settings.len(), 1);
        assert_eq!(settings[0].timeout, 45.0);
        assert_eq!(
            settings[0].headers.get("x-via").map(String::as_str),
            Some("proxy 100%")
        );
    }

    #[test]
    fn an_ansi_ini_is_installed_as_utf8() {
        let fx = Fixture::new(Os::Linux);
        let (head, tail) = INI.split_once("Büro").unwrap();
        let mut bytes = head.as_bytes().to_vec();
        bytes.extend_from_slice(b"B\xfcro"); // cp1252, not valid UTF-8
        bytes.extend_from_slice(tail.as_bytes());
        std::fs::write(fx.staging.join("agent.ini"), bytes).unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        let installed = std::fs::read(&fx.layout.ini).unwrap();
        assert!(
            String::from_utf8(installed)
                .unwrap()
                .contains("Büro Drucker"),
            "the installed ini must be UTF-8 with the umlaut"
        );
    }

    #[test]
    fn a_missing_ini_is_refused() {
        let fx = Fixture::new(Os::Linux);
        std::fs::remove_file(fx.staging.join("agent.ini")).unwrap();
        assert_eq!(
            apply(&fx.staging, &fx.env()).unwrap_err().code(),
            "bad_staging"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_in_staging_are_refused() {
        let fx = Fixture::new(Os::Linux);
        let real = fx.tmp.path().join("real.ini");
        std::fs::write(&real, INI).unwrap();
        std::fs::remove_file(fx.staging.join("agent.ini")).unwrap();
        std::os::unix::fs::symlink(&real, fx.staging.join("agent.ini")).unwrap();
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "bad_staging");
        assert!(fx.events().is_empty());
        // any other link counts too
        let fx = Fixture::new(Os::Linux);
        std::os::unix::fs::symlink("/etc/shadow", fx.staging.join("loot")).unwrap();
        assert_eq!(
            apply(&fx.staging, &fx.env()).unwrap_err().code(),
            "bad_staging"
        );
    }

    #[cfg(windows)]
    #[test]
    fn junctions_in_staging_are_refused() {
        let fx = Fixture::new(Os::Windows);
        let target = fx.tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(fx.staging.join("junction"))
            .arg(&target)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            apply(&fx.staging, &fx.env()).unwrap_err().code(),
            "bad_staging"
        );
    }

    #[cfg(unix)]
    #[test]
    fn result_never_follows_a_planted_link() {
        let fx = Fixture::new(Os::Linux);
        let victim = fx.tmp.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        std::os::unix::fs::symlink(&victim, fx.staging.join("result.json")).unwrap();
        write_result(&fx.staging, &json!({"ok": true})).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        let written = std::fs::read_to_string(fx.staging.join("result.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&written).unwrap(),
            json!({"ok": true})
        );
    }

    #[test]
    fn write_result_replaces_an_old_result() {
        let fx = Fixture::new(Os::Linux);
        std::fs::write(fx.staging.join("result.json"), b"{\"ok\":false}").unwrap();
        write_result(&fx.staging, &json!({"ok": true, "baseline_last_seen": 1.5})).unwrap();
        let written = std::fs::read_to_string(fx.staging.join("result.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&written).unwrap(),
            json!({"ok": true, "baseline_last_seen": 1.5})
        );
    }

    #[test]
    fn a_failing_quarantine_removal_does_not_stop_the_install() {
        let fx = Fixture::new(Os::Macos);
        fx.failing.borrow_mut().push("xattr".into());
        apply(&fx.staging, &fx.env()).unwrap();
        let ev = fx.events();
        assert!(
            ev.iter().any(|e| e.contains("launchctl bootstrap system")),
            "{ev:?}"
        );
    }

    #[test]
    fn a_failing_launchctl_bootstrap_does_stop_it() {
        let fx = Fixture::new(Os::Macos);
        fx.failing.borrow_mut().push("bootstrap".into());
        assert_eq!(apply(&fx.staging, &fx.env()).unwrap_err().code(), "system");
    }

    #[test]
    fn a_failing_acl_command_stops_before_the_key_is_written() {
        let fx = Fixture::new(Os::Linux);
        fx.failing.borrow_mut().push("chmod".into());
        assert_eq!(apply(&fx.staging, &fx.env()).unwrap_err().code(), "system");
        // the key was never written, and nothing is left of the attempt
        assert!(!fx.layout.ini.exists());
        assert!(!fx.layout.dir.join(".agent.ini.new").exists());
        assert!(!fx.layout.marker.exists());
    }

    #[test]
    fn a_failing_install_keeps_the_installed_ini() {
        // "Change printers" must not empty the installed agent.ini before the new one is complete:
        // an abort in between would destroy the key
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let installed = std::fs::read(&fx.layout.ini).unwrap();
        let changed = INI.replace("shop-pc", "other-pc");
        std::fs::write(fx.staging.join("agent.ini"), changed).unwrap();
        fx.failing.borrow_mut().push("chmod".into());
        assert_eq!(apply(&fx.staging, &fx.env()).unwrap_err().code(), "system");
        assert_eq!(std::fs::read(&fx.layout.ini).unwrap(), installed);
        assert!(!fx.layout.dir.join(".agent.ini.new").exists());
    }

    #[test]
    fn a_second_install_replaces_the_ini_and_leaves_no_temporary_file() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let changed = INI.replace("shop-pc", "other-pc");
        std::fs::write(fx.staging.join("agent.ini"), changed).unwrap();
        // a leftover of a run that died
        std::fs::write(fx.layout.dir.join(".agent.ini.new"), b"half").unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        let ini = std::fs::read_to_string(&fx.layout.ini).unwrap();
        assert!(
            ini.contains("other-pc") && !ini.contains("shop-pc"),
            "{ini}"
        );
        let mut names: Vec<_> = std::fs::read_dir(&fx.layout.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [".installed-by-setup", "agent.ini", "printpapi-agent"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn rights_do_not_depend_on_the_umask_of_the_caller() {
        use std::os::unix::fs::PermissionsExt;
        // pkexec does not reset the umask, and 000 would make the systemd unit writable for all.
        // This is the only test that touches the umask, so nothing here races with another.
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let before = unsafe { libc::umask(0) };
        let fx = Fixture::new(Os::Linux);
        std::fs::write(fx.staging.join("SumatraPDF.exe"), b"abc").unwrap();
        let result = apply_with(&fx.staging, &fx.env(), ABC_SHA);
        let l = &fx.layout;
        let seen = [
            mode(&l.dir),
            mode(&l.binary),
            mode(&l.sumatra),
            mode(&l.marker),
            mode(&l.autostart),
            mode(&l.ini),
        ];
        unsafe { libc::umask(before) };
        result.unwrap();
        // the ini is 0600 until the (faked) chown and chmod run, never wider than that
        assert_eq!(seen, [0o755, 0o755, 0o755, 0o644, 0o644, 0o600]);
    }

    #[test]
    fn a_failing_stop_is_ignored() {
        let fx = Fixture::new(Os::Linux);
        fx.failing.borrow_mut().push("systemctl stop".into());
        apply(&fx.staging, &fx.env()).unwrap();
    }

    #[test]
    fn a_failure_after_the_stop_starts_the_old_agent_again() {
        for os in [Os::Linux, Os::Macos] {
            let fx = Fixture::new(os);
            apply(&fx.staging, &fx.env()).unwrap();
            fx.events.borrow_mut().clear();
            fx.failing.borrow_mut().push("chmod".into());
            fx.failing.borrow_mut().push("daemon-reload".into());
            let e = apply(&fx.staging, &fx.env()).unwrap_err();
            // the original error comes back, not one of the restart
            assert_eq!(e.code(), "system", "{os:?}");
            assert!(e.to_string().contains("chmod"), "{os:?}: {e}");
            let ev = fx.events();
            let stop = ev
                .iter()
                .position(|e| e.contains("stop") || e.contains("bootout"));
            let start = ev
                .iter()
                .position(|e| e.contains("enable --now") || e.contains("bootstrap"));
            assert!(stop < start && start.is_some(), "{os:?}: {ev:?}");
        }
    }

    #[test]
    fn a_failure_on_a_first_install_starts_nothing() {
        for os in [Os::Linux, Os::Macos] {
            let fx = Fixture::new(os);
            fx.failing.borrow_mut().push("chmod".into());
            let e = apply(&fx.staging, &fx.env()).unwrap_err();
            assert!(e.to_string().contains("chmod"), "{os:?}: {e}");
            let ev = fx.events();
            assert!(
                ev.iter()
                    .all(|e| !e.contains("enable") && !e.contains("bootstrap")),
                "{os:?}: {ev:?}"
            );
        }
    }

    #[test]
    fn retry_waits_and_gives_up() {
        let waits = RefCell::new(Vec::new());
        let sleep = |s: u64| waits.borrow_mut().push(s);
        let mut left = 3;
        let r = retry(10, &sleep, &mut || {
            if left > 0 {
                left -= 1;
                Err(std::io::Error::other("locked"))
            } else {
                Ok(5)
            }
        });
        assert_eq!(r.unwrap(), 5);
        assert_eq!(waits.borrow().len(), 3);
        waits.borrow_mut().clear();
        let mut tries = 0;
        let r: std::io::Result<()> = retry(10, &sleep, &mut || {
            tries += 1;
            Err(std::io::Error::other("locked"))
        });
        assert!(r.is_err());
        assert_eq!(tries, 10);
        assert_eq!(waits.borrow().len(), 9, "no wait after the last try");
    }

    #[test]
    fn remove_needs_the_marker() {
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        std::fs::write(&fx.layout.binary, b"hand made").unwrap();
        let e = remove(&fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(fx.layout.dir.exists());
        assert!(fx.events().is_empty());
    }

    #[test]
    fn remove_stops_unregisters_and_deletes() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        fx.events.borrow_mut().clear();
        remove(&fx.env()).unwrap();
        assert_eq!(
            fx.events(),
            vec![
                "run systemctl stop printpapi-agent".to_string(),
                format!("run pkill -f {}", fx.layout.tray.display()),
                "run systemctl is-active --quiet printpapi-agent".to_string(),
                "run systemctl disable printpapi-agent".to_string()
            ]
        );
        assert!(!fx.layout.dir.exists());
        assert!(!fx.layout.autostart.exists());
    }

    #[test]
    fn remove_deletes_nothing_while_the_agent_still_runs() {
        for os in [Os::Linux, Os::Macos] {
            let fx = Fixture::new(os);
            apply(&fx.staging, &fx.env()).unwrap();
            fx.events.borrow_mut().clear();
            // the stop failed and the process lives on: it would keep printing without its files
            fx.running.set(true);
            fx.failing.borrow_mut().push("stop".into());
            fx.failing.borrow_mut().push("bootout".into());
            let e = remove(&fx.env()).unwrap_err();
            assert_eq!(e.code(), "system", "{os:?}");
            assert!(e.to_string().contains("still running"), "{os:?}: {e}");
            assert!(fx.layout.dir.is_dir(), "{os:?}");
            assert!(fx.layout.binary.is_file(), "{os:?}");
            assert!(fx.layout.marker.is_file(), "{os:?}");
            assert!(fx.layout.autostart.is_file(), "{os:?}");
            let ev = fx.events();
            assert!(
                ev.iter()
                    .all(|e| !e.contains("disable") && !e.contains("remove")),
                "{os:?}: {ev:?}"
            );
            // a second try works once the agent is down
            fx.running.set(false);
            remove(&fx.env()).unwrap();
            assert!(!fx.layout.dir.exists(), "{os:?}");
        }
    }

    #[test]
    fn a_check_that_fails_means_the_agent_is_not_running() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        // `systemctl is-active` exits non-zero for a stopped unit
        fx.failing.borrow_mut().push("systemctl".into());
        remove(&fx.env()).unwrap();
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn remove_survives_failing_commands() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        fx.failing.borrow_mut().push("systemctl".into());
        remove(&fx.env()).unwrap();
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn export_config_copies_the_protected_ini_over_what_is_there() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        std::fs::write(fx.staging.join("agent.ini"), b"old").unwrap();
        export_config(&fx.staging, &fx.env()).unwrap();
        let copy = std::fs::read_to_string(fx.staging.join("agent.ini")).unwrap();
        assert!(copy.contains("api_key = SECRET-KEY"), "{copy}");
    }

    #[test]
    fn export_config_without_an_install_is_a_system_error() {
        let fx = Fixture::new(Os::Linux);
        assert_eq!(
            export_config(&fx.staging, &fx.env()).unwrap_err().code(),
            "system"
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_config_never_writes_through_a_planted_link() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let victim = fx.tmp.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        std::fs::remove_file(fx.staging.join("agent.ini")).unwrap();
        std::os::unix::fs::symlink(&victim, fx.staging.join("agent.ini")).unwrap();
        export_config(&fx.staging, &fx.env()).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    }

    #[cfg(unix)]
    #[test]
    fn export_config_hands_the_copy_to_the_staging_owner() {
        use std::os::unix::fs::{MetadataExt, chown};
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        // only root may give a folder away; as anybody else this test has nothing to show
        if chown(&fx.staging, Some(54321), Some(54321)).is_err() {
            return;
        }
        export_config(&fx.staging, &fx.env()).unwrap();
        let meta = std::fs::metadata(fx.staging.join("agent.ini")).unwrap();
        assert_eq!((meta.uid(), meta.gid()), (54321, 54321));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_install_folder_is_refused() {
        let fx = Fixture::new(Os::Linux);
        let elsewhere = fx.tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("agent.ini"), b"api_key = LOOT").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &fx.layout.dir).unwrap();
        for e in [
            apply(&fx.staging, &fx.env()).unwrap_err(),
            remove(&fx.env()).unwrap_err(),
            export_config(&fx.staging, &fx.env()).unwrap_err(),
        ] {
            assert_eq!(e.code(), "system");
            assert!(e.to_string().contains("symbolic link"), "{e}");
        }
        // nothing was touched: not registered, not written, no command run
        assert!(fx.events().is_empty(), "{:?}", fx.events());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 1);
        let staged = std::fs::read(fx.staging.join("agent.ini")).unwrap();
        assert_eq!(staged, INI.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_agent_ini_is_refused() {
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        let victim = fx.tmp.path().join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        std::os::unix::fs::symlink(&victim, &fx.layout.ini).unwrap();
        for e in [
            apply(&fx.staging, &fx.env()).unwrap_err(),
            export_config(&fx.staging, &fx.env()).unwrap_err(),
        ] {
            assert_eq!(e.code(), "system");
            assert!(e.to_string().contains("symbolic link"), "{e}");
        }
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert!(fx.events().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn as_root_an_install_folder_of_somebody_else_is_refused() {
        use std::os::unix::fs::chown;
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        // as root the folder is handed to another user, as anybody else it already is not root's
        let _ = chown(&fx.layout.dir, Some(54321), Some(54321));
        let e = check_install_dir_as(&fx.layout, true).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(e.to_string().contains("not owned by root"), "{e}");
        // without root rights the owner is of no interest (tests as a normal user, hand installs)
        assert_eq!(check_install_dir_as(&fx.layout, false), Ok(()));
        // the real call decides by the real user id
        let root = unsafe { libc::geteuid() } == 0;
        assert_eq!(check_install_dir(&fx.layout).is_err(), root);
        if root {
            assert_eq!(apply(&fx.staging, &fx.env()).unwrap_err().code(), "system");
            assert!(fx.events().is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_is_not_there_yet_is_fine() {
        let fx = Fixture::new(Os::Linux);
        assert_eq!(check_install_dir_as(&fx.layout, true), Ok(()));
        assert_eq!(check_install_dir(&fx.layout), Ok(()));
    }

    #[test]
    fn sha256_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(verify_sha256(&f, ABC_SHA), Ok(()));
        assert_eq!(verify_sha256(&f, &ABC_SHA.to_uppercase()), Ok(()));
        assert!(verify_sha256(&f, SUMATRA_EXE_SHA256).is_err());
        assert!(verify_sha256(&tmp.path().join("missing"), ABC_SHA).is_err());
    }

    #[test]
    fn error_codes_and_texts() {
        let all = [
            (ApplyError::BadStaging("x".into()), "bad_staging"),
            (ApplyError::KeyInUse, "key_in_use"),
            (ApplyError::NameTaken, "name_taken"),
            (ApplyError::Proxy, "proxy"),
            (ApplyError::Unreachable("x".into()), "unreachable"),
            (ApplyError::System("x".into()), "system"),
        ];
        for (e, code) in all {
            assert_eq!(e.code(), code);
            assert!(!e.to_string().is_empty());
        }
        assert!(ApplyError::Proxy.to_string().contains("[headers]"));
    }

    #[test]
    fn cli_ignores_other_arguments_and_reports_a_bad_staging_folder() {
        assert!(cli(&[]).is_none());
        assert!(cli(&["--version".to_string()]).is_none());
        // a staging folder without agent.ini fails before anything is touched
        let tmp = tempfile::tempdir().unwrap();
        let args = [
            "install".to_string(),
            tmp.path().to_string_lossy().into_owned(),
        ];
        assert!(cli(&args).is_some());
        let result: Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("result.json")).unwrap())
                .unwrap();
        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["code"], json!("bad_staging"));
    }

    /// A folder link: a symlink, or a junction on Windows.
    fn link_dir(link: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }
    }

    #[test]
    fn a_linked_staging_folder_is_never_written_through() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let real = fx.tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("agent.ini"), b"precious").unwrap();
        let link = fx.tmp.path().join("link");
        link_dir(&link, &real);

        assert!(write_result(&link, &json!({"ok": true})).is_err());
        assert_eq!(
            export_config(&link, &fx.env()).unwrap_err().code(),
            "system"
        );
        assert_eq!(std::fs::read(real.join("agent.ini")).unwrap(), b"precious");
        assert!(!real.join("result.json").exists());

        // none of the three subcommands writes its result through the link either
        for command in ["install", "uninstall", "read-config"] {
            let args = [command.to_string(), link.display().to_string()];
            assert_eq!(run_args(&args, Ok(&fx.env())), Some(1), "{command}");
            assert!(!real.join("result.json").exists(), "{command}");
            assert_eq!(std::fs::read(real.join("agent.ini")).unwrap(), b"precious");
        }
    }

    #[test]
    fn the_agent_is_stopped_before_its_files_are_replaced() {
        let fx = Fixture::new(Os::Linux);
        let binary_there = Rc::new(RefCell::new(Vec::new()));
        let mut env = fx.env();
        env.run = {
            let (binary_there, binary) = (binary_there.clone(), fx.layout.binary.clone());
            Rc::new(move |c| {
                if c.argv.join(" ").contains("stop") {
                    binary_there.borrow_mut().push(binary.exists());
                }
                Ok(Vec::new())
            })
        };
        apply(&fx.staging, &env).unwrap();
        assert_eq!(*binary_there.borrow(), vec![false]);
    }

    #[test]
    fn the_key_is_written_before_the_marker_and_the_marker_before_the_autostart_file() {
        // a marker that cannot be written: the key is already in
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.marker).unwrap();
        assert_eq!(apply(&fx.staging, &fx.env()).unwrap_err().code(), "system");
        let ini = std::fs::read_to_string(&fx.layout.ini).unwrap();
        assert!(ini.contains("api_key = SECRET-KEY"), "{ini}");
        assert!(!fx.layout.autostart.exists());

        // an autostart file that cannot be written: the marker is already there
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.autostart).unwrap();
        assert_eq!(apply(&fx.staging, &fx.env()).unwrap_err().code(), "system");
        assert!(fx.layout.marker.is_file());
        assert!(fx.events().iter().all(|e| !e.contains("enable")));
    }

    #[test]
    fn a_stuck_copy_is_tried_ten_times_and_leaves_nothing_behind() {
        let fx = Fixture::new(Os::Linux);
        // a folder where the binary should go: neither copying nor renaming over it works
        std::fs::create_dir_all(&fx.layout.binary).unwrap();
        let waits = RefCell::new(0);
        let sleep = |_: u64| *waits.borrow_mut() += 1;
        let mut env = fx.env();
        env.sleep = &sleep;
        assert_eq!(apply(&fx.staging, &env).unwrap_err().code(), "system");
        assert_eq!(*waits.borrow(), 9);
        assert!(!fx.layout.dir.join(".printpapi-agent.new").exists());
        assert!(!fx.layout.marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_installed_agent_is_runnable_by_the_service_user_whatever_the_source_rights() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fx = Fixture::new(Os::Linux);
        let src = fx.tmp.path().join("self.exe");
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o700)).unwrap();
        // a folder an earlier run (or a strict umask) made unreadable for others
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        std::fs::set_permissions(&fx.layout.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        assert_eq!(mode(&fx.layout.binary), 0o755);
        assert_eq!(mode(&fx.layout.dir), 0o755);
        // and over an existing binary with other rights
        std::fs::set_permissions(&fx.layout.binary, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        assert_eq!(mode(&fx.layout.binary), 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn the_installed_binary_never_keeps_the_owner_of_the_source() {
        // the setup is started by an ordinary user and elevated: the copy must belong to the
        // elevated process (root), not to the user whose file it was made from. Only meaningful as
        // root (CI container), where giving the source to someone else works; skipped otherwise.
        use std::os::unix::fs::{MetadataExt, chown};
        let fx = Fixture::new(Os::Linux);
        let src = fx.tmp.path().join("self.exe");
        if chown(&src, Some(501), Some(20)).is_err() {
            return;
        }
        // a leftover of an earlier run that died, owned by that user as well
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        let tmp = fx.layout.dir.join(".printpapi-agent.new");
        std::fs::write(&tmp, b"old").unwrap();
        chown(&tmp, Some(501), Some(20)).unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        assert_eq!(std::fs::metadata(&fx.layout.binary).unwrap().uid(), 0);
        assert_eq!(
            std::fs::read(&fx.layout.binary).unwrap(),
            b"the running binary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_binary_is_replaced_by_a_new_file_not_overwritten() {
        use std::os::unix::fs::MetadataExt;
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        // keep the old file alive, or the next one could be handed the same inode number
        let held = fx.layout.dir.join("held");
        std::fs::hard_link(&fx.layout.binary, &held).unwrap();
        let before = std::fs::metadata(&fx.layout.binary).unwrap().ino();
        apply(&fx.staging, &fx.env()).unwrap();
        assert_ne!(std::fs::metadata(&fx.layout.binary).unwrap().ino(), before);
    }

    /// Make deleting `layout.binary` fail until the returned guard is dropped. `None` when this
    /// user cannot (root ignores folder rights).
    fn lock_binary(fx: &Fixture) -> Option<Box<dyn std::any::Any>> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&fx.layout.binary)
                .unwrap();
            Some(Box::new(f))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            struct Restore(PathBuf);
            impl Drop for Restore {
                fn drop(&mut self) {
                    let _ =
                        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
                }
            }
            let dir = fx.layout.dir.clone();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let guard = Restore(dir.clone());
            if std::fs::File::create(dir.join("probe")).is_ok() {
                return None;
            }
            Some(Box::new(guard))
        }
    }

    #[test]
    fn a_failed_removal_can_be_repeated() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let Some(guard) = lock_binary(&fx) else {
            return;
        };
        let waits = RefCell::new(0);
        let sleep = |_: u64| *waits.borrow_mut() += 1;
        let mut env = fx.env();
        env.sleep = &sleep;
        assert_eq!(remove(&env).unwrap_err().code(), "system");
        assert_eq!(*waits.borrow(), 9);
        assert!(
            fx.layout.marker.is_file(),
            "the marker must outlive a failed removal"
        );
        assert!(fx.layout.binary.exists());
        drop(guard);
        remove(&fx.env()).unwrap();
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn the_commands_report_through_result_json_and_the_exit_code() {
        let fx = Fixture::new(Os::Linux);
        let dir = fx.staging.display().to_string();
        let result = |fx: &Fixture| -> Value {
            serde_json::from_slice(&std::fs::read(fx.staging.join("result.json")).unwrap()).unwrap()
        };
        // not installed yet: uninstall and read-config fail and say why
        for command in ["uninstall", "read-config"] {
            let args = [command.to_string(), dir.clone()];
            assert_eq!(run_args(&args, Ok(&fx.env())), Some(1), "{command}");
            assert_eq!(result(&fx)["code"], json!("system"), "{command}");
        }
        let args = ["install".to_string(), dir.clone()];
        assert_eq!(run_args(&args, Ok(&fx.env())), Some(0));
        assert_eq!(
            result(&fx),
            json!({"ok": true, "baseline_last_seen": 123.5})
        );
        // read-config puts the installed ini in the folder, uninstall removes the install
        std::fs::write(fx.staging.join("agent.ini"), b"old").unwrap();
        let args = ["read-config".to_string(), dir.clone()];
        assert_eq!(run_args(&args, Ok(&fx.env())), Some(0));
        assert_eq!(result(&fx), json!({"ok": true}));
        assert!(
            std::fs::read_to_string(fx.staging.join("agent.ini"))
                .unwrap()
                .contains("api_key = SECRET-KEY")
        );
        assert!(fx.layout.dir.exists());
        let args = ["uninstall".to_string(), dir.clone()];
        assert_eq!(run_args(&args, Ok(&fx.env())), Some(0));
        assert_eq!(result(&fx), json!({"ok": true}));
        assert!(!fx.layout.dir.exists());
    }

    #[test]
    fn a_missing_program_path_is_reported_in_result_json() {
        let fx = Fixture::new(Os::Linux);
        let args = ["uninstall".to_string(), fx.staging.display().to_string()];
        let err = ApplyError::System("cannot find the running program: x".into());
        assert_eq!(run_args(&args, Err(err)), Some(1));
        let written = std::fs::read_to_string(fx.staging.join("result.json")).unwrap();
        assert!(
            written.contains("cannot find the running program"),
            "{written}"
        );
    }

    #[test]
    fn a_wrong_number_of_arguments_is_a_usage_error() {
        let fx = Fixture::new(Os::Linux);
        let env = fx.env();
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(run_args(&words(&["install"]), Ok(&env)), Some(2));
        assert_eq!(run_args(&words(&["install", "a", "b"]), Ok(&env)), Some(2));
        assert!(run_args(&words(&["run"]), Ok(&env)).is_none());
        assert!(fx.events().is_empty());
    }

    // ---- tray icon, status folder, patch and restart ----

    impl Fixture {
        /// The tray program as the setup stages it: one file, on macOS an `.app` folder.
        fn stage_tray(&self, bytes: &[u8]) {
            let tray = self.staging.join("tray");
            if self.os == Os::Macos {
                std::fs::create_dir_all(tray.join("Contents")).unwrap();
                std::fs::write(tray.join("Contents").join("Info.plist"), bytes).unwrap();
            } else {
                std::fs::write(tray, bytes).unwrap();
            }
        }

        /// Stage a change instead of a whole agent.ini.
        fn stage_patch(&self, json: &str) {
            std::fs::remove_file(self.staging.join("agent.ini")).unwrap();
            std::fs::write(self.staging.join("patch.json"), json).unwrap();
        }

        /// `run` that notes what `watch` says about the disk whenever a command whose line
        /// contains one of `needles` runs.
        fn run_watching(
            &self,
            needles: &'static [&'static str],
            watch: impl Fn() -> String + 'static,
        ) -> (Runner, Rc<RefCell<Vec<String>>>) {
            let seen: Rc<RefCell<Vec<String>>> = Rc::default();
            let inner = self.run.clone();
            let log = seen.clone();
            let run: Runner = Rc::new(move |c| {
                let line = c.argv.join(" ");
                if needles.iter().any(|n| line.contains(n)) {
                    log.borrow_mut().push(watch());
                }
                inner(c)
            });
            (run, seen)
        }
    }

    fn tray_text(path: &Path) -> String {
        std::fs::read(path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| "gone".into())
    }

    #[test]
    fn the_tray_is_installed_with_its_autostart() {
        for os in [Os::Linux, Os::Windows] {
            let fx = Fixture::new(os);
            fx.stage_tray(b"the tray");
            apply(&fx.staging, &fx.env()).unwrap();
            assert_eq!(
                std::fs::read(&fx.layout.tray).unwrap(),
                b"the tray",
                "{os:?}"
            );
            let ev = fx.events().join("\n");
            if os == Os::Windows {
                assert!(fx.layout.tray_autostart.is_none());
                assert!(ev.contains("reg.exe add HKLM"), "{ev}");
                assert!(ev.contains("printpapi-tray.exe\" --tray /f"), "{ev}");
            } else {
                let auto = tray_text(fx.layout.tray_autostart.as_ref().unwrap());
                assert!(auto.contains("--tray"), "{auto}");
                assert!(auto.contains("printpapi-tray"), "{auto}");
                let entry = tray_text(fx.layout.app_entry.as_ref().unwrap());
                assert!(
                    entry.contains("Type=Application") && !entry.contains("--tray"),
                    "{entry}"
                );
                assert!(!ev.contains("reg.exe"), "{ev}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_tray_file_is_runnable_for_everybody() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fx = Fixture::new(Os::Linux);
        fx.stage_tray(b"the tray");
        let before = unsafe { libc::umask(0) };
        let result = apply(&fx.staging, &fx.env());
        unsafe { libc::umask(before) };
        result.unwrap();
        assert_eq!(mode(&fx.layout.tray), 0o755);
        assert_eq!(mode(fx.layout.tray_autostart.as_ref().unwrap()), 0o644);
        assert_eq!(mode(fx.layout.app_entry.as_ref().unwrap()), 0o644);
        assert_eq!(mode(&fx.layout.status_dir), 0o755);
    }

    #[test]
    fn an_install_without_a_staged_tray_leaves_the_tray_alone() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        let ev = fx.events().join("\n");
        assert!(!ev.contains("pkill"), "{ev}");
        assert!(!fx.layout.tray.exists());
        assert!(!fx.layout.tray_autostart.as_ref().unwrap().exists());
        // the agent still gets its status folder
        assert!(fx.layout.status_dir.is_dir());
    }

    #[test]
    fn tray_is_stopped_before_it_is_replaced() {
        for os in [Os::Linux, Os::Windows] {
            let fx = Fixture::new(os);
            std::fs::create_dir_all(&fx.layout.dir).unwrap();
            std::fs::write(&fx.layout.tray, b"old tray").unwrap();
            fx.stage_tray(b"new tray");
            let tray = fx.layout.tray.clone();
            let (run, seen) = fx.run_watching(&["pkill", "taskkill"], move || tray_text(&tray));
            let mut env = fx.env();
            env.run = run;
            apply(&fx.staging, &env).unwrap();
            assert_eq!(*seen.borrow(), vec!["old tray".to_string()], "{os:?}");
            assert_eq!(tray_text(&fx.layout.tray), "new tray", "{os:?}");
        }
    }

    #[test]
    fn tray_is_stopped_before_it_is_removed() {
        for os in [Os::Linux, Os::Windows] {
            let fx = Fixture::new(os);
            fx.stage_tray(b"the tray");
            apply(&fx.staging, &fx.env()).unwrap();
            fx.events.borrow_mut().clear();
            let tray = fx.layout.tray.clone();
            let (run, seen) = fx.run_watching(&["pkill", "taskkill"], move || tray_text(&tray));
            let mut env = fx.env();
            env.run = run;
            remove(&env).unwrap();
            assert_eq!(*seen.borrow(), vec!["the tray".to_string()], "{os:?}");
            assert!(
                !fx.layout.tray.exists() && !fx.layout.dir.exists(),
                "{os:?}"
            );
        }
    }

    #[test]
    fn remove_takes_the_tray_the_status_folder_and_the_autostart_with_it() {
        for os in [Os::Linux, Os::Windows, Os::Macos] {
            let fx = Fixture::new(os);
            fx.stage_tray(b"the tray");
            apply(&fx.staging, &fx.env()).unwrap();
            if os == Os::Macos {
                // the faked `ditto` copied nothing: put an app where it would be
                std::fs::create_dir_all(fx.layout.tray.join("Contents")).unwrap();
            }
            std::fs::write(fx.layout.status_dir.join("status.json"), b"{}").unwrap();
            fx.events.borrow_mut().clear();
            remove(&fx.env()).unwrap();
            assert!(!fx.layout.dir.exists(), "{os:?}");
            assert!(!fx.layout.tray.exists(), "{os:?}");
            assert!(!fx.layout.status_dir.exists(), "{os:?}");
            for extra in [&fx.layout.tray_autostart, &fx.layout.app_entry]
                .into_iter()
                .flatten()
            {
                assert!(!extra.exists(), "{os:?}: {extra:?}");
            }
            let ev = fx.events().join("\n");
            assert_eq!(
                ev.contains("reg.exe delete HKLM"),
                os == Os::Windows,
                "{ev}"
            );
        }
    }

    #[test]
    fn a_removal_that_cannot_stop_the_agent_keeps_the_tray_files() {
        let fx = Fixture::new(Os::Linux);
        fx.stage_tray(b"the tray");
        apply(&fx.staging, &fx.env()).unwrap();
        fx.running.set(true);
        assert!(remove(&fx.env()).is_err());
        assert!(fx.layout.tray.is_file());
        assert!(fx.layout.status_dir.is_dir());
        assert!(fx.layout.tray_autostart.as_ref().unwrap().is_file());
    }

    #[test]
    fn the_tray_app_on_macos_is_copied_by_ditto_after_the_old_one_is_stopped() {
        let fx = Fixture::new(Os::Macos);
        std::fs::create_dir_all(fx.layout.tray.join("Contents")).unwrap();
        std::fs::write(fx.layout.tray.join("old"), b"x").unwrap();
        fx.stage_tray(b"plist");
        // a missing quarantine flag is no reason to fail
        fx.failing.borrow_mut().push("xattr -dr".into());
        let tray = fx.layout.tray.clone();
        let (run, seen) = fx.run_watching(&["usr/bin/ditto"], move || {
            tray.join("old").exists().to_string()
        });
        let mut env = fx.env();
        env.run = run;
        apply(&fx.staging, &env).unwrap();
        let ev = fx.events();
        let pos = |needle: &str| ev.iter().position(|e| e.contains(needle)).unwrap();
        let app = fx.layout.tray.display().to_string();
        assert!(pos("pkill") < pos("ditto"), "{ev:?}");
        assert_eq!(
            ev[pos("ditto")],
            format!(
                "run /usr/bin/ditto {} {app}",
                fx.staging.join("tray").display()
            )
        );
        assert!(pos("ditto") < pos("chown -R root:wheel"), "{ev:?}");
        assert!(pos("chown -R root:wheel") < pos("chmod -R go-w"), "{ev:?}");
        // the old app is gone before ditto lays the new one down, so nothing old stays inside
        assert_eq!(*seen.borrow(), vec!["false".to_string()]);
        let plist = tray_text(fx.layout.tray_autostart.as_ref().unwrap());
        assert!(plist.contains("com.printpapi.tray"), "{plist}");
    }

    #[test]
    fn a_tray_of_the_wrong_shape_is_refused_before_anything_happens() {
        // a folder where a file is expected, and a file where the macOS app folder is expected
        for (os, make_dir) in [(Os::Linux, true), (Os::Windows, true), (Os::Macos, false)] {
            let fx = Fixture::new(os);
            let tray = fx.staging.join("tray");
            if make_dir {
                std::fs::create_dir(&tray).unwrap();
            } else {
                std::fs::write(&tray, b"file").unwrap();
            }
            let e = apply(&fx.staging, &fx.env()).unwrap_err();
            assert_eq!(e.code(), "bad_staging", "{os:?}");
            assert!(fx.events().is_empty(), "{os:?}");
            assert!(!fx.layout.dir.exists(), "{os:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_tray_in_staging_is_refused() {
        let fx = Fixture::new(Os::Linux);
        std::os::unix::fs::symlink("/bin/sh", fx.staging.join("tray")).unwrap();
        assert_eq!(
            apply(&fx.staging, &fx.env()).unwrap_err().code(),
            "bad_staging"
        );
        assert!(fx.events().is_empty());
    }

    #[test]
    fn the_status_folder_gets_its_rights_before_the_agent_starts() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        assert!(fx.layout.status_dir.is_dir());
        let ev = fx.events();
        let dir = fx.layout.status_dir.display().to_string();
        let chown = ev
            .iter()
            .position(|e| *e == format!("run chown printpapi {dir}"))
            .unwrap();
        let chmod = ev
            .iter()
            .position(|e| *e == format!("run chmod 755 {dir}"))
            .unwrap();
        let start = ev.iter().position(|e| e.contains("enable --now")).unwrap();
        assert!(chown < chmod && chmod < start, "{ev:?}");
        // and a failing rights command stops the install, not the agent's later silence
        let fx = Fixture::new(Os::Linux);
        fx.failing.borrow_mut().push("chown printpapi".into());
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(fx.events().iter().all(|e| !e.contains("enable --now")));
    }

    #[test]
    fn windows_sets_the_status_folder_rights_with_icacls() {
        let fx = Fixture::new(Os::Windows);
        apply(&fx.staging, &fx.env()).unwrap();
        let ev = fx.events().join("\n");
        let dir = fx.layout.status_dir.display().to_string();
        assert!(
            ev.contains(&format!(
                "icacls.exe {dir} /inheritance:r /setowner *S-1-5-32-544"
            )),
            "{ev}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pre_made_status_dir_is_taken_over_with_our_owner() {
        use std::os::unix::fs::{MetadataExt, chown};
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.status_dir).unwrap();
        std::fs::write(fx.layout.status_dir.join("status.json"), b"planted").unwrap();
        // as root the folder is handed to another user; as anybody else it is already the
        // runner's, which the install trusts, so only the root run (the Docker run) shows the case
        let handed = chown(&fx.layout.status_dir, Some(54321), Some(54321)).is_ok();
        apply(&fx.staging, &fx.env()).unwrap();
        let meta = std::fs::metadata(&fx.layout.status_dir).unwrap();
        if handed {
            assert_eq!(meta.uid(), 0, "the folder of another user must not be kept");
            assert!(!fx.layout.status_dir.join("status.json").exists());
        } else {
            assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_status_dir_check_keeps_its_own_and_trusted_owners() {
        use std::os::unix::fs::MetadataExt;
        let fx = Fixture::new(Os::Linux);
        let dir = &fx.layout.status_dir;
        std::fs::create_dir_all(dir).unwrap();
        let mine = std::fs::metadata(dir).unwrap().uid();
        std::fs::write(dir.join("keep"), b"x").unwrap();
        vet_status_dir(dir, &[mine]).unwrap();
        assert!(dir.join("keep").exists());
        // nobody trusted: it goes, with what is in it
        vet_status_dir(dir, &[]).unwrap();
        assert!(!dir.exists());
        // nothing there is fine
        vet_status_dir(dir, &[]).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_status_dir_is_refused() {
        let fx = Fixture::new(Os::Linux);
        let elsewhere = fx.tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("precious"), b"x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &fx.layout.status_dir).unwrap();
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(e.to_string().contains("symbolic link"), "{e}");
        assert!(elsewhere.join("precious").exists());
        // and the removal does not follow it either
        let fx2 = Fixture::new(Os::Linux);
        apply(&fx2.staging, &fx2.env()).unwrap();
        std::fs::remove_dir_all(&fx2.layout.status_dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &fx2.layout.status_dir).unwrap();
        remove(&fx2.env()).unwrap();
        assert!(elsewhere.join("precious").exists());
    }

    #[test]
    fn a_patch_merges_with_the_installed_ini() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        fx.events.borrow_mut().clear();
        fx.stage_patch(
            r#"{"printers": "Zebra GK420d", "timeout": 45, "headers": [["X-Via", "proxy 100%"]]}"#,
        );
        apply(&fx.staging, &fx.env()).unwrap();
        let ini = std::fs::read_to_string(&fx.layout.ini).unwrap();
        // what the patch names changed, what it does not name stayed
        assert!(ini.contains("api_key = SECRET-KEY"), "{ini}");
        assert!(ini.contains("name = shop-pc"), "{ini}");
        assert!(
            ini.contains("server_url = https://print.example.com/"),
            "{ini}"
        );
        assert!(
            ini.contains("Zebra GK420d") && !ini.contains("Büro Drucker"),
            "{ini}"
        );
        let settings = fx.settings.borrow();
        let last = settings.last().unwrap();
        assert_eq!(last.timeout, 45.0);
        assert_eq!(
            last.headers.get("x-via").map(String::as_str),
            Some("proxy 100%")
        );
        // it is an ordinary install of the merged file: registered with the server first
        assert_eq!(fx.events()[0], "POST /agent/register");
        assert!(
            std::fs::read_dir(&fx.layout.dir)
                .unwrap()
                .all(|e| { !e.unwrap().file_name().to_string_lossy().ends_with(".new") })
        );
    }

    #[test]
    fn a_bad_patch_touches_nothing() {
        let bad_ones = [
            r#"{"headers": [["authorization", "x"]]}"#,
            r#"{"timeout": 5}"#,
            r#"{"printers": "a = socket://nohost"}"#,
            r#"{"unknown": 1}"#,
            "not json",
        ];
        for bad in bad_ones {
            let fx = Fixture::new(Os::Linux);
            apply(&fx.staging, &fx.env()).unwrap();
            let installed = std::fs::read(&fx.layout.ini).unwrap();
            fx.events.borrow_mut().clear();
            fx.stage_patch(bad);
            crate::ERR_LINES.with(|l| l.borrow_mut().clear());
            let e = apply(&fx.staging, &fx.env()).unwrap_err();
            assert_eq!(e.code(), "bad_staging", "{bad}: {e}");
            assert!(!e.to_string().contains("SECRET"), "{bad}: {e}");
            assert!(fx.events().is_empty(), "{bad}: {:?}", fx.events());
            assert_eq!(std::fs::read(&fx.layout.ini).unwrap(), installed, "{bad}");
        }
    }

    #[test]
    fn a_patch_without_an_installed_ini_is_a_system_error() {
        let fx = Fixture::new(Os::Linux);
        fx.stage_patch(r#"{"timeout": 45}"#);
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(fx.events().is_empty());
        assert!(!fx.layout.dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_patch_never_reads_a_symlinked_installed_ini() {
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        let victim = fx.tmp.path().join("victim");
        std::fs::write(&victim, INI).unwrap();
        std::os::unix::fs::symlink(&victim, &fx.layout.ini).unwrap();
        fx.stage_patch(r#"{"timeout": 45}"#);
        let e = apply(&fx.staging, &fx.env()).unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e}");
        assert!(fx.events().is_empty());
    }

    #[test]
    fn a_staged_ini_wins_over_a_staged_patch() {
        let fx = Fixture::new(Os::Linux);
        std::fs::write(fx.staging.join("patch.json"), r#"{"timeout": 45}"#).unwrap();
        apply(&fx.staging, &fx.env()).unwrap();
        let ini = std::fs::read_to_string(&fx.layout.ini).unwrap();
        assert!(!ini.contains("timeout"), "{ini}");
    }

    #[test]
    fn restart_needs_the_marker() {
        let fx = Fixture::new(Os::Linux);
        std::fs::create_dir_all(&fx.layout.dir).unwrap();
        std::fs::write(&fx.layout.binary, b"hand made").unwrap();
        let e = restart(&fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(fx.events().is_empty());
    }

    #[test]
    fn restart_runs_stop_then_start() {
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            let fx = Fixture::new(os);
            apply(&fx.staging, &fx.env()).unwrap();
            fx.events.borrow_mut().clear();
            restart(&fx.env()).unwrap();
            let ev = fx.events();
            let stop = ev
                .iter()
                .position(|e| e.contains("stop") || e.contains("bootout") || e.contains("/end"))
                .unwrap();
            let start = ev
                .iter()
                .position(|e| {
                    e.contains("enable --now") || e.contains("bootstrap") || e.contains("/run")
                })
                .unwrap();
            assert!(stop < start, "{os:?}: {ev:?}");
            // it only restarts: no server, no files
            assert!(
                ev.iter()
                    .all(|e| !e.starts_with("POST") && !e.starts_with("GET")),
                "{ev:?}"
            );
        }
    }

    #[test]
    fn restart_ignores_a_failing_stop_but_not_a_failing_start() {
        let fx = Fixture::new(Os::Linux);
        apply(&fx.staging, &fx.env()).unwrap();
        fx.failing.borrow_mut().push("systemctl stop".into());
        restart(&fx.env()).unwrap();
        fx.failing.borrow_mut().push("enable --now".into());
        let e = restart(&fx.env()).unwrap_err();
        assert_eq!(e.code(), "system");
        assert!(e.to_string().contains("cannot start the agent"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn restart_refuses_a_symlinked_install_folder() {
        let fx = Fixture::new(Os::Linux);
        let elsewhere = fx.tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &fx.layout.dir).unwrap();
        assert!(
            restart(&fx.env())
                .unwrap_err()
                .to_string()
                .contains("symbolic link")
        );
        assert!(fx.events().is_empty());
    }

    #[test]
    fn restart_reports_through_result_json_and_the_exit_code() {
        let fx = Fixture::new(Os::Linux);
        let dir = fx.staging.display().to_string();
        let result = || -> Value {
            serde_json::from_slice(&std::fs::read(fx.staging.join("result.json")).unwrap()).unwrap()
        };
        let args = ["restart".to_string(), dir.clone()];
        // not installed: it says why
        assert_eq!(run_args(&args, Ok(&fx.env())), Some(1));
        assert_eq!(result()["code"], json!("system"));
        apply(&fx.staging, &fx.env()).unwrap();
        assert_eq!(run_args(&args, Ok(&fx.env())), Some(0));
        assert_eq!(result(), json!({"ok": true}));
        // one argument only, like the others
        assert_eq!(run_args(&["restart".to_string()], Ok(&fx.env())), Some(2));
    }
}
