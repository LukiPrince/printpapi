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
use std::time::{Duration, Instant, SystemTime};

use printpapi_agent::apply::{SUMATRA_EXE_SHA256, SUMATRA_URL, SUMATRA_ZIP_SHA256, verify_sha256};
use printpapi_agent::client::Client;
use printpapi_agent::config::{
    Config, HttpSettings, IniUpdate, Printer, apply_patch as merge_patch, decode_ini,
    format_printers, parse_patch, parse_printers, update_ini,
};
use printpapi_agent::discover::discover;
use printpapi_agent::install::{self, Installed, Layout, Os};
use printpapi_agent::proc::{Cmd, Runner, run_command_with};
use printpapi_agent::{Platform, setup};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::elevate::{self, Elevate};
use crate::settings::{self, AgentStatus};
use crate::staging::Staging;
use crate::tray;

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
    install::layout(Os::host(), &install::program_files())
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

/// The word of the contract for what is on this machine. Where the setup cannot install at all
/// (`supported` is false: Linux without systemd) nothing is looked for.
fn install_state(supported: bool, detect: impl FnOnce() -> Installed) -> &'static str {
    if !supported {
        return "unsupported";
    }
    match detect() {
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

/// How long the window waits for the agent to report in, and how long it rests between looks.
const ONLINE_WITHIN: Duration = Duration::from_secs(60);
const ONLINE_PAUSE: Duration = Duration::from_secs(2);
/// One look at the server gives up after this many seconds (or the time left, if that is less).
const ONLINE_ATTEMPT_SECS: f64 = 5.0;

/// The HTTP timeout of one look with `left` of the waiting time remaining.
// ponytail: ureq times every phase (resolve, connect, send, response) on its own, so a look at a
// server that hangs in each phase can still end a few times this timeout past the deadline, about
// 20 s at most. A hard cap would need a thread per look.
pub fn attempt_timeout(left: Duration) -> f64 {
    left.as_secs_f64().clamp(1.0, ONLINE_ATTEMPT_SECS)
}

/// `check(left)` until it says yes or `within` has passed. No new look starts after the deadline,
/// and `check` gets the time left so that it can bound itself. `pause` only runs between looks.
/// The clock is injected, so a test can let a look "hang" without waiting.
pub fn poll_until(
    within: Duration,
    now: &dyn Fn() -> Instant,
    pause: &dyn Fn(),
    check: &mut dyn FnMut(Duration) -> bool,
) -> bool {
    let start = now();
    loop {
        let left = within.saturating_sub(now().saturating_duration_since(start));
        if left.is_zero() {
            return false;
        }
        if check(left) {
            return true;
        }
        if now().saturating_duration_since(start) >= within {
            return false;
        }
        pause();
    }
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
        printers: Some(&printers),
        timeout: None,
        headers: None,
    };
    Ok(update_ini(existing, &update))
}

