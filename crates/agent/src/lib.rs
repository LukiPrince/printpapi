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

use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use serde_json::Value;

use client::Client;
use config::{Capabilities, Config, Printer, parse_printers};
use print::{Fns, add_capabilities};
use proc::{Runner, real_runner};
use run::{PrinterMap, build_printer_map, poll_forever, register_with_retry};

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
    pub poll: &'a dyn Fn(&str, &str, &PrinterMap),
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
    let printers =
        parse_printers(&cfg.require("printers").map_err(Fatal::Exit)?).map_err(Fatal::Crash)?;
    let printers = add_capabilities(printers, hooks.caps);
    let reg = (hooks.register)(&base, &key, &name, &printers);
    let printer_by_id = build_printer_map(&printers, &reg).map_err(Fatal::Crash)?;
    out_line(&format!(
        "print-agent registered as computer {}, printers={}",
        pyrepr::py_str(&reg["computer_id"]),
        printer_by_id.repr()
    ));
    (hooks.poll)(&base, &key, &printer_by_id);
    Ok(())
}

/// The agent: load `<base_dir>/agent.ini`, register, poll forever. Only returns on failure.
pub fn run(base_dir: &Path) -> Result<(), Fatal> {
    let cfg = Config::load(base_dir).map_err(Fatal::Exit)?;
    let settings = cfg.http_settings().map_err(Fatal::Exit)?;
    let http = Client::new(&settings);
    let bundled = base_dir.join("SumatraPDF.exe");
    let sumatra = if bundled.exists() {
        bundled.to_string_lossy().into_owned()
    } else {
        "SumatraPDF.exe".to_string()
    };
    let runner = real_runner();
    let platform = Platform::host();
    let caps = select_caps_collector(platform, Rc::clone(&runner));
    let (raw, pdf) = select_backend(platform, &sumatra, runner);
    let fns = Fns::native(raw, pdf);
    let sleep = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));
    let hooks = Hooks {
        caps: &*caps,
        register: &|base, key, name, printers| {
            register_with_retry(
                &http,
                base,
                key,
                name,
                printers,
                &sleep,
                &log::log_error,
                300,
            )
        },
        poll: &|base, key, map| poll_forever(base, key, map, &http, &fns, &sleep, &log::log_error),
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
        assert_eq!(
            calls[1],
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
