// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The status file: what the agent says about itself for the tray icon, in `status.json` next to
//! the crash log. It never holds the key or a header value, so every local user may read it. The
//! agent opens no port and takes no command; the icon only reads this file.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::config::{Config, Printer, parse_printers};
use crate::install::Os;
use crate::mask_key;
use crate::run::PollOutcome;

const FILE_NAME: &str = "status.json";
const TMP_NAME: &str = "status.json.tmp";
/// Every file the agent writes into its status folder: what removal may delete, and nothing else.
pub const FILES: [&str; 3] = [FILE_NAME, TMP_NAME, crate::log::LOG_NAME];
/// An unchanged status is still written this often, so the tray sees a living agent (it calls a
/// file older than 120 s "not running").
const REFRESH_SECS: f64 = 60.0;

/// Where `%ProgramData%` points on a standard installation, and what is used when Windows cannot
/// say (and off Windows, where `status_dir_in` has no use for it).
const DEFAULT_PROGRAM_DATA: &str = "C:\\ProgramData";

/// The Windows `ProgramData` folder, asked of Windows and not of the environment (see
/// `install::program_files` for why). Its default path is `%ALLUSERSPROFILE%`, which is
/// `%ProgramData%`, which is `%SystemDrive%\ProgramData`.
/// https://learn.microsoft.com/en-us/windows/win32/shell/knownfolderid (FOLDERID_ProgramData)
/// https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath
pub fn program_data() -> PathBuf {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Com::CoTaskMemFree;
        use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath};
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: `raw` is only read when the call succeeded, as a null-terminated string that
        // this call allocated; it is freed once, whether or not the call succeeded (see the docs).
        let found = unsafe {
            let hr = SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, std::ptr::null_mut(), &mut raw);
            let path = (hr >= 0 && !raw.is_null()).then(|| {
                let len = (0..).take_while(|&n| *raw.add(n) != 0).count();
                crate::install::wide_to_path(std::slice::from_raw_parts(raw, len))
            });
            CoTaskMemFree(raw.cast());
            path
        };
        found.unwrap_or_else(|| PathBuf::from(DEFAULT_PROGRAM_DATA))
    }
    #[cfg(not(windows))]
    PathBuf::from(DEFAULT_PROGRAM_DATA)
}

/// The status folder of `os`: `<ProgramData>\printpapi-agent` on Windows; elsewhere the standard place
/// for state that outlives a run (`/Library/Application Support` is the macOS folder for
/// application support files, `/var/lib` the FHS one for variable state data).
/// https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch05s08.html
pub fn status_dir_in(os: Os, program_data: &Path) -> PathBuf {
    match os {
        Os::Windows => {
            let base = program_data.to_string_lossy();
            PathBuf::from(format!(
                "{}\\printpapi-agent",
                base.trim_end_matches(['/', '\\'])
            ))
        }
        Os::Macos => PathBuf::from("/Library/Application Support/printpapi-agent"),
        Os::Linux => PathBuf::from("/var/lib/printpapi-agent"),
    }
}

/// The status folder on this machine.
pub fn status_dir(os: Os) -> PathBuf {
    status_dir_in(os, &program_data())
}

/// Who owns a folder, as far as [`distrust`] cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirOwner {
    /// Unix: the user id.
    Uid(u32),
    /// Windows: the owner as an `S-1-5-...` text.
    Sid(String),
    /// The owner could not be read.
    Unknown,
}

/// What the agent looks at before it writes into its status folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirFacts {
    pub is_dir: bool,
    /// A symbolic link or, on Windows, any reparse point (a junction is one).
    pub is_link: bool,
    pub owner: DirOwner,
}

/// Why the agent must not write its status file and its log into a folder, `None` when it may:
/// the folder exists, is no link and belongs to whoever the install makes it belong to (`uids`:
/// root and, on Linux, the service user; on Windows SYSTEM and Administrators). The agent runs as
/// SYSTEM or root, and where `%ProgramData%` lets every user create a folder, a user could have
/// made this one first, as a junction, and have the agent's writes land where it points. Pure.
pub fn distrust(facts: Option<&DirFacts>, uids: &[u32]) -> Option<&'static str> {
    let Some(f) = facts else {
        return Some("is missing");
    };
    if f.is_link {
        return Some("is a link");
    }
    if !f.is_dir {
        return Some("is not a folder");
    }
    let trusted = match &f.owner {
        DirOwner::Uid(u) => uids.contains(u),
        DirOwner::Sid(sid) => crate::apply::trusted_windows_owner(sid),
        DirOwner::Unknown => false,
    };
    (!trusted).then_some("is owned by somebody else")
}

