// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// No console window next to the setup window in a release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod elevate;
mod staging;

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

fn main() -> ExitCode {
    let webview = tauri::webview_version().map_err(|e| e.to_string());
    if let Some(problem) = startup_problem(webview, cfg!(windows)) {
        fail(problem);
        return ExitCode::FAILURE;
    }
    let built = tauri::Builder::default()
        .manage(Mutex::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::get_platform,
            commands::detect_install,
            commands::default_name,
            commands::needs_insecure_confirm,
            commands::check_connection,
            commands::list_printers,
            commands::read_existing,
            commands::sumatra_present,
            commands::download_sumatra,
            commands::test_print,
            commands::apply,
            commands::wait_online,
            commands::uninstall,
        ])
        .build(tauri::generate_context!());
    let app = match built {
        Ok(app) => app,
        Err(e) => {
            fail(&format!("The setup window could not be opened: {e}"));
            return ExitCode::FAILURE;
        }
    };
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
