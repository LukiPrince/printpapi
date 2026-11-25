// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The headless modes the .msi runs as LocalSystem (docs/agent.md, "Managed Windows install"):
//! `--managed-install <ini or folder>`, `--managed-refresh` and `--managed-uninstall`. They stage the
//! same files the window stages and run the agent's own admin subcommands, so the agent crate has no
//! new behaviour of its own. There is no window and no message box (session 0 has no desktop); the
//! exit code is 0 for ok, 1 for failed (msiexec reports 1603) and 2 for a wrong call. A failure leaves
//! one key-free line in `printpapi-managed.log` in the private folder of the account that ran it.
//!
//! The logic runs against a [`Host`], so the tests on every system use a fake machine; only the
//! entry [`run`] is Windows-only, other systems print the usage and exit 2.
// Everything behind `run` is only reached on Windows; the tests still build and run it everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use printpapi_agent::apply::{check_install_dir, is_link, trusted_windows_owner};
use printpapi_agent::config::{Config, IniUpdate, decode_ini, update_ini};
use printpapi_agent::install::{self, Installed, Layout};
use printpapi_agent::marker;
use printpapi_agent::setup::default_device_name;
use printpapi_agent::update::{StatusSeen, read_status_file, still_printing};

use crate::commands::{SetupError, run_elevated};
use crate::elevate::Elevate;
use crate::staging::{Staging, write_private};

/// Empty file next to the running program: this machine's agent was put there by the .msi, so the
/// .msi may refresh and remove it. A person who removes the agent with the window and installs it
/// again by hand, then removes the .msi, loses that agent (docs/agent.md says so).
pub const FLAG: &str = ".managed-install";
/// The failure log, in the private folder of the account that ran the mode.
pub const LOG: &str = "printpapi-managed.log";
/// A prepared ini is a few hundred bytes; more than this is not one.
const MAX_INI: u64 = 256 * 1024;

const USAGE: &str = "usage: printpapi-setup --managed-install <absolute path of an agent.ini, or of a folder with <COMPUTERNAME>.ini> | --managed-refresh | --managed-uninstall (Windows only, run by the .msi)";
const FOREIGN: &str = "An agent that was not installed by the setup is already here. Remove it first (docs/agent.md).";
const PRINTING: &str = "The agent is printing a job. The install waited two minutes for it and stopped, so that the job is not printed twice: try again when the queue is quiet.";
/// The removal never stops for a print (see `uninstall`), so it says the opposite of `PRINTING`.
const PRINTING_REMOVED: &str = "The agent was still printing after two minutes. It is removed anyway, so the job may be printed again.";
/// Debug builds only (`cargo test`): the folder of the log, so that a test never writes to the
/// log of the person who runs it. A release program does not read it.
#[cfg(debug_assertions)]
const LOG_DIR_ENV: &str = "PRINTPAPI_MANAGED_LOG_DIR";
/// How long a refresh or an install over a running agent waits for a print to end (the same two
/// minutes the update of the window waits, `update::QUIET_WAIT_SECS`).
const QUIET_WAIT_SECS: u64 = 120;
const KEY_IN_USE: &str = "The key is already used by another PC: one key per PC";
/// An upgrade with `AGENT_INI` of a PC that a pairing code reconnected: its agent.ini holds the new
/// key, and the prepared file's old key meets the name of the same device.
const NAME_TAKEN_UPGRADE: &str = "If this PC was reconnected with a pairing code (Reconnect), upgrade without AGENT_INI, or take the key out with unprotect-key (docs/agent.md).";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Install(PathBuf),
    Refresh,
    Uninstall,
}

impl Command {
    fn word(&self) -> &'static str {
        match self {
            Command::Install(_) => "managed-install",
            Command::Refresh => "managed-refresh",
            Command::Uninstall => "managed-uninstall",
        }
    }
}

/// A `--managed-*` call that is not one of the three forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage;

/// `None` when no argument starts with `--managed-`: this is not a managed call at all.
pub fn parse(args: &[OsString]) -> Option<Result<Command, Usage>> {
    let managed = |a: &OsString| a.to_string_lossy().starts_with("--managed-");
    if !args.iter().any(managed) {
        return None;
    }
    let word = |a: &OsString| a.to_str().map(str::to_owned).unwrap_or_default();
    Some(match args {
        [a] if word(a) == "--managed-refresh" => Ok(Command::Refresh),
        [a] if word(a) == "--managed-uninstall" => Ok(Command::Uninstall),
        [a, path] if word(a) == "--managed-install" => {
            let path = PathBuf::from(cut_quote(&path.to_string_lossy()));
            if path.is_absolute() {
                Ok(Command::Install(path))
            } else {
                Err(Usage)
            }
        }
        _ => Err(Usage),
    })
}

/// `"C:\folder\"` on a command line ends in an escaped quote: Windows hands the path over with that
/// quote still on it, and the backslash is gone. No Windows path holds a quote, so it is cut off. A
/// drive root (`"D:\"` arrives as `D:"`) would be left as `D:`, the current folder of that drive, so
/// its backslash is put back.
fn cut_quote(text: &str) -> String {
    let cut = text.trim_end_matches('"');
    if cut.len() != text.len() && cut.ends_with(':') {
        format!("{cut}\\")
    } else {
        cut.to_string()
    }
}

/// What the modes need of the machine.
pub trait Host {
    fn layout(&self) -> Layout;
    fn computer_name(&self) -> Option<String>;
    /// The folder of the running program.
    fn own_dir(&self) -> io::Result<PathBuf>;
    fn program_files(&self) -> PathBuf;
    /// The owner of a file as an `S-1-5-...` text.
    fn owner_sid(&self, path: &Path) -> io::Result<String>;
    fn detect(&self) -> Installed;
    fn staging(&self) -> io::Result<Staging>;
    /// Put the tray program in the staging folder (`commands::stage_own_tray`).
    fn stage_tray(&self, dir: &Path) -> Result<bool, SetupError>;
    /// Run the agent with its subcommand. LocalSystem is administrator already: no prompt.
    fn elevate(&self, agent: &Path, args: &[&OsStr]) -> Result<(), Elevate>;
    /// Where the failure log goes; `None` when there is no such place.
    fn log_dir(&self) -> Option<PathBuf>;
    fn default_name(&self) -> String;
    /// The brand id of this program (`None`: the stock build).
    fn brand_id(&self) -> Option<String>;
    /// The status file of the agent as it is now (`None`: no file, or not a regular file).
    fn status(&self) -> Option<StatusSeen>;
    /// Seconds since 1970.
    fn now(&self) -> f64;
    fn pause(&self, secs: u64);
}

/// Run one mode. `Err` is the one key-free line that was also written to the log.
pub fn execute(host: &dyn Host, command: &Command) -> Result<(), String> {
    let result = guard(host).and_then(|()| match command {
        Command::Install(path) => install(host, path),
        Command::Refresh => refresh(host),
        Command::Uninstall => uninstall(host),
    });
    if let Err(line) = &result {
        log(host, command, line);
    }
    result
}

/// The exe that SYSTEM runs must sit in a folder that only administrators can write to.
fn guard(host: &dyn Host) -> Result<(), String> {
    let own = host
        .own_dir()
        .map_err(|e| format!("cannot find this program: {e}"))?;
    if under(&own, &host.program_files()) {
        Ok(())
    } else {
        Err(format!(
            "This program runs from {}, not from the Program Files folder, so it does not run as an installer step.",
            own.display()
        ))
    }
}

/// Wait until the agent does not print: two looks one second apart that both find it quiet. The
/// install stops the agent, and a job that is stopped between the spooler and the report stays
/// `claimed` and is printed again by the server 300 s later (a label cut off, then a whole one). An
/// agent that is stopped, offline or has a stale status file does not hold anything up
/// (`update::still_printing`). After two minutes the call fails and the deployment tool tries again.
/// An agent that is quiet for the first time at the two-minute look gets its second look: it is not
/// printing any more, and the call must not say otherwise.
fn wait_quiet(host: &dyn Host) -> Result<(), String> {
    let mut quiet = 0;
    for waited in 0..=QUIET_WAIT_SECS + 1 {
        if still_printing(host.status().as_ref(), host.now()) {
            quiet = 0;
            if waited >= QUIET_WAIT_SECS {
                return Err(PRINTING.into());
            }
        } else {
            quiet += 1;
            if quiet == 2 {
                return Ok(());
            }
        }
        host.pause(1);
    }
    Err(PRINTING.into())
}

/// `path` is `base` or inside it. Windows compares names without regard to case.
fn under(path: &Path, base: &Path) -> bool {
    let fold = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (path, base) = (fold(path), fold(base));
    !base.is_empty() && path.starts_with(&base)
}

