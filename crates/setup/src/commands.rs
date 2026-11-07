// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The commands the setup window calls (the contract is `web/setup/api.ts`). The rules live in
//! `printpapi_agent`; this file only joins them to the window: it keeps the working folder and the
//! checked connection between the steps, asks for administrator rights, and turns every failure
//! into `{ code, message }`.
//!
//! The key stays in this process. It reaches the elevated agent through `agent.ini` in the private
//! staging folder, never through an argument, and it never goes back to the window.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use printpapi_agent::apply::{SUMATRA_EXE_SHA256, SUMATRA_URL, SUMATRA_ZIP_SHA256, verify_sha256};
use printpapi_agent::client::Client;
use printpapi_agent::config::{
    Config, HttpSettings, IniUpdate, Printer, decode_ini, format_printers, parse_printers,
    update_ini,
};
use printpapi_agent::discover::discover;
use printpapi_agent::install::{self, Installed, Layout, Os};
use printpapi_agent::proc::{Cmd, Runner, run_command_with};
use printpapi_agent::{Platform, setup};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::elevate::{self, Elevate};
use crate::staging::Staging;

/// How the window is told that something failed. `code` is a stable word it can branch on
/// (`proxy`, `cancelled`, ...), `message` is text for the person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SetupError {
    code: String,
    message: String,
}

impl SetupError {
    fn new(code: &str, message: impl Into<String>) -> SetupError {
        SetupError {
            code: code.into(),
            message: message.into(),
        }
    }

    fn io(what: &str, e: std::io::Error) -> SetupError {
        SetupError::new("system", format!("{what}: {e}"))
    }
}

