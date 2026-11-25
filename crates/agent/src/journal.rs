// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The print journal: the outcome of a job that was printed, on disk before it is reported, so a
//! result that could not be delivered is delivered later (also after a restart) and a job the
//! server hands out again is answered from here instead of being printed a second time. See
//! docs/agent.md, Reliability.
//!
//! One file, UTF-8, one JSON object per line, append-only; each line is the whole state of one
//! job and the last line of a uid wins. Every record is written as "\n" + json, so a torn record
//! of an earlier crash is ended by the next one and costs only itself. The file never holds the
//! key, a payload, a title or a header: the status folder is readable by every local user.
//!
//! Interfaces this rests on:
//! - `File::sync_data` is the fsync of a record (`F_FULLFSYNC` on Apple systems, slower but once per
//!   printed job): <https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_data>, and on
//!   Windows `FlushFileBuffers`: <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers>.
//! - A rename over a file that somebody holds open without `FILE_SHARE_DELETE` is refused on
//!   Windows while appends to it work (share modes of `CreateFileW`):
//!   <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew>. That is
//!   why a compaction may fail and is only retried later.
//! - systemd's `StateDirectory=` and `$STATE_DIRECTORY` (several folders are joined with a colon):
//!   <https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html>.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::mask_key;
use crate::run::Log;

pub const FILE_NAME: &str = "print-journal.jsonl";
pub const TMP_NAME: &str = "print-journal.jsonl.tmp";
/// Entries kept. More drops the oldest acknowledged one, else the oldest.
pub const MAX_ENTRIES: usize = 1000;
/// Lines in the file before it is compacted (typical: two per job).
pub const MAX_LINES: usize = 4000;
/// The hard stop of the file's growth, and what a file may hold to be read at all.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;
/// Characters of an error text that are stored and replayed.
pub const MAX_ERROR: usize = 500;
/// A failed compaction is tried again this many lines later.
const COMPACT_RETRY: usize = 1000;
/// Reading an unreadable file: tries and the wait between them.
const READ_TRIES: u32 = 3;
const READ_WAIT: Duration = Duration::from_millis(200);

/// The outcome of one printed job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub uid: String,
    pub job_id: i64,
    pub ok: bool,
    pub error: Option<String>,
    /// The server has confirmed this result.
    pub acked: bool,
}

impl Entry {
    /// "\n" + the JSON object: newline first, see the module text.
    fn line(&self) -> String {
        let v = json!({
            "uid": self.uid, "job_id": self.job_id, "ok": self.ok,
            "error": self.error, "acked": self.acked,
        });
        format!("\n{v}")
    }
}

/// A uid the server could have made: 1 to 64 of [A-Za-z0-9_-].
pub fn valid_uid(uid: &str) -> bool {
    (1..=64).contains(&uid.len())
        && uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One line of the file, if it is the object this version writes. Anything else is skipped.
fn parse_line(raw: &[u8]) -> Option<Entry> {
    let Value::Object(o) = serde_json::from_slice(raw).ok()? else {
        return None;
    };
    let uid = o.get("uid")?.as_str()?;
    if !valid_uid(uid) {
        return None;
    }
    let error = match o.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return None,
    };
    Some(Entry {
        uid: uid.to_string(),
        job_id: o.get("job_id")?.as_i64()?,
        ok: o.get("ok")?.as_bool()?,
        error,
        acked: o.get("acked")?.as_bool()?,
    })
}

pub struct Journal {
    /// Oldest first; an update moves the entry to the end.
    // ponytail: linear lookup, at most MAX_ENTRIES (1000) entries; a map when that ever grows.
    entries: Vec<Entry>,
    /// None: this process keeps results in memory only.
    path: Option<PathBuf>,
    /// The agent's key, only to keep it out of error texts.
    key: String,
    /// Lines the file holds (counted at load, then plus every append).
    lines: usize,
    /// The whole file was read. Only then may it be rewritten (compacted): an append cannot
    /// destroy what was not read.
    complete: bool,
    /// Entries exist that the file may not hold; the next write puts all of them there.
    dirty: bool,
    /// Acknowledged failures whose line could not be written. They are no entries any more, but
    /// the file still ends in their un-acknowledged line, so the next write has to say it.
    forgotten: Vec<Entry>,
    /// Compaction is not tried before the file has this many lines.
    compact_at: usize,
}

