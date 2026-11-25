// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The printpapi agent: runs on the machine with the printers, registers them with the server,
//! then long-polls for jobs, prints them and reports the result. Outbound connections only.

pub mod apply;
pub mod backend_cups;
pub mod backend_windows;
pub mod client;
pub mod config;
pub mod discover;
pub mod ini;
pub mod install;
pub mod keystore;
pub mod log;
pub mod print;
pub mod proc;
pub mod pyrepr;
pub mod run;
pub mod setup;
pub mod status;

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use client::Client;
use config::{Capabilities, Config, HttpSettings, Printer, parse_printers};
use keystore::KeyStore;
use print::{Fns, add_capabilities};
use proc::{Runner, real_runner};
use run::{PrinterMap, build_printer_map, poll_forever, register_with_retry};
use status::{Reporter, Status, StatusWriter, unix_now};

pub use print::{PdfFn, RawFn};
type RegisterFn<'a> = dyn Fn(&str, &str, &str, &[Printer]) -> Value + 'a;
pub type CapsFn = Box<dyn Fn(&str) -> Option<Capabilities>>;

/// Print to stdout, ignoring a closed or missing stdout (a background run has none).
pub fn out_line(msg: &str) {
    #[cfg(test)]
    OUT_LINES.with(|l| l.borrow_mut().push(msg.to_string()));
    let _ = writeln!(std::io::stdout(), "{msg}");
}

/// Print to stderr, ignoring a closed or missing stderr.
pub fn err_line(msg: &str) {
    #[cfg(test)]
    ERR_LINES.with(|l| l.borrow_mut().push(msg.to_string()));
    let _ = writeln!(std::io::stderr(), "{msg}");
}

#[cfg(test)]
thread_local! {
    /// What `err_line` printed on this thread, so tests can check the console output.
    pub static ERR_LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// What `out_line` printed on this thread.
    pub static OUT_LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Which printing backend a machine uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    /// Linux and macOS (macOS is CUPS underneath, see docs/agent.md#macos).
    Cups,
}

impl Platform {
    /// `sys.platform` spelling in, backend out: `win32` is Windows, everything else CUPS.
    pub fn from_name(name: &str) -> Platform {
        if name.starts_with("win") {
            Platform::Windows
        } else {
            Platform::Cups
        }
    }

    pub fn host() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Cups
        }
    }
}

/// `(raw_fn, pdf_fn)` for the platform. Windows: the spooler + SumatraPDF; else CUPS `lp`.
pub fn select_backend(platform: Platform, sumatra: &str, run: Runner) -> (RawFn, PdfFn) {
    match platform {
        Platform::Windows => {
            let sumatra = sumatra.to_string();
            (
                Box::new(backend_windows::raw_to_printer),
                Box::new(move |p, d, o| backend_windows::pdf_to_printer(p, d, o, &sumatra, &run)),
            )
        }
        Platform::Cups => {
            let run2 = run.clone();
            (
                Box::new(move |p, d| backend_cups::raw_to_printer_cups(p, d, &run)),
                Box::new(move |p, d, o| backend_cups::pdf_to_printer_cups(p, d, o, &run2)),
            )
        }
    }
}

/// The capability collector for the platform (best-effort, `None` on any failure).
pub fn select_caps_collector(platform: Platform, run: Runner) -> CapsFn {
    match platform {
        #[cfg(windows)]
        Platform::Windows => {
            Box::new(|p| backend_windows::collect_capabilities_windows(p, &backend_windows::Win32))
        }
        #[cfg(not(windows))]
        Platform::Windows => Box::new(|_| None),
        Platform::Cups => Box::new(move |p| backend_cups::collect_capabilities_cups(p, &run)),
    }
}

/// Why `run` stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum Fatal {
    /// A bad agent.ini: logged as `exit: <msg>`.
    Exit(String),
    /// Anything else that took the agent down.
    Crash(String),
}

impl Fatal {
    /// The crash-log line for this ending.
    pub fn log_line(&self) -> String {
        match self {
            Fatal::Exit(m) => format!("exit: {m}"),
            Fatal::Crash(m) => m.clone(),
        }
    }
}

/// The pieces `start` drives, so a test can run the whole start-up without a network.
pub struct Hooks<'a> {
    pub caps: &'a dyn Fn(&str) -> Option<Capabilities>,
    pub register: &'a RegisterFn<'a>,
    /// Called once the printers are bound to their ids, right before polling starts.
    pub registered: &'a dyn Fn(),
    pub poll: &'a dyn Fn(&str, &str, &PrinterMap),
}

/// `msg` without the key. A key under 8 characters is no real key (the server hands out 32 hex
/// digits) and masking it would cut up the text.
// ponytail: only the key as one piece is found; a message that quotes part of it is not.
pub(crate) fn mask_key(msg: String, key: &str) -> String {
    if key.len() < 8 {
        msg
    } else {
        msg.replace(key, "***")
    }
}

