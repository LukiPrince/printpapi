// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The tray icon mode of the app (`--tray`): the state of the agent in the notification area, and a
//! menu with "Settings…", "Open dashboard" and "Quit". The icon only reads the status file the
//! agent writes; it opens no port and sends the agent nothing. "Quit" ends the icon, never the
//! agent.
//!
//! The decisions (state from the file, which address may be opened, who holds the lock) are pure
//! functions with tests; [`setup`] is the thin part that talks to Tauri.
//!
//! Tauri's tray API: https://tauri.app/learn/system-tray/ (`TrayIconBuilder`, `.menu`,
//! `.on_menu_event`, `.show_menu_on_left_click`) and, for the calls that change the icon later,
//! `tauri::tray::TrayIcon::{set_icon, set_tooltip}` and `tauri::menu::MenuItem::{set_text,
//! set_enabled}` (tauri 2.12.5 `src/tray/mod.rs`, `src/menu/normal.rs`). These dispatch to the main
//! thread themselves, so the polling thread may call them.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use printpapi_agent::install::{self, Os};
use printpapi_agent::proc::{Cmd, run_command};
use printpapi_agent::status;
use serde::Deserialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};

/// A status file that was not written for this long means the agent is not running: the agent
/// rewrites it every 30 s from a thread of its own, whatever its main thread is doing. An `online`
/// agent that has not made contact with the server for this long is shown as offline.
const STALE_SECS: f64 = 120.0;
/// The status file is a few hundred bytes; more than this is not read.
const MAX_STATUS_BYTES: u64 = 64 * 1024;
/// How often the file is read.
const POLL: Duration = Duration::from_secs(5);
/// What the tooltip says first; with the status line (at most [`LINE_MAX`] characters) it stays
/// below the 127 characters a Windows tooltip holds.
/// https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataw
const TOOLTIP_PREFIX: &str = "printpapi Agent: ";
const LINE_MAX: usize = 100;
const OPEN_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Online,
    Offline,
    NotRunning,
}

/// The part of the status file this icon uses. Other fields (printers, header names) are for the
/// settings window; a field of the wrong type fails the whole parse, which means "not running".
#[derive(Deserialize)]
struct StatusView {
    state: String,
    server: String,
    #[serde(default)]
    last_contact: Option<f64>,
    #[serde(default)]
    error: Option<String>,
}

fn not_running() -> (TrayState, Option<String>) {
    (
        TrayState::NotRunning,
        Some("Agent is not running".to_string()),
    )
}

/// How long the agent has been silent, for a line of text: minutes up to two hours, then hours.
pub fn quiet_for(secs: f64) -> String {
    if secs < 7200.0 {
        format!("{} min", (secs / 60.0).floor() as u64)
    } else {
        format!("{} h", (secs / 3600.0).floor() as u64)
    }
}

/// The state and the line for the first menu item and the tooltip. A file that is missing, cut
/// off or of the wrong types is "not running" (`load_status` also says so for one that is old). An
/// `online` agent whose last contact is older than [`STALE_SECS`] is offline: the file is young
/// (the agent writes it from a thread of its own), so it runs, but its main thread is in a long job
/// or a call that hangs, and the server has not heard from it. One that never made contact is not
/// `online` at all.
// ponytail: the second value is always `Some` (the plan's signature); it is a line for every state.
pub fn read_state(text: Option<&str>, now: f64) -> (TrayState, Option<String>) {
    let Some(view) = text.and_then(|t| serde_json::from_str::<StatusView>(t).ok()) else {
        return not_running();
    };
    match view.state.as_str() {
        "online" => {
            let Some(at) = view.last_contact else {
                return not_running();
            };
            if now - at > STALE_SECS {
                let line = format!(
                    "Offline: no contact with the server for {}",
                    quiet_for(now - at)
                );
                return (TrayState::Offline, Some(fit(&line)));
            }
            let line = match host_of(&view.server) {
                Some(host) => format!("Connected to {host}"),
                None => "Connected".to_string(),
            };
            (TrayState::Online, Some(fit(&line)))
        }
        "offline" => {
            // one menu line: the whitespace of a multi-line error collapses
            let error = view
                .error
                .as_deref()
                .map(|e| e.split_whitespace().collect::<Vec<_>>().join(" "));
            let line = match error.as_deref() {
                Some(e) if !e.is_empty() => format!("Offline: {e}"),
                _ => "Offline".to_string(),
            };
            (TrayState::Offline, Some(fit(&line)))
        }
        _ => not_running(),
    }
}

