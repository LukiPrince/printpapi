// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Running the agent's admin subcommands with administrator rights: UAC on Windows, an
//! `osascript` password dialog on macOS, `pkexec` on Linux. The arguments are only ever a
//! subcommand and the path of the private staging folder, never the key.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

/// Why the elevated run did not finish.
#[derive(Debug, Clone, PartialEq)]
pub enum Elevate {
    /// The person closed the prompt. Nothing has happened.
    Cancelled,
    Failed(String),
}

/// One argument as `CommandLineToArgvW` (and Rust's `std`, which the agent is built with) reads it
/// back: quoted when it holds a blank, a tab or a quote, quotes escaped, backslashes before a quote
/// or before the closing quote doubled.
// ponytail: works on the lossy UTF-8 text of the argument. The arguments are a subcommand and a
// temp folder; a path with a lone UTF-16 surrogate would need `OsStrExt::encode_wide` instead.
// Pure text, so it is tested on every host; only the Windows branch of `run` calls it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_quote(arg: &OsStr) -> OsString {
    let text = arg.to_string_lossy();
    if !text.is_empty() && !text.contains([' ', '\t', '"']) {
        return arg.to_os_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in text.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    OsString::from(out)
}

/// `'word'` for a POSIX shell: an apostrophe inside becomes `'\''`.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The AppleScript for `osascript -e`: every word shell-quoted with single quotes, then the whole
/// command escaped for the AppleScript string it sits in.
pub fn applescript(argv: &[&str]) -> String {
    let command = argv
        .iter()
        .map(|w| shell_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{escaped}\" with administrator privileges")
}

/// `pkexec` exit codes (pkexec(1), "RETURN VALUE",
/// https://www.freedesktop.org/software/polkit/docs/latest/pkexec.1.html): 126 is the dialog
/// dismissed, 127 is "not authorised, the login failed, or another error". Both read as a refusal
/// here, so a 127 that has another cause is reported to the person as "not granted" as well.
pub fn pkexec_result(code: Option<i32>) -> Result<(), Elevate> {
    match code {
        Some(0) => Ok(()),
        Some(126 | 127) => Err(Elevate::Cancelled),
        Some(c) => Err(Elevate::Failed(format!("the agent exited with code {c}"))),
        None => Err(Elevate::Failed("the agent was stopped by a signal".into())),
    }
}

/// `osascript` reports a dismissed password dialog as "User canceled. (-128)".
fn osascript_result(code: Option<i32>, stderr: &str) -> Result<(), Elevate> {
    match code {
        Some(0) => Ok(()),
        // the form osascript prints; a bare "-128" could be part of a path in an error text
        _ if stderr.contains("(-128)") => Err(Elevate::Cancelled),
        Some(c) => Err(Elevate::Failed(format!("osascript exited with code {c}"))),
        None => Err(Elevate::Failed("osascript was stopped by a signal".into())),
    }
}

/// Run `agent args...` with administrator rights and wait for it. `Ok` only when it exited 0; what
/// went wrong inside is in `result.json` in the staging folder.
pub fn run(agent: &Path, args: &[&OsStr]) -> Result<(), Elevate> {
    if cfg!(windows) {
        run_windows(agent, args)
    } else if cfg!(target_os = "macos") {
        let mut argv = vec![agent.to_string_lossy().into_owned()];
        argv.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let out = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(applescript(&argv))
            .output()
            .map_err(|e| Elevate::Failed(format!("cannot start osascript: {e}")))?;
        osascript_result(out.status.code(), &String::from_utf8_lossy(&out.stderr))
    } else {
        let status = Command::new("pkexec")
            .arg(agent)
            .args(args)
            .status()
            .map_err(|e| Elevate::Failed(format!("cannot start pkexec: {e}")))?;
        pkexec_result(status.code())
    }
}

#[cfg(not(windows))]
fn run_windows(_: &Path, _: &[&OsStr]) -> Result<(), Elevate> {
    Err(Elevate::Failed("not a Windows machine".into()))
}

