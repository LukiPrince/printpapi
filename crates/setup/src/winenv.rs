// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! What this program may believe when it runs as LocalSystem (Windows).
//!
//! Windows Installer starts the headless modes as LocalSystem, but with the environment of the person
//! who started the installation: on a runner the local profile folder that Windows named was that
//! person's. Nothing that SYSTEM runs or reads back may be made in a folder of theirs, and no variable
//! of theirs (a proxy, a temp folder, a computer name) may reach the agent.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf, Prefix};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WinLocalSystemSid,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW;
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct Token(HANDLE);

impl Token {
    fn own() -> io::Result<Token> {
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo handle of this process needs no close; `handle` is only used on success
        let ok = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &mut handle,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Token(handle))
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful OpenProcessToken and is closed exactly once
        unsafe { CloseHandle(self.0) };
    }
}

/// Is this process LocalSystem? Asked of its token, not of a name in the environment.
/// https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-iswellknownsid
pub fn is_local_system() -> io::Result<bool> {
    let token = Token::own()?;
    // u64s: TOKEN_USER holds a pointer and has to be aligned for it; the SID (at most 68 bytes) is
    // written behind it in the same buffer
    let mut buf = [0u64; 32];
    let mut used = 0u32;
    // SAFETY: the length passed is the size of the buffer; the result is read as TOKEN_USER only
    // when the call succeeded, and its SID points into the buffer, which is still alive
    unsafe {
        if GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            &mut used,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        Ok(IsWellKnownSid(user.User.Sid, WinLocalSystemSid) != 0)
    }
}

/// The variables that belong to the account of this process: the machine's and its own. `bInherit` is
/// FALSE, but that is not enough: measured, the block still takes `SystemRoot` and `SystemDrive` from the
/// calling process and builds `ProgramData`, `ComSpec`, `PUBLIC` and others from them, which the page below
/// does not say. So the caller sets those first ([`take_own_environment`]).
/// https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-createenvironmentblock
pub fn own_environment() -> io::Result<Vec<(OsString, OsString)>> {
    let token = Token::own()?;
    let mut block: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: the block is read only after success, up to its double NUL, and freed once
    unsafe {
        if CreateEnvironmentBlock(&mut block, token.0, 0) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut at = block.cast::<u16>().cast_const();
        let mut vars = Vec::new();
        while *at != 0 {
            let len = (0..).take_while(|&n| *at.add(n) != 0).count();
            vars.extend(split(std::slice::from_raw_parts(at, len)));
            at = at.add(len + 1);
        }
        DestroyEnvironmentBlock(block);
        Ok(vars)
    }
}

/// `NAME=value`. A name that starts with `=` (`=C:=C:\\dir`, the current folder of a drive) is not a
/// variable anyone set.
fn split(entry: &[u16]) -> Option<(OsString, OsString)> {
    let eq = entry.iter().position(|&c| c == u16::from(b'='))?;
    if eq == 0 {
        return None;
    }
    Some((
        OsString::from_wide(&entry[..eq]),
        OsString::from_wide(&entry[eq + 1..]),
    ))
}

/// Replace the whole environment of this process by `vars`.
///
/// # Safety
/// No other thread may be running (`std::env::set_var`).
unsafe fn replace(vars: &[(OsString, OsString)]) {
    let old: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
    for name in old.iter().filter(|n| !starts_with_eq(n)) {
        // SAFETY: the caller's promise
        unsafe { std::env::remove_var(name) };
    }
    for (name, value) in vars {
        // SAFETY: the caller's promise
        unsafe { std::env::set_var(name, value) };
    }
}

fn starts_with_eq(name: &OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b"=")
}

/// `C:\Windows`, asked of Windows and not of a variable.
/// https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getsystemwindowsdirectoryw
fn windows_dir() -> io::Result<PathBuf> {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer is as long as the length passed
    let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    match n {
        0 => Err(io::Error::last_os_error()),
        n if n < buf.len() => Ok(PathBuf::from(OsString::from_wide(&buf[..n]))),
        _ => Err(io::Error::other(
            "the path of the Windows folder is too long",
        )),
    }
}

/// `C:` of `C:\Windows`.
fn drive_of(path: &Path) -> Option<PathBuf> {
    match path.components().next()? {
        Component::Prefix(p) if matches!(p.kind(), Prefix::Disk(_)) => {
            Some(PathBuf::from(p.as_os_str()))
        }
        _ => None,
    }
}