/// At most [`LINE_MAX`] characters, with `…` where it was cut.
fn fit(line: &str) -> String {
    if line.chars().count() <= LINE_MAX {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(LINE_MAX - 1).collect();
    cut.push('…');
    cut
}

/// The `host[:port]` of an `http(s)` address, without credentials, path or query.
fn host_of(server: &str) -> Option<&str> {
    let rest = strip_scheme(server)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    (!host.is_empty()).then_some(host)
}

/// The text after `http://` or `https://` (any case), `None` for every other start.
fn strip_scheme(url: &str) -> Option<&str> {
    ["https://", "http://"].into_iter().find_map(|scheme| {
        let head = url.get(..scheme.len())?;
        head.eq_ignore_ascii_case(scheme)
            .then(|| &url[scheme.len()..])
    })
}

/// The address "Open dashboard" opens: the `server` of the status file, as written. Whether it may
/// be opened is [`open_url_cmd`]'s decision.
pub fn dashboard_url(text: Option<&str>) -> Option<String> {
    #[derive(Deserialize)]
    struct ServerOnly {
        server: String,
    }
    serde_json::from_str::<ServerOnly>(text?)
        .ok()
        .map(|s| s.server)
}

/// The command that opens `url` in the default browser, run as the signed-in user. `None` unless
/// the address is `http://` or `https://` with a host and without whitespace, control characters
/// or quotes (the file is written by the agent, but this is the trust boundary before a program is
/// started). Programs are absolute where a search path would matter: Windows `explorer.exe` lives
/// in the Windows folder, the parent of the system folder `sys`; macOS `/usr/bin/open`; Linux
/// `xdg-open`.
// ponytail: `xdg-open` is found through PATH, because its place differs between distributions and
// the icon runs as the user, who could set their own PATH anyway. Ceiling: none that matters.
pub fn open_url_cmd(os: Os, sys: &Path, url: &str) -> Option<Cmd> {
    let rest = strip_scheme(url)?;
    let bad = |c: char| {
        c.is_whitespace() || c.is_control() || matches!(c, '"' | '\\' | '<' | '>' | '^' | '|' | '`')
    };
    if rest.is_empty() || rest.starts_with(['/', '?', '#']) || url.chars().any(bad) {
        return None;
    }
    let program = match os {
        Os::Windows => {
            // `explorer.exe <address>` hands an address to the default program for it.
            let sys = sys.to_string_lossy();
            let sys = sys.trim_end_matches(['\\', '/']);
            let windows = &sys[..sys.rfind(['\\', '/'])?];
            if windows.is_empty() || windows.ends_with(':') {
                return None;
            }
            format!("{windows}\\explorer.exe")
        }
        Os::Macos => "/usr/bin/open".to_string(),
        Os::Linux => "xdg-open".to_string(),
    };
    Some(Cmd::new(vec![program, url.to_string()], OPEN_TIMEOUT_SECS))
}

/// Hold the lock of this user's tray icon until the returned file is dropped (or the process ends,
/// also by a crash: the system releases it). `None`: another icon holds it, or the lock file
/// cannot be made, in which case two icons could not be kept apart, so this one stays away, too.
// ponytail: the lock is per user, not per login session. A user signed in twice at once gets one
// icon, in the session that started first. Ceiling: two sessions of one user.
pub fn single_instance(lock: &Path) -> Option<File> {
    std::fs::create_dir_all(lock.parent()?).ok()?;
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// The text of the status file, or `None` when it is missing, unreadable, not UTF-8, or has not
/// been written for [`STALE_SECS`]. Reading stops after [`MAX_STATUS_BYTES`].
pub fn load_status(path: &Path, now: SystemTime) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let written = file.metadata().ok()?.modified().ok()?;
    // a file dated in the future (a clock that was set back) is not old
    if now
        .duration_since(written)
        .is_ok_and(|age| age.as_secs_f64() > STALE_SECS)
    {
        return None;
    }
    let mut text = String::new();
    (&mut file)
        .take(MAX_STATUS_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The folder of the lock file: `$XDG_RUNTIME_DIR` (made per user by the system, see the XDG Base
/// Directory Specification, https://specifications.freedesktop.org/basedir-spec/latest/), else
/// `~/.cache`. Only paths from the root count (`has_root`, which also holds where this is tested on Windows). `None` when neither is usable.
#[cfg_attr(windows, allow(dead_code))] // used off Windows; its test runs everywhere
pub fn unix_lock_dir(xdg_runtime_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute = |v: Option<OsString>| v.map(PathBuf::from).filter(|p| p.has_root());
    let base = absolute(xdg_runtime_dir).or_else(|| absolute(home).map(|h| h.join(".cache")))?;
    Some(base.join("printpapi"))
}

/// A menu text in which `&` is a literal ampersand and not the mark of a keyboard shortcut
/// (`MenuItem::set_text`: "To display a `&` without assigning a mnemonic, use `&&`").
pub fn literal_menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

fn icon_bytes(state: TrayState) -> &'static [u8] {
    match state {
        TrayState::Online => include_bytes!("../icons/tray-online.png"),
        TrayState::Offline => include_bytes!("../icons/tray-offline.png"),
        TrayState::NotRunning => include_bytes!("../icons/tray-stopped.png"),
    }
}

/// The lock file of this user's icon: in the local profile (Windows `FOLDERID_LocalAppData`),
/// else in the runtime folder or the cache.
pub fn lock_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::staging::local_app_data()
            .ok()
            .map(|p| p.join("printpapi").join("printpapi-tray.lock"))
    }
    #[cfg(not(windows))]
    {
        unix_lock_dir(
            std::env::var_os("XDG_RUNTIME_DIR"),
            std::env::var_os("HOME"),
        )
        .map(|d| d.join("printpapi-tray.lock"))
    }
}

/// What identifies the installed tray program: size, time of the last change and, on Unix, the
/// inode (an update renames a new file over it, so the inode differs even when the rest does not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

/// The stamp of `path`, `None` when there is nothing there.
pub fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        inode: std::os::unix::fs::MetadataExt::ino(&meta),
    })
}

