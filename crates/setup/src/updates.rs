// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Updates in the window and the tray icon: what is known (no network), the daily look at the signed
//! file, the download into the private staging folder, and the elevated install of the installed
//! agent's own `restart --update-from <staging>`. Everything that matters is checked again by the
//! elevated agent (`printpapi_agent::update`); the window only fetches and shows.
//!
//! The window never writes anything outside its staging folder and its cache file, and it does not
//! contact the printpapi server: the one request is for `printpapi-update.txt` at the release
//! folder named by the agent's own status file (default GitHub, or `update_url`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use printpapi_agent::client::{DownloadError, DownloadFn};
use printpapi_agent::install::{Layout, Os};
use printpapi_agent::marker;
use printpapi_agent::update::{
    self, DEFAULT_BASE, MANIFEST_NAME, MAX_MANIFEST, State, TrayKind, Version, open_signed, pick,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::{SetupError, Shared, lock};
use crate::elevate::{self, Elevate};
use crate::tray;

/// The release page of a version, for "Release notes".
const NOTES_URL: &str = "https://github.com/LukiPrince/printpapi/releases/tag/v";
const MANIFEST_DEADLINE: Duration = Duration::from_secs(30);
const FILE_DEADLINE: Duration = Duration::from_secs(600);
/// An answer (a 404 and a refused file too) is trusted for a day, a network failure for an hour.
const CACHE_OK_SECS: f64 = 24.0 * 3600.0;
const CACHE_ERROR_SECS: f64 = 3600.0;

/// The last attempt of an update, from `update-state.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastAttempt {
    version: String,
    ok: bool,
    message: String,
    at: f64,
}

/// What the window knows about updates (the contract is `UpdateInfo` in `web/setup/api.ts`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// This build has a public key, so it can check and install.
    supported: bool,
    /// `notify`, `auto`, `off`, or `unknown` (no status file, or an agent older than the field).
    mode: String,
    /// The version of this program.
    own: String,
    /// The version of the installed agent: the marker, else the status file, else unknown.
    installed: Option<String>,
    /// This window runs from the installed program.
    from_installed: bool,
    /// The newest version the last look found.
    latest: Option<String>,
    checked_at: Option<f64>,
    last_attempt: Option<LastAttempt>,
    error: Option<String>,
}

/// The answer of a look, kept per user. Display only: installing fetches afresh.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    checked_at: f64,
    base: String,
    latest: Option<String>,
    /// `ok`, `none` (404: nothing published yet), `refused` (an answer this program will not
    /// take: another key, another schema; it stays as it is until a new program) or `error`
    /// (the network).
    kind: String,
    /// What went wrong, for `error` (never a key or a header: the request has none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// The version of the program that asked. A `refused` answer was made by that program's keys
    /// and schema: a newer program, installed by hand as SECURITY.md says, may take what it
    /// refused. A file from before this field counts as another program's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    by: Option<String>,
}

/// A `refused` answer counts only for the program that got it; every other answer is the same
/// for every program.
fn answer_fits(cache: &Cache, own: &str) -> bool {
    cache.kind != "refused" || cache.by.as_deref() == Some(own)
}

/// Is the cache still an answer for `base` at `now`?
pub fn cache_fresh(cache: &Cache, base: &str, now: f64) -> bool {
    let limit = if cache.kind == "error" {
        CACHE_ERROR_SECS
    } else {
        CACHE_OK_SECS
    };
    cache.base == base && now >= cache.checked_at && now - cache.checked_at < limit
}

/// The mode word and the folder of the status file (`updates`, `update_url`). A missing field is
/// `unknown`, which is never looked up; `update_url` `null` is the release folder.
pub fn settings_of_status(text: Option<&str>) -> (String, Option<String>) {
    let Some(v) = text.and_then(|t| serde_json::from_str::<Value>(t).ok()) else {
        return ("unknown".into(), None);
    };
    let mode = match v.get("updates").and_then(Value::as_str) {
        Some(m @ ("notify" | "auto" | "off")) => m.to_string(),
        _ => "unknown".into(),
    };
    let base = match v.get("update_url") {
        Some(Value::String(url)) => update::valid_base(url),
        Some(Value::Null) | None => Some(DEFAULT_BASE.to_string()),
        _ => None,
    };
    (mode, base)
}

/// The installed version: the marker's, else the version of the running agent's status file.
pub fn installed_version(
    marker_version: Option<&str>,
    status_text: Option<&str>,
) -> Option<String> {
    marker_version
        .and_then(Version::parse)
        .or_else(|| {
            status_text
                .and_then(|t| serde_json::from_str::<Value>(t).ok())
                .and_then(|v| v.get("version")?.as_str().and_then(Version::parse))
        })
        .map(|v| v.to_string())
}

/// Is the installed agent newer than this program?
pub fn installed_is_newer(installed: Option<&str>, own: &str) -> bool {
    matches!((installed.and_then(Version::parse), Version::parse(own)), (Some(i), Some(o)) if i > o)
}

/// Everything `info` needs, read from the machine or made up by a test.
#[derive(Clone)]
pub struct Facts {
    pub supported: bool,
    pub status_text: Option<String>,
    pub marker_version: Option<String>,
    pub own: String,
    pub from_installed: bool,
    pub cache: Option<Cache>,
    pub state: Option<State>,
}

/// Why a look is not made when the status file says `off`: the agent writes that for `updates = off`
/// and for an `update_url` it cannot use (it needs http:// or https://, no user name, no password,
/// no ? and no #).
const OFF_TEXT: &str = "Checking is switched off in agent.ini (updates = off, or an update_url that is not a usable folder).";

/// What "nothing published" says: a 404 is no error of the network, but it is not "up to date".
const NOT_PUBLISHED: &str = "No signed update has been published yet.";

/// The window's view, without the network. An answer counts only while the mode looks things up
/// and for the folder it was made for: after "off", or a changed mirror, there is no notice.
pub fn info(f: &Facts) -> UpdateInfo {
    let (mode, base) = settings_of_status(f.status_text.as_deref());
    let looks = mode == "notify" || mode == "auto";
    let cache = f
        .cache
        .as_ref()
        .filter(|c| looks && base.as_deref() == Some(c.base.as_str()) && answer_fits(c, &f.own));
    let (latest, checked_at, error) = match cache {
        Some(c) if c.kind == "error" || c.kind == "refused" => {
            (None, Some(c.checked_at), c.error.clone())
        }
        Some(c) if c.kind == "none" => (None, Some(c.checked_at), Some(NOT_PUBLISHED.into())),
        Some(c) => (c.latest.clone(), Some(c.checked_at), None),
        None => (None, None, None),
    };
    UpdateInfo {
        supported: f.supported,
        mode,
        own: f.own.clone(),
        installed: installed_version(f.marker_version.as_deref(), f.status_text.as_deref()),
        from_installed: f.from_installed,
        latest,
        checked_at,
        last_attempt: f.state.as_ref().map(|s| LastAttempt {
            version: s.version.clone(),
            ok: s.ok,
            message: s.message.clone(),
            at: s.at,
        }),
        error,
    }
}

