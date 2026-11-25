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
    format_printers, parse_patch, update_ini,
};
use printpapi_agent::discover::discover;
use printpapi_agent::install::{self, Installed, Layout, Os};
use printpapi_agent::marker::{self, BrandMark};
use printpapi_agent::pair::{self, PairError, Pairing};
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
    pub(crate) code: String,
    pub(crate) message: String,
}

impl SetupError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> SetupError {
        SetupError {
            code: code.into(),
            message: message.into(),
        }
    }

    /// The text for the person.
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn io(what: &str, e: std::io::Error) -> SetupError {
        SetupError::new("system", format!("{what}: {e}"))
    }
}

impl From<setup::ConnectError> for SetupError {
    fn from(e: setup::ConnectError) -> SetupError {
        SetupError::new(e.code(), e.to_string())
    }
}

impl From<PairError> for SetupError {
    fn from(e: PairError) -> SetupError {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRequest {
    server_url: Option<String>,
    /// `None` with a `server_url`: the connection was made by a pairing code, and the key is in
    /// this process (`AppState::paired`), never in the window.
    api_key: Option<String>,
    name: Option<String>,
    /// The pairing the window confirmed (`PairedView::pair_id`), when it names a connection by its
    /// address only: what the person saw is then what is installed, whatever was paired since.
    #[serde(default)]
    pair_id: Option<u64>,
    printers: Vec<PrinterChoice>,
    /// Set by `fill_paired`: the key came from a pairing.
    #[serde(skip)]
    paired: bool,
    /// Set by `fill_paired`: the window sent a key (the person typed the address), so the headers
    /// of the installed agent carry over, as documented. A pairing never asks for them: the agent keeps
    /// them only when the server is the same one.
    #[serde(skip)]
    keep_headers: bool,
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
    /// The link this window was started with (`printpapi://pair?...`), until the window takes it.
    link: Option<Pairing>,
    /// The connection a pairing code made: it holds the key, which stays here until the install.
    paired: Option<Conn>,
    /// Counts the pairings of this window; `paired` is the one with the number `paired_id`.
    pair_seq: u64,
    paired_id: u64,
}

impl AppState {
    pub(crate) fn staging(&mut self) -> Result<&Staging, SetupError> {
        match &mut self.staging {
            Some(s) => Ok(s),
            slot @ None => Ok(slot.insert(
                Staging::new()
                    .map_err(|e| SetupError::io("cannot create the working folder", e))?,
            )),
        }
    }

    /// A window started by a link (or told of one by the system).
    pub fn with_link(link: Option<Pairing>) -> AppState {
        AppState {
            link,
            ..AppState::default()
        }
    }

    /// The pending link, once: the window asks, and it is gone.
    pub fn take_link(&mut self) -> Option<PairingView> {
        self.link.take().map(PairingView::from)
    }

    /// A link that arrived while the window runs (macOS opens the running program).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn set_link(&mut self, link: Pairing) {
        self.link = Some(link);
    }

    /// Delete the working folder now (the window closes without dropping managed state).
    pub fn clear(&mut self) {
        self.staging = None;
        self.conn = None;
        self.link = None;
        self.paired = None;
    }
}

/// What the window is told of a link or a typed code. The key is never part of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    server_url: Option<String>,
    code: String,
}

impl From<Pairing> for PairingView {
    fn from(p: Pairing) -> PairingView {
        PairingView {
            server_url: p.server,
            code: p.code,
        }
    }
}

/// What a redeemed code says, for the second confirmation. No key.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedView {
    name: String,
    org: String,
    /// The creator's address, spelled out where it is not plain ASCII (`pair::shown_address`).
    created_by: Option<String>,
    /// `created_by` is not a bare e-mail address (`pair::is_plain_address`): punctuation around it or
    /// characters spelled out above. The window says so.
    created_by_escaped: bool,
    replaces: bool,
    /// Which pairing this is; the install must name it again (`ApplyRequest::pair_id`).
    pair_id: u64,
    /// The address the code was spent at, as this program used it. The window shows this one and
    /// sends it back, so what the person confirmed is what the install uses.
    server_url: String,
}

pub(crate) type Shared<'a> = State<'a, Mutex<AppState>>;

pub(crate) fn lock<'a>(state: &'a Shared) -> MutexGuard<'a, AppState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- machine

pub(crate) fn layout() -> Layout {
    install::layout(Os::host(), &install::program_files())
}

/// Windows `CREATE_NO_WINDOW`: without it a console window flashes up for every `schtasks`, `sc`
/// and `tasklist` the detection runs.
/// https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
#[cfg(windows)]
pub(crate) fn no_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
pub(crate) fn no_window(_: &mut std::process::Command) {}