fn is_unc(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with(r"\\?\UNC\")
        || (text.starts_with(r"\\") && !text.starts_with(r"\\?\") && !text.starts_with(r"\\.\"))
}

/// The ini to use: the file itself, or `<COMPUTERNAME>.ini` in the folder. Not a link, a regular
/// file of at most 256 KiB, and (on this computer, not on a share) owned by SYSTEM or
/// Administrators, and so is the folder that holds it: a file that a user could have swapped is not
/// used, and neither is one in a folder that a user made first (a standard user may create folders
/// in C:\ProgramData, and keeps the owner's rights on it whatever icacls /grant:r says).
// ponytail: the owner checks do not see an explicit entry for a user on a folder that an
// administrator took over. docs/agent.md does not repair such a folder (`/setowner`, `/reset /T`
// follow a junction in it and miss a handle opened before): it puts the folder in
// `%SystemRoot%`, where a standard user cannot make one, and a folder that somebody else made is
// refused here by its owner. A full ACL check would be a parser of its own.
fn resolve(host: &dyn Host, arg: &Path) -> Result<PathBuf, String> {
    let look = |p: &Path| {
        std::fs::symlink_metadata(p).map_err(|e| format!("cannot look at {}: {e}", p.display()))
    };
    let link = |p: &Path| {
        format!(
            "{} is a link, and the setup does not follow it.",
            p.display()
        )
    };
    let meta = look(arg)?;
    if is_link(&meta) {
        return Err(link(arg));
    }
    let file = if meta.is_dir() {
        refuse_root(arg)?;
        let name = host
            .computer_name()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .ok_or("The name of this computer is not known, so its file in the folder cannot be found.")?;
        if name.contains("..")
            || name
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        {
            return Err("The name of this computer cannot be used as a file name.".into());
        }
        let file = arg.join(format!("{name}.ini"));
        let found = std::fs::symlink_metadata(&file)
            .map_err(|_| format!("There is no {name}.ini in {}.", arg.display()))?;
        if is_link(&found) {
            return Err(link(&file));
        }
        if !found.is_file() {
            return Err(format!("{} is not a file.", file.display()));
        }
        file
    } else if meta.is_file() {
        if let Some(folder) = arg.parent() {
            refuse_root(folder)?;
        }
        arg.to_path_buf()
    } else {
        return Err(format!("{} is not a file or a folder.", arg.display()));
    };
    if look(&file)?.len() > MAX_INI {
        return Err(format!(
            "{} is larger than 256 KiB: not an agent.ini.",
            file.display()
        ));
    }
    if !is_unc(&file) {
        let owner = |p: &Path| {
            host.owner_sid(p)
                .map_err(|e| format!("cannot read the owner of {}: {e}", p.display()))
        };
        // the folder first: a folder that somebody made is the cause, and the advice for a file
        // (below) must not be the first thing an administrator reads about it
        if let Some(folder) = file.parent() {
            let meta = look(folder)?;
            if is_link(&meta) {
                return Err(link(folder));
            }
            if !trusted_windows_owner(&owner(folder)?) {
                return Err(format!(
                    "The folder {} is not owned by SYSTEM or Administrators, so a file in it is not used. Use a folder that an administrator made for this, in a place that standard users cannot write to (docs/agent.md).",
                    folder.display()
                ));
            }
        }
        if !trusted_windows_owner(&owner(&file)?) {
            // not /setowner: it leaves the access list, so whoever made the file would still read
            // the key and edit the file, and this check would pass
            return Err(format!(
                "{} is not owned by SYSTEM or Administrators, so it is not used. Delete it and write it again from an elevated session (as administrator) into the protected folder (docs/agent.md).",
                file.display()
            ));
        }
    }
    Ok(file)
}

/// A drive root is no place for the file: the owner of `C:\` is TrustedInstaller and of `S:\` is
/// SYSTEM, and the default rights of a root give every user the right to change the files in it.
/// (A share's root is not a drive: the file on a share is not asked for its owner.)
fn refuse_root(folder: &Path) -> Result<(), String> {
    if folder.parent().is_none() && !is_unc(folder) {
        return Err(format!(
            "{} is a drive root. Put the file in a folder of its own that an administrator made and protected (docs/agent.md).",
            folder.display()
        ));
    }
    Ok(())
}

/// The text of a prepared ini: UTF-16 with a byte order mark (what PowerShell 5 writes by default)
/// or what `decode_ini` reads.
fn decode_prepared(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], word: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| word(*c)).collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => decode_ini(bytes),
    }
}

fn read_prepared(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_INI + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_INI {
        return Err(format!(
            "{} is larger than 256 KiB: not an agent.ini.",
            path.display()
        ));
    }
    Ok(decode_prepared(&bytes))
}

/// The `name` of the agent.ini that is installed: read here, as SYSTEM, after the install folder
/// passed its checks (`read-config` hands out the headers only). `None` without an installed file.
fn installed_name(l: &Layout) -> Result<Option<String>, String> {
    check_install_dir(l).map_err(|e| e.to_string())?;
    let text = match std::fs::read(&l.ini) {
        Ok(bytes) => decode_ini(&bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read the installed agent.ini: {e}")),
    };
    // the parser's own complaint quotes a line, and that can be the key line
    let cfg = Config::from_text(&text)
        .map_err(|_| "The installed agent.ini cannot be read.".to_string())?;
    Ok(cfg
        .get("name")
        .ok()
        .flatten()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty()))
}

/// A line for the log: no key, no line break.
fn scrub(text: &str, key: Option<&str>) -> String {
    let text = match key.filter(|k| !k.is_empty()) {
        Some(k) => text.replace(k, "***"),
        None => text.to_string(),
    };
    text.replace(['\r', '\n'], " ")
}

/// What the agent's `result.json` said, as one line.
fn describe(e: &SetupError, key: Option<&str>) -> String {
    if e.code == "key_in_use" {
        KEY_IN_USE.to_string()
    } else {
        scrub(e.message(), key)
    }
}

fn install(host: &dyn Host, arg: &Path) -> Result<(), String> {
    let l = host.layout();
    let ini_path = resolve(host, arg)?;
    let text = read_prepared(&ini_path)?;
    let cfg = Config::from_text(&text)
        .map_err(|_| "The prepared agent.ini cannot be read.".to_string())?;
    let key = cfg.get("api_key").ok().flatten();
    let key = key.as_deref();
    let before = host.detect();
    if before == Installed::Foreign {
        return Err(FOREIGN.into());
    }
    // A renamed PC must not register under a new name and meet 409: without a name in the file,
    // the installed file's name stays, and a fresh machine gets its computer's name.
    let named = cfg
        .get("name")
        .ok()
        .flatten()
        .is_some_and(|n| !n.trim().is_empty());
    let name = if named {
        None
    } else if before == Installed::Managed {
        Some(installed_name(&l)?.unwrap_or_else(|| host.default_name()))
    } else {
        Some(host.default_name())
    };
    // The deployment tool owns the versions of a managed machine, unless the file says otherwise.
    let updates = cfg.get("updates").ok().flatten().is_none().then_some("off");
    let text = if name.is_some() || updates.is_some() {
        update_ini(
            Some(&text),
            &IniUpdate {
                server_url: None,
                api_key: None,
                name: name.as_deref(),
                printers: None,
                timeout: None,
                headers: None,
                updates,
            },
        )
    } else {
        text
    };

    let staging = host
        .staging()
        .map_err(|e| format!("cannot make the private working folder: {e}"))?;
    let dir = staging.dir().to_path_buf();
    let agent = staging
        .agent()
        .map_err(|e| format!("cannot unpack the agent: {e}"))?;
    let staged_ini = write_private(&dir, "agent.ini", text.as_bytes())
        .map_err(|e| format!("cannot write the settings: {e}"))?;
    let ran = stage_sumatra(&ini_path, &dir)
        .and_then(|()| host.stage_tray(&dir).map_err(|e| e.message().to_string()))
        .and_then(|_| {
            // as late as this program can: everything it does itself is done before the look. What
            // is left before the stop is the agent's own install, which registers with the server
            // first (a second or two, more on a slow connection): a job claimed meanwhile is cut
            // off by the stop and printed again
            if before == Installed::Managed {
                wait_quiet(host)?;
            }
            run_elevated(&|a, args| host.elevate(a, args), &agent, &dir, "install")
                .map(|_| ())
                .map_err(|e| {
                    if e.code == "name_taken" && before == Installed::Managed {
                        // a 401 has several causes (a revoked or exchanged key, a name of another
                        // key, a Print only key): the agent's sentence stays, Reconnect is one more
                        format!("{} {NAME_TAKEN_UPGRADE}", describe(&e, key))
                    } else {
                        describe(&e, key)
                    }
                })
        });
    // the file holds the key: gone whatever happened
    let _ = std::fs::remove_file(&staged_ini);
    let ran = ran.and_then(|()| write_flag(host));
    if let Err(line) = ran {
        // A first install that failed late leaves an agent and a boot task that nobody owns, and
        // the .msi's rollback removes only the setup program.
        if before == Installed::None && l.marker.exists() {
            let _ = run_elevated(&|a, args| host.elevate(a, args), &agent, &dir, "uninstall");
        }
        return Err(scrub(&line, key));
    }
    Ok(())
}

/// `SumatraPDF.exe` next to the ini goes along (the agent accepts only the pinned build).
fn stage_sumatra(ini: &Path, dir: &Path) -> Result<(), String> {
    let Some(side) = ini.parent().map(|p| p.join("SumatraPDF.exe")) else {
        return Ok(());
    };
    match std::fs::symlink_metadata(&side) {
        Ok(meta) if meta.is_file() && !is_link(&meta) => {
            std::fs::copy(&side, dir.join("SumatraPDF.exe"))
                .map(|_| ())
                .map_err(|e| format!("cannot copy {}: {e}", side.display()))
        }
        _ => Ok(()),
    }
}

