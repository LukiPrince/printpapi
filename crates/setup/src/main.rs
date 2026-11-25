// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// No console window next to the setup window in a release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod elevate;
mod managed;
mod settings;
mod staging;
mod tray;
mod updates;
#[cfg(windows)]
mod winenv;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::sync::{Mutex, PoisonError};

use tauri::{Manager, RunEvent};

use commands::AppState;

/// What the person reads when the Microsoft Edge WebView2 Runtime is missing.
const WEBVIEW_MISSING: &str = "This setup needs the Microsoft Edge WebView2 Runtime.\n\n\
     https://developer.microsoft.com/microsoft-edge/webview2/";

/// Is there a web view to show the window in? Tauri makes its windows only once the event loop
/// runs, and a missing runtime then panics after Tauri's own dialog (tauri 2.12.5 `app.rs`,
/// `tauri-runtime-wry` 2.12.3 `lib.rs`: "Could not find the WebView2 Runtime"), so `.build()` never
/// reports it. This is the check Tauri makes itself, run first so that the person gets the text
/// above. Other platforms ship their web view or fail to start before this point.
fn startup_problem(webview: Result<String, String>, on_windows: bool) -> Option<&'static str> {
    (on_windows && webview.is_err()).then_some(WEBVIEW_MISSING)
}

/// What this run of the program is for. One parser, so every mode has one rule for its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Exactly `--version`: print the version and end, before anything else is looked at.
    Version,
    /// An argument that starts with `--managed-`: the headless modes of the .msi (Windows only,
    /// `managed.rs`). A call that is not one of the three forms is kept as an error: exit 2.
    Managed(Result<managed::Command, managed::Usage>),
    /// Any argument `--tray`: the icon.
    Tray,
    /// The settings window or the setup, opened on a section when `--section <word>` names one.
    Window { section: Option<&'static str> },
}

/// The mode for the arguments after the program name. A `--section` word that is not one of
/// [`updates::SECTIONS`] is ignored.
pub fn mode(args: &[OsString]) -> Mode {
    if args.len() == 1 && args[0] == "--version" {
        return Mode::Version;
    }
    if let Some(call) = managed::parse(args) {
        return Mode::Managed(call);
    }
    if args.iter().any(|a| a == "--tray") {
        return Mode::Tray;
    }
    let section = args
        .windows(2)
        .find(|w| w[0] == "--section")
        .and_then(|w| w[1].to_str())
        .and_then(|word| updates::SECTIONS.iter().copied().find(|s| *s == word));
    Mode::Window { section }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mode = mode(&args);
    // the installer's modes: no tray lock, no web view check, no window (session 0 has no desktop)
    if let Mode::Managed(call) = &mode {
        return managed::run(call);
    }
    updates::note_start();
    // the updater starts the new program with this before it changes anything: no lock, no web
    // view check, no window. `let _`: a program without a console may have no stdout.
    if mode == Mode::Version {
        let _ = writeln!(
            std::io::stdout(),
            "printpapi-setup {}",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }
    if let Mode::Window { section } = &mode {
        updates::set_start_section(*section);
    }
    let tray_mode = mode == Mode::Tray;
    // The icon lives as long as the process; a second icon of this user ends, once the first one
    // has had time to end itself after an update (see `tray::single_instance_within`).
    let _single = if tray_mode {
        match tray::lock_path()
            .as_deref()
            .and_then(|lock| tray::single_instance_within(lock, tray::LOCK_WAIT))
        {
            Some(lock) => Some(lock),
            None => return ExitCode::SUCCESS,
        }
    } else {
        None
    };
    // The icon needs no web view: it makes no window.
    if !tray_mode {
        let webview = tauri::webview_version().map_err(|e| e.to_string());
        if let Some(problem) = startup_problem(webview, cfg!(windows)) {
            fail(problem);
            return ExitCode::FAILURE;
        }
    }
    let built = tauri::Builder::default()
        // `tauri.conf.json` has `"create": false`: the window is made here, so that `--tray` has none.
        .setup(move |app| {
            if tray_mode {
                tray::setup(app)
            } else {
                tauri::WebviewWindowBuilder::from_config(
                    app.handle(),
                    &app.config().app.windows[0],
                )?
                .build()?;
                Ok(())
            }
        })
        .manage(Mutex::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::get_platform,
            commands::detect_install,
            commands::default_name,
            commands::needs_insecure_confirm,
            commands::check_connection,
            commands::list_printers,
            commands::sumatra_present,
            commands::download_sumatra,
            commands::test_print,
            commands::apply,
            commands::wait_online,
            commands::uninstall,
            commands::read_status,
            commands::read_log,
            commands::apply_patch,
            commands::restart_agent,
            commands::header_values,
            updates::update_info,
            updates::check_update,
            updates::download_update,
            updates::install_update,
            updates::open_release_notes,
            updates::open_installed,
            updates::start_section,
        ])
        .build(tauri::generate_context!());
    let app = match built {
        Ok(app) => app,
        Err(e) => {
            fail(&format!("The setup window could not be opened: {e}"));
            return ExitCode::FAILURE;
        }
    };
    // ponytail: WebView2 leaves its profile (`%LOCALAPPDATA%\com.printpapi.setup\EBWebView`, about 9 MB, no
    // key in it) behind. It is runtime data, not staging, and msedgewebview2.exe still holds the
    // folder while the process ends, so it is not removed. Ceiling: a setup run again and again.
    // The process ends right after the exit event without dropping managed state, so the working
    // folder (it holds the unpacked agent) is removed here.
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            let state = handle.state::<Mutex<AppState>>();
            state.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
    });
    ExitCode::SUCCESS
}

