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
/// Linux service user.
pub const SERVICE_USER: &str = "printpapi";
/// File that tells a later run of the setup "this installation is mine".
pub const MARKER_NAME: &str = ".installed-by-setup";

const SID_SYSTEM: &str = "*S-1-5-18";
const SID_ADMINISTRATORS: &str = "*S-1-5-32-544";
const CMD_TIMEOUT: u64 = 60;

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

/// The folders and files of an installation. `program_files` is `%ProgramFiles%` and only
/// matters on Windows; macOS and Linux install to `/opt/printpapi`.
pub fn layout(os: Os, program_files: &Path) -> Layout {
    let dir = match os {
        Os::Windows => join(os, program_files, "printpapi"),
        Os::Linux | Os::Macos => PathBuf::from("/opt/printpapi"),
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
    Layout {
        binary: join(os, &dir, binary_name),
        ini: join(os, &dir, "agent.ini"),
        marker: join(os, &dir, MARKER_NAME),
        sumatra: join(os, &dir, "SumatraPDF.exe"),
        autostart,
        dir,
    }
}

/// The folder with the system tools: `%SystemRoot%\System32` on Windows, else `/`.
// ponytail: trusts the `SystemRoot` environment variable of the elevated process. Fine while the
// setup is unsigned (the spec accepts that gap); with code signing (roadmap 12) ask
// `GetSystemDirectoryW` instead (windows-sys, feature `Win32_System_SystemInformation`).
pub fn system_dir() -> PathBuf {
    if cfg!(windows) {
        system32(std::env::var("SystemRoot").ok().as_deref())
    } else {
        PathBuf::from("/")
    }
}

/// `<SystemRoot>\System32`, with `C:\Windows` when the variable is missing or empty.
fn system32(system_root: Option<&str>) -> PathBuf {
    let root = system_root
        .filter(|r| !r.is_empty())
        .unwrap_or("C:\\Windows");
    join(Os::Windows, Path::new(root), "System32")
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

/// Same text as `docs/agent.md#run-as-a-service`.
fn systemd_unit(l: &Layout) -> String {
    format!(
        "[Unit]\nDescription=printpapi agent\nAfter=network-online.target cups.service\n\n\
         [Service]\nExecStart={}\nRestart=always\nRestartSec=5\nUser={SERVICE_USER}\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        s(&l.binary)
    )
}

/// Same text as `docs/agent.md#run-as-a-service`.
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
    supported_at(os, Path::new("/run/systemd/system"))
}

/// `supported` with the path that proves systemd is running as a parameter.
fn supported_at(os: Os, systemd_dir: &Path) -> bool {
    match os {
        Os::Linux => systemd_dir.exists(),
        Os::Windows | Os::Macos => true,
    }
}

/// Restrict `agent.ini` (it holds the key): SYSTEM and Administrators on Windows, `root:printpapi`
/// 0640 on Linux, root 0600 on macOS. Windows uses SIDs, because group names are localised
/// ("Administratoren" on a German Windows). icacls takes a SID with a leading `*`
/// (https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/icacls).
pub fn ini_acl_cmds(os: Os, l: &Layout, sys: &Path) -> Vec<Cmd> {
    let ini = s(&l.ini);
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
/// (the admin subcommands): on Linux and macOS it would find itself.
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

/// Windows: a task called `printpapi-agent`, or a running `printpapi-agent.exe`.
fn windows_agent_present(os: Os, sys: &Path, run: &Runner) -> bool {
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

/// The log of the installed agent, for "see the log" in an error message. Windows: the crash log
/// in SYSTEM's `%LOCALAPPDATA%`. macOS: the file the plist writes to. Linux: the crash log in the
/// temp dir (the unit sets no `PrivateTmp`).
pub fn service_log_path(os: Os, sys: &Path) -> PathBuf {
    match os {
        Os::Windows => join(
            os,
            sys,
            "config\\systemprofile\\AppData\\Local\\print_agent-error.log",
        ),
        Os::Linux => PathBuf::from("/tmp/print_agent-error.log"),
        Os::Macos => PathBuf::from("/var/log/printpapi-agent.log"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

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
    fn layouts_per_platform() {
        let w = win_layout("C:\\Program Files");
        assert_eq!(p(&w.dir), "C:\\Program Files\\printpapi");
        assert_eq!(
            p(&w.binary),
            "C:\\Program Files\\printpapi\\printpapi-agent.exe"
        );
        assert_eq!(p(&w.ini), "C:\\Program Files\\printpapi\\agent.ini");
        assert_eq!(
            p(&w.marker),
            "C:\\Program Files\\printpapi\\.installed-by-setup"
        );
        assert_eq!(
            p(&w.sumatra),
            "C:\\Program Files\\printpapi\\SumatraPDF.exe"
        );
        assert_eq!(
            p(&w.autostart),
            "C:\\Program Files\\printpapi\\printpapi-agent-task.xml"
        );
        // a trailing backslash does not double up
        assert_eq!(p(&win_layout("C:\\PF\\").dir), "C:\\PF\\printpapi");
        let l = linux();
        assert_eq!(p(&l.dir), "/opt/printpapi");
        assert_eq!(p(&l.binary), "/opt/printpapi/printpapi-agent");
        assert_eq!(p(&l.ini), "/opt/printpapi/agent.ini");
        assert_eq!(p(&l.marker), "/opt/printpapi/.installed-by-setup");
        assert_eq!(
            p(&l.autostart),
            "/etc/systemd/system/printpapi-agent.service"
        );
        let m = mac();
        assert_eq!(p(&m.binary), "/opt/printpapi/printpapi-agent");
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
        ] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
        assert!(
            text.contains("<Command>C:\\Program Files\\printpapi\\printpapi-agent.exe</Command>")
        );
    }

    #[test]
    fn task_xml_escapes_the_path() {
        let text = utf16(&autostart_file(
            Os::Windows,
            &win_layout("C:\\Programme & Ü"),
        ));
        assert!(
            text.contains("C:\\Programme &amp; Ü\\printpapi\\printpapi-agent.exe"),
            "{text}"
        );
    }

    #[test]
    fn windows_programs_are_absolute() {
        let l = win_layout("C:\\Program Files");
        let sys = Path::new(WIN_SYS);
        let mut all = stop_cmds(Os::Windows, &l, sys);
        all.extend(ini_acl_cmds(Os::Windows, &l, sys));
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
        let cmds = ini_acl_cmds(Os::Windows, &l, Path::new(WIN_SYS));
        assert_eq!(cmds.len(), 1);
        let a = &cmds[0].argv;
        assert!(a[0].ends_with("icacls.exe"));
        assert_eq!(a[1], "C:\\Program Files\\printpapi\\agent.ini");
        for needle in ["/inheritance:r", "*S-1-5-18:F", "*S-1-5-32-544:F"] {
            assert!(a.iter().any(|x| x == needle), "{needle} missing in {a:?}");
        }
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
                "C:\\Users\\Jürgen Müller\\Programme\\printpapi\\printpapi-agent-task.xml",
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
    fn systemd_unit_runs_as_printpapi() {
        let text = String::from_utf8(autostart_file(Os::Linux, &linux())).unwrap();
        for needle in [
            "User=printpapi",
            "Restart=always",
            "RestartSec=5",
            "ExecStart=/opt/printpapi/printpapi-agent",
            "After=network-online.target cups.service",
            "WantedBy=multi-user.target",
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
            "<string>/opt/printpapi/printpapi-agent</string>",
        ] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
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
        let cmds = ini_acl_cmds(Os::Linux, &linux(), Path::new("/"));
        assert_eq!(
            argvs(&cmds),
            [
                vec!["chown", "root:printpapi", "/opt/printpapi/agent.ini"],
                vec!["chmod", "640", "/opt/printpapi/agent.ini"],
            ]
        );
    }

    #[test]
    fn macos_acl() {
        let cmds = ini_acl_cmds(Os::Macos, &mac(), Path::new("/"));
        assert_eq!(
            argvs(&cmds),
            [vec!["/bin/chmod", "600", "/opt/printpapi/agent.ini"]]
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
            ["id", "-u", "printpapi"]
        );
        assert_eq!(
            argvs(&identity_cmds(Os::Linux)),
            [vec![
                "useradd",
                "--system",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                "printpapi"
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
                    "/opt/printpapi/printpapi-agent"
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
        all.extend(ini_acl_cmds(Os::Macos, &l, sys));
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

    #[test]
    fn system32_follows_system_root_and_falls_back() {
        assert_eq!(p(&system32(Some("D:\\Win"))), "D:\\Win\\System32");
        assert_eq!(p(&system32(Some("D:\\Win\\"))), "D:\\Win\\System32");
        assert_eq!(p(&system32(Some(""))), "C:\\Windows\\System32");
        assert_eq!(p(&system32(None)), "C:\\Windows\\System32");
    }

    #[test]
    fn system_dir_is_the_real_tool_folder() {
        let dir = system_dir();
        if cfg!(windows) {
            for tool in ["schtasks.exe", "tasklist.exe", "icacls.exe"] {
                assert!(dir.join(tool).exists(), "{tool} not in {dir:?}");
            }
        } else {
            assert_eq!(dir, Path::new("/"));
        }
    }

    #[test]
    fn service_logs() {
        let w = service_log_path(Os::Windows, Path::new(WIN_SYS));
        assert_eq!(
            p(&w),
            "C:\\Windows\\System32\\config\\systemprofile\\AppData\\Local\\print_agent-error.log"
        );
        assert_eq!(
            p(&service_log_path(Os::Macos, Path::new("/"))),
            "/var/log/printpapi-agent.log"
        );
        assert_eq!(
            p(&service_log_path(Os::Linux, Path::new("/"))),
            "/tmp/print_agent-error.log"
        );
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
        }
    }

    type Seen = Rc<RefCell<Vec<Cmd>>>;

    const NOTHING_RUNNING: &str = "INFO: No tasks are running which match the specified criteria.";

    /// Windows: `schtasks /query` succeeds or fails, `tasklist` answers `tasklist_out`, but only
    /// to a call that captures stdout and filters for the agent. Every program is absolute.
    fn windows_fake(schtasks_ok: bool, tasklist_out: &'static str) -> (Runner, Seen) {
        let seen: Seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let run: Runner = Rc::new(move |c| {
            log.borrow_mut().push(c.clone());
            assert!(c.argv[0].starts_with(WIN_SYS), "not absolute: {:?}", c.argv);
            if c.argv[0].ends_with("schtasks.exe") {
                return if schtasks_ok {
                    Ok(Vec::new())
                } else {
                    Err("1".into())
                };
            }
            assert!(c.argv[0].ends_with("tasklist.exe"), "{:?}", c.argv);
            if c.capture
                && c.argv
                    .iter()
                    .any(|a| a == "IMAGENAME eq printpapi-agent.exe")
            {
                Ok(tasklist_out.as_bytes().to_vec())
            } else {
                Ok(Vec::new())
            }
        });
        (run, seen)
    }

    /// Unix: only `<pgrep> -x printpapi-agent` is known. Exit 0 if `running`, 1 if not, an error
    /// like a missing program if `pgrep_exists` is false.
    fn unix_fake(pgrep: &'static str, running: bool, pgrep_exists: bool) -> (Runner, Seen) {
        let seen: Seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let run: Runner = Rc::new(move |c| {
            log.borrow_mut().push(c.clone());
            assert_eq!(c.argv, [pgrep, "-x", "printpapi-agent"]);
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
        let (win, _) = windows_fake(false, NOTHING_RUNNING);
        assert_eq!(
            detect(Os::Windows, &l, Path::new(WIN_SYS), &win),
            Installed::None
        );
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
        let (win, seen) = windows_fake(false, NOTHING_RUNNING);
        assert_eq!(
            detect(Os::Windows, &l, Path::new(WIN_SYS), &win),
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
        let (win, seen) = windows_fake(true, "\"printpapi-agent.exe\"");
        assert_eq!(
            detect(Os::Windows, &l, Path::new(WIN_SYS), &win),
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
        let sys = Path::new(WIN_SYS);
        let (task, seen) = windows_fake(true, "");
        assert_eq!(detect(Os::Windows, &l, sys, &task), Installed::Foreign);
        assert_eq!(
            seen.borrow()[0].argv[1..],
            ["/query", "/tn", "printpapi-agent"]
        );
        let (running, seen) = windows_fake(
            false,
            "\"PrintPapi-Agent.exe\",\"4242\",\"Services\",\"0\",\"9,000 K\"",
        );
        assert_eq!(detect(Os::Windows, &l, sys, &running), Installed::Foreign);
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1].argv[1..],
            [
                "/fi",
                "IMAGENAME eq printpapi-agent.exe",
                "/fo",
                "csv",
                "/nh"
            ]
        );
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
