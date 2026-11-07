// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! What installing the agent means on each platform, as data and pure functions: the folder
//! layout, the autostart file (Task Scheduler XML, systemd unit, launchd plist) and the commands
//! that stop, start and remove it. Nothing here touches the machine; `apply` runs the commands
//! through the injected [`Runner`], never a shell.
//!
//! Every function takes the target [`Os`] as a value, so the texts of all three systems are tested
//! on every host. Paths are therefore built as strings with the separator of the target.

use std::path::{Path, PathBuf};

use crate::proc::{Cmd, Runner};

/// Name of the Task Scheduler task.
pub const TASK_NAME: &str = "printpapi-agent";
/// launchd label.
pub const LAUNCHD_LABEL: &str = "com.printpapi.agent";
/// Linux service user. Not `printpapi`: `docs/migration.md` runs the server as that user, and the
/// agent must not share its identity.
pub const SERVICE_USER: &str = "printpapi-agent";
/// Name of the install folder. Not `printpapi`: the server may live in `/opt/printpapi`
/// (`docs/migration.md`), and the removal of the agent must never reach it.
pub const INSTALL_DIR_NAME: &str = "printpapi-agent";
/// File that tells a later run of the setup "this installation is mine".
pub const MARKER_NAME: &str = ".installed-by-setup";
/// Name of the registry value (Windows) that starts the tray icon at every logon.
pub const TRAY_RUN_VALUE: &str = "printpapi-tray";
/// launchd label of the tray icon's LaunchAgent.
pub const TRAY_LAUNCHD_LABEL: &str = "com.printpapi.tray";
/// The executable inside the macOS `.app`. Tauri 2 names it after the Cargo binary
/// (`printpapi-setup`), not after `productName` ("the binary name is no longer renamed to match
/// `productName`", https://v2.tauri.app/start/migrate/from-tauri-1/).
const MACOS_TRAY_EXE: &str = "printpapi-setup";
const WIN_RUN_KEY: &str = "HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const SID_USERS: &str = "*S-1-5-32-545";

pub(crate) const SID_SYSTEM: &str = "*S-1-5-18";
pub(crate) const SID_ADMINISTRATORS: &str = "*S-1-5-32-544";
const CMD_TIMEOUT: u64 = 60;
/// Exists while systemd is the running init (see sd_booted(3)).
const SYSTEMD_RUN_DIR: &str = "/run/systemd/system";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Linux,
    Macos,
}

impl Os {
    pub fn host() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }

    fn sep(self) -> char {
        if self == Os::Windows { '\\' } else { '/' }
    }
}

/// Where everything lives on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub dir: PathBuf,
    pub binary: PathBuf,
    pub ini: PathBuf,
    pub marker: PathBuf,
    pub sumatra: PathBuf,
    pub autostart: PathBuf,
    /// Where the agent writes `status.json` and its crash log, and the tray icon reads them.
    pub status_dir: PathBuf,
    /// The tray icon program: a file, on macOS the `.app` folder.
    pub tray: PathBuf,
    /// The file that starts the tray icon at logon. Windows uses the registry (`None` here).
    pub tray_autostart: Option<PathBuf>,
    /// Linux only: the application menu entry that opens the window without the icon.
    pub app_entry: Option<PathBuf>,
}

/// `base` + `name` with the separator of `os`, whatever the host's own is.
fn join(os: Os, base: &Path, name: &str) -> PathBuf {
    let base = base.to_string_lossy();
    let base = base.trim_end_matches(['/', '\\']);
    PathBuf::from(format!("{base}{}{name}", os.sep()))
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The folders and files of an installation on this machine: [`layout_with`] with the real
/// `ProgramData` folder.
pub fn layout(os: Os, program_files: &Path) -> Layout {
    layout_with(os, program_files, &crate::status::program_data())
}

/// The folders and files of an installation. `program_files` is `%ProgramFiles%` and
/// `program_data` is `%ProgramData%`; both only matter on Windows. macOS and Linux install to
/// `/opt/printpapi-agent`.
///
/// The places of the tray icon's autostart follow the platforms' own rules:
/// - Windows: the `Run` key, https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys
/// - Linux: `$XDG_CONFIG_DIRS/autostart`, `/etc/xdg/autostart` by default,
///   https://specifications.freedesktop.org/autostart/latest/ ; the menu entry belongs in
///   `/usr/share/applications`, https://specifications.freedesktop.org/desktop-entry/latest/
/// - macOS: `/Library/LaunchAgents` holds agents for all users,
///   https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html
pub fn layout_with(os: Os, program_files: &Path, program_data: &Path) -> Layout {
    let dir = match os {
        Os::Windows => join(os, program_files, INSTALL_DIR_NAME),
        Os::Linux | Os::Macos => PathBuf::from(format!("/opt/{INSTALL_DIR_NAME}")),
    };
    let binary_name = if os == Os::Windows {
        "printpapi-agent.exe"
    } else {
        "printpapi-agent"
    };
    let autostart = match os {
        Os::Windows => join(os, &dir, "printpapi-agent-task.xml"),
        Os::Linux => PathBuf::from("/etc/systemd/system/printpapi-agent.service"),
        Os::Macos => PathBuf::from("/Library/LaunchDaemons/com.printpapi.agent.plist"),
    };
    let tray = match os {
        Os::Windows => join(os, &dir, "printpapi-tray.exe"),
        Os::Linux => join(os, &dir, "printpapi-tray"),
        Os::Macos => PathBuf::from("/Applications/printpapi Agent.app"),
    };
    let tray_autostart = match os {
        Os::Windows => None,
        Os::Linux => Some(PathBuf::from("/etc/xdg/autostart/printpapi-tray.desktop")),
        Os::Macos => Some(PathBuf::from(
            "/Library/LaunchAgents/com.printpapi.tray.plist",
        )),
    };
    let app_entry =
        (os == Os::Linux).then(|| PathBuf::from("/usr/share/applications/printpapi-agent.desktop"));
    Layout {
        binary: join(os, &dir, binary_name),
        ini: join(os, &dir, "agent.ini"),
        marker: join(os, &dir, MARKER_NAME),
        sumatra: join(os, &dir, "SumatraPDF.exe"),
        autostart,
        status_dir: crate::status::status_dir_in(os, program_data),
        tray,
        tray_autostart,
        app_entry,
        dir,
    }
}

/// Where `%ProgramFiles%` points on a standard installation, and what is used when Windows cannot
/// say (and off Windows, where `layout` has no use for it).
const DEFAULT_PROGRAM_FILES: &str = "C:\\Program Files";

/// The folder the Windows agent is installed under. It is asked of Windows
/// (`SHGetKnownFolderPath(FOLDERID_ProgramFiles)`), not of the environment: the install runs with
/// administrator rights, and whoever started it can hand it an environment variable of their own.
/// https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath
// ponytail: this is the 64-bit `Program Files`, which is what an x86_64 build gets. A 32-bit build
// under WOW64 would be given `Program Files (x86)` by this call, and `FOLDERID_ProgramFilesX64`
// would be the one to ask for; there is no such build.
pub fn program_files() -> PathBuf {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Com::CoTaskMemFree;
        use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: `raw` is only read when the call succeeded, as a null-terminated string that
        // this call allocated; it is freed once, whether or not the call succeeded (see the docs).
        let found = unsafe {
            let hr =
                SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut raw);
            let path = (hr >= 0 && !raw.is_null()).then(|| {
                let len = (0..).take_while(|&n| *raw.add(n) != 0).count();
                wide_to_path(std::slice::from_raw_parts(raw, len))
            });
            CoTaskMemFree(raw.cast());
            path
        };
        found.unwrap_or_else(|| PathBuf::from(DEFAULT_PROGRAM_FILES))
    }
    #[cfg(not(windows))]
    PathBuf::from(DEFAULT_PROGRAM_FILES)
}

#[cfg(windows)]
pub(crate) fn wide_to_path(wide: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_wide(wide))
}