/// Take the environment of this process's own account, work in the system folder, and look for no DLL
/// in the current one. Children inherit all three (for the DLL order see the note of
/// https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-setdlldirectoryw).
///
/// # Safety
/// No other thread may be running (see [`replace`]).
unsafe fn take_own_environment() -> io::Result<()> {
    let windows = windows_dir()?;
    let drive =
        drive_of(&windows).ok_or_else(|| io::Error::other("the Windows folder is on no drive"))?;
    let fixed = [
        ("SystemRoot", &windows),
        ("windir", &windows),
        ("SystemDrive", &drive),
    ];
    for (name, value) in fixed {
        // SAFETY: the caller's promise
        unsafe { std::env::set_var(name, value) };
    }
    let vars = own_environment()?;
    // SAFETY: the caller's promise
    unsafe { replace(&vars) };
    for (name, value) in fixed {
        let now = std::env::var_os(name).unwrap_or_default();
        if !now.eq_ignore_ascii_case(value) {
            return Err(io::Error::other(format!(
                "{name} is not {}",
                value.display()
            )));
        }
    }
    std::env::set_current_dir(printpapi_agent::install::system_dir())?;
    // SAFETY: an empty NUL-terminated wide string
    if unsafe { SetDllDirectoryW([0u16].as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// As LocalSystem, leave what the starter handed over: the environment and the current folder. For any
/// other account nothing changes: it is that person's own program.
///
/// # Safety
/// No other thread may be running (see [`replace`]).
pub unsafe fn leave_the_starters_environment() -> io::Result<()> {
    if is_local_system()? {
        // SAFETY: the caller's promise
        unsafe { take_own_environment() }?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_run_is_not_local_system() {
        assert!(!is_local_system().unwrap());
    }

    #[test]
    fn entries_are_split_at_the_first_equals_sign_and_drive_entries_are_left_out() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(split(&w("A=b=c")), Some(("A".into(), "b=c".into())));
        assert_eq!(split(&w("EMPTY=")), Some(("EMPTY".into(), "".into())));
        assert_eq!(split(&w("=C:=C:\\dir")), None);
        assert_eq!(split(&w("no equals sign")), None);
    }

    /// Run in a process of its own (the test below): replacing the environment races with every
    /// other test.
    #[test]
    #[ignore = "started by the_environment_of_the_starter_is_gone_after_the_replacement"]
    fn child_replaces_its_environment() {
        for name in PLANTED {
            assert_eq!(std::env::var_os(name), Some("Z:\\planted".into()), "{name}");
        }
        // SAFETY: --test-threads=1 and nothing else runs in this process
        unsafe { take_own_environment() }.unwrap();
        for (name, value) in std::env::vars_os() {
            assert!(
                !value.to_string_lossy().to_lowercase().contains("planted"),
                "{name:?}={value:?}"
            );
        }
        assert!(std::env::var_os("PRINTPAPI_PLANTED").is_none());
        let windows = windows_dir().unwrap();
        let same = |name: &str, want: &Path| {
            let got = std::env::var_os(name).unwrap_or_default();
            assert!(
                got.eq_ignore_ascii_case(want.as_os_str()),
                "{name}: {got:?}"
            );
        };
        same("SystemRoot", &windows);
        same("windir", &windows);
        same("SystemDrive", &drive_of(&windows).unwrap());
        for name in ["COMPUTERNAME", "Path", "LOCALAPPDATA"] {
            assert!(std::env::var_os(name).is_some(), "{name} is missing");
        }
        assert_eq!(
            std::env::current_dir().unwrap(),
            printpapi_agent::install::system_dir()
        );
        println!("child replaced");
    }

    /// CreateEnvironmentBlock was seen to take the first two from the caller, and to build the others
    /// from them, although it was asked not to inherit.
    const PLANTED: [&str; 8] = [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "ProgramData",
        "ComSpec",
        "LOCALAPPDATA",
        "TEMP",
        "PRINTPAPI_PLANTED",
    ];

    #[test]
    fn the_environment_of_the_starter_is_gone_after_the_replacement() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "winenv::tests::child_replaces_its_environment",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(std::env::temp_dir());
        for name in PLANTED {
            child.env(name, "Z:\\planted");
        }
        let out = child.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success() && text.contains("child replaced"),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn the_drive_of_the_windows_folder() {
        assert_eq!(
            drive_of(Path::new("C:\\Windows")),
            Some(PathBuf::from("C:"))
        );
        assert_eq!(drive_of(Path::new("\\\\server\\share")), None);
    }
}
