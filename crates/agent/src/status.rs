// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The status file: what the agent says about itself for the tray icon, in `status.json` next to
//! the crash log. It never holds the key or a header value, so every local user may read it. The
//! agent opens no port and takes no command; the icon only reads this file.

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

/// Seconds since the Unix epoch, the time stamps of the status file.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// What the agent says about itself. No field can hold the key or a header value.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub version: String,
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
        if write_atomic(&self.dir, &render(s)).is_ok() {
            self.last = Some((key, now));
        }
    }
}

/// `status.json` in `dir` via a temporary file in the same folder and a rename, so the tray never
/// reads half a file. Readable by everybody (Unix: 0644 whatever the umask).
fn write_atomic(dir: &Path, text: &str) -> std::io::Result<()> {
    let tmp = dir.join(TMP_NAME);
    let result = (|| {
        std::fs::write(&tmp, text)?;
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