/// The folder with the system tools: `System32` on Windows, else `/`. Windows is asked
/// (`GetSystemDirectoryW`), not the `SystemRoot` environment variable, for the reason given at
/// [`program_files`].
/// https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getsystemdirectoryw
// ponytail: a 32-bit build under WOW64 would get `SysWOW64` here; the agent is x86_64. When the
// call fails (it does not on a working system), `C:\Windows\System32` is used.
pub fn system_dir() -> PathBuf {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
        let mut buf = vec![0u16; 260];
        loop {
            // SAFETY: the buffer is as long as the length passed
            let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
            if n == 0 {
                return PathBuf::from("C:\\Windows\\System32");
            }
            if n < buf.len() {
                return wide_to_path(&buf[..n]);
            }
            // too short: `n` is the length needed, with the terminator
            buf.resize(n, 0);
        }
    }
    #[cfg(not(windows))]
    PathBuf::from("/")
}

/// Absolute path of a system tool. Rust's `Command` looks in the program's own folder first on
/// Windows, and that folder (the staging folder) is writable by users.
fn tool(os: Os, sys: &Path, name: &str) -> String {
    s(&join(os, sys, name))
}

fn cmd(argv: Vec<String>) -> Cmd {
    Cmd::new(argv, CMD_TIMEOUT)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Task Scheduler XML, UTF-16 with a byte order mark, which is what `schtasks /xml` expects.
/// `ExecutionTimeLimit` `PT0S` matters: without it Windows stops the task after 72 hours
/// (https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-executiontimelimit).
/// The element names and their values follow the schema at
/// https://learn.microsoft.com/en-us/windows/win32/taskschd/task-scheduler-schema.
fn task_xml(l: &Layout) -> Vec<u8> {
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>printpapi agent</Description>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <Enabled>true</Enabled>
    </BootTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>S-1-5-18</UserId>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>255</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
    </Exec>
  </Actions>
</Task>
"#,
        xml_escape(&s(&l.binary))
    )
    .replace('\n', "\r\n");
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Like `docs/agent.md#run-as-a-service`, with the setup's own folder and user.
fn systemd_unit(l: &Layout) -> String {
    format!(
        "[Unit]\nDescription=printpapi agent\nAfter=network-online.target cups.service\n\n\
         [Service]\nExecStart={}\nRestart=always\nRestartSec=5\nUser={SERVICE_USER}\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        s(&l.binary)
    )
}

/// Like `docs/agent.md#run-as-a-service`, with the setup's own folder.
fn launchd_plist(l: &Layout) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/var/log/printpapi-agent.log</string>
  <key>StandardErrorPath</key><string>/var/log/printpapi-agent.log</string>
</dict>
</plist>
"#,
        xml_escape(&s(&l.binary))
    )
}

/// The bytes of the autostart file at `layout.autostart`.
pub fn autostart_file(os: Os, l: &Layout) -> Vec<u8> {
    match os {
        Os::Windows => task_xml(l),
        Os::Linux => systemd_unit(l).into_bytes(),
        Os::Macos => launchd_plist(l).into_bytes(),
    }
}

/// Stop a running agent. The caller ignores failures: there may be nothing to stop.
pub fn stop_cmds(os: Os, _l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Windows => vec![cmd(vec![
            tool(os, sys, "schtasks.exe"),
            "/end".into(),
            "/tn".into(),
            TASK_NAME.into(),
        ])],
        Os::Linux => vec![cmd(argv(&["systemctl", "stop", "printpapi-agent"]))],
        Os::Macos => vec![cmd(vec![
            tool(os, sys, "bin/launchctl"),
            "bootout".into(),
            format!("system/{LAUNCHD_LABEL}"),
        ])],
    }
}

/// Linux: does the service user exist? `None` on the other systems.
pub fn identity_check(os: Os) -> Option<Cmd> {
    (os == Os::Linux).then(|| cmd(argv(&["id", "-u", SERVICE_USER])))
}

/// Is the agent still running after `stop_cmds`? Success of the command means it is. `None` on
/// Windows: a running exe is locked there, so deleting its folder fails anyway.
pub fn running_check(os: Os) -> Option<Cmd> {
    match os {
        Os::Linux => Some(cmd(argv(&[
            "systemctl",
            "is-active",
            "--quiet",
            "printpapi-agent",
        ]))),
        // after `bootout` the label is gone, so `print` fails; it succeeds while the job is loaded
        Os::Macos => Some(cmd(vec![
            "/bin/launchctl".into(),
            "print".into(),
            format!("system/{LAUNCHD_LABEL}"),
        ])),
        Os::Windows => None,
    }
}

/// Create what `identity_check` found missing: the Linux system user.
pub fn identity_cmds(os: Os) -> Vec<Cmd> {
    match os {
        Os::Linux => vec![cmd(argv(&[
            "useradd",
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            SERVICE_USER,
        ]))],
        Os::Windows | Os::Macos => Vec::new(),
    }
}

/// Can the setup install here at all? Linux needs systemd.
pub fn supported(os: Os) -> bool {
    supported_at(os, Path::new(SYSTEMD_RUN_DIR))
}

/// `supported` with the path that proves systemd is running as a parameter.
fn supported_at(os: Os, systemd_dir: &Path) -> bool {
    match os {
        Os::Linux => systemd_dir.exists(),
        Os::Windows | Os::Macos => true,
    }
}

/// Restrict `ini`, an `agent.ini` or the new file that is about to become one (it holds the key): SYSTEM and Administrators on Windows,
/// `root:printpapi-agent` 0640 on Linux, root 0600 on macOS. Windows uses SIDs, because group names are localised
/// ("Administratoren" on a German Windows). icacls takes a SID with a leading `*`
/// (https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/icacls).
pub fn ini_acl_cmds(os: Os, ini: &Path, sys: &Path) -> Vec<Cmd> {
    let ini = s(ini);
    match os {
        Os::Windows => vec![cmd(vec![
            tool(os, sys, "icacls.exe"),
            ini,
            "/inheritance:r".into(),
            "/grant:r".into(),
            format!("{SID_SYSTEM}:F"),
            format!("{SID_ADMINISTRATORS}:F"),
        ])],
        Os::Linux => vec![
            cmd(vec![
                "chown".into(),
                format!("root:{SERVICE_USER}"),
                ini.clone(),
            ]),
            cmd(vec!["chmod".into(), "640".into(), ini]),
        ],
        Os::Macos => vec![cmd(vec![tool(os, sys, "bin/chmod"), "600".into(), ini])],
    }
}

/// Register the autostart and start the agent. On macOS the first command (`xattr -d`) fails when
/// the file carries no quarantine attribute, so the caller must not treat its failure as fatal.
pub fn start_cmds(os: Os, l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Windows => {
            let schtasks = tool(os, sys, "schtasks.exe");
            vec![
                cmd(vec![
                    schtasks.clone(),
                    "/create".into(),
                    "/tn".into(),
                    TASK_NAME.into(),
                    "/xml".into(),
                    s(&l.autostart),
                    "/f".into(),
                ]),
                cmd(vec![
                    schtasks,
                    "/run".into(),
                    "/tn".into(),
                    TASK_NAME.into(),
                ]),
            ]
        }
        Os::Linux => vec![
            cmd(argv(&["systemctl", "daemon-reload"])),
            cmd(argv(&["systemctl", "enable", "--now", "printpapi-agent"])),
        ],
        Os::Macos => vec![
            cmd(vec![
                tool(os, sys, "usr/bin/xattr"),
                "-d".into(),
                "com.apple.quarantine".into(),
                s(&l.binary),
            ]),
            cmd(vec![
                tool(os, sys, "bin/launchctl"),
                "bootstrap".into(),
                "system".into(),
                s(&l.autostart),
            ]),
        ],
    }
}

/// Take the autostart out, after `stop_cmds`. The caller deletes the autostart file and the
/// installation folder afterwards. macOS needs nothing here: `bootout` in `stop_cmds` already
/// unloaded the daemon.
pub fn remove_cmds(os: Os, _l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Windows => vec![cmd(vec![
            tool(os, sys, "schtasks.exe"),
            "/delete".into(),
            "/tn".into(),
            TASK_NAME.into(),
            "/f".into(),
        ])],
        Os::Linux => vec![cmd(argv(&["systemctl", "disable", "printpapi-agent"]))],
        Os::Macos => Vec::new(),
    }
}