fn write_flag(host: &dyn Host) -> Result<(), String> {
    let dir = host
        .own_dir()
        .map_err(|e| format!("cannot find this program: {e}"))?;
    std::fs::write(dir.join(FLAG), b"")
        .map_err(|e| format!("The agent is installed, but the flag file cannot be written: {e}"))
}

fn flag_present(host: &dyn Host) -> bool {
    host.own_dir()
        .ok()
        .is_some_and(|d| std::fs::symlink_metadata(d.join(FLAG)).is_ok_and(|m| m.is_file()))
}

/// Did another build than this program install the agent here (a white-label build took the
/// machine over, or the marker cannot be read)? The .msi custom actions run unattended, so they
/// leave such an agent alone: a refresh would be refused by the install (`other_brand`) and fail
/// the upgrade with 1603, and a removal would take away an agent the .msi did not make.
fn belongs_to_another_build(host: &dyn Host, l: &Layout) -> bool {
    marker::read(&l.marker)
        .is_some_and(|m| !marker::may_replace(&m.brand, host.brand_id().as_deref()))
}

/// A removal is the stricter case: it leaves an agent alone only when its marker names another
/// brand. A marker nobody can read is no proof that the stock .msi did not make the agent, and a
/// machine must stay cleanable (the window's Remove works the same way for any marker).
fn taken_over_by_another_brand(host: &dyn Host, l: &Layout) -> bool {
    marker::read(&l.marker).is_some_and(
        |m| matches!(&m.brand, marker::BrandMark::Brand(id) if host.brand_id().as_deref() != Some(id)),
    )
}

const ANOTHER_BUILD: &str = "The agent here was set up by another build of this program (a white-label setup took the machine over, or its marker cannot be read): left alone.";

fn remove_flag(host: &dyn Host) {
    if let Ok(dir) = host.own_dir() {
        let _ = std::fs::remove_file(dir.join(FLAG));
    }
}

/// A major upgrade without `AGENT_INI`: the agent binary and the tray are replaced, every setting
/// stays. The install gets only an empty `patch.json` and the tray, so the sealed key never
/// leaves its file.
fn refresh(host: &dyn Host) -> Result<(), String> {
    if !flag_present(host) || host.detect() != Installed::Managed {
        return Ok(());
    }
    let l = host.layout();
    if belongs_to_another_build(host, &l) {
        note(host, Command::Refresh.word(), ANOTHER_BUILD);
        return Ok(());
    }
    let installed = marker::read(&l.marker).and_then(|m| m.version);
    if crate::updates::installed_is_newer(installed.as_deref(), env!("CARGO_PKG_VERSION")) {
        note(
            host,
            Command::Refresh.word(),
            &format!(
                "The installed agent ({}) is newer than this program ({}): left alone.",
                installed.unwrap_or_default(),
                env!("CARGO_PKG_VERSION")
            ),
        );
        return Ok(());
    }
    let staging = host
        .staging()
        .map_err(|e| format!("cannot make the private working folder: {e}"))?;
    let dir = staging.dir().to_path_buf();
    let agent = staging
        .agent()
        .map_err(|e| format!("cannot unpack the agent: {e}"))?;
    let patch = write_private(&dir, "patch.json", b"{}")
        .map_err(|e| format!("cannot write the change: {e}"))?;
    let ran = host
        .stage_tray(&dir)
        .map_err(|e| scrub(e.message(), None))
        .and_then(|_| {
            // after the staging, the last thing this program does before the agent's own install
            // (see `install` for what is left before the stop)
            wait_quiet(host)?;
            run_elevated(&|a, args| host.elevate(a, args), &agent, &dir, "install")
                .map(|_| ())
                .map_err(|e| describe(&e, None))
        });
    let _ = std::fs::remove_file(patch);
    ran
}

/// Removing the .msi removes the agent it installed, and only that one (not one that a brand's
/// build took over).
fn uninstall(host: &dyn Host) -> Result<(), String> {
    if !flag_present(host) {
        return Ok(());
    }
    if host.detect() != Installed::Managed {
        // gone already, or somebody else's: the flag has nothing left to say
        remove_flag(host);
        note(
            host,
            Command::Uninstall.word(),
            "There is no agent of the setup on this machine: left alone.",
        );
        return Ok(());
    }
    if taken_over_by_another_brand(host, &host.layout()) {
        remove_flag(host);
        note(host, Command::Uninstall.word(), ANOTHER_BUILD);
        return Ok(());
    }
    let staging = host
        .staging()
        .map_err(|e| format!("cannot make the private working folder: {e}"))?;
    let dir = staging.dir().to_path_buf();
    let agent = staging
        .agent()
        .map_err(|e| format!("cannot unpack the agent: {e}"))?;
    // The removal waits for a print like the install does, but never fails for it: a .msi that
    // cannot be removed because a printer is slow is worse than a job that is printed twice.
    if wait_quiet(host).is_err() {
        note(host, Command::Uninstall.word(), PRINTING_REMOVED);
    }
    run_elevated(&|a, args| host.elevate(a, args), &agent, &dir, "uninstall")
        .map_err(|e| describe(&e, None))?;
    remove_flag(host);
    Ok(())
}

fn note(host: &dyn Host, word: &str, text: &str) {
    let Some(dir) = host.log_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG))
    {
        // seconds since 1970, UTC: a log of a few lines needs no calendar. One write of the whole
        // line: two programs that log at once (an append) do not tear each other's lines.
        let line = format!("{at} {word}: {}\n", text.replace(['\r', '\n'], " "));
        let _ = file.write_all(line.as_bytes());
    }
}

fn log(host: &dyn Host, command: &Command, line: &str) {
    note(host, command.word(), line);
}

// ---- the real machine

struct Machine;

impl Host for Machine {
    fn layout(&self) -> Layout {
        crate::commands::layout()
    }

    fn computer_name(&self) -> Option<String> {
        std::env::var("COMPUTERNAME").ok()
    }

