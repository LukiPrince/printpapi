// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! What the settings window needs besides the install commands: the agent's state read from its
//! status file, the tail of its log, and the tray icon program that a setup run puts next to the
//! agent. Pure decisions with tests; `commands.rs` joins them to Tauri.
//!
//! Everything here is readable by every local user, so it never touches a key or a header value.

use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use printpapi_agent::config::Printer;
use printpapi_agent::install::{Layout, Os};
use printpapi_agent::proc::{Cmd, Runner};
use serde::{Deserialize, Serialize};

use crate::commands::{PrinterChoice, printer_to_choice};
use crate::tray::{self, TrayState};

// ---- the state of the agent

/// The words of `AgentStatus.state` in `web/setup/api.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Online,
    Offline,
    NotRunning,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastJob {
    id: i64,
    ok: bool,
    at: f64,
}

/// What the window shows about the agent (the contract is `AgentStatus` in `web/setup/api.ts`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    state: AgentState,
    name: String,
    server: String,
    version: String,
    last_contact: Option<f64>,
    last_job: Option<LastJob>,
    printers: Vec<PrinterChoice>,
    timeout: f64,
    header_names: Vec<String>,
    error: Option<String>,
}

impl AgentStatus {
    fn not_running() -> AgentStatus {
        AgentStatus {
            state: AgentState::NotRunning,
            name: String::new(),
            server: String::new(),
            version: String::new(),
            last_contact: None,
            last_job: None,
            printers: Vec::new(),
            timeout: 0.0,
            header_names: Vec::new(),
            error: None,
        }
    }
}