/// agent.ini -> printers -> register -> poll. Returns only if `poll` does (tests) or start-up fails.
pub fn start(cfg: &Config, hooks: &Hooks) -> Result<(), Fatal> {
    let base = cfg
        .require("server_url")
        .map_err(Fatal::Exit)?
        .trim_end_matches('/')
        .to_string();
    let key = cfg.require("api_key").map_err(Fatal::Exit)?;
    let name = cfg
        .get("name")
        .map_err(Fatal::Exit)?
        .unwrap_or_else(|| "agent".to_string());
    // The printer errors quote the entry, which is how an operator finds it. An entry that pulls the
    // key in with `%(api_key)s` must not put it into the log (in /tmp on Linux) or on the console.
    let printers = parse_printers(&cfg.require("printers").map_err(Fatal::Exit)?)
        .map_err(|m| Fatal::Crash(mask_key(m, &key)))?;
    let printers = add_capabilities(printers, hooks.caps);
    let reg = (hooks.register)(&base, &key, &name, &printers);
    let printer_by_id = build_printer_map(&printers, &reg).map_err(Fatal::Crash)?;
    out_line(&format!(
        "print-agent registered as computer {}, printers={}",
        pyrepr::py_str(&reg["computer_id"]),
        printer_by_id.repr()
    ));
    (hooks.registered)();
    (hooks.poll)(&base, &key, &printer_by_id);
    Ok(())
}

/// The SumatraPDF to start. Next to the exe when it is there. Without it, a setup-installed agent
/// (marker file, runs as SYSTEM) still gets the absolute path: a bare name would be searched on
/// the machine PATH and a local user could plant a SumatraPDF.exe there. The job then fails with
/// the spawn error naming the missing file. Only a hand-installed agent falls back to the PATH.
fn sumatra_program(base_dir: &Path) -> String {
    let bundled = base_dir.join("SumatraPDF.exe");
    if bundled.exists() || base_dir.join(install::MARKER_NAME).exists() {
        bundled.to_string_lossy().into_owned()
    } else {
        "SumatraPDF.exe".to_string()
    }
}

/// The folder for the status file and the crash log, only for an agent that setup installed (the
/// marker file next to the exe). A hand-set-up agent behaves as before.
fn setup_data_dir(base_dir: &Path, os: install::Os) -> Option<PathBuf> {
    base_dir
        .join(install::MARKER_NAME)
        .exists()
        .then(|| status::status_dir(os))
}

/// The folder only if the agent may use it: it exists, is no link and belongs to SYSTEM or the
/// administrators (root, the service user). Setup creates it with its permissions; an agent must
/// not create one that setup did not, a log line sent to a missing folder would be lost, and a
/// folder that a user made first (a junction, say) would send what the agent writes as SYSTEM
/// wherever that user wants. Otherwise nothing is written there and `refused` hears why: the log
/// then stays where it was before the setup existed.
fn vetted_dir(
    dir: Option<PathBuf>,
    usable: &dyn Fn(&Path) -> Result<(), &'static str>,
    refused: &dyn Fn(&str),
) -> Option<PathBuf> {
    let dir = dir?;
    match usable(&dir) {
        Ok(()) => Some(dir),
        Err(why) => {
            refused(&format!(
                "status folder {} {why}; no status file and no log are written there",
                dir.display()
            ));
            None
        }
    }
}

/// Longest stretch the register back-off sleeps without letting the status writer refresh its
/// file (the writer rewrites an unchanged status after 60 s, the tray calls 120 s "not running").
const TICK_SECS: u64 = 30;

/// Sleep `secs` in steps of at most [`TICK_SECS`], calling `tick` after each.
fn sleep_ticking(secs: u64, sleep: &dyn Fn(u64), tick: &dyn Fn()) {
    let mut left = secs;
    while left > 0 {
        let step = left.min(TICK_SECS);
        sleep(step);
        left -= step;
        tick();
    }
}

/// What `run` has once agent.ini is read and its key is known.
struct Prepared {
    cfg: Config,
    settings: HttpSettings,
    reporter: Arc<Mutex<Reporter>>,
}

/// One read of agent.ini for the agent: the settings, and, when a stored key could not be opened,
/// the text that says so (the config then holds no key). `Err` is a file that cannot be used; its
/// text has no quoted line and so no key.
fn read_config(
    base_dir: &Path,
    store: Option<&KeyStore>,
) -> Result<(Config, HttpSettings, Option<String>), String> {
    let text = Config::load_text(base_dir)?;
    let (text, key_text) = match keystore::open(&text, store) {
        Ok(Some(opened)) => (opened, None),
        Ok(None) => (text, None),
        Err(why) => (text, Some(why)),
    };
    let cfg = Config::from_text(&text)
        .map_err(|_| "agent.ini cannot be read (a line that is not valid)".to_string())?;
    let settings = cfg.http_settings()?;
    Ok((cfg, settings, key_text))
}