pub(crate) fn runner() -> Runner {
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
///
/// An agent that this setup installed (`managed`) is looked at once more: who made it?
/// `installed` is the brand its marker names, `this` the brand of this build (`None`: stock).
/// - `other_brand`: this build may not replace it (a stock window over a brand's install, one
///   brand over another, a marker nobody can read, which is no proof of who made the agent). A
///   stock marker is `managed` or `adoptable`. The window offers removal only.
/// - `adoptable`: a brand's build over a stock install. One button takes it over.
/// - `managed`: the same maker, or a marker that cannot be looked at.
fn install_state(
    supported: bool,
    detect: impl FnOnce() -> Installed,
    installed: impl FnOnce() -> Option<BrandMark>,
    this: Option<&str>,
) -> &'static str {
    if !supported {
        return "unsupported";
    }
    match detect() {
        Installed::None => "none",
        Installed::Foreign => "foreign",
        Installed::Managed => match installed() {
            Some(mark) if !marker::may_replace(&mark, this) => "other_brand",
            Some(mark) if this.is_some_and(|id| mark != BrandMark::Brand(id.into())) => "adoptable",
            _ => "managed",
        },
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

/// The `agent.ini` of a new install: every setting this window knows. A change of an installed
/// agent is a patch (`patch_json`), which the elevated install merges into the installed file, so
/// that `[headers]`, `timeout` and comments stay and the key never has to be read here.
pub fn build_ini(
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
        updates: None,
    };
    Ok(update_ini(None, &update))
}

/// The arguments of an admin subcommand, with `--keep-pid <this window>` added to the two that end
/// tray icons. The icon and this window are one program, so ending "the icons" ends this window
/// too, before it can show the result (and on Windows locks the exe that is to be removed).
fn keep_pid_args(args: &[&OsStr], pid: u32) -> Vec<std::ffi::OsString> {
    let mut words: Vec<std::ffi::OsString> = args.iter().map(|a| (*a).to_os_string()).collect();
    if args
        .first()
        .is_some_and(|c| *c == OsStr::new("install") || *c == OsStr::new("uninstall"))
    {
        words.push("--keep-pid".into());
        words.push(pid.to_string().into());
    }
    words
}

/// `elevate` with `keep_pid_args`.
fn elevate_keeping_window(
    elevate: Elevator,
    pid: u32,
    agent: &Path,
    args: &[&OsStr],
) -> Result<(), Elevate> {
    let words = keep_pid_args(args, pid);
    let words: Vec<&OsStr> = words.iter().map(|w| w.as_os_str()).collect();
    elevate(agent, &words)
}

/// Whether the icon of the installed program is to be started for this user once a run is over.
/// `ended` is whether that run was to end the icons (it replaces or removes their program). A
/// success starts the new one. A failure after they were ended must not leave the user without
/// icon until the next logon, when there is a program to start; a refused prompt (`cancelled`)
/// changed nothing.
fn icon_wanted_again(failure: Option<&str>, ended: bool, installed: bool) -> bool {
    match failure {
        None => ended,
        Some("cancelled") => false,
        Some(_) => ended && installed,
    }
}

/// `start` the icon if `icon_wanted_again`, given how `outcome` came out.
fn restart_icon<T>(
    outcome: &Result<T, SetupError>,
    ended: bool,
    installed: bool,
    start: &dyn Fn(),
) {
    let failure = outcome.as_ref().err().map(|e| e.code.as_str());
    if icon_wanted_again(failure, ended, installed) {
        start();
    }
}

fn read_result(dir: &Path) -> Option<Value> {
    let bytes = std::fs::read(dir.join("result.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// How an admin subcommand gets started with administrator rights: `elevate::run` in the app, a
/// fake in the tests (`agent`, then the subcommand and the staging folder).
pub(crate) type Elevator<'a> = &'a dyn Fn(&Path, &[&OsStr]) -> Result<(), Elevate>;

/// Run one admin subcommand on the staging folder and read what it reports.
pub(crate) fn run_elevated(
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
    let this = crate::brand::current().map(|b| b.id.as_str());
    install_state(
        install::supported(os),
        || install::detect(os, &layout(), &install::system_dir(), &runner()),
        || marker::read(&layout().marker).map(|m| m.brand),
        this,
    )
}

#[tauri::command(async)]
pub fn default_name() -> String {
    setup::default_device_name()
}

/// Does this address need the person's confirmation (plain http outside the private network)? A
/// brand's fixed server comes first: a link to another server is refused here, not asked about, so
/// nobody is offered "Use it anyway" for an address the setup would refuse afterwards.
fn needs_insecure_confirm_in(
    brand: Option<&crate::brand::Brand>,
    url: &str,
) -> Result<bool, SetupError> {
    crate::brand::enforce_server(brand, url)?;
    Ok(setup::needs_insecure_confirm(url))
}

#[tauri::command(async)]
pub fn needs_insecure_confirm(url: String) -> Result<bool, SetupError> {
    needs_insecure_confirm_in(crate::brand::current(), &url)
}

/// `check_connection` without the state. A brand's fixed server is checked before the first
/// call. Returns the address as it was normalised, and whether the device name exists.
fn check_connection_in(
    brand: Option<&crate::brand::Brand>,
    http: &dyn printpapi_agent::client::Http,
    server_url: &str,
    api_key: &str,
    name: &str,
) -> Result<(String, bool), SetupError> {
    let server_url = setup::normalize_server_url(server_url)?;
    crate::brand::enforce_server(brand, &server_url)?;
    let exists = setup::check_connection(http, &server_url, api_key, name)?;
    Ok((server_url, exists))
}

#[tauri::command(async)]
pub fn check_connection(
    state: Shared,
    server_url: String,
    api_key: String,
    name: String,
) -> Result<bool, SetupError> {
    let settings = HttpSettings::default();
    let client = Client::new(&settings);
    let (server_url, exists) = check_connection_in(
        crate::brand::current(),
        &client,
        &server_url,
        &api_key,
        &name,
    )?;
    lock(&state).conn = Some(Conn {
        server_url,
        api_key,
        name,
        settings,
    });
    Ok(exists)
}

/// The link this window was started with, once. Polled by the window (no event permission needed).
#[tauri::command(async)]
pub fn take_link(state: Shared) -> Option<PairingView> {
    lock(&state).take_link()
}

/// What a pasted link or code says. No network: nothing is contacted before the person clicks.
#[tauri::command(async)]
pub fn read_pairing(text: String) -> Option<PairingView> {
    pair::parse(&text).map(PairingView::from)
}

/// Spend a pairing code. The address is checked and reached first (`setup::reach`), so a server
/// that cannot serve the setup shows up with the code unspent. The key stays in this process: the
/// window gets the account, the creator and the device, to confirm before anything changes.
#[tauri::command(async)]
pub fn pair(
    state: Shared,
    server_url: String,
    code: String,
    name: String,
    insecure_ok: bool,
) -> Result<PairedView, SetupError> {
    let settings = HttpSettings::default();
    let client = Client::new(&settings);
    let (mut view, conn) = pair_in(
        crate::brand::current(),
        &client,
        settings,
        &server_url,
        &code,
        &name,
        insecure_ok,
    )?;
    let mut guard = lock(&state);
    guard.conn = None;
    guard.paired = Some(conn);
    guard.pair_seq += 1;
    guard.paired_id = guard.pair_seq;
    view.pair_id = guard.paired_id;
    Ok(view)
}

/// `pair_with` behind the fixed-server check of a brand: before the address is reached, before
/// the person is asked about plain http and before the code is spent.
fn pair_in(
    brand: Option<&crate::brand::Brand>,
    http: &dyn printpapi_agent::client::Http,
    settings: HttpSettings,
    server_url: &str,
    code: &str,
    name: &str,
    insecure_ok: bool,
) -> Result<(PairedView, Conn), SetupError> {
    crate::brand::enforce_server(brand, server_url)?;
    pair_with(http, settings, server_url, code, name, insecure_ok)
}

/// The creator's address as the window shows it, and whether anything in it was spelled out.
fn shown_creator(created_by: Option<&str>) -> (Option<String>, bool) {
    (
        created_by.map(pair::shown_address),
        created_by.is_some_and(|a| !pair::is_plain_address(a)),
    )
}

/// `pair` without the state: the address is checked, plain http outside the private network is
/// refused unless the person agreed (before any call: the code and the key would cross it
/// readable), the server is reached, and only then the code is spent.
fn pair_with(
    http: &dyn printpapi_agent::client::Http,
    settings: HttpSettings,
    server_url: &str,
    code: &str,
    name: &str,
    insecure_ok: bool,
) -> Result<(PairedView, Conn), SetupError> {
    let server_url = pair::valid_server(server_url).ok_or(PairError::BadUrl)?;
    if setup::needs_insecure_confirm(&server_url) && !insecure_ok {
        return Err(SetupError::new(
            "insecure",
            "This address is not encrypted. The code and the key would travel readable over the network. Confirm that you want to use it.",
        ));
    }
    let code = pair::parse(code).ok_or(PairError::BadCode)?.code;
    let got = setup::pair_checked(http, &server_url, &code, name.trim())?;
    let (created_by, created_by_escaped) = shown_creator(got.created_by.as_deref());
    let view = PairedView {
        name: got.name.clone(),
        org: got.org,
        created_by,
        created_by_escaped,
        replaces: got.replaces,
        pair_id: 0,
        server_url: server_url.clone(),
    };
    let conn = Conn {
        server_url,
        api_key: got.key,
        name: got.name,
        settings,
    };
    Ok((view, conn))
}

/// The refusal when the window names a connection this process does not hold.
fn no_connection() -> SetupError {
    SetupError::new(
        "system",
        "There is no connection to install with. Go back to the first step.",
    )
}

/// A connection named by its address only is the pairing the window confirmed last: another code
/// may have been spent since (a second window state, a step that was left and entered again), and
/// the card the person said yes to is not the one whose key would be installed.
fn check_confirmed(request: &ApplyRequest, paired_id: u64) -> Result<(), SetupError> {
    let by_address = request.server_url.is_some() && request.api_key.is_none();
    if by_address && request.pair_id != Some(paired_id) {
        return Err(no_connection());
    }
    Ok(())
}

/// A connection the window names by its address only is the one a pairing made: the key, the
/// server and the name come from this process. Anything else with no key is an error. A key the
/// window sent means the person typed the address, and the installed headers carry over.
fn fill_paired(request: &mut ApplyRequest, paired: Option<&Conn>) -> Result<(), SetupError> {
    if request.server_url.is_none() || request.api_key.is_some() {
        request.keep_headers = request.api_key.is_some();
        return Ok(());
    }
    let none = no_connection;
    let asked = request
        .server_url
        .as_deref()
        .and_then(|u| setup::normalize_server_url(u).ok());
    let p = paired
        .filter(|p| asked.as_deref() == Some(p.server_url.as_str()))
        .ok_or_else(none)?;
    request.server_url = Some(p.server_url.clone());
    request.api_key = Some(p.api_key.clone());
    request.name = Some(p.name.clone());
    request.paired = true;
    request.keep_headers = false;
    Ok(())
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

/// The `[headers]` of the installed `agent.ini`, from a copy that an elevated `read-config` leaves
/// in the staging folder. The copy holds the headers only (never the key, whatever the installed
/// file looks like), and is deleted whatever happened: it is in a folder the user owns.
fn headers_of_installed(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
    let ini = dir.join("agent.ini");
    let _ = std::fs::remove_file(&ini);
    let ran = run_elevated(elevate, agent, dir, "read-config");
    let text = std::fs::read(&ini).map(|b| decode_ini(&b));
    let _ = std::fs::remove_file(&ini);
    ran?;
    let text = text.map_err(|e| SetupError::io("cannot read the installed settings", e))?;
    Config::from_text(&text)
        .and_then(|cfg| cfg.http_settings())
        .map(|settings| settings.headers)
        .map_err(|_| SetupError::new("system", "The installed agent.ini cannot be read."))
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

/// The name of the exe inside the pinned ZIP.
const SUMATRA_EXE_IN_ZIP: &str = "SumatraPDF-3.6.1-64.exe";
/// The pinned ZIP is 10 MB (10,031,830 bytes); the limit leaves room for a later pin.
const MAX_SUMATRA_ZIP: u64 = 64 * 1024 * 1024;

/// The pinned download: no key, none of agent.ini's headers, the browser User-Agent, the proxy
/// settings of this person, at most three minutes for all of it (`printpapi_agent::client::download`).
fn download(url: &str) -> Result<Vec<u8>, String> {
    printpapi_agent::client::download(url, MAX_SUMATRA_ZIP, Duration::from_secs(180))
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
) -> Result<Applied, SetupError> {
    let new_conn = match (&request.server_url, &request.api_key, &request.name) {
        (Some(url), Some(key), Some(name)) => {
            Some((setup::normalize_server_url(url)?, key.clone(), name.clone()))
        }
        _ => None,
    };
    if new_conn.is_none() {
        return Err(SetupError::new(
            "system",
            "There is no connection to install with. Go back to the first step.",
        ));
    }
    let text = build_ini(
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
    let ran = ran?;
    // A paired install is proven by the status file of this computer, never by the server's
    // `last_seen_at`: on a replacement PC the old agent keeps polling and would make an agent that
    // never started look online.
    if request.paired {
        return Ok(Applied {
            baseline: local_baseline(&ran)?,
            conn: None,
        });
    }
    let baseline = ran["baseline_last_seen"]
        .as_f64()
        .ok_or_else(|| SetupError::new("system", "The agent did not report a start time."))?;

    Ok(Applied {
        baseline,
        conn: new_conn.map(|(server_url, api_key, name)| Conn {
            server_url,
            api_key,
            name,
            // what the agent will use, headers included: the wait asks the new server with them
            settings: Config::from_text(&text)
                .ok()
                .and_then(|c| c.http_settings().ok())
                .unwrap_or_default(),
        }),
    })
}

/// The tray icon program this window offers to the install: staged as `tray` unless this window
/// already is the installed one (see `settings::stage_tray`). Unix paths are resolved first, so a
/// link to the installed program is still that program.
// ponytail: on Windows the running exe is compared as the system reports it, which is the long
// path the installer wrote; a window started through an 8.3 short path of the install folder
// would be taken for a copy. Ceiling: the window ends with the install then (taskkill by image).
#[allow(clippy::too_many_arguments)] // the machine's facts, injected one by one for the tests
fn stage_own_tray_with(
    brand: Option<&crate::brand::Brand>,
    os: Os,
    exe: &Path,
    appimage: Option<&OsStr>,
    appdir: Option<&OsStr>,
    l: &Layout,
    dir: &Path,
    run: &Runner,
) -> Result<bool, SetupError> {
    crate::brand::stage_brand(brand, dir)?;
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

/// Stage what the install needs from this program: the tray (whether one was staged is the
/// answer) and, for a branded build, `brand.json`, which names the brand in the marker. The apply
/// commands and the .msi refresh all come through here.
pub(crate) fn stage_own_tray(dir: &Path) -> Result<bool, SetupError> {
    let exe = std::env::current_exe().map_err(|e| SetupError::io("cannot find this program", e))?;
    stage_own_tray_with(
        crate::brand::current(),
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

/// Does this install merge into the installed `agent.ini` instead of writing a new one? A new
/// connection over an installed agent does: `[headers]` and `timeout` of the installed file must
/// survive, and the window never reads that file (it holds the key, which may be stored encrypted).
/// So the connection goes into the staging folder as a patch, and the elevated install merges it,
/// like any other change. A first install has nothing to keep, and a change of printers does not
/// touch the connection.
fn merges_into_installed(new_connection: bool, installed_by_setup: bool) -> bool {
    new_connection && installed_by_setup
}

/// `elevate::run` for the commands that end icons: they are told to spare this window.
fn window_elevator() -> impl Fn(&Path, &[&OsStr]) -> Result<(), Elevate> {
    |agent, args| elevate_keeping_window(&elevate::run, std::process::id(), agent, args)
}

/// Whatever came into the request, a brand with a fixed server installs for that server only.
fn enforce_fixed_server(
    brand: Option<&crate::brand::Brand>,
    request: &ApplyRequest,
) -> Result<(), SetupError> {
    match &request.server_url {
        Some(url) => crate::brand::enforce_server(brand, url),
        None => Ok(()),
    }
}

#[tauri::command(async)]
pub fn apply(state: Shared, mut request: ApplyRequest) -> Result<f64, SetupError> {
    // an installed agent that is newer than this program is not put back by accident
    crate::updates::older_guard(false)?;
    {
        let guard = lock(&state);
        check_confirmed(&request, guard.paired_id)?;
        fill_paired(&mut request, guard.paired.as_ref())?;
    }
    enforce_fixed_server(crate::brand::current(), &request)?;
    let (agent, dir) = staged(&state)?;
    let tray_staged = stage_own_tray(&dir)?;
    let elevate = window_elevator();
    let merged = merges_into_installed(request.server_url.is_some(), layout().marker.is_file());
    let paired = request.paired;
    let applied = if merged {
        // the window has no key to ask the server with: its wait reads the status file
        apply_over_installed_in(&elevate, &agent, &dir, &request).map(|baseline| Applied {
            baseline,
            conn: None,
        })
    } else {
        apply_in(&elevate, &agent, &dir, &request)
    };
    restart_icon(
        &applied,
        tray_staged,
        layout().tray.exists(),
        &start_installed_tray,
    );
    let applied = applied?;
    let mut guard = lock(&state);
    if merged || paired {
        guard.conn = None;
    }
    if let Some(conn) = applied.conn {
        guard.conn = Some(conn);
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
    let removed = run_elevated(&window_elevator(), &agent, &dir, "uninstall");
    // The removal ended the icons, this window excepted. When it failed, an icon that is still
    // installed comes back; when it worked, the window shows the result and cleans up after itself:
    // the working folder holds the unpacked agent, and nothing else needs it any more.
    if removed.is_err() {
        restart_icon(
            &removed,
            true,
            layout().tray.exists(),
            &start_installed_tray,
        );
    }
    removed?;
    lock(&state).clear();
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
    /// `(server_url, api_key, name)`, set by the install over an installed agent, never by the
    /// window.
    #[serde(skip)]
    connection: Option<(String, String, String)>,
    printers: Option<Vec<PrinterChoice>>,
    timeout: Option<f64>,
    /// `[name, value]` sets a header, `[name, null]` removes it.
    headers: Option<Vec<(String, Option<String>)>>,
    /// `notify`, `auto` or `off`.
    #[serde(default)]
    updates: Option<String>,
    /// Go back to this older program on purpose (see `updates::older_guard`).
    #[serde(default, rename = "goBack")]
    go_back: bool,
    /// Set with a connection the person typed: the installed headers carry over. Never from the
    /// window's own JSON.
    #[serde(skip)]
    keep_headers: bool,
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
    if let Some((url, key, name)) = &req.connection {
        obj.insert("server_url".into(), Value::String(url.clone()));
        obj.insert("api_key".into(), Value::String(key.clone()));
        obj.insert("name".into(), Value::String(name.clone()));
    }
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
    if let Some(u) = &req.updates {
        obj.insert("updates".into(), Value::String(u.clone()));
    }
    if req.keep_headers {
        obj.insert("keep_headers".into(), Value::Bool(true));
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

/// A new connection and the printers over an installed agent: one elevated install that merges
/// them into the installed file, so `[headers]` and `timeout` stay and there is one prompt.
fn apply_over_installed_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
    request: &ApplyRequest,
) -> Result<f64, SetupError> {
    let (Some(url), Some(key), Some(name)) = (&request.server_url, &request.api_key, &request.name)
    else {
        return Err(SetupError::new(
            "system",
            "There is no connection to install with. Go back to the first step.",
        ));
    };
    let patch = PatchRequest {
        connection: Some((setup::normalize_server_url(url)?, key.clone(), name.clone())),
        printers: Some(request.printers.clone()),
        timeout: None,
        headers: None,
        updates: None,
        go_back: false,
        keep_headers: request.keep_headers,
    };
    apply_patch_in(elevate, agent, dir, &patch)
}

/// Apply a change. Returns the time on this computer right before the agent was started again,
/// which `wait_online` then compares the agent's next contact with (the window has no key, so there
/// is no server-side start time to give).
#[tauri::command(async)]
pub fn apply_patch(state: Shared, patch: PatchRequest) -> Result<f64, SetupError> {
    crate::updates::older_guard(patch.go_back)?;
    let (agent, dir) = staged(&state)?;
    let tray_staged = stage_own_tray(&dir)?;
    let baseline = apply_patch_in(&window_elevator(), &agent, &dir, &patch);
    restart_icon(
        &baseline,
        tray_staged,
        layout().tray.exists(),
        &start_installed_tray,
    );
    let baseline = baseline?;
    // the wait that follows reads the status file, not a connection left over from the wizard
    lock(&state).conn = None;
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

/// The `[headers]` of the installed file, for Settings, Advanced, "show header values".
fn header_values_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
) -> Result<Vec<(String, String)>, SetupError> {
    Ok(headers_of_installed(elevate, agent, dir)?
        .into_iter()
        .collect())
}

#[tauri::command(async)]
pub fn header_values(state: Shared) -> Result<Vec<(String, String)>, SetupError> {
    let (agent, dir) = staged(&state)?;
    header_values_in(&elevate::run, &agent, &dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use printpapi_agent::config::parse_printers;
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
    fn a_printer_the_file_cannot_spell_is_refused() {
        assert!(build_ini(Some(("https://x", "k", "n")), &[choice("a;b", false, None)]).is_err());
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
        let state = |detect: Installed, mark: Option<BrandMark>, this: Option<&str>| {
            install_state(true, || detect, || mark, this)
        };
        assert_eq!(state(Installed::None, None, None), "none");
        assert_eq!(
            state(Installed::Managed, Some(BrandMark::Stock), None),
            "managed"
        );
        assert_eq!(state(Installed::Foreign, None, None), "foreign");
        assert_eq!(os_name(Os::Linux), "linux");
    }

    #[test]
    fn who_made_the_installed_agent_decides_between_managed_adoptable_and_other_brand() {
        let acme = || Some(BrandMark::Brand("acme".into()));
        let stock = || Some(BrandMark::Stock);
        let bad = || Some(BrandMark::Unreadable);
        let state = |mark: Option<BrandMark>, this: Option<&str>| {
            install_state(true, || Installed::Managed, || mark, this)
        };
        // stock over stock (and an install that has no marker to look at): as before
        assert_eq!(state(stock(), None), "managed");
        assert_eq!(state(None, None), "managed");
        assert_eq!(state(None, Some("acme")), "managed");
        // the same brand
        assert_eq!(state(acme(), Some("acme")), "managed");
        // a brand over a stock install: one button takes it over
        assert_eq!(state(stock(), Some("acme")), "adoptable");
        // a stock window over a brand's install, one brand over another, a marker nobody can read
        assert_eq!(state(acme(), None), "other_brand");
        assert_eq!(state(acme(), Some("other")), "other_brand");
        assert_eq!(state(bad(), None), "other_brand");
        assert_eq!(state(bad(), Some("acme")), "other_brand");
        // only an install that the setup made is looked at
        let none = install_state(true, || Installed::None, acme, None);
        assert_eq!(none, "none");
        let foreign = install_state(true, || Installed::Foreign, acme, Some("other"));
        assert_eq!(foreign, "foreign");
    }

    #[test]
    fn where_the_setup_cannot_install_nothing_is_looked_for() {
        let asked = Cell::new(false);
        let word = install_state(
            false,
            || {
                asked.set(true);
                Installed::Managed
            },
            || None,
            None,
        );
        assert_eq!(word, "unsupported");
        assert!(!asked.get(), "the machine must not be probed");
    }

    #[test]
    fn closing_the_window_deletes_the_working_folder_and_forgets_the_rest() {
        let mut state = AppState::default();
        let dir = state.staging().unwrap().dir().to_path_buf();
        std::fs::write(dir.join("agent.ini"), "key").unwrap();
        state.clear();
        assert!(!dir.exists());
        assert!(state.staging.is_none() && state.conn.is_none());
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
            head.contains(&format!(
                "user-agent: {}",
                printpapi_agent::client::BROWSER_UA.to_ascii_lowercase()
            )),
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
    /// What `read-config` leaves in the staging folder: the headers, never the key.
    const HEADERS_ONLY: &str = "[headers]\nCF-Access = abc\n";

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
    fn header_values_come_from_a_copy_that_holds_only_the_headers_and_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // a copy left behind by an earlier run must not be taken for this one's
        std::fs::write(dir.join("agent.ini"), "stale").unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!(args[0], "read-config");
            let dir = staging_dir_of(args);
            assert!(!dir.join("agent.ini").exists());
            std::fs::write(dir.join("agent.ini"), HEADERS_ONLY).unwrap();
            write_result(&dir, json!({"ok": true}));
            Ok(())
        };
        let headers = headers_of_installed(&elevate, Path::new("agent"), dir).unwrap();
        assert!(!dir.join("agent.ini").exists());
        assert_eq!(
            headers.into_iter().collect::<Vec<_>>(),
            vec![("cf-access".to_string(), "abc".to_string())]
        );
    }

    #[test]
    fn the_copy_is_deleted_after_a_failed_or_cancelled_read() {
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
                std::fs::write(dir.join("agent.ini"), HEADERS_ONLY).unwrap();
                if let Some(r) = &result {
                    write_result(&dir, r.clone());
                }
                outcome.clone()
            };
            let e = headers_of_installed(&elevate, Path::new("agent"), dir).expect_err("an error");
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
        let e =
            headers_of_installed(&elevate, Path::new("agent"), tmp.path()).expect_err("an error");
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
            pair_id: None,
            printers,
            paired: false,
            keep_headers: false,
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
        let applied = apply_in(&elevate, Path::new("agent"), dir, &req).unwrap();
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
            let e = apply_in(&elevate, Path::new("agent"), dir, &req)
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
        let e = apply_in(&elevate, Path::new("agent"), tmp.path(), &req)
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
        let e = apply_in(&elevate, Path::new("agent"), tmp.path(), &req)
            .err()
            .expect("an error");
        assert_eq!(e.code, "printers");
        assert!(!asked.get());
        assert!(names_in(tmp.path()).is_empty());
    }

    #[test]
    fn a_first_install_has_no_installed_file_to_keep() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            let dir = staging_dir_of(args);
            let ini = std::fs::read_to_string(dir.join("agent.ini")).unwrap();
            assert!(
                !ini.contains("[headers]") && !ini.contains("timeout"),
                "{ini}"
            );
            write_result(&dir, json!({"ok": true, "baseline_last_seen": 2.0}));
            Ok(())
        };
        let req = request(
            Some(("https://y", "NEW", "pc2")),
            vec![choice("A", false, None)],
        );
        let applied = apply_in(&elevate, Path::new("agent"), tmp.path(), &req).unwrap();
        assert_eq!(applied.conn.unwrap().settings, HttpSettings::default());
    }

    #[test]
    fn a_new_connection_over_an_installed_agent_is_merged_by_the_agent_not_read_by_the_window() {
        // the settings window knows no text of the installed file: reading it would be a second
        // administrator prompt, so the elevated install merges the connection into it instead
        assert!(merges_into_installed(true, true));
        // a change of printers (a patch of its own), or a first install
        assert!(!merges_into_installed(false, true));
        assert!(!merges_into_installed(true, false));
    }

    #[test]
    fn a_connection_over_an_installed_agent_asks_once_and_names_only_what_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // an agent.ini of an earlier run would win over the patch in the agent
        std::fs::write(dir.join("agent.ini"), "stale").unwrap();
        let asked = Cell::new(0);
        let elevate = |_: &Path, args: &[&OsStr]| {
            asked.set(asked.get() + 1);
            assert_eq!(args[0], "install", "no read-config before it");
            let dir = staging_dir_of(args);
            assert!(!dir.join("agent.ini").exists());
            let patch =
                parse_patch(&std::fs::read_to_string(dir.join("patch.json")).unwrap()).unwrap();
            assert_eq!(
                patch.connection,
                Some(("https://y".into(), "NEW-KEY".into(), "pc2".into()))
            );
            assert_eq!(patch.printers.as_deref(), Some("A"));
            // headers and timeout are the installed file's: not named here
            assert!(patch.headers.is_empty() && patch.timeout.is_none());
            write_result(&dir, json!({"ok": true, "baseline_local": 1794048802.0}));
            Ok(())
        };
        let req = request(
            Some(("https://y/", "NEW-KEY", "pc2")),
            vec![choice("A", false, None)],
        );
        let baseline = apply_over_installed_in(&elevate, Path::new("agent"), dir, &req).unwrap();
        assert_eq!(baseline, 1794048802.0);
        assert_eq!(asked.get(), 1);
        assert_eq!(names_in(dir), ["result.json"]);
    }

    // ---- changes of the settings

    fn patch_request(
        printers: Option<Vec<PrinterChoice>>,
        timeout: Option<f64>,
        headers: Option<Vec<(&str, Option<&str>)>>,
    ) -> PatchRequest {
        PatchRequest {
            connection: None,
            printers,
            timeout,
            headers: headers.map(|h| {
                h.into_iter()
                    .map(|(n, v)| (n.to_string(), v.map(String::from)))
                    .collect()
            }),
            updates: None,
            go_back: false,
            keep_headers: false,
        }
    }

    #[test]
    fn a_change_can_carry_the_update_mode_and_nothing_else_of_the_connection() {
        let mut req = patch_request(None, None, None);
        req.updates = Some("auto".into());
        assert_eq!(patch_json(&req).unwrap(), r#"{"updates":"auto"}"#);
        // the agent's own rule: three words only, so a prompt is never shown for a typo
        req.updates = Some("sometimes".into());
        assert_eq!(patch_json(&req).unwrap_err().code, "change");
        // the window sends goBack and updates by these names
        let parsed: PatchRequest =
            serde_json::from_str(r#"{"updates":"off","goBack":true}"#).unwrap();
        assert_eq!(parsed.updates.as_deref(), Some("off"));
        assert!(parsed.go_back);
        let plain: PatchRequest = serde_json::from_str("{}").unwrap();
        assert!(!plain.go_back && plain.updates.is_none());
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
    fn install_and_uninstall_are_told_which_process_is_this_window() {
        let pid = 4242;
        let dir = OsStr::new("/tmp/staging");
        let words = |args: &[&OsStr]| -> Vec<String> {
            keep_pid_args(args, pid)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        for command in ["install", "uninstall"] {
            assert_eq!(
                words(&[OsStr::new(command), dir]),
                [command, "/tmp/staging", "--keep-pid", "4242"]
            );
        }
        // the others end no icon
        for command in ["read-config", "restart"] {
            assert_eq!(
                words(&[OsStr::new(command), dir]),
                [command, "/tmp/staging"]
            );
        }
        // and the call that reaches the elevation carries it
        let seen = std::cell::RefCell::new(Vec::new());
        let elevate = |_: &Path, args: &[&OsStr]| {
            seen.borrow_mut()
                .extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
            Ok(())
        };
        elevate_keeping_window(
            &elevate,
            pid,
            Path::new("agent"),
            &[OsStr::new("uninstall"), dir],
        )
        .unwrap();
        assert_eq!(
            *seen.borrow(),
            ["uninstall", "/tmp/staging", "--keep-pid", "4242"]
        );
    }

    #[test]
    fn the_icon_comes_back_after_a_run_that_ended_it_fails() {
        // (failure code, icons were ended, an icon program is installed) -> start the icon
        let cases = [
            (None, true, true, true),
            (None, true, false, true),
            (None, false, true, false),
            (Some("system"), true, true, true),
            (Some("unreachable"), true, true, true),
            // a failure before it came to the icons, or without anything installed to start
            (Some("system"), false, true, false),
            (Some("system"), true, false, false),
            // a refused prompt changed nothing, the icons are still there
            (Some("cancelled"), true, true, false),
        ];
        for (failure, ended, installed, want) in cases {
            assert_eq!(
                icon_wanted_again(failure, ended, installed),
                want,
                "{failure:?} ended={ended} installed={installed}"
            );
        }
        let started = Cell::new(0);
        let err = SetupError::new("system", "boom");
        restart_icon(&Err::<(), _>(err.clone()), true, true, &|| {
            started.set(started.get() + 1)
        });
        restart_icon(&Ok(()), true, true, &|| started.set(started.get() + 1));
        restart_icon(&Err::<(), _>(err), false, true, &|| {
            started.set(started.get() + 1)
        });
        assert_eq!(started.get(), 2);
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
    fn header_values_come_from_the_headers_only_copy_and_the_key_never_reaches_this_side() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            assert_eq!(args[0], "read-config");
            let dir = staging_dir_of(args);
            std::fs::write(
                dir.join("agent.ini"),
                HEADERS_ONLY.replace("CF-Access = abc", "CF-Access = abc\nX-Other = 100%"),
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
        let staged = stage_own_tray_with(
            None,
            Os::Windows,
            installed_exe,
            None,
            None,
            &l,
            tmp.path(),
            &run,
        )
        .unwrap();
        assert!(!staged);
        assert!(names_in(tmp.path()).is_empty());
        // a copy somewhere else is offered, and the install replaces the installed one with it
        let elsewhere = tmp.path().join("printpapi-setup.exe");
        std::fs::write(&elsewhere, b"exe").unwrap();
        assert!(
            stage_own_tray_with(
                None,
                Os::Windows,
                &elsewhere,
                None,
                None,
                &l,
                tmp.path(),
                &run
            )
            .unwrap()
        );
        assert_eq!(std::fs::read(tmp.path().join("tray")).unwrap(), b"exe");
    }

    // ---- a brand with a fixed server: the wiring of the commands

    const FIXED: &str = "https://print.acme.example";

    #[test]
    fn a_fixed_server_is_checked_before_any_call_of_the_connection_check() {
        let brand = crate::brand::acme();
        let http = Recorder {
            urls: Default::default(),
            paired_key: None,
        };
        let e = check_connection_in(
            Some(&brand),
            &http,
            "https://print.other.example",
            KEY,
            "pc",
        )
        .unwrap_err();
        assert_eq!(e.code, "server_fixed");
        assert!(http.urls.borrow().is_empty(), "{:?}", http.urls);
        // the fixed server itself (any case, trailing slash) is reached (the recorder answers 401
        // for the key), and a stock build is free to ask any server
        let e = check_connection_in(
            Some(&brand),
            &http,
            "HTTPS://Print.Acme.example/",
            KEY,
            "pc",
        )
        .unwrap_err();
        assert_eq!(e.code, "bad_key");
        let reached = http.urls.borrow().len();
        assert!(reached > 0);
        let e =
            check_connection_in(None, &http, "https://print.other.example", KEY, "pc").unwrap_err();
        assert_eq!(e.code, "bad_key");
        assert!(http.urls.borrow().len() > reached);
    }

    #[test]
    fn a_fixed_server_comes_before_the_question_about_plain_http() {
        let brand = crate::brand::acme();
        // another server, plain http: refused as server_fixed, no "use it anyway"
        let e = needs_insecure_confirm_in(Some(&brand), "http://evil.example").unwrap_err();
        assert_eq!(e.code, "server_fixed");
        let e = needs_insecure_confirm_in(Some(&brand), "https://print.other.example").unwrap_err();
        assert_eq!(e.code, "server_fixed");
        // the fixed server is asked about as before (it is https here), and stock is unchanged
        assert!(!needs_insecure_confirm_in(Some(&brand), FIXED).unwrap());
        assert!(needs_insecure_confirm_in(None, "http://evil.example").unwrap());
        let mut plain = crate::brand::acme();
        plain.server = Some("http://print.acme.example".into());
        assert!(needs_insecure_confirm_in(Some(&plain), "http://print.acme.example").unwrap());
    }

    #[test]
    fn a_fixed_server_is_checked_before_the_address_is_reached_and_the_code_spent() {
        let brand = crate::brand::acme();
        let http = Recorder {
            urls: Default::default(),
            paired_key: Some("k".repeat(30).leak()),
        };
        // plain http to another server: refused as server_fixed, not asked about as insecure
        for other in ["https://print.other.example", "http://print.other.example"] {
            let e = pair_in(
                Some(&brand),
                &http,
                HttpSettings::default(),
                other,
                "7K3M-Q2ZD-9XHT",
                "pc",
                false,
            )
            .err()
            .expect("refused");
            assert_eq!(e.code, "server_fixed", "{other}");
        }
        assert!(http.urls.borrow().is_empty(), "{:?}", http.urls);
        let (view, _) = pair_in(
            Some(&brand),
            &http,
            HttpSettings::default(),
            FIXED,
            "7K3M-Q2ZD-9XHT",
            "pc",
            false,
        )
        .expect("paired");
        assert_eq!(view.server_url, FIXED);
        assert_eq!(http.urls.borrow().len(), 3);
    }

    #[test]
    fn an_apply_request_for_another_server_is_refused_under_a_fixed_server() {
        let brand = crate::brand::acme();
        let e = enforce_fixed_server(Some(&brand), &by_address("https://print.other.example"))
            .unwrap_err();
        assert_eq!(e.code, "server_fixed");
        enforce_fixed_server(Some(&brand), &by_address(FIXED)).unwrap();
        enforce_fixed_server(None, &by_address("https://print.other.example")).unwrap();
        // a request without an address (printers only) has nothing to enforce
        let mut no_server = by_address(FIXED);
        no_server.server_url = None;
        enforce_fixed_server(Some(&brand), &no_server).unwrap();
    }

    #[test]
    fn a_brand_is_staged_with_the_tray_and_a_stock_build_stages_no_brand_file() {
        let brand = crate::brand::acme();
        let tmp = tempfile::tempdir().unwrap();
        let run: Runner = Rc::new(|_| panic!("nothing is run"));
        let l = install::layout(Os::Windows, Path::new("C:\\Program Files"));
        let exe = tmp.path().join("printpapi-setup.exe");
        std::fs::write(&exe, b"exe").unwrap();
        stage_own_tray_with(
            Some(&brand),
            Os::Windows,
            &exe,
            None,
            None,
            &l,
            tmp.path(),
            &run,
        )
        .unwrap();
        let staged: Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("brand.json")).unwrap()).unwrap();
        assert_eq!(staged, json!({"id": "acme", "name": "Acme Print"}));
        let stock = tempfile::tempdir().unwrap();
        stage_own_tray_with(None, Os::Windows, &exe, None, None, &l, stock.path(), &run).unwrap();
        assert!(!stock.path().join("brand.json").exists());
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
    // ---- pairing

    fn paired_conn() -> Conn {
        Conn {
            server_url: "https://print.example.com".into(),
            api_key: KEY.into(),
            name: "warehouse-pc".into(),
            settings: HttpSettings::default(),
        }
    }

    fn by_address(url: &str) -> ApplyRequest {
        ApplyRequest {
            server_url: Some(url.into()),
            api_key: None,
            name: Some("typed in the window".into()),
            pair_id: None,
            printers: vec![choice("A", false, None)],
            paired: false,
            keep_headers: false,
        }
    }

    /// Every call a command makes, and nothing else: no network, no mock of the server's logic.
    struct Recorder {
        urls: std::cell::RefCell<Vec<String>>,
        /// `/computers` answers 401 and the pairing endpoint gives this key.
        paired_key: Option<&'static str>,
    }

    impl printpapi_agent::client::Http for Recorder {
        fn get(
            &self,
            url: &str,
            _: &str,
        ) -> Result<Option<Value>, printpapi_agent::client::HttpError> {
            self.urls.borrow_mut().push(format!("GET {url}"));
            if url.ends_with("/computers") {
                Err(printpapi_agent::client::HttpError::Os(
                    "server returned 401".into(),
                ))
            } else {
                Ok(Some(json!({"ok": true})))
            }
        }
        fn get_bytes(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<Vec<u8>>, printpapi_agent::client::HttpError> {
            unreachable!("no download here")
        }
        fn post(
            &self,
            url: &str,
            _: &str,
            _: &Value,
        ) -> Result<Option<Value>, printpapi_agent::client::HttpError> {
            self.urls.borrow_mut().push(format!("POST {url}"));
            Ok(self.paired_key.map(|k| {
                json!({"key": k, "name": "warehouse-pc", "org": "Acme", "created_by": null,
                       "replaces": false})
            }))
        }
    }

    #[test]
    fn plain_http_is_refused_before_any_call_unless_the_person_agreed() {
        let http = Recorder {
            urls: Default::default(),
            paired_key: Some("k".repeat(30).leak()),
        };
        let server = "http://print.example.com";
        let e = pair_with(
            &http,
            HttpSettings::default(),
            server,
            "7K3M-Q2ZD-9XHT",
            "pc",
            false,
        )
        .err()
        .expect("refused");
        assert_eq!(e.code, "insecure");
        assert!(
            http.urls.borrow().is_empty(),
            "nothing was contacted: {:?}",
            http.urls
        );
        // agreed: reach, then the spend, in that order, on exactly that address
        let (view, conn) = pair_with(
            &http,
            HttpSettings::default(),
            server,
            "7K3M-Q2ZD-9XHT",
            "pc",
            true,
        )
        .expect("paired");
        assert_eq!(
            *http.urls.borrow(),
            [
                "GET http://print.example.com/health",
                "GET http://print.example.com/computers",
                "POST http://print.example.com/agent/pair",
            ]
        );
        assert_eq!(view.server_url, "http://print.example.com");
        assert_eq!(conn.server_url, view.server_url);
        // https needs no agreement; an address with a user name never gets that far
        let https = Recorder {
            urls: Default::default(),
            paired_key: Some("k".repeat(30).leak()),
        };
        assert!(
            pair_with(
                &https,
                HttpSettings::default(),
                "https://print.example.com/",
                "7K3M-Q2ZD-9XHT",
                "pc",
                false
            )
            .is_ok()
        );
        let bad = Recorder {
            urls: Default::default(),
            paired_key: None,
        };
        let e = pair_with(
            &bad,
            HttpSettings::default(),
            "https://user@print.example.com",
            "7K3M-Q2ZD-9XHT",
            "pc",
            true,
        )
        .err()
        .expect("refused");
        assert_eq!(e.code, "bad_url");
        assert!(bad.urls.borrow().is_empty());
    }

    #[test]
    fn a_link_is_taken_once_and_clear_drops_link_and_paired_connection() {
        let link = pair::parse(
            "printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT",
        );
        let mut s = AppState::with_link(link);
        assert_eq!(
            s.take_link(),
            Some(PairingView {
                server_url: Some("https://print.example.com".into()),
                code: "7K3M-Q2ZD-9XHT".into()
            })
        );
        assert_eq!(s.take_link(), None, "once");
        s.set_link(pair::parse("7K3M-Q2ZD-9XHT").unwrap());
        s.paired = Some(paired_conn());
        s.conn = Some(paired_conn());
        s.clear();
        assert!(s.take_link().is_none() && s.paired.is_none() && s.conn.is_none());
    }

    #[test]
    fn the_window_shows_a_code_or_link_without_any_network() {
        let v = read_pairing("  7k3m q2zd 9xht ".into()).unwrap();
        assert_eq!((v.server_url, v.code.as_str()), (None, "7K3M-Q2ZD-9XHT"));
        let v = read_pairing(
            "printpapi://pair?server=http%3A%2F%2F192.168.1.5%3A3460&code=7K3M-Q2ZD-9XHT".into(),
        )
        .unwrap();
        assert_eq!(v.server_url.as_deref(), Some("http://192.168.1.5:3460"));
        assert!(
            read_pairing(
                "printpapi://pair?server=https%3A%2F%2Fa@b.example&code=7K3M-Q2ZD-9XHT".into()
            )
            .is_none()
        );
        assert!(read_pairing("hello".into()).is_none());
    }

    #[test]
    fn the_install_takes_only_the_pairing_the_window_confirmed() {
        let s = AppState {
            paired: Some(paired_conn()),
            pair_seq: 2,
            paired_id: 2,
            ..Default::default()
        };
        let named = |id: Option<u64>| {
            let mut r = by_address("https://print.example.com");
            r.pair_id = id;
            r
        };
        // the card the person said yes to is the one that was paired last
        check_confirmed(&named(Some(2)), s.paired_id).unwrap();
        // the card of an earlier pairing (the step was left and entered again), or none at all
        for id in [Some(1), Some(3), None] {
            let e = check_confirmed(&named(id), s.paired_id).unwrap_err();
            assert_eq!(e.code, "system", "{id:?}");
        }
        // a key the person typed has nothing to confirm
        let mut typed = by_address("https://y");
        typed.api_key = Some("TYPED".into());
        check_confirmed(&typed, s.paired_id).unwrap();
        // a change of printers names no connection
        check_confirmed(&request(None, vec![choice("A", false, None)]), 0).unwrap();
    }

    #[test]
    fn the_confirmation_spells_out_an_address_that_is_not_plain_ascii() {
        let https = Recorder {
            urls: Default::default(),
            paired_key: Some("k".repeat(30).leak()),
        };
        let (view, _) = pair_with(
            &https,
            HttpSettings::default(),
            "https://print.example.com",
            "7K3M-Q2ZD-9XHT",
            "pc",
            false,
        )
        .expect("paired");
        assert!(!view.created_by_escaped && view.created_by.is_none());
        assert_eq!(shown_creator(None), (None, false));
        assert_eq!(
            shown_creator(Some("boss@acme.example")),
            (Some("boss@acme.example".into()), false)
        );
        assert_eq!(
            shown_creator(Some("boss@\u{430}cme.example")),
            (Some("boss@\\u{430}cme.example".into()), true)
        );
        assert_eq!(
            shown_creator(Some("boss@acme.example\u{1cbb}")),
            (Some("boss@acme.example\\u{1cbb}".into()), true)
        );
        // the same letters with plain ASCII punctuation around them are not a bare address either
        for odd in [
            "<boss@acme.example>",
            "boss@acme.example.",
            "\"boss@acme.example\"",
            "boss@acme.example,",
            "mailto:boss@acme.example",
            "boss@acme.example (owner)",
        ] {
            assert_eq!(shown_creator(Some(odd)), (Some(odd.into()), true), "{odd}");
        }
        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("createdByEscaped").is_some() && json.get("pairId").is_some());
        assert!(json.get("apiKey").is_none() && json.get("key").is_none());
    }

    #[test]
    fn fill_paired_takes_key_server_and_name_only_for_the_same_address() {
        let mut r = by_address("https://print.example.com/");
        fill_paired(&mut r, Some(&paired_conn())).unwrap();
        assert_eq!(r.server_url.as_deref(), Some("https://print.example.com"));
        assert_eq!(r.api_key.as_deref(), Some(KEY));
        assert_eq!(
            r.name.as_deref(),
            Some("warehouse-pc"),
            "the server's name, not the window's"
        );
        assert!(
            r.paired && !r.keep_headers,
            "a pairing never asks to keep headers"
        );
        // another address, or nothing paired: an error, and nothing is filled
        for (url, conn) in [
            ("https://other.example.org", Some(paired_conn())),
            ("https://print.example.com", None),
        ] {
            let mut r = by_address(url);
            let e = fill_paired(&mut r, conn.as_ref()).unwrap_err();
            assert_eq!(e.code, "system");
            assert_eq!(
                e.message,
                "There is no connection to install with. Go back to the first step."
            );
            assert!(r.api_key.is_none() && !r.paired);
        }
    }

    #[test]
    fn a_typed_key_keeps_the_headers_a_pairing_does_not_and_a_change_of_printers_has_no_connection()
    {
        let mut typed = by_address("https://y");
        typed.api_key = Some("TYPED".into());
        fill_paired(&mut typed, Some(&paired_conn())).unwrap();
        assert!(typed.keep_headers && !typed.paired);
        let mut paired = by_address("https://print.example.com");
        fill_paired(&mut paired, Some(&paired_conn())).unwrap();
        assert!(!paired.keep_headers);
        let mut no_connection = request(None, vec![choice("A", false, None)]);
        fill_paired(&mut no_connection, None).unwrap();
        assert!(!no_connection.keep_headers);

        // the flag reaches the patch the install merges, and only for the typed key
        for (req, want) in [(typed, true), (paired, false)] {
            let tmp = tempfile::tempdir().unwrap();
            let seen = Cell::new(None);
            let elevate = |_: &Path, args: &[&OsStr]| {
                let dir = staging_dir_of(args);
                let text = std::fs::read_to_string(dir.join("patch.json")).unwrap();
                seen.set(Some(parse_patch(&text).unwrap().keep_headers));
                write_result(&dir, json!({"ok": true, "baseline_local": 5.0}));
                Ok(())
            };
            apply_over_installed_in(&elevate, Path::new("agent"), tmp.path(), &req).unwrap();
            assert_eq!(seen.get(), Some(want));
        }
        // and build_ini of a first install has no installed file to keep anything of
        let ini = build_ini(Some(("https://x", KEY, "pc")), &[choice("A", false, None)]).unwrap();
        assert!(!ini.contains("[headers]"));
    }

    #[test]
    fn a_paired_install_waits_on_the_status_file_and_keeps_no_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let elevate = |_: &Path, args: &[&OsStr]| {
            let dir = staging_dir_of(args);
            // the server's last_seen_at would come from the old agent on another computer
            write_result(
                &dir,
                json!({"ok": true, "baseline_local": 42.0, "baseline_last_seen": 7.0}),
            );
            Ok(())
        };
        let mut req = by_address("https://print.example.com");
        fill_paired(&mut req, Some(&paired_conn())).unwrap();
        let applied = apply_in(&elevate, Path::new("agent"), tmp.path(), &req).unwrap();
        assert_eq!(applied.baseline, 42.0);
        assert!(
            applied.conn.is_none(),
            "wait_online then reads the status file"
        );
        // a typed key keeps today's proof
        let applied = apply_in(
            &elevate,
            Path::new("agent"),
            tmp.path(),
            &request(
                Some(("https://y", "TYPED", "pc")),
                vec![choice("A", false, None)],
            ),
        )
        .unwrap();
        assert_eq!(applied.baseline, 7.0);
        assert!(applied.conn.is_some());
    }

    #[test]
    fn the_paired_key_is_in_no_argument_no_error_and_no_result() {
        let tmp = tempfile::tempdir().unwrap();
        let args_seen = std::cell::RefCell::new(String::new());
        let elevate = |_: &Path, args: &[&OsStr]| {
            *args_seen.borrow_mut() = args
                .iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            let dir = staging_dir_of(args);
            // the key reaches the agent as a file in the private folder, and only there
            let ini = std::fs::read_to_string(dir.join("agent.ini")).unwrap();
            assert!(ini.contains(KEY));
            write_result(&dir, json!({"ok": true, "baseline_local": 1.0}));
            Ok(())
        };
        let mut req = by_address("https://print.example.com");
        fill_paired(&mut req, Some(&paired_conn())).unwrap();
        apply_in(&elevate, Path::new("agent"), tmp.path(), &req).unwrap();
        assert!(!args_seen.borrow().contains(KEY));
        assert_eq!(
            names_in(tmp.path()),
            ["result.json"],
            "the file with the key is gone"
        );
        assert!(
            !std::fs::read_to_string(tmp.path().join("result.json"))
                .unwrap()
                .contains(KEY)
        );
        // no error the pairing can make holds a key, and the texts are the documented ones
        for e in [
            PairError::BadUrl,
            PairError::BadCode,
            PairError::NameTaken,
            PairError::BadName,
            PairError::Throttled,
            PairError::OldServer,
            PairError::Proxy,
            PairError::Unreachable("connection failed".into()),
            PairError::BadReply,
        ] {
            let s: SetupError = e.clone().into();
            assert_eq!(
                (s.code.as_str(), s.message.as_str()),
                (e.code(), e.to_string().as_str())
            );
            assert!(!s.message.contains(KEY));
        }
    }
}