/// The status file as the agent writes it (`render` in `crates/agent/src/status.rs`). One field of
/// the wrong type fails the whole parse: the window then says "not running" and shows nothing.
#[derive(Deserialize)]
struct StatusFile {
    state: String,
    version: String,
    name: String,
    server: String,
    last_contact: Option<f64>,
    last_job: Option<JobFile>,
    printers: Vec<PrinterFile>,
    timeout: f64,
    header_names: Vec<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct JobFile {
    id: i64,
    ok: bool,
    at: f64,
}

#[derive(Deserialize)]
struct PrinterFile {
    name: String,
    pdf: bool,
    target: String,
}

/// The state with the same rules as the tray icon (`tray::read_state`: a missing file or a broken
/// one is "not running", an `online` one without a recent contact is offline), and the details of
/// the file where it can be read.
pub fn parse_status(text: Option<&str>, now: f64) -> AgentStatus {
    let state = match tray::read_state(text, now).0 {
        TrayState::Online => AgentState::Online,
        TrayState::Offline => AgentState::Offline,
        TrayState::NotRunning => AgentState::NotRunning,
    };
    let Some(file) = text.and_then(|t| serde_json::from_str::<StatusFile>(t).ok()) else {
        return AgentStatus::not_running();
    };
    // an agent that says "online" but has not been in touch for a while (see `tray::read_state`)
    let silence = (file.state == "online" && state == AgentState::Offline)
        .then_some(file.last_contact)
        .flatten()
        .map(|at| {
            format!(
                "No contact with the server for {}.",
                tray::quiet_for(now - at)
            )
        });
    AgentStatus {
        state,
        name: file.name,
        server: file.server,
        version: file.version,
        last_contact: file.last_contact,
        last_job: file.last_job.map(|j| LastJob {
            id: j.id,
            ok: j.ok,
            at: j.at,
        }),
        printers: file
            .printers
            .iter()
            .map(|p| printer_to_choice(&Printer::new(&p.name, p.pdf, &p.target)))
            .collect(),
        timeout: file.timeout,
        header_names: file.header_names,
        error: file.error.or(silence),
    }
}

pub fn unix_secs(now: SystemTime) -> f64 {
    now.duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// `parse_status` of the file at `path`, which counts as missing when it is older than the tray
/// icon allows (`tray::load_status`).
pub fn status_from_file(path: &Path, now: SystemTime) -> AgentStatus {
    parse_status(tray::load_status(path, now).as_deref(), unix_secs(now))
}

/// Has the agent made contact with its server after `baseline` (seconds since the epoch of this
/// machine)? The wait after a change of settings: the window has no key, so it cannot ask the
/// server, and the agent writes `last_contact` itself when it registers and polls.
pub fn online_since(text: Option<&str>, now: f64, baseline: f64) -> bool {
    let status = parse_status(text, now);
    status.state == AgentState::Online && status.last_contact.is_some_and(|at| at > baseline)
}

/// `online_since` for the status file at `path`.
pub fn online_since_file(path: &Path, now: SystemTime, baseline: f64) -> bool {
    online_since(
        tray::load_status(path, now).as_deref(),
        unix_secs(now),
        baseline,
    )
}

// ---- the log

/// The log is read from its end; older lines are of no use in a window.
const MAX_LOG_BYTES: u64 = 64 * 1024;
/// The most lines a caller may ask for.
const MAX_LOG_LINES: usize = 1000;

/// The last `n` lines of `text`, joined by `\n` (a final line break does not count as a line).
pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The last `lines` lines of the log at `path`. A log that does not exist yet is an empty text.
/// Only the last [`MAX_LOG_BYTES`] are read, so a big log costs nothing.
pub fn read_log_in(path: &Path, lines: usize) -> std::io::Result<String> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let cut = len > MAX_LOG_BYTES;
    if cut {
        file.seek(SeekFrom::Start(len - MAX_LOG_BYTES))?;
    }
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // the start of a cut read is the middle of a line
    let text = match (cut, text.find('\n')) {
        (true, Some(i)) => &text[i + 1..],
        _ => &text,
    };
    Ok(last_lines(text, lines.min(MAX_LOG_LINES)))
}

// ---- the tray icon program

/// The program a setup run offers as the tray icon: this program itself. Windows: the running
/// exe. Linux: the AppImage file when this very program runs from one (`$APPIMAGE`; the running
/// exe is then inside the mount `$APPDIR`, which is gone with the process), else the running exe.
/// The AppImage runtime sets both variables, and every child process inherits them, so
/// `$APPIMAGE` alone proves nothing: a .deb-installed setup started from a terminal or launcher
/// that is itself an AppImage would otherwise install that foreign AppImage as the tray icon. It
/// counts only when `current_exe` lies below `$APPDIR` (both resolved by the caller).
/// https://github.com/AppImage/type2-runtime/blob/main/src/runtime/runtime.c (`setenv` of
/// `APPIMAGE` and `APPDIR` before the `execv` of `AppRun`)
/// macOS: the `.app` folder around the running exe, `None` outside one.
pub fn tray_source(
    os: Os,
    current_exe: &Path,
    appimage: Option<&OsStr>,
    appdir: Option<&OsStr>,
) -> Option<PathBuf> {
    match os {
        Os::Windows => Some(current_exe.to_path_buf()),
        Os::Linux => match (appimage, appdir) {
            (Some(a), Some(d))
                if !a.is_empty() && !d.is_empty() && current_exe.starts_with(Path::new(d)) =>
            {
                Some(PathBuf::from(a))
            }
            _ => Some(current_exe.to_path_buf()),
        },
        Os::Macos => current_exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
            .map(Path::to_path_buf),
    }
}

/// Is `source` the installed tray program? Windows file names do not differ by case.
fn is_installed(os: Os, source: &Path, installed: &Path) -> bool {
    if os == Os::Windows {
        source
            .to_string_lossy()
            .eq_ignore_ascii_case(&installed.to_string_lossy())
    } else {
        source == installed
    }
}

/// Put the tray program into the staging folder as `tray`, for the elevated install to copy.
/// Nothing is staged when the window runs from the installed place: the install ends every
/// program at that place (`install::tray_stop_cmds`), this window included, before it replaces
/// it. Returns whether something was staged.
pub fn stage_tray(
    os: Os,
    source: Option<&Path>,
    installed: &Path,
    staging: &Path,
    run: &Runner,
) -> std::io::Result<bool> {
    let Some(source) = source else {
        return Ok(false);
    };
    if is_installed(os, source, installed) {
        return Ok(false);
    }
    let target = staging.join("tray");
    // an earlier run's copy, file or folder
    match std::fs::remove_file(&target) {
        Ok(()) => {}
        Err(_) => match std::fs::remove_dir_all(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        },
    }
    if os == Os::Macos {
        // `ditto` copies a bundle with its metadata (and links, which `std::fs` would follow)
        let argv = vec![
            "/usr/bin/ditto".to_string(),
            source.to_string_lossy().into_owned(),
            target.to_string_lossy().into_owned(),
        ];
        run(&Cmd::new(argv, 120)).map_err(std::io::Error::other)?;
    } else {
        let bytes = std::fs::read(source)?;
        crate::staging::write_executable(staging, "tray", &bytes)?;
    }
    Ok(true)
}

/// The command that starts the installed tray icon for the current user, without administrator
/// rights. macOS: `open -n -a <app> --args --tray`; `-n` starts the installed copy even when
/// another instance of the app (this window, from Downloads) runs, `--args` hands the rest to the
/// app (`man open`). Elsewhere the program itself.
pub fn tray_start_cmd(os: Os, l: &Layout) -> Cmd {
    let tray = l.tray.to_string_lossy().into_owned();
    let argv = match os {
        Os::Macos => vec![
            "/usr/bin/open".to_string(),
            "-n".into(),
            "-a".into(),
            tray,
            "--args".into(),
            "--tray".into(),
        ],
        Os::Windows | Os::Linux => vec![tray, "--tray".into()],
    };
    Cmd::new(argv, 15)
}

#[cfg(test)]
mod tests {
    use super::*;
    use printpapi_agent::install::layout;
    use serde_json::{Value, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    const NOW: f64 = 1_794_048_800.0;

    fn file(state: &str, last_contact: f64) -> String {
        json!({
            "version": "3.3.0", "name": "PACKTISCH-1", "server": "https://print.example.com",
            "state": state, "last_contact": last_contact,
            "last_job": {"id": 4711, "ok": true, "at": last_contact - 90.0},
            "printers": [
                {"name": "Zebra GK420d", "pdf": false, "target": "Zebra GK420d"},
                {"name": "Lager", "pdf": false, "target": "socket://10.0.0.5:9100"},
                {"name": "Kasse", "pdf": false, "target": "file:///tmp/bon.prn"},
                {"name": "HP", "pdf": true, "target": "HP"}
            ],
            "timeout": 60.0,
            "header_names": ["cf-access-client-id"],
            "error": null
        })
        .to_string()
    }

    #[test]
    fn a_missing_file_is_not_running() {
        let s = parse_status(None, NOW);
        assert_eq!(s, AgentStatus::not_running());
        assert_eq!(s.state, AgentState::NotRunning);
    }

    #[test]
    fn a_truncated_file_is_not_running() {
        let text = file("online", NOW);
        let cut = &text[..text.len() / 2];
        assert_eq!(parse_status(Some(cut), NOW), AgentStatus::not_running());
        assert_eq!(parse_status(Some(""), NOW), AgentStatus::not_running());
    }

    #[test]
    fn wrong_types_are_not_running_and_show_no_details() {
        for text in [
            // printers is not a list
            json!({"version": "1", "name": "n", "server": "https://x", "state": "online",
                   "last_contact": NOW, "printers": "Zebra", "timeout": 60.0,
                   "header_names": [], "error": null}),
            // timeout is text
            json!({"version": "1", "name": "n", "server": "https://x", "state": "online",
                   "last_contact": NOW, "printers": [], "timeout": "60",
                   "header_names": [], "error": null}),
            // a job id that is not a number
            json!({"version": "1", "name": "n", "server": "https://x", "state": "online",
                   "last_contact": NOW, "last_job": {"id": "x", "ok": true, "at": 1.0},
                   "printers": [], "timeout": 60.0, "header_names": [], "error": null}),
            json!([1, 2, 3]),
        ] {
            assert_eq!(
                parse_status(Some(&text.to_string()), NOW),
                AgentStatus::not_running(),
                "{text}"
            );
        }
    }

    #[test]
    fn a_good_file_gives_every_field_in_the_words_of_the_contract() {
        let s = parse_status(Some(&file("online", NOW - 10.0)), NOW);
        let v: Value = serde_json::to_value(&s).unwrap();
        assert_eq!(v["state"], "online");
        assert_eq!(v["name"], "PACKTISCH-1");
        assert_eq!(v["server"], "https://print.example.com");
        assert_eq!(v["version"], "3.3.0");
        assert_eq!(v["lastContact"], NOW - 10.0);
        assert_eq!(
            v["lastJob"],
            json!({"id": 4711, "ok": true, "at": NOW - 100.0})
        );
        assert_eq!(v["timeout"], 60.0);
        assert_eq!(v["headerNames"], json!(["cf-access-client-id"]));
        assert_eq!(v["error"], Value::Null);
        // the printers the way the window handles them: a target only where it differs, a file
        // target locked
        assert_eq!(
            v["printers"],
            json!([
                {"name": "Zebra GK420d", "pdf": false, "target": null, "locked": false},
                {"name": "Lager", "pdf": false, "target": "socket://10.0.0.5:9100", "locked": false},
                {"name": "Kasse", "pdf": false, "target": "file:///tmp/bon.prn", "locked": true},
                {"name": "HP", "pdf": true, "target": null, "locked": false}
            ])
        );
    }

    #[test]
    fn offline_keeps_its_error_and_a_silent_online_agent_is_offline_with_the_silence() {
        let mut v: Value = serde_json::from_str(&file("offline", NOW - 5.0)).unwrap();
        v["error"] = json!("connection refused");
        let s = parse_status(Some(&v.to_string()), NOW);
        assert_eq!(s.state, AgentState::Offline);
        assert_eq!(s.error.as_deref(), Some("connection refused"));
        // the file is young (the agent writes it from a thread of its own), but the agent has not
        // talked to the server for ten minutes: it runs, and it says what it knows
        let s = parse_status(Some(&file("online", NOW - 600.0)), NOW);
        assert_eq!(s.state, AgentState::Offline);
        assert_eq!(s.name, "PACKTISCH-1");
        assert_eq!(
            s.error.as_deref(),
            Some("No contact with the server for 10 min.")
        );
        // a better reason in the file wins
        let mut v: Value = serde_json::from_str(&file("online", NOW - 600.0)).unwrap();
        v["error"] = json!("poll failed");
        assert_eq!(
            parse_status(Some(&v.to_string()), NOW).error.as_deref(),
            Some("poll failed")
        );
        // and a contact in the last two minutes is online without a word
        let s = parse_status(Some(&file("online", NOW - 100.0)), NOW);
        assert_eq!((s.state, s.error), (AgentState::Online, None));
    }

    #[test]
    fn an_old_file_on_disk_is_not_running_and_a_fresh_one_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("status.json");
        let now = SystemTime::now();
        let text = file("online", unix_secs(now) - 5.0);
        std::fs::write(&path, &text).unwrap();
        assert_eq!(status_from_file(&path, now).state, AgentState::Online);
        // ten minutes later nobody has rewritten it
        let later = now + Duration::from_secs(600);
        assert_eq!(status_from_file(&path, later), AgentStatus::not_running());
        assert_eq!(
            status_from_file(&tmp.path().join("none.json"), now),
            AgentStatus::not_running()
        );
        std::fs::write(&path, b"{\"state\": ").unwrap();
        assert_eq!(status_from_file(&path, now), AgentStatus::not_running());
    }

