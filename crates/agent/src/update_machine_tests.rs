// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The steps of an update against a simulated machine: real temp folders and files, a fake
//! command runner that behaves like the service manager, and a clock that only the injected
//! `sleep` advances.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ring::signature::{Ed25519KeyPair, KeyPair};

use super::*;
use crate::apply::ApplyError;
use crate::install::{Layout, Os};
use crate::proc::Runner;

pub(super) const SEED: &[u8; 32] = b"printpapi test seed, not secret!";
pub(super) const OLD_AGENT: &[u8] = b"agent 3.6.0";
pub(super) const OLD_TRAY: &[u8] = b"tray 3.6.0";
pub(super) const NEW_AGENT: &[u8] = b"agent 3.7.0";
pub(super) const NEW_TRAY: &[u8] = b"tray 3.7.0";
pub(super) const INI: &str = "[agent]\nserver_url = https://print.example.com\napi_key = SECRET-KEY-VALUE\nname = pc\nprinters = p\nupdates = auto\n\n[headers]\nCF-Access-Client-Secret = HEADER-SECRET-VALUE\n";

fn public() -> [u8; 32] {
    Ed25519KeyPair::from_seed_unchecked(SEED)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the new agent does after it was started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NewAgent {
    Good,
    /// Starts, never reaches the server.
    NeverOnline,
    /// Is online, but reports the old version (the old process still answers).
    WrongVersion,
    /// Online, then a new process every few seconds.
    CrashLoop,
}

#[derive(Default)]
pub(super) struct Proc {
    pub version: Option<String>,
    pub started_at: f64,
    pub online_at: f64,
    pub stopped_at: f64,
}

pub(super) struct Sim {
    pub tmp: tempfile::TempDir,
    pub layout: Layout,
    pub os: Os,
    pub staging: PathBuf,
    pub clock: Rc<Cell<f64>>,
    /// every command, as a line
    pub log: Rc<RefCell<Vec<String>>>,
    pub proc_: Rc<RefCell<Proc>>,
    pub new_agent: Rc<Cell<NewAgent>>,
    /// busy until the clock reaches this
    pub busy_until: Rc<Cell<f64>>,
    /// the first start commands fail this many times
    pub start_failures: Rc<Cell<u32>>,
    /// the first start of the new agent is ignored (Windows: IgnoreNew)
    pub ignore_first_start: Rc<Cell<bool>>,
    /// the first start of the old agent (the way back) is ignored
    pub ignore_first_old: Rc<Cell<bool>>,
    /// the Windows task is Running whatever is done
    pub stuck_running: Rc<Cell<bool>>,
    /// `schtasks /query` fails
    pub query_fails: Rc<Cell<bool>>,
    /// the status file does not exist
    pub no_status: Rc<Cell<bool>>,
    /// the start command removes this file (a way to make a swap fail)
    pub remove_on_start: Rc<RefCell<Option<PathBuf>>>,
    /// what a probe of a file with this content does instead of reporting its version
    pub probe_error: Rc<RefCell<Vec<(String, String)>>>,
    /// called in every sleep, with the clock after it
    pub on_sleep: Rc<RefCell<Option<Box<dyn Fn(f64)>>>>,
    /// called when a probe runs, with the path of the program
    pub on_probe: Rc<RefCell<Option<Box<dyn Fn(&Path)>>>>,
    /// files the network serves, by URL
    pub downloads: Rc<RefCell<Vec<(String, Result<Vec<u8>, DownloadError>)>>>,
    pub downloaded: Rc<RefCell<Vec<String>>>,
    /// called before the first download
    pub on_download: Rc<RefCell<Option<Box<dyn Fn()>>>>,
    pub at_reboot: Rc<RefCell<Vec<PathBuf>>>,
    /// How many .app folders the fake ditto unpacks from an app archive
    pub unpack_apps: Rc<Cell<u32>>,
    pub keys: Vec<[u8; 32]>,
    pub version: &'static str,
    pub self_exe: PathBuf,
    run: Runner,
    sleep: Box<dyn Fn(u64)>,
    now: Box<dyn Fn() -> f64>,
    reboot: Box<dyn Fn(&Path) -> std::io::Result<()>>,
    read_status: Box<dyn Fn(&Path) -> Option<StatusSeen>>,
    download: Box<dyn Fn(&str, u64, Duration) -> Result<Vec<u8>, DownloadError>>,
    http_for: Box<dyn Fn(&crate::config::HttpSettings) -> Box<dyn crate::client::Http>>,
    owner: Box<dyn Fn(&Path) -> Option<String>>,
    serves: Box<dyn Fn(u32) -> bool>,
}

pub(super) fn target_name(os: Os) -> String {
    target(os)
}

impl Sim {
    pub fn new(os: Os) -> Sim {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("opt");
        std::fs::create_dir_all(dir.join(".update")).unwrap();
        let ext = if os == Os::Windows { ".exe" } else { "" };
        let app = tmp.path().join("Applications").join("printpapi Agent.app");
        let layout = Layout {
            binary: dir.join(format!("printpapi-agent{ext}")),
            ini: dir.join("agent.ini"),
            marker: dir.join(".installed-by-setup"),
            sumatra: dir.join("SumatraPDF.exe"),
            autostart: tmp.path().join("etc").join("autostart-file"),
            status_dir: tmp.path().join("status"),
            tray: if os == Os::Macos {
                app.clone()
            } else {
                dir.join(format!("printpapi-tray{ext}"))
            },
            tray_autostart: None,
            app_entry: None,
            dir: dir.clone(),
        };
        std::fs::create_dir_all(&layout.status_dir).unwrap();
        std::fs::create_dir_all(tmp.path().join("etc")).unwrap();
        std::fs::write(&layout.binary, OLD_AGENT).unwrap();
        if os == Os::Macos {
            std::fs::create_dir_all(app.join("Contents").join("MacOS")).unwrap();
            std::fs::write(
                app.join("Contents").join("MacOS").join("printpapi-setup"),
                OLD_TRAY,
            )
            .unwrap();
        } else {
            std::fs::write(&layout.tray, OLD_TRAY).unwrap();
        }
        std::fs::write(&layout.ini, INI).unwrap();
        std::fs::write(&layout.marker, "version=3.6.0\n").unwrap();
        std::fs::write(layout.autostart.clone(), b"the unit").unwrap();
        std::fs::write(dir.join(".update").join(LOCK_NAME), b"").unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir(&staging).unwrap();

        let clock = Rc::new(Cell::new(10_000.0));
        let log: Rc<RefCell<Vec<String>>> = Rc::default();
        let proc_: Rc<RefCell<Proc>> = Rc::new(RefCell::new(Proc {
            version: Some("3.6.0".into()),
            started_at: 9_000.0,
            online_at: 9_001.0,
            stopped_at: 0.0,
        }));
        let new_agent = Rc::new(Cell::new(NewAgent::Good));
        let busy_until = Rc::new(Cell::new(0.0));
        let start_failures = Rc::new(Cell::new(0));
        let ignore_first_start = Rc::new(Cell::new(false));
        let ignore_first_old = Rc::new(Cell::new(false));
        let stuck_running = Rc::new(Cell::new(false));
        let query_fails = Rc::new(Cell::new(false));
        let no_status = Rc::new(Cell::new(false));
        let remove_on_start: Rc<RefCell<Option<PathBuf>>> = Rc::default();
        let probe_error: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
        let on_sleep: Rc<RefCell<Option<Box<dyn Fn(f64)>>>> = Rc::default();
        let on_probe: Rc<RefCell<Option<Box<dyn Fn(&Path)>>>> = Rc::default();
        let downloads: Rc<RefCell<Vec<(String, Result<Vec<u8>, DownloadError>)>>> = Rc::default();
        let downloaded: Rc<RefCell<Vec<String>>> = Rc::default();
        let on_download: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::default();
        let at_reboot: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
        let unpack_apps = Rc::new(Cell::new(1));

        let run: Runner = {
            let (log, proc_, clock, binary) = (
                log.clone(),
                proc_.clone(),
                clock.clone(),
                layout.binary.clone(),
            );
            let (new_agent, start_failures, ignore_first, stuck, query_fails) = (
                new_agent.clone(),
                start_failures.clone(),
                ignore_first_start.clone(),
                stuck_running.clone(),
                query_fails.clone(),
            );
            let ignore_old = ignore_first_old.clone();
            let ignored_old_once = Cell::new(false);
            let (remove_on_start, probe_error, on_probe, unpack_apps) = (
                remove_on_start.clone(),
                probe_error.clone(),
                on_probe.clone(),
                unpack_apps.clone(),
            );
            let ignored_once = Cell::new(false);
            Rc::new(move |c| {
                let line = c.argv.join(" ");
                let program = c.argv[0].clone();
                if c.argv.get(1).map(String::as_str) == Some("--version") {
                    log.borrow_mut().push(format!(
                        "probe {}",
                        Path::new(&program).file_name().unwrap().to_string_lossy()
                    ));
                    if let Some(hook) = on_probe.borrow().as_ref() {
                        hook(Path::new(&program));
                    }
                    let content = String::from_utf8_lossy(
                        &std::fs::read(&program).map_err(|e| e.to_string())?,
                    )
                    .into_owned();
                    for (needle, error) in probe_error.borrow().iter() {
                        if content.contains(needle.as_str()) {
                            return Err(error.clone());
                        }
                    }
                    let (word, version) = content.split_once(' ').ok_or("not a program")?;
                    let name = match word {
                        "agent" => "printpapi-agent",
                        "tray" => "printpapi-setup",
                        _ => return Err("cannot run".into()),
                    };
                    return Ok(format!("{name} {version}\n").into_bytes());
                }
                log.borrow_mut().push(line.clone());
                if program.ends_with("ditto") {
                    // ¤ditto -x -k <zip> <dir>¤ unpacks, ¤ditto <from> <to>¤ copies a folder
                    if c.argv[1] == "-x" {
                        for i in 0..unpack_apps.get() {
                            let inner = Path::new(&c.argv[4])
                                .join(format!("printpapi{i}.app"))
                                .join("Contents")
                                .join("MacOS");
                            std::fs::create_dir_all(&inner).map_err(|e| e.to_string())?;
                            std::fs::write(inner.join("printpapi-setup"), NEW_TRAY)
                                .map_err(|e| e.to_string())?;
                        }
                    } else {
                        copy_dir(Path::new(&c.argv[1]), Path::new(&c.argv[2]))
                            .map_err(|e| e.to_string())?;
                    }
                    return Ok(Vec::new());
                }
                if program.ends_with("xattr")
                    || program.ends_with("chown")
                    || program.ends_with("chmod")
                {
                    return Ok(Vec::new());
                }
                let stop = line.contains("/end") || line.contains("systemctl stop");
                let query = line.contains("/query");
                let start = line.contains("/run")
                    || line.contains("systemctl restart")
                    || line.contains("kickstart");
                if stop {
                    let mut p = proc_.borrow_mut();
                    if p.version.is_some() {
                        p.stopped_at = clock.get();
                    }
                    p.version = None;
                }
                if query {
                    if query_fails.get() {
                        return Err("query failed".into());
                    }
                    let state = if stuck.get() || proc_.borrow().version.is_some() {
                        "Running"
                    } else {
                        "Ready"
                    };
                    return Ok(format!(r#""\printpapi-agent","N/A","{state}""#).into_bytes());
                }
                if start {
                    if start_failures.get() > 0 {
                        start_failures.set(start_failures.get() - 1);
                        return Err(format!("start failed: {line}"));
                    }
                    if let Some(path) = remove_on_start.borrow().as_ref() {
                        let _ = std::fs::remove_file(path);
                    }
                    // the program on disk decides what starts: "agent <version>"
                    let content =
                        String::from_utf8_lossy(&std::fs::read(&binary).unwrap_or_default())
                            .into_owned();
                    let version = content
                        .strip_prefix("agent ")
                        .unwrap_or("0.0.0")
                        .to_string();
                    let is_new = version != "3.6.0";
                    if ignore_first.get() && !ignored_once.get() && is_new {
                        ignored_once.set(true);
                        return Ok(Vec::new());
                    }
                    if ignore_old.get() && !ignored_old_once.get() && !is_new {
                        ignored_old_once.set(true);
                        return Ok(Vec::new());
                    }
                    let now = clock.get();
                    let mut p = proc_.borrow_mut();
                    let proc = |version: String, online_at: f64| Proc {
                        version: Some(version),
                        started_at: now + 1.0,
                        online_at,
                        stopped_at: 0.0,
                    };
                    *p = if !is_new {
                        proc("3.6.0".into(), now + 2.0)
                    } else {
                        match new_agent.get() {
                            NewAgent::NeverOnline => proc(version, f64::INFINITY),
                            NewAgent::WrongVersion => proc("3.6.0".into(), now + 2.0),
                            NewAgent::Good | NewAgent::CrashLoop => proc(version, now + 2.0),
                        }
                    };
                }
                Ok(Vec::new())
            })
        };
        let sleep: Box<dyn Fn(u64)> = {
            let (clock, on_sleep) = (clock.clone(), on_sleep.clone());
            Box::new(move |s| {
                clock.set(clock.get() + s as f64);
                if let Some(hook) = on_sleep.borrow().as_ref() {
                    hook(clock.get());
                }
            })
        };
        let now: Box<dyn Fn() -> f64> = {
            let clock = clock.clone();
            Box::new(move || clock.get())
        };
        let reboot: Box<dyn Fn(&Path) -> std::io::Result<()>> = {
            let at_reboot = at_reboot.clone();
            Box::new(move |p| {
                at_reboot.borrow_mut().push(p.to_path_buf());
                Ok(())
            })
        };
        let read_status: Box<dyn Fn(&Path) -> Option<StatusSeen>> = {
            let (proc_, clock, busy_until, new_agent, no_status) = (
                proc_.clone(),
                clock.clone(),
                busy_until.clone(),
                new_agent.clone(),
                no_status.clone(),
            );
            Box::new(move |_| {
                if no_status.get() {
                    return None;
                }
                let p = proc_.borrow();
                let now = clock.get();
                let Some(version) = &p.version else {
                    // a stopped agent leaves an old file behind
                    let text = format!(
                        r#"{{"version":"3.6.0","state":"online","started_at":9000.0,"last_contact":{}}}"#,
                        p.stopped_at
                    );
                    return Some(StatusSeen {
                        age: (now - p.stopped_at).max(0.0),
                        text,
                    });
                };
                if now < p.online_at {
                    return Some(StatusSeen {
                        age: 1.0,
                        text: format!(
                            r#"{{"version":"{version}","state":"offline","started_at":{},"last_contact":null}}"#,
                            p.started_at
                        ),
                    });
                }
                // a crash loop is a new process every few seconds
                let started = if version != "3.6.0" && new_agent.get() == NewAgent::CrashLoop {
                    p.started_at + ((now - p.online_at) / 5.0).floor() * 5.0
                } else {
                    p.started_at
                };
                let busy = now < busy_until.get();
                Some(StatusSeen {
                    age: 1.0,
                    text: format!(
                        r#"{{"version":"{version}","state":"online","started_at":{started},"last_contact":{},"busy":{busy}}}"#,
                        now - 0.5
                    ),
                })
            })
        };
        let download: Box<dyn Fn(&str, u64, Duration) -> Result<Vec<u8>, DownloadError>> = {
            let (downloads, downloaded, on_download) =
                (downloads.clone(), downloaded.clone(), on_download.clone());
            Box::new(move |url, _, _| {
                if let Some(hook) = on_download.borrow().as_ref() {
                    hook();
                }
                downloaded.borrow_mut().push(url.to_string());
                downloads
                    .borrow()
                    .iter()
                    .find(|(u, _)| u == url)
                    .map(|(_, r)| r.clone())
                    .unwrap_or(Err(DownloadError::NotFound))
            })
        };
        let self_exe = layout.binary.clone();
        Sim {
            tmp,
            layout,
            os,
            staging,
            clock,
            log,
            proc_,
            new_agent,
            busy_until,
            start_failures,
            ignore_first_start,
            ignore_first_old,
            stuck_running,
            query_fails,
            no_status,
            remove_on_start,
            probe_error,
            on_sleep,
            on_probe,
            downloads,
            downloaded,
            on_download,
            at_reboot,
            unpack_apps,
            keys: vec![public()],
            version: "3.6.0",
            self_exe,
            run,
            sleep,
            now,
            reboot,
            read_status,
            download,
            http_for: Box::new(|_| unreachable!("an update never talks to the printpapi server")),
            owner: Box::new(|_| None),
            serves: Box::new(|_| false),
        }
    }

    pub fn env(&self) -> Env<'_> {
        Env {
            os: self.os,
            layout: self.layout.clone(),
            sys: PathBuf::from(if self.os == Os::Windows {
                "C:\\Windows\\System32"
            } else {
                "/"
            }),
            run: self.run.clone(),
            http_for: &*self.http_for,
            self_exe: self.self_exe.clone(),
            sleep: &*self.sleep,
            now: &*self.now,
            keep_pid: None,
            delete_at_reboot: &*self.reboot,
            owner_sid: &*self.owner,
            serves_mount: &*self.serves,
            keystore: None,
            update_keys: &self.keys,
            download: &*self.download,
            read_status: &*self.read_status,
            version: self.version,
        }
    }

    pub fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    pub fn binary(&self) -> Vec<u8> {
        std::fs::read(&self.layout.binary).unwrap()
    }

    pub fn tray(&self) -> Vec<u8> {
        if self.os == Os::Macos {
            return std::fs::read(
                self.layout
                    .tray
                    .join("Contents")
                    .join("MacOS")
                    .join("printpapi-setup"),
            )
            .unwrap();
        }
        std::fs::read(&self.layout.tray).unwrap()
    }

    pub fn marker_text(&self) -> String {
        std::fs::read_to_string(&self.layout.marker).unwrap()
    }

    /// The signed file of a release plus its files, as `(name, bytes)`.
    pub fn release(
        &self,
        version: &str,
        agent: &[u8],
        tray: Option<&[u8]>,
    ) -> Vec<(String, Vec<u8>)> {
        let t = target_name(self.os);
        let file = |role: &str, bytes: &[u8]| {
            format!(
                r#""{role}":{{"file":"printpapi-update-{role}-{t}","size":{},"sha256":"{}"}}"#,
                bytes.len(),
                sha256_hex(bytes)
            )
        };
        let mut roles = vec![file("agent", agent)];
        let mut files = vec![(format!("printpapi-update-agent-{t}"), agent.to_vec())];
        if let Some(tray) = tray {
            // macOS ships the app as a ZIP under another role
            let (role, suffix) = if self.os == Os::Macos {
                ("app", ".zip")
            } else {
                ("tray", "")
            };
            roles.push(format!(
                r#""{role}":{{"file":"printpapi-update-{role}-{t}{suffix}","size":{},"sha256":"{}"}}"#,
                tray.len(),
                sha256_hex(tray)
            ));
            files.push((
                format!("printpapi-update-{role}-{t}{suffix}"),
                tray.to_vec(),
            ));
        }
        let payload = format!(
            r#"{{"schema":1,"version":"{version}","targets":{{"{t}":{{{}}}}}}}"#,
            roles.join(",")
        );
        let pair = Ed25519KeyPair::from_seed_unchecked(SEED).unwrap();
        let mut signed =
            format!("sig {}\n\n", hex(pair.sign(payload.as_bytes()).as_ref())).into_bytes();
        signed.extend_from_slice(payload.as_bytes());
        let mut all = vec![(MANIFEST_NAME.to_string(), signed)];
        all.extend(files);
        all
    }

    /// The release in the staging folder, as the window leaves it.
    pub fn stage(&self, release: &[(String, Vec<u8>)]) {
        for (name, bytes) in release {
            std::fs::write(self.staging.join(name), bytes).unwrap();
        }
    }

    /// The release on the network at `base`.
    pub fn serve(&self, base: &str, release: &[(String, Vec<u8>)]) {
        for (name, bytes) in release {
            self.downloads
                .borrow_mut()
                .push((format!("{base}/{name}"), Ok(bytes.clone())));
        }
    }

    pub fn good_release(&self) -> Vec<(String, Vec<u8>)> {
        self.release("3.7.0", NEW_AGENT, Some(NEW_TRAY))
    }

    /// What the machine looks like, to see that nothing changed.
    pub fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
            entries.sort_by_key(|e| e.path());
            for e in entries {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    // a held lock cannot be read on Windows, and it has no content
                    let bytes = if p.file_name().is_some_and(|n| n == LOCK_NAME) {
                        Vec::new()
                    } else {
                        std::fs::read(&p).unwrap()
                    };
                    out.push((
                        p.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                        bytes,
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            self.layout.dir.parent().unwrap(),
            self.layout.dir.parent().unwrap(),
            &mut out,
        );
        out
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

pub(super) fn code(r: Outcome) -> String {
    match r {
        Ok(v) => panic!("expected an error, got {v:?}"),
        Err(e) => e.code().to_string(),
    }
}

pub(super) fn error(r: Outcome) -> ApplyError {
    r.expect_err("expected an error")
}