impl Journal {
    /// No file: for tests, and when there is no usable folder.
    pub fn memory() -> Journal {
        Journal {
            entries: Vec::new(),
            path: None,
            key: String::new(),
            lines: 0,
            complete: false,
            dirty: false,
            forgotten: Vec::new(),
            compact_at: MAX_LINES,
        }
    }

    /// The key that is kept out of every stored error text (a journal in memory has no file to
    /// keep it out of, but a replay is sent over the wire).
    pub fn with_key(mut self, key: &str) -> Journal {
        self.key = key.to_string();
        self
    }

    /// Read the journal at `path`. Never fails: whatever cannot be used leaves the journal in
    /// memory and one line in `log`. Opening writes only when the file holds more than MAX_LINES
    /// lines: it is compacted.
    pub fn open(path: PathBuf, key: &str, log: Log) -> Journal {
        let mut j = Journal::memory();
        j.key = key.to_string();
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                j.path = Some(path);
                j.complete = true;
                return j;
            }
            Err(e) => {
                log(&format!(
                    "journal {} cannot be inspected ({e}); new results are still appended",
                    path.display()
                ));
                j.path = Some(path);
                return j;
            }
            Ok(md) if !md.is_file() || md.len() > MAX_FILE => {
                log(&format!(
                    "journal {} is no plain file of at most {MAX_FILE} bytes; results are kept in memory only and the path is left alone",
                    path.display()
                ));
                return j;
            }
            Ok(_) => {}
        }
        let mut data = None;
        for i in 0..READ_TRIES {
            match std::fs::read(&path) {
                Ok(d) => {
                    data = Some(d);
                    break;
                }
                Err(e) if i + 1 == READ_TRIES => log(&format!(
                    "journal {} cannot be read ({e}); earlier results are unknown, new ones are appended",
                    path.display()
                )),
                Err(_) => std::thread::sleep(READ_WAIT),
            }
        }
        j.path = Some(path);
        if let Some(data) = data {
            j.complete = true;
            for raw in data.split(|b| *b == b'\n') {
                if raw.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                j.lines += 1;
                if let Some(e) = parse_line(raw) {
                    j.apply(e);
                }
            }
            if j.complete && j.lines > MAX_LINES {
                j.compact(log);
            }
        }
        j
    }

    /// One line read from the file: an acknowledged failure is forgotten, anything else replaces
    /// the entry of its uid.
    fn apply(&mut self, e: Entry) {
        self.entries.retain(|x| x.uid != e.uid);
        if e.acked && !e.ok {
            return;
        }
        self.entries.push(e);
        self.bound();
    }

    /// At most MAX_ENTRIES: the oldest acknowledged one goes first, else the oldest. Eviction can
    /// only cause a duplicate print, never a lost one.
    fn bound(&mut self) {
        while self.entries.len() > MAX_ENTRIES {
            let at = self.entries.iter().position(|e| e.acked).unwrap_or(0);
            self.entries.remove(at);
        }
    }

    pub fn get(&self, uid: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.uid == uid)
    }

    /// Not acknowledged, oldest first.
    pub fn pending(&self) -> Vec<Entry> {
        self.entries.iter().filter(|e| !e.acked).cloned().collect()
    }

    /// The outcome of a printed job, durable (fsync) before the caller reports it. The error is
    /// masked and cut, because a replay sends that text.
    pub fn record(&mut self, uid: &str, job_id: i64, ok: bool, error: Option<&str>, log: Log) {
        let error = error.map(|e| {
            mask_key(e.to_string(), &self.key)
                .chars()
                .take(MAX_ERROR)
                .collect::<String>()
        });
        let e = Entry {
            uid: uid.to_string(),
            job_id,
            ok,
            error,
            acked: false,
        };
        self.entries.retain(|x| x.uid != uid);
        self.forgotten.retain(|x| x.uid != uid);
        self.entries.push(e.clone());
        self.bound();
        self.write(&[e], true, log);
    }

    /// A job handed out again whose entry was acknowledged: it is pending again, so a report that
    /// fails is flushed later. A no-op for an unknown uid or one that is pending already.
    pub fn reopen(&mut self, uid: &str, log: Log) {
        let Some(at) = self.entries.iter().position(|e| e.uid == uid && e.acked) else {
            return;
        };
        let mut e = self.entries.remove(at);
        e.acked = false;
        self.forgotten.retain(|x| x.uid != uid);
        self.entries.push(e.clone());
        self.write(&[e], false, log);
    }

    /// The server has the result. An ok result stays (acknowledged) to answer a later hand-out; a
    /// failure is forgotten, because a server that hands it out again wants it printed.
    pub fn delivered(&mut self, uid: &str, log: Log) {
        let Some(at) = self.entries.iter().position(|e| e.uid == uid) else {
            return;
        };
        let mut e = self.entries.remove(at);
        e.acked = true;
        if e.ok {
            self.entries.push(e.clone());
        }
        // a lost line only causes one more post
        self.write(&[e], false, log);
    }

    /// To the back of the line (memory only): one entry a proxy refuses must not block the rest.
    pub fn defer(&mut self, uid: &str) {
        if let Some(at) = self.entries.iter().position(|e| e.uid == uid) {
            let e = self.entries.remove(at);
            self.entries.push(e);
        }
    }

    /// Append `new` (or, after a failed write, every entry and then `new`), `sync` for a record.
    /// A disk error never stops printing: the entries stay in memory, one line says so when it
    /// starts and one when it is over.
    fn write(&mut self, new: &[Entry], sync: bool, log: Log) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let mut text = String::new();
        let mut count = 0;
        // after a failed write the recovery is made durable too: it is the only copy of what the
        // failed writes were meant to say
        let sync = sync || self.dirty;
        if self.dirty {
            // first, so that an entry written again later wins over them
            for e in self.forgotten.iter().chain(&self.entries) {
                text += &e.line();
                count += 1;
            }
        }
        for e in new {
            text += &e.line();
            count += 1;
        }
        match append(&path, text.as_bytes(), sync) {
            Ok(()) => {
                if self.dirty {
                    self.dirty = false;
                    log(&format!("journal {} is writable again", path.display()));
                }
                self.forgotten.clear();
                self.lines += count;
                if self.complete && self.lines > MAX_LINES && self.lines >= self.compact_at {
                    self.compact(log);
                }
            }
            Err(e) => {
                if !self.dirty {
                    log(&format!(
                        "journal {} cannot be written ({e}); results are kept in memory until it can",
                        path.display()
                    ));
                }
                self.dirty = true;
                for e in new.iter().filter(|e| e.acked && !e.ok) {
                    self.forgotten.retain(|x| x.uid != e.uid);
                    self.forgotten.push(e.clone());
                }
            }
        }
    }

    /// Rewrite the file with the entries that are left: tmp file, fsync, rename over. Any failure
    /// (Windows refuses the rename while somebody holds the file open without share-delete) leaves
    /// the old file, which is a superset, and tries again COMPACT_RETRY lines later.
    fn compact(&mut self, log: Log) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_file_name(TMP_NAME);
        let result = (|| -> std::io::Result<()> {
            let _ = std::fs::remove_file(&tmp);
            let mut f = create_new(&tmp)?;
            for e in &self.entries {
                f.write_all(e.line().as_bytes())?;
            }
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, path)?;
            sync_dir(path);
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.lines = self.entries.len();
                self.compact_at = MAX_LINES;
                self.forgotten.clear();
                // every entry is in the new file, so a write that had failed is made good
                if self.dirty {
                    self.dirty = false;
                    log(&format!("journal {} is writable again", path.display()));
                }
            }
            Err(_) => {
                let _ = std::fs::remove_file(&tmp);
                self.compact_at = self.lines + COMPACT_RETRY;
            }
        }
    }
}