    #[test]
    fn the_wait_after_a_change_needs_a_contact_newer_than_the_change() {
        let text = file("online", NOW - 2.0);
        assert!(online_since(Some(&text), NOW, NOW - 5.0));
        // the old agent's last write, from before the change
        assert!(!online_since(Some(&text), NOW, NOW - 1.0));
        // offline is not online, however new
        assert!(!online_since(
            Some(&file("offline", NOW - 1.0)),
            NOW,
            NOW - 5.0
        ));
        assert!(!online_since(None, NOW, 0.0));
        assert!(!online_since(Some("{"), NOW, 0.0));
    }

    #[test]
    fn the_wait_reads_the_file_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("status.json");
        let now = SystemTime::now();
        let at = unix_secs(now);
        std::fs::write(&path, file("online", at - 1.0)).unwrap();
        assert!(online_since_file(&path, now, at - 3.0));
        assert!(!online_since_file(&path, now, at));
        assert!(!online_since_file(&tmp.path().join("none"), now, 0.0));
    }

    // ---- the log

    #[test]
    fn the_last_lines_are_the_last_lines() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
        assert_eq!(last_lines("", 3), "");
        assert_eq!(last_lines("a\r\nb\r\n", 1), "b");
    }

    #[test]
    fn a_log_that_does_not_exist_yet_is_empty_and_an_unreadable_one_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_log_in(&tmp.path().join("none.log"), 50).unwrap(), "");
        // a folder where the file should be
        assert!(read_log_in(tmp.path(), 50).is_err());
    }

    #[test]
    fn a_big_log_is_read_from_its_end_without_a_cut_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("print_agent-error.log");
        let mut text = String::new();
        for i in 0..20_000 {
            text.push_str(&format!("2026-11-07 10:00:00 line {i} Büro\n"));
        }
        std::fs::write(&path, &text).unwrap();
        assert!(text.len() as u64 > MAX_LOG_BYTES * 5);
        assert_eq!(
            read_log_in(&path, 2).unwrap(),
            "2026-11-07 10:00:00 line 19998 Büro\n2026-11-07 10:00:00 line 19999 Büro"
        );
        // asking for more than a window can hold is held to the limit, and the first line of what
        // was read (the middle of one in the file) is not among them
        let all = read_log_in(&path, usize::MAX).unwrap();
        assert!(all.lines().count() <= MAX_LOG_LINES);
        assert!(
            all.lines()
                .all(|l| l.starts_with("2026-11-07 10:00:00 line "))
        );
    }

    #[test]
    fn a_log_with_broken_text_still_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x.log");
        std::fs::write(&path, b"ok line\nB\xfcro line\n").unwrap();
        let text = read_log_in(&path, 5).unwrap();
        assert!(text.starts_with("ok line\nB") && text.ends_with("ro line"));
    }

    // ---- the tray program

    #[test]
    fn the_program_offered_as_tray_icon_is_this_program() {
        let win = Path::new("C:\\Users\\a\\Downloads\\printpapi-setup.exe");
        assert_eq!(
            tray_source(Os::Windows, win, None, None).as_deref(),
            Some(win)
        );
        let exe = Path::new("/tmp/.mount_x/usr/bin/printpapi-setup");
        let image = OsStr::new("/home/a/printpapi.AppImage");
        let mount = OsStr::new("/tmp/.mount_x");
        // an AppImage is the file, not the mount inside it
        assert_eq!(
            tray_source(Os::Linux, exe, Some(image), Some(mount)).as_deref(),
            Some(Path::new("/home/a/printpapi.AppImage"))
        );
        assert_eq!(
            tray_source(Os::Linux, exe, Some(OsStr::new("")), Some(mount)).as_deref(),
            Some(exe)
        );
        assert_eq!(
            tray_source(Os::Linux, exe, None, None).as_deref(),
            Some(exe)
        );
        // the variables are inherited: a .deb-installed setup that a terminal running as an
        // AppImage started sees them, but its exe is not inside that mount, so the foreign
        // AppImage is not offered as the tray icon
        let deb = Path::new("/usr/bin/printpapi-setup");
        assert_eq!(
            tray_source(Os::Linux, deb, Some(image), Some(mount)).as_deref(),
            Some(deb)
        );
        // without $APPDIR nothing shows that the exe is the AppImage's
        assert_eq!(
            tray_source(Os::Linux, exe, Some(image), None).as_deref(),
            Some(exe)
        );
        // a mount that only shares a name prefix is another folder
        assert_eq!(
            tray_source(
                Os::Linux,
                Path::new("/tmp/.mount_xy/usr/bin/printpapi-setup"),
                Some(image),
                Some(mount)
            )
            .as_deref(),
            Some(Path::new("/tmp/.mount_xy/usr/bin/printpapi-setup"))
        );
        let mac =
            Path::new("/Users/a/Downloads/printpapi Agent.app/Contents/MacOS/printpapi-setup");
        assert_eq!(
            tray_source(Os::Macos, mac, None, None).as_deref(),
            Some(Path::new("/Users/a/Downloads/printpapi Agent.app"))
        );
        // a development run, outside a bundle
        assert_eq!(
            tray_source(
                Os::Macos,
                Path::new("/src/target/debug/printpapi-setup"),
                None,
                None
            ),
            None
        );
    }

    fn recorder() -> (Runner, Rc<RefCell<Vec<Vec<String>>>>) {
        let calls: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        let seen = calls.clone();
        let run: Runner = Rc::new(move |cmd: &Cmd| {
            seen.borrow_mut().push(cmd.argv.clone());
            Ok(Vec::new())
        });
        (run, calls)
    }

    #[test]
    fn a_window_run_from_the_installed_place_stages_no_tray() {
        let tmp = tempfile::tempdir().unwrap();
        let (run, calls) = recorder();
        let installed = Path::new("C:\\Program Files\\printpapi-agent\\printpapi-tray.exe");
        // the case of a Windows path does not matter
        let source = Path::new("c:\\program files\\PRINTPAPI-AGENT\\printpapi-tray.exe");
        assert!(!stage_tray(Os::Windows, Some(source), installed, tmp.path(), &run).unwrap());
        assert!(!tmp.path().join("tray").exists());
        // Linux: the AppImage that was installed
        let l = Path::new("/opt/printpapi-agent/printpapi-tray");
        assert!(!stage_tray(Os::Linux, Some(l), l, tmp.path(), &run).unwrap());
        // macOS: the installed bundle
        let app = Path::new("/Applications/printpapi Agent.app");
        assert!(!stage_tray(Os::Macos, Some(app), app, tmp.path(), &run).unwrap());
        // nothing to offer
        assert!(!stage_tray(Os::Macos, None, app, tmp.path(), &run).unwrap());
        assert!(calls.borrow().is_empty());
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_download_stages_its_own_program_as_tray() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("downloaded-setup");
        std::fs::write(&src, b"program bytes").unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir(&staging).unwrap();
        // an earlier copy goes
        std::fs::write(staging.join("tray"), b"old").unwrap();
        let (run, calls) = recorder();
        let installed = Path::new("/opt/printpapi-agent/printpapi-tray");
        assert!(stage_tray(Os::Linux, Some(&src), installed, &staging, &run).unwrap());
        assert_eq!(
            std::fs::read(staging.join("tray")).unwrap(),
            b"program bytes"
        );
        assert!(
            calls.borrow().is_empty(),
            "a file is copied, no program is run"
        );
        // a source that cannot be read is an error, not an empty file
        let none = tmp.path().join("missing");
        assert!(stage_tray(Os::Linux, Some(&none), installed, &staging, &run).is_err());
    }

    #[test]
    fn macos_copies_the_bundle_with_ditto_by_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        // an earlier copy, a folder, goes first
        std::fs::create_dir_all(tmp.path().join("tray/Contents")).unwrap();
        let (run, calls) = recorder();
        let src = Path::new("/Users/a/Downloads/printpapi Agent.app");
        let installed = Path::new("/Applications/printpapi Agent.app");
        assert!(stage_tray(Os::Macos, Some(src), installed, tmp.path(), &run).unwrap());
        assert!(!tmp.path().join("tray").exists(), "the old copy is gone");
        assert_eq!(
            calls.borrow().as_slice(),
            [vec![
                "/usr/bin/ditto".to_string(),
                src.to_string_lossy().into_owned(),
                tmp.path().join("tray").to_string_lossy().into_owned(),
            ]]
        );
    }

    #[test]
    fn the_icon_is_started_for_the_user_with_absolute_programs() {
        let win = layout(Os::Windows, Path::new("C:\\Program Files"));
        assert_eq!(
            tray_start_cmd(Os::Windows, &win).argv,
            [
                "C:\\Program Files\\printpapi-agent\\printpapi-tray.exe",
                "--tray"
            ]
        );
        let lin = layout(Os::Linux, Path::new("/unused"));
        assert_eq!(
            tray_start_cmd(Os::Linux, &lin).argv,
            ["/opt/printpapi-agent/printpapi-tray", "--tray"]
        );
        let mac = layout(Os::Macos, Path::new("/unused"));
        assert_eq!(
            tray_start_cmd(Os::Macos, &mac).argv,
            [
                "/usr/bin/open",
                "-n",
                "-a",
                "/Applications/printpapi Agent.app",
                "--args",
                "--tray"
            ]
        );
    }
}