/// The facts about `dir`, without following a link; `None` when it cannot be looked at.
pub fn inspect_dir(dir: &Path) -> Option<DirFacts> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    Some(DirFacts {
        is_dir: meta.is_dir(),
        is_link: crate::apply::is_link(&meta),
        owner: owner_of(dir, &meta),
    })
}

#[cfg(unix)]
fn owner_of(_: &Path, meta: &std::fs::Metadata) -> DirOwner {
    use std::os::unix::fs::MetadataExt;
    DirOwner::Uid(meta.uid())
}

#[cfg(windows)]
fn owner_of(dir: &Path, _: &std::fs::Metadata) -> DirOwner {
    crate::apply::dir_owner_sid(dir).map_or(DirOwner::Unknown, DirOwner::Sid)
}

/// The user ids that may own the status folder of `os`: root, and on Linux the service user. Empty
/// on Windows, where owners are SIDs.
#[cfg(unix)]
pub fn trusted_uids(os: Os) -> Vec<u32> {
    let mut uids = vec![0];
    if os == Os::Linux
        && let Ok(name) = std::ffi::CString::new(crate::install::SERVICE_USER)
    {
        // SAFETY: `name` is a valid C string; the returned record is read at once, before any
        // other call could reuse it
        let entry = unsafe { libc::getpwnam(name.as_ptr()) };
        if !entry.is_null() {
            uids.push(unsafe { (*entry).pw_uid });
        }
    }
    uids
}

#[cfg(not(unix))]
pub fn trusted_uids(_: Os) -> Vec<u32> {
    Vec::new()
}

/// May the agent write its status file and its log into `dir`? `Err` says why not.
// ponytail: the folder can still be swapped between this look and the first write. The folder is
// in a place only administrators (and the service user) can change once the install has set its
// rights, so the gap is for somebody who already is one.
pub fn usable_status_dir(dir: &Path, os: Os) -> Result<(), &'static str> {
    match distrust(inspect_dir(dir).as_ref(), &trusted_uids(os)) {
        None => Ok(()),
        Some(why) => Err(why),
    }
}

/// Seconds since the Unix epoch, the time stamps of the status file.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

static STARTED_AT: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

/// When this process started, as far as the status file is concerned: the first time anybody asks
/// (the agent asks right at its start). A restarted agent is a new process with a later value, which
/// is how a caller tells "restarted" from "the old process wrote again".
pub fn process_started_at() -> f64 {
    *STARTED_AT.get_or_init(unix_now)
}

/// What the agent says about itself. No field can hold the key or a header value.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub version: String,
    /// Seconds since the epoch at which this process started.
    pub started_at: f64,
    pub name: String,
    pub server: String,
    /// `"online"` or `"offline"`. "Not running" is the tray's reading of a missing or old file.
    pub state: &'static str,
    pub last_contact: Option<f64>,
    /// `(job id, printed ok, when)`.
    pub last_job: Option<(i64, bool, f64)>,
    pub printers: Vec<Printer>,
    pub timeout: f64,
    pub header_names: Vec<String>,
    pub error: Option<String>,
}