/// The signed file of `base` looked at. `Err` is the sentence for the window.
fn fetch_manifest(
    base: &str,
    keys: &[[u8; 32]],
    download: &DownloadFn,
) -> Result<Option<printpapi_agent::update::Manifest>, SetupError> {
    match download(
        &format!("{base}/{MANIFEST_NAME}"),
        MAX_MANIFEST as u64,
        MANIFEST_DEADLINE,
    ) {
        Ok(bytes) => open_signed(&bytes, keys).map(Some).map_err(from_apply),
        Err(DownloadError::NotFound) => Ok(None),
        Err(DownloadError::Other(e)) => Err(SetupError::new(
            "unreachable",
            format!("The update server could not be reached: {e}"),
        )),
    }
}

fn from_apply(e: printpapi_agent::apply::ApplyError) -> SetupError {
    SetupError::new(e.code(), e.to_string())
}

/// Look for a newer version, when the status file allows it. `force` ignores the cache. The
/// outcome is cached: a day for an answer and for a 404, an hour for a failure.
pub fn check(
    f: &Facts,
    force: bool,
    now: f64,
    keys: &[[u8; 32]],
    download: &DownloadFn,
) -> (UpdateInfo, Option<Cache>) {
    let (mode, base) = settings_of_status(f.status_text.as_deref());
    let allowed = f.supported && !keys.is_empty() && (mode == "notify" || mode == "auto");
    let Some(base) = base.filter(|_| allowed) else {
        return (info(f), None);
    };
    if !force
        && f.cache
            .as_ref()
            .is_some_and(|c| cache_fresh(c, &base, now) && answer_fits(c, &f.own))
    {
        return (info(f), None);
    }
    let cache = match fetch_manifest(&base, keys, download) {
        Ok(Some(m)) => Cache {
            checked_at: now,
            base,
            latest: Some(m.version.to_string()),
            kind: "ok".into(),
            error: None,
            by: Some(f.own.clone()),
        },
        Ok(None) => Cache {
            checked_at: now,
            base,
            latest: None,
            kind: "none".into(),
            error: None,
            by: Some(f.own.clone()),
        },
        Err(e) => Cache {
            checked_at: now,
            base,
            latest: None,
            // only the network may be better in an hour; a file that is refused stays refused
            kind: if e.code == "unreachable" {
                "error"
            } else {
                "refused"
            }
            .into(),
            error: Some(e.message().to_string()),
            by: Some(f.own.clone()),
        },
    };
    let facts = Facts {
        cache: Some(cache.clone()),
        ..f.clone()
    };
    (info(&facts), Some(cache))
}

/// Fetch the signed file and the files of this system into `dir` (the private staging folder), each
/// checked against size and SHA-256. Returns the version. `kind` is what is installed as the tray.
/// `installed` is the version of the installed agent (not of this window, which may be newer):
/// the elevated agent compares with the same one.
pub fn download_into(
    dir: &Path,
    base: &str,
    os: Os,
    kind: TrayKind,
    installed: Version,
    keys: &[[u8; 32]],
    download: &DownloadFn,
) -> Result<String, SetupError> {
    let manifest_bytes = match download(
        &format!("{base}/{MANIFEST_NAME}"),
        MAX_MANIFEST as u64,
        MANIFEST_DEADLINE,
    ) {
        Ok(b) => b,
        Err(DownloadError::NotFound) => {
            return Err(SetupError::new("not_published", NOT_PUBLISHED));
        }
        Err(DownloadError::Other(e)) => {
            return Err(SetupError::new(
                "unreachable",
                format!("The update server could not be reached: {e}"),
            ));
        }
    };
    let manifest = open_signed(&manifest_bytes, keys).map_err(from_apply)?;
    if manifest.version <= installed {
        return Err(SetupError::new(
            "not_newer",
            format!("Version {installed} is the newest there is."),
        ));
    }
    let (agent, tray) = pick(&manifest, &update::target(os), kind).map_err(from_apply)?;
    crate::staging::write_private(dir, MANIFEST_NAME, &manifest_bytes)
        .map_err(|e| SetupError::new("system", format!("cannot save the update: {e}")))?;
    for r in std::iter::once(&agent).chain(tray.as_ref()) {
        let bytes = match download(&format!("{base}/{}", r.file), r.size, FILE_DEADLINE) {
            Ok(b) => b,
            Err(DownloadError::NotFound) => return Err(bad_download()),
            Err(DownloadError::Other(e)) => {
                return Err(SetupError::new(
                    "unreachable",
                    format!("The update server could not be reached: {e}"),
                ));
            }
        };
        if !r.matches(&bytes) {
            return Err(bad_download());
        }
        crate::staging::write_private(dir, &r.file, &bytes)
            .map_err(|e| SetupError::new("system", format!("cannot save the update: {e}")))?;
    }
    Ok(manifest.version.to_string())
}

fn bad_download() -> SetupError {
    SetupError::new(
        "bad_file",
        "A downloaded file is not the one the release lists. Try again in a few minutes.",
    )
}

