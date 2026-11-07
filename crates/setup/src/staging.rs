// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The private working folder of one setup run: the unpacked agent, the `agent.ini` and the
//! `result.json` the elevated agent writes. It is gone again when the `Staging` is dropped.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The agent built for this target, put in by `build.rs` (`AGENT_BIN`).
const AGENT_BIN: &[u8] = include_bytes!(env!("AGENT_BIN"));
const AGENT_NAME: &str = if cfg!(windows) {
    "printpapi-agent.exe"
} else {
    "printpapi-agent"
};

pub struct Staging {
    dir: tempfile::TempDir,
}

impl Staging {
    pub fn new() -> io::Result<Staging> {
        Staging::in_root(private_root())
    }

    /// A folder with a random name under `root`. A `root` that could not be found is an error:
    /// there is no other place to fall back to that is as private.
    fn in_root(root: io::Result<PathBuf>) -> io::Result<Staging> {
        // `tempfile` takes the umask for the folder (0755 on Linux), so the owner-only mode is
        // asked for; on Windows the folder inherits the ACL of `root`.
        let mut builder = tempfile::Builder::new();
        builder.prefix("printpapi-setup-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        Ok(Staging {
            dir: builder.tempdir_in(root?)?,
        })
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Unpack the embedded agent binary into the folder and return its path.
    pub fn agent(&self) -> io::Result<PathBuf> {
        write_executable(self.dir(), AGENT_NAME, AGENT_BIN)
    }
}

/// Where the folder goes. Unix: the temp folder, which the owner-only mode of the folder protects.
// ponytail: `mode` is ignored on Windows, and a folder made there is only as private as its parent.
// `%TEMP%` can point to a shared folder (say C:\Temp) and the agent.ini in it holds the key, readable
// by other local users while the elevation prompt is open. So Windows asks for the user's local
// profile, which is private by ACL; if that fails, the setup fails too. The ceiling: a profile that
// is shared by policy (a redirected AppData) is as private as that share.
#[cfg(not(windows))]
fn private_root() -> io::Result<PathBuf> {
    Ok(std::env::temp_dir())
}

/// `%LOCALAPPDATA%` of the user, asked of Windows (`SHGetKnownFolderPath(FOLDERID_LocalAppData)`),
/// not of the environment.
/// https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath
#[cfg(windows)]
fn private_root() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};
    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: `raw` is only read when the call succeeded, as a null-terminated string that this
    // call allocated; it is freed once, whether or not the call succeeded (see the docs).
    let (hr, path) = unsafe {
        let hr = SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut raw);
        let path = (hr >= 0 && !raw.is_null()).then(|| {
            let len = (0..).take_while(|&n| *raw.add(n) != 0).count();
            PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
                raw, len,
            )))
        });
        CoTaskMemFree(raw.cast());
        (hr, path)
    };
    path.ok_or_else(|| {
        io::Error::other(format!(
            "Windows did not say where the local profile folder is (error {hr:#x})"
        ))
    })
}

/// `dir/name` with `bytes` as content, executable for its owner only.
pub fn write_executable(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    write_new(dir, name, bytes, 0o700)
}

/// `dir/name` with `bytes` as content, readable by its owner only (the file holds the key).
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    write_new(dir, name, bytes, 0o600)
}

/// A new file: an older entry of that name is removed first, and `create_new` refuses to open
/// anything that appeared meanwhile. `mode` only matters on Unix.
fn write_new(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> io::Result<PathBuf> {
    let path = dir.join(name);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    opts.open(&path)?.write_all(bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_cleans_up() {
        let staging = Staging::new().unwrap();
        let dir = staging.dir().to_path_buf();
        assert!(dir.is_dir());
        std::fs::write(dir.join("agent.ini"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        drop(staging);
        assert!(!dir.exists());
    }

    #[test]
    fn the_folder_is_made_under_the_given_root() {
        let root = tempfile::tempdir().unwrap();
        let staging = Staging::in_root(Ok(root.path().to_path_buf())).unwrap();
        assert_eq!(staging.dir().parent().unwrap(), root.path());
    }

    #[test]
    fn without_a_root_it_fails_instead_of_falling_back_to_temp() {
        let err = Staging::in_root(Err(io::Error::other("no profile"))).err();
        assert_eq!(err.unwrap().to_string(), "no profile");
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_folder_is_in_the_local_profile_not_in_temp() {
        let staging = Staging::new().unwrap();
        // what Windows says, and not %TEMP%, which a shared folder could be set as
        assert_eq!(staging.dir().parent().unwrap(), private_root().unwrap());
        let local = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap());
        assert_eq!(
            staging.dir().parent().unwrap().canonicalize().unwrap(),
            local.canonicalize().unwrap()
        );
    }

    #[test]
    fn two_stagings_are_two_folders() {
        let (a, b) = (Staging::new().unwrap(), Staging::new().unwrap());
        assert_ne!(a.dir(), b.dir());
    }

    #[test]
    fn the_agent_is_written_executable_for_its_owner_only() {
        let staging = Staging::new().unwrap();
        let path = write_executable(staging.dir(), "printpapi-agent", b"binary").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"binary");
        assert_eq!(path.parent().unwrap(), staging.dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn the_agent_can_be_unpacked_again_for_the_next_admin_step() {
        let staging = Staging::new().unwrap();
        let first = staging.agent().unwrap();
        assert_eq!(staging.agent().unwrap(), first);
    }

    #[test]
    fn a_private_file_replaces_an_older_one_and_is_owner_only() {
        let staging = Staging::new().unwrap();
        write_private(staging.dir(), "agent.ini", b"old").unwrap();
        let path = write_private(staging.dir(), "agent.ini", b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn the_embedded_agent_is_unpacked() {
        let staging = Staging::new().unwrap();
        let path = staging.agent().unwrap();
        assert!(path.starts_with(staging.dir()));
        assert!(std::fs::metadata(&path).unwrap().len() > 1000);
    }
}