impl Status {
    /// The settings part from agent.ini, before any contact with the server. A part that cannot
    /// be read stays at its default: a bad agent.ini stops start-up with its own message.
    pub fn from_config(cfg: &Config) -> Status {
        let key = cfg.get("api_key").ok().flatten().unwrap_or_default();
        let settings = cfg.http_settings().unwrap_or_default();
        let printers = cfg
            .get("printers")
            .ok()
            .flatten()
            .and_then(|spec| parse_printers(&spec).ok())
            .unwrap_or_default()
            .into_iter()
            // an entry can pull the key in with `%(api_key)s`
            .map(|p| {
                Printer::new(
                    &mask_key(p.name, &key),
                    p.can_pdf,
                    &mask_key(p.target, &key),
                )
            })
            .collect();
        let name = cfg
            .get("name")
            .ok()
            .flatten()
            .unwrap_or_else(|| "agent".into());
        let server = cfg.get("server_url").ok().flatten().unwrap_or_default();
        Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: process_started_at(),
            name: mask_key(name, &key),
            server: mask_key(without_credentials(server.trim_end_matches('/')), &key),
            state: "offline",
            last_contact: None,
            last_job: None,
            printers,
            timeout: settings.timeout,
            header_names: settings.headers.into_keys().collect(),
            error: None,
        }
    }
}