impl From<setup::ConnectError> for SetupError {
    fn from(e: setup::ConnectError) -> SetupError {
        SetupError::new(e.code(), e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FoundPrinter {
    name: String,
    per_user: bool,
    usable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterChoice {
    name: String,
    pdf: bool,
    target: Option<String>,
    locked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Existing {
    server_url: String,
    name: String,
    printers: Vec<PrinterChoice>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRequest {
    server_url: Option<String>,
    api_key: Option<String>,
    name: Option<String>,
    printers: Vec<PrinterChoice>,
}

/// What the steps share. Nothing in here is printed, logged or sent to the window.
struct Conn {
    server_url: String,
    api_key: String,
    name: String,
    settings: HttpSettings,
}

#[derive(Default)]
pub struct AppState {
    staging: Option<Staging>,
    conn: Option<Conn>,
    /// The installed agent.ini while its printers are being changed, so that `[headers]`,
    /// `timeout` and comments survive the rewrite.
    ini_text: Option<String>,
}

impl AppState {
    fn staging(&mut self) -> Result<&Staging, SetupError> {
        match &mut self.staging {
            Some(s) => Ok(s),
            slot @ None => Ok(slot.insert(
                Staging::new()
                    .map_err(|e| SetupError::io("cannot create the working folder", e))?,
            )),
        }
    }

    /// Delete the working folder now (the window closes without dropping managed state).
    pub fn clear(&mut self) {
        self.staging = None;
        self.conn = None;
        self.ini_text = None;
    }
}

type Shared<'a> = State<'a, Mutex<AppState>>;

fn lock<'a>(state: &'a Shared) -> MutexGuard<'a, AppState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- machine

fn layout() -> Layout {
    let program_files =
        std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
    install::layout(Os::host(), Path::new(&program_files))
}

/// Windows `CREATE_NO_WINDOW`: without it a console window flashes up for every `schtasks`, `sc`
/// and `tasklist` the detection runs.
/// https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
#[cfg(windows)]
fn no_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn no_window(_: &mut std::process::Command) {}

fn runner() -> Runner {
    Rc::new(|cmd: &Cmd| run_command_with(cmd, &no_window))
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "windows",
        Os::Macos => "macos",
        Os::Linux => "linux",
    }
}

fn installed_name(os: Os, found: Installed) -> &'static str {
    if !install::supported(os) {
        return "unsupported";
    }
    match found {
        Installed::None => "none",
        Installed::Managed => "managed",
        Installed::Foreign => "foreign",
    }
}

// ---- pure helpers

pub fn choice_to_printer(c: &PrinterChoice) -> Printer {
    Printer::new(&c.name, c.pdf, c.target.as_deref().unwrap_or(&c.name))
}

/// A printer of the installed `agent.ini`. A `file://` target is kept as it is and locked: the
/// window does not edit those.
pub fn printer_to_choice(p: &Printer) -> PrinterChoice {
    PrinterChoice {
        name: p.name.clone(),
        pdf: p.can_pdf,
        target: (p.target != p.name).then(|| p.target.clone()),
        locked: p.target.starts_with("file://"),
    }
}

/// What a finished elevated run comes to: the error of a cancelled prompt, the agent's own
/// `result.json` (it knows why it failed), or, without one, what the prompt said.
pub fn outcome(elevated: Result<(), Elevate>, result: Option<Value>) -> Result<Value, SetupError> {
    if elevated == Err(Elevate::Cancelled) {
        return Err(SetupError::new(
            "cancelled",
            "Administrator rights were not granted. Nothing was changed.",
        ));
    }
    match (result, elevated) {
        (Some(v), _) if v["ok"] == Value::Bool(false) => Err(SetupError::new(
            v["code"].as_str().unwrap_or("system"),
            v["message"]
                .as_str()
                .unwrap_or("The agent reported an error."),
        )),
        (Some(v), Ok(())) if v["ok"] == Value::Bool(true) => Ok(v),
        (_, Err(Elevate::Failed(m))) => Err(SetupError::new("system", m)),
        _ => Err(SetupError::new(
            "system",
            "The agent did not report a result.",
        )),
    }
}

/// `check` until it is true, at most `attempts` times, `pause` between tries.
pub fn poll(attempts: u32, pause: &dyn Fn(), check: &mut dyn FnMut() -> bool) -> bool {
    for n in 0..attempts {
        if check() {
            return true;
        }
        if n + 1 < attempts {
            pause();
        }
    }
    false
}

/// The `agent.ini` the install writes. A new install has no earlier text; changing printers sets
/// only `printers` and leaves everything else of the installed file as it is.
pub fn build_ini(
    existing: Option<&str>,
    conn: Option<(&str, &str, &str)>,
    printers: &[PrinterChoice],
) -> Result<String, String> {
    let list: Vec<Printer> = printers.iter().map(choice_to_printer).collect();
    let printers = format_printers(&list)?;
    let update = IniUpdate {
        server_url: conn.map(|c| c.0),
        api_key: conn.map(|c| c.1),
        name: conn.map(|c| c.2),
        printers: &printers,
    };
    Ok(update_ini(existing, &update))
}

fn read_result(dir: &Path) -> Option<Value> {
    let bytes = std::fs::read(dir.join("result.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Run one admin subcommand on the staging folder and read what it reports.
fn run_elevated(agent: &Path, dir: &Path, command: &str) -> Result<Value, SetupError> {
    // an old result must not be mistaken for the answer of this run
    let _ = std::fs::remove_file(dir.join("result.json"));
    let elevated = elevate::run(agent, &[OsStr::new(command), dir.as_os_str()]);
    outcome(elevated, read_result(dir))
}

fn staged(state: &Shared) -> Result<(PathBuf, PathBuf), SetupError> {
    let mut guard = lock(state);
    let staging = guard.staging()?;
    let agent = staging
        .agent()
        .map_err(|e| SetupError::io("cannot unpack the agent", e))?;
    Ok((agent, staging.dir().to_path_buf()))
}

// ---- commands

#[tauri::command(async)]
pub fn get_platform() -> &'static str {
    os_name(Os::host())
}

#[tauri::command(async)]
pub fn detect_install() -> &'static str {
    let os = Os::host();
    let found = if install::supported(os) {
        install::detect(os, &layout(), &install::system_dir(), &runner())
    } else {
        Installed::None
    };
    installed_name(os, found)
}

#[tauri::command(async)]
pub fn default_name() -> String {
    setup::default_device_name()
}

#[tauri::command(async)]
pub fn needs_insecure_confirm(url: String) -> bool {
    setup::needs_insecure_confirm(&url)
}

#[tauri::command(async)]
pub fn check_connection(
    state: Shared,
    server_url: String,
    api_key: String,
    name: String,
) -> Result<bool, SetupError> {
    let server_url = setup::normalize_server_url(&server_url)?;
    let settings = HttpSettings::default();
    let client = Client::new(&settings);
    let exists = setup::check_connection(&client, &server_url, &api_key, &name)?;
    lock(&state).conn = Some(Conn {
        server_url,
        api_key,
        name,
        settings,
    });
    Ok(exists)
}

#[tauri::command(async)]
pub fn list_printers() -> Vec<FoundPrinter> {
    discover(Platform::host(), &runner())
        .into_iter()
        .map(|f| FoundPrinter {
            name: f.name,
            per_user: f.per_user,
            usable: f.usable,
        })
        .collect()
}

#[tauri::command(async)]
pub fn read_existing(state: Shared) -> Result<Existing, SetupError> {
    let (agent, dir) = staged(&state)?;
    let ini = dir.join("agent.ini");
    let _ = std::fs::remove_file(&ini);
    let ran = run_elevated(&agent, &dir, "read-config");
    let text = std::fs::read(&ini).map(|b| decode_ini(&b));
    // the copy holds the key: gone before anything else happens
    let _ = std::fs::remove_file(&ini);
    ran?;
    let text = text.map_err(|e| SetupError::io("cannot read the installed settings", e))?;

    let unreadable = || SetupError::new("system", "The installed agent.ini cannot be read.");
    let cfg = Config::from_text(&text).map_err(|_| unreadable())?;
    let get = |key: &str| cfg.get(key).ok().flatten();
    let (server_url, api_key) = (get("server_url"), get("api_key"));
    let name = get("name").unwrap_or_else(|| "agent".into());
    let printers =
        parse_printers(&get("printers").unwrap_or_default()).map_err(|_| unreadable())?;
    let settings = cfg.http_settings().map_err(|_| unreadable())?;
    let (Some(server_url), Some(api_key)) = (server_url, api_key) else {
        return Err(unreadable());
    };

    let existing = Existing {
        server_url: server_url.clone(),
        name: name.clone(),
        printers: printers.iter().map(printer_to_choice).collect(),
    };
    let mut guard = lock(&state);
    guard.conn = Some(Conn {
        server_url,
        api_key,
        name,
        settings,
    });
    guard.ini_text = Some(text);
    Ok(existing)
}

fn sumatra_in(dir: &Path) -> PathBuf {
    dir.join("SumatraPDF.exe")
}

#[tauri::command(async)]
pub fn sumatra_present(state: Shared) -> Result<bool, SetupError> {
    let mut guard = lock(&state);
    Ok(layout().sumatra.is_file() || sumatra_in(guard.staging()?.dir()).is_file())
}

/// SumatraPDF where the test print finds it: next to the installed agent, else the downloaded copy.
fn sumatra_for_test(installed: &Path, staging: &Path) -> PathBuf {
    if installed.is_file() {
        installed.to_path_buf()
    } else {
        sumatra_in(staging)
    }
}

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
/// The ZIP is about 8 MB.
const MAX_SUMATRA_ZIP: u64 = 64 * 1024 * 1024;

fn download(url: &str) -> Result<Vec<u8>, String> {
    use ureq::config::Config;
    use ureq::tls::{RootCerts, TlsConfig};
    let agent = ureq::Agent::new_with_config(
        Config::builder()
            .user_agent(BROWSER_UA)
            .timeout_global(Some(Duration::from_secs(180)))
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build(),
    );
    let mut reply = agent.get(url).call().map_err(|e| e.to_string())?;
    reply
        .body_mut()
        .with_config()
        .limit(MAX_SUMATRA_ZIP)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// Load the pinned SumatraPDF into the staging folder as `SumatraPDF.exe`. ZIP and exe are checked
/// against the SHA-256 built into the agent; the elevated install checks the exe once more.
fn fetch_sumatra(dir: &Path, system_dir: &Path, run: &Runner) -> Result<(), SetupError> {
    let bad = |what: &str| SetupError::new("sumatra", format!("SumatraPDF: {what}"));
    let zip = dir.join("SumatraPDF.zip");
    let extracted = dir.join("sumatra-extract");
    let result = (|| {
        let bytes = download(SUMATRA_URL).map_err(|e| bad(&format!("download failed ({e})")))?;
        std::fs::write(&zip, bytes).map_err(|e| SetupError::io("cannot save SumatraPDF", e))?;
        verify_sha256(&zip, SUMATRA_ZIP_SHA256)
            .map_err(|_| bad("the download is not the expected file"))?;
        let _ = std::fs::remove_dir_all(&extracted);
        std::fs::create_dir(&extracted).map_err(|e| SetupError::io("cannot unpack", e))?;
        let tar = system_dir.join("tar.exe");
        let argv = [
            tar.to_string_lossy().into_owned(),
            "-xf".into(),
            zip.to_string_lossy().into_owned(),
            "-C".into(),
            extracted.to_string_lossy().into_owned(),
        ];
        run(&Cmd::new(argv.to_vec(), 60)).map_err(|e| bad(&format!("cannot unpack ({e})")))?;
        let target = sumatra_in(dir);
        let _ = std::fs::remove_file(&target);
        std::fs::rename(extracted.join("SumatraPDF-3.6.1-64.exe"), &target)
            .map_err(|e| SetupError::io("cannot unpack", e))?;
        if verify_sha256(&target, SUMATRA_EXE_SHA256).is_err() {
            let _ = std::fs::remove_file(&target);
            return Err(bad("the program in the download is not the expected one"));
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_dir_all(&extracted);
    result
}

#[tauri::command(async)]
pub fn download_sumatra(state: Shared) -> Result<(), SetupError> {
    if !cfg!(windows) {
        return Err(SetupError::new(
            "sumatra",
            "SumatraPDF is only needed on Windows.",
        ));
    }
    let dir = lock(&state).staging()?.dir().to_path_buf();
    fetch_sumatra(&dir, &install::system_dir(), &runner())
}

#[tauri::command(async)]
pub fn test_print(state: Shared, printer: PrinterChoice) -> Result<(), SetupError> {
    let dir = lock(&state).staging()?.dir().to_path_buf();
    let sumatra = sumatra_for_test(&layout().sumatra, &dir);
    setup::test_print(
        &choice_to_printer(&printer),
        &sumatra.to_string_lossy(),
        runner(),
    )
    .map_err(|m| SetupError::new("print", m))
}

#[tauri::command(async)]
pub fn apply(state: Shared, request: ApplyRequest) -> Result<f64, SetupError> {
    let (agent, dir) = staged(&state)?;
    let new_conn = match (&request.server_url, &request.api_key, &request.name) {
        (Some(url), Some(key), Some(name)) => {
            Some((setup::normalize_server_url(url)?, key.clone(), name.clone()))
        }
        _ => None,
    };
    let existing = lock(&state).ini_text.clone();
    if new_conn.is_none() && existing.is_none() {
        return Err(SetupError::new(
            "system",
            "There is no connection to install with. Go back to the first step.",
        ));
    }
    let text = build_ini(
        if new_conn.is_some() {
            None
        } else {
            existing.as_deref()
        },
        new_conn
            .as_ref()
            .map(|(u, k, n)| (u.as_str(), k.as_str(), n.as_str())),
        &request.printers,
    )
    .map_err(|m| SetupError::new("printers", m))?;

    let ini = dir.join("agent.ini");
    crate::staging::write_private(&dir, "agent.ini", text.as_bytes())
        .map_err(|e| SetupError::io("cannot write the settings", e))?;
    let ran = run_elevated(&agent, &dir, "install");
    // the file holds the key: gone whatever happened
    let _ = std::fs::remove_file(&ini);
    let baseline = ran?["baseline_last_seen"]
        .as_f64()
        .ok_or_else(|| SetupError::new("system", "The agent did not report a start time."))?;

    if let Some((server_url, api_key, name)) = new_conn {
        lock(&state).conn = Some(Conn {
            server_url,
            api_key,
            name,
            settings: HttpSettings::default(),
        });
    }
    Ok(baseline)
}

const ONLINE_TRIES: u32 = 30;

#[tauri::command(async)]
pub fn wait_online(state: Shared, baseline: f64) -> Result<bool, SetupError> {
    let (url, key, name, settings) = {
        let guard = lock(&state);
        let Some(c) = &guard.conn else {
            return Err(SetupError::new(
                "system",
                "There is no connection to check.",
            ));
        };
        (
            c.server_url.clone(),
            c.api_key.clone(),
            c.name.clone(),
            c.settings.clone(),
        )
    };
    let client = Client::new(&settings);
    // a failed look (the network, a restart of the server) is "not yet", not the end
    Ok(poll(
        ONLINE_TRIES,
        &|| std::thread::sleep(Duration::from_secs(2)),
        &mut || setup::agent_seen_after(&client, &url, &key, &name, baseline).unwrap_or(false),
    ))
}

#[tauri::command(async)]
pub fn uninstall(state: Shared) -> Result<(), SetupError> {
    let (agent, dir) = staged(&state)?;
    run_elevated(&agent, &dir, "uninstall")?;
    let mut guard = lock(&state);
    guard.conn = None;
    guard.ini_text = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    fn choice(name: &str, pdf: bool, target: Option<&str>) -> PrinterChoice {
        PrinterChoice {
            name: name.into(),
            pdf,
            target: target.map(String::from),
            locked: false,
        }
    }

    #[test]
    fn a_choice_without_target_prints_to_its_own_name() {
        let p = choice_to_printer(&choice("Büro Drucker", true, None));
        assert_eq!(
            (p.name.as_str(), p.can_pdf, p.target.as_str()),
            ("Büro Drucker", true, "Büro Drucker")
        );
        let p = choice_to_printer(&choice("Zebra", false, Some("socket://10.0.0.5:9100")));
        assert_eq!(p.target, "socket://10.0.0.5:9100");
    }

    #[test]
    fn installed_printers_come_back_as_choices_and_file_targets_are_locked() {
        let list = parse_printers(
            "Büro Drucker|pdf ; Zebra = socket://10.0.0.5:9100 ; Out = file:///tmp/out",
        )
        .unwrap();
        let choices: Vec<_> = list.iter().map(printer_to_choice).collect();
        assert_eq!(choices[0], choice("Büro Drucker", true, None));
        assert_eq!(
            choices[1],
            choice("Zebra", false, Some("socket://10.0.0.5:9100"))
        );
        assert!(choices[2].locked && choices[2].target.as_deref() == Some("file:///tmp/out"));
        assert!(!choices[0].locked && !choices[1].locked);
    }

    #[test]
    fn a_cancelled_prompt_is_cancelled_whatever_an_old_result_says() {
        let e = outcome(Err(Elevate::Cancelled), Some(json!({"ok": true}))).unwrap_err();
        assert_eq!(e.code, "cancelled");
    }

    #[test]
    fn the_agents_own_error_wins_over_the_exit_code() {
        let result = json!({"ok": false, "code": "proxy", "message": "A proxy refused."});
        let e = outcome(Err(Elevate::Failed("exit code 1".into())), Some(result)).unwrap_err();
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            ("proxy", "A proxy refused.")
        );
    }

    #[test]
    fn a_run_without_a_result_is_an_error_even_when_it_exited_cleanly() {
        assert_eq!(outcome(Ok(()), None).unwrap_err().code, "system");
        assert_eq!(
            outcome(Err(Elevate::Failed("no pkexec".into())), None)
                .unwrap_err()
                .message,
            "no pkexec"
        );
        // success needs both
        assert!(outcome(Err(Elevate::Failed("x".into())), Some(json!({"ok": true}))).is_err());
        assert_eq!(
            outcome(
                Ok(()),
                Some(json!({"ok": true, "baseline_last_seen": 12.5}))
            )
            .unwrap()["baseline_last_seen"],
            12.5
        );
    }

    #[test]
    fn poll_stops_at_the_first_yes_and_pauses_only_between_tries() {
        let pauses = Cell::new(0);
        let calls = RefCell::new(0);
        let found = poll(5, &|| pauses.set(pauses.get() + 1), &mut || {
            *calls.borrow_mut() += 1;
            *calls.borrow() == 3
        });
        assert!(found);
        assert_eq!((*calls.borrow(), pauses.get()), (3, 2));

        pauses.set(0);
        assert!(!poll(4, &|| pauses.set(pauses.get() + 1), &mut || false));
        assert_eq!(pauses.get(), 3);
    }

    #[test]
    fn a_new_install_writes_every_setting_and_keeps_umlauts() {
        let text = build_ini(
            None,
            Some(("https://print.example.com", "KEY%1", "Büro-PC")),
            &[choice("Büro Drucker", true, None)],
        )
        .unwrap();
        let cfg = Config::from_text(&text).unwrap();
        assert_eq!(
            cfg.get("server_url").unwrap().unwrap(),
            "https://print.example.com"
        );
        assert_eq!(cfg.get("api_key").unwrap().unwrap(), "KEY%1");
        assert_eq!(cfg.get("name").unwrap().unwrap(), "Büro-PC");
        assert_eq!(cfg.get("printers").unwrap().unwrap(), "Büro Drucker|pdf");
    }

    #[test]
    fn changing_printers_leaves_everything_else_of_the_file_alone() {
        let before = "# mine\n[agent]\nserver_url = https://x\napi_key = K\nname = pc\nprinters = Old\n\n[headers]\nCF-Access = abc\n";
        let text = build_ini(Some(before), None, &[choice("New", false, None)]).unwrap();
        assert!(text.contains("# mine"));
        assert!(text.contains("api_key = K"));
        assert!(text.contains("[headers]\nCF-Access = abc"));
        assert!(text.contains("printers = New"));
        assert!(!text.contains("Old"));
    }

    #[test]
    fn a_printer_the_file_cannot_spell_is_refused() {
        assert!(
            build_ini(
                None,
                Some(("https://x", "k", "n")),
                &[choice("a;b", false, None)]
            )
            .is_err()
        );
    }

    #[test]
    fn test_print_looks_next_to_the_installed_agent_first() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = tmp.path().join("SumatraPDF.exe");
        let staging = tmp.path().join("staging");
        assert_eq!(
            sumatra_for_test(&installed, &staging),
            staging.join("SumatraPDF.exe")
        );
        std::fs::write(&installed, b"x").unwrap();
        assert_eq!(sumatra_for_test(&installed, &staging), installed);
    }

    #[test]
    fn detection_maps_to_the_words_of_the_contract() {
        assert_eq!(installed_name(Os::Windows, Installed::None), "none");
        assert_eq!(installed_name(Os::Macos, Installed::Managed), "managed");
        assert_eq!(installed_name(Os::Windows, Installed::Foreign), "foreign");
        assert_eq!(os_name(Os::Linux), "linux");
    }

    #[test]
    fn a_staging_path_with_blank_and_umlaut_survives_the_whole_command_line() {
        // Review focus 1: what goes to the elevated agent is the folder as one argument.
        let dir = Path::new("C:\\Users\\Jürgen Müller\\AppData\\Local\\Temp\\printpapi-setup-x");
        let q = elevate::windows_quote(dir.as_os_str());
        assert_eq!(
            q,
            std::ffi::OsString::from(
                "\"C:\\Users\\Jürgen Müller\\AppData\\Local\\Temp\\printpapi-setup-x\""
            )
        );
    }
}
