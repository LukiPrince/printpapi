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
        // A random name in the temp folder. `tempfile` takes the umask for the folder (0755 on
        // Linux), so the owner-only mode is asked for; on Windows the folder inherits the ACL of
        // the user's own temp folder.
        let mut builder = tempfile::Builder::new();
        builder.prefix("printpapi-setup-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        Ok(Staging {
            dir: builder.tempdir()?,
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