/// Delete what `download_into` staged, whatever happened to it: a name list is read from the folder
/// itself (the signed file names the others), so nothing else in the folder is touched.
pub fn clean_staged(dir: &Path) {
    let names: Vec<String> = std::fs::read(dir.join(MANIFEST_NAME))
        .ok()
        .and_then(|b| {
            let text = String::from_utf8_lossy(&b).into_owned();
            let payload = text.split("\n\n").nth(1)?.to_string();
            let v: Value = serde_json::from_str(&payload).ok()?;
            let mut names = Vec::new();
            for target in v.get("targets")?.as_object()?.values() {
                for role in target.as_object()?.values() {
                    if let Some(f) = role.get("file").and_then(Value::as_str) {
                        names.push(f.to_string());
                    }
                }
            }
            Some(names)
        })
        .unwrap_or_default();
    for name in names.iter().map(String::as_str).chain([MANIFEST_NAME]) {
        // a name from a file we wrote ourselves, but never trust it as a path
        if !name.contains(['/', '\\']) && !name.starts_with('.') {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
}

/// How the elevated run is started: the installed agent, the arguments (a fake in tests).
pub type Elevator<'a> = &'a dyn Fn(&Path, &[&OsStr]) -> Result<(), Elevate>;

/// Install what `download_into` staged: the installed agent (`agent`, the one only administrators
/// can write) is started elevated with `restart --update-from <dir>` and reports through
/// `result.json`. Refused when the marker holds no version (an installation of an older setup
/// cannot be updated this way: it is brought up to date by this setup instead). The staged files
/// are deleted whatever the outcome.
pub fn install_in(
    elevate: Elevator,
    agent: &Path,
    dir: &Path,
    marker_version: Option<&str>,
) -> Result<String, SetupError> {
    let result = (|| {
        if marker_version.and_then(Version::parse).is_none() {
            return Err(SetupError::new(
                "no_version",
                "This installation was made by an older setup, so it cannot update itself yet. Use Install from this setup.",
            ));
        }
        // an old answer must not be mistaken for this run's
        let _ = std::fs::remove_file(dir.join("result.json"));
        let elevated = elevate(
            agent,
            &[
                OsStr::new("restart"),
                OsStr::new("--update-from"),
                dir.as_os_str(),
            ],
        );
        let read = std::fs::read(dir.join("result.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        crate::commands::outcome(elevated, read)
            .map(|v| v["version"].as_str().unwrap_or_default().to_string())
    })();
    clean_staged(dir);
    result
}

// ---- the real machine

fn layout() -> Layout {
    printpapi_agent::install::layout(Os::host(), &printpapi_agent::install::program_files())
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// `update-check.json` beside the lock file of the tray icon.
fn cache_path() -> Option<PathBuf> {
    tray::lock_path().map(|p| p.with_file_name("update-check.json"))
}

fn read_cache() -> Option<Cache> {
    let bytes = std::fs::read(cache_path()?).ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn write_cache(cache: &Cache) {
    if let Some(path) = cache_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, serde_json::to_vec(cache).unwrap_or_default());
    }
}

/// Is the program that started as `exe` the installed tray program? What the tray install would
/// copy (`settings::tray_source`: the AppImage file, not the exe inside its mount) is compared
/// with the installed one, the same way the install decides to stage nothing.
pub fn from_installed_at(
    os: Os,
    exe: &Path,
    appimage: Option<&OsStr>,
    appdir: Option<&OsStr>,
    installed: &Path,
) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match os {
        Os::Macos => canon(exe).starts_with(canon(installed)),
        _ => {
            let appdir = appdir.map(|d| canon(Path::new(d)));
            crate::settings::tray_source(
                os,
                &canon(exe),
                appimage,
                appdir.as_deref().map(Path::as_os_str),
            )
            .is_some_and(|source| {
                crate::settings::is_installed(os, &canon(&source), &canon(installed))
            })
        }
    }
}

static FROM_INSTALLED: OnceLock<bool> = OnceLock::new();

/// Is this program the installed tray program (so the window is "from the installed one")? Decided
/// once, when the program starts (`note_start`): an update that replaces the running file leaves
/// `/proc/self/exe` pointing at a deleted file on Linux, and this program is still the installed
/// one that was replaced.
fn runs_from_installed() -> bool {
    *FROM_INSTALLED.get_or_init(|| {
        std::env::current_exe().is_ok_and(|exe| {
            from_installed_at(
                Os::host(),
                &exe,
                std::env::var_os("APPIMAGE").as_deref(),
                std::env::var_os("APPDIR").as_deref(),
                &layout().tray,
            )
        })
    })
}

/// Look at where this program runs from now, before anything can replace it.
pub fn note_start() {
    let _ = runs_from_installed();
}

/// The facts of this machine now.
pub fn facts() -> Facts {
    let l = layout();
    let status_text = tray::load_status(&tray::status_path(), SystemTime::now());
    Facts {
        supported: update::supported(),
        status_text,
        marker_version: marker::read(&l.marker).and_then(|m| m.version),
        own: env!("CARGO_PKG_VERSION").to_string(),
        from_installed: runs_from_installed(),
        cache: read_cache(),
        state: update::read_state(&l),
    }
}

/// The accident guard of `apply` and `apply_patch`: an installed agent that is newer than this
/// program is not put back, unless `go_back` (the person pressed "Go back to X"). Unknown and
/// equal versions pass. It is no security border: the person can always run an older setup.
pub fn guard(installed: Option<&str>, own: &str, go_back: bool) -> Result<(), SetupError> {
    if !go_back && installed_is_newer(installed, own) {
        return Err(SetupError::new(
            "older",
            format!(
                "The installed agent ({}) is newer than this program ({own}). Open the installed version instead.",
                installed.unwrap_or_default(),
            ),
        ));
    }
    Ok(())
}

/// [`guard`] for the installation on this machine.
pub fn older_guard(go_back: bool) -> Result<(), SetupError> {
    let f = facts();
    let installed = installed_version(f.marker_version.as_deref(), f.status_text.as_deref());
    guard(installed.as_deref(), &f.own, go_back)
}

/// What the window is told to start on: a word of `--section`.
static START: OnceLock<Option<&'static str>> = OnceLock::new();

/// The sections a command line may open.
pub const SECTIONS: [&str; 6] = [
    "overview",
    "printers",
    "connection",
    "advanced",
    "log",
    "updates",
];

pub fn set_start_section(section: Option<&'static str>) {
    remember(&START, section);
}

/// The first answer stays: a second call (a second window) changes nothing.
fn remember(cell: &OnceLock<Option<&'static str>>, section: Option<&'static str>) {
    let _ = cell.set(section);
}

fn recall(cell: &OnceLock<Option<&'static str>>) -> Option<&'static str> {
    cell.get().copied().flatten()
}

#[tauri::command(async)]
pub fn start_section() -> Option<&'static str> {
    recall(&START)
}

#[tauri::command(async)]
pub fn update_info() -> UpdateInfo {
    info(&facts())
}

/// A look at the machine and, when it is due, at the signed file; the cache is kept.
pub fn look(force: bool) -> UpdateInfo {
    let f = facts();
    let (info, cache) = check(
        &f,
        force,
        now_secs(),
        &update::keys(),
        &printpapi_agent::client::download,
    );
    if let Some(cache) = cache {
        write_cache(&cache);
    }
    info
}

#[tauri::command(async)]
pub fn check_update(force: bool) -> UpdateInfo {
    look(force)
}

/// The text of the tray menu item: "Update to 3.7.0…" while a newer version than the installed
/// (else this) one is known, else "Updates…".
pub fn menu_text(info: &UpdateInfo) -> String {
    let current = info.installed.as_deref().unwrap_or(&info.own);
    match (
        info.latest.as_deref().and_then(Version::parse),
        Version::parse(current),
    ) {
        (Some(latest), Some(current)) if latest > current => format!("Update to {latest}…"),
        _ => "Updates…".into(),
    }
}

/// The release folder to download from: only where the agent's own settings allow a look
/// (`notify` or `auto`, a valid folder). Off and unknown make no request, as the docs promise.
pub fn download_base(status_text: Option<&str>) -> Result<String, SetupError> {
    let (mode, base) = settings_of_status(status_text);
    match (mode.as_str(), base) {
        ("notify" | "auto", Some(base)) => Ok(base),
        // an update_url that is no folder makes the agent write "off" (status.rs), so this is
        // "off" too: the line names both causes
        ("off", _) => Err(SetupError::new("off", OFF_TEXT)),
        _ => Err(SetupError::new(
            "off",
            "The agent has not said how it is set up. Start it first.",
        )),
    }
}

/// The version the elevated agent compares with: the installed one, else this program's.
pub fn installed_for_download(f: &Facts) -> Version {
    installed_version(f.marker_version.as_deref(), f.status_text.as_deref())
        .and_then(|v| Version::parse(&v))
        .or_else(|| Version::parse(&f.own))
        .unwrap_or(Version(0, 0, 0))
}

#[tauri::command(async)]
pub fn download_update(state: Shared) -> Result<String, SetupError> {
    if !update::supported() {
        return Err(SetupError::new(
            "no_key",
            "This build has no update key, so it cannot check for updates.",
        ));
    }
    let f = facts();
    let base = download_base(f.status_text.as_deref())?;
    let dir = lock(&state).staging()?.dir().to_path_buf();
    let l = layout();
    let result = download_into(
        &dir,
        &base,
        Os::host(),
        update::tray_kind(Os::host(), &l.tray),
        installed_for_download(&f),
        &update::keys(),
        &printpapi_agent::client::download,
    );
    if result.is_err() {
        clean_staged(&dir);
    }
    result
}

#[tauri::command(async)]
pub fn install_update(state: Shared) -> Result<String, SetupError> {
    let dir = lock(&state).staging()?.dir().to_path_buf();
    let l = layout();
    let marker_version = marker::read(&l.marker).and_then(|m| m.version);
    install_in(&elevate::run, &l.binary, &dir, marker_version.as_deref())
}

/// `N.N.N` only: the version becomes part of an address.
pub fn notes_url(version: &str) -> Option<String> {
    Version::parse(version).map(|v| format!("{NOTES_URL}{v}"))
}

#[tauri::command(async)]
pub fn open_release_notes(version: String) -> Result<(), SetupError> {
    let url =
        notes_url(&version).ok_or_else(|| SetupError::new("system", "That is no version."))?;
    let cmd = tray::open_url_cmd(Os::host(), &printpapi_agent::install::system_dir(), &url)
        .ok_or_else(|| SetupError::new("system", "The release page cannot be opened."))?;
    printpapi_agent::proc::run_command(&cmd)
        .map(|_| ())
        .map_err(|e| SetupError::new("system", e))
}

/// The command that opens the installed program's window (not its icon).
pub fn window_start_cmd(os: Os, l: &Layout) -> printpapi_agent::proc::Cmd {
    let mut cmd = crate::settings::tray_start_cmd(os, l);
    // the icon is started with `--tray` (macOS: after `--args`); the window is the same without it
    cmd.argv.retain(|a| a != "--tray" && a != "--args");
    cmd
}

/// Start the installed version's window and end this one: after an update, the window reopens as
/// the new version.
#[tauri::command(async)]
pub fn open_installed(app: tauri::AppHandle) -> Result<(), SetupError> {
    let cmd = window_start_cmd(Os::host(), &layout());
    let Some((program, args)) = cmd.argv.split_first() else {
        return Err(SetupError::new(
            "system",
            "There is no installed program to open.",
        ));
    };
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            SetupError::new("system", format!("cannot open the installed version: {e}"))
        })?;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use printpapi_agent::update::sha256_hex;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::cell::RefCell;

    const SEED: &[u8; 32] = b"printpapi test seed, not secret!";

    fn public() -> [u8; 32] {
        Ed25519KeyPair::from_seed_unchecked(SEED)
            .unwrap()
            .public_key()
            .as_ref()
            .try_into()
            .unwrap()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The signed file and the files of a release for `os` with an agent and a tray.
    fn release(os: Os, version: &str) -> Vec<(String, Vec<u8>)> {
        let t = update::target(os);
        let (agent, tray) = (b"agent bytes".to_vec(), b"tray bytes".to_vec());
        let payload = format!(
            r#"{{"schema":1,"version":"{version}","targets":{{"{t}":{{"agent":{{"file":"agent-{t}","size":{},"sha256":"{}"}},"tray":{{"file":"tray-{t}","size":{},"sha256":"{}"}}}}}}}}"#,
            agent.len(),
            sha256_hex(&agent),
            tray.len(),
            sha256_hex(&tray)
        );
        let pair = Ed25519KeyPair::from_seed_unchecked(SEED).unwrap();
        let mut signed =
            format!("sig {}\n\n", hex(pair.sign(payload.as_bytes()).as_ref())).into_bytes();
        signed.extend_from_slice(payload.as_bytes());
        vec![
            (MANIFEST_NAME.to_string(), signed),
            (format!("agent-{t}"), agent),
            (format!("tray-{t}"), tray),
        ]
    }

    type Served = Vec<(String, Result<Vec<u8>, DownloadError>)>;

    /// A network that serves `files` under `base` and counts the requests.
    struct Net {
        files: RefCell<Served>,
        asked: RefCell<Vec<String>>,
    }

    impl Net {
        fn new(base: &str, files: &[(String, Vec<u8>)]) -> Net {
            Net {
                files: RefCell::new(
                    files
                        .iter()
                        .map(|(n, b)| (format!("{base}/{n}"), Ok(b.clone())))
                        .collect(),
                ),
                asked: RefCell::default(),
            }
        }
        fn get(&self, url: &str, _: u64, _: Duration) -> Result<Vec<u8>, DownloadError> {
            self.asked.borrow_mut().push(url.to_string());
            self.files
                .borrow()
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, r)| r.clone())
                .unwrap_or(Err(DownloadError::NotFound))
        }
    }

    fn status(extra: &str) -> String {
        format!(r#"{{"version":"3.6.0","state":"online"{extra}}}"#)
    }

    fn facts(status_text: Option<String>) -> Facts {
        Facts {
            supported: true,
            status_text,
            marker_version: Some("3.6.0".into()),
            own: "3.6.0".into(),
            from_installed: true,
            cache: None,
            state: None,
        }
    }

    const NOW: f64 = 1_800_000_000.0;

    #[test]
    fn the_mode_and_the_folder_come_from_the_status_file() {
        let f = |extra: &str| settings_of_status(Some(&status(extra)));
        assert_eq!(
            f(r#","updates":"notify","update_url":null"#),
            ("notify".into(), Some(DEFAULT_BASE.into()))
        );
        assert_eq!(
            f(r#","updates":"auto","update_url":"https://m.example/x""#),
            ("auto".into(), Some("https://m.example/x".into()))
        );
        assert_eq!(f(r#","updates":"off""#).0, "off");
        // an agent older than the fields, a word that is none of the three, no file at all
        assert_eq!(f("").0, "unknown");
        assert_eq!(f(r#","updates":"Auto""#).0, "unknown");
        assert_eq!(settings_of_status(None).0, "unknown");
        assert_eq!(settings_of_status(Some("not json")).0, "unknown");
        // a folder that is no folder is no base
        assert_eq!(f(r#","updates":"notify","update_url":"ftp://x/y""#).1, None);
        assert_eq!(f(r#","updates":"notify","update_url":7"#).1, None);
    }

    #[test]
    fn the_installed_version_is_the_marker_then_the_status_then_unknown() {
        assert_eq!(
            installed_version(Some("3.7.0"), Some(&status(""))),
            Some("3.7.0".into())
        );
        assert_eq!(
            installed_version(None, Some(&status(""))),
            Some("3.6.0".into())
        );
        // a marker without a usable version falls through to the status file
        assert_eq!(
            installed_version(Some("junk"), Some(&status(""))),
            Some("3.6.0".into())
        );
        assert_eq!(installed_version(None, None), None);
        assert_eq!(installed_version(None, Some(r#"{"version":"x"}"#)), None);
    }

    #[test]
    fn an_installed_agent_is_newer_only_when_both_versions_are_known_and_it_is_greater() {
        assert!(installed_is_newer(Some("3.8.0"), "3.7.0"));
        assert!(installed_is_newer(Some("3.10.0"), "3.9.9"));
        assert!(!installed_is_newer(Some("3.7.0"), "3.7.0"));
        assert!(!installed_is_newer(Some("3.6.9"), "3.7.0"));
        assert!(!installed_is_newer(None, "3.7.0"));
        assert!(!installed_is_newer(Some("junk"), "3.7.0"));
    }

    #[test]
    fn the_window_that_is_the_installed_program_is_known_by_the_tray_source_not_by_the_exe() {
        let installed = Path::new("/opt/printpapi-agent/printpapi-tray");
        let same = |os, exe: &str, appimage: Option<&str>, appdir: Option<&str>| {
            from_installed_at(
                os,
                Path::new(exe),
                appimage.map(OsStr::new),
                appdir.map(OsStr::new),
                installed,
            )
        };
        // an ELF, a copy elsewhere
        assert!(same(
            Os::Linux,
            "/opt/printpapi-agent/printpapi-tray",
            None,
            None
        ));
        assert!(!same(
            Os::Linux,
            "/home/u/Downloads/printpapi-setup",
            None,
            None
        ));
        // an AppImage is installed as the AppImage file: the exe is inside its mount
        assert!(same(
            Os::Linux,
            "/tmp/.mount_x/usr/bin/printpapi-setup",
            Some("/opt/printpapi-agent/printpapi-tray"),
            Some("/tmp/.mount_x"),
        ));
        assert!(!same(
            Os::Linux,
            "/tmp/.mount_x/usr/bin/printpapi-setup",
            Some("/home/u/Downloads/printpapi.AppImage"),
            Some("/tmp/.mount_x"),
        ));
        // $APPIMAGE of a parent AppImage proves nothing for a program outside its mount
        assert!(!same(
            Os::Linux,
            "/usr/bin/printpapi-setup",
            Some("/opt/printpapi-agent/printpapi-tray"),
            Some("/tmp/.mount_x"),
        ));
        // Windows names do not differ by case
        let win = Path::new("C:\\Program Files\\printpapi-agent\\printpapi-tray.exe");
        assert!(from_installed_at(
            Os::Windows,
            Path::new("c:\\program files\\printpapi-agent\\printpapi-tray.exe"),
            None,
            None,
            win
        ));
        // macOS: anything inside the installed app
        let app = Path::new("/Applications/printpapi-agent.app");
        assert!(from_installed_at(
            Os::Macos,
            Path::new("/Applications/printpapi-agent.app/Contents/MacOS/printpapi-setup"),
            None,
            None,
            app
        ));
        assert!(!from_installed_at(
            Os::Macos,
            Path::new("/Users/u/Downloads/printpapi.app/Contents/MacOS/printpapi-setup"),
            None,
            None,
            app
        ));
    }

    #[test]
    fn a_newer_installed_agent_is_not_put_back_by_accident_and_a_deliberate_go_back_passes() {
        let older = guard(Some("3.8.0"), "3.7.0", false).unwrap_err();
        assert_eq!(older.code, "older");
        assert!(older.message().contains("3.8.0") && older.message().contains("3.7.0"));
        // pressing Go back passes, and so do equal, older and unknown installed versions
        assert!(guard(Some("3.8.0"), "3.7.0", true).is_ok());
        for installed in [Some("3.7.0"), Some("3.6.9"), Some("junk"), None] {
            assert!(guard(installed, "3.7.0", false).is_ok(), "{installed:?}");
        }
    }

    #[test]
    fn the_cache_lasts_a_day_for_an_answer_and_a_404_and_an_hour_for_a_failure_and_only_for_its_base()
     {
        let c = |kind: &str, at: f64| Cache {
            checked_at: at,
            base: "b".into(),
            latest: None,
            kind: kind.into(),
            error: None,
            by: None,
        };
        for kind in ["ok", "none"] {
            assert!(cache_fresh(&c(kind, NOW), "b", NOW + 86_399.0), "{kind}");
            assert!(!cache_fresh(&c(kind, NOW), "b", NOW + 86_400.0), "{kind}");
        }
        assert!(cache_fresh(&c("error", NOW), "b", NOW + 3_599.0));
        assert!(!cache_fresh(&c("error", NOW), "b", NOW + 3_600.0));
        assert!(!cache_fresh(&c("ok", NOW), "other", NOW + 1.0));
        // a clock set back makes it stale, not eternal
        assert!(!cache_fresh(&c("ok", NOW), "b", NOW - 5.0));
    }

    #[test]
    fn nothing_is_asked_when_off_unknown_unsupported_or_the_folder_is_bad() {
        let net = Net::new(DEFAULT_BASE, &release(Os::Linux, "3.7.0"));
        let keys = [public()];
        let ask =
            |f: &Facts, keys: &[[u8; 32]]| check(f, true, NOW, keys, &|u, l, d| net.get(u, l, d));
        for text in [
            status(r#","updates":"off""#),
            status(""),
            status(r#","updates":"notify","update_url":"ftp://x""#),
        ] {
            let (info, cache) = ask(&facts(Some(text)), &keys);
            assert!(cache.is_none());
            assert_eq!(info.latest, None);
        }
        assert!(ask(&facts(None), &keys).1.is_none());
        let mut unsupported = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        unsupported.supported = false;
        assert!(ask(&unsupported, &keys).1.is_none());
        // and a build without a key never asks either
        assert!(
            ask(
                &facts(Some(status(r#","updates":"notify","update_url":null"#))),
                &[]
            )
            .1
            .is_none()
        );
        assert!(net.asked.borrow().is_empty());
    }

    #[test]
    fn a_look_asks_once_a_day_and_says_what_it_found() {
        let net = Net::new(DEFAULT_BASE, &release(Os::Linux, "3.7.0"));
        let keys = [public()];
        let mut f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        let (info, cache) = check(&f, false, NOW, &keys, &|u, l, d| net.get(u, l, d));
        let cache = cache.expect("a look was made");
        assert_eq!(info.latest.as_deref(), Some("3.7.0"));
        assert_eq!(info.checked_at, Some(NOW));
        assert_eq!(net.asked.borrow().len(), 1);
        // the next look within a day asks nothing; a forced one does
        f.cache = Some(cache);
        let (again, none) = check(&f, false, NOW + 3600.0, &keys, &|u, l, d| net.get(u, l, d));
        assert!(none.is_none());
        assert_eq!(again.latest.as_deref(), Some("3.7.0"));
        assert_eq!(net.asked.borrow().len(), 1);
        assert!(
            check(&f, true, NOW + 3600.0, &keys, &|u, l, d| net.get(u, l, d))
                .1
                .is_some()
        );
        assert_eq!(net.asked.borrow().len(), 2);
        // after a day it asks again
        assert!(
            check(&f, false, NOW + 90_000.0, &keys, &|u, l, d| net
                .get(u, l, d))
            .1
            .is_some()
        );
    }

    #[test]
    fn nothing_published_yet_is_cached_as_an_answer_and_a_network_error_as_a_failure() {
        let keys = [public()];
        let f = facts(Some(status(r#","updates":"auto","update_url":null"#)));
        let empty = Net::new(DEFAULT_BASE, &[]);
        let (info, cache) = check(&f, false, NOW, &keys, &|u, l, d| empty.get(u, l, d));
        let cache = cache.unwrap();
        assert_eq!(
            (cache.kind.as_str(), info.latest, info.error.as_deref()),
            (
                "none",
                None,
                Some("No signed update has been published yet.")
            )
        );
        let down = |_: &str, _: u64, _: Duration| {
            Err(DownloadError::Other("connection failed: refused".into()))
        };
        let (info, cache) = check(&f, false, NOW, &keys, &down);
        assert_eq!(cache.as_ref().unwrap().kind, "error");
        assert_eq!(
            info.error.as_deref(),
            Some("The update server could not be reached: connection failed: refused")
        );
        // the failure is old after an hour, the answer after a day
        assert!(!cache_fresh(
            cache.as_ref().unwrap(),
            DEFAULT_BASE,
            NOW + 3600.0
        ));
    }

    #[test]
    fn a_file_with_another_signature_is_an_error_and_no_version() {
        let net = Net::new(DEFAULT_BASE, &release(Os::Linux, "3.7.0"));
        let f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        let (info, cache) = check(&f, false, NOW, &[[9; 32]], &|u, l, d| net.get(u, l, d));
        assert_eq!(info.latest, None);
        assert!(
            info.error
                .unwrap()
                .contains("not signed with a key this program knows")
        );
        // a refused file is not a network failure: it does not come back every hour
        let cache = cache.unwrap();
        assert_eq!(cache.kind, "refused");
        assert!(cache_fresh(&cache, DEFAULT_BASE, NOW + 86_399.0));
        assert!(!cache_fresh(&cache, DEFAULT_BASE, NOW + 86_400.0));
    }

    #[test]
    fn a_refused_answer_is_not_kept_for_a_program_that_did_not_get_it() {
        // the new setup, installed by hand, knows the key the old icon refused
        let mut f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        f.own = "3.7.0".into();
        let refused = |by: Option<&str>| Cache {
            checked_at: NOW,
            base: DEFAULT_BASE.into(),
            latest: None,
            kind: "refused".into(),
            error: Some(
                "The update is not signed with a key this program knows. It was not installed."
                    .into(),
            ),
            by: by.map(Into::into),
        };
        // not shown, and the next look asks again
        for by in [Some("3.6.0"), None] {
            f.cache = Some(refused(by));
            assert_eq!(info(&f).error, None, "{by:?}");
            let net = Net::new(DEFAULT_BASE, &release(Os::Linux, "3.8.0"));
            let (i, cache) = check(&f, false, NOW + 60.0, &[public()], &|u, l, d| {
                net.get(u, l, d)
            });
            assert_eq!(i.latest.as_deref(), Some("3.8.0"), "{by:?}");
            assert_eq!(cache.unwrap().by.as_deref(), Some("3.7.0"));
        }
        // the program that got it keeps it for the day
        f.cache = Some(refused(Some("3.7.0")));
        assert!(info(&f).error.unwrap().contains("not signed"));
        let net = Net::new(DEFAULT_BASE, &release(Os::Linux, "3.8.0"));
        let (_, cache) = check(&f, false, NOW + 60.0, &[public()], &|u, l, d| {
            net.get(u, l, d)
        });
        assert!(cache.is_none() && net.asked.borrow().is_empty());
        // an answer that is no refusal is the same for every program
        let mut ok = refused(None);
        ok.kind = "ok".into();
        ok.latest = Some("3.8.0".into());
        f.cache = Some(ok);
        assert_eq!(info(&f).latest.as_deref(), Some("3.8.0"));
        // an old cache file (no `by`) still reads
        let old: Cache =
            serde_json::from_str(r#"{"checked_at":1.0,"base":"b","latest":null,"kind":"refused"}"#)
                .unwrap();
        assert_eq!(old.by, None);
    }

    #[test]
    fn a_newer_schema_and_a_bad_file_are_refused_for_a_day_and_only_the_network_for_an_hour() {
        let f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        let keys = [public()];
        let asked = |body: Vec<u8>| {
            let net = Net::new(DEFAULT_BASE, &[(MANIFEST_NAME.to_string(), body)]);
            check(&f, false, NOW, &keys, &|u, l, d| net.get(u, l, d))
        };
        // schema 2 under a good signature: this program cannot read it
        let pair = Ed25519KeyPair::from_seed_unchecked(SEED).unwrap();
        let payload = r#"{"schema":2,"version":"9.0.0","targets":{}}"#;
        let mut signed =
            format!("sig {}\n\n", hex(pair.sign(payload.as_bytes()).as_ref())).into_bytes();
        signed.extend_from_slice(payload.as_bytes());
        let (info, cache) = asked(signed);
        assert_eq!(cache.unwrap().kind, "refused");
        assert!(info.error.unwrap().contains("setup program"));
        let (_, cache) = asked(b"not a signed file".to_vec());
        assert_eq!(cache.unwrap().kind, "refused");
        let down = |_: &str, _: u64, _: Duration| Err(DownloadError::Other("refused".into()));
        let (_, cache) = check(&f, false, NOW, &keys, &down);
        assert_eq!(cache.unwrap().kind, "error");
    }

    fn cached(kind: &str, latest: Option<&str>, base: &str) -> Option<Cache> {
        Some(Cache {
            checked_at: NOW,
            base: base.into(),
            latest: latest.map(Into::into),
            kind: kind.into(),
            error: None,
            by: None,
        })
    }

    #[test]
    fn an_answer_is_shown_only_while_the_mode_looks_and_for_the_folder_it_was_made_for() {
        let with = |extra: &str| {
            let mut f = facts(Some(status(extra)));
            f.cache = cached("ok", Some("3.8.0"), DEFAULT_BASE);
            f
        };
        let latest = |f: &Facts| info(f).latest;
        assert_eq!(
            latest(&with(r#","updates":"notify","update_url":null"#)),
            Some("3.8.0".into())
        );
        // off, unknown, and a changed mirror: the old answer is no notice any more
        assert_eq!(latest(&with(r#","updates":"off","update_url":null"#)), None);
        assert_eq!(latest(&with("")), None);
        assert_eq!(
            latest(&with(
                r#","updates":"auto","update_url":"https://m.example/x""#
            )),
            None
        );
        assert_eq!(info(&with(r#","updates":"off""#)).checked_at, None);
        // and the tray says nothing of it
        assert_eq!(
            menu_text(&info(&with(r#","updates":"off","update_url":null"#))),
            "Updates…"
        );
        assert_eq!(
            menu_text(&info(&with(r#","updates":"auto","update_url":null"#))),
            "Update to 3.8.0…"
        );
    }

    #[test]
    fn a_404_is_said_not_published_and_a_folder_that_is_none_asks_nothing() {
        let mut f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        f.cache = cached("none", None, DEFAULT_BASE);
        let i = info(&f);
        assert_eq!(
            i.error.as_deref(),
            Some("No signed update has been published yet.")
        );
        assert_eq!(i.latest, None);
        // a status file with a folder that is none (no agent writes one with a mode that looks):
        // nothing is shown and nothing is asked
        let bad = facts(Some(status(r#","updates":"notify","update_url":7"#)));
        assert_eq!(info(&bad).error, None);
        let net = Net::new(DEFAULT_BASE, &release(Os::Linux, "3.7.0"));
        let (_, cache) = check(&bad, true, NOW, &[public()], &|u, l, d| net.get(u, l, d));
        assert!(cache.is_none() && net.asked.borrow().is_empty());
        // off stays "off", whatever the folder
        let off = facts(Some(status(r#","updates":"off","update_url":7"#)));
        assert_eq!(info(&off).error, None);
        assert_eq!(info(&off).mode, "off");
    }

    #[test]
    fn a_download_asks_only_where_the_agent_allows_a_look() {
        let base = |extra: &str| download_base(Some(&status(extra)));
        assert_eq!(
            base(r#","updates":"auto","update_url":"https://m.example/x""#).unwrap(),
            "https://m.example/x"
        );
        assert_eq!(
            base(r#","updates":"notify","update_url":null"#).unwrap(),
            DEFAULT_BASE
        );
        for refused in [base(r#","updates":"off","update_url":null"#), base("")] {
            assert_eq!(refused.unwrap_err().code, "off");
        }
        assert_eq!(download_base(None).unwrap_err().code, "off");
        // an update_url the agent cannot use is written as "off": the sentence names that cause
        assert!(
            base(r#","updates":"off","update_url":null"#)
                .unwrap_err()
                .message()
                .contains("update_url")
        );
        assert_eq!(
            base(r#","updates":"notify","update_url":7"#)
                .unwrap_err()
                .code,
            "off"
        );
    }

    #[test]
    fn the_download_compares_with_the_installed_agent_not_with_a_newer_window() {
        let mut f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        f.marker_version = Some("3.7.0".into());
        f.own = "3.8.0".into();
        assert_eq!(installed_for_download(&f), Version(3, 7, 0));
        // 3.8.0 is newer than the installed 3.7.0, so it is downloaded
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new("https://m.example/p", &release(Os::Linux, "3.8.0"));
        assert!(
            download_into(
                dir.path(),
                "https://m.example/p",
                Os::Linux,
                TrayKind::Program,
                installed_for_download(&f),
                &[public()],
                &|u, l, d| net.get(u, l, d),
            )
            .is_ok()
        );
        // nothing known of the installed agent: this program's version
        f.marker_version = None;
        f.status_text = None;
        assert_eq!(installed_for_download(&f), Version(3, 8, 0));
    }

    #[test]
    fn the_info_shows_the_last_attempt_and_the_cache_file_round_trips() {
        let mut f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        f.state = Some(State {
            version: "3.7.0".into(),
            from: "3.6.0".into(),
            at: 5.0,
            ok: false,
            tries: 1,
            message: "put back".into(),
        });
        let info = info(&f);
        assert_eq!(info.last_attempt.as_ref().unwrap().message, "put back");
        assert_eq!(info.mode, "notify");
        let json = serde_json::to_value(&info).unwrap();
        for key in [
            "supported",
            "mode",
            "own",
            "installed",
            "fromInstalled",
            "latest",
            "checkedAt",
            "lastAttempt",
            "error",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        let c = Cache {
            checked_at: 1.0,
            base: "b".into(),
            latest: Some("3.7.0".into()),
            kind: "ok".into(),
            error: None,
            by: None,
        };
        let text = serde_json::to_string(&c).unwrap();
        assert!(!text.contains("error"), "{text}");
        assert_eq!(serde_json::from_str::<Cache>(&text).unwrap(), c);
    }

    #[test]
    fn the_download_stages_the_signed_file_and_the_files_it_names_checked_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new("https://m.example/p", &release(Os::Linux, "3.7.0"));
        let v = download_into(
            dir.path(),
            "https://m.example/p",
            Os::Linux,
            TrayKind::Program,
            Version(3, 6, 0),
            &[public()],
            &|u, l, d| net.get(u, l, d),
        )
        .unwrap();
        assert_eq!(v, "3.7.0");
        let t = update::target(Os::Linux);
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                format!("agent-{t}"),
                MANIFEST_NAME.to_string(),
                format!("tray-{t}")
            ]
        );
        assert_eq!(
            std::fs::read(dir.path().join(format!("agent-{t}"))).unwrap(),
            b"agent bytes"
        );
    }

    #[test]
    fn a_download_that_is_not_what_the_release_lists_is_refused() {
        let t = update::target(Os::Linux);
        let try_with =
            |files: Vec<(String, Vec<u8>)>, own: Version, kind: TrayKind, keys: &[[u8; 32]]| {
                let dir = tempfile::tempdir().unwrap();
                let net = Net::new("https://m.example/p", &files);
                download_into(
                    dir.path(),
                    "https://m.example/p",
                    Os::Linux,
                    kind,
                    own,
                    keys,
                    &|u, l, d| net.get(u, l, d),
                )
                .unwrap_err()
            };
        let good = release(Os::Linux, "3.7.0");
        let mut wrong = good.clone();
        wrong[1].1 = b"agent bytez".to_vec();
        assert_eq!(
            try_with(wrong, Version(3, 6, 0), TrayKind::Program, &[public()]).code,
            "bad_file"
        );
        let mut gone = good.clone();
        gone.remove(2);
        assert_eq!(
            try_with(gone, Version(3, 6, 0), TrayKind::Program, &[public()]).code,
            "bad_file"
        );
        assert_eq!(
            try_with(vec![], Version(3, 6, 0), TrayKind::Program, &[public()]).code,
            "not_published"
        );
        assert_eq!(
            try_with(
                good.clone(),
                Version(3, 7, 0),
                TrayKind::Program,
                &[public()]
            )
            .code,
            "not_newer"
        );
        assert_eq!(
            try_with(
                good.clone(),
                Version(3, 6, 0),
                TrayKind::Program,
                &[[9; 32]]
            )
            .code,
            "bad_signature"
        );
        // no AppImage in this release
        assert_eq!(
            try_with(good, Version(3, 6, 0), TrayKind::AppImage, &[public()]).code,
            "no_target"
        );
        let _ = t;
    }

    fn elevator<'a>(
        asked: &'a RefCell<Vec<Vec<String>>>,
        result: &'a str,
        outcome: Result<(), Elevate>,
    ) -> impl Fn(&Path, &[&OsStr]) -> Result<(), Elevate> + 'a {
        move |agent, args| {
            let mut line = vec![agent.to_string_lossy().into_owned()];
            line.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
            let dir = Path::new(args[2]);
            if !result.is_empty() {
                std::fs::write(dir.join("result.json"), result).unwrap();
            }
            asked.borrow_mut().push(line);
            outcome.clone()
        }
    }

    fn staged_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new("b", &release(Os::Linux, "3.7.0"));
        download_into(
            dir.path(),
            "b",
            Os::Linux,
            TrayKind::Program,
            Version(3, 6, 0),
            &[public()],
            &|u, l, d| net.get(u, l, d),
        )
        .unwrap();
        std::fs::write(dir.path().join("foreign.txt"), b"x").unwrap();
        dir
    }

    fn left(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_install_elevates_the_installed_agent_with_the_three_word_command_and_reads_its_answer() {
        let dir = staged_dir();
        let asked = RefCell::default();
        let e = elevator(&asked, r#"{"ok":true,"version":"3.7.0"}"#, Ok(()));
        let v = install_in(
            &e,
            Path::new("/opt/printpapi-agent/printpapi-agent"),
            dir.path(),
            Some("3.6.0"),
        )
        .unwrap();
        assert_eq!(v, "3.7.0");
        assert_eq!(
            asked.borrow()[0],
            [
                "/opt/printpapi-agent/printpapi-agent",
                "restart",
                "--update-from",
                &dir.path().to_string_lossy()
            ]
        );
        // the staged files are gone, what was not staged stays
        assert_eq!(left(dir.path()), ["foreign.txt", "result.json"]);
    }

    #[test]
    fn staged_files_are_deleted_after_a_failure_and_after_a_cancelled_prompt() {
        for (result, outcome, code) in [
            (
                r#"{"ok":false,"code":"rolled_back","message":"Version 3.7.0 did not connect, so 3.6.0 was put back. It is running again."}"#,
                Ok(()),
                "rolled_back",
            ),
            ("", Err(Elevate::Cancelled), "cancelled"),
            (
                r#"{"ok":false,"code":"bad_signature","message":"x"}"#,
                Ok(()),
                "bad_signature",
            ),
        ] {
            let dir = staged_dir();
            let asked = RefCell::default();
            let e = elevator(&asked, result, outcome);
            let err = install_in(&e, Path::new("/a"), dir.path(), Some("3.6.0")).unwrap_err();
            assert_eq!(err.code, code);
            assert!(
                !left(dir.path()).iter().any(|n| n.starts_with("agent-")
                    || n.starts_with("tray-")
                    || n == MANIFEST_NAME),
                "{code}"
            );
        }
    }

    #[test]
    fn an_installation_without_a_version_in_its_marker_is_not_updated_this_way() {
        for marker in [None, Some("")] {
            let dir = staged_dir();
            let asked = RefCell::default();
            let e = elevator(&asked, "", Ok(()));
            let err = install_in(&e, Path::new("/a"), dir.path(), marker).unwrap_err();
            assert_eq!(err.code, "no_version");
            assert!(
                asked.borrow().is_empty(),
                "no prompt for an installation this cannot update"
            );
            assert!(
                !left(dir.path()).contains(&MANIFEST_NAME.to_string()),
                "deleted all the same"
            );
        }
    }

    #[test]
    fn only_what_the_signed_file_names_is_deleted() {
        let dir = staged_dir();
        std::fs::write(dir.path().join("agent.ini"), b"key").unwrap();
        clean_staged(dir.path());
        assert_eq!(left(dir.path()), ["agent.ini", "foreign.txt"]);
        // a signed file that names a path is not followed
        std::fs::write(
            dir.path().join(MANIFEST_NAME),
            "sig x\n\n{\"targets\":{\"a\":{\"agent\":{\"file\":\"../agent.ini\"}}}}",
        )
        .unwrap();
        clean_staged(dir.path());
        assert_eq!(left(dir.path()), ["agent.ini", "foreign.txt"]);
    }

    #[test]
    fn the_tray_item_names_the_version_that_waits() {
        let mut f = facts(Some(status(r#","updates":"notify","update_url":null"#)));
        assert_eq!(menu_text(&info(&f)), "Updates…");
        f.cache = Some(Cache {
            checked_at: NOW,
            base: DEFAULT_BASE.into(),
            latest: Some("3.7.0".into()),
            kind: "ok".into(),
            error: None,
            by: None,
        });
        assert_eq!(menu_text(&info(&f)), "Update to 3.7.0…");
        f.marker_version = Some("3.7.0".into());
        assert_eq!(menu_text(&info(&f)), "Updates…");
        f.marker_version = Some("3.10.0".into());
        assert_eq!(menu_text(&info(&f)), "Updates…");
    }

    #[test]
    fn release_notes_open_only_for_a_plain_version() {
        assert_eq!(
            notes_url("3.7.0").as_deref(),
            Some("https://github.com/LukiPrince/printpapi/releases/tag/v3.7.0")
        );
        for bad in ["", "3.7", "3.7.0/../x", "3.7.0?x=1", "v3.7.0", "3.7.0 "] {
            assert_eq!(notes_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_window_is_started_like_the_icon_without_its_argument() {
        for os in [Os::Windows, Os::Linux, Os::Macos] {
            let l = printpapi_agent::install::layout(os, Path::new("C:\\Program Files"));
            let window = window_start_cmd(os, &l);
            assert!(
                !window.argv.iter().any(|a| a == "--tray" || a == "--args"),
                "{os:?}"
            );
            assert!(
                window.argv.iter().any(|a| a.contains("printpapi")),
                "{os:?}"
            );
        }
        let mac = window_start_cmd(
            Os::Macos,
            &printpapi_agent::install::layout(Os::Macos, Path::new("/")),
        );
        assert_eq!(mac.argv[..3], ["/usr/bin/open", "-n", "-a"]);
    }

    #[test]
    fn the_section_of_the_command_line_is_remembered_once() {
        // the words the nav of the window has
        assert!(SECTIONS.contains(&"updates") && SECTIONS.contains(&"overview"));
        assert!(!SECTIONS.contains(&"remove") && !SECTIONS.contains(&"restart"));
        let cell = OnceLock::new();
        assert_eq!(recall(&cell), None, "nothing asked for");
        remember(&cell, Some("updates"));
        assert_eq!(recall(&cell), Some("updates"));
        remember(&cell, Some("log"));
        remember(&cell, None);
        assert_eq!(recall(&cell), Some("updates"), "the first one stays");
        let none = OnceLock::new();
        remember(&none, None);
        assert_eq!(recall(&none), None);
    }
}