/// Say why the setup cannot start. A program without a console cannot say so on stderr, so on
/// Windows a message box says it, too.
fn fail(text: &str) {
    eprintln!("{text}");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |s: &str| -> Vec<u16> {
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let text = wide(text);
        let title = wide("printpapi Agent Setup");
        // SAFETY: both buffers are NUL-terminated and outlive the call; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_web_view_on_windows_is_named_before_the_window_is_built() {
        let problem = startup_problem(Err("no runtime".into()), true).expect("a problem");
        assert!(problem.starts_with("This setup needs the Microsoft Edge WebView2 Runtime."));
        assert!(
            problem
                .ends_with("Runtime.\n\nhttps://developer.microsoft.com/microsoft-edge/webview2/")
        );
        assert_eq!(startup_problem(Ok("130.0".into()), true), None);
        // elsewhere the web view is part of the system or the program does not start at all
        assert_eq!(startup_problem(Err("x".into()), false), None);
    }

    fn words(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn version_is_exactly_one_argument_and_comes_before_everything() {
        assert_eq!(mode(&words(&["--version"])), Mode::Version);
        for not in [
            &["--version", "--tray"][..],
            &["--tray", "--version"],
            &["--version", "x"],
            &["-V"],
            &["--VERSION"],
        ] {
            assert_ne!(mode(&words(not)), Mode::Version, "{not:?}");
        }
    }

    #[test]
    fn tray_is_any_argument_tray() {
        assert_eq!(mode(&words(&["--tray"])), Mode::Tray);
        assert_eq!(mode(&words(&["--x", "--tray"])), Mode::Tray);
        assert_eq!(
            mode(&words(&["--tray", "--section", "updates"])),
            Mode::Tray
        );
    }

    #[test]
    fn a_window_opens_on_a_known_section_and_ignores_the_rest() {
        assert_eq!(mode(&[]), Mode::Window { section: None });
        assert_eq!(
            mode(&words(&["--section", "updates"])),
            Mode::Window {
                section: Some("updates")
            }
        );
        assert_eq!(
            mode(&words(&["x", "--section", "log"])),
            Mode::Window {
                section: Some("log")
            }
        );
        for unknown in [
            &["--section", "remove"][..],
            &["--section"],
            &["--section", ""],
            &["updates"],
            &["--section", "UPDATES"],
        ] {
            assert_eq!(
                mode(&words(unknown)),
                Mode::Window { section: None },
                "{unknown:?}"
            );
        }
    }

    /// The setup and the agent are released together: one number.
    #[test]
    fn version_matches_workspace() {
        let root = include_str!("../../../Cargo.toml");
        let own = include_str!("../Cargo.toml");
        let version = |text: &str, section: &str| -> String {
            let body = text.split(&format!("[{section}]")).nth(1).expect("section");
            let body = body.split("\n[").next().unwrap();
            let line = body
                .lines()
                .find(|l| l.trim_start().starts_with("version"))
                .expect("a version line");
            line.split('"').nth(1).expect("quoted version").to_string()
        };
        assert_eq!(version(own, "package"), version(root, "workspace.package"));
    }
}