/// `arg` as one argument of a Desktop Entry `Exec` line: in double quotes, with `"`, `` ` ``, `$` and
/// `\` escaped by a backslash, then every backslash doubled again because the value is a string
/// first (the spec asks for four in the file), and `%` as `%%` so it is no field code.
/// https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
fn desktop_exec_quote(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '\\' => out.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Desktop Entry file that runs the tray program with `extra` arguments.
fn desktop_entry(l: &Layout, extra: &str) -> Vec<u8> {
    format!(
        "[Desktop Entry]\nType=Application\nName=printpapi Agent\nComment=printpapi agent status and settings\nExec={}{extra}\nTerminal=false\n",
        desktop_exec_quote(&s(&l.tray))
    )
    .into_bytes()
}

/// launchd plist of the tray icon. `LimitLoadToSessionType` `Aqua` loads it only into a graphical
/// login session (launchd.plist(5)). No `KeepAlive`: "Quit" in the menu must stay quit.
fn tray_launchd_plist(l: &Layout) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{TRAY_LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}/Contents/MacOS/{MACOS_TRAY_EXE}</string>
    <string>--tray</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict>
</plist>
"#,
        xml_escape(&s(&l.tray))
    )
}

/// The bytes of the file at `layout.tray_autostart`: the Linux autostart entry or the macOS
/// LaunchAgent. `None` on Windows, which uses [`tray_register_cmds`].
pub fn tray_autostart_file(os: Os, l: &Layout) -> Option<Vec<u8>> {
    match os {
        Os::Windows => None,
        Os::Linux => Some(desktop_entry(l, " --tray")),
        Os::Macos => Some(tray_launchd_plist(l).into_bytes()),
    }
}

/// Linux: the entry in the application menu. It opens the window directly (no `--tray`), so the
/// settings stay reachable on a desktop that shows no tray icons (GNOME without AppIndicator).
pub fn app_entry_file(l: &Layout) -> Vec<u8> {
    desktop_entry(l, "")
}

/// Windows: make the tray icon start at every logon, with a value in the `Run` key of all users
/// (`HKLM`). The data is the command line, `"<tray>" --tray`.
/// https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/reg-add
pub fn tray_register_cmds(os: Os, l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Windows => vec![cmd(vec![
            tool(os, sys, "reg.exe"),
            "add".into(),
            WIN_RUN_KEY.into(),
            "/v".into(),
            TRAY_RUN_VALUE.into(),
            "/t".into(),
            "REG_SZ".into(),
            "/d".into(),
            format!("\"{}\" --tray", s(&l.tray)),
            "/f".into(),
        ])],
        Os::Linux | Os::Macos => Vec::new(),
    }
}

/// The reverse of [`tray_register_cmds`]. The caller ignores a failure: the value may be gone.
pub fn tray_unregister_cmds(os: Os, _l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Windows => vec![cmd(vec![
            tool(os, sys, "reg.exe"),
            "delete".into(),
            WIN_RUN_KEY.into(),
            "/v".into(),
            TRAY_RUN_VALUE.into(),
            "/f".into(),
        ])],
        Os::Linux | Os::Macos => Vec::new(),
    }
}

/// End the tray icons of all sessions, before the program is replaced or removed (a running exe
/// is locked on Windows). The caller ignores failures: there may be none running.
/// https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/taskkill
/// (`/im` names the image, `/f` ends it forcefully); `pkill -f` matches the whole command line.
// ponytail: this also ends a settings window of the same program, it is the same image and path.
// Whoever starts an install from that window must not stage a `tray` (then nothing is stopped), and
// a removal from it ends the window by design, because its exe is locked on Windows anyway.
pub fn tray_stop_cmds(os: Os, l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Windows => vec![cmd(vec![
            tool(os, sys, "taskkill.exe"),
            "/f".into(),
            "/im".into(),
            "printpapi-tray.exe".into(),
        ])],
        Os::Linux => vec![cmd(vec!["pkill".into(), "-f".into(), s(&l.tray)])],
        Os::Macos => {
            // the full path of the app's programs, as on Linux; `pkill -f` takes a regex, so the
            // dot of ".app" is escaped, and a copy elsewhere (Downloads) or an editor that has the
            // bundle open is not hit
            let pattern = format!("{}/Contents/MacOS/", s(&l.tray).replace('.', r"\."));
            vec![cmd(vec![
                tool(os, sys, "usr/bin/pkill"),
                "-f".into(),
                pattern,
            ])]
        }
    }
}

/// The rights of `layout.status_dir`, set after the folder exists: readable by every local user,
/// writable only by whoever runs the agent. On Windows by SID, as in [`ini_acl_cmds`]: the owner
/// is taken over (Administrators), then inheritance is cut and SYSTEM and Administrators get full
/// access (inherited by what is created inside) and Users read and execute. Two commands, because
/// icacls refuses `/setowner` together with `/inheritance` or `/grant` in one call (checked on
/// Windows 11: "Ungültiger Parameter", exit 87); the docs list them in separate syntax lines.
/// https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/icacls
pub fn status_dir_cmds(os: Os, l: &Layout, sys: &Path) -> Vec<Cmd> {
    let dir = s(&l.status_dir);
    match os {
        Os::Windows => vec![
            cmd(vec![
                tool(os, sys, "icacls.exe"),
                dir.clone(),
                "/setowner".into(),
                SID_ADMINISTRATORS.into(),
            ]),
            cmd(vec![
                tool(os, sys, "icacls.exe"),
                dir,
                "/inheritance:r".into(),
                "/grant:r".into(),
                format!("{SID_SYSTEM}:(OI)(CI)F"),
                format!("{SID_ADMINISTRATORS}:(OI)(CI)F"),
                format!("{SID_USERS}:(OI)(CI)RX"),
            ]),
        ],
        Os::Linux => vec![
            cmd(vec!["chown".into(), SERVICE_USER.into(), dir.clone()]),
            cmd(vec!["chmod".into(), "755".into(), dir]),
        ],
        Os::Macos => vec![
            cmd(vec![
                tool(os, sys, "usr/sbin/chown"),
                "root:wheel".into(),
                dir.clone(),
            ]),
            cmd(vec![tool(os, sys, "bin/chmod"), "755".into(), dir]),
        ],
    }
}

/// macOS: put the `.app` folder `from` at `layout.tray`. `ditto` copies a bundle with its
/// metadata; what it does with the owner of the source is not relied on, so the copy is handed to
/// root explicitly, made unwritable for others, and freed of the quarantine flag (the second
/// command, which the caller lets fail: a local copy may not carry it). Empty elsewhere, where the
/// tray is one file that is written directly.
pub fn tray_copy_cmds(os: Os, from: &Path, l: &Layout, sys: &Path) -> Vec<Cmd> {
    match os {
        Os::Macos => {
            let app = s(&l.tray);
            vec![
                cmd(vec![tool(os, sys, "usr/bin/ditto"), s(from), app.clone()]),
                cmd(vec![
                    tool(os, sys, "usr/bin/xattr"),
                    "-dr".into(),
                    "com.apple.quarantine".into(),
                    app.clone(),
                ]),
                cmd(vec![
                    tool(os, sys, "usr/sbin/chown"),
                    "-R".into(),
                    "root:wheel".into(),
                    app.clone(),
                ]),
                cmd(vec![
                    tool(os, sys, "bin/chmod"),
                    "-R".into(),
                    "go-w".into(),
                    app,
                ]),
            ]
        }
        Os::Windows | Os::Linux => Vec::new(),
    }
}

/// Who owns the agent on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    None,
    /// Installed by this setup (the marker exists).
    Managed,
    /// Something is there but the setup did not put it there. Installing on top would run two
    /// agents and print twice.
    Foreign,
}

/// Is an agent installed, and is it ours? The marker decides first. Without it, an autostart file
/// or a running process called `printpapi-agent` means somebody set it up by hand.
///
/// Call this from the setup program, never from a process that is itself named `printpapi-agent`
/// (the admin subcommands): on Linux and macOS it would find itself. The same goes for any process
/// whose name starts with the 15 characters `printpapi-agent`: Linux keeps only that much of the
/// name in `comm`, so `pgrep -x` matches a `printpapi-agent-setup` too. The setup binary is
/// therefore called `printpapi-setup`.
pub fn detect(os: Os, l: &Layout, sys: &Path, run: &Runner) -> Installed {
    if l.marker.exists() {
        return Installed::Managed;
    }
    if l.autostart.exists() {
        return Installed::Foreign;
    }
    let found = if os == Os::Windows {
        windows_agent_present(os, sys, run)
    } else {
        // `-x` matches the whole process name; "printpapi-agent" is 15 characters, which is all
        // that Linux keeps in `comm`. A missing `pgrep` errors like "not running": the install
        // must not be blocked because the check cannot be made.
        let program = if os == Os::Macos {
            tool(os, sys, "usr/bin/pgrep")
        } else {
            "pgrep".to_string()
        };
        let mut pgrep = cmd(vec![program, "-x".into(), "printpapi-agent".into()]);
        pgrep.capture = true;
        run(&pgrep).is_ok()
    };
    if found {
        Installed::Foreign
    } else {
        Installed::None
    }
}

