// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The marker file `.installed-by-setup`: its existence says "the setup installed this agent" (that has
//! not changed), its content says which version and, for a white-label build, which brand.
//!
//! Frozen format (AGENTS.md): UTF-8 lines `key=value`, LF, at most 4 KiB.
//!
//! ```text
//! version=3.7.0
//! brand=acme
//! ```
//!
//! An empty or missing file, which is what 3.3.0 to 3.6.0 wrote, means "version unknown, stock
//! brand". Unknown keys are kept when the version is rewritten, so a later release can add one.

use std::io::Read;
use std::path::Path;

pub use crate::brand::valid_brand_id;
use crate::update::Version;

/// More than this is not a marker.
pub const MAX_LEN: usize = 4096;

/// Whose build installed this agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrandMark {
    /// No `brand` line: the stock build.
    Stock,
    Brand(String),
    /// A file that cannot be read as a marker: nobody may assume a stock install.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// The version of the agent that was installed or last updated. `None`: unknown.
    pub version: Option<String>,
    pub brand: BrandMark,
}

/// Read the content of a marker. Never fails: what cannot be read is [`BrandMark::Unreadable`].
pub fn parse(bytes: &[u8]) -> Marker {
    let unreadable = Marker {
        version: None,
        brand: BrandMark::Unreadable,
    };
    if bytes.len() > MAX_LEN {
        return unreadable;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return unreadable;
    };
    let mut marker = Marker {
        version: None,
        brand: BrandMark::Stock,
    };
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return unreadable;
        };
        match key.trim() {
            // a value that is no N.N.N is "unknown", not a reason to distrust the file
            "version" => {
                marker.version = Version::parse(value.trim()).map(|v| v.to_string());
            }
            "brand" => {
                let id = value.trim();
                if !valid_brand_id(id) {
                    return unreadable;
                }
                marker.brand = BrandMark::Brand(id.to_string());
            }
            _ => {}
        }
    }
    marker
}

/// The text of `old_text` with its `version` line set to `version`, every other line kept
/// in its place. A marker without a version line gets one at the top. Line ends become LF.
pub fn with_version(old_text: &str, version: &str) -> String {
    let line = format!("version={version}");
    let mut out: Vec<String> = Vec::new();
    let mut done = false;
    for old in old_text.lines() {
        let old = old.trim_end_matches('\r');
        if old.trim().is_empty() {
            continue;
        }
        if old
            .split_once('=')
            .is_some_and(|(k, _)| k.trim() == "version")
        {
            if !done {
                out.push(line.clone());
                done = true;
            }
        } else {
            out.push(old.to_string());
        }
    }
    if !done {
        out.insert(0, line);
    }
    out.join("\n") + "\n"
}

