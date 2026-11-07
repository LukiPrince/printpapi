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

fn main() -> ExitCode {
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
            window_failed(&e.to_string());
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

/// The window could not be made. On Windows that is almost always a missing WebView2 runtime
/// (https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution), and a
/// program without a console cannot say so on stderr.
fn window_failed(error: &str) {
    eprintln!("The setup window could not be opened: {error}");
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
        let text = wide(
            "This setup needs the Microsoft Edge WebView2 Runtime.\n\n\
             https://developer.microsoft.com/microsoft-edge/webview2/",
        );
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
