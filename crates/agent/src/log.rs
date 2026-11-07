// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The crash log: one line per event in `print_agent-error.log`. A background run has no console,
//! so this file is what an operator reads over remote desktop.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Python wrote the log in text mode, so on Windows every line ended in CRLF (old Notepad needs it).
const EOL: &str = if cfg!(windows) { "\r\n" } else { "\n" };
const LOG_NAME: &str = "print_agent-error.log";

/// `%LOCALAPPDATA%` when set and non-empty, else the temp dir.
pub fn log_dir(local_app_data: Option<OsString>) -> PathBuf {
    match local_app_data {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => std::env::temp_dir(),
    }
}

/// Append `<local time> <msg>` to the log in `dir`; write errors are ignored (a missing or
/// read-only folder must never crash the caller).
pub fn append_log(dir: &Path, msg: &str) {
    let line = format!(
        "{} {msg}{EOL}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG_NAME))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Where `log_error` writes: the folder set by [`set_log_dir`], else [`log_dir`].
fn target_dir(set: Option<&PathBuf>, local_app_data: Option<OsString>) -> PathBuf {
    match set {
        Some(d) => d.clone(),
        None => log_dir(local_app_data),
    }
}

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Send `log_error` to `dir` from now on (once per process; a later call changes nothing). A
/// setup-installed agent runs as SYSTEM or root, and this puts the log next to the status file
/// where the tray icon can show it, instead of in the profile of SYSTEM.
pub fn set_log_dir(dir: PathBuf) {
    let _ = LOG_DIR.set(dir);
}

/// Append one line to the crash log.
pub fn log_error(msg: &str) {
    append_log(
        &target_dir(LOG_DIR.get(), std::env::var_os("LOCALAPPDATA")),
        msg,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_error_appends_to_the_crash_log() {
        let dir = tempfile::tempdir().unwrap();
        append_log(dir.path(), "register failed (try 1)");
        append_log(dir.path(), "register failed (try 2)");
        let text = std::fs::read_to_string(dir.path().join("print_agent-error.log")).unwrap();
        assert!(text.contains("try 1") && text.contains("try 2"));
        // "%Y-%m-%d %H:%M:%S <msg>"
        let first = text.lines().next().unwrap();
        let (stamp, msg) = first.split_at(19);
        assert!(
            chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").is_ok(),
            "{first}"
        );
        assert_eq!(msg, " register failed (try 1)");
    }

    #[test]
    fn log_lines_end_the_platform_way() {
        let dir = tempfile::tempdir().unwrap();
        append_log(dir.path(), "x");
        let raw = std::fs::read(dir.path().join("print_agent-error.log")).unwrap();
        assert!(raw.ends_with(if cfg!(windows) { b" x\r\n" } else { b" x\n" }));
    }

    #[test]
    fn test_log_error_falls_back_to_the_tempdir_without_localappdata() {
        assert_eq!(log_dir(None), std::env::temp_dir());
        assert_eq!(log_dir(Some(OsString::new())), std::env::temp_dir());
        assert_eq!(log_dir(Some("C:/x".into())), PathBuf::from("C:/x"));
    }

    #[test]
    fn log_goes_to_the_set_dir() {
        let set = PathBuf::from("/var/lib/printpapi");
        // a directory set by a setup-installed agent wins over %LOCALAPPDATA% (SYSTEM's profile)
        assert_eq!(target_dir(Some(&set), Some("C:/x".into())), set);
        assert_eq!(target_dir(None, Some("C:/x".into())), PathBuf::from("C:/x"));
        // and the real thing end to end; the setting is once per process, so it is the only
        // test that sets it
        let dir = tempfile::tempdir().unwrap();
        set_log_dir(dir.path().to_path_buf());
        log_error("into the set dir");
        let text = std::fs::read_to_string(dir.path().join("print_agent-error.log")).unwrap();
        assert!(text.contains("into the set dir"));
    }

    #[test]
    fn test_log_error_swallows_a_missing_log_directory() {
        let dir = tempfile::tempdir().unwrap();
        append_log(&dir.path().join("does-not-exist"), "should not raise");
    }
}