/// Windows: something called `printpapi-agent` is registered or running. Four looks, because the
/// setup asks without administrator rights and each look is blind to something:
/// - the task file `System32\Tasks\printpapi-agent`: a task made from an elevated shell with
///   `/ru SYSTEM` (docs/agent.md) is invisible to `schtasks /query` for a normal user, but its
///   file name is not. Anything but a clean "not found" counts as there: the file of a task the
///   user may not read answers "access denied", not "missing".
/// - `schtasks /query`, for a task the file check cannot see;
/// - `sc query`, for the service of `nssm install printpapi-agent ...` (exit 1060 when missing);
/// - `tasklist`, for an agent started by hand (the Python agent runs as `pythonw.exe` and is
///   not found this way).
// ponytail: `sc query` answers "access denied" (exit 5) for a service whose ACL hides it from
// users; that counts as "not there". An nssm service has the default ACL, which allows the query.
fn windows_agent_present(os: Os, sys: &Path, run: &Runner) -> bool {
    // host-native join on purpose: this reads the real disk, which only matters on Windows
    if task_file_present(sys.join("Tasks").join(TASK_NAME).try_exists()) {
        return true;
    }
    let mut query = cmd(vec![
        tool(os, sys, "schtasks.exe"),
        "/query".into(),
        "/tn".into(),
        TASK_NAME.into(),
    ]);
    query.capture = true;
    if run(&query).is_ok() {
        return true;
    }
    let mut service = cmd(vec![
        tool(os, sys, "sc.exe"),
        "query".into(),
        TASK_NAME.into(),
    ]);
    service.capture = true;
    if run(&service).is_ok() {
        return true;
    }
    let mut list = cmd(vec![
        tool(os, sys, "tasklist.exe"),
        "/fi".into(),
        "IMAGENAME eq printpapi-agent.exe".into(),
        "/fo".into(),
        "csv".into(),
        "/nh".into(),
    ]);
    list.capture = true;
    matches!(run(&list), Ok(out) if String::from_utf8_lossy(&out)
        .to_ascii_lowercase()
        .contains("printpapi-agent.exe"))
}

/// `Path::try_exists` on the task file: only a definite "no" means there is no task. A task that
/// was created by an elevated user gives `Err(PermissionDenied)` to everybody else, and that
/// means it exists. (`Path::exists` would turn the error into `false`.)
fn task_file_present(found: std::io::Result<bool>) -> bool {
    !matches!(found, Ok(false))
}