/// What `start` needs from agent.ini, checked while the agent waits for a key: leaving the wait
/// with one of these missing would end the process, or send an empty key for ever. The texts name
/// a setting and quote no value.
fn startable(cfg: &Config) -> Result<(), String> {
    cfg.require("server_url")?;
    cfg.get("name")?;
    if cfg.require("api_key")?.is_empty() {
        return Err("agent.ini [agent]: the key \"api_key\" is empty".to_string());
    }
    parse_printers(&cfg.require("printers")?)
        .map_err(|_| "agent.ini [agent]: printers cannot be read".to_string())?;
    Ok(())
}

/// agent.ini, the stored key and the status reporter, before anything needs the key: no HTTP
/// client and no journal exists until this returns.
///
/// A stored key that cannot be read (agent.ini copied from another Windows installation, a
/// damaged blob) does not end a setup-installed agent: it says so in the status file, writes the
/// text to the log once, and looks again every minute, so the tray keeps its name, server and
/// printers for the repair, a hand edit works without a restart, and a transient DPAPI failure
/// heals. Without a status folder (an agent set up by hand, with a console or an own service
/// manager) it ends with the text at once. A bad file at the first read ends it as before. The
/// agent never writes agent.ini.
fn prepare(
    base_dir: &Path,
    data_dir: Option<PathBuf>,
    store: Option<&KeyStore>,
    sleep: &dyn Fn(u64),
    log: &dyn Fn(&str),
) -> Result<Prepared, Fatal> {
    // the first read: the text of agent.ini can be read, but the key in it perhaps not
    let (cfg, settings, key_text) = read_config(base_dir, store).map_err(Fatal::Exit)?;
    status::process_started_at();
    let has_status_file = data_dir.is_some();
    let reporter = Arc::new(Mutex::new(Reporter::new(
        Status::from_config(&cfg),
        data_dir.map(StatusWriter::new),
        cfg.get("api_key").ok().flatten().unwrap_or_default(),
    )));
    let report = |f: &dyn Fn(&mut Reporter)| {
        f(&mut reporter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    };
    // The main thread can be busy for minutes (a big job, a call that hangs) and the tray calls a
    // file that is not rewritten for 120 s "not running": a thread of its own keeps it young.
    if has_status_file
        && let Err(e) = status::spawn_heartbeat(&reporter, status::HEARTBEAT, unix_now)
    {
        log::log_error(&format!("no status heartbeat: {e}"));
    }
    let Some(mut text) = key_text else {
        return Ok(Prepared {
            cfg,
            settings,
            reporter,
        });
    };
    if !has_status_file {
        return Err(Fatal::Exit(text));
    }
    let mut logged: Option<String> = None;
    loop {
        report(&|r| r.key_failed(&text, unix_now()));
        if logged.as_deref() != Some(text.as_str()) {
            log(&text);
            logged = Some(text.clone());
        }
        sleep_ticking(60, sleep, &|| report(&|r| r.tick(unix_now())));
        match read_config(base_dir, store) {
            // only a file the agent can start with ends the wait: a half-saved hand edit (the blob
            // line gone, an empty `api_key =`) must not end the process or register with no key
            Ok((cfg, settings, None)) if startable(&cfg).is_ok() => {
                let key = cfg.get("api_key").ok().flatten().unwrap_or_default();
                report(&|r| r.key_opened(Status::from_config(&cfg), key.clone(), unix_now()));
                log("the stored key can be read again");
                return Ok(Prepared {
                    cfg,
                    settings,
                    reporter,
                });
            }
            // still unreadable, a half-saved hand edit, a bad header: the next text, and on
            Ok((_, _, Some(next))) | Err(next) => text = next,
            Ok((cfg, _, None)) => text = startable(&cfg).unwrap_err(),
        }
    }
}

/// The agent: load `<base_dir>/agent.ini`, register, poll forever. Only returns on failure.
pub fn run(base_dir: &Path) -> Result<(), Fatal> {
    let os = install::Os::host();
    let data_dir = vetted_dir(
        setup_data_dir(base_dir, os),
        &|dir| status::usable_status_dir(dir, os),
        &log::log_error,
    );
    if let Some(dir) = &data_dir {
        log::set_log_dir(dir.clone());
    }
    let sleep = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));
    let Prepared {
        cfg,
        settings,
        reporter,
    } = prepare(
        base_dir,
        data_dir,
        keystore::host().as_ref(),
        &sleep,
        &log::log_error,
    )?;
    let report = |f: &dyn Fn(&mut Reporter)| {
        f(&mut reporter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    };
    let http = Client::new(&settings);
    let sumatra = sumatra_program(base_dir);
    let runner = real_runner();
    let platform = Platform::host();
    let caps = select_caps_collector(platform, Rc::clone(&runner));
    let (raw, pdf) = select_backend(platform, &sumatra, runner);
    let fns = Fns::native(raw, pdf);
    let wait = |secs: u64| sleep_ticking(secs, &sleep, &|| report(&|r| r.tick(unix_now())));
    let hooks = Hooks {
        caps: &*caps,
        register: &|base, key, name, printers| {
            register_with_retry(
                &http,
                base,
                key,
                name,
                printers,
                &wait,
                &|msg| {
                    log::log_error(msg);
                    report(&|r| r.register_failed(msg, unix_now()));
                },
                300,
            )
        },
        registered: &|| report(&|r| r.registered(unix_now())),
        poll: &|base, key, map| {
            poll_forever(
                base,
                key,
                map,
                &http,
                &fns,
                &sleep,
                &log::log_error,
                &mut |outcome| report(&|r| r.polled(outcome, unix_now())),
            )
        },
    };
    start(&cfg, &hooks)
}