/// Was the program, which was there at the start, replaced or removed? An icon that never saw it
/// there has nothing to compare (a development run from elsewhere) and does not end.
pub fn replaced(at_start: Option<&FileStamp>, now: Option<&FileStamp>) -> bool {
    at_start.is_some() && at_start != now
}

/// Where the agent leaves its status file: `status.json` in the agent's own status folder.
pub fn status_path() -> PathBuf {
    status::status_dir(Os::host()).join("status.json")
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Start the settings window: this program again, without `--tray`, as a process of its own.
fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Open `url` if it is one that may be opened; a thread, so that a slow program does not hold
/// the menu.
fn open_dashboard(url: Option<String>) {
    let Some(cmd) = url.and_then(|u| open_url_cmd(Os::host(), &install::system_dir(), &u)) else {
        return;
    };
    std::thread::spawn(move || {
        let _ = run_command(&cmd);
    });
}

/// Build the icon and its menu and keep both up to date. Called from Tauri's `setup`, in `--tray`
/// mode only; there is no window.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // no Dock icon and no menu bar of its own
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    let line = MenuItem::with_id(app, "status", "Agent is not running", false, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let dashboard = MenuItem::with_id(app, "dashboard", "Open dashboard", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &line,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &dashboard,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    // the address of "Open dashboard": the last server the status file named
    let address: Arc<Mutex<Option<String>>> = Arc::default();
    let for_menu = Arc::clone(&address);
    let tray = TrayIconBuilder::with_id("printpapi-agent")
        .icon(Image::from_bytes(icon_bytes(TrayState::NotRunning))?)
        .tooltip(format!("{TOOLTIP_PREFIX}Agent is not running"))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "settings" => open_settings(),
            "dashboard" => open_dashboard(
                for_menu
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            // the icon only; the agent is a service of its own
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    let handle = app.handle().clone();
    std::thread::spawn(move || watch(&handle, &tray, &line, &dashboard, &address));
    Ok(())
}

/// Read the status file every [`POLL`] and change icon, tooltip and menu when something differs.
/// The icon also ends itself when the installed program file is replaced or removed. An update or
/// a removal cannot find the process of an AppImage by its command line (the running program is
/// inside a mount, not at the installed path), and a stale icon would hold the lock of
/// [`single_instance`] so that the new one ends at once. On Windows the running exe cannot be
/// replaced, and the install ends it by image name.
fn watch(
    app: &tauri::AppHandle,
    tray: &TrayIcon,
    line: &MenuItem<tauri::Wry>,
    dashboard: &MenuItem<tauri::Wry>,
    address: &Mutex<Option<String>>,
) {
    let path = status_path();
    let program =
        (!cfg!(windows)).then(|| install::layout(Os::host(), &install::program_files()).tray);
    let at_start = program.as_deref().and_then(file_stamp);
    let mut shown: Option<(TrayState, String, bool)> = None;
    loop {
        if let Some(program) = &program
            && replaced(at_start.as_ref(), file_stamp(program).as_ref())
        {
            app.exit(0);
            return;
        }
        let text = load_status(&path, SystemTime::now());
        let (state, text_line) = read_state(text.as_deref(), now_secs());
        let text_line = text_line.unwrap_or_default();
        let mut last = address.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(url) = dashboard_url(text.as_deref()) {
            *last = Some(url);
        }
        let can_open = last
            .as_deref()
            .is_some_and(|u| open_url_cmd(Os::host(), &install::system_dir(), u).is_some());
        drop(last);
        let now_shown = (state, text_line, can_open);
        if shown.as_ref() != Some(&now_shown) {
            let (state, text_line, can_open) = &now_shown;
            if let Ok(image) = Image::from_bytes(icon_bytes(*state)) {
                let _ = tray.set_icon(Some(image));
            }
            let _ = tray.set_tooltip(Some(format!("{TOOLTIP_PREFIX}{text_line}")));
            let _ = line.set_text(literal_menu_text(text_line));
            let _ = dashboard.set_enabled(*can_open);
            shown = Some(now_shown);
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    const NOW: f64 = 1_794_048_800.0;

    fn status(state: &str, last_contact: &str, error: &str, server: &str) -> String {
        format!(
            r#"{{"version":"3.3.0","name":"PACKTISCH-1","server":"{server}","state":"{state}",
            "last_contact":{last_contact},"last_job":null,"printers":[],"timeout":60,
            "header_names":[],"error":{error}}}"#
        )
    }

    fn online() -> String {
        status(
            "online",
            "1794048790.0",
            "null",
            "https://print.example.com",
        )
    }

    #[test]
    fn missing_file_is_not_running() {
        assert_eq!(
            read_state(None, NOW),
            (
                TrayState::NotRunning,
                Some("Agent is not running".to_string())
            )
        );
    }

    #[test]
    fn a_silent_agent_with_a_young_file_is_offline_not_dead() {
        // The agent rewrites the file from a thread of its own while its main thread is in a long
        // job or a call that hangs, so a young file with an old contact is an agent that runs and
        // has not talked to the server for a while. (The age of the file itself is `load_status`.)
        let just_fresh = status("online", "1794048680.0", "null", "https://p.example.com");
        assert_eq!(read_state(Some(&just_fresh), NOW).0, TrayState::Online);
        let quiet = status("online", "1794048679.0", "null", "https://p.example.com");
        assert_eq!(
            read_state(Some(&quiet), NOW),
            (
                TrayState::Offline,
                Some("Offline: no contact with the server for 2 min".to_string())
            )
        );
        let long = status("online", "1794037999.0", "null", "https://p.example.com");
        assert_eq!(
            read_state(Some(&long), NOW).1.as_deref(),
            Some("Offline: no contact with the server for 3 h")
        );
        // an online agent that never made contact is not online, and says nothing true either
        let never = status("online", "null", "null", "https://p.example.com");
        assert_eq!(read_state(Some(&never), NOW).0, TrayState::NotRunning);
    }

    #[test]
    fn a_silence_is_told_in_minutes_then_hours() {
        assert_eq!(quiet_for(121.0), "2 min");
        assert_eq!(quiet_for(600.0), "10 min");
        assert_eq!(quiet_for(7199.0), "119 min");
        assert_eq!(quiet_for(7200.0), "2 h");
        assert_eq!(quiet_for(10801.0), "3 h");
    }

    #[test]
    fn truncated_json_is_not_running() {
        let whole = online();
        for cut in [0, 1, 17, whole.len() / 2, whole.len() - 1] {
            assert_eq!(
                read_state(Some(&whole[..cut]), NOW).0,
                TrayState::NotRunning,
                "cut at {cut}"
            );
        }
        for junk in ["", "null", "[]", "42", "{}", "\u{0}\u{0}\u{0}"] {
            assert_eq!(
                read_state(Some(junk), NOW).0,
                TrayState::NotRunning,
                "{junk:?}"
            );
        }
    }

    #[test]
    fn wrong_types_are_not_running() {
        let cases = [
            r#"{"state":5,"server":"https://a.example","last_contact":1794048790.0}"#,
            r#"{"state":"online","server":["x"],"last_contact":1794048790.0}"#,
            r#"{"state":"online","server":"https://a.example","last_contact":"yesterday"}"#,
            r#"{"state":"offline","server":"https://a.example","error":{"a":1}}"#,
            r#"{"state":"flying","server":"https://a.example","last_contact":1794048790.0}"#,
            r#"{"server":"https://a.example","last_contact":1794048790.0}"#,
        ];
        for case in cases {
            assert_eq!(
                read_state(Some(case), NOW).0,
                TrayState::NotRunning,
                "{case}"
            );
        }
    }

    #[test]
    fn offline_shows_the_error() {
        let text = status(
            "offline",
            "1794040000.0",
            r#""could not reach print.example.com""#,
            "https://print.example.com",
        );
        assert_eq!(
            read_state(Some(&text), NOW),
            (
                TrayState::Offline,
                Some("Offline: could not reach print.example.com".to_string())
            )
        );
        // no error text, and an error with line breaks and more than a menu line holds
        let bare = status("offline", "null", "null", "https://print.example.com");
        assert_eq!(read_state(Some(&bare), NOW).1.as_deref(), Some("Offline"));
        let long = status(
            "offline",
            "null",
            &format!(r#""first\nsecond   {}""#, "x".repeat(300)),
            "https://print.example.com",
        );
        let line = read_state(Some(&long), NOW).1.unwrap();
        assert!(line.starts_with("Offline: first second xxx"), "{line}");
        assert!(!line.contains('\n'));
        assert!(line.chars().count() <= 100, "{}", line.chars().count());
    }

    #[test]
    fn online_names_the_server_host() {
        assert_eq!(
            read_state(Some(&online()), NOW),
            (
                TrayState::Online,
                Some("Connected to print.example.com".to_string())
            )
        );
        // port kept; path, credentials and the scheme's case do not matter
        let odd = status(
            "online",
            "1794048790.0",
            "null",
            "HTTP://user:secret@10.0.0.5:3460/base/",
        );
        let line = read_state(Some(&odd), NOW).1.unwrap();
        assert_eq!(line, "Connected to 10.0.0.5:3460");
        assert!(!line.contains("secret"));
        // nothing usable as a host: still online
        let none = status("online", "1794048790.0", "null", "not a url");
        assert_eq!(read_state(Some(&none), NOW).1.as_deref(), Some("Connected"));
    }

    #[test]
    fn the_dashboard_address_is_the_server_of_the_file() {
        assert_eq!(
            dashboard_url(Some(&online())).as_deref(),
            Some("https://print.example.com")
        );
        assert_eq!(dashboard_url(None), None);
        assert_eq!(dashboard_url(Some("{")), None);
        assert_eq!(dashboard_url(Some(r#"{"server":7}"#)), None);
    }

    const WIN_SYS: &str = "C:\\Windows\\System32";

    #[test]
    fn open_url_refuses_other_schemes() {
        for url in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "",
            "ftp://print.example.com",
            "ms-msdt:/id PCWDiagnostic",
            "print.example.com",
            "http:",
            "http://",
            "https:///x",
            "https://a.example/with space",
            "https://a.example/\"quote",
            "https://a.example/\nnewline",
            "-https://a.example",
            " https://a.example",
        ] {
            for os in [Os::Windows, Os::Macos, Os::Linux] {
                assert_eq!(open_url_cmd(os, Path::new(WIN_SYS), url), None, "{url:?}");
            }
        }
        assert!(open_url_cmd(Os::Linux, Path::new("/"), "HTTPS://A.example/x?y=1#z").is_some());
    }

    #[test]
    fn open_url_uses_absolute_programs() {
        let url = "https://print.example.com/dashboard";
        let win = open_url_cmd(Os::Windows, Path::new(WIN_SYS), url).unwrap();
        assert_eq!(win.argv, ["C:\\Windows\\explorer.exe", url]);
        let mac = open_url_cmd(Os::Macos, Path::new("/"), url).unwrap();
        assert_eq!(mac.argv, ["/usr/bin/open", url]);
        let linux = open_url_cmd(Os::Linux, Path::new("/"), "http://localhost:3460").unwrap();
        assert_eq!(linux.argv, ["xdg-open", "http://localhost:3460"]);
        // a system folder that is not under a Windows folder gives no command
        assert_eq!(open_url_cmd(Os::Windows, Path::new("System32"), url), None);
    }

    #[test]
    fn second_instance_gets_no_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("sub").join("printpapi-tray.lock");
        let first = single_instance(&lock).expect("the first icon gets the lock");
        assert!(single_instance(&lock).is_none(), "a second icon stays away");
        drop(first);
        assert!(single_instance(&lock).is_some(), "the lock is free again");
    }

    #[test]
    fn the_icon_ends_when_its_program_is_replaced_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("printpapi-tray");
        // not installed where this icon looks: nothing to watch, it never ends for that
        assert_eq!(file_stamp(&program), None);
        assert!(!replaced(None, None));
        std::fs::write(&program, b"old program").unwrap();
        let start = file_stamp(&program);
        assert!(start.is_some());
        assert!(!replaced(start.as_ref(), file_stamp(&program).as_ref()));
        // an update puts a new file in place, as `place_file` of the install does
        let new = dir.path().join(".printpapi-tray.new");
        std::fs::write(&new, b"a newer program, other length").unwrap();
        std::fs::rename(&new, &program).unwrap();
        assert!(replaced(start.as_ref(), file_stamp(&program).as_ref()));
        // a removal
        let start = file_stamp(&program);
        std::fs::remove_file(&program).unwrap();
        assert!(replaced(start.as_ref(), file_stamp(&program).as_ref()));
    }

    #[test]
    fn without_a_lock_file_there_is_no_icon() {
        let dir = tempfile::tempdir().unwrap();
        // a file where the lock's folder should be
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        assert!(single_instance(&blocker.join("printpapi-tray.lock")).is_none());
    }

    #[test]
    fn status_file_is_read_only_while_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        assert_eq!(load_status(&path, SystemTime::now()), None);
        std::fs::write(&path, online()).unwrap();
        assert_eq!(load_status(&path, SystemTime::now()), Some(online()));
        // the same file, seen 121 s later
        let later = SystemTime::now() + Duration::from_secs(121);
        assert_eq!(load_status(&path, later), None);
        // a file dated in the future is not old
        let earlier = SystemTime::now() - Duration::from_secs(3600);
        assert_eq!(load_status(&path, earlier), Some(online()));
    }

    #[test]
    fn a_huge_or_binary_status_file_is_not_read_as_status() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        std::fs::write(&path, vec![b'a'; 200_000]).unwrap();
        let text = load_status(&path, SystemTime::now());
        assert_eq!(read_state(text.as_deref(), NOW).0, TrayState::NotRunning);
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(load_status(&path, SystemTime::now()), None);
    }

    #[test]
    fn the_lock_folder_is_the_runtime_dir_else_the_cache() {
        use std::ffi::OsString;
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            unix_lock_dir(os("/run/user/1000"), os("/home/a")),
            Some(PathBuf::from("/run/user/1000/printpapi"))
        );
        for empty in [None, os(""), os("relative/dir")] {
            assert_eq!(
                unix_lock_dir(empty, os("/home/a")),
                Some(PathBuf::from("/home/a/.cache/printpapi"))
            );
        }
        assert_eq!(unix_lock_dir(None, None), None);
        assert_eq!(unix_lock_dir(None, os("")), None);
        assert_eq!(unix_lock_dir(None, os("home")), None);
    }

    #[test]
    fn an_ampersand_in_a_menu_line_is_not_a_shortcut() {
        assert_eq!(
            literal_menu_text("Offline: a&b && c"),
            "Offline: a&&b &&&& c"
        );
        assert_eq!(literal_menu_text("plain"), "plain");
    }
}