/// The log of the installed agent, for "see the log" in an error message: the crash log next to
/// the status file (see `status::status_dir_in`; `program_data` is only used on Windows). On
/// macOS the plist's `StandardErrorPath` file holds what the agent prints to the console as well.
pub fn service_log_path(os: Os, program_data: &Path) -> PathBuf {
    join(
        os,
        &crate::status::status_dir_in(os, program_data),
        crate::log::LOG_NAME,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    const WIN_SYS: &str = "C:\\Windows\\System32";

    fn win_layout(program_files: &str) -> Layout {
        layout(Os::Windows, Path::new(program_files))
    }

    fn mac() -> Layout {
        layout(Os::Macos, Path::new("/unused"))
    }

    fn linux() -> Layout {
        layout(Os::Linux, Path::new("/unused"))
    }

    fn argvs(cmds: &[Cmd]) -> Vec<Vec<String>> {
        cmds.iter().map(|c| c.argv.clone()).collect()
    }

    fn utf16(bytes: &[u8]) -> String {
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "byte order mark");
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    fn p(l: &Path) -> String {
        l.to_string_lossy().into_owned()
    }

    #[test]
    fn the_agent_shares_neither_folder_nor_user_with_the_server() {
        // docs/migration.md runs the server from /opt/printpapi as the user `printpapi`
        assert_ne!(SERVICE_USER, "printpapi");
        for l in [linux(), mac()] {
            assert_ne!(p(&l.dir), "/opt/printpapi");
            assert!(!l.binary.starts_with("/opt/printpapi/"));
        }
        assert!(!p(&win_layout("C:\\Program Files").dir).ends_with("\\printpapi"));
    }

    #[test]
    fn layouts_per_platform() {
        let w = win_layout("C:\\Program Files");
        assert_eq!(p(&w.dir), "C:\\Program Files\\printpapi-agent");
        assert_eq!(
            p(&w.binary),
            "C:\\Program Files\\printpapi-agent\\printpapi-agent.exe"
        );
        assert_eq!(p(&w.ini), "C:\\Program Files\\printpapi-agent\\agent.ini");
        assert_eq!(
            p(&w.marker),
            "C:\\Program Files\\printpapi-agent\\.installed-by-setup"
        );
        assert_eq!(
            p(&w.sumatra),
            "C:\\Program Files\\printpapi-agent\\SumatraPDF.exe"
        );
        assert_eq!(
            p(&w.autostart),
            "C:\\Program Files\\printpapi-agent\\printpapi-agent-task.xml"
        );
        // a trailing backslash does not double up
        assert_eq!(p(&win_layout("C:\\PF\\").dir), "C:\\PF\\printpapi-agent");
        let l = linux();
        assert_eq!(p(&l.dir), "/opt/printpapi-agent");
        assert_eq!(p(&l.binary), "/opt/printpapi-agent/printpapi-agent");
        assert_eq!(p(&l.ini), "/opt/printpapi-agent/agent.ini");
        assert_eq!(p(&l.marker), "/opt/printpapi-agent/.installed-by-setup");
        assert_eq!(
            p(&l.autostart),
            "/etc/systemd/system/printpapi-agent.service"
        );
        let m = mac();
        assert_eq!(p(&m.binary), "/opt/printpapi-agent/printpapi-agent");
        assert_eq!(
            p(&m.autostart),
            "/Library/LaunchDaemons/com.printpapi.agent.plist"
        );
    }

    #[test]
    fn task_xml_never_times_out() {
        let bytes = autostart_file(Os::Windows, &win_layout("C:\\Program Files"));
        let text = utf16(&bytes);
        for needle in [
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<UserId>S-1-5-18</UserId>",
            "<BootTrigger>\r\n      <Enabled>true</Enabled>\r\n    </BootTrigger>",
            "<Count>255</Count>",
            "<AllowStartOnDemand>true</AllowStartOnDemand>",
            "<Actions Context=\"Author\">",
            "<Principal id=\"Author\">",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<RestartOnFailure>",
            "<Interval>PT1M</Interval>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<RunLevel>HighestAvailable</RunLevel>",
            "<Description>printpapi agent</Description>",
            "<AllowHardTerminate>true</AllowHardTerminate>",
            // the task must be enabled, or it never starts
            "    <Enabled>true</Enabled>\r\n    <ExecutionTimeLimit>",
        ] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
        assert!(text.contains(
            "<Command>C:\\Program Files\\printpapi-agent\\printpapi-agent.exe</Command>"
        ));
    }

    #[test]
    fn task_xml_escapes_the_path() {
        let text = utf16(&autostart_file(
            Os::Windows,
            &win_layout("C:\\Programme & Ü"),
        ));
        assert!(
            text.contains("C:\\Programme &amp; Ü\\printpapi-agent\\printpapi-agent.exe"),
            "{text}"
        );
    }

    #[test]
    fn windows_programs_are_absolute() {
        let l = win_layout("C:\\Program Files");
        let sys = Path::new(WIN_SYS);
        let mut all = stop_cmds(Os::Windows, &l, sys);
        all.extend(ini_acl_cmds(Os::Windows, &l.ini, sys));
        all.extend(start_cmds(Os::Windows, &l, sys));
        all.extend(remove_cmds(Os::Windows, &l, sys));
        assert!(all.len() >= 5);
        for c in all {
            assert!(c.argv[0].starts_with(WIN_SYS), "{:?}", c.argv);
            assert!(c.argv[0].ends_with(".exe"), "{:?}", c.argv);
        }
    }

    #[test]
    fn acl_uses_sids_not_names() {
        let l = win_layout("C:\\Program Files");
        let cmds = ini_acl_cmds(Os::Windows, &l.ini, Path::new(WIN_SYS));
        assert_eq!(cmds.len(), 1);
        let a = &cmds[0].argv;
        assert!(a[0].ends_with("icacls.exe"));
        // /grant:r replaces what is there; plain /grant would add to it
        assert_eq!(
            a[1..],
            [
                "C:\\Program Files\\printpapi-agent\\agent.ini",
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:F",
                "*S-1-5-32-544:F"
            ]
        );
        assert!(
            !a.iter()
                .any(|x| x.contains("Administrators") || x.contains("SYSTEM"))
        );
    }

    #[test]
    fn task_is_created_from_the_xml() {
        // a path with a space and an umlaut stays one argument
        let l = win_layout("C:\\Users\\Jürgen Müller\\Programme");
        let cmds = start_cmds(Os::Windows, &l, Path::new(WIN_SYS));
        assert_eq!(
            cmds[0].argv[1..],
            [
                "/create",
                "/tn",
                "printpapi-agent",
                "/xml",
                "C:\\Users\\Jürgen Müller\\Programme\\printpapi-agent\\printpapi-agent-task.xml",
                "/f"
            ]
        );
        assert_eq!(cmds[1].argv[1..], ["/run", "/tn", "printpapi-agent"]);
    }

    #[test]
    fn windows_stop_and_remove() {
        let l = win_layout("C:\\Program Files");
        let sys = Path::new(WIN_SYS);
        assert_eq!(
            stop_cmds(Os::Windows, &l, sys)[0].argv[1..],
            ["/end", "/tn", "printpapi-agent"]
        );
        assert_eq!(
            remove_cmds(Os::Windows, &l, sys)[0].argv[1..],
            ["/delete", "/tn", "printpapi-agent", "/f"]
        );
        assert!(identity_check(Os::Windows).is_none());
        assert!(identity_cmds(Os::Windows).is_empty());
    }

    #[test]
    fn running_check_asks_the_service_manager() {
        assert_eq!(
            running_check(Os::Linux).unwrap().argv,
            ["systemctl", "is-active", "--quiet", "printpapi-agent"]
        );
        assert_eq!(
            running_check(Os::Macos).unwrap().argv,
            ["/bin/launchctl", "print", "system/com.printpapi.agent"]
        );
        // a locked exe stops the removal on Windows anyway
        assert!(running_check(Os::Windows).is_none());
    }

    #[test]
    fn systemd_unit_runs_as_printpapi_agent() {
        let text = String::from_utf8(autostart_file(Os::Linux, &linux())).unwrap();
        for needle in [
            "User=printpapi-agent",
            "Restart=always",
            "RestartSec=5",
            "ExecStart=/opt/printpapi-agent/printpapi-agent",
            "After=network-online.target cups.service",
            "WantedBy=multi-user.target",
            "Description=printpapi agent",
        ] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
    }

    #[test]
    fn plist_loads_at_boot() {
        let text = String::from_utf8(autostart_file(Os::Macos, &mac())).unwrap();
        for needle in [
            "<key>Label</key><string>com.printpapi.agent</string>",
            "<key>RunAtLoad</key><true/>",
            "<key>KeepAlive</key><true/>",
            "<string>/opt/printpapi-agent/printpapi-agent</string>",
            "<key>StandardOutPath</key><string>/var/log/printpapi-agent.log</string>",
            "<key>StandardErrorPath</key><string>/var/log/printpapi-agent.log</string>",
        ] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
    }

    #[test]
    fn xml_escape_covers_the_five_characters() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("Ü plain"), "Ü plain");
    }

    /// No command may run into its timeout at once, and none may hang for ever.
    #[test]
    fn every_command_has_a_sane_timeout() {
        let sys = Path::new(WIN_SYS);
        for os in [Os::Windows, Os::Linux, Os::Macos] {
            let l = layout(os, Path::new("C:\\Program Files"));
            let mut all = stop_cmds(os, &l, sys);
            all.extend(ini_acl_cmds(os, &l.ini, sys));
            all.extend(start_cmds(os, &l, sys));
            all.extend(remove_cmds(os, &l, sys));
            all.extend(identity_cmds(os));
            all.extend(identity_check(os));
            all.extend(running_check(os));
            assert!(!all.is_empty());
            for c in all {
                assert!(
                    (Duration::from_secs(10)..=Duration::from_secs(300)).contains(&c.timeout),
                    "{:?}",
                    c
                );
            }
        }
    }

    /// The texts say "like docs/agent.md"; this keeps them from drifting apart. The manual steps
    /// there use `/opt/printpapi` and the user `printpapi`, the setup its own folder and user (so
    /// that removing the agent cannot reach a server in `/opt/printpapi`): those two are the only
    /// difference allowed.
    #[test]
    fn unit_and_plist_are_the_text_of_the_docs() {
        let doc = include_str!("../../../docs/agent.md")
            .replace("\r\n", "\n")
            .replace("/opt/printpapi/", "/opt/printpapi-agent/")
            .replace("User=printpapi\n", "User=printpapi-agent\n");
        let unit = String::from_utf8(autostart_file(Os::Linux, &linux())).unwrap();
        assert!(
            doc.contains(&unit),
            "systemd unit differs from docs/agent.md:\n{unit}"
        );
        let plist = String::from_utf8(autostart_file(Os::Macos, &mac())).unwrap();
        assert!(
            doc.contains(&plist),
            "plist differs from docs/agent.md:\n{plist}"
        );
    }

    #[test]
    fn plist_escapes_the_path() {
        let mut l = mac();
        l.binary = PathBuf::from("/opt/a & b/printpapi-agent");
        let text = String::from_utf8(autostart_file(Os::Macos, &l)).unwrap();
        assert!(text.contains("<string>/opt/a &amp; b/printpapi-agent</string>"));
    }

    #[test]
    fn linux_acl() {
        let cmds = ini_acl_cmds(Os::Linux, &linux().ini, Path::new("/"));
        assert_eq!(
            argvs(&cmds),
            [
                vec![
                    "chown",
                    "root:printpapi-agent",
                    "/opt/printpapi-agent/agent.ini"
                ],
                vec!["chmod", "640", "/opt/printpapi-agent/agent.ini"],
            ]
        );
    }

    #[test]
    fn macos_acl() {
        let cmds = ini_acl_cmds(Os::Macos, &mac().ini, Path::new("/"));
        assert_eq!(
            argvs(&cmds),
            [vec!["/bin/chmod", "600", "/opt/printpapi-agent/agent.ini"]]
        );
    }

    #[test]
    fn linux_commands() {
        let l = linux();
        let sys = Path::new("/");
        assert_eq!(
            argvs(&start_cmds(Os::Linux, &l, sys)),
            [
                vec!["systemctl", "daemon-reload"],
                vec!["systemctl", "enable", "--now", "printpapi-agent"],
            ]
        );
        assert_eq!(
            argvs(&stop_cmds(Os::Linux, &l, sys)),
            [vec!["systemctl", "stop", "printpapi-agent"]]
        );
        assert_eq!(
            argvs(&remove_cmds(Os::Linux, &l, sys)),
            [vec!["systemctl", "disable", "printpapi-agent"]]
        );
        assert_eq!(
            identity_check(Os::Linux).unwrap().argv,
            ["id", "-u", "printpapi-agent"]
        );
        assert_eq!(
            argvs(&identity_cmds(Os::Linux)),
            [vec![
                "useradd",
                "--system",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                "printpapi-agent"
            ]]
        );
    }

    #[test]
    fn macos_programs_are_absolute() {
        let l = mac();
        let sys = Path::new("/");
        assert_eq!(
            argvs(&start_cmds(Os::Macos, &l, sys)),
            [
                vec![
                    "/usr/bin/xattr",
                    "-d",
                    "com.apple.quarantine",
                    "/opt/printpapi-agent/printpapi-agent"
                ],
                vec![
                    "/bin/launchctl",
                    "bootstrap",
                    "system",
                    "/Library/LaunchDaemons/com.printpapi.agent.plist"
                ],
            ]
        );
        assert_eq!(
            argvs(&stop_cmds(Os::Macos, &l, sys)),
            [vec![
                "/bin/launchctl",
                "bootout",
                "system/com.printpapi.agent"
            ]]
        );
        assert!(remove_cmds(Os::Macos, &l, sys).is_empty());
        assert!(identity_check(Os::Macos).is_none());
        assert!(identity_cmds(Os::Macos).is_empty());
        let mut all = start_cmds(Os::Macos, &l, sys);
        all.extend(stop_cmds(Os::Macos, &l, sys));
        all.extend(ini_acl_cmds(Os::Macos, &l.ini, sys));
        for c in all {
            assert!(c.argv[0].starts_with('/'), "{:?}", c.argv);
        }
    }

    #[test]
    fn only_linux_without_systemd_is_unsupported() {
        let with = tempfile::tempdir().unwrap();
        let without = with.path().join("missing");
        for dir in [with.path(), without.as_path()] {
            assert!(supported_at(Os::Windows, dir));
            assert!(supported_at(Os::Macos, dir));
        }
        assert_eq!(SYSTEMD_RUN_DIR, "/run/systemd/system");
        assert!(supported_at(Os::Linux, with.path()));
        assert!(!supported_at(Os::Linux, &without));
    }

    #[test]
    fn host_os_matches_the_build_target() {
        assert_eq!(Os::host() == Os::Windows, cfg!(windows));
        assert_eq!(Os::host() == Os::Macos, cfg!(target_os = "macos"));
        assert_eq!(
            Os::host() == Os::Linux,
            !cfg!(windows) && !cfg!(target_os = "macos")
        );
    }

    #[cfg(windows)]
    #[test]
    fn program_files_is_asked_of_windows_not_of_the_environment() {
        // nothing else in this binary reads `ProgramFiles`, so changing it here races with nobody
        let real = std::env::var_os("ProgramFiles").unwrap();
        // SAFETY: see above; the value is put back before the test ends
        unsafe { std::env::set_var("ProgramFiles", "Z:\\planted") };
        let dir = program_files();
        unsafe { std::env::set_var("ProgramFiles", &real) };
        assert!(dir.is_absolute() && dir.is_dir(), "{dir:?}");
        assert!(!dir.starts_with("Z:\\planted"), "{dir:?}");
        // and it is the folder the environment names on this machine
        assert_eq!(
            dir.to_string_lossy().to_lowercase(),
            real.to_string_lossy().to_lowercase()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn program_files_has_a_value_off_windows_too() {
        assert_eq!(program_files(), Path::new("C:\\Program Files"));
    }

    #[test]
    fn system_dir_is_the_real_tool_folder() {
        let dir = system_dir();
        if cfg!(windows) {
            // the system folder Windows names, whatever the environment says
            // (Windows spells it `C:\WINDOWS\system32`)
            let last = dir.file_name().unwrap().to_string_lossy().to_lowercase();
            assert!(dir.is_absolute() && last == "system32", "{dir:?}");
            for tool in ["schtasks.exe", "tasklist.exe", "icacls.exe", "sc.exe"] {
                assert!(dir.join(tool).exists(), "{tool} not in {dir:?}");
            }
        } else {
            assert_eq!(dir, Path::new("/"));
        }
    }

    #[test]
    fn service_logs() {
        // where `run` sends the crash log of a setup-installed agent: next to the status file
        let w = service_log_path(Os::Windows, Path::new("C:\\ProgramData"));
        assert_eq!(
            p(&w),
            "C:\\ProgramData\\printpapi-agent\\print_agent-error.log"
        );
        assert_eq!(
            p(&service_log_path(Os::Macos, Path::new("/"))),
            "/Library/Application Support/printpapi-agent/print_agent-error.log"
        );
        assert_eq!(
            p(&service_log_path(Os::Linux, Path::new("/"))),
            "/var/lib/printpapi-agent/print_agent-error.log"
        );
    }

    fn win_with_data(program_files: &str, program_data: &str) -> Layout {
        layout_with(
            Os::Windows,
            Path::new(program_files),
            Path::new(program_data),
        )
    }

    #[test]
    fn tray_places_per_platform() {
        let w = win_with_data("C:\\Program Files", "C:\\ProgramData");
        assert_eq!(p(&w.status_dir), "C:\\ProgramData\\printpapi-agent");
        assert_eq!(
            p(&w.tray),
            "C:\\Program Files\\printpapi-agent\\printpapi-tray.exe"
        );
        // Windows starts the icon from the Run key, not from a file
        assert!(w.tray_autostart.is_none() && w.app_entry.is_none());
        let l = linux();
        assert_eq!(p(&l.status_dir), "/var/lib/printpapi-agent");
        assert_eq!(p(&l.tray), "/opt/printpapi-agent/printpapi-tray");
        assert_eq!(
            p(l.tray_autostart.as_ref().unwrap()),
            "/etc/xdg/autostart/printpapi-tray.desktop"
        );
        assert_eq!(
            p(l.app_entry.as_ref().unwrap()),
            "/usr/share/applications/printpapi-agent.desktop"
        );
        let m = mac();
        assert_eq!(
            p(&m.status_dir),
            "/Library/Application Support/printpapi-agent"
        );
        assert_eq!(p(&m.tray), "/Applications/printpapi Agent.app");
        assert_eq!(
            p(m.tray_autostart.as_ref().unwrap()),
            "/Library/LaunchAgents/com.printpapi.tray.plist"
        );
        assert!(m.app_entry.is_none());
    }

    #[test]
    fn linux_tray_autostart_and_app_entry() {
        let l = linux();
        let auto = String::from_utf8(tray_autostart_file(Os::Linux, &l).unwrap()).unwrap();
        for needle in [
            "[Desktop Entry]\n",
            "Type=Application\n",
            "Name=printpapi Agent\n",
            "Exec=\"/opt/printpapi-agent/printpapi-tray\" --tray\n",
        ] {
            assert!(auto.contains(needle), "{needle} missing in {auto}");
        }
        // the application menu entry opens the window directly: no --tray
        let entry = String::from_utf8(app_entry_file(&l)).unwrap();
        assert!(
            entry.contains("Exec=\"/opt/printpapi-agent/printpapi-tray\"\n"),
            "{entry}"
        );
        assert!(!entry.contains("--tray"), "{entry}");
        assert!(entry.contains("Type=Application\n") && entry.contains("Name=printpapi Agent\n"));
        // a path with a space stays one argument
        let mut spaced = linux();
        spaced.tray = PathBuf::from("/opt/my dir/printpapi-tray");
        let auto = String::from_utf8(tray_autostart_file(Os::Linux, &spaced).unwrap()).unwrap();
        assert!(
            auto.contains("Exec=\"/opt/my dir/printpapi-tray\" --tray\n"),
            "{auto}"
        );
    }

    /// Desktop Entry spec, "The Exec key": an argument with a space is in double quotes, and
    /// `"`, `` ` ``, `$` and `\` in it get a backslash; a backslash in a value is written twice
    /// again (four in the file); `%` is `%%`.
    /// https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
    #[test]
    fn desktop_exec_quoting_follows_the_spec() {
        assert_eq!(desktop_exec_quote("/a b/c"), "\"/a b/c\"");
        assert_eq!(desktop_exec_quote("a\"b"), "\"a\\\\\"b\"");
        assert_eq!(desktop_exec_quote("a$b"), "\"a\\\\$b\"");
        assert_eq!(desktop_exec_quote("a`b"), "\"a\\\\`b\"");
        assert_eq!(desktop_exec_quote("a\\b"), "\"a\\\\\\\\b\"");
        assert_eq!(desktop_exec_quote("100%"), "\"100%%\"");
    }

    #[test]
    fn macos_tray_autostart_is_a_launch_agent_for_gui_sessions() {
        let text = String::from_utf8(tray_autostart_file(Os::Macos, &mac()).unwrap()).unwrap();
        for needle in [
            "<key>Label</key><string>com.printpapi.tray</string>",
            "<string>/Applications/printpapi Agent.app/Contents/MacOS/printpapi-setup</string>",
            "<string>--tray</string>",
            "<key>RunAtLoad</key><true/>",
            "<key>LimitLoadToSessionType</key><string>Aqua</string>",
        ] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
        // an agent that only runs in the graphical session must not be kept alive in others
        assert!(!text.contains("KeepAlive"), "{text}");
        let mut odd = mac();
        odd.tray = PathBuf::from("/Applications/a & b.app");
        let text = String::from_utf8(tray_autostart_file(Os::Macos, &odd).unwrap()).unwrap();
        assert!(
            text.contains("/Applications/a &amp; b.app/Contents/MacOS/"),
            "{text}"
        );
        assert!(tray_autostart_file(Os::Windows, &win_layout("C:\\PF")).is_none());
    }

    #[test]
    fn reg_uses_absolute_path_and_hklm_run() {
        let l = win_layout("C:\\Program Files");
        let sys = Path::new(WIN_SYS);
        let add = tray_register_cmds(Os::Windows, &l, sys);
        assert_eq!(
            argvs(&add),
            [vec![
                "C:\\Windows\\System32\\reg.exe",
                "add",
                "HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "printpapi-tray",
                "/t",
                "REG_SZ",
                "/d",
                "\"C:\\Program Files\\printpapi-agent\\printpapi-tray.exe\" --tray",
                "/f"
            ]]
        );
        let del = tray_unregister_cmds(Os::Windows, &l, sys);
        assert_eq!(
            argvs(&del),
            [vec![
                "C:\\Windows\\System32\\reg.exe",
                "delete",
                "HKLM\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "printpapi-tray",
                "/f"
            ]]
        );
        // the other systems register with a file, not with commands
        for os in [Os::Linux, Os::Macos] {
            assert!(tray_register_cmds(os, &linux(), Path::new("/")).is_empty());
            assert!(tray_unregister_cmds(os, &linux(), Path::new("/")).is_empty());
        }
    }

    #[test]
    fn tray_stop_commands() {
        assert_eq!(
            argvs(&tray_stop_cmds(
                Os::Windows,
                &win_layout("C:\\Program Files"),
                Path::new(WIN_SYS)
            )),
            [vec![
                "C:\\Windows\\System32\\taskkill.exe",
                "/f",
                "/im",
                "printpapi-tray.exe"
            ]]
        );
        assert_eq!(
            argvs(&tray_stop_cmds(Os::Linux, &linux(), Path::new("/"))),
            [vec!["pkill", "-f", "/opt/printpapi-agent/printpapi-tray"]]
        );
        assert_eq!(
            argvs(&tray_stop_cmds(Os::Macos, &mac(), Path::new("/"))),
            [vec![
                "/usr/bin/pkill",
                "-f",
                r"/Applications/printpapi Agent\.app/Contents/MacOS/"
            ]]
        );
    }

    #[test]
    fn status_dir_rights_use_sids() {
        let l = win_with_data("C:\\Program Files", "C:\\ProgramData");
        let cmds = status_dir_cmds(Os::Windows, &l, Path::new(WIN_SYS));
        assert_eq!(
            argvs(&cmds),
            [
                vec![
                    "C:\\Windows\\System32\\icacls.exe",
                    "C:\\ProgramData\\printpapi-agent",
                    "/setowner",
                    "*S-1-5-32-544"
                ],
                vec![
                    "C:\\Windows\\System32\\icacls.exe",
                    "C:\\ProgramData\\printpapi-agent",
                    "/inheritance:r",
                    "/grant:r",
                    "*S-1-5-18:(OI)(CI)F",
                    "*S-1-5-32-544:(OI)(CI)F",
                    "*S-1-5-32-545:(OI)(CI)RX"
                ]
            ]
        );
        // icacls refuses /setowner next to any other switch, so it stands alone
        for c in &cmds {
            let setowner = c.argv.iter().any(|x| x == "/setowner");
            assert!(!setowner || c.argv.len() == 4, "{:?}", c.argv);
            assert!(
                !c.argv
                    .iter()
                    .any(|x| x.contains("Users") || x.contains("SYSTEM"))
            );
        }
    }

    #[test]
    fn status_dir_rights_on_unix() {
        assert_eq!(
            argvs(&status_dir_cmds(Os::Linux, &linux(), Path::new("/"))),
            [
                vec!["chown", "printpapi-agent", "/var/lib/printpapi-agent"],
                vec!["chmod", "755", "/var/lib/printpapi-agent"],
            ]
        );
        assert_eq!(
            argvs(&status_dir_cmds(Os::Macos, &mac(), Path::new("/"))),
            [
                vec![
                    "/usr/sbin/chown",
                    "root:wheel",
                    "/Library/Application Support/printpapi-agent"
                ],
                vec![
                    "/bin/chmod",
                    "755",
                    "/Library/Application Support/printpapi-agent"
                ],
            ]
        );
    }

    #[test]
    fn the_macos_app_is_copied_with_ditto_and_given_to_root() {
        let m = mac();
        let from = Path::new("/var/folders/x y/staging/tray");
        let cmds = tray_copy_cmds(Os::Macos, from, &m, Path::new("/"));
        assert_eq!(
            argvs(&cmds),
            [
                vec![
                    "/usr/bin/ditto",
                    "/var/folders/x y/staging/tray",
                    "/Applications/printpapi Agent.app"
                ],
                vec![
                    "/usr/bin/xattr",
                    "-dr",
                    "com.apple.quarantine",
                    "/Applications/printpapi Agent.app"
                ],
                vec![
                    "/usr/sbin/chown",
                    "-R",
                    "root:wheel",
                    "/Applications/printpapi Agent.app"
                ],
                vec![
                    "/bin/chmod",
                    "-R",
                    "go-w",
                    "/Applications/printpapi Agent.app"
                ],
            ]
        );
        assert!(tray_copy_cmds(Os::Linux, from, &linux(), Path::new("/")).is_empty());
        assert!(
            tray_copy_cmds(Os::Windows, from, &win_layout("C:\\PF"), Path::new(WIN_SYS)).is_empty()
        );
    }

    #[test]
    fn the_tray_commands_are_absolute_where_they_must_be() {
        let l = win_layout("C:\\Program Files");
        let sys = Path::new(WIN_SYS);
        let mut all = tray_register_cmds(Os::Windows, &l, sys);
        all.extend(tray_unregister_cmds(Os::Windows, &l, sys));
        all.extend(tray_stop_cmds(Os::Windows, &l, sys));
        all.extend(status_dir_cmds(Os::Windows, &l, sys));
        assert_eq!(all.len(), 5);
        for c in all {
            assert!(c.argv[0].starts_with(WIN_SYS), "{:?}", c.argv);
        }
        let m = mac();
        let mut all = tray_stop_cmds(Os::Macos, &m, Path::new("/"));
        all.extend(status_dir_cmds(Os::Macos, &m, Path::new("/")));
        all.extend(tray_copy_cmds(
            Os::Macos,
            Path::new("/s/tray"),
            &m,
            Path::new("/"),
        ));
        for c in all {
            assert!(c.argv[0].starts_with('/'), "{:?}", c.argv);
        }
    }

    /// A layout inside a temp dir, to try `detect` against real files.
    fn temp_layout(dir: &Path) -> Layout {
        Layout {
            dir: dir.to_path_buf(),
            binary: dir.join("printpapi-agent"),
            ini: dir.join("agent.ini"),
            marker: dir.join(MARKER_NAME),
            sumatra: dir.join("SumatraPDF.exe"),
            autostart: dir.join("autostart"),
            status_dir: dir.join("status"),
            tray: dir.join("printpapi-tray"),
            tray_autostart: None,
            app_entry: None,
        }
    }

    type Seen = Rc<RefCell<Vec<Cmd>>>;

    const NOTHING_RUNNING: &str = "INFO: No tasks are running which match the specified criteria.";

    /// A stand-in for `System32` on disk: `detect` looks for the task file in it.
    fn fake_system32() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Tasks")).unwrap();
        dir
    }

    /// What the Windows tools answer. `schtasks /query` and `sc query` succeed or fail,
    /// `tasklist` answers the `tasklist` text.
    struct Win {
        schtasks: bool,
        service: bool,
        tasklist: &'static str,
    }

    const WIN_NOTHING: Win = Win {
        schtasks: false,
        service: false,
        tasklist: NOTHING_RUNNING,
    };

    /// Windows: every program is absolute (inside `sys`), every question captures stdout and has
    /// exactly the arguments `detect` is documented to use. `tasklist` only answers a call that
    /// filters for the agent.
    fn windows_fake(sys: &Path, win: Win) -> (Runner, Seen) {
        let seen: Seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let sys = s(sys);
        let run: Runner = Rc::new(move |c| {
            log.borrow_mut().push(c.clone());
            assert!(c.argv[0].starts_with(&sys), "not absolute: {:?}", c.argv);
            assert!(c.capture, "output is needed: {:?}", c.argv);
            let ok_or = |ok: bool| {
                if ok {
                    Ok(Vec::new())
                } else {
                    Err("Command exited with 1".to_string())
                }
            };
            if c.argv[0].ends_with("schtasks.exe") {
                assert_eq!(c.argv[1..], ["/query", "/tn", "printpapi-agent"]);
                return ok_or(win.schtasks);
            }
            if c.argv[0].ends_with("sc.exe") {
                assert_eq!(c.argv[1..], ["query", "printpapi-agent"]);
                return ok_or(win.service);
            }
            assert!(c.argv[0].ends_with("tasklist.exe"), "{:?}", c.argv);
            assert!(
                c.argv
                    .iter()
                    .any(|a| a == "IMAGENAME eq printpapi-agent.exe"),
                "{:?}",
                c.argv
            );
            Ok(win.tasklist.as_bytes().to_vec())
        });
        (run, seen)
    }

    /// Unix: only `<pgrep> -x printpapi-agent` is known, and it must capture. Exit 0 if `running`,
    /// 1 if not, an error like a missing program if `pgrep_exists` is false.
    fn unix_fake(pgrep: &'static str, running: bool, pgrep_exists: bool) -> (Runner, Seen) {
        let seen: Seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let run: Runner = Rc::new(move |c| {
            log.borrow_mut().push(c.clone());
            assert_eq!(c.argv, [pgrep, "-x", "printpapi-agent"]);
            assert!(c.capture, "output is needed: {:?}", c.argv);
            if !pgrep_exists {
                Err("No such file or directory (os error 2)".into())
            } else if running {
                Ok(Vec::new())
            } else {
                Err("Command exited with 1".into())
            }
        });
        (run, seen)
    }

    fn unix_cases() -> [(Os, &'static str); 2] {
        [(Os::Linux, "pgrep"), (Os::Macos, "/usr/bin/pgrep")]
    }

    #[test]
    fn detect_none_when_nothing_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        let sys = fake_system32();
        let (win, seen) = windows_fake(sys.path(), WIN_NOTHING);
        assert_eq!(detect(Os::Windows, &l, sys.path(), &win), Installed::None);
        assert_eq!(seen.borrow().len(), 3, "query, service, tasklist");
        for (os, pgrep) in unix_cases() {
            let (not_running, _) = unix_fake(pgrep, false, true);
            assert_eq!(
                detect(os, &l, Path::new("/"), &not_running),
                Installed::None
            );
            // no pgrep on the machine: it cannot be told, so do not block the install
            let (no_pgrep, _) = unix_fake(pgrep, false, false);
            assert_eq!(detect(os, &l, Path::new("/"), &no_pgrep), Installed::None);
        }
    }

    #[test]
    fn detect_autostart_file_without_marker_is_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        std::fs::write(&l.autostart, "x").unwrap();
        let sys = fake_system32();
        let (win, seen) = windows_fake(sys.path(), WIN_NOTHING);
        assert_eq!(
            detect(Os::Windows, &l, sys.path(), &win),
            Installed::Foreign
        );
        assert!(seen.borrow().is_empty());
        for (os, pgrep) in unix_cases() {
            let (not_running, _) = unix_fake(pgrep, false, true);
            assert_eq!(
                detect(os, &l, Path::new("/"), &not_running),
                Installed::Foreign
            );
        }
    }

    #[test]
    fn detect_the_normal_install_is_managed_and_nobody_asks_the_system() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        // what the setup leaves behind: the autostart file and the marker, together
        std::fs::write(&l.autostart, "x").unwrap();
        std::fs::write(&l.marker, "").unwrap();
        let sys = fake_system32();
        let (win, seen) = windows_fake(
            sys.path(),
            Win {
                schtasks: true,
                service: true,
                tasklist: "\"printpapi-agent.exe\"",
            },
        );
        assert_eq!(
            detect(Os::Windows, &l, sys.path(), &win),
            Installed::Managed
        );
        assert!(seen.borrow().is_empty());
        for (os, pgrep) in unix_cases() {
            let (running, seen) = unix_fake(pgrep, true, true);
            assert_eq!(detect(os, &l, Path::new("/"), &running), Installed::Managed);
            assert!(seen.borrow().is_empty());
        }
    }

    #[test]
    fn detect_finds_a_hand_made_task_or_a_running_process_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        let sys = fake_system32();
        let (task, seen) = windows_fake(
            sys.path(),
            Win {
                schtasks: true,
                ..WIN_NOTHING
            },
        );
        assert_eq!(
            detect(Os::Windows, &l, sys.path(), &task),
            Installed::Foreign
        );
        assert_eq!(
            seen.borrow()[0].argv[1..],
            ["/query", "/tn", "printpapi-agent"]
        );
        let (running, seen) = windows_fake(
            sys.path(),
            Win {
                tasklist: "\"PrintPapi-Agent.exe\",\"4242\",\"Services\",\"0\",\"9,000 K\"",
                ..WIN_NOTHING
            },
        );
        assert_eq!(
            detect(Os::Windows, &l, sys.path(), &running),
            Installed::Foreign
        );
        let seen = seen.borrow();
        assert_eq!(seen.len(), 3);
        assert_eq!(
            seen[2].argv[1..],
            [
                "/fi",
                "IMAGENAME eq printpapi-agent.exe",
                "/fo",
                "csv",
                "/nh"
            ]
        );
    }

    /// A task made from an elevated shell is invisible to `schtasks /query` for a normal user,
    /// but its file in `System32\Tasks` is not. The agent is not running at that moment.
    #[test]
    fn detect_finds_a_task_that_schtasks_cannot_see_by_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        let sys = fake_system32();
        std::fs::write(sys.path().join("Tasks").join("printpapi-agent"), "<Task/>").unwrap();
        let (win, _) = windows_fake(sys.path(), WIN_NOTHING);
        assert_eq!(
            detect(Os::Windows, &l, sys.path(), &win),
            Installed::Foreign
        );
        // another task in the folder is not ours
        let other = fake_system32();
        std::fs::write(other.path().join("Tasks").join("printpapi-agent-x"), "").unwrap();
        let (win, _) = windows_fake(other.path(), WIN_NOTHING);
        assert_eq!(detect(Os::Windows, &l, other.path(), &win), Installed::None);
    }

    #[test]
    fn only_a_clean_not_found_means_there_is_no_task_file() {
        use std::io::{Error, ErrorKind};
        assert!(!task_file_present(Ok(false)));
        assert!(task_file_present(Ok(true)));
        // `try_exists` on the task of another user: measured to be PermissionDenied
        assert!(task_file_present(Err(Error::from(
            ErrorKind::PermissionDenied
        ))));
        assert!(task_file_present(Err(Error::other("anything else"))));
    }

    /// The service of `nssm install printpapi-agent ...` runs the Python agent as `pythonw.exe`,
    /// so neither the task nor `tasklist` sees it.
    #[test]
    fn detect_finds_an_nssm_service_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        let sys = fake_system32();
        let (win, seen) = windows_fake(
            sys.path(),
            Win {
                service: true,
                ..WIN_NOTHING
            },
        );
        assert_eq!(
            detect(Os::Windows, &l, sys.path(), &win),
            Installed::Foreign
        );
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].argv[0].ends_with("sc.exe"));
    }

    #[test]
    fn detect_finds_a_hand_started_agent_on_linux_and_macos() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        for (os, pgrep) in unix_cases() {
            let (running, seen) = unix_fake(pgrep, true, true);
            assert_eq!(detect(os, &l, Path::new("/"), &running), Installed::Foreign);
            assert_eq!(seen.borrow().len(), 1);
        }
    }
}