/// `ShellExecuteExW` with the verb `runas` shows the UAC prompt; the process is waited for through
/// its handle. A dismissed prompt is `ERROR_CANCELLED`.
/// https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw
#[cfg(windows)]
fn run_windows(agent: &Path, args: &[&OsStr]) -> Result<(), Elevate> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let wide = |s: &OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let parameters = args
        .iter()
        .map(|a| windows_quote(a))
        .collect::<Vec<_>>()
        .join(OsStr::new(" "));
    let verb = wide(OsStr::new("runas"));
    let file = wide(agent.as_os_str());
    let parameters = wide(&parameters);

    // SAFETY: `info` is a zeroed SHELLEXECUTEINFOW with its size and the pointers to the three
    // NUL-terminated buffers above, which outlive the call; the process handle it returns is
    // closed below.
    unsafe {
        // ShellExecuteEx may hand work to COM shell extensions; Microsoft advises initialising COM
        // first. A thread that already has another apartment answers RPC_E_CHANGED_MODE (negative),
        // which is fine and must not be balanced by CoUninitialize.
        let com = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.nShow = SW_HIDE;
        let started = ShellExecuteExW(&mut info) != 0;
        let error = GetLastError();
        let result = if !started {
            if error == ERROR_CANCELLED {
                Err(Elevate::Cancelled)
            } else {
                Err(Elevate::Failed(format!(
                    "the administrator prompt failed (Windows error {error})"
                )))
            }
        } else if info.hProcess.is_null() {
            Err(Elevate::Failed("no process to wait for".into()))
        } else {
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            let got = GetExitCodeProcess(info.hProcess, &mut code) != 0;
            CloseHandle(info.hProcess);
            match (got, code) {
                (true, 0) => Ok(()),
                (true, c) => Err(Elevate::Failed(format!("the agent exited with code {c}"))),
                (false, _) => Err(Elevate::Failed("cannot read the agent's exit code".into())),
            }
        };
        if com >= 0 {
            CoUninitialize();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_quote_wraps_a_path_with_blanks_and_umlauts() {
        assert_eq!(
            windows_quote(OsStr::new("C:\\Users\\Jürgen Müller\\x")),
            OsString::from("\"C:\\Users\\Jürgen Müller\\x\"")
        );
    }

    #[test]
    fn windows_quote_doubles_the_backslashes_before_the_closing_quote() {
        assert_eq!(
            windows_quote(OsStr::new("C:\\a b\\")),
            OsString::from("\"C:\\a b\\\\\"")
        );
    }

    #[test]
    fn windows_quote_escapes_quotes_and_leaves_plain_words_alone() {
        assert_eq!(
            windows_quote(OsStr::new("install")),
            OsString::from("install")
        );
        assert_eq!(windows_quote(OsStr::new("")), OsString::from("\"\""));
        assert_eq!(
            windows_quote(OsStr::new("a\"b c")),
            OsString::from("\"a\\\"b c\"")
        );
        assert_eq!(
            windows_quote(OsStr::new("a\\\"b c")),
            OsString::from("\"a\\\\\\\"b c\"")
        );
        // backslashes that are not followed by a quote stay as they are
        assert_eq!(
            windows_quote(OsStr::new("C:\\temp\\x")),
            OsString::from("C:\\temp\\x")
        );
    }

    #[test]
    fn applescript_quotes_a_path_with_blank_umlaut_and_apostrophe() {
        let script = applescript(&[
            "/Users/Jürgen Müller/O'Neil/printpapi-agent",
            "install",
            "/var/folders/x y/T/staging",
        ]);
        // each word on its own in single quotes, an apostrophe as '\'' ; the backslash is then
        // doubled for the AppleScript string
        assert_eq!(
            script,
            "do shell script \"'/Users/Jürgen Müller/O'\\\\''Neil/printpapi-agent' 'install' \
             '/var/folders/x y/T/staging'\" with administrator privileges"
        );
        assert!(script.ends_with("with administrator privileges"));
    }

    #[test]
    fn applescript_escapes_double_quotes_and_backslashes() {
        let script = applescript(&["/tmp/a\"b\\c", "install"]);
        assert_eq!(
            script,
            "do shell script \"'/tmp/a\\\"b\\\\c' 'install'\" with administrator privileges"
        );
    }

    #[test]
    fn osascript_reports_a_dismissed_dialog_as_cancelled() {
        assert_eq!(osascript_result(Some(0), ""), Ok(()));
        assert_eq!(
            osascript_result(
                Some(1),
                "execution error: User canceled. (-128)
"
            ),
            Err(Elevate::Cancelled)
        );
        assert!(matches!(
            osascript_result(Some(1), "execution error: no such file (-43)"),
            Err(Elevate::Failed(_))
        ));
        // a path that happens to contain 128 is not a cancellation
        assert!(matches!(
            osascript_result(
                Some(1),
                "cannot open /var/folders/printpapi-setup-128abc/agent"
            ),
            Err(Elevate::Failed(_))
        ));
        assert!(matches!(
            osascript_result(None, ""),
            Err(Elevate::Failed(_))
        ));
    }

    /// What the agent (a Rust program) makes of the command line `run_windows` builds, read back
    /// with the same Win32 parser the C runtime follows.
    #[cfg(windows)]
    #[test]
    fn a_staging_path_survives_the_command_line_as_one_argument() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

        let read_back = |arg: &str| -> Vec<OsString> {
            let line: Vec<u16> = OsStr::new("agent.exe install ")
                .encode_wide()
                .chain(windows_quote(OsStr::new(arg)).encode_wide())
                .chain(std::iter::once(0))
                .collect();
            let mut count = 0;
            // SAFETY: `line` is NUL-terminated; the array it returns holds `count` NUL-terminated
            // strings and is freed once with LocalFree, as the documentation requires.
            unsafe {
                let argv = CommandLineToArgvW(line.as_ptr(), &mut count);
                assert!(!argv.is_null());
                let out = (0..count as usize)
                    .map(|i| {
                        let p = *argv.add(i);
                        let len = (0..).take_while(|&n| *p.add(n) != 0).count();
                        OsString::from_wide(std::slice::from_raw_parts(p, len))
                    })
                    .collect();
                LocalFree(argv as _);
                out
            }
        };
        for path in [
            r"C:\Users\Jürgen Müller\AppData\Local\Temp\printpapi-setup-x1",
            r"C:\a b\",
            r#"C:\with "quote" and \\"#,
            r"C:\plain\path",
        ] {
            assert_eq!(
                read_back(path),
                [
                    OsString::from("agent.exe"),
                    OsString::from("install"),
                    OsString::from(path)
                ],
                "{path}"
            );
        }
    }

    #[test]
    fn pkexec_exit_codes() {
        assert_eq!(pkexec_result(Some(0)), Ok(()));
        assert_eq!(pkexec_result(Some(126)), Err(Elevate::Cancelled));
        assert_eq!(pkexec_result(Some(127)), Err(Elevate::Cancelled));
        assert!(matches!(pkexec_result(Some(1)), Err(Elevate::Failed(_))));
        assert!(matches!(pkexec_result(None), Err(Elevate::Failed(_))));
    }
}
