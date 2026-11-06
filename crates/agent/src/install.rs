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
pub fn system_dir() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot")
            .ok()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "C:\\Windows".to_string());
        join(Os::Windows, Path::new(&root), "System32")
    } else {
        PathBuf::from("/")
    }
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
    match os {
        Os::Linux => Path::new("/run/systemd/system").exists(),
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

/// Is an agent installed, and is it ours?
pub fn detect(os: Os, l: &Layout, sys: &Path, run: &Runner) -> Installed {
    if l.marker.exists() {
        return Installed::Managed;
    }
    if l.autostart.exists() {
        return Installed::Foreign;
    }
    if os == Os::Windows {
        let mut query = cmd(vec![
            tool(os, sys, "schtasks.exe"),
            "/query".into(),
            "/tn".into(),
            TASK_NAME.into(),
        ]);
        query.capture = true;
        if run(&query).is_ok() {
            return Installed::Foreign;
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
        if let Ok(out) = run(&list)
            && String::from_utf8_lossy(&out)
                .to_ascii_lowercase()
                .contains("printpapi-agent.exe")
        {
            return Installed::Foreign;
        }
    }
    Installed::None
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
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<BootTrigger>",
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
            "com.printpapi.agent",
            "RunAtLoad",
            "KeepAlive",
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
        assert!(supported(Os::Windows));
        assert!(supported(Os::Macos));
        // on Linux the answer is the machine's own
        assert_eq!(
            supported(Os::Linux),
            Path::new("/run/systemd/system").exists()
        );
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

    type Seen = Rc<RefCell<Vec<Vec<String>>>>;

    fn fake(schtasks_ok: bool, tasklist_out: &'static str) -> (Runner, Seen) {
        let seen: Seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let run: Runner = Rc::new(move |c| {
            log.borrow_mut().push(c.argv.clone());
            if c.argv[0].ends_with("schtasks.exe") {
                return if schtasks_ok {
                    Ok(Vec::new())
                } else {
                    Err("1".into())
                };
            }
            Ok(tasklist_out.as_bytes().to_vec())
        });
        (run, seen)
    }

    #[test]
    fn detect_managed_foreign_none() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        let sys = Path::new(WIN_SYS);
        let (nothing, _) = fake(
            false,
            "INFO: No tasks are running which match the specified criteria.",
        );
        for os in [Os::Windows, Os::Linux, Os::Macos] {
            assert_eq!(detect(os, &l, sys, &nothing), Installed::None);
        }

        // an autostart file without the marker is somebody else's
        std::fs::write(&l.autostart, "x").unwrap();
        for os in [Os::Windows, Os::Linux, Os::Macos] {
            assert_eq!(detect(os, &l, sys, &nothing), Installed::Foreign);
        }
        std::fs::remove_file(&l.autostart).unwrap();

        // the marker wins, and nobody asks the system
        std::fs::write(&l.marker, "").unwrap();
        let (never, seen) = fake(true, "\"printpapi-agent.exe\"");
        for os in [Os::Windows, Os::Linux, Os::Macos] {
            assert_eq!(detect(os, &l, sys, &never), Installed::Managed);
        }
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn detect_finds_a_hand_made_task_or_a_running_process_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let l = temp_layout(dir.path());
        let sys = Path::new(WIN_SYS);
        let (task, seen) = fake(true, "");
        assert_eq!(detect(Os::Windows, &l, sys, &task), Installed::Foreign);
        assert_eq!(seen.borrow()[0][1..], ["/query", "/tn", "printpapi-agent"]);
        let (running, _) = fake(
            false,
            "\"PrintPapi-Agent.exe\",\"4242\",\"Services\",\"0\",\"9,000 K\"",
        );
        assert_eq!(detect(Os::Windows, &l, sys, &running), Installed::Foreign);
        // the same answers mean nothing on the other systems: they have no schtasks
        assert_eq!(detect(Os::Linux, &l, sys, &running), Installed::None);
        assert_eq!(detect(Os::Macos, &l, sys, &task), Installed::None);
    }
}
