// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The three subcommands that run with administrator rights: `install <dir>`, `uninstall <dir>`
//! and `read-config <dir>`. They get one argument, the path of a private staging folder, and treat
//! everything in it as untrusted because the user who started the setup can write there.
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
use crate::config::{Config, HttpSettings, Printer, decode_ini, parse_printers};
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
                "The server refused this key. Either the key was revoked, or this device name                  belongs to a different key. Check the key on the Keys page of your dashboard,                  or pick another name.",
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

fn read_staged(staging: &Path, sumatra_sha: &str) -> Result<Staged, ApplyError> {
    check_staging(staging)?;
    let bytes = std::fs::read(staging.join("agent.ini"))
        .map_err(|e| bad(format!("agent.ini cannot be read: {e}")))?;
    let text = decode_ini(&bytes);
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
    Ok(Staged {
        text,
        base,
        key,
        name,
        settings,
        printers,
        sumatra,
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

/// Put a copy of `from` at `to` as a new file: written next to it, given its rights (the service
/// user has to be able to run it whatever the rights of the source were), then renamed over `to`. A
/// new inode matters on macOS, where the kernel caches the code signature per vnode and kills a
/// binary that was overwritten in place.
///
/// The bytes are written, not `fs::copy`'d: as root, macOS's copy (`fclonefileat`, `fcopyfile`)
/// keeps the source's owner, which is the user who started the setup. The LaunchDaemon runs as
/// root, so that user could then rewrite what root runs. A file this process creates belongs to
/// this process (root, elevated).
fn install_binary(from: &Path, to: &Path) -> std::io::Result<()> {
    let tmp = to.with_file_name(".printpapi-agent.new");
    let result = std::fs::read(from)
        .and_then(|bytes| {
            // a leftover of a run that died keeps its owner when written into again
            match std::fs::remove_file(&tmp) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            std::fs::write(&tmp, bytes)
        })
        .and_then(|()| set_mode(&tmp, 0o755))
        .and_then(|()| std::fs::rename(&tmp, to));
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

/// Install. Returns `last_seen_at` of this device as read right after registering, which the
/// window compares against to know the agent really started.
pub fn apply(staging: &Path, env: &Env) -> Result<f64, ApplyError> {
    apply_with(staging, env, SUMATRA_EXE_SHA256)
}

fn apply_with(staging: &Path, env: &Env, sumatra_sha: &str) -> Result<f64, ApplyError> {
    let staged = read_staged(staging, sumatra_sha)?;
    let l = &env.layout;

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
    // there is nothing to stop on a first install
    for c in install::stop_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }

    std::fs::create_dir_all(&l.dir).map_err(|e| system("cannot create the install folder", e))?;
    // the service user has to get into the folder whatever the umask of the caller is
    set_mode(&l.dir, 0o755).map_err(|e| system("cannot set the install folder's rights", e))?;
    retry(FILE_ATTEMPTS, env.sleep, &mut || {
        install_binary(&env.self_exe, &l.binary)
    })
    .map_err(|e| system("cannot copy the agent", e))?;
    if let Some(exe) = &staged.sumatra {
        retry(FILE_ATTEMPTS, env.sleep, &mut || {
            std::fs::write(&l.sumatra, exe)
        })
        .map_err(|e| system("cannot copy SumatraPDF", e))?;
    }
    // The key goes in only after the file has its restrictive rights.
    std::fs::File::create(&l.ini).map_err(|e| system("cannot create agent.ini", e))?;
    for c in install::ini_acl_cmds(env.os, l, &env.sys) {
        (env.run)(&c).map_err(|e| system("cannot restrict agent.ini", e))?;
    }
    std::fs::write(&l.ini, staged.text.as_bytes())
        .map_err(|e| system("cannot write agent.ini", e))?;
    // The marker before the autostart file: a run that dies in between is found as ours next time.
    std::fs::write(&l.marker, b"").map_err(|e| system("cannot write the marker", e))?;
    if let Some(parent) = l.autostart.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| system("cannot create the autostart folder", e))?;
    }
    std::fs::write(&l.autostart, install::autostart_file(env.os, l))
        .map_err(|e| system("cannot write the autostart file", e))?;
    // The old agent is stopped by now. Its long-poll bumped last_seen_at until then, so the value
    // the window compares against is read here, not before. A failing read keeps the first one.
    let baseline = last_seen(&*http, &staged, computer_id).unwrap_or(first_seen);
    for (i, c) in install::start_cmds(env.os, l, &env.sys).iter().enumerate() {
        // the first macOS command removes the quarantine attribute, which a local file may lack
        let optional = env.os == Os::Macos && i == 0;
        if let Err(e) = (env.run)(c)
            && !optional
        {
            return Err(system("cannot start the agent", e));
        }
    }
    Ok(baseline)
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
    if !l.marker.is_file() {
        return Err(ApplyError::System(
            "This agent was not installed by the setup, so the setup leaves it alone.".into(),
        ));
    }
    for c in install::stop_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    // ponytail: a failing delete (task or unit already gone from an earlier half run) is not told
    // apart from a stuck one. The folder goes next, so a leftover entry has nothing to start. A
    // stuck one stays registered with nothing behind it, and `install::detect` then reports it as
    // foreign until somebody removes it by hand.
    for c in install::remove_cmds(env.os, l, &env.sys) {
        let _ = (env.run)(&c);
    }
    match std::fs::remove_file(&l.autostart) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(system("cannot delete the autostart file", e)),
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
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
    let os = Os::host();
    let http_for = |s: &HttpSettings| -> Box<dyn Http> { Box::new(Client::new(s)) };
    let sleep = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));
    let env = std::env::current_exe()
        .map(|self_exe| Env {
            os,
            layout: install::layout(os, Path::new(&program_files)),
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
        Some(c @ ("install" | "uninstall" | "read-config")) => c,
        _ => return None,
    };
    let [_, dir] = args else {
        crate::err_line("usage: printpapi-agent install|uninstall|read-config <staging folder>");
        return Some(2);
    };
    let staging = Path::new(dir);
    let outcome = env.and_then(|env| match command {
        "install" => apply(staging, env).map(|b| json!({"ok": true, "baseline_last_seen": b})),
        "uninstall" => remove(env).map(|()| json!({"ok": true})),
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
            let run: Runner = {
                let (events, failing) = (events.clone(), failing.clone());
                let layout = layout.clone();
                Rc::new(move |c| {
                    let line = c.argv.join(" ");
                    let mut note = format!("run {line}");
                    // what the disk looks like at the moment this command runs
                    if line.contains("chmod") || line.contains("icacls") {
                        let len = std::fs::metadata(&layout.ini).map(|m| m.len());
                        note.push_str(&format!(" [ini len {len:?}]"));
                    }
                    if line.contains("enable") || line.contains("/create") {
                        note.push_str(&format!(
                            " [marker {} autostart {}]",
                            layout.marker.exists(),
                            layout.autostart.exists()
                        ));
                    }
                    events.borrow_mut().push(note);
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
        let ini = fx.layout.ini.display();
        assert_eq!(
            fx.events(),
            vec![
                "POST /agent/register".to_string(),
                "GET /computers".to_string(),
                "run id -u printpapi".to_string(),
                "run systemctl stop printpapi-agent".to_string(),
                // the ini is empty while the ACL is set, the key comes after
                format!("run chown root:printpapi {ini}"),
                format!("run chmod 640 {ini} [ini len Ok(0)]"),
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
        assert_eq!(std::fs::read(&fx.layout.ini).unwrap(), b"");
        assert!(!fx.layout.marker.exists());
    }

    #[test]
    fn a_failing_stop_is_ignored() {
        let fx = Fixture::new(Os::Linux);
        fx.failing.borrow_mut().push("systemctl stop".into());
        apply(&fx.staging, &fx.env()).unwrap();
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
                "run systemctl stop printpapi-agent",
                "run systemctl disable printpapi-agent"
            ]
        );
        assert!(!fx.layout.dir.exists());
        assert!(!fx.layout.autostart.exists());
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
}
