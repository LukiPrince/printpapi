// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// No console window next to the setup window in a release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod brand;
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
use printpapi_agent::pair::{self, Pairing};

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
    /// The settings window or the setup, opened on a section when `--section <word>` names one, or
    /// with a pairing link: exactly one argument, and one that `pair::parse` reads as a link with a
    /// server (that is how Windows and Linux hand a `printpapi://` link over).
    Window {
        section: Option<&'static str>,
        link: Option<Pairing>,
    },
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
    // Split, quoted or injected arguments are no link: a link is the one and only argument.
    // ponytail: on Windows and Linux (and from the macOS icon) the one-time code stands in the
    // window's command line, because that is how the systems deliver a URL. Another user of the
    // machine could read it and spend it first; it works once, for 30 minutes, gives an agent-only
    // key, and the dashboard shows that a key was fetched, with a Revoke button. The API key is never
    // in an argument. Ceiling: the code is only as private as the process list of this computer.
    let link = match args {
        [one] => one
            .to_str()
            .and_then(pair::parse)
            .filter(|p| p.server.is_some()),
        _ => None,
    };
    Mode::Window { section, link }
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
    let mut start_link = None;
    if let Mode::Window { section, link } = &mode {
        updates::set_start_section(*section);
        start_link = link.clone();
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
                open_window(app)
            }
        })
        .manage(Mutex::new(AppState::with_link(start_link)))
        .invoke_handler(tauri::generate_handler![
            brand::get_brand,
            brand::brand_icon,
            brand::license_text,
            brand::open_support,
            brand::show_window,
            commands::get_platform,
            commands::detect_install,
            commands::default_name,
            commands::needs_insecure_confirm,
            commands::check_connection,
            commands::take_link,
            commands::read_pairing,
            commands::pair,
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
    app.run(move |handle, event| match event {
        RunEvent::Exit => {
            let state = handle.state::<Mutex<AppState>>();
            state.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
        // macOS opens the running program with a link (`CFBundleURLTypes` of `Info.plist`): the
        // window keeps it for its next look, the icon starts a window of its own with it.
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => {
            let first = urls
                .iter()
                .filter_map(|u| pair::parse(u.as_str()))
                .find(|p| p.server.is_some());
            if let Some(p) = first {
                if tray_mode {
                    tray::open_with_link(&pair::link(
                        p.server.as_deref().unwrap_or_default(),
                        &p.code,
                    ));
                } else {
                    let state = handle.state::<Mutex<AppState>>();
                    state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .set_link(p);
                }
            }
        }
        _ => {}
    });
    ExitCode::SUCCESS
}

/// How long a branded window may stay hidden. It is shown as soon as the page has rendered the
/// brand; this is for a bundle that never gets there, so that the window is never lost.
const SHOW_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(5);

/// Make the window. In a branded build it starts hidden: the page the window loads is the stock one
/// until it has asked for the brand, and that must not be seen first (the page shows the window
/// itself, `brand::show_window`). The stock build shows it at once, as it always did.
fn open_window(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let brand = brand::current();
    let window =
        tauri::WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
            .title(brand::window_title(brand))
            .visible(brand.is_none())
            .build()?;
    if brand.is_some() {
        std::thread::spawn(move || {
            std::thread::sleep(SHOW_BACKSTOP);
            let _ = window.show();
        });
    }
    Ok(())
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
        let title = wide(brand::window_title(brand::current()));
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
        let plain = |section| Mode::Window {
            section,
            link: None,
        };
        assert_eq!(mode(&[]), plain(None));
        assert_eq!(
            mode(&words(&["--section", "updates"])),
            plain(Some("updates"))
        );
        assert_eq!(mode(&words(&["x", "--section", "log"])), plain(Some("log")));
        for unknown in [
            &["--section", "remove"][..],
            &["--section"],
            &["--section", ""],
            &["updates"],
            &["--section", "UPDATES"],
        ] {
            assert_eq!(mode(&words(unknown)), plain(None), "{unknown:?}");
        }
    }

    const LINK: &str =
        "printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT";

    #[test]
    fn exactly_one_link_argument_opens_the_window_with_it() {
        let Mode::Window { section, link } = mode(&words(&[LINK])) else {
            panic!("a window");
        };
        assert_eq!(section, None);
        let link = link.expect("the link");
        assert_eq!(link.server.as_deref(), Some("https://print.example.com"));
        assert_eq!(link.code, "7K3M-Q2ZD-9XHT");
        // the same words with the section rule are not a link
        assert!(matches!(
            mode(&words(&["--section", "log"])),
            Mode::Window { link: None, .. }
        ));
    }

    #[test]
    fn split_quoted_or_injected_arguments_are_no_link() {
        for args in [
            &[LINK, "--section", "log"][..],
            &["--section", "log", LINK],
            &[LINK, LINK],
            &[
                "printpapi://pair?server=https%3A%2F%2Fprint.example.com",
                "&code=7K3M-Q2ZD-9XHT",
            ],
            // quoted by the caller: the quote is part of the argument
            &["\"printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT\""],
            &["printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT --x"],
            // a bare code or an address alone has no server to show
            &["7K3M-Q2ZD-9XHT"],
            &["https://print.example.com"],
            &[""],
        ] {
            assert!(
                matches!(mode(&words(args)), Mode::Window { link: None, .. }),
                "{args:?}"
            );
        }
    }

    #[test]
    fn a_link_never_beats_the_tray_or_the_version() {
        assert_eq!(mode(&words(&["--tray"])), Mode::Tray);
        assert_eq!(mode(&words(&["--version"])), Mode::Version);
        assert_eq!(mode(&words(&[LINK, "--tray"])), Mode::Tray);
    }

    #[cfg(windows)]
    #[test]
    fn an_argument_that_is_no_unicode_does_not_panic() {
        use std::os::windows::ffi::OsStringExt;
        let lone_surrogate = OsString::from_wide(&[0x70, 0xD800, 0x71]);
        assert_eq!(
            mode(&[lone_surrogate]),
            Mode::Window {
                section: None,
                link: None
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_argument_that_is_no_unicode_does_not_panic() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'p', 0xFF, 0xFE]);
        assert_eq!(
            mode(&[bad]),
            Mode::Window {
                section: None,
                link: None
            }
        );
    }

    /// The scheme the bundle registers on macOS is the one the link parser reads.
    #[test]
    fn info_plist_names_the_scheme_of_the_link_parser() {
        let plist = include_str!("../Info.plist");
        assert!(plist.contains("CFBundleURLTypes"));
        assert!(
            plist.contains(&format!("<string>{}</string>", pair::SCHEME)),
            "{plist}"
        );
        assert!(plist.contains("<string>com.printpapi.setup</string>"));
        assert!(plist.contains("<string>Editor</string>"));
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