fn read_result(dir: &Path) -> Option<Value> {
    let bytes = std::fs::read(dir.join("result.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// How an admin subcommand gets started with administrator rights: `elevate::run` in the app, a
/// fake in the tests (`agent`, then the subcommand and the staging folder).
type Elevator<'a> = &'a dyn Fn(&Path, &[&OsStr]) -> Result<(), Elevate>;

/// Run one admin subcommand on the staging folder and read what it reports.
fn run_elevated(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
    command: &str,
) -> Result<Value, SetupError> {
    // an old result must not be mistaken for the answer of this run
    let _ = std::fs::remove_file(dir.join("result.json"));
    let elevated = elevate(agent, &[OsStr::new(command), dir.as_os_str()]);
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
    install_state(install::supported(os), || {
        install::detect(os, &layout(), &install::system_dir(), &runner())
    })
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

/// The installed `agent.ini`, understood: what the window shows, what the later steps need, and
/// the text itself (kept so that a rewrite of the printers leaves the rest alone).
struct InstalledConfig {
    existing: Existing,
    conn: Conn,
    text: String,
}

/// Read the installed settings the way the agent does, so that a hand-edited file means the same
/// to the window: the trailing slashes of `server_url` are dropped (`crates/agent/src/lib.rs`
/// `start`), `[headers]` and `timeout` come along.
fn parse_installed(text: String) -> Result<InstalledConfig, SetupError> {
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
    let server_url = server_url.trim_end_matches('/').to_string();

    let existing = Existing {
        server_url: server_url.clone(),
        name: name.clone(),
        printers: printers.iter().map(printer_to_choice).collect(),
    };
    Ok(InstalledConfig {
        existing,
        conn: Conn {
            server_url,
            api_key,
            name,
            settings,
        },
        text,
    })
}

/// Have the installed `agent.ini` copied into the staging folder by an elevated `read-config`,
/// read it and delete the copy again, whatever happened: it holds the key.
fn read_existing_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
) -> Result<InstalledConfig, SetupError> {
    let ini = dir.join("agent.ini");
    let _ = std::fs::remove_file(&ini);
    let ran = run_elevated(elevate, agent, dir, "read-config");
    let text = std::fs::read(&ini).map(|b| decode_ini(&b));
    // the copy holds the key: gone before anything else happens
    let _ = std::fs::remove_file(&ini);
    ran?;
    let text = text.map_err(|e| SetupError::io("cannot read the installed settings", e))?;
    parse_installed(text)
}

#[tauri::command(async)]
pub fn read_existing(state: Shared) -> Result<Existing, SetupError> {
    let (agent, dir) = staged(&state)?;
    let loaded = read_existing_in(&elevate::run, &agent, &dir)?;
    let mut guard = lock(&state);
    guard.conn = Some(loaded.conn);
    guard.ini_text = Some(loaded.text);
    Ok(loaded.existing)
}

fn sumatra_in(dir: &Path) -> PathBuf {
    dir.join("SumatraPDF.exe")
}

#[tauri::command(async)]
pub fn sumatra_present(state: Shared) -> Result<bool, SetupError> {
    let mut guard = lock(&state);
    Ok(sumatra_path(&layout().sumatra, guard.staging()?.dir()).is_file())
}

/// SumatraPDF where the test print and the "is it there" check find it: next to the installed
/// agent, else the downloaded copy.
fn sumatra_path(installed: &Path, staging: &Path) -> PathBuf {
    if installed.is_file() {
        installed.to_path_buf()
    } else {
        sumatra_in(staging)
    }
}

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
/// The name of the exe inside the pinned ZIP.
const SUMATRA_EXE_IN_ZIP: &str = "SumatraPDF-3.6.1-64.exe";
/// The pinned ZIP is 10 MB (10,031,830 bytes); the limit leaves room for a later pin.
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

/// What the download has to be: where it comes from and the SHA-256 of the ZIP and of the exe in it.
struct Pins<'a> {
    url: &'a str,
    zip_sha256: &'a str,
    exe_sha256: &'a str,
}

const SUMATRA_PINS: Pins = Pins {
    url: SUMATRA_URL,
    zip_sha256: SUMATRA_ZIP_SHA256,
    exe_sha256: SUMATRA_EXE_SHA256,
};

/// Load the pinned SumatraPDF into the staging folder as `SumatraPDF.exe`. ZIP and exe are checked
/// against the SHA-256 built into the agent; the elevated install checks the exe once more. What
/// does not match is deleted, and so are the ZIP and the unpack folder in every case.
fn fetch_sumatra(
    dir: &Path,
    system_dir: &Path,
    run: &Runner,
    download: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    pins: &Pins,
) -> Result<(), SetupError> {
    let bad = |what: &str| SetupError::new("sumatra", format!("SumatraPDF: {what}"));
    let zip = dir.join("SumatraPDF.zip");
    let extracted = dir.join("sumatra-extract");
    let result = (|| {
        let bytes = download(pins.url).map_err(|e| bad(&format!("download failed ({e})")))?;
        std::fs::write(&zip, bytes).map_err(|e| SetupError::io("cannot save SumatraPDF", e))?;
        verify_sha256(&zip, pins.zip_sha256)
            .map_err(|_| bad("the download is not the expected file"))?;
        let _ = std::fs::remove_dir_all(&extracted);
        std::fs::create_dir(&extracted).map_err(|e| SetupError::io("cannot unpack", e))?;
        // ponytail: `tar.exe` reads its arguments in the ANSI code page, so a staging path with
        // characters outside it (user name "Łukasz Ж" on code page 1252) fails to open; the person
        // then sees "cannot unpack" and can still install without SumatraPDF. A relative path with
        // the staging folder as working directory would lift this (`Cmd` has no cwd yet).
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
        std::fs::rename(extracted.join(SUMATRA_EXE_IN_ZIP), &target)
            .map_err(|e| SetupError::io("cannot unpack", e))?;
        if verify_sha256(&target, pins.exe_sha256).is_err() {
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
    fetch_sumatra(
        &dir,
        &install::system_dir(),
        &runner(),
        &download,
        &SUMATRA_PINS,
    )
}

#[tauri::command(async)]
pub fn test_print(state: Shared, printer: PrinterChoice) -> Result<(), SetupError> {
    let dir = lock(&state).staging()?.dir().to_path_buf();
    let sumatra = sumatra_path(&layout().sumatra, &dir);
    setup::test_print(
        &choice_to_printer(&printer),
        &sumatra.to_string_lossy(),
        runner(),
    )
    .map_err(|m| SetupError::new("print", m))
}

/// The time on this computer that the elevated command took right before it started the agent
/// (`baseline_local` of its `result.json`), which the wait after a change compares the status
/// file's `last_contact` with. It is the agent's own word, written after the old agent was
/// stopped: a time read here once the elevated run is back could be later than the new agent's
/// first contact, and the wait would then miss it.
fn local_baseline(result: &Value) -> Result<f64, SetupError> {
    result["baseline_local"]
        .as_f64()
        .ok_or_else(|| SetupError::new("system", "The agent did not report a start time."))
}

/// What a finished install hands back: the start time of the agent's record on the server and,
/// for a new connection, the connection the later steps use.
struct Applied {
    baseline: f64,
    conn: Option<Conn>,
}

/// Write the `agent.ini` into the staging folder, have the elevated agent install it and delete the
/// file again, whatever the outcome: it holds the key.
fn apply_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
    request: &ApplyRequest,
    existing: Option<&str>,
) -> Result<Applied, SetupError> {
    let new_conn = match (&request.server_url, &request.api_key, &request.name) {
        (Some(url), Some(key), Some(name)) => {
            Some((setup::normalize_server_url(url)?, key.clone(), name.clone()))
        }
        _ => None,
    };
    if new_conn.is_none() && existing.is_none() {
        return Err(SetupError::new(
            "system",
            "There is no connection to install with. Go back to the first step.",
        ));
    }
    let text = build_ini(
        if new_conn.is_some() { None } else { existing },
        new_conn
            .as_ref()
            .map(|(u, k, n)| (u.as_str(), k.as_str(), n.as_str())),
        &request.printers,
    )
    .map_err(|m| SetupError::new("printers", m))?;

    let ini = dir.join("agent.ini");
    crate::staging::write_private(dir, "agent.ini", text.as_bytes())
        .map_err(|e| SetupError::io("cannot write the settings", e))?;
    let ran = run_elevated(elevate, agent, dir, "install");
    // the file holds the key: gone whatever happened
    let _ = std::fs::remove_file(&ini);
    let baseline = ran?["baseline_last_seen"]
        .as_f64()
        .ok_or_else(|| SetupError::new("system", "The agent did not report a start time."))?;

    Ok(Applied {
        baseline,
        conn: new_conn.map(|(server_url, api_key, name)| Conn {
            server_url,
            api_key,
            name,
            settings: HttpSettings::default(),
        }),
    })
}

/// The tray icon program this window offers to the install: staged as `tray` unless this window
/// already is the installed one (see `settings::stage_tray`). Unix paths are resolved first, so a
/// link to the installed program is still that program.
// ponytail: on Windows the running exe is compared as the system reports it, which is the long
// path the installer wrote; a window started through an 8.3 short path of the install folder
// would be taken for a copy. Ceiling: the window ends with the install then (taskkill by image).
fn stage_own_tray_with(
    os: Os,
    exe: &Path,
    appimage: Option<&OsStr>,
    appdir: Option<&OsStr>,
    l: &Layout,
    dir: &Path,
    run: &Runner,
) -> Result<bool, SetupError> {
    let resolve = |p: &Path| {
        if cfg!(windows) {
            p.to_path_buf()
        } else {
            std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
        }
    };
    // `exe` comes resolved from the system, `$APPDIR` may not be: compare like with like
    let appdir = appdir.map(|d| resolve(Path::new(d)));
    let source = settings::tray_source(
        os,
        &resolve(exe),
        appimage,
        appdir.as_deref().map(Path::as_os_str),
    )
    .map(|p| resolve(&p));
    settings::stage_tray(os, source.as_deref(), &resolve(&l.tray), dir, run)
        .map_err(|e| SetupError::io("cannot prepare the tray icon", e))
}

fn stage_own_tray(dir: &Path) -> Result<bool, SetupError> {
    let exe = std::env::current_exe().map_err(|e| SetupError::io("cannot find this program", e))?;
    stage_own_tray_with(
        Os::host(),
        &exe,
        std::env::var_os("APPIMAGE").as_deref(),
        std::env::var_os("APPDIR").as_deref(),
        &layout(),
        dir,
        &runner(),
    )
}

/// Start the tray icon that was just installed, for the user of this window and without
/// administrator rights. It is not waited for (it runs until the user quits it), and a failure is
/// not an error of the install: the icon starts at the next logon anyway.
fn start_installed_tray() {
    let cmd = settings::tray_start_cmd(Os::host(), &layout());
    let Some((program, args)) = cmd.argv.split_first() else {
        return;
    };
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    no_window(&mut command);
    let _ = command.spawn();
}

#[tauri::command(async)]
pub fn apply(state: Shared, request: ApplyRequest) -> Result<f64, SetupError> {
    let (agent, dir) = staged(&state)?;
    let tray_staged = stage_own_tray(&dir)?;
    let existing = lock(&state).ini_text.clone();
    let applied = apply_in(&elevate::run, &agent, &dir, &request, existing.as_deref())?;
    if let Some(conn) = applied.conn {
        lock(&state).conn = Some(conn);
    }
    if tray_staged {
        start_installed_tray();
    }
    Ok(applied.baseline)
}

#[tauri::command(async)]
pub fn wait_online(state: Shared, baseline: f64) -> Result<bool, SetupError> {
    let conn = lock(&state).conn.as_ref().map(|c| {
        (
            c.server_url.clone(),
            c.api_key.clone(),
            c.name.clone(),
            c.settings.clone(),
        )
    });
    let Some((url, key, name, settings)) = conn else {
        // After a change of settings the window has no key to ask the server with, so it waits for
        // the agent's own word: a contact in the status file after `baseline`.
        return Ok(poll_until(
            ONLINE_WITHIN,
            &Instant::now,
            &|| std::thread::sleep(ONLINE_PAUSE),
            &mut |_| settings::online_since_file(&tray::status_path(), SystemTime::now(), baseline),
        ));
    };
    // a failed look (the network, a restart of the server) is "not yet", not the end
    Ok(poll_until(
        ONLINE_WITHIN,
        &Instant::now,
        &|| std::thread::sleep(ONLINE_PAUSE),
        &mut |left| {
            let settings = HttpSettings {
                timeout: attempt_timeout(left),
                ..settings.clone()
            };
            let client = Client::new(&settings);
            setup::agent_seen_after(&client, &url, &key, &name, baseline).unwrap_or(false)
        },
    ))
}

#[tauri::command(async)]
pub fn uninstall(state: Shared) -> Result<(), SetupError> {
    let (agent, dir) = staged(&state)?;
    run_elevated(&elevate::run, &agent, &dir, "uninstall")?;
    let mut guard = lock(&state);
    guard.conn = None;
    guard.ini_text = None;
    Ok(())
}

// ---- the settings window

/// What the window shows about the agent. Reads one small file, needs no administrator rights.
#[tauri::command(async)]
pub fn read_status() -> AgentStatus {
    settings::status_from_file(&tray::status_path(), SystemTime::now())
}

/// The last `lines` lines of the agent's log (empty when it wrote none yet).
#[tauri::command(async)]
pub fn read_log(lines: usize) -> Result<String, SetupError> {
    let path = install::service_log_path(Os::host(), &printpapi_agent::status::program_data());
    settings::read_log_in(&path, lines).map_err(|e| SetupError::io("cannot read the log", e))
}

/// What the window may change in an installed `agent.ini`: only what is named.
#[derive(Debug, Deserialize)]
pub struct PatchRequest {
    printers: Option<Vec<PrinterChoice>>,
    timeout: Option<f64>,
    /// `[name, value]` sets a header, `[name, null]` removes it.
    headers: Option<Vec<(String, Option<String>)>>,
}

/// What the agent reads at start, with nothing else in it: a patch is tried on this before the
/// administrator prompt, so that a header name the file cannot hold, a timeout that is too short or
/// a printer list that cannot be read fail without a prompt.
const PREFLIGHT_INI: &str =
    "[agent]\nserver_url = https://example.invalid\napi_key = k\nprinters = p\n";

/// The text of `patch.json` (the format of `config::parse_patch`), checked against the agent's
/// own rules.
pub fn patch_json(req: &PatchRequest) -> Result<String, SetupError> {
    let mut obj = serde_json::Map::new();
    if let Some(list) = &req.printers {
        let printers: Vec<Printer> = list.iter().map(choice_to_printer).collect();
        let text = format_printers(&printers).map_err(|m| SetupError::new("printers", m))?;
        obj.insert("printers".into(), Value::String(text));
    }
    if let Some(t) = req.timeout {
        // NaN and infinity have no JSON form and become null, which the agent refuses
        obj.insert("timeout".into(), serde_json::json!(t));
    }
    if let Some(h) = &req.headers {
        obj.insert("headers".into(), serde_json::json!(h));
    }
    let text = Value::Object(obj).to_string();
    let change = |m: String| SetupError::new("change", m);
    let patch = parse_patch(&text).map_err(change)?;
    merge_patch(PREFLIGHT_INI, &patch).map_err(change)?;
    Ok(text)
}

/// Stage the change as `patch.json` and have the elevated agent apply it to the installed
/// `agent.ini`. One prompt, and the window never holds the file with the key. The patch can hold
/// header values, so it is deleted whatever happened.
fn apply_patch_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
    req: &PatchRequest,
) -> Result<f64, SetupError> {
    let text = patch_json(req)?;
    // the agent takes a staged agent.ini over a patch
    match std::fs::remove_file(dir.join("agent.ini")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(SetupError::io("cannot clear the working folder", e));
        }
        _ => {}
    }
    crate::staging::write_private(dir, "patch.json", text.as_bytes())
        .map_err(|e| SetupError::io("cannot write the change", e))?;
    let ran = run_elevated(elevate, agent, dir, "install");
    let _ = std::fs::remove_file(dir.join("patch.json"));
    local_baseline(&ran?)
}

/// Apply a change. Returns the time on this computer right before the agent was started again,
/// which `wait_online` then compares the agent's next contact with (the window has no key, so there
/// is no server-side start time to give).
#[tauri::command(async)]
pub fn apply_patch(state: Shared, patch: PatchRequest) -> Result<f64, SetupError> {
    let (agent, dir) = staged(&state)?;
    let tray_staged = stage_own_tray(&dir)?;
    let baseline = apply_patch_in(&elevate::run, &agent, &dir, &patch)?;
    // the wait that follows reads the status file, not a connection left over from the wizard
    lock(&state).conn = None;
    if tray_staged {
        start_installed_tray();
    }
    Ok(baseline)
}

fn restart_in(elevate: Elevator, agent: &Path, dir: &Path) -> Result<f64, SetupError> {
    local_baseline(&run_elevated(elevate, agent, dir, "restart")?)
}

/// Restart the agent. Returns the time on this computer right before it was started, like
/// `apply_patch`, for the wait that follows.
#[tauri::command(async)]
pub fn restart_agent(state: Shared) -> Result<f64, SetupError> {
    let (agent, dir) = staged(&state)?;
    lock(&state).conn = None;
    restart_in(&elevate::run, &agent, &dir)
}

/// The `[headers]` of the installed file, read by an elevated `read-config` like the key would be.
/// Only the headers go back to the window.
fn header_values_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
) -> Result<Vec<(String, String)>, SetupError> {
    let loaded = read_existing_in(elevate, agent, dir)?;
    Ok(loaded.conn.settings.headers.into_iter().collect())
}

#[tauri::command(async)]
pub fn header_values(state: Shared) -> Result<Vec<(String, String)>, SetupError> {
    let (agent, dir) = staged(&state)?;
    header_values_in(&elevate::run, &agent, &dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

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
            sumatra_path(&installed, &staging),
            staging.join("SumatraPDF.exe")
        );
        std::fs::write(&installed, b"x").unwrap();
        assert_eq!(sumatra_path(&installed, &staging), installed);
    }

    #[test]
    fn detection_maps_to_the_words_of_the_contract() {
        assert_eq!(install_state(true, || Installed::None), "none");
        assert_eq!(install_state(true, || Installed::Managed), "managed");
        assert_eq!(install_state(true, || Installed::Foreign), "foreign");
        assert_eq!(os_name(Os::Linux), "linux");
    }

    #[test]
    fn where_the_setup_cannot_install_nothing_is_looked_for() {
        let asked = Cell::new(false);
        let word = install_state(false, || {
            asked.set(true);
            Installed::Managed
        });
        assert_eq!(word, "unsupported");
        assert!(!asked.get(), "the machine must not be probed");
    }

    #[test]
    fn closing_the_window_deletes_the_working_folder_and_forgets_the_rest() {
        let mut state = AppState::default();
        let dir = state.staging().unwrap().dir().to_path_buf();
        std::fs::write(dir.join("agent.ini"), "key").unwrap();
        state.ini_text = Some("key".into());
        state.clear();
        assert!(!dir.exists());
        assert!(state.staging.is_none() && state.conn.is_none() && state.ini_text.is_none());
    }

    /// A one-shot HTTP server on loopback: answers the first request with `status_line` and `body`
    /// and hands back the request head it received.
    fn serve_once(
        status_line: &'static str,
        body: &'static [u8],
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file.zip", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if socket.read(&mut byte).unwrap() == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            write!(
                socket,
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            socket.write_all(body).unwrap();
            String::from_utf8_lossy(&head).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn the_download_sends_a_browser_user_agent() {
        let (url, server) = serve_once("200 OK", b"zip bytes");
        assert_eq!(download(&url).unwrap(), b"zip bytes");
        let head = server.join().unwrap().to_ascii_lowercase();
        assert!(
            head.contains(&format!("user-agent: {}", BROWSER_UA.to_ascii_lowercase())),
            "{head}"
        );
    }

    #[test]
    fn a_refused_download_is_an_error_not_a_file() {
        let (url, server) = serve_once("403 Forbidden", b"denied");
        assert!(download(&url).is_err());
        server.join().unwrap();
    }

    // ---- the waiting for the agent

    /// A clock that only moves when a test moves it.
    struct FakeClock {
        base: Instant,
        passed: Cell<Duration>,
    }

    impl FakeClock {
        fn new() -> FakeClock {
            FakeClock {
                base: Instant::now(),
                passed: Cell::new(Duration::ZERO),
            }
        }
        fn now(&self) -> Instant {
            self.base + self.passed.get()
        }
        fn advance(&self, by: Duration) {
            self.passed.set(self.passed.get() + by);
        }
    }

    #[test]
    fn polling_stops_at_the_first_yes_and_pauses_only_between_looks() {
        let clock = FakeClock::new();
        let (pauses, calls) = (Cell::new(0), Cell::new(0));
        let found = poll_until(
            Duration::from_secs(60),
            &|| clock.now(),
            &|| {
                pauses.set(pauses.get() + 1);
                clock.advance(ONLINE_PAUSE);
            },
            &mut |_| {
                calls.set(calls.get() + 1);
                calls.get() == 3
            },
        );
        assert!(found);
        assert_eq!((calls.get(), pauses.get()), (3, 2));
    }

    #[test]
    fn polling_gives_up_after_the_time_not_after_a_number_of_looks() {
        let clock = FakeClock::new();
        let calls = Cell::new(0);
        let found = poll_until(
            ONLINE_WITHIN,
            &|| clock.now(),
            &|| clock.advance(ONLINE_PAUSE),
            &mut |_| {
                calls.set(calls.get() + 1);
                false
            },
        );
        assert!(!found);
        assert_eq!(clock.passed.get(), ONLINE_WITHIN);
        assert_eq!(calls.get(), 30);
    }

    #[test]
    fn a_look_that_hangs_ends_the_waiting_at_the_deadline() {
        // every look takes as long as the time it is given (a server that never answers); the
        // next one must not start once the time is over
        let clock = FakeClock::new();
        let calls = Cell::new(0);
        let pauses = Cell::new(0);
        let found = poll_until(
            ONLINE_WITHIN,
            &|| clock.now(),
            &|| {
                pauses.set(pauses.get() + 1);
                clock.advance(ONLINE_PAUSE);
            },
            &mut |left| {
                calls.set(calls.get() + 1);
                clock.advance(Duration::from_secs_f64(attempt_timeout(left)));
                false
            },
        );
        assert!(!found);
        // a round is a 5 s look and a 2 s pause; the ninth starts at 56 s with 4 s left and is
        // held to 4 s, so the waiting ends at 60 s
        assert_eq!(clock.passed.get(), ONLINE_WITHIN);
        assert_eq!((calls.get(), pauses.get()), (9, 8));
        // and one that overshoots on its own is still the last
        let clock = FakeClock::new();
        let calls = Cell::new(0);
        poll_until(
            ONLINE_WITHIN,
            &|| clock.now(),
            &|| panic!("no pause after the deadline"),
            &mut |_| {
                calls.set(calls.get() + 1);
                clock.advance(Duration::from_secs(50 * 60));
                false
            },
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_look_never_gets_more_than_five_seconds_nor_less_than_one() {
        assert_eq!(attempt_timeout(Duration::from_secs(60)), 5.0);
        assert_eq!(attempt_timeout(Duration::from_secs(3)), 3.0);
        assert_eq!(attempt_timeout(Duration::from_millis(200)), 1.0);
    }

    // ---- the installed agent.ini

    const KEY: &str = "SECRET-KEY-123";
    const INSTALLED: &str = "[agent]\nserver_url = https://x/\napi_key = SECRET-KEY-123\nname = pc\nprinters = Büro Drucker|pdf\n\n[headers]\nCF-Access = abc\n";

    fn staging_dir_of(args: &[&OsStr]) -> PathBuf {
        PathBuf::from(args[1])
    }

    fn write_result(dir: &Path, result: Value) {
        std::fs::write(dir.join("result.json"), result.to_string()).unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_hand_edited_server_url_is_read_the_way_the_agent_reads_it() {
        let loaded = parse_installed(INSTALLED.replace("https://x/", "https://x//")).unwrap();
        assert_eq!(loaded.conn.server_url, "https://x");
        assert_eq!(loaded.existing.server_url, "https://x");
        assert_eq!(loaded.conn.name, "pc");
        assert_eq!(loaded.conn.settings.headers.len(), 1);
        assert_eq!(
            loaded.existing.printers,
            vec![choice("Büro Drucker", true, None)]
        );
        assert_eq!(loaded.text, INSTALLED.replace("https://x/", "https://x//"));
    }

    #[test]
    fn an_agent_ini_without_a_key_or_with_a_broken_printer_list_is_unreadable() {
        for text in [
            "[agent]\nserver_url = https://x\nprinters = A\n",
            "[agent]\napi_key = k\nprinters = A\n",
            "[agent]\nserver_url = https://x\napi_key = k\nprinters = a;;\n= \n",
            "[agent]
server_url = https://x
api_key = k
printers = Z = socket://nohost
",
            "not an ini [",
        ] {
            let e = parse_installed(text.into()).err().expect(text);
            assert_eq!(e.code, "system", "{text}");
        }
    }

    #[test]
    fn reading_the_installed_file_hands_over_its_settings_but_never_the_key_to_the_window() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // a copy left behind by an earlier run must not be taken for this one's
        std::fs::write(dir.join("agent.ini"), "stale").unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!(args[0], "read-config");
            let dir = staging_dir_of(args);
            assert!(!dir.join("agent.ini").exists());
            std::fs::write(dir.join("agent.ini"), INSTALLED).unwrap();
            write_result(&dir, json!({"ok": true}));
            Ok(())
        };
        let loaded = read_existing_in(&elevate, Path::new("agent"), dir).unwrap();
        assert!(!dir.join("agent.ini").exists());
        assert_eq!(loaded.conn.api_key, KEY);
        let for_the_window = serde_json::to_string(&loaded.existing).unwrap();
        assert!(!for_the_window.contains(KEY), "{for_the_window}");
        assert!(for_the_window.contains("\"serverUrl\":\"https://x\""));
    }

    #[test]
    fn the_copy_with_the_key_is_deleted_after_a_failed_or_cancelled_read() {
        for (outcome, result, code) in [
            (
                Err(Elevate::Failed("exit 1".into())),
                Some(json!({"ok": false, "code": "system", "message": "no file"})),
                "system",
            ),
            (Err(Elevate::Cancelled), None, "cancelled"),
            // exited cleanly, but the agent wrote no result
            (Ok(()), None, "system"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            let elevate = |_: &Path, args: &[&OsStr]| {
                let dir = staging_dir_of(args);
                std::fs::write(dir.join("agent.ini"), INSTALLED).unwrap();
                if let Some(r) = &result {
                    write_result(&dir, r.clone());
                }
                outcome.clone()
            };
            let e = read_existing_in(&elevate, Path::new("agent"), dir)
                .err()
                .expect("an error");
            assert_eq!(e.code, code);
            assert!(!dir.join("agent.ini").exists(), "{code}");
        }
    }

    #[test]
    fn a_read_that_reports_success_without_a_file_is_an_error_and_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            write_result(&staging_dir_of(args), json!({"ok": true}));
            Ok(())
        };
        let e = read_existing_in(&elevate, Path::new("agent"), tmp.path())
            .err()
            .expect("an error");
        assert_eq!(e.code, "system");
        assert_eq!(names_in(tmp.path()), ["result.json"]);
    }

    #[test]
    fn an_old_result_is_not_taken_for_the_answer_of_this_run() {
        let tmp = tempfile::tempdir().unwrap();
        write_result(tmp.path(), json!({"ok": true, "baseline_last_seen": 1.0}));
        let elevate = |_: &Path, _: &[&OsStr]| Ok(());
        let e = run_elevated(&elevate, Path::new("agent"), tmp.path(), "install").unwrap_err();
        assert_eq!(e.code, "system");
        assert!(!tmp.path().join("result.json").exists());
    }

    // ---- the install

    fn request(conn: Option<(&str, &str, &str)>, printers: Vec<PrinterChoice>) -> ApplyRequest {
        ApplyRequest {
            server_url: conn.map(|c| c.0.into()),
            api_key: conn.map(|c| c.1.into()),
            name: conn.map(|c| c.2.into()),
            printers,
        }
    }

    #[test]
    fn an_install_gives_the_key_to_the_agent_by_file_only_and_returns_the_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], "install");
            assert!(!args[1].to_string_lossy().contains(KEY));
            let dir = staging_dir_of(args);
            let ini = std::fs::read_to_string(dir.join("agent.ini")).unwrap();
            assert!(ini.contains(KEY));
            assert!(ini.contains("server_url = https://x\n"), "{ini}");
            write_result(&dir, json!({"ok": true, "baseline_last_seen": 12.5}));
            Ok(())
        };
        let req = request(
            Some(("https://x/", KEY, "pc")),
            vec![choice("Büro", true, None)],
        );
        let applied = apply_in(&elevate, Path::new("agent"), dir, &req, None).unwrap();
        assert_eq!(applied.baseline, 12.5);
        let conn = applied.conn.expect("a new connection");
        assert_eq!(
            (conn.server_url.as_str(), conn.api_key.as_str()),
            ("https://x", KEY)
        );
        assert!(!dir.join("agent.ini").exists());
    }

    #[test]
    fn the_file_with_the_key_is_gone_after_a_failed_or_cancelled_install() {
        for (outcome, result, code) in [
            (
                Err(Elevate::Failed("exit 1".into())),
                Some(json!({"ok": false, "code": "policy", "message": "no"})),
                "policy",
            ),
            (Err(Elevate::Cancelled), None, "cancelled"),
            (Ok(()), Some(json!({"ok": true})), "system"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            // an answer of an earlier run
            write_result(dir, json!({"ok": true, "baseline_last_seen": 1.0}));
            let elevate = |_: &Path, args: &[&OsStr]| {
                let dir = staging_dir_of(args);
                assert!(dir.join("agent.ini").exists(), "the agent needs the file");
                if let Some(r) = &result {
                    write_result(&dir, r.clone());
                }
                outcome.clone()
            };
            let req = request(
                Some(("https://x", KEY, "pc")),
                vec![choice("A", false, None)],
            );
            let e = apply_in(&elevate, Path::new("agent"), dir, &req, None)
                .err()
                .expect("an error");
            assert_eq!(e.code, code);
            assert!(!dir.join("agent.ini").exists(), "{code}");
        }
    }

    #[test]
    fn without_a_connection_nothing_is_written_or_elevated() {
        let tmp = tempfile::tempdir().unwrap();
        let asked = Cell::new(false);
        let elevate = |_: &Path, _: &[&OsStr]| {
            asked.set(true);
            Ok(())
        };
        let req = request(None, vec![choice("A", false, None)]);
        let e = apply_in(&elevate, Path::new("agent"), tmp.path(), &req, None)
            .err()
            .expect("an error");
        assert_eq!(e.code, "system");
        assert!(!asked.get());
        assert!(names_in(tmp.path()).is_empty());

        // a printer the file cannot spell stops before the prompt, too
        let req = request(
            Some(("https://x", KEY, "pc")),
            vec![choice("a;b", false, None)],
        );
        let e = apply_in(&elevate, Path::new("agent"), tmp.path(), &req, None)
            .err()
            .expect("an error");
        assert_eq!(e.code, "printers");
        assert!(!asked.get());
        assert!(names_in(tmp.path()).is_empty());
    }

    #[test]
    fn changing_printers_rewrites_only_them_and_keeps_the_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            let dir = staging_dir_of(args);
            let ini = std::fs::read_to_string(dir.join("agent.ini")).unwrap();
            assert!(ini.contains("printers = New"), "{ini}");
            assert!(ini.contains("[headers]\nCF-Access = abc"), "{ini}");
            assert!(ini.contains("server_url = https://x/"), "{ini}");
            write_result(&dir, json!({"ok": true, "baseline_last_seen": 3.0}));
            Ok(())
        };
        let req = request(None, vec![choice("New", false, None)]);
        let applied = apply_in(
            &elevate,
            Path::new("agent"),
            tmp.path(),
            &req,
            Some(INSTALLED),
        )
        .unwrap();
        assert_eq!(applied.baseline, 3.0);
        assert!(applied.conn.is_none());
        assert!(!tmp.path().join("agent.ini").exists());
    }

    #[test]
    fn a_new_connection_does_not_build_on_an_installed_file_still_in_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            let dir = staging_dir_of(args);
            let ini = std::fs::read_to_string(dir.join("agent.ini")).unwrap();
            assert!(!ini.contains("CF-Access"), "{ini}");
            assert!(!ini.contains(KEY), "{ini}");
            write_result(&dir, json!({"ok": true, "baseline_last_seen": 2.0}));
            Ok(())
        };
        let req = request(
            Some(("https://y", "NEW", "pc2")),
            vec![choice("A", false, None)],
        );
        apply_in(
            &elevate,
            Path::new("agent"),
            tmp.path(),
            &req,
            Some(INSTALLED),
        )
        .unwrap();
    }

    // ---- changes of the settings

    fn patch_request(
        printers: Option<Vec<PrinterChoice>>,
        timeout: Option<f64>,
        headers: Option<Vec<(&str, Option<&str>)>>,
    ) -> PatchRequest {
        PatchRequest {
            printers,
            timeout,
            headers: headers.map(|h| {
                h.into_iter()
                    .map(|(n, v)| (n.to_string(), v.map(String::from)))
                    .collect()
            }),
        }
    }

    #[test]
    fn a_change_is_the_agents_own_patch_file_with_only_what_changed() {
        let req = patch_request(
            Some(vec![
                choice("Büro Drucker", true, None),
                choice("Zebra", false, Some("socket://10.0.0.5:9100")),
            ]),
            Some(45.0),
            Some(vec![("CF-Access", Some("abc")), ("Old", None)]),
        );
        let text = patch_json(&req).unwrap();
        let patch = parse_patch(&text).unwrap();
        assert_eq!(
            patch.printers.as_deref(),
            Some("Büro Drucker|pdf ; Zebra = socket://10.0.0.5:9100")
        );
        assert_eq!(patch.timeout, Some(45.0));
        assert_eq!(
            patch.headers,
            vec![
                ("CF-Access".to_string(), Some("abc".to_string())),
                ("Old".to_string(), None)
            ]
        );
        // nothing named, nothing in the file
        let text = patch_json(&patch_request(None, None, None)).unwrap();
        assert_eq!(text, "{}");
        assert_eq!(
            parse_patch(&text).unwrap(),
            printpapi_agent::config::Patch::default()
        );
    }

    #[test]
    fn a_change_the_agent_would_refuse_is_refused_before_the_prompt() {
        let e = patch_json(&patch_request(
            Some(vec![choice("a;b", false, None)]),
            None,
            None,
        ))
        .unwrap_err();
        assert_eq!(e.code, "printers");
        let e = patch_json(&patch_request(None, Some(10.0), None)).unwrap_err();
        assert_eq!(e.code, "change");
        assert!(e.message.contains("30"), "{}", e.message);
        let e = patch_json(&patch_request(None, Some(f64::NAN), None)).unwrap_err();
        assert_eq!(e.code, "change");
        let e = patch_json(&patch_request(
            None,
            None,
            Some(vec![("two words", Some("x"))]),
        ))
        .unwrap_err();
        assert_eq!(e.code, "change");
        let e = patch_json(&patch_request(
            None,
            None,
            Some(vec![("Authorization", Some("x"))]),
        ))
        .unwrap_err();
        assert_eq!(e.code, "change");
    }

    #[test]
    fn a_change_reaches_the_agent_by_file_and_the_file_with_the_values_is_gone_afterwards() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // an agent.ini of an earlier run would win over the patch in the agent
        std::fs::write(dir.join("agent.ini"), "stale").unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!(args[0], "install");
            assert!(!args[1].to_string_lossy().contains("SECRET"));
            let dir = staging_dir_of(args);
            assert!(!dir.join("agent.ini").exists());
            let patch = std::fs::read_to_string(dir.join("patch.json")).unwrap();
            assert!(patch.contains("SECRET"), "{patch}");
            write_result(
                &dir,
                json!({"ok": true, "baseline_last_seen": 1.0, "baseline_local": 1794048800.5}),
            );
            Ok(())
        };
        let req = patch_request(None, None, Some(vec![("X-Token", Some("SECRET"))]));
        // what the agent says it took right before it started, not a time read here afterwards
        let baseline = apply_patch_in(&elevate, Path::new("agent"), dir, &req).unwrap();
        assert_eq!(baseline, 1794048800.5);
        assert_eq!(names_in(dir), ["result.json"]);
    }

    #[test]
    fn a_change_or_restart_without_a_baseline_from_the_agent_is_an_error_not_a_guess() {
        // an older agent, or a result that is not what was asked: the window must not wait on a
        // time it made up after the agent was already running
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            write_result(&staging_dir_of(args), json!({"ok": true}));
            Ok(())
        };
        let req = patch_request(None, Some(45.0), None);
        let e = apply_patch_in(&elevate, Path::new("agent"), tmp.path(), &req).unwrap_err();
        assert_eq!(e.code, "system");
        assert!(e.message.contains("start time"), "{}", e.message);
        let e = restart_in(&elevate, Path::new("agent"), tmp.path()).unwrap_err();
        assert!(e.message.contains("start time"), "{}", e.message);
    }

    #[test]
    fn the_patch_is_deleted_after_a_failed_or_cancelled_run_and_a_bad_one_asks_nothing() {
        for (outcome, result, code) in [
            (
                Err(Elevate::Failed("exit 1".into())),
                Some(json!({"ok": false, "code": "unreachable", "message": "no route"})),
                "unreachable",
            ),
            (Err(Elevate::Cancelled), None, "cancelled"),
            (Ok(()), None, "system"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let elevate = |_: &Path, args: &[&OsStr]| {
                let dir = staging_dir_of(args);
                assert!(dir.join("patch.json").exists());
                if let Some(r) = &result {
                    write_result(&dir, r.clone());
                }
                outcome.clone()
            };
            let req = patch_request(None, None, Some(vec![("X-Token", Some("SECRET"))]));
            let e = apply_patch_in(&elevate, Path::new("agent"), tmp.path(), &req).unwrap_err();
            assert_eq!(e.code, code);
            assert!(!tmp.path().join("patch.json").exists(), "{code}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let asked = Cell::new(false);
        let elevate = |_: &Path, _: &[&OsStr]| {
            asked.set(true);
            Ok(())
        };
        let req = patch_request(None, Some(1.0), None);
        assert!(apply_patch_in(&elevate, Path::new("agent"), tmp.path(), &req).is_err());
        assert!(!asked.get());
        assert!(names_in(tmp.path()).is_empty());
    }

    #[test]
    fn a_restart_is_one_elevated_run_of_the_restart_command() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!((args.len(), args[0]), (2, OsStr::new("restart")));
            write_result(
                &staging_dir_of(args),
                json!({"ok": true, "baseline_local": 1794048801.0}),
            );
            Ok(())
        };
        assert_eq!(
            restart_in(&elevate, Path::new("agent"), tmp.path()).unwrap(),
            1794048801.0
        );
        let elevate = |_: &Path, _: &[&OsStr]| Err(Elevate::Cancelled);
        let e = restart_in(&elevate, Path::new("agent"), tmp.path()).unwrap_err();
        assert_eq!(e.code, "cancelled");
    }

    #[test]
    fn header_values_come_from_the_installed_file_and_the_key_stays_here() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!(args[0], "read-config");
            let dir = staging_dir_of(args);
            std::fs::write(
                dir.join("agent.ini"),
                INSTALLED.replace("CF-Access = abc", "CF-Access = abc\nX-Other = 100%"),
            )
            .unwrap();
            write_result(&dir, json!({"ok": true}));
            Ok(())
        };
        let values = header_values_in(&elevate, Path::new("agent"), tmp.path()).unwrap();
        assert_eq!(
            values,
            vec![
                ("cf-access".to_string(), "abc".to_string()),
                ("x-other".to_string(), "100%".to_string())
            ]
        );
        assert!(!serde_json::to_string(&values).unwrap().contains(KEY));
        assert!(!tmp.path().join("agent.ini").exists());
    }

    // ---- the tray program and the wait

    #[test]
    fn a_window_that_runs_from_the_installed_tray_stages_none() {
        let tmp = tempfile::tempdir().unwrap();
        let run: Runner = Rc::new(|_| panic!("nothing is run"));
        let l = install::layout(Os::Windows, Path::new("C:\\Program Files"));
        let installed_exe = Path::new("C:\\Program Files\\printpapi-agent\\printpapi-tray.exe");
        let staged =
            stage_own_tray_with(Os::Windows, installed_exe, None, None, &l, tmp.path(), &run)
                .unwrap();
        assert!(!staged);
        assert!(names_in(tmp.path()).is_empty());
        // a copy somewhere else is offered, and the install replaces the installed one with it
        let elsewhere = tmp.path().join("printpapi-setup.exe");
        std::fs::write(&elsewhere, b"exe").unwrap();
        assert!(
            stage_own_tray_with(Os::Windows, &elsewhere, None, None, &l, tmp.path(), &run).unwrap()
        );
        assert_eq!(std::fs::read(tmp.path().join("tray")).unwrap(), b"exe");
    }

    // ---- the SumatraPDF download

    const ZIP: &[u8] = b"zipbytes";
    const ZIP_SHA: &str = "f3294ee9ec99a7ca7db514f129502a0b4c795dba7fa66d41050e8d70ddf7aa9b";
    const EXE: &[u8] = b"exebytes";
    const EXE_SHA: &str = "b3af69384e39c71c4a77c3519b3e42bcdd4439406284c725a01e97fee0f081fd";
    const PINS: Pins = Pins {
        url: "https://pin.test/sumatra.zip",
        zip_sha256: ZIP_SHA,
        exe_sha256: EXE_SHA,
    };

    /// A `tar` that "unpacks" an exe with the given content into the folder after `-C`.
    fn fake_tar(exe: &'static [u8]) -> Runner {
        Rc::new(move |cmd: &Cmd| {
            assert_eq!(
                cmd.argv[0],
                Path::new("sys").join("tar.exe").to_string_lossy()
            );
            assert_eq!(cmd.argv[1], "-xf");
            assert!(Path::new(&cmd.argv[2]).is_file(), "the ZIP is on disk");
            std::fs::write(Path::new(&cmd.argv[4]).join(SUMATRA_EXE_IN_ZIP), exe).unwrap();
            Ok(Vec::new())
        })
    }

    fn serve(bytes: &'static [u8]) -> impl Fn(&str) -> Result<Vec<u8>, String> {
        move |url| {
            assert_eq!(url, PINS.url);
            Ok(bytes.to_vec())
        }
    }

    #[test]
    fn a_checked_download_leaves_only_the_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let system = Path::new("sys");
        fetch_sumatra(tmp.path(), system, &fake_tar(EXE), &serve(ZIP), &PINS).unwrap();
        assert_eq!(names_in(tmp.path()), ["SumatraPDF.exe"]);
        assert_eq!(
            std::fs::read(tmp.path().join("SumatraPDF.exe")).unwrap(),
            EXE
        );
    }

    #[test]
    fn a_zip_with_the_wrong_checksum_is_thrown_away_before_it_is_unpacked() {
        let tmp = tempfile::tempdir().unwrap();
        let run: Runner = Rc::new(|_| panic!("a ZIP that does not match is never unpacked"));
        let e =
            fetch_sumatra(tmp.path(), Path::new("sys"), &run, &serve(b"other"), &PINS).unwrap_err();
        assert_eq!(e.code, "sumatra");
        assert!(names_in(tmp.path()).is_empty());
    }

    #[test]
    fn an_exe_with_the_wrong_checksum_is_deleted_with_everything_around_it() {
        let tmp = tempfile::tempdir().unwrap();
        // an earlier exe must not survive a failed run either
        std::fs::write(tmp.path().join("SumatraPDF.exe"), b"old").unwrap();
        let e = fetch_sumatra(
            tmp.path(),
            Path::new("sys"),
            &fake_tar(b"wrongexe"),
            &serve(ZIP),
            &PINS,
        )
        .unwrap_err();
        assert_eq!(e.code, "sumatra");
        assert!(names_in(tmp.path()).is_empty());
    }

    #[test]
    fn a_failed_download_or_unpack_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let down = |_: &str| -> Result<Vec<u8>, String> { Err("offline".into()) };
        let run: Runner = Rc::new(|_| panic!("nothing to unpack"));
        let e = fetch_sumatra(tmp.path(), Path::new("sys"), &run, &down, &PINS).unwrap_err();
        assert_eq!(e.code, "sumatra");
        assert!(e.message.contains("offline"));
        assert!(names_in(tmp.path()).is_empty());

        let run: Runner = Rc::new(|_| Err("tar failed".into()));
        let e = fetch_sumatra(tmp.path(), Path::new("sys"), &run, &serve(ZIP), &PINS).unwrap_err();
        assert!(e.message.contains("tar failed"));
        assert!(names_in(tmp.path()).is_empty());
    }
}
