// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The `--managed-*` modes are dispatched before the tray lock and the web view check: without a
//! path the program says so and exits 2 within seconds, on every system, and never opens a window.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// A folder of its own for the log of one call. A wrong call is logged on Windows, and without
/// this the test would append to the `printpapi-managed.log` of whoever runs it (the variable is
/// read by debug builds only, which is what `cargo test` builds).
struct LogDir(PathBuf);

impl LogDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "printpapi-managed-cli-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        LogDir(dir)
    }
}

impl Drop for LogDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The exit code and what the program wrote to stderr; `None` when it did not end in 30 s.
fn run(args: &[&str]) -> Option<(i32, String)> {
    run_logging_to(args, &LogDir::new().0)
}

fn run_logging_to(args: &[&str], log_dir: &Path) -> Option<(i32, String)> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_printpapi-setup"))
        .args(args)
        .env("PRINTPAPI_MANAGED_LOG_DIR", log_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the setup program starts");
    // the pipe is read on a thread, so that a program that fills it still ends
    let mut pipe = child.stderr.take().expect("a pipe");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    });
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        if let Some(status) = child.try_wait().expect("wait") {
            return status
                .code()
                .map(|c| (c, reader.join().unwrap_or_default()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn exit_code(args: &[&str]) -> Option<i32> {
    run(args).map(|(code, _)| code)
}

#[test]
fn an_install_without_a_path_is_a_wrong_call_and_ends_at_once() {
    assert_eq!(exit_code(&["--managed-install"]), Some(2));
}

#[test]
fn a_relative_path_and_an_unknown_mode_are_wrong_calls_too() {
    assert_eq!(exit_code(&["--managed-install", "relative.ini"]), Some(2));
    assert_eq!(exit_code(&["--managed-nope"]), Some(2));
    // before the tray: a --tray next to it does not start an icon
    assert_eq!(exit_code(&["--tray", "--managed-install"]), Some(2));
}

#[test]
fn a_wrong_call_says_how_to_call_it() {
    // the usage line is what spec test 3 asks for: deleting it must turn a test red
    for args in [
        &["--managed-install"][..],
        &["--managed-install", "relative.ini"],
        &["--managed-nope"],
    ] {
        let (code, stderr) = run(args).expect("ends");
        assert_eq!(code, 2, "{args:?}");
        assert!(
            stderr.starts_with("usage: printpapi-setup --managed-install"),
            "{args:?}: {stderr:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn a_wrong_call_leaves_its_line_in_the_log_folder_the_test_names() {
    let log = LogDir::new();
    let (code, _) = run_logging_to(&["--managed-install", "relative.ini"], &log.0).expect("ends");
    assert_eq!(code, 2);
    let text = std::fs::read_to_string(log.0.join("printpapi-managed.log")).expect("a log line");
    assert!(
        text.contains("managed: usage: printpapi-setup --managed-install"),
        "{text:?}"
    );
    assert_eq!(text.lines().count(), 1, "{text:?}");
}

#[cfg(not(windows))]
#[test]
fn off_windows_every_mode_prints_the_usage_and_exits_2() {
    assert_eq!(exit_code(&["--managed-refresh"]), Some(2));
    assert_eq!(exit_code(&["--managed-uninstall"]), Some(2));
    assert_eq!(
        exit_code(&["--managed-install", "/etc/printpapi.ini"]),
        Some(2)
    );
}