/// `url` without `user:password@` in its authority: the file is readable by every local user.
fn without_credentials(url: &str) -> String {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return url.to_string();
    };
    let rest = &url[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{}{}", &url[..start], &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// The JSON text of the status file.
pub fn render(s: &Status) -> String {
    json!({
        "version": s.version,
        "started_at": s.started_at,
        "name": s.name,
        "server": s.server,
        "state": s.state,
        "last_contact": s.last_contact,
        "last_job": s.last_job.map(|(id, ok, at)| json!({"id": id, "ok": ok, "at": at})),
        "printers": s.printers.iter()
            .map(|p| json!({"name": p.name, "pdf": p.can_pdf, "target": p.target}))
            .collect::<Vec<Value>>(),
        "timeout": s.timeout,
        "header_names": s.header_names,
        "error": s.error,
    })
    .to_string()
}

/// Writes the status file when it matters. Write errors are ignored: a missing or locked folder
/// must never stop the agent from printing.
pub struct StatusWriter {
    dir: PathBuf,
    /// What was written last (without `last_contact`) and when.
    last: Option<(String, f64)>,
}

impl StatusWriter {
    pub fn new(dir: PathBuf) -> StatusWriter {
        StatusWriter { dir, last: None }
    }

    /// Write `s` if its content changed (not counting `last_contact`) or the last write is 60 s
    /// old.
    pub fn update(&mut self, s: &Status, now: f64) {
        let key = render(&Status {
            last_contact: None,
            ..s.clone()
        });
        if let Some((last_key, at)) = &self.last
            && *last_key == key
            && now - at < REFRESH_SECS
        {
            return;
        }
        self.write_now(s, now, key);
    }

    /// Write `s` whatever was written last. `key` is its `render` without `last_contact`.
    fn write_now(&mut self, s: &Status, now: f64, key: String) {
        if write_atomic(&self.dir, &render(s)).is_ok() {
            self.last = Some((key, now));
        }
    }

    /// Write `s` now, changed or not.
    pub fn refresh(&mut self, s: &Status, now: f64) {
        let key = render(&Status {
            last_contact: None,
            ..s.clone()
        });
        self.write_now(s, now, key);
    }
}

/// `status.json` in `dir` via a temporary file in the same folder and a rename, so the tray never
/// reads half a file. Readable by everybody (Unix: 0644 whatever the umask). The temporary file is
/// deleted first and created with `create_new`, which refuses to open anything that is there: a
/// file or link that somebody placed at that name is never written through (the agent can run as
/// SYSTEM).
fn write_atomic(dir: &Path, text: &str) -> std::io::Result<()> {
    let tmp = dir.join(TMP_NAME);
    let result = (|| {
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::rename(&tmp, dir.join(FILE_NAME))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The agent's view of itself over time: turns what happens into a [`Status`] and hands it to the
/// writer. Without a writer (a hand-set-up agent) it does nothing.
pub struct Reporter {
    status: Status,
    writer: Option<StatusWriter>,
    /// To keep it out of error texts.
    key: String,
}

impl Reporter {
    pub fn new(status: Status, writer: Option<StatusWriter>, key: String) -> Reporter {
        Reporter {
            status,
            writer,
            key,
        }
    }

    fn flush(&mut self, now: f64) {
        if let Some(w) = &mut self.writer {
            w.update(&self.status, now);
        }
    }

    /// The server took the registration.
    pub fn registered(&mut self, now: f64) {
        self.status.state = "online";
        self.status.last_contact = Some(now);
        self.status.error = None;
        self.flush(now);
    }

    /// The agent is alive: write the last state again whatever the writer last did, so the file
    /// stays young while the main thread is in a long job or a hanging call. Called from the thread
    /// of [`spawn_heartbeat`]. Nothing about the state changes, `last_contact` included.
    pub fn heartbeat(&mut self, now: f64) {
        if let Some(w) = &mut self.writer {
            w.refresh(&self.status, now);
        }
    }

    /// Time has passed with nothing to report (the register back-off): lets the writer refresh
    /// the file, so the tray does not read a waiting agent as a dead one.
    pub fn tick(&mut self, now: f64) {
        self.flush(now);
    }

    /// A registration try failed; `msg` is what the log gets.
    pub fn register_failed(&mut self, msg: &str, now: f64) {
        self.status.state = "offline";
        self.status.error = Some(mask_key(msg.to_string(), &self.key));
        self.flush(now);
    }

    /// One poll came to `outcome`.
    pub fn polled(&mut self, outcome: &PollOutcome, now: f64) {
        match outcome {
            Err(e) => {
                self.status.state = "offline";
                self.status.error = Some(mask_key(e.clone(), &self.key));
            }
            Ok(done) => {
                self.status.state = "online";
                self.status.last_contact = Some(now);
                self.status.error = None;
                if let Some(d) = done {
                    self.status.last_job = Some((d.id, d.ok, now));
                }
            }
        }
        self.flush(now);
    }
}

/// How often the heartbeat thread rewrites the status file. The tray calls a file older than 120 s
/// "not running", and the agent's main thread can be busy for longer than that: printing a big
/// job, or waiting on a connection that hangs.
pub const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);

/// Rewrite the status file every `every` from a thread of its own, with the state the reporter
/// holds at that moment. The reporter is locked only for the write, and the thread holds it by a
/// weak reference: it ends when the reporter is dropped.
pub fn spawn_heartbeat(
    reporter: &std::sync::Arc<std::sync::Mutex<Reporter>>,
    every: std::time::Duration,
    now: fn() -> f64,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let weak = std::sync::Arc::downgrade(reporter);
    std::thread::Builder::new()
        .name("status-heartbeat".into())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                let Some(reporter) = weak.upgrade() else {
                    return;
                };
                // a panic elsewhere must not stop the heartbeat
                reporter
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .heartbeat(now());
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::run::JobDone;
    use serde_json::Value;

    const KEY: &str = "SECRET-KEY-123";
    const HEADER_VALUE: &str = "HEADER-SECRET";

    fn config() -> Config {
        Config::from_text(&format!(
            "[agent]\nserver_url = https://print.example.com/\napi_key = {KEY}\nname = PACKTISCH-1\n\
             printers = Zebra GK420d; Brother | pdf = Brother QL\ntimeout = 45\n\
             [headers]\nCF-Access-Client-Id = {HEADER_VALUE}\n"
        ))
        .unwrap()
    }

    fn reporter(dir: &std::path::Path) -> Reporter {
        Reporter::new(
            Status::from_config(&config()),
            Some(StatusWriter::new(dir.to_path_buf())),
            KEY.to_string(),
        )
    }

    fn read(dir: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(dir.join("status.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn render_never_contains_the_key_or_header_values() {
        let mut s = Status::from_config(&config());
        s.error = Some("boom".into());
        let text = render(&s);
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains(HEADER_VALUE), "{text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        // the header name is no secret, and is what the settings window lists
        assert_eq!(
            v["header_names"],
            serde_json::json!(["cf-access-client-id"])
        );
    }

    #[test]
    fn render_has_the_shape_the_tray_reads() {
        let mut s = Status::from_config(&config());
        s.state = "online";
        s.last_contact = Some(1794048800.0);
        s.last_job = Some((4711, true, 1794048700.0));
        let v: Value = serde_json::from_str(&render(&s)).unwrap();
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["name"], "PACKTISCH-1");
        assert_eq!(v["server"], "https://print.example.com");
        assert_eq!(v["state"], "online");
        assert_eq!(v["last_contact"], 1794048800.0);
        assert_eq!(
            v["last_job"],
            serde_json::json!({"id": 4711, "ok": true, "at": 1794048700.0})
        );
        assert_eq!(
            v["printers"],
            serde_json::json!([
                {"name": "Zebra GK420d", "pdf": false, "target": "Zebra GK420d"},
                {"name": "Brother", "pdf": true, "target": "Brother QL"}
            ])
        );
        assert_eq!(v["timeout"], 45.0);
        assert_eq!(v["error"], Value::Null);
        let fresh: Value = serde_json::from_str(&render(&Status::from_config(&config()))).unwrap();
        assert_eq!(fresh["last_contact"], Value::Null);
        assert_eq!(fresh["last_job"], Value::Null);
    }

    #[test]
    fn a_printer_entry_that_pulls_the_key_in_does_not_leak_it() {
        let cfg = Config::from_text(&format!(
            "[agent]\nserver_url = https://x\napi_key = {KEY}\nprinters = n = socket://%(api_key)s:9100\n"
        ))
        .unwrap();
        assert!(!render(&Status::from_config(&cfg)).contains(KEY));
    }

    #[test]
    fn the_status_folder_per_system() {
        use crate::install::Os;
        let pd = std::path::Path::new("C:\\ProgramData");
        assert_eq!(
            status_dir_in(Os::Windows, pd),
            std::path::PathBuf::from("C:\\ProgramData\\printpapi-agent")
        );
        assert_eq!(
            status_dir_in(Os::Macos, pd),
            std::path::PathBuf::from("/Library/Application Support/printpapi-agent")
        );
        assert_eq!(
            status_dir_in(Os::Linux, pd),
            std::path::PathBuf::from("/var/lib/printpapi-agent")
        );
    }

    #[test]
    fn writer_skips_unchanged_within_60_s() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = StatusWriter::new(dir.path().to_path_buf());
        let mut s = Status::from_config(&config());
        w.update(&s, 1000.0);
        let first = std::fs::read(dir.path().join("status.json")).unwrap();
        // only last_contact moves: not a change
        s.last_contact = Some(1030.0);
        w.update(&s, 1030.0);
        assert_eq!(
            std::fs::read(dir.path().join("status.json")).unwrap(),
            first
        );
        w.update(&s, 1059.9);
        assert_eq!(
            std::fs::read(dir.path().join("status.json")).unwrap(),
            first
        );
    }

    #[test]
    fn writer_rewrites_after_60_s() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = StatusWriter::new(dir.path().to_path_buf());
        let mut s = Status::from_config(&config());
        w.update(&s, 1000.0);
        s.last_contact = Some(1060.0);
        w.update(&s, 1060.0);
        assert_eq!(read(dir.path())["last_contact"], 1060.0);
    }

    #[test]
    fn writer_rewrites_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = StatusWriter::new(dir.path().to_path_buf());
        let mut s = Status::from_config(&config());
        w.update(&s, 1000.0);
        s.state = "online";
        w.update(&s, 1001.0);
        assert_eq!(read(dir.path())["state"], "online");
    }

    #[test]
    fn writer_leaves_no_temporary_file_and_ignores_a_missing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = StatusWriter::new(dir.path().to_path_buf());
        w.update(&Status::from_config(&config()), 1.0);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("status.json")]);
        let mut gone = StatusWriter::new(dir.path().join("does-not-exist"));
        gone.update(&Status::from_config(&config()), 1.0); // must not panic
    }

    #[test]
    fn a_file_planted_as_the_temporary_file_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        // a hard link is the same file under two names: writing into it would overwrite the victim
        std::fs::hard_link(&victim, dir.path().join("status.json.tmp")).unwrap();
        let mut w = StatusWriter::new(dir.path().to_path_buf());
        w.update(&Status::from_config(&config()), 1.0);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert_eq!(read(dir.path())["name"], "PACKTISCH-1");
        assert!(!dir.path().join("status.json.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn status_file_is_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A leftover temporary file with a closed mode: writing into it keeps that mode, so only
        // the explicit chmod makes the result readable (a fresh file would get 0644 from the
        // usual umask anyway).
        let tmp = dir.path().join("status.json.tmp");
        std::fs::write(&tmp, "old").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut w = StatusWriter::new(dir.path().to_path_buf());
        w.update(&Status::from_config(&config()), 1.0);
        let mode = std::fs::metadata(dir.path().join("status.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    fn facts(is_dir: bool, is_link: bool, owner: DirOwner) -> Option<DirFacts> {
        Some(DirFacts {
            is_dir,
            is_link,
            owner,
        })
    }

    #[test]
    fn the_status_folder_is_trusted_only_when_the_system_owns_a_real_folder() {
        let uids = [0, 996];
        let sid = |s: &str| DirOwner::Sid(s.to_string());
        // root, or the service user; SYSTEM or Administrators
        for owner in [
            DirOwner::Uid(0),
            DirOwner::Uid(996),
            sid("S-1-5-18"),
            sid("S-1-5-32-544"),
        ] {
            let f = facts(true, false, owner.clone());
            assert_eq!(distrust(f.as_ref(), &uids), None, "{owner:?}");
        }
        // any other owner: a user made the folder first
        for owner in [
            DirOwner::Uid(1000),
            sid("S-1-5-21-1-2-3-1001"),
            sid("S-1-5-32-545"),
            DirOwner::Unknown,
        ] {
            let f = facts(true, false, owner.clone());
            assert_eq!(
                distrust(f.as_ref(), &uids),
                Some("is owned by somebody else"),
                "{owner:?}"
            );
        }
        // a link or a reparse point is never followed, whoever owns it
        assert_eq!(
            distrust(facts(true, true, DirOwner::Uid(0)).as_ref(), &uids),
            Some("is a link")
        );
        assert_eq!(
            distrust(facts(false, false, DirOwner::Uid(0)).as_ref(), &uids),
            Some("is not a folder")
        );
        // the agent never makes the folder: missing is not usable
        assert_eq!(distrust(None, &uids), Some("is missing"));
    }

    #[test]
    fn inspect_reads_what_the_decision_needs() {
        let dir = tempfile::tempdir().unwrap();
        let f = inspect_dir(dir.path()).unwrap();
        assert!(f.is_dir && !f.is_link);
        #[cfg(unix)]
        assert_eq!(f.owner, DirOwner::Uid(unsafe { libc::geteuid() }));
        #[cfg(windows)]
        assert!(
            matches!(&f.owner, DirOwner::Sid(s) if s.starts_with("S-1-5-")),
            "{f:?}"
        );
        assert!(inspect_dir(&dir.path().join("gone")).is_none());
        // a link (a junction on Windows) is reported as one, not followed
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        #[cfg(windows)]
        {
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(dir.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }
        assert!(inspect_dir(&link).unwrap().is_link);
    }

    #[test]
    fn a_failed_poll_is_offline_with_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = reporter(dir.path());
        r.polled(&Ok(None), 100.0);
        assert_eq!(read(dir.path())["state"], "online");
        r.polled(&Err(format!("connection refused ({KEY})")), 102.0);
        let v = read(dir.path());
        assert_eq!(v["state"], "offline");
        assert_eq!(v["error"], "connection refused (***)");
        // the last good contact stays
        assert_eq!(v["last_contact"], 100.0);
        r.polled(&Ok(None), 104.0);
        let v = read(dir.path());
        assert_eq!(v["state"], "online");
        assert!(v["error"].is_null());
        assert_eq!(v["last_contact"], 104.0);
    }

    #[test]
    fn a_handled_job_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = reporter(dir.path());
        r.polled(
            &Ok(Some(JobDone {
                id: 4711,
                ok: false,
            })),
            200.0,
        );
        let v = read(dir.path());
        assert_eq!(v["state"], "online");
        assert_eq!(
            v["last_job"],
            serde_json::json!({"id": 4711, "ok": false, "at": 200.0})
        );
        // a later quiet poll keeps the last job
        r.polled(&Ok(None), 300.0);
        assert_eq!(read(dir.path())["last_job"]["id"], 4711);
    }

    #[test]
    fn registering_is_reported_too() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = reporter(dir.path());
        r.register_failed(&format!("register failed: {KEY}"), 10.0);
        let v = read(dir.path());
        assert_eq!(v["state"], "offline");
        assert_eq!(v["error"], "register failed: ***");
        r.registered(20.0);
        let v = read(dir.path());
        assert_eq!(v["state"], "online");
        assert_eq!(v["last_contact"], 20.0);
        assert!(v["error"].is_null());
    }

    #[test]
    fn without_a_writer_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        // a reporter without a writer next to an empty folder: nothing may appear in it
        let mut r = Reporter::new(Status::from_config(&config()), None, KEY.to_string());
        r.registered(1.0);
        r.register_failed("x", 2.0);
        r.polled(&Ok(None), 3.0);
        r.tick(4.0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_status_says_when_the_process_started() {
        let a = Status::from_config(&config());
        let b = Status::from_config(&config());
        // one value for the whole process, near now; a restarted agent gets a later one
        assert_eq!(a.started_at, b.started_at);
        assert!(a.started_at > 1.7e9 && a.started_at <= unix_now());
        let v: Value = serde_json::from_str(&render(&a)).unwrap();
        assert_eq!(v["started_at"], a.started_at);
    }

    #[test]
    fn a_heartbeat_rewrites_the_last_state_though_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = reporter(dir.path());
        r.polled(&Ok(None), 100.0);
        std::fs::remove_file(dir.path().join("status.json")).unwrap();
        // 5 s later: a tick would not write (unchanged, under the refresh time), a heartbeat does
        r.tick(105.0);
        assert!(!dir.path().join("status.json").exists());
        r.heartbeat(105.0);
        let v = read(dir.path());
        assert_eq!(v["state"], "online");
        // the last state, not a made-up contact
        assert_eq!(v["last_contact"], 100.0);
        // and it counts as the last write
        std::fs::remove_file(dir.path().join("status.json")).unwrap();
        r.tick(110.0);
        assert!(!dir.path().join("status.json").exists());
    }

    #[test]
    fn a_reporter_without_a_writer_has_no_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Reporter::new(Status::from_config(&config()), None, KEY.to_string());
        r.heartbeat(1.0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_heartbeat_thread_refreshes_the_file_while_the_main_thread_is_busy() {
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(reporter(dir.path())));
        shared.lock().unwrap().polled(&Ok(None), unix_now());
        let status = dir.path().join("status.json");
        let beat = spawn_heartbeat(&shared, Duration::from_millis(10), unix_now).unwrap();
        // a long job or a hanging call: the main thread does nothing with the reporter, and
        // the file is written again anyway, again and again
        for round in 0..3 {
            std::fs::remove_file(&status).unwrap();
            let end = Instant::now() + Duration::from_secs(5);
            while !status.exists() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(status.exists(), "no refresh in round {round}");
        }
        assert_eq!(read(dir.path())["state"], "online");
        // the thread ends with the reporter instead of living on
        drop(shared);
        beat.join().unwrap();
    }

    #[test]
    fn a_tick_refreshes_an_unchanged_status_after_60_s() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = reporter(dir.path());
        r.register_failed("register failed", 0.0);
        std::fs::remove_file(dir.path().join("status.json")).unwrap();
        r.tick(30.0);
        assert!(!dir.path().join("status.json").exists());
        r.tick(60.0);
        let v = read(dir.path());
        assert_eq!(v["state"], "offline");
        assert_eq!(v["error"], "register failed");
    }

    #[test]
    fn credentials_in_the_server_address_never_reach_the_file() {
        let cases = [
            (
                "https://user:pass@print.example.com/",
                "https://print.example.com",
            ),
            (
                "https://tok@print.example.com:8443/base",
                "https://print.example.com:8443/base",
            ),
            // an @ after the authority belongs to the path or query
            (
                "https://print.example.com/a@b",
                "https://print.example.com/a@b",
            ),
            (
                "https://print.example.com/?u=a@b",
                "https://print.example.com/?u=a@b",
            ),
            ("print.example.com", "print.example.com"),
        ];
        for (given, shown) in cases {
            let cfg = Config::from_text(&format!(
                "[agent]
server_url = {given}
api_key = {KEY}
printers = p
"
            ))
            .unwrap();
            assert_eq!(Status::from_config(&cfg).server, shown, "{given}");
        }
    }
}