    fn own_dir(&self) -> io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        exe.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| io::Error::other("the program has no folder"))
    }

    fn program_files(&self) -> PathBuf {
        install::program_files()
    }

    #[cfg(windows)]
    fn owner_sid(&self, path: &Path) -> io::Result<String> {
        printpapi_agent::apply::dir_owner_sid(path)
    }

    #[cfg(not(windows))]
    fn owner_sid(&self, _: &Path) -> io::Result<String> {
        Err(io::Error::other(
            "the owner of a file is read on Windows only",
        ))
    }

    fn detect(&self) -> Installed {
        install::detect(
            install::Os::host(),
            &self.layout(),
            &install::system_dir(),
            &crate::commands::runner(),
        )
    }

    fn staging(&self) -> io::Result<Staging> {
        Staging::new()
    }

    fn stage_tray(&self, dir: &Path) -> Result<bool, SetupError> {
        crate::commands::stage_own_tray(dir)
    }

    fn elevate(&self, agent: &Path, args: &[&OsStr]) -> Result<(), Elevate> {
        let mut command = std::process::Command::new(agent);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        crate::commands::no_window(&mut command);
        let status = command
            .status()
            .map_err(|e| Elevate::Failed(format!("cannot start the agent: {e}")))?;
        match status.code() {
            Some(0) => Ok(()),
            Some(code) => Err(Elevate::Failed(format!(
                "the agent exited with code {code}"
            ))),
            None => Err(Elevate::Failed("the agent was stopped".into())),
        }
    }

    fn log_dir(&self) -> Option<PathBuf> {
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os(LOG_DIR_ENV).filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        crate::staging::private_root().ok()
    }

    fn default_name(&self) -> String {
        default_device_name()
    }

    fn brand_id(&self) -> Option<String> {
        crate::brand::current().map(|b| b.id.clone())
    }

    fn status(&self) -> Option<StatusSeen> {
        read_status_file(&self.layout().status_dir.join("status.json"))
    }

    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }

    fn pause(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

/// `let _`: a program without a console may have no stderr.
fn say(text: &str) {
    let _ = writeln!(io::stderr(), "{text}");
}

/// The entry of `main`: exit 0 ok, 1 failed, 2 a wrong call or a system that has no such mode.
pub fn run(call: &Result<Command, Usage>) -> ExitCode {
    // Windows Installer runs this as LocalSystem with the environment of whoever started it: nothing
    // below, and no agent started from here, may see that (winenv.rs).
    // SAFETY: the first thing `main` does for a managed call; no thread has been started
    #[cfg(windows)]
    if let Err(e) = unsafe { crate::winenv::leave_the_starters_environment() } {
        let line = format!("cannot leave the environment of the program that started this: {e}");
        note(&Machine, "managed", &line);
        say(&line);
        return ExitCode::FAILURE;
    }
    match call {
        Ok(command) if cfg!(windows) => match execute(&Machine, command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(line) => {
                say(&line);
                ExitCode::FAILURE
            }
        },
        // session 0 shows nobody the text: the log is where the administrator looks for it
        _ if cfg!(windows) => wrong_call(&Machine),
        _ => {
            say(USAGE);
            ExitCode::from(2)
        }
    }
}

/// A call that is none of the three forms (a relative path, `AGENT_INI=D:\`): the usage goes to the
/// log as well, so that `msiexec`'s 1603 leaves a line to read. Nothing of the call is echoed.
fn wrong_call(host: &dyn Host) -> ExitCode {
    note(host, "managed", USAGE);
    say(USAGE);
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};

    const KEY: &str = "SECRET-KEY-123";

    /// What the fake agent does on its next run.
    #[derive(Clone)]
    struct Reply {
        ok: bool,
        code: &'static str,
        message: String,
        /// The run leaves the marker behind (an install that failed late).
        marker: bool,
    }

    fn done() -> Reply {
        Reply {
            ok: true,
            code: "",
            message: String::new(),
            marker: false,
        }
    }

    fn failed(code: &'static str, message: &str, marker: bool) -> Reply {
        Reply {
            ok: false,
            code,
            message: message.into(),
            marker,
        }
    }

    /// One run of the fake agent: the words it got and what the staging folder held.
    struct Run {
        command: String,
        dir: PathBuf,
        args: Vec<String>,
        files: BTreeMap<String, Vec<u8>>,
    }

    struct Fake {
        root: tempfile::TempDir,
        layout: Layout,
        own: PathBuf,
        pf: PathBuf,
        name: Option<String>,
        sid: Result<String, String>,
        /// The owner of a folder, when it is not the owner of the files.
        folder_sid: Option<Result<String, String>>,
        detect: Cell<Installed>,
        staging_fails: bool,
        tray_fails: bool,
        log: Option<PathBuf>,
        /// The brand id of the build that runs (`None`: the stock program).
        brand: Option<String>,
        runs: RefCell<Vec<Run>>,
        replies: RefCell<VecDeque<Reply>>,
        /// How many looks at the status file find the agent printing (`u32::MAX`: always).
        printing_looks: Cell<u32>,
        /// What the status file says while it is not printing (`None`: there is no file).
        quiet_status: Option<String>,
        looks: Cell<u32>,
        slept: Cell<u64>,
    }

    impl Fake {
        fn new(detect: Installed) -> Fake {
            let root = tempfile::tempdir().unwrap();
            let p = root.path();
            let pf = p.join("Program Files");
            let own = pf.join("printpapi Agent Setup");
            std::fs::create_dir_all(&own).unwrap();
            std::fs::create_dir_all(p.join("work")).unwrap();
            let dir = pf.join("printpapi-agent");
            let layout = Layout {
                binary: dir.join("printpapi-agent.exe"),
                ini: dir.join("agent.ini"),
                marker: dir.join(".installed-by-setup"),
                sumatra: dir.join("SumatraPDF.exe"),
                autostart: dir.join("task.xml"),
                status_dir: p.join("status"),
                tray: dir.join("printpapi-tray.exe"),
                tray_autostart: None,
                app_entry: None,
                dir,
            };
            Fake {
                layout,
                own,
                pf,
                name: Some("SHOP-PC".into()),
                sid: Ok("S-1-5-18".into()),
                folder_sid: None,
                detect: Cell::new(detect),
                staging_fails: false,
                tray_fails: false,
                log: Some(p.join("log")),
                brand: None,
                runs: RefCell::new(Vec::new()),
                replies: RefCell::new(VecDeque::new()),
                printing_looks: Cell::new(0),
                quiet_status: None,
                looks: Cell::new(0),
                slept: Cell::new(0),
                root,
            }
        }

        fn work(&self) -> PathBuf {
            self.root.path().join("work")
        }

        /// A prepared ini as `<name>.ini` in a folder; returns the folder.
        fn prepared(&self, file: &str, text: &str) -> PathBuf {
            let dir = self.root.path().join("prep");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
            dir
        }

        fn reply(&self, r: Reply) {
            self.replies.borrow_mut().push_back(r);
        }

        fn install_dir_with(&self, ini: &str) {
            std::fs::create_dir_all(&self.layout.dir).unwrap();
            std::fs::write(&self.layout.ini, ini).unwrap();
        }

        fn flag(&self) -> PathBuf {
            self.own.join(FLAG)
        }

        fn logged(&self) -> String {
            self.log
                .as_ref()
                .and_then(|d| std::fs::read_to_string(d.join(LOG)).ok())
                .unwrap_or_default()
        }

        fn commands(&self) -> Vec<String> {
            self.runs
                .borrow()
                .iter()
                .map(|r| r.command.clone())
                .collect()
        }

        fn staged(&self, run: usize, file: &str) -> String {
            let runs = self.runs.borrow();
            String::from_utf8_lossy(&runs[run].files[file]).into_owned()
        }
    }

    impl Host for Fake {
        fn layout(&self) -> Layout {
            self.layout.clone()
        }
        fn computer_name(&self) -> Option<String> {
            self.name.clone()
        }
        fn own_dir(&self) -> io::Result<PathBuf> {
            Ok(self.own.clone())
        }
        fn program_files(&self) -> PathBuf {
            self.pf.clone()
        }
        fn owner_sid(&self, path: &Path) -> io::Result<String> {
            match &self.folder_sid {
                Some(sid) if path.is_dir() => sid.clone(),
                _ => self.sid.clone(),
            }
            .map_err(io::Error::other)
        }
        fn detect(&self) -> Installed {
            self.detect.get()
        }
        fn staging(&self) -> io::Result<Staging> {
            if self.staging_fails {
                return Err(io::Error::other("no profile"));
            }
            Staging::in_root(Ok(self.work()))
        }
        fn stage_tray(&self, dir: &Path) -> Result<bool, SetupError> {
            if self.tray_fails {
                return Err(SetupError::new("system", "cannot prepare the tray icon"));
            }
            std::fs::write(dir.join("tray"), b"tray").unwrap();
            Ok(true)
        }
        fn elevate(&self, agent: &Path, args: &[&OsStr]) -> Result<(), Elevate> {
            let args: Vec<String> = args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let dir = PathBuf::from(&args[1]);
            assert!(agent.starts_with(&dir), "the agent is the staged copy");
            let mut files = BTreeMap::new();
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name != "printpapi-agent.exe" && name != "printpapi-agent" {
                    files.insert(name, std::fs::read(entry.path()).unwrap());
                }
            }
            self.runs.borrow_mut().push(Run {
                command: args[0].clone(),
                dir: dir.clone(),
                args,
                files,
            });
            let reply = self.replies.borrow_mut().pop_front().unwrap_or_else(done);
            if reply.marker {
                std::fs::create_dir_all(&self.layout.dir).unwrap();
                std::fs::write(&self.layout.marker, b"").unwrap();
            }
            let result = if reply.ok {
                serde_json::json!({"ok": true})
            } else {
                serde_json::json!({"ok": false, "code": reply.code, "message": reply.message})
            };
            std::fs::write(dir.join("result.json"), result.to_string()).unwrap();
            if reply.ok {
                Ok(())
            } else {
                Err(Elevate::Failed("exit 1".into()))
            }
        }
        fn log_dir(&self) -> Option<PathBuf> {
            self.log.clone()
        }
        fn default_name(&self) -> String {
            "FAKE-NAME".into()
        }
        fn brand_id(&self) -> Option<String> {
            self.brand.clone()
        }
        fn status(&self) -> Option<StatusSeen> {
            self.looks.set(self.looks.get() + 1);
            let left = self.printing_looks.get();
            if left > 0 {
                if left != u32::MAX {
                    self.printing_looks.set(left - 1);
                }
                return Some(StatusSeen {
                    age: 0.5,
                    text: r#"{"state":"online","busy":true}"#.into(),
                });
            }
            self.quiet_status
                .clone()
                .map(|text| StatusSeen { age: 0.5, text })
        }
        fn now(&self) -> f64 {
            1_000.0
        }
        fn pause(&self, secs: u64) {
            self.slept.set(self.slept.get() + secs);
        }
    }

    fn ini(extra: &str) -> String {
        format!(
            "; by the deployment tool\n[agent]\nserver_url = https://print.example\napi_key = {KEY}\n{extra}printers = Zebra\ntimeout = 45\n\n[headers]\nX-Test = a\n"
        )
    }

    fn install_of(dir: &Path) -> Command {
        Command::Install(dir.to_path_buf())
    }

    fn words(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    // ---- 3. the call

    #[test]
    fn a_call_is_one_of_three_forms_and_anything_else_is_a_usage_error() {
        let abs = std::env::temp_dir().join("x.ini");
        let abs = abs.to_str().unwrap();
        assert_eq!(parse(&words(&["--tray"])), None);
        assert_eq!(parse(&[]), None);
        assert_eq!(
            parse(&words(&["--managed-install", abs])),
            Some(Ok(Command::Install(PathBuf::from(abs))))
        );
        // a folder with a trailing backslash inside quotes arrives with a quote on its end
        assert_eq!(
            parse(&words(&["--managed-install", &format!("{abs}\"")])),
            Some(Ok(Command::Install(PathBuf::from(abs))))
        );
        assert_eq!(
            parse(&words(&["--managed-install", "\""])),
            Some(Err(Usage))
        );
        assert_eq!(
            parse(&words(&["--managed-refresh"])),
            Some(Ok(Command::Refresh))
        );
        assert_eq!(
            parse(&words(&["--managed-uninstall"])),
            Some(Ok(Command::Uninstall))
        );
        for bad in [
            &["--managed-install"][..],
            &["--managed-install", "relative.ini"],
            &["--managed-install", ""],
            &["--managed-install", abs, "extra"],
            &["--managed-refresh", abs],
            &["--managed-uninstall", "x"],
            &["--managed-nope"],
            &["--tray", "--managed-refresh"],
            &["x", "--managed-install", abs],
        ] {
            assert_eq!(parse(&words(bad)), Some(Err(Usage)), "{bad:?}");
        }
    }

    #[test]
    fn the_quote_that_a_trailing_backslash_leaves_is_cut_and_a_drive_root_stays_a_root() {
        assert_eq!(cut_quote("C:\\folder\""), "C:\\folder");
        assert_eq!(cut_quote("C:\\folder"), "C:\\folder");
        assert_eq!(cut_quote("D:\""), "D:\\");
        assert_eq!(cut_quote("D:"), "D:", "no quote, nothing to put back");
        assert_eq!(cut_quote("\""), "");
        #[cfg(windows)]
        assert_eq!(
            parse(&words(&["--managed-install", "D:\""])),
            Some(Ok(Command::Install(PathBuf::from("D:\\"))))
        );
    }

    #[test]
    fn a_wrong_call_leaves_a_line_in_the_log_and_exits_2() {
        let h = Fake::new(Installed::None);
        assert_eq!(
            format!("{:?}", wrong_call(&h)),
            format!("{:?}", ExitCode::from(2))
        );
        let log = h.logged();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(
            log.contains("usage: printpapi-setup --managed-install"),
            "{log}"
        );
    }

    #[test]
    fn a_machine_without_these_modes_prints_the_usage_and_exits_2() {
        if cfg!(windows) {
            return;
        }
        for call in [Ok(Command::Refresh), Ok(Command::Uninstall), Err(Usage)] {
            assert_eq!(
                format!("{:?}", run(&call)),
                format!("{:?}", ExitCode::from(2))
            );
        }
    }

    // ---- 4. intake

    #[test]
    fn a_folder_gives_the_file_of_this_computer_and_not_another() {
        let h = Fake::new(Installed::None);
        h.prepared("OTHER-PC.ini", &ini("name = other\n"));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = mine\n"));
        execute(&h, &install_of(&dir)).unwrap();
        assert!(h.staged(0, "agent.ini").contains("name = mine"));
        assert!(!h.staged(0, "agent.ini").contains("other"));
    }

    #[test]
    fn a_file_path_is_used_as_it_is() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared("anything.ini", &ini("name = mine\n"));
        execute(&h, &install_of(&dir.join("anything.ini"))).unwrap();
        assert!(h.staged(0, "agent.ini").contains("name = mine"));
    }

    #[test]
    fn a_folder_without_the_file_of_this_computer_is_refused_and_nothing_is_staged() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared("OTHER-PC.ini", &ini(""));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("SHOP-PC.ini"), "{e}");
        assert!(h.runs.borrow().is_empty());
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
        assert!(
            h.logged().contains("managed-install"),
            "the failure is logged"
        );
        let missing = h.root.path().join("nothing-here");
        assert!(execute(&h, &install_of(&missing)).is_err());
    }

    #[test]
    fn a_computer_name_that_is_a_path_is_refused() {
        for bad in [
            "..\\evil", "a/b", "a\\b", "x..y", "C:", "", "  ", "a*b", "a\nb",
        ] {
            let mut h = Fake::new(Installed::None);
            h.name = Some(bad.to_string());
            let dir = h.prepared("evil.ini", &ini(""));
            assert!(execute(&h, &install_of(&dir)).is_err(), "{bad:?}");
            assert!(h.runs.borrow().is_empty(), "{bad:?}");
        }
        let mut h = Fake::new(Installed::None);
        h.name = None;
        let dir = h.prepared("x.ini", &ini(""));
        assert!(execute(&h, &install_of(&dir)).is_err());
    }

    #[test]
    fn a_file_over_256_kib_is_not_an_ini() {
        let h = Fake::new(Installed::None);
        let big = ini("") + &"; padding\n".repeat(30_000);
        assert!(big.len() as u64 > MAX_INI);
        let dir = h.prepared("SHOP-PC.ini", &big);
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("256 KiB"), "{e}");
        assert!(h.runs.borrow().is_empty());
        let just = ini("") + &" ".repeat(MAX_INI as usize - ini("").len());
        let dir = h.prepared("SHOP-PC.ini", &just);
        execute(&h, &install_of(&dir)).unwrap();
    }

    /// The mode exists on Windows only, where the link that matters is a junction (a reparse point,
    /// made without any right): as the path itself and as the folder that holds the file.
    #[cfg(windows)]
    #[test]
    fn a_junction_is_refused_as_the_path_and_as_the_folder_of_a_file() {
        let h = Fake::new(Installed::None);
        let real = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let junction = h.root.path().join("junction");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        for arg in [junction.clone(), junction.join("SHOP-PC.ini")] {
            let e = execute(&h, &install_of(&arg)).unwrap_err();
            assert!(e.contains("link"), "{arg:?}: {e}");
        }
        assert!(h.runs.borrow().is_empty());
        // and the real folder still works
        execute(&h, &install_of(&real)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_refused_as_the_path_and_inside_the_folder() {
        let h = Fake::new(Installed::None);
        let real = h.prepared("real.ini", &ini(""));
        let link = h.root.path().join("link.ini");
        std::os::unix::fs::symlink(real.join("real.ini"), &link).unwrap();
        let e = execute(&h, &install_of(&link)).unwrap_err();
        assert!(e.contains("link"), "{e}");
        let folder = h.root.path().join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        std::os::unix::fs::symlink(real.join("real.ini"), folder.join("SHOP-PC.ini")).unwrap();
        assert!(
            execute(&h, &install_of(&folder))
                .unwrap_err()
                .contains("link")
        );
        let folder_link = h.root.path().join("folder-link");
        std::os::unix::fs::symlink(&real, &folder_link).unwrap();
        assert!(
            execute(&h, &install_of(&folder_link))
                .unwrap_err()
                .contains("link")
        );
        assert!(h.runs.borrow().is_empty());
    }

    #[test]
    fn utf_16_and_a_byte_order_mark_are_read() {
        let text = "[agent]\nname = Küche\nprinters = Zebra\n";
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let bom: Vec<u8> = [0xEF, 0xBB, 0xBF].into_iter().chain(text.bytes()).collect();
        for bytes in [&le, &be, &bom, &text.as_bytes().to_vec()] {
            assert_eq!(decode_prepared(bytes), text);
        }
        // and all the way through an install
        let h = Fake::new(Installed::None);
        let full = ini("");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(full.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let dir = h.prepared("SHOP-PC.ini", "");
        std::fs::write(dir.join("SHOP-PC.ini"), utf16).unwrap();
        execute(&h, &install_of(&dir)).unwrap();
        assert!(
            h.staged(0, "agent.ini")
                .contains("server_url = https://print.example")
        );
    }

    #[test]
    fn a_file_that_system_or_administrators_do_not_own_is_refused() {
        for sid in ["S-1-5-21-1-2-3-1001", "S-1-1-0", "S-1-5-32-545"] {
            let mut h = Fake::new(Installed::None);
            h.sid = Ok(sid.into());
            let dir = h.prepared("SHOP-PC.ini", &ini(""));
            let e = execute(&h, &install_of(&dir)).unwrap_err();
            assert!(e.contains("not owned by SYSTEM or Administrators"), "{e}");
            assert!(h.runs.borrow().is_empty(), "{sid}");
        }
        let mut h = Fake::new(Installed::None);
        h.sid = Err("access denied".into());
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        assert!(
            execute(&h, &install_of(&dir))
                .unwrap_err()
                .contains("cannot read the owner")
        );
        for sid in ["S-1-5-18", "S-1-5-32-544"] {
            let mut h = Fake::new(Installed::None);
            h.sid = Ok(sid.into());
            let dir = h.prepared("SHOP-PC.ini", &ini(""));
            execute(&h, &install_of(&dir)).unwrap();
        }
    }

    #[test]
    fn a_folder_that_somebody_else_owns_is_refused_even_when_the_file_is_system_s() {
        // a standard user may make C:\ProgramData\<name> first: no icacls line repairs that folder,
        // the user's own entries stay on it, and the user can still edit what SYSTEM drops in
        for sid in ["S-1-5-21-1-2-3-1001", "S-1-1-0", "S-1-5-32-545"] {
            let mut h = Fake::new(Installed::None);
            h.folder_sid = Some(Ok(sid.into()));
            let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
            // as a folder, and as the path of the file in it
            for arg in [dir.clone(), dir.join("SHOP-PC.ini")] {
                let e = execute(&h, &install_of(&arg)).unwrap_err();
                assert!(e.contains("not owned by SYSTEM or Administrators"), "{e}");
                assert!(e.contains("folder"), "{e}");
                assert!(!e.contains(KEY));
            }
            assert!(h.runs.borrow().is_empty(), "{sid}");
        }
        let mut h = Fake::new(Installed::None);
        h.folder_sid = Some(Err("access denied".into()));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        assert!(
            execute(&h, &install_of(&dir))
                .unwrap_err()
                .contains("cannot read the owner")
        );
        for sid in ["S-1-5-18", "S-1-5-32-544"] {
            let mut h = Fake::new(Installed::None);
            h.folder_sid = Some(Ok(sid.into()));
            let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
            execute(&h, &install_of(&dir)).unwrap();
        }
    }

    #[test]
    fn a_file_the_wrong_owner_made_is_to_be_written_again_not_taken_over() {
        let mut h = Fake::new(Installed::None);
        h.sid = Ok("S-1-5-21-1-2-3-1001".into());
        h.folder_sid = Some(Ok("S-1-5-32-544".into()));
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("not owned by SYSTEM or Administrators"), "{e}");
        // /setowner leaves the access list, so the one who made the file keeps reading the key
        // and editing the file, and both owner checks pass: the file is made again instead
        assert!(!e.contains("icacls") && !e.contains("/setowner"), "{e}");
        assert!(
            e.contains("Delete it") && e.contains("elevated session"),
            "{e}"
        );
        assert!(!e.contains(KEY));
    }

    #[test]
    fn a_folder_that_somebody_made_is_named_before_the_file_whatever_the_file_s_owner_is() {
        let mut h = Fake::new(Installed::None);
        h.sid = Ok("S-1-5-21-1-2-3-1001".into());
        h.folder_sid = Some(Ok("S-1-5-21-1-2-3-1001".into()));
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.starts_with("The folder"), "{e}");
        assert!(!e.contains("/setowner"), "{e}");
        // an advice to delete would also go to the owner of a system folder
        assert!(!e.to_lowercase().contains("delete"), "{e}");
    }

    #[test]
    fn a_drive_root_is_refused_with_a_message_of_its_own() {
        let h = Fake::new(Installed::None);
        let root = std::env::temp_dir()
            .ancestors()
            .last()
            .unwrap()
            .to_path_buf();
        let e = execute(&h, &install_of(&root)).unwrap_err();
        assert!(e.contains("of its own"), "{e}");
        assert!(
            !e.to_lowercase().contains("delete") && !e.contains("not owned"),
            "{e}"
        );
        assert!(h.runs.borrow().is_empty());
        // a file directly in a root, and the folders below it
        assert!(refuse_root(&root).is_err());
        assert!(refuse_root(&root.join("x")).is_ok());
        assert!(refuse_root(&std::env::temp_dir()).is_ok());
        // a share's root is a share, not a drive: the owner of a file on it is not asked for anyway
        assert!(refuse_root(Path::new(r"\\server\share\")).is_ok());
    }

    #[test]
    fn a_file_on_a_share_is_not_asked_for_its_owner() {
        assert!(is_unc(Path::new(r"\\server\share\x.ini")));
        assert!(is_unc(Path::new(r"\\?\UNC\server\share\x.ini")));
        assert!(!is_unc(Path::new(r"\\?\C:\x.ini")));
        assert!(!is_unc(Path::new(r"C:\x.ini")));
        assert!(!is_unc(Path::new("/tmp/x.ini")));
    }

    #[test]
    fn an_ini_that_cannot_be_read_is_refused_without_quoting_it() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared(
            "SHOP-PC.ini",
            &format!("[agent]\napi_key = {KEY}\n this is not a line\n[agent\n"),
        );
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(!e.contains(KEY) && !h.logged().contains(KEY));
        assert!(h.runs.borrow().is_empty());
    }

    // ---- 5. the name, the update mode, what survives

    #[test]
    fn a_managed_machine_keeps_its_name_when_the_file_has_none() {
        let h = Fake::new(Installed::Managed);
        h.install_dir_with(
            "[agent]\nserver_url = https://old\napi_key_dpapi = 00\nname = OLD-PC\nprinters = x\n",
        );
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        execute(&h, &install_of(&dir)).unwrap();
        let staged = h.staged(0, "agent.ini");
        assert!(staged.contains("name = OLD-PC"), "{staged}");
        assert!(!staged.contains("FAKE-NAME"));
    }

    #[test]
    fn a_fresh_machine_or_one_without_an_installed_file_gets_the_computers_name() {
        for detect in [Installed::None, Installed::Managed] {
            let h = Fake::new(detect);
            let dir = h.prepared("SHOP-PC.ini", &ini(""));
            execute(&h, &install_of(&dir)).unwrap();
            assert!(
                h.staged(0, "agent.ini").contains("name = FAKE-NAME"),
                "{detect:?}"
            );
        }
    }

    #[test]
    fn an_installed_file_that_cannot_be_read_stops_the_install_without_quoting_it() {
        let h = Fake::new(Installed::Managed);
        h.install_dir_with(&format!("api_key = {KEY}\n garbage\n[agent\n"));
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("installed agent.ini cannot be read"), "{e}");
        assert!(!e.contains(KEY) && !h.logged().contains(KEY));
        assert!(h.runs.borrow().is_empty());
    }

    #[test]
    fn a_name_in_the_file_wins_and_the_file_is_staged_as_it_is() {
        let h = Fake::new(Installed::Managed);
        h.install_dir_with("[agent]\nname = OLD-PC\n");
        let prepared = ini("name = NEW-PC\nupdates = auto\n");
        let dir = h.prepared("SHOP-PC.ini", &prepared);
        execute(&h, &install_of(&dir)).unwrap();
        assert_eq!(h.staged(0, "agent.ini"), prepared);
    }

    #[test]
    fn a_managed_install_switches_updates_off_unless_the_file_names_them() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        execute(&h, &install_of(&dir)).unwrap();
        assert!(h.staged(0, "agent.ini").contains("updates = off"));
        for mode in ["notify", "auto", "off"] {
            let h = Fake::new(Installed::None);
            let dir = h.prepared(
                "SHOP-PC.ini",
                &ini(&format!("name = a\nupdates = {mode}\n")),
            );
            execute(&h, &install_of(&dir)).unwrap();
            let staged = h.staged(0, "agent.ini");
            assert!(staged.contains(&format!("updates = {mode}")), "{staged}");
            assert_eq!(staged.matches("updates").count(), 1, "{staged}");
        }
    }

    #[test]
    fn headers_the_timeout_and_comments_of_the_file_survive_the_additions() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        execute(&h, &install_of(&dir)).unwrap();
        let staged = h.staged(0, "agent.ini");
        for kept in [
            "; by the deployment tool",
            "timeout = 45",
            "[headers]",
            "X-Test = a",
            "printers = Zebra",
            KEY,
        ] {
            assert!(staged.contains(kept), "{kept}: {staged}");
        }
    }

    // ---- 5b. a running agent is not stopped in the middle of a print

    const BUSY: &str = r#"{"state":"online","busy":true}"#;

    #[test]
    fn an_install_over_a_printing_agent_waits_until_it_is_quiet() {
        let h = Fake::new(Installed::Managed);
        h.printing_looks.set(3);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        execute(&h, &install_of(&dir)).unwrap();
        assert_eq!(h.commands(), ["install"]);
        // three looks that find it printing, then two quiet ones one second apart
        assert_eq!(h.looks.get(), 5);
        assert_eq!(h.slept.get(), 4);
    }

    #[test]
    fn an_install_that_waits_two_minutes_stops_with_a_line_and_touches_nothing() {
        let h = Fake::new(Installed::Managed);
        h.printing_looks.set(u32::MAX);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("printing"), "{e}");
        assert_eq!(h.slept.get(), QUIET_WAIT_SECS);
        assert!(h.runs.borrow().is_empty() && !h.flag().exists());
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
        assert!(h.logged().contains("managed-install"), "{}", h.logged());
        assert!(!h.logged().contains(KEY));
    }

    #[test]
    fn a_print_that_ends_at_the_last_second_is_not_a_failure() {
        // printing at every look of the first two minutes, quiet from the next one on: one more
        // look is taken before the wait gives up
        let h = Fake::new(Installed::Managed);
        h.printing_looks.set(QUIET_WAIT_SECS as u32);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        execute(&h, &install_of(&dir)).unwrap();
        assert_eq!(h.commands(), ["install"]);
        assert_eq!(h.slept.get(), QUIET_WAIT_SECS + 1);

        // still printing at that look: the wait is over
        let h = Fake::new(Installed::Managed);
        h.printing_looks.set(QUIET_WAIT_SECS as u32 + 1);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        assert!(execute(&h, &install_of(&dir)).is_err());
        assert!(h.runs.borrow().is_empty());
    }

    #[test]
    fn a_stopped_offline_or_stale_agent_holds_nothing_up() {
        for status in [
            None,
            Some(r#"{"state":"offline","busy":false}"#),
            Some("not json"),
        ] {
            let mut h = Fake::new(Installed::Managed);
            h.quiet_status = status.map(str::to_string);
            let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
            execute(&h, &install_of(&dir)).unwrap();
            assert_eq!(h.slept.get(), 1, "{status:?}: two looks, one second apart");
        }
        // an old file of a busy agent is a file that nobody writes any more
        let seen = StatusSeen {
            age: 600.0,
            text: BUSY.into(),
        };
        assert!(!still_printing(Some(&seen), 1_000.0));
    }

    #[test]
    fn the_first_job_after_an_offline_phase_is_a_print_too() {
        // claimed() sets busy before the state turns online again
        let mut h = Fake::new(Installed::Managed);
        h.quiet_status = Some(r#"{"state":"offline","busy":true}"#.into());
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("printing"), "{e}");
        assert_eq!(h.slept.get(), QUIET_WAIT_SECS);
        assert!(h.runs.borrow().is_empty());
    }

    #[test]
    fn the_wait_comes_after_everything_is_staged_so_that_a_job_claimed_meanwhile_is_not_cut() {
        // the tray cannot be staged: nothing was waited for, because nothing was left to do
        // between the last look and the stop of the agent
        let mut h = Fake::new(Installed::Managed);
        h.tray_fails = true;
        h.printing_looks.set(u32::MAX);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("tray"), "{e}");
        assert_eq!(h.looks.get(), 0);

        let mut h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.tray_fails = true;
        h.printing_looks.set(u32::MAX);
        let e = execute(&h, &Command::Refresh).unwrap_err();
        assert!(e.contains("tray"), "{e}");
        assert_eq!(h.looks.get(), 0);
    }

    #[test]
    fn a_machine_without_an_agent_does_not_wait_or_look() {
        for detect in [Installed::None, Installed::Foreign] {
            let h = Fake::new(detect);
            h.printing_looks.set(u32::MAX);
            let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
            let _ = execute(&h, &install_of(&dir));
            assert_eq!((h.looks.get(), h.slept.get()), (0, 0), "{detect:?}");
        }
        // an uninstall or a refresh that does nothing does not look either
        let h = Fake::new(Installed::None);
        h.printing_looks.set(u32::MAX);
        execute(&h, &Command::Refresh).unwrap();
        assert_eq!(h.looks.get(), 0);
    }

    #[test]
    fn a_refresh_waits_for_a_print_too_and_gives_up_after_two_minutes() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.printing_looks.set(2);
        execute(&h, &Command::Refresh).unwrap();
        assert_eq!(h.commands(), ["install"]);
        assert_eq!(h.slept.get(), 3);

        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.printing_looks.set(u32::MAX);
        let e = execute(&h, &Command::Refresh).unwrap_err();
        assert!(e.contains("printing"), "{e}");
        assert!(h.runs.borrow().is_empty());
        assert!(
            h.flag().is_file(),
            "a refresh that waited in vain keeps the flag"
        );
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
        assert!(h.logged().contains("managed-refresh"));
    }

    // ---- 6. a foreign agent

    #[test]
    fn a_foreign_agent_stops_everything_before_anything_is_staged() {
        let h = Fake::new(Installed::Foreign);
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert_eq!(e, FOREIGN);
        assert!(h.runs.borrow().is_empty());
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
        assert!(!h.flag().exists());
        for detect in [Installed::None, Installed::Managed] {
            let h = Fake::new(detect);
            let dir = h.prepared("SHOP-PC.ini", &ini(""));
            execute(&h, &install_of(&dir)).unwrap();
        }
    }

    // ---- 7. the happy path

    #[test]
    fn the_agent_gets_the_ini_the_tray_and_the_pinned_sumatra_and_only_install_dir() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        std::fs::write(dir.join("SumatraPDF.exe"), b"sumatra").unwrap();
        execute(&h, &install_of(&dir)).unwrap();
        let runs = h.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].args.len(), 2);
        assert_eq!(runs[0].args[0], "install");
        assert_eq!(PathBuf::from(&runs[0].args[1]), runs[0].dir);
        let names: Vec<&str> = runs[0].files.keys().map(String::as_str).collect();
        assert_eq!(names, ["SumatraPDF.exe", "agent.ini", "tray"]);
        assert_eq!(runs[0].files["SumatraPDF.exe"], b"sumatra");
        drop(runs);
        // the working folder is gone, and the flag is there only now
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
        assert!(h.flag().is_file());
        assert!(h.logged().is_empty());
    }

    #[test]
    fn without_a_sumatra_next_to_the_ini_none_is_staged() {
        let h = Fake::new(Installed::None);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        execute(&h, &install_of(&dir)).unwrap();
        assert!(!h.runs.borrow()[0].files.contains_key("SumatraPDF.exe"));
    }

    // ---- 8. failures

    #[test]
    fn a_failed_install_is_one_key_free_log_line_and_no_flag() {
        let h = Fake::new(Installed::None);
        h.reply(failed(
            "unreachable",
            &format!("cannot reach https://print.example\nwith {KEY}"),
            false,
        ));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("cannot reach https://print.example"), "{e}");
        assert!(!e.contains(KEY) && !e.contains('\n'));
        let log = h.logged();
        assert_eq!(log.lines().count(), 1, "{log}");
        assert!(log.contains("managed-install") && !log.contains(KEY));
        assert!(!h.flag().exists());
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
    }

    #[test]
    fn a_key_that_another_pc_uses_says_one_key_per_pc() {
        let h = Fake::new(Installed::None);
        h.reply(failed(
            "key_in_use",
            "The server says this key belongs to another device.",
            false,
        ));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert_eq!(e, "The key is already used by another PC: one key per PC");
        assert!(h.logged().contains("one key per PC"));
    }

    #[test]
    fn an_upgrade_that_meets_a_taken_name_says_the_pc_was_reconnected() {
        // the device got another key by a pairing code (Reconnect), sealed in its agent.ini: the
        // key of the prepared file registers under the same name and the server refuses it
        let h = Fake::new(Installed::Managed);
        h.install_dir_with("[agent]\nname = shop\n");
        h.reply(failed(
            "name_taken",
            "The server refused this key. Either the key was revoked or is not meant for agents, or this device name belongs to a different key.",
            false,
        ));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = shop\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        // the real causes stay in the line, Reconnect is named as one possibility
        assert!(
            e.starts_with("The server refused this key. Either the key was revoked"),
            "{e}"
        );
        assert!(e.ends_with(NAME_TAKEN_UPGRADE), "{e}");
        assert!(
            e.contains("If this PC was reconnected") && e.contains("without AGENT_INI"),
            "{e}"
        );
        assert!(h.logged().contains("without AGENT_INI"), "{}", h.logged());
        // a first install keeps the agent's own words
        let h = Fake::new(Installed::None);
        h.reply(failed("name_taken", "The server refused this key.", false));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert_eq!(e, "The server refused this key.");
    }

    #[test]
    fn a_working_folder_that_cannot_be_made_fails_without_a_panic_and_a_tray_that_cannot_be_staged_too()
     {
        let mut h = Fake::new(Installed::None);
        h.staging_fails = true;
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("working folder"), "{e}");
        assert!(h.runs.borrow().is_empty() && !h.flag().exists());
        let mut h = Fake::new(Installed::None);
        h.tray_fails = true;
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        assert!(execute(&h, &install_of(&dir)).unwrap_err().contains("tray"));
        assert!(h.runs.borrow().is_empty() && !h.flag().exists());
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
    }

    #[test]
    fn without_a_place_for_the_log_the_failure_is_still_reported() {
        let mut h = Fake::new(Installed::Foreign);
        h.log = None;
        let dir = h.prepared("SHOP-PC.ini", &ini(""));
        assert!(execute(&h, &install_of(&dir)).is_err());
    }

    // ---- 9. a first install that fails late

    #[test]
    fn a_first_install_that_fails_after_the_marker_exists_is_taken_back() {
        let h = Fake::new(Installed::None);
        h.reply(failed("system", "the task could not be made", true));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        assert!(execute(&h, &install_of(&dir)).is_err());
        assert_eq!(h.commands(), ["install", "uninstall"]);
        // the key file is gone before the second run
        assert!(!h.runs.borrow()[1].files.contains_key("agent.ini"));
        assert!(!h.flag().exists());
    }

    #[test]
    fn a_failure_that_left_no_marker_or_hit_an_agent_that_was_there_removes_nothing() {
        let h = Fake::new(Installed::None);
        h.reply(failed("unreachable", "offline", false));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        assert!(execute(&h, &install_of(&dir)).is_err());
        assert_eq!(h.commands(), ["install"]);
        let h = Fake::new(Installed::Managed);
        h.reply(failed("system", "broken", true));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        assert!(execute(&h, &install_of(&dir)).is_err());
        assert_eq!(h.commands(), ["install"]);
    }

    // ---- 10. refresh

    #[test]
    fn a_refresh_installs_an_empty_patch_and_the_tray_and_keeps_the_flag() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        execute(&h, &Command::Refresh).unwrap();
        let runs = h.runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].args[0], "install");
        let names: Vec<&str> = runs[0].files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["patch.json", "tray"],
            "no agent.ini: the key stays in its own file"
        );
        assert_eq!(runs[0].files["patch.json"], b"{}");
        drop(runs);
        assert!(h.flag().is_file());
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
    }

    #[test]
    fn a_refresh_without_the_flag_or_on_a_machine_that_is_not_managed_does_nothing() {
        let h = Fake::new(Installed::Managed);
        execute(&h, &Command::Refresh).unwrap();
        assert!(h.runs.borrow().is_empty());
        for detect in [Installed::None, Installed::Foreign] {
            let h = Fake::new(detect);
            std::fs::write(h.flag(), b"").unwrap();
            execute(&h, &Command::Refresh).unwrap();
            assert!(h.runs.borrow().is_empty(), "{detect:?}");
        }
    }

    /// A machine that the stock .msi was deployed to and that a white-label build then took over:
    /// the flag file is still there, the marker names the brand.
    fn taken_over_by(h: &Fake, marker: &[u8]) {
        std::fs::write(h.flag(), b"").unwrap();
        std::fs::create_dir_all(&h.layout.dir).unwrap();
        std::fs::write(&h.layout.marker, marker).unwrap();
    }

    #[test]
    fn a_refresh_leaves_an_agent_of_another_build_alone_and_succeeds() {
        // otherwise the install answers other_brand, the custom action (Return="check") fails
        // with 1603 and every later .msi upgrade rolls back
        for marker in [&b"version=3.6.0\nbrand=acme\n"[..], b"\xff\xfe junk"] {
            let h = Fake::new(Installed::Managed);
            taken_over_by(&h, marker);
            execute(&h, &Command::Refresh).unwrap();
            assert!(h.runs.borrow().is_empty());
            assert!(h.logged().contains("another build"), "{}", h.logged());
            assert!(
                h.flag().is_file(),
                "the flag stays: the person may take the machine back"
            );
        }
        // the build of that brand itself refreshes
        let mut h = Fake::new(Installed::Managed);
        h.brand = Some("acme".into());
        taken_over_by(&h, b"version=0.0.1\nbrand=acme\n");
        execute(&h, &Command::Refresh).unwrap();
        assert_eq!(h.commands(), ["install"]);
        // a stock marker is the stock build's own
        let h = Fake::new(Installed::Managed);
        taken_over_by(&h, b"version=0.0.1\n");
        execute(&h, &Command::Refresh).unwrap();
        assert_eq!(h.commands(), ["install"]);
    }

    #[test]
    fn a_refresh_never_downgrades_an_installed_agent_and_says_so() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        std::fs::create_dir_all(&h.layout.dir).unwrap();
        std::fs::write(&h.layout.marker, b"version=99.0.0\n").unwrap();
        execute(&h, &Command::Refresh).unwrap();
        assert!(h.runs.borrow().is_empty());
        assert!(h.logged().contains("newer"), "{}", h.logged());
        // an older or unknown version is refreshed
        for marker in ["version=0.0.1\n", ""] {
            let h = Fake::new(Installed::Managed);
            std::fs::write(h.flag(), b"").unwrap();
            std::fs::create_dir_all(&h.layout.dir).unwrap();
            std::fs::write(&h.layout.marker, marker).unwrap();
            execute(&h, &Command::Refresh).unwrap();
            assert_eq!(h.commands(), ["install"]);
        }
    }

    #[test]
    fn a_failed_refresh_is_logged_and_fails() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.reply(failed("system", "the agent would not start", false));
        let e = execute(&h, &Command::Refresh).unwrap_err();
        assert!(e.contains("would not start"));
        assert!(h.logged().contains("managed-refresh"));
        assert!(h.flag().is_file(), "a failed refresh keeps the flag");
    }

    // ---- 11. uninstall

    #[test]
    fn an_uninstall_without_the_flag_leaves_the_agent_alone() {
        let h = Fake::new(Installed::Managed);
        execute(&h, &Command::Uninstall).unwrap();
        assert!(h.runs.borrow().is_empty());
    }

    #[test]
    fn an_uninstall_with_the_flag_removes_the_agent_and_then_the_flag() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.commands(), ["uninstall"]);
        assert!(!h.flag().exists());
    }

    #[test]
    fn removing_the_msi_does_not_remove_an_agent_that_another_build_took_over() {
        let h = Fake::new(Installed::Managed);
        taken_over_by(&h, b"version=3.6.0\nbrand=acme\n");
        execute(&h, &Command::Uninstall).unwrap();
        assert!(h.runs.borrow().is_empty(), "the agent is not removed");
        assert!(!h.flag().exists(), "the flag has nothing left to say");
        assert!(h.logged().contains("another build"), "{}", h.logged());
        // a marker nobody can read is not known to be a brand's: the .msi that wrote the flag
        // still cleans the machine it installed (a refresh stays refused, see the next test)
        let h = Fake::new(Installed::Managed);
        taken_over_by(&h, b"\xff\xfe junk");
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.commands(), ["uninstall"]);
        assert!(!h.flag().exists());
        // the build of that brand removes its own, and a stock marker is the stock build's
        let mut h = Fake::new(Installed::Managed);
        h.brand = Some("acme".into());
        taken_over_by(&h, b"brand=acme\n");
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.commands(), ["uninstall"]);
        let h = Fake::new(Installed::Managed);
        taken_over_by(&h, b"version=3.6.0\n");
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.commands(), ["uninstall"]);
    }

    #[test]
    fn a_removal_waits_for_a_print_like_the_install_and_then_goes_on_whatever_it_found() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.printing_looks.set(2);
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.commands(), ["uninstall"]);
        assert_eq!(h.slept.get(), 3);

        // a print that does not end must not make the removal of the .msi fail: it stops waiting
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.printing_looks.set(u32::MAX);
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.slept.get(), QUIET_WAIT_SECS);
        assert_eq!(h.commands(), ["uninstall"]);
        assert!(!h.flag().exists());
        assert!(h.logged().contains("managed-uninstall"), "{}", h.logged());
        assert!(h.logged().contains("printing"), "{}", h.logged());
        // the removal is not "stopped, try again" like an install: it went on, and the job may be
        // printed again
        assert!(h.logged().contains("removed anyway"), "{}", h.logged());
        assert!(h.logged().contains("printed again"), "{}", h.logged());
        assert!(!h.logged().contains("try again"), "{}", h.logged());
        assert!(!h.logged().contains("stopped"), "{}", h.logged());
    }

    #[test]
    fn a_removal_over_a_print_that_ends_at_the_last_second_logs_nothing() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.printing_looks.set(QUIET_WAIT_SECS as u32);
        execute(&h, &Command::Uninstall).unwrap();
        assert_eq!(h.commands(), ["uninstall"]);
        assert!(!h.logged().contains("printing"), "{}", h.logged());
    }

    #[test]
    fn a_removal_that_has_nothing_to_remove_does_not_look() {
        for detect in [Installed::None, Installed::Foreign] {
            let h = Fake::new(detect);
            std::fs::write(h.flag(), b"").unwrap();
            h.printing_looks.set(u32::MAX);
            execute(&h, &Command::Uninstall).unwrap();
            assert_eq!((h.looks.get(), h.slept.get()), (0, 0), "{detect:?}");
        }
    }

    #[test]
    fn a_failed_removal_keeps_the_flag_logs_and_fails() {
        let h = Fake::new(Installed::Managed);
        std::fs::write(h.flag(), b"").unwrap();
        h.reply(failed("system", "the folder is in use", false));
        let e = execute(&h, &Command::Uninstall).unwrap_err();
        assert!(e.contains("in use"));
        assert!(h.flag().is_file());
        assert!(h.logged().contains("managed-uninstall"));
    }

    #[test]
    fn an_uninstall_never_fails_for_an_agent_that_is_gone_or_is_not_ours() {
        for detect in [Installed::None, Installed::Foreign] {
            let h = Fake::new(detect);
            std::fs::write(h.flag(), b"").unwrap();
            execute(&h, &Command::Uninstall).unwrap();
            assert!(h.runs.borrow().is_empty(), "{detect:?}");
            assert!(h.logged().contains("left alone"), "{detect:?}");
            assert!(!h.flag().exists(), "a flag with nothing behind it goes");
        }
    }

    // ---- 12. where the program runs from

    #[test]
    fn the_program_runs_only_from_inside_program_files() {
        let mut h = Fake::new(Installed::None);
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        h.own = h.root.path().join("Temp").join("x");
        std::fs::create_dir_all(&h.own).unwrap();
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(e.contains("Program Files"), "{e}");
        assert!(h.runs.borrow().is_empty());
        for command in [Command::Refresh, Command::Uninstall] {
            assert!(execute(&h, &command).is_err());
        }
        assert!(under(
            Path::new("C:/Program Files/printpapi Agent Setup"),
            Path::new("c:/program files")
        ));
        assert!(under(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!under(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!under(Path::new("/a"), Path::new("/a/b")));
        assert!(!under(Path::new("/a"), Path::new("")));
    }

    // ---- 13. no secret anywhere

    #[test]
    fn the_key_is_in_no_argument_no_error_no_log_and_no_staged_file_after_the_run() {
        let h = Fake::new(Installed::None);
        h.reply(failed(
            "system",
            &format!("agent.ini: api_key = {KEY} is not valid"),
            true,
        ));
        let dir = h.prepared("SHOP-PC.ini", &ini("name = a\n"));
        let e = execute(&h, &install_of(&dir)).unwrap_err();
        assert!(!e.contains(KEY), "{e}");
        assert!(!h.logged().contains(KEY));
        for run in h.runs.borrow().iter() {
            assert!(run.args.iter().all(|a| !a.contains(KEY)), "{:?}", run.args);
        }
        assert_eq!(std::fs::read_dir(h.work()).unwrap().count(), 0);
        assert_eq!(scrub("a KEY b\r\nc", Some("KEY")), "a *** b  c");
    }

    /// `dir_owner_sid` is named for folders but asks Windows about any path: the owner of a file is
    /// a SID, and a file made by a test belongs to somebody (no prepared file is ever "ownerless").
    #[cfg(windows)]
    #[test]
    fn the_real_owner_lookup_works_on_a_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let sid = Machine.owner_sid(file.path()).unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert_eq!(
            trusted_windows_owner(&sid),
            sid == "S-1-5-18" || sid == "S-1-5-32-544"
        );
        assert!(
            Machine
                .owner_sid(&file.path().with_extension("missing"))
                .is_err()
        );
        // and on the folder that holds it, which the install now asks for too
        let folder = Machine.owner_sid(file.path().parent().unwrap()).unwrap();
        assert!(folder.starts_with("S-1-5-"), "{folder}");
    }
}