/// Log every panic to the crash log (and still print it): the agent runs without a console.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::log_error(&format!("panic: {info}"));
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn test_select_backend_by_platform() {
        assert_eq!(Platform::from_name("win32"), Platform::Windows);
        for unixish in ["linux", "darwin"] {
            assert_eq!(Platform::from_name(unixish), Platform::Cups); // macOS is CUPS underneath
        }
        assert_eq!(
            Platform::host(),
            if cfg!(windows) {
                Platform::Windows
            } else {
                Platform::Cups
            }
        );
    }

    #[test]
    fn select_backend_cups_runs_lp_and_windows_runs_sumatra() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        let run: Runner = Rc::new(move |c| {
            s.borrow_mut().push(c.argv.clone());
            Ok(Vec::new())
        });
        let (raw, pdf) = select_backend(Platform::Cups, "S.exe", run.clone());
        raw("Zebra", b"x").unwrap();
        pdf("HP", b"%PDF", None).unwrap();
        let (_, pdf_w) = select_backend(Platform::Windows, "S.exe", run);
        pdf_w("HP", b"%PDF", None).unwrap();
        let seen = seen.borrow();
        assert_eq!(seen[0], ["lp", "-d", "Zebra", "-o", "raw"]);
        assert_eq!(seen[1], ["lp", "-d", "HP"]);
        assert_eq!(seen[2][..3], ["S.exe", "-print-to", "HP"]);
    }

    #[test]
    fn test_select_caps_collector_by_platform() {
        let run: Runner = Rc::new(|_| Err("no tool".into()));
        // CUPS: asks lpoptions (here failing -> None); Windows off Windows: nothing to ask
        assert!(select_caps_collector(Platform::Cups, run.clone())("HP").is_none());
        if !cfg!(windows) {
            assert!(select_caps_collector(Platform::Windows, run)("HP").is_none());
        }
    }

    fn cfg(text: &str) -> Config {
        Config::from_text(text).unwrap()
    }

    #[test]
    fn test_main_registers_with_retry_and_starts_the_poll_loop() {
        // server_url=https://x, api_key=k, printers=p, no name -> default "agent"
        let cfg = cfg("[agent]\nserver_url=https://x/\napi_key=k\nprinters=p\n");
        let calls: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let hooks = Hooks {
            caps: &|_| None,
            register: &|base, key, name, printers| {
                calls
                    .borrow_mut()
                    .push(format!("register {base} {key} {name} {:?}", printers));
                serde_json::json!({"computer_id": 9, "printer_ids": {"p": 1}})
            },
            registered: &|| calls.borrow_mut().push("registered".to_string()),
            poll: &|base, key, map| {
                calls
                    .borrow_mut()
                    .push(format!("poll {base} {key} {}", map.repr()))
            },
        };
        start(&cfg, &hooks).unwrap();
        let calls = calls.borrow();
        assert_eq!(
            calls[0],
            "register https://x k agent [Printer { name: \"p\", can_pdf: false, target: \"p\", capabilities: None }]"
        );
        // "registered" only once the printers are bound, right before the poll
        assert_eq!(calls[1], "registered");
        assert_eq!(
            calls[2],
            "poll https://x k {1: {'name': 'p', 'can_pdf': False, 'target': 'p'}}"
        );
    }

    #[test]
    fn start_attaches_capabilities_and_reports_bad_config() {
        let seen = RefCell::new(None);
        let hooks = Hooks {
            caps: &|t| {
                Some(Capabilities {
                    papers: Some(vec![format!("A4@{t}")]),
                    ..Default::default()
                })
            },
            register: &|_, _, _, printers| {
                *seen.borrow_mut() = Some(printers.to_vec());
                serde_json::json!({"computer_id": 1, "printer_ids": {}})
            },
            registered: &|| {},
            poll: &|_, _, _| {},
        };
        start(
            &cfg("[agent]\nserver_url=u\napi_key=k\nname=pc\nprinters=p; s = socket://h:1\n"),
            &hooks,
        )
        .unwrap();
        let printers = seen.borrow().clone().unwrap();
        assert_eq!(
            printers[0].capabilities.as_ref().unwrap().papers,
            Some(vec!["A4@p".to_string()])
        );
        assert!(printers[1].capabilities.is_none());
        let bad = cfg("[agent]\nserver_url=u\napi_key=k\n");
        assert!(matches!(start(&bad, &hooks), Err(Fatal::Exit(m)) if m.contains("printers")));
        let bad = cfg("[agent]\nserver_url=u\napi_key=k\nprinters=n = socket://h\n");
        assert!(
            matches!(start(&bad, &hooks), Err(Fatal::Crash(m)) if m.contains("expected socket://host:port"))
        );
    }

    #[test]
    fn a_setup_installed_agent_never_looks_up_sumatra_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir
            .path()
            .join("SumatraPDF.exe")
            .to_string_lossy()
            .into_owned();
        // hand-installed, no SumatraPDF next to the exe: PATH lookup as before
        assert_eq!(sumatra_program(dir.path()), "SumatraPDF.exe");
        // setup-installed (runs as SYSTEM) without the file: absolute path, so the job fails
        std::fs::write(dir.path().join(install::MARKER_NAME), "").unwrap();
        assert_eq!(sumatra_program(dir.path()), abs);
        assert!(Path::new(&sumatra_program(dir.path())).is_absolute());
        // with the file, either way
        std::fs::write(dir.path().join("SumatraPDF.exe"), "x").unwrap();
        assert_eq!(sumatra_program(dir.path()), abs);
    }

    #[test]
    fn registered_is_not_reported_when_the_printers_cannot_be_bound() {
        let cfg = cfg("[agent]
server_url=https://x
api_key=k
printers=p
");
        let called = RefCell::new(Vec::<&str>::new());
        let hooks = Hooks {
            caps: &|_| None,
            // a reply that names a printer the agent never sent
            register: &|_, _, _, _| serde_json::json!({"computer_id": 1, "printer_ids": {"zzz": 1}}),
            registered: &|| called.borrow_mut().push("registered"),
            poll: &|_, _, _| called.borrow_mut().push("poll"),
        };
        assert!(matches!(start(&cfg, &hooks), Err(Fatal::Crash(_))));
        assert!(called.borrow().is_empty());
    }

    #[test]
    fn the_register_back_off_keeps_the_status_file_fresh_and_offline() {
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        let key = "SECRET-KEY-123";
        let cfg = cfg(&format!(
            "[agent]
server_url=https://x
api_key={key}
printers=p
"
        ));
        let reporter = RefCell::new(Reporter::new(
            Status::from_config(&cfg),
            Some(StatusWriter::new(dir.path().to_path_buf())),
            key.to_string(),
        ));
        let status = dir.path().join("status.json");
        let clock = Cell::new(1000.0);
        reporter
            .borrow_mut()
            .register_failed("register failed (try 9), retry in 300s: 401", clock.get());
        // the file as the tray would see it: removed after each look, so a rewrite shows up
        let (rewritten_at, last_write) = (RefCell::new(Vec::new()), Cell::new(clock.get()));
        let sleep = |secs: u64| clock.set(clock.get() + secs as f64);
        let tick = || {
            reporter.borrow_mut().tick(clock.get());
            if status.exists() {
                last_write.set(clock.get());
                rewritten_at.borrow_mut().push(clock.get());
                std::fs::remove_file(&status).unwrap();
            }
            assert!(
                clock.get() - last_write.get() <= 60.0,
                "file stale at {}",
                clock.get()
            );
        };
        // the wait the 9th retry sleeps, and the longest one after it
        sleep_ticking(300, &sleep, &tick);
        sleep_ticking(300, &sleep, &tick);
        assert_eq!(clock.get(), 1600.0);
        assert!(rewritten_at.borrow().len() >= 9, "{rewritten_at:?}");
        // and a last look at what was written
        reporter.borrow_mut().tick(clock.get() + 60.0);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&status).unwrap()).unwrap();
        assert_eq!(v["state"], "offline");
        assert_eq!(v["error"], "register failed (try 9), retry in 300s: 401");
    }

    #[test]
    fn sleep_ticking_sleeps_the_whole_time_in_short_steps() {
        let slept = RefCell::new(Vec::new());
        let ticks = RefCell::new(0);
        sleep_ticking(75, &|s| slept.borrow_mut().push(s), &|| {
            *ticks.borrow_mut() += 1
        });
        assert_eq!(*slept.borrow(), vec![30, 30, 15]);
        assert_eq!(*ticks.borrow(), 3);
        slept.borrow_mut().clear();
        sleep_ticking(2, &|s| slept.borrow_mut().push(s), &|| {});
        assert_eq!(*slept.borrow(), vec![2]);
        sleep_ticking(0, &|_| panic!("no time to sleep"), &|| panic!("no tick"));
    }

    #[test]
    fn the_status_folder_is_only_used_when_it_may_be() {
        let told = RefCell::new(Vec::new());
        let refused = |m: &str| told.borrow_mut().push(m.to_string());
        let dir = PathBuf::from("/var/lib/printpapi-agent");
        assert_eq!(
            vetted_dir(Some(dir.clone()), &|_| Ok(()), &refused),
            Some(dir.clone())
        );
        assert!(told.borrow().is_empty());
        // missing, a link or somebody else's: not used, and the reason is logged
        assert_eq!(
            vetted_dir(Some(dir.clone()), &|_| Err("is a link"), &refused),
            None
        );
        assert_eq!(told.borrow().len(), 1);
        assert!(told.borrow()[0].contains("is a link"), "{told:?}");
        assert!(told.borrow()[0].contains("printpapi-agent"), "{told:?}");
        // no setup-installed agent at all: nothing to look at, nothing to say
        assert_eq!(
            vetted_dir(None, &|_| panic!("nothing to look at"), &refused),
            None
        );
        assert_eq!(told.borrow().len(), 1);
    }

    #[test]
    fn a_folder_that_is_not_there_is_not_made_up() {
        let dir = tempfile::tempdir().unwrap();
        let told = RefCell::new(Vec::new());
        let gone = dir.path().join("gone");
        let got = vetted_dir(
            Some(gone.clone()),
            &|d| status::usable_status_dir(d, install::Os::host()),
            &|m| told.borrow_mut().push(m.to_string()),
        );
        assert_eq!(got, None);
        assert!(!gone.exists());
        assert!(told.borrow()[0].contains("is missing"), "{told:?}");
    }

    #[test]
    fn no_status_without_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        // a hand-set-up agent behaves as before: no status file, no log redirection
        assert_eq!(setup_data_dir(dir.path(), install::Os::host()), None);
        std::fs::write(dir.path().join(install::MARKER_NAME), "").unwrap();
        assert_eq!(
            setup_data_dir(dir.path(), install::Os::host()),
            Some(status::status_dir(install::Os::host()))
        );
    }

    #[test]
    fn test_dunder_main_logs_a_bad_agent_ini_before_exiting() {
        // no agent.ini next to the executable: the failure is an `exit:` line for the crash log
        let dir = tempfile::tempdir().unwrap();
        let fatal = run(dir.path()).unwrap_err();
        assert!(matches!(&fatal, Fatal::Exit(m) if m.starts_with("missing or invalid")));
        let log_dir = tempfile::tempdir().unwrap();
        log::append_log(log_dir.path(), &fatal.log_line());
        let text = std::fs::read_to_string(log_dir.path().join("print_agent-error.log")).unwrap();
        assert!(text.contains("exit: missing or invalid"));
    }

    #[test]
    fn a_broken_agent_ini_never_puts_a_value_into_the_fatal_message() {
        // A hand-damaged api_key line makes the parsers quote it. What `main` logs to the crash
        // log (in /tmp on Linux, readable by everybody) and prints must not carry any value.
        let ok = "server_url = https://x\nprinters = a | raw | file:///t\n";
        let key = "api_key = SECRET-KEY-0123\n";
        let cases = [
            // no separator: the parser quotes the line
            format!("[agent]\n{ok}api_key SECRET-KEY-0123\n"),
            // text before the first section
            format!("SECRET-KEY-0123\n[agent]\n{ok}{key}"),
            // a stray % in the key, and a broken reference
            format!("[agent]\n{ok}api_key = SECRET-KEY-0123%KEY\n"),
            format!("[agent]\n{ok}api_key = SECRET-KEY-0123%(nope)s\n"),
            format!("[agent]\n{ok}{key}name = SECRET-KEY-0123%1\n"),
            format!("[agent]\n{ok}{key}timeout = %(api_key)s\n"),
            // a printer entry that pulls the key in
            format!("[agent]\nserver_url = https://x\n{key}printers = n = socket://%(api_key)s\n"),
        ];
        for text in cases {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("agent.ini"), &text).unwrap();
            let fatal = run(dir.path()).unwrap_err();
            let (Fatal::Exit(m) | Fatal::Crash(m)) = &fatal;
            assert!(!m.is_empty(), "{text}");
            assert!(!m.contains("SECRET"), "{text}: {m}");
            assert!(!fatal.log_line().contains("SECRET"), "{text}");
        }
    }

    #[test]
    fn a_bad_timeout_or_header_stops_start_up_before_any_network() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.ini"),
            "[agent]\nserver_url=http://x\napi_key=k\nprinters=p\ntimeout=5\n",
        )
        .unwrap();
        assert!(matches!(run(dir.path()), Err(Fatal::Exit(m)) if m.contains("must be >= 30")));
    }

    // ---- the stored key at start ----

    fn xor(b: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(b.iter().map(|x| x ^ 0x5a).collect())
    }
    fn refuse(_: &[u8]) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::other("refused"))
    }
    const STORE: keystore::KeyStore = keystore::KeyStore {
        protect: xor,
        unprotect: xor,
    };
    /// A store that wrote the blob but cannot read it any more (another installation).
    const OTHER_PC: keystore::KeyStore = keystore::KeyStore {
        protect: xor,
        unprotect: refuse,
    };
    const KEY: &str = "SECRET-KEY-123";
    const PLAIN: &str = "[agent]\nserver_url = https://x\napi_key = SECRET-KEY-123\nname = pc\nprinters = Büro|pdf\n";

    struct Start {
        base: tempfile::TempDir,
        status: tempfile::TempDir,
    }

    impl Start {
        fn new(ini: &str) -> Start {
            let s = Start {
                base: tempfile::tempdir().unwrap(),
                status: tempfile::tempdir().unwrap(),
            };
            s.write(ini);
            s
        }
        fn write(&self, ini: &str) {
            std::fs::write(self.base.path().join("agent.ini"), ini).unwrap();
        }
        fn status(&self) -> Value {
            let text = std::fs::read_to_string(self.status.path().join("status.json")).unwrap();
            serde_json::from_str(&text).unwrap()
        }
        fn prepare(
            &self,
            store: &keystore::KeyStore,
            sleep: &dyn Fn(u64),
            log: &dyn Fn(&str),
        ) -> Result<Prepared, Fatal> {
            prepare(
                self.base.path(),
                Some(self.status.path().to_path_buf()),
                Some(store),
                sleep,
                log,
            )
        }
    }

    fn sealed() -> String {
        keystore::seal(PLAIN, Some(&STORE)).unwrap().unwrap()
    }

    #[test]
    fn prepare_waits_while_the_stored_key_cannot_be_read_and_heals_itself() {
        let start = Start::new(&sealed());
        let sleeps = RefCell::new(Vec::new());
        let logs = RefCell::new(Vec::new());
        let sleep = |secs: u64| {
            sleeps.borrow_mut().push(secs);
            let n = sleeps.borrow().len();
            if n == 1 {
                // what the tray and the window see while the agent waits
                let v = start.status();
                assert_eq!(v["state"], "offline");
                assert_eq!(v["key_unreadable"], true);
                assert_eq!(v["error"], keystore::UNREADABLE);
                assert_eq!(v["name"], "pc");
                assert_eq!(v["printers"][0]["name"], "Büro");
                assert!(!v.to_string().contains(KEY));
            }
            if n == 4 {
                // the administrator puts the key in by hand
                start.write(PLAIN);
            }
        };
        let log = |m: &str| logs.borrow_mut().push(m.to_string());
        let prepared = start.prepare(&OTHER_PC, &sleep, &log).unwrap();
        // 60 s per try, in steps the status file stays young through
        assert_eq!(*sleeps.borrow(), vec![30; 4]);
        assert_eq!(prepared.cfg.require("api_key").unwrap(), KEY);
        let v = start.status();
        assert_eq!(v["key_unreadable"], false);
        assert_eq!(v["error"], Value::Null);
        // one line for the text, one for the repair
        assert_eq!(
            *logs.borrow(),
            vec![
                keystore::UNREADABLE.to_string(),
                "the stored key can be read again".to_string()
            ]
        );
        assert!(prepared.settings.headers.is_empty());
    }

    #[test]
    fn prepare_keeps_waiting_through_a_half_saved_file_and_logs_each_text_once() {
        let start = Start::new(&sealed());
        let sleeps = RefCell::new(0u32);
        let logs = RefCell::new(Vec::new());
        let sleep = |_| {
            *sleeps.borrow_mut() += 1;
            match *sleeps.borrow() {
                // a hand edit that is not finished: a key twice, the secret on a line of its own
                4 => start.write("[agent]\napi_key = SECRET-KEY-123\napi_key = SECRET-KEY-123\n"),
                // a header the agent would refuse
                6 => start.write(&format!("{PLAIN}[headers]\nauthorization = x\n")),
                8 => start.write(PLAIN),
                _ => {}
            }
        };
        let log = |m: &str| logs.borrow_mut().push(m.to_string());
        start.prepare(&OTHER_PC, &sleep, &log).unwrap();
        let logs = logs.borrow();
        assert_eq!(logs.len(), 4, "{logs:?}");
        assert_eq!(logs[0], keystore::UNREADABLE);
        assert!(
            logs[1].contains("invalid") && logs[2].contains("[headers]"),
            "{logs:?}"
        );
        assert_eq!(logs[3], "the stored key can be read again");
        assert!(logs.iter().all(|l| !l.contains(KEY)), "{logs:?}");
        // it waited through every try
        assert_eq!(*sleeps.borrow(), 8);
    }

    #[test]
    fn prepare_keeps_waiting_while_a_half_saved_file_has_no_usable_key() {
        let start = Start::new(&sealed());
        let sleeps = RefCell::new(0u32);
        let logs = RefCell::new(Vec::new());
        let sleep = |_| {
            *sleeps.borrow_mut() += 1;
            let n = *sleeps.borrow();
            match n {
                // the blob line is gone, the key is not typed yet
                4 => start.write(&PLAIN.replace("api_key = SECRET-KEY-123\n", "")),
                // an empty key, saved before the paste
                6 => start.write(&PLAIN.replace("SECRET-KEY-123", "")),
                // no server yet
                8 => start.write(&PLAIN.replace("server_url = https://x\n", "")),
                // a name that start would refuse
                10 => start.write(&PLAIN.replace("name = pc", "name = 50%")),
                12 => start.write(PLAIN),
                _ => {}
            }
            if matches!(n, 5 | 7 | 9 | 11) {
                // the agent is still waiting, and says so
                let v = start.status();
                assert_eq!(v["key_unreadable"], true, "try {n}");
                assert_eq!(v["state"], "offline", "try {n}");
                assert_eq!(v["name"], "pc", "try {n}");
            }
        };
        let log = |m: &str| logs.borrow_mut().push(m.to_string());
        let prepared = start.prepare(&OTHER_PC, &sleep, &log).unwrap();
        assert_eq!(*sleeps.borrow(), 12);
        assert_eq!(prepared.cfg.require("api_key").unwrap(), KEY);
        let logs = logs.borrow();
        assert_eq!(logs.len(), 6, "{logs:?}");
        assert_eq!(logs[0], keystore::UNREADABLE);
        assert!(logs[1].contains("api_key"), "{logs:?}");
        assert!(
            logs[2].contains("api_key") && logs[2].contains("empty"),
            "{logs:?}"
        );
        assert!(logs[3].contains("server_url"), "{logs:?}");
        assert!(logs[4].contains("name"), "{logs:?}");
        assert_eq!(logs[5], "the stored key can be read again");
        assert_eq!(start.status()["key_unreadable"], false);
    }

    #[test]
    fn prepare_without_a_status_folder_ends_at_once_with_the_text() {
        let start = Start::new(&sealed());
        let sleep = |_| panic!("no waiting without a status folder");
        let e = prepare(
            start.base.path(),
            None,
            Some(&OTHER_PC),
            &sleep,
            &|_: &str| {},
        )
        .err()
        .unwrap();
        assert_eq!(e, Fatal::Exit(keystore::UNREADABLE.to_string()));
        // and a system that has no store at all
        let e = prepare(start.base.path(), None, None, &sleep, &|_: &str| {})
            .err()
            .unwrap();
        assert_eq!(e, Fatal::Exit(keystore::NOT_ON_THIS_SYSTEM.to_string()));
    }

    #[test]
    fn a_bad_agent_ini_at_the_first_read_still_ends_the_agent() {
        let sleep = |_| panic!("no waiting for a bad file");
        for (ini, needle) in [
            ("[agent]\napi_key = a\napi_key = b\n", "invalid"),
            ("[other]\nx = 1\n", "missing or invalid"),
            (
                "[agent]\nserver_url = https://x\napi_key = k\nprinters = p\ntimeout = 5\n",
                "timeout",
            ),
        ] {
            let start = Start::new(ini);
            let e = start.prepare(&STORE, &sleep, &|_: &str| {}).err().unwrap();
            let Fatal::Exit(text) = e else {
                panic!("{ini}")
            };
            assert!(text.contains(needle), "{text}");
        }
        // a file that is not there at all
        let start = Start::new(PLAIN);
        std::fs::remove_file(start.base.path().join("agent.ini")).unwrap();
        assert!(matches!(
            start.prepare(&STORE, &sleep, &|_: &str| {}).err(),
            Some(Fatal::Exit(_))
        ));
    }

    #[test]
    fn prepare_opens_a_readable_key_and_a_plain_one_without_waiting() {
        let sleep = |_| panic!("nothing to wait for");
        for ini in [sealed(), PLAIN.to_string()] {
            let start = Start::new(&ini);
            let prepared = start.prepare(&STORE, &sleep, &|_: &str| {}).unwrap();
            assert_eq!(prepared.cfg.require("api_key").unwrap(), KEY);
            // the masking key is the real one from the first moment
            let mut r = prepared.reporter.lock().unwrap();
            r.register_failed(&format!("401 for {KEY}"), 1.0);
            drop(r);
            let v = start.status();
            assert!(!v.to_string().contains(KEY), "{v}");
            assert_eq!(v["key_unreadable"], false);
        }
        // a plain key next to a blob that nobody can read is no reason to wait either
        let start = Start::new(
            &sealed().replace("api_key_dpapi", "api_key = SECRET-KEY-123\napi_key_dpapi"),
        );
        let prepared = start.prepare(&OTHER_PC, &sleep, &|_: &str| {}).unwrap();
        assert_eq!(prepared.cfg.require("api_key").unwrap(), KEY);
    }

    #[test]
    fn once_the_key_is_readable_it_is_masked_in_error_texts() {
        let start = Start::new(&sealed());
        let sleeps = RefCell::new(0u32);
        let sleep = |_| {
            *sleeps.borrow_mut() += 1;
            if *sleeps.borrow() == 2 {
                start.write(PLAIN);
            }
        };
        let prepared = start.prepare(&OTHER_PC, &sleep, &|_: &str| {}).unwrap();
        prepared
            .reporter
            .lock()
            .unwrap()
            .register_failed(&format!("the server said no to {KEY}"), 5.0);
        let v = start.status();
        assert_eq!(v["error"], "the server said no to ***");
    }
}