/// A new file only the owner can read (the journal holds job ids and error texts).
fn create_new(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Open, append, optionally fsync, close: the handle is never kept, so nothing holds the file
/// between jobs. Refused (a write error) when the file would pass MAX_FILE.
fn append(path: &Path, text: &[u8], sync: bool) -> std::io::Result<()> {
    let existed = std::fs::symlink_metadata(path);
    let len = existed.as_ref().map_or(0, std::fs::Metadata::len);
    if len + text.len() as u64 > MAX_FILE {
        return Err(std::io::Error::other(format!(
            "the file would pass {MAX_FILE} bytes"
        )));
    }
    let mut o = OpenOptions::new();
    o.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(text)?;
    if sync {
        f.sync_data()?;
    }
    drop(f);
    if existed.is_err() {
        sync_dir(path);
    }
    Ok(())
}

/// fsync the folder of `path` so a new file or a rename survives a power cut. Unix only (Windows
/// has no such call and NTFS journals the metadata); best effort.
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Ok(d) = File::open(dir)
    {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Lines(RefCell<Vec<String>>);

    impl Lines {
        fn new() -> Lines {
            Lines(RefCell::new(Vec::new()))
        }
        fn log(&self) -> impl Fn(&str) + '_ {
            |m| self.0.borrow_mut().push(m.to_string())
        }
        fn all(&self) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn open(dir: &Path, lines: &Lines) -> Journal {
        Journal::open(dir.join(FILE_NAME), KEY, &lines.log())
    }

    fn uid(n: usize) -> String {
        format!("u{n:030}")
    }

    fn file_text(dir: &Path) -> String {
        String::from_utf8_lossy(&std::fs::read(dir.join(FILE_NAME)).unwrap()).into_owned()
    }

    #[test]
    fn a_record_is_there_after_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("aa", 7, true, None, &l.log());
        j.record("bb", 8, false, Some("copy 2 failed"), &l.log());
        let j = open(dir.path(), &l);
        assert_eq!(
            j.get("aa"),
            Some(&Entry {
                uid: "aa".into(),
                job_id: 7,
                ok: true,
                error: None,
                acked: false
            })
        );
        assert_eq!(j.get("bb").unwrap().error.as_deref(), Some("copy 2 failed"));
        assert_eq!(j.pending().len(), 2);
        assert_eq!(l.all(), Vec::<String>::new());
    }

    #[test]
    fn the_last_line_of_a_uid_wins_and_an_acked_ok_survives() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("aa", 7, true, None, &l.log());
        j.delivered("aa", &l.log());
        j.record("bb", 8, true, None, &l.log());
        j.reopen("aa", &l.log());
        j.delivered("aa", &l.log());
        let j = open(dir.path(), &l);
        assert!(j.get("aa").unwrap().acked);
        assert!(!j.get("bb").unwrap().acked);
        // oldest first, an update moves the entry to the end
        assert_eq!(
            j.pending().iter().map(|e| e.job_id).collect::<Vec<_>>(),
            [8]
        );
    }

    #[test]
    fn delivered_and_reopen_on_an_unknown_uid_do_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.delivered("nope", &l.log());
        j.reopen("nope", &l.log());
        j.defer("nope");
        assert!(!dir.path().join(FILE_NAME).exists());
        assert!(j.pending().is_empty());
    }

    #[test]
    fn an_acked_failure_is_gone_also_after_loading_the_file_again() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("fa", 1, false, Some("jam"), &l.log()); // never acknowledged: stays
        j.record("fb", 2, false, Some("jam"), &l.log());
        j.delivered("fb", &l.log()); // acknowledged: forgotten
        j.record("ok", 3, true, None, &l.log());
        j.delivered("ok", &l.log()); // an acked ok stays
        assert!(j.get("fb").is_none());
        let j = open(dir.path(), &l);
        assert!(j.get("fb").is_none());
        assert!(j.get("fa").is_some());
        assert!(j.get("ok").unwrap().acked);
    }

    #[test]
    fn damaged_lines_cost_only_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let good = |n: i64| {
            Entry {
                uid: format!("g{n}"),
                job_id: n,
                ok: true,
                error: None,
                acked: false,
            }
            .line()
        };
        let mut raw: Vec<u8> = Vec::new();
        raw.extend(good(1).bytes());
        // a torn record, a torn multi-byte character, a NUL block, garbage, wrong types
        raw.extend(b"\n{\"uid\":\"t1\",\"job_id\":2,\"ok\":tr");
        raw.extend(good(3).bytes());
        let mut torn = b"\n{\"uid\":\"t2\",\"job_id\":4,\"ok\":true,\"error\":\"".to_vec();
        torn.extend("é".as_bytes().iter().take(1));
        raw.extend(torn);
        raw.extend(good(5).bytes());
        raw.extend(b"\n");
        raw.extend(vec![0u8; 4096]);
        raw.extend(good(6).bytes());
        raw.extend(b"\nnot json at all");
        raw.extend(b"\n[1,2]");
        raw.extend(b"\n{\"uid\":\"w1\",\"job_id\":\"7\",\"ok\":true,\"acked\":false}");
        raw.extend(b"\n{\"uid\":\"w2\",\"job_id\":7,\"ok\":1,\"acked\":false}");
        raw.extend(b"\n{\"uid\":\"w3\",\"job_id\":7,\"ok\":true,\"acked\":false,\"error\":5}");
        raw.extend(b"\n{\"uid\":\"bad uid!\",\"job_id\":7,\"ok\":true,\"acked\":false}");
        raw.extend(b"\n{\"uid\":\"\",\"job_id\":7,\"ok\":true,\"acked\":false}");
        std::fs::write(dir.path().join(FILE_NAME), &raw).unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        let ids: Vec<i64> = j.pending().iter().map(|e| e.job_id).collect();
        assert_eq!(ids, [1, 3, 5, 6]);
        // the next record after a torn one loads intact
        j.record("after", 9, true, None, &l.log());
        let j = open(dir.path(), &l);
        assert!(j.get("after").is_some() && j.get("g6").is_some());
        assert_eq!(l.all(), Vec::<String>::new());
    }

    #[test]
    fn opening_never_rewrites_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw = b"garbage\n".to_vec();
        raw.extend(
            Entry {
                uid: "aa".into(),
                job_id: 1,
                ok: true,
                error: None,
                acked: true,
            }
            .line()
            .bytes(),
        );
        raw.extend(b"\n\n{torn");
        std::fs::write(dir.path().join(FILE_NAME), &raw).unwrap();
        let l = Lines::new();
        let _ = open(dir.path(), &l);
        assert_eq!(std::fs::read(dir.path().join(FILE_NAME)).unwrap(), raw);
        assert!(!dir.path().join(TMP_NAME).exists());
    }

    #[test]
    fn a_folder_or_an_oversized_file_at_the_path_is_memory_only_and_left_alone() {
        let l = Lines::new();
        // a folder
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(FILE_NAME)).unwrap();
        let mut j = open(dir.path(), &l);
        j.record("aa", 1, true, None, &l.log());
        assert!(j.get("aa").is_some());
        assert!(dir.path().join(FILE_NAME).is_dir());
        assert_eq!(
            std::fs::read_dir(dir.path().join(FILE_NAME))
                .unwrap()
                .count(),
            0
        );
        // too big
        let dir2 = tempfile::tempdir().unwrap();
        let big = vec![b'x'; MAX_FILE as usize + 1];
        std::fs::write(dir2.path().join(FILE_NAME), &big).unwrap();
        let mut j = open(dir2.path(), &l);
        j.record("aa", 1, true, None, &l.log());
        assert_eq!(
            std::fs::metadata(dir2.path().join(FILE_NAME))
                .unwrap()
                .len(),
            big.len() as u64
        );
        let all = l.all();
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all[0].contains(FILE_NAME) && all[1].contains(FILE_NAME));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_the_path_is_memory_only_and_the_target_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"mine").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(FILE_NAME)).unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("aa", 1, true, None, &l.log());
        assert!(j.get("aa").is_some());
        assert_eq!(std::fs::read(&target).unwrap(), b"mine");
        assert_eq!(l.all().len(), 1);
    }

    /// Makes the journal file unreadable for this process and returns what must be dropped (or
    /// undone) to make it readable again; None when the platform cannot do it here (root).
    #[cfg(windows)]
    fn lock_out(path: &Path) -> Option<File> {
        use std::os::windows::fs::OpenOptionsExt;
        Some(
            OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(path)
                .unwrap(),
        )
    }

    #[cfg(unix)]
    fn lock_out(path: &Path) -> Option<impl Drop> {
        use std::os::unix::fs::PermissionsExt;
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o600));
            }
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(path).is_ok() {
            // root reads everything
            let _ = Restore(path.to_path_buf());
            return None;
        }
        Some(Restore(path.to_path_buf()))
    }

    #[test]
    fn an_unreadable_file_is_kept_as_it_is_and_only_appended_to() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let old = Entry {
            uid: "old".into(),
            job_id: 1,
            ok: true,
            error: None,
            acked: false,
        };
        std::fs::write(&path, old.line()).unwrap();
        let Some(held) = lock_out(&path) else { return };
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        assert!(j.get("old").is_none());
        assert_eq!(l.all().len(), 1, "{:?}", l.all());
        drop(held);
        j.record("new", 2, true, None, &l.log());
        // no compaction, however many lines come: the unread content might be all that is left of
        // results the server never heard of
        for _ in 0..(MAX_LINES / 2 + 100) {
            j.delivered("new", &l.log());
            j.reopen("new", &l.log());
        }
        assert!(!dir.path().join(TMP_NAME).exists());
        assert!(file_text(dir.path()).lines().count() > MAX_LINES);
        let j = open(dir.path(), &l);
        assert!(j.get("old").is_some() && j.get("new").is_some());
    }

    #[test]
    fn an_acked_failure_whose_line_could_not_be_written_is_still_forgotten_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("fa", 1, false, Some("jam"), &l.log());
        let Some(held) = lock_out(&path) else { return };
        // the server accepted the failure, the line that says so cannot be written
        j.delivered("fa", &l.log());
        drop(held);
        assert!(j.dirty);
        assert!(j.get("fa").is_none());
        // the next good write brings the forgotten failure to disk as well
        j.record("fb", 2, true, None, &l.log());
        assert!(!j.dirty);
        let j = open(dir.path(), &l);
        assert!(
            j.get("fa").is_none(),
            "an accepted failure must not come back"
        );
        assert!(j.get("fb").is_some());
    }

    #[test]
    fn opening_a_complete_file_of_more_than_4000_lines_compacts_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw = String::new();
        for n in 0..(MAX_LINES / 2 + 3) {
            for (uid, id) in [("aa", 1), ("bb", 2)] {
                raw += &Entry {
                    uid: uid.into(),
                    job_id: id,
                    ok: true,
                    error: None,
                    acked: n % 2 == 0,
                }
                .line();
            }
        }
        std::fs::write(dir.path().join(FILE_NAME), &raw).unwrap();
        assert!(raw.lines().count() > MAX_LINES);
        let l = Lines::new();
        let j = open(dir.path(), &l);
        assert_eq!(
            file_text(dir.path())
                .lines()
                .filter(|x| !x.is_empty())
                .count(),
            2
        );
        assert!(!dir.path().join(TMP_NAME).exists());
        assert!(j.get("aa").is_some() && j.get("bb").is_some());
        let j = open(dir.path(), &l);
        assert_eq!(j.entries.len(), 2);
        assert_eq!(l.all(), Vec::<String>::new());
    }

    #[test]
    fn at_most_a_thousand_entries_the_oldest_acked_goes_first_then_the_oldest() {
        let l = Lines::new();
        let mut j = Journal::memory();
        for n in 0..MAX_ENTRIES {
            j.record(&uid(n), n as i64, true, None, &l.log());
        }
        j.delivered(&uid(5), &l.log());
        j.record(&uid(MAX_ENTRIES), 1000, true, None, &l.log());
        assert_eq!(j.entries.len(), MAX_ENTRIES);
        assert!(j.get(&uid(5)).is_none(), "the oldest acked one went first");
        assert!(j.get(&uid(0)).is_some());
        // none acked: the oldest goes
        j.record(&uid(MAX_ENTRIES + 1), 1001, true, None, &l.log());
        assert!(j.get(&uid(0)).is_none());
        assert_eq!(j.entries.len(), MAX_ENTRIES);
        assert!(j.get(&uid(MAX_ENTRIES + 1)).is_some());
    }

    #[test]
    fn the_file_is_compacted_past_4000_lines() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("keep", 1, true, None, &l.log());
        j.record("fail", 2, false, Some("x"), &l.log());
        for _ in 0..(MAX_LINES / 2 + 10) {
            j.delivered("keep", &l.log());
            j.reopen("keep", &l.log());
        }
        let text = file_text(dir.path());
        assert!(text.lines().count() < 100, "{}", text.lines().count());
        assert!(!dir.path().join(TMP_NAME).exists());
        let j = open(dir.path(), &l);
        assert_eq!(j.entries.len(), 2);
        assert!(j.get("keep").is_some() && j.get("fail").is_some());
    }

    #[cfg(windows)]
    #[test]
    fn a_compaction_that_the_system_refuses_leaves_the_file_and_works_later() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("keep", 1, true, None, &l.log());
        // somebody reads the file without FILE_SHARE_DELETE: appends work, the rename does not
        let reader = OpenOptions::new()
            .read(true)
            .share_mode(1 | 2) // FILE_SHARE_READ | FILE_SHARE_WRITE
            .open(dir.path().join(FILE_NAME))
            .unwrap();
        for _ in 0..(MAX_LINES / 2 + 10) {
            j.delivered("keep", &l.log());
            j.reopen("keep", &l.log());
        }
        assert!(file_text(dir.path()).lines().count() > MAX_LINES);
        assert!(!dir.path().join(TMP_NAME).exists());
        drop(reader);
        for _ in 0..(COMPACT_RETRY / 2 + 10) {
            j.delivered("keep", &l.log());
            j.reopen("keep", &l.log());
        }
        assert!(file_text(dir.path()).lines().count() < 100);
        assert!(open(dir.path(), &l).get("keep").is_some());
    }

    #[test]
    fn a_failed_append_keeps_the_entries_and_the_next_good_write_puts_them_all_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("later");
        let l = Lines::new();
        let mut j = Journal::open(folder.join(FILE_NAME), KEY, &l.log());
        j.record("aa", 1, true, None, &l.log());
        j.record("bb", 2, true, None, &l.log());
        assert_eq!(
            l.all().len(),
            1,
            "one line for the failure, not one per write"
        );
        assert!(j.get("aa").is_some() && j.get("bb").is_some());
        std::fs::create_dir(&folder).unwrap();
        j.record("cc", 3, true, None, &l.log());
        let all = l.all();
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all[1].contains("again"));
        let j = Journal::open(folder.join(FILE_NAME), KEY, &l.log());
        for u in ["aa", "bb", "cc"] {
            assert!(j.get(u).is_some(), "{u}");
        }
    }

    #[test]
    fn a_file_at_the_maximum_is_not_grown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, vec![b'x'; MAX_FILE as usize]).unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("aa", 1, true, None, &l.log());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), MAX_FILE);
        assert!(j.get("aa").is_some());
        assert_eq!(l.all().len(), 1);
    }

    #[test]
    fn the_error_is_masked_and_cut_and_no_line_holds_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        let long = format!("{KEY} refused {}", "ü".repeat(900));
        j.record("aa", 1, false, Some(&long), &l.log());
        let e = j.get("aa").unwrap();
        let text = e.error.clone().unwrap();
        assert_eq!(text.chars().count(), MAX_ERROR);
        assert!(text.starts_with("*** refused "));
        assert!(!file_text(dir.path()).contains(KEY));
        // a replay sends exactly what was stored
        let j = open(dir.path(), &l);
        assert_eq!(j.get("aa").unwrap().error.as_deref(), Some(text.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("aa", 1, true, None, &l.log());
        let mode = std::fs::metadata(dir.path().join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn defer_moves_an_entry_to_the_back_in_memory_only() {
        let dir = tempfile::tempdir().unwrap();
        let l = Lines::new();
        let mut j = open(dir.path(), &l);
        j.record("aa", 1, true, None, &l.log());
        j.record("bb", 2, true, None, &l.log());
        j.defer("aa");
        assert_eq!(j.pending()[0].uid, "bb");
        let before = file_text(dir.path());
        j.defer("bb");
        assert_eq!(file_text(dir.path()), before);
    }

    #[test]
    fn a_uid_is_one_to_sixty_four_safe_characters() {
        assert!(valid_uid("aZ09_-"));
        assert!(valid_uid(&"a".repeat(64)));
        assert!(!valid_uid(""));
        assert!(!valid_uid(&"a".repeat(65)));
        assert!(!valid_uid("a b"));
        assert!(!valid_uid("a/../b"));
        assert!(!valid_uid("ä"));
    }
}
