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
pub mod log;
pub mod print;
pub mod proc;
pub mod pyrepr;
pub mod run;
pub mod setup;
pub mod status;

use std::cell::RefCell;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::Value;

use client::Client;
use config::{Capabilities, Config, Printer, parse_printers};
use print::{Fns, add_capabilities};
use proc::{Runner, real_runner};
use run::{PrinterMap, build_printer_map, poll_forever, register_with_retry};
use status::{Reporter, Status, StatusWriter, unix_now};

pub use print::{PdfFn, RawFn};
type RegisterFn<'a> = dyn Fn(&str, &str, &str, &[Printer]) -> Value + 'a;
pub type CapsFn = Box<dyn Fn(&str) -> Option<Capabilities>>;

/// Print to stdout, ignoring a closed or missing stdout (a background run has none).
pub fn out_line(msg: &str) {
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

/// The folder only if it exists. Setup creates it with its permissions; an agent must not create
/// one that setup did not, and a log line sent to a missing folder would be lost.
fn existing_dir(dir: &Option<PathBuf>) -> Option<&PathBuf> {
    dir.as_ref().filter(|d| d.is_dir())
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

/// The agent: load `<base_dir>/agent.ini`, register, poll forever. Only returns on failure.
pub fn run(base_dir: &Path) -> Result<(), Fatal> {
    let data_dir = setup_data_dir(base_dir, install::Os::host());
    if let Some(dir) = existing_dir(&data_dir) {
        log::set_log_dir(dir.clone());
    }
    let cfg = Config::load(base_dir).map_err(Fatal::Exit)?;
    let settings = cfg.http_settings().map_err(Fatal::Exit)?;
    let reporter = RefCell::new(Reporter::new(
        Status::from_config(&cfg),
        data_dir.map(StatusWriter::new),
        cfg.get("api_key").ok().flatten().unwrap_or_default(),
    ));
    let http = Client::new(&settings);
    let sumatra = sumatra_program(base_dir);
    let runner = real_runner();
    let platform = Platform::host();
    let caps = select_caps_collector(platform, Rc::clone(&runner));
    let (raw, pdf) = select_backend(platform, &sumatra, runner);
    let fns = Fns::native(raw, pdf);
    let sleep = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));
    let wait = |secs: u64| sleep_ticking(secs, &sleep, &|| reporter.borrow_mut().tick(unix_now()));
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
                    reporter.borrow_mut().register_failed(msg, unix_now());
                },
                300,
            )
        },
        registered: &|| reporter.borrow_mut().registered(unix_now()),
        poll: &|base, key, map| {
            poll_forever(
                base,
                key,
                map,
                &http,
                &fns,
                &sleep,
                &log::log_error,
                &mut |outcome| reporter.borrow_mut().polled(outcome, unix_now()),
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
    fn the_log_only_moves_into_a_folder_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let there = Some(dir.path().to_path_buf());
        assert_eq!(existing_dir(&there), there.as_ref());
        // a missing folder (setup has not made it, or it was deleted) keeps the old log place
        assert_eq!(existing_dir(&Some(dir.path().join("gone"))), None);
        assert_eq!(existing_dir(&None), None);
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
}