/// The raw text of the marker at `path`: only a regular file (a link or a folder there is
/// not ours), at most [`MAX_LEN`] bytes plus one, so an oversized file is still seen as oversized.
pub fn read_text(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_LEN as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// The text of `old_text` with its `brand` line set to `id` (in place when there is one, else after
/// the other lines), every other line kept. Line ends become LF.
pub fn with_brand(old_text: &str, id: &str) -> String {
    let line = format!("brand={id}");
    let mut out: Vec<String> = Vec::new();
    let mut done = false;
    for old in old_text.lines() {
        let old = old.trim_end_matches('\r');
        if old.trim().is_empty() {
            continue;
        }
        if old
            .split_once('=')
            .is_some_and(|(k, _)| k.trim() == "brand")
        {
            if !done {
                out.push(line.clone());
                done = true;
            }
        } else {
            out.push(old.to_string());
        }
    }
    if !done {
        out.push(line);
    }
    out.join("\n") + "\n"
}

/// May a setup of the brand `this` (`None`: the stock build) install over an installation that
/// the marker says was made by `installed`? A stock installation can be taken over by any build
/// (adoption); a brand's installation only by the same brand; an unreadable marker by nobody.
/// Removing and restarting do not ask: a machine must always be cleanable.
pub fn may_replace(installed: &BrandMark, this: Option<&str>) -> bool {
    match installed {
        BrandMark::Stock => true,
        BrandMark::Brand(id) => this == Some(id.as_str()),
        BrandMark::Unreadable => false,
    }
}

/// The marker at `path`, `None` when there is no regular file.
pub fn read(path: &Path) -> Option<Marker> {
    read_text(path).map(|bytes| parse(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_marker_is_a_stock_install_of_unknown_version() {
        let m = parse(b"");
        assert_eq!(m.version, None);
        assert_eq!(m.brand, BrandMark::Stock);
    }

    #[test]
    fn version_and_brand_lines_are_read() {
        let m = parse(b"version=3.7.0\nbrand=acme\n");
        assert_eq!(m.version.as_deref(), Some("3.7.0"));
        assert_eq!(m.brand, BrandMark::Brand("acme".into()));
        // CRLF, blank lines, blanks around the equals sign and unknown keys are fine
        let m = parse(b"\r\nfuture = 1\r\nversion = 3.10.2\r\n");
        assert_eq!(m.version.as_deref(), Some("3.10.2"));
        assert_eq!(m.brand, BrandMark::Stock);
    }

    #[test]
    fn a_version_that_is_not_n_n_n_is_unknown_but_the_file_stays_readable() {
        for v in ["3.7", "v3.7.0", "3.7.0-rc1", "", "3.7.0.1"] {
            let m = parse(format!("version={v}\n").as_bytes());
            assert_eq!(m.version, None, "{v}");
            assert_eq!(m.brand, BrandMark::Stock, "{v}");
        }
    }

    #[test]
    fn what_cannot_be_read_is_unreadable_not_stock() {
        for bytes in [
            &b"\xff\xfe\x00"[..],
            b"version=3.7.0\nnonsense\n",
            b"brand=Acme\n",
            b"brand=printpapi\n",
            b"brand=stock\n",
            b"brand=a\n",
            b"brand=\n",
            b"brand=1abc\n",
        ] {
            assert_eq!(parse(bytes).brand, BrandMark::Unreadable, "{bytes:?}");
        }
        let long = vec![b'#'; MAX_LEN + 1];
        assert_eq!(parse(&long).brand, BrandMark::Unreadable);
        let ok = format!("version=3.7.0\n{}\n", "x=".to_string() + &"y".repeat(100));
        assert_eq!(parse(ok.as_bytes()).brand, BrandMark::Stock);
    }

    #[test]
    fn brand_ids_follow_the_rule() {
        for ok in ["ab", "acme", "a-b-c", "a1", &"a".repeat(31)] {
            assert!(valid_brand_id(ok), "{ok}");
        }
        for bad in [
            "a",
            "",
            "Acme",
            "-a",
            "a_b",
            "printpapi",
            "stock",
            &"a".repeat(32),
        ] {
            assert!(!valid_brand_id(bad), "{bad}");
        }
    }

    #[test]
    fn with_version_sets_the_line_and_keeps_every_other_one() {
        assert_eq!(with_version("", "3.7.0"), "version=3.7.0\n");
        assert_eq!(
            with_version("brand=acme\nfuture=1\n", "3.7.0"),
            "version=3.7.0\nbrand=acme\nfuture=1\n"
        );
        assert_eq!(
            with_version("brand=acme\r\nversion=3.6.0\r\nfuture=1\r\n", "3.7.0"),
            "brand=acme\nversion=3.7.0\nfuture=1\n"
        );
        // a doubled version line collapses into one
        assert_eq!(
            with_version("version=1.0.0\nversion=2.0.0\n", "3.7.0"),
            "version=3.7.0\n"
        );
    }

    #[test]
    fn with_brand_sets_the_line_and_keeps_every_other_one() {
        assert_eq!(
            with_brand("version=3.7.0\n", "acme"),
            "version=3.7.0\nbrand=acme\n"
        );
        assert_eq!(with_brand("", "acme"), "brand=acme\n");
        assert_eq!(
            with_brand("version=3.7.0\r\nbrand=old\r\nfuture=1\r\n", "acme"),
            "version=3.7.0\nbrand=acme\nfuture=1\n"
        );
        assert_eq!(with_brand("brand=a1\nbrand=a2\n", "acme"), "brand=acme\n");
        // what it writes reads back
        let m = parse(with_brand("version=3.7.0\n", "acme").as_bytes());
        assert_eq!(m.brand, BrandMark::Brand("acme".into()));
        assert_eq!(m.version.as_deref(), Some("3.7.0"));
    }

    #[test]
    fn who_may_replace_whom() {
        let acme = BrandMark::Brand("acme".into());
        // (installed, this build, allowed)
        let table: [(&BrandMark, Option<&str>, bool); 8] = [
            (&BrandMark::Stock, None, true),
            (&BrandMark::Stock, Some("acme"), true),
            (&acme, Some("acme"), true),
            (&acme, None, false),
            (&acme, Some("other"), false),
            (&BrandMark::Unreadable, None, false),
            (&BrandMark::Unreadable, Some("acme"), false),
            (&BrandMark::Stock, Some("other"), true),
        ];
        for (installed, this, ok) in table {
            assert_eq!(
                may_replace(installed, this),
                ok,
                "{installed:?} over {this:?}"
            );
        }
    }

    #[test]
    fn read_accepts_only_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".installed-by-setup");
        assert_eq!(read(&path), None);
        std::fs::write(&path, "version=3.7.0\n").unwrap();
        assert_eq!(read(&path).unwrap().version.as_deref(), Some("3.7.0"));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(read(&path), None);
    }

    #[test]
    fn an_oversized_file_is_unreadable_without_being_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m");
        std::fs::write(&path, vec![b'a'; 1 << 20]).unwrap();
        assert_eq!(read(&path).unwrap().brand, BrandMark::Unreadable);
    }
}
