# Agent

The agent runs on the machine with the printers. It registers its printers with the server,
then long-polls for jobs, prints them, and reports the result. It makes **outbound connections
only** — nothing listens on the printer's machine, so NAT/firewalls are no problem.

## Setup app

The easiest way on Windows, macOS and Linux with a desktop: download `printpapi-setup-<platform>`
(`.exe` or `.msi` on Windows, `.dmg` on macOS, `.deb`, `.rpm` or `.AppImage` on Linux) from the
[latest release](https://github.com/LukiPrince/printpapi/releases/latest), open it and go through
four steps (connect to the server with a pairing link or an agent key, pick the printers, test
print, done). The way in is [a link or a code](#connect-with-a-link-or-a-code) from the dashboard:
no key to copy. An IT provider can build this program under its own name, see
[White-label builds](white-label.md).
It writes `agent.ini`, installs the agent to `%ProgramFiles%\printpapi-agent\` (Windows) or
`/opt/printpapi-agent/` (macOS, Linux) and starts it with the machine: a scheduled task
`printpapi-agent` on Windows, a launchd daemon `com.printpapi.agent` on macOS, a systemd unit
`printpapi-agent.service` (running as the system user `printpapi-agent`) on Linux. The folder and
the user are not the ones of a printpapi server on the same machine (`/opt/printpapi`, user
`printpapi`, see the [migration guide](migration.md)), so the two do not touch each other. Open the
setup again to change the printers or remove the agent; the removal deletes only what the setup
installed, and the folder only when nothing else is in it.

### Connect with a link or a code

On the **Devices** page of your dashboard click **Connect device**. The dialog shows a link
(`printpapi://pair?server=...&code=...`), the code with the address, and the command for a
terminal. Paste the link (or the code) into the first step of the setup. The window says which
server it connects to and does nothing until you click **Connect**; then it shows the account, the
person who made the code and the device name, and asks a second time before the printers step.
An address with a character outside printable ASCII is spelled out there (`boss@\u{430}cme.example`)
with a warning, because such a character can look like another letter.
Until that point nothing has changed on this computer or for any device. The key the server hands
out is an *Agent only* key (see [Key rights](api.md#key-rights)); it stays inside the setup program
and goes to the agent only through the private working folder, never through a command line, a log
or the window.

- **Reconnect** on a device section makes a code for that device. The new key takes over its name
  and printer ids when the new agent starts, so a replacement PC or a re-install keeps the
  integrations that use fixed printer ids. The old agent keeps working until then. A job the old
  machine is printing at that moment is recorded when it finishes, for five minutes, so the new one
  does not print it again ([details](api.md#pairing-a-device)). If the old key is not an
  *Agent only* key, it keeps working for the API: the dashboard lists it with **Revoke old key**.
- A code works once and for 30 minutes. A key that was fetched and never used is revoked after
  24 hours; the Devices page shows a fetched key with **Revoke** (the server cannot say who
  fetched it, only that and when). More than ten wrong codes from one address in 15 minutes block
  that address for 15 minutes.
- **Opening the link.** The setup registers `printpapi://` for all users: on Windows in
  `HKLM\Software\Classes\printpapi`, on Linux with the application menu entry (`MimeType` and
  `%u`; GNOME and KDE need the package `desktop-file-utils` for `update-desktop-database`), on
  macOS in the app. The button **Open on this
  computer** in the dashboard needs that and the browser asks first. This is best effort: if the
  registration fails the install still works, and pasting the link into the window is the main
  path. A link opens the window with its fields filled in and a warning that a link asked for it;
  nothing is contacted before you click. **The code stands in the window's command line** while it
  runs, because that is how Windows and Linux hand a link over: another user of the same machine
  could read it and use it first. It works once, for 30 minutes, and gives an *Agent only* key that
  the dashboard shows and can revoke. The agent key is never in a command line.
- **Headers belong to their server.** `[headers]` in `agent.ini` can hold the secret of an access
  proxy, so a new connection to **another server** (another scheme, host or port) removes the
  installed headers unless the change names them, and the call to the new server goes out without
  them. A pairing to the **same** server keeps them, and so do a typed address and key (the second way in),
  as before. If a proxy in front of the server needs headers, use
  [`printpapi-agent pair`](#pair-from-the-terminal): for the same server it sends the installed
  headers, for another server it needs `--keep-headers`, which sends them to that server and keeps
  them in `agent.ini`.
- The window needs the whole server: besides `/agent/*` it reads `/health` and `/computers`. A
  server that publishes only `/agent/*` fails the check in step 1, with the code still unspent. Use
  [the terminal way](#pair-from-the-terminal) there.

### Tray icon and settings

The setup also installs a small icon (next to the clock on Windows, in the menu bar on macOS, in
the status area on Linux) that starts with every user's login. Its colour, symbol and word show
the agent: connected, offline, or not running. Its menu has the status line, **Settings...**,
**Updates...** ([below](#updates)), **Open dashboard** and **Quit**; Quit ends only the icon, the
agent keeps printing.

**Settings...** opens a window with Overview, Printers (add, remove, change the kind, test print),
Connection (another server or key), Advanced (`timeout` and the `[headers]` values), Log,
SumatraPDF (Windows), Restart and Remove, and Updates after Overview. Looking needs no administrator rights; each change asks
for them once.

- The icon only reads `status.json`, which the agent rewrites every 30 seconds from a thread of
  its own, so it stays young while a long job prints or a connection hangs: Windows
  `%ProgramData%\printpapi-agent\`, macOS `/Library/Application Support/printpapi-agent/`, Linux
  `/var/lib/printpapi-agent/`. The file holds the server, device name, state, last contact and job, the
  printers, the header names, the time the agent process started, whether the stored key can be
  read, the update mode and mirror, and whether a job is being printed, never the key and never a
  header value. A file that is missing or older than two minutes shows "not running"; an agent that
  has not been in touch with the server for two minutes shows "offline" with the time it has been
  silent. The agent still opens no port and takes no commands.
  The crash log of an agent the setup installed lives in the same folder, and so does the
  [print journal](#the-print-journal), `print-journal.jsonl`. A change made in the settings window keeps it.
- Connection: another server creates the device there. **The same server:** create a code with
  **Reconnect** on this device in the dashboard and enter it; the device keeps its name and its
  printers. A new key typed by hand on the same server needs a new device name, because the server
  ties a name to its key; the printers then get new ids, the old device stays in the dashboard, and
  the window warns before it does that. When the stored key cannot be read, the pairing form is the
  way back ([When the key cannot be read](#when-the-key-cannot-be-read)).
- On Linux the icon needs the library `libayatana-appindicator3` (`libayatana-appindicator3.so.1`;
  `libappindicator3` works too). On Debian and Ubuntu it is the package `libayatana-appindicator3-1`
  ([packages.debian.org](https://packages.debian.org/sid/libs/libayatana-appindicator3-1)).
  Without it the icon cannot be shown: it writes one line saying
  so to `printpapi-tray.log` (in `$XDG_RUNTIME_DIR/printpapi/`, else `~/.cache/printpapi/`) and ends
  quietly at every logon instead of crashing. The agent is not affected, and the application menu's
  **printpapi Agent** entry still opens the settings window.
- GNOME shows no tray icons without the
  [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).
  The application menu has a **printpapi Agent** entry that opens the settings window directly.
- Removing the agent removes the icon, its autostart entry and the status folder too (the files
  the agent wrote there; anything else in that folder stays).

- The setup is not code-signed yet, so the system warns first. Windows: *More info*, then *Run
  anyway*. macOS: System Settings, Privacy & Security, *Open Anyway*. Every setup download is marked
  Beta on the downloads page until someone has run it by hand.
- Smart App Control and WDAC block the unsigned exe with no way around it. Use the Python agent
  ([Locked-down Windows](#locked-down-windows)).
- An AppImage starts only once it is executable: run `chmod +x` on the file, or tick *Allow
  executing as program* in its properties.
- Linux without a desktop or without systemd: use the manual steps below.
- Windows on ARM64 has its own files: `printpapi-setup-windows-aarch64.exe` and
  `printpapi-setup-windows-aarch64.msi`. The agent and the setup are built for ARM64; SumatraPDF,
  which prints the PDFs, stays the x64 build that Windows runs under emulation.
- The `.msi` installs the setup program for all users, and, with a prepared file, the agent too,
  without a window: see [Managed Windows install (.msi)](#managed-windows-install-msi).
- The `.rpm` is for current Fedora and openSUSE releases with a desktop.
  `sudo dnf install ./printpapi-setup-linux-x86_64.rpm` on Fedora,
  `sudo zypper install --allow-unsigned-rpm ./printpapi-setup-linux-x86_64.rpm` on openSUSE (the
  aarch64 file is `printpapi-setup-linux-aarch64.rpm`). The file is not signed. Fedora 45 checks
  signatures by default ([the change](https://fedoraproject.org/wiki/Changes/Enforcing_signature_checking_by_default)),
  so a plain `rpm -i` refuses it there; `dnf install ./file.rpm` takes a local file. It is built
  on Ubuntu 22.04 and needs the `pkexec` command and the WebKitGTK 4.1 and
  AppIndicator libraries; the package names all of them as requirements, so the installer pulls them in
  (the package that provides `pkexec` is polkit on Fedora and pkexec on openSUSE). Every file in the downloads is Beta; for the
  `.rpm`, nobody has installed it on a real desktop yet (SELinux, the tray, the password prompt).
- **Red Hat, Rocky Linux, AlmaLinux:** not supported today. AlmaLinux 9 has only the older
  WebKitGTK that the window cannot use, and AlmaLinux 10 has WebKitGTK 4.1 only in the testing part
  of EPEL. Use the agent archive and the [systemd steps](#run-as-a-service) there.
- An agent you set up by hand is not taken over: the setup stops and says so, so a job is never
  printed twice.

## Updates

The agent and the tray icon / settings program of a setup install can update themselves. This
applies only to an agent the setup installed (the folder has the marker file
`.installed-by-setup`). An agent you set up by hand from an archive, the Python agent and the server
are never touched, and an agent from an archive makes no request for updates at all.

**What the person sees.** The tray menu has an **Updates...** item, which reads **Update to 3.7.0...**
while a newer version is known. It opens the settings window on its **Updates** section; the
Overview shows one line when a version is available. **Install 3.7.0** downloads the files, asks for
administrator rights once, replaces the agent and the setup program, checks that the new agent
connects to the server, and reopens the window as the new version. A new version that does not
connect is replaced by the old one automatically, and the window says so.

**Three modes**, in `agent.ini` under `[agent]` (the select in the window changes it for you and asks
for administrator rights once):

| `updates` | in the window | what happens |
|---|---|---|
| `notify` (default) | Tell me | the icon, and the window when it opens, look once a day and say when a version is out |
| `auto` | Install automatically | also installs it, at night, from a scheduled task (Windows), a systemd timer (Linux) or a launchd job (macOS) of its own that exists only in this mode |
| `off` | Off | no look, no request, no schedule |

Any other value counts as `off`, so a typo never switches unattended installs on. After a hand edit
of `updates`, restart the agent from the window (Restart), because that is when the schedule is
brought in line with the file.

**What is sent.** In `notify` and `auto` every signed-in user who has the icon makes one request a
day for one file, `printpapi-update.txt`, from the release page (GitHub answers it with redirects).
The window looks once when it opens (the answer is kept for a day, so it adds no second request) and
again when you press **Check now**. A file that is missing (404) or that this program refuses
(another key, a newer format) is remembered for a day, by the program that got that answer (a newer setup, installed by hand, asks again); the look is repeated hourly only while the
network fails. With `updates = off`, or an `update_url` the agent refuses, the icon and the window
make no request, show no notice and cannot start a download. Installing downloads the files named in it.
No version, device name or id is sent, only a browser's User-Agent; the agent's key and the
`[headers]` of `agent.ini` are never sent, because the address is not your printpapi server. The
unattended update asks the same way as the root or SYSTEM user, with the proxy settings of that
user. A company that wants none of this sets `updates = off`, or points `update_url` at its own
mirror (below), or builds the agent with its own keys.

**Mirror.** `update_url = https://mirror.example/printpapi` names a folder that holds the files of a
release exactly as they are on the release page (`printpapi-update.txt` and the `printpapi-update-*`
files). `http` is allowed, because the content is protected by a signature, not by the connection. An
address with a user name, a query or a fragment, or another scheme, switches checking off.

**What is checked.** The update is one file with an Ed25519 signature, `printpapi-update.txt`. It
lists the size and SHA-256 of every program file of the release. The public keys that may sign it
are in `crates/agent/update-keys.txt` and built into the agent; there is no setting and no
environment variable that changes them (see [SECURITY.md](../SECURITY.md#signed-updates)). TLS, the
host and a mirror are not trusted: a file that does not match, a version that is not newer than the
installed one, or a signature that no key verifies is refused before anything on the computer
changes. An agent that was built without a key (this is the case until the owner of a release adds
one) says "This build has no update key, so it cannot check for updates." and makes no request.

**What an install of an update does.** The installed agent, which only administrators can write,
reads each file once into memory, checks those bytes and starts the new agent and the new setup
program once with `--version`, so a program that cannot run here (Smart App Control, a wrong
architecture, a missing library) stops the update before anything is changed. It waits until the
agent is connected and not printing, swaps the two program files by renaming them, restarts the agent
the way the Restart section does, and waits up to five minutes for the new version to report in
twice with the same process. If it does not, the old files are renamed back (this needs no network)
and the old agent is started again. The result says which: "Version 3.7.0 did not connect, so 3.6.0
was put back. It is running again." Only the two program files and the marker change: `agent.ini`,
the service unit or task, the rights and the status folder stay as they were. The last attempt is
in `update-state.json` in the install folder (no secrets): `message` is the sentence the window shows,
`detail` the reason behind it (for example why the new agent did not connect), which the Windows
task and a click, both without a console, would otherwise lose. An unattended run gives a version up
after two failed nights; a click in the window always tries.

- The agent has to be connected: an agent that is stopped or offline is never updated, by click or at
  night ("The agent has to be connected, so that the update can check the new version. Otherwise run
  the setup of the new version."). A queue that is printing, or one long job (a slow printer can hold the agent busy for more than two
  minutes without a contact with the server), is waited for, up to two minutes.
- A job that was claimed between the last look and the stop is requeued and can print twice, as with
  every restart.
- The marker file now holds the installed version (`version=3.7.0`); an empty marker, which setups up
  to 3.6.0 wrote, still means "installed by the setup". A setup program that finds a newer agent than
  itself offers the installed version instead of its sections, and a deliberate **Go back to X**
  behind a confirmation. Going back needs `updates` on **Tell me** or **Off** first. The card does
  that for an automatic setting: it switches **Install automatically** to **Tell me**, so that the
  nightly update does not install the newer version again, and leaves **Tell me** and **Off** as they
  are. When the agent is stopped the window cannot read the mode, so the install reads `agent.ini`
  and lowers only an automatic setting; an `updates = off` stays off.
- Before you go back to 3.6.0 or older by other means, choose **Tell me** or **Off**. Otherwise the
  nightly entry stays behind, answers the command `restart --update now` with a usage error and does
  nothing; removing the agent then leaves the folder `.update` and `update-state.json`.
- A setup program from an older release cannot be stopped from putting an older agent back.
- The `.deb`, `.rpm` and `.msi` programs belong to the package manager or the installer, and an
  update never replaces them: it replaces the agent and the copy of the setup program in the install
  folder. A window started from such a program that is older than the installed agent shows only
  the card "A newer version is installed" (open the installed version, or go back on purpose). An AppImage stays an
  AppImage and an ELF program stays one; an AppImage is not started before it is put in place,
  because starting it as root needs FUSE.
- Every machine starts at the version its setup was. The first release that can be updated has to be
  installed by hand once; opening the newer setup offers **Install X from this setup** in one click.
- Only the server, its Docker image, the WooCommerce plugin and the Python agent are outside this.
  SumatraPDF is never updated.
- On the command line, an administrator can run `printpapi-agent restart --update now` (the look and
  install the nightly job does). It says nothing and does nothing, and exits 0, unless `updates = auto`
  with a valid folder, the agent is installed by the setup, connected, and a newer signed release is
  there; check `update-state.json` in the install folder for the last attempt. An older agent answers
  it with a usage line and does nothing.

## Managed Windows install (.msi)

For a fleet of Windows PCs that a deployment tool sets up (Intune, a script, a startup script).
`printpapi-setup-windows-x86_64.msi` (`printpapi-setup-windows-aarch64.msi` on ARM64; Beta like every
setup download) always installs the setup program for all users, to
`%ProgramFiles%\printpapi Agent Setup\`. It installs the agent as well, silently, only when the
public property `AGENT_INI` names a prepared `agent.ini`:

```bat
msiexec /i printpapi-setup-windows-x86_64.msi /qn /norestart /l*v C:\Windows\Temp\printpapi-msi.log AGENT_INI=%SystemRoot%\printpapi-deploy
```

(`%SystemRoot%` is `C:\Windows`; the command prompt fills it in. A tool that does not takes the path
spelled out.)

**One key and one `agent.ini` per PC.** The server ties a key to exactly one device, so a file that
many PCs share cannot work: the first PC registers, every other one is refused with the line "The
key is already used by another PC: one key per PC". Issue a key for each PC with the rights **Agent
only** (`POST /apikeys` with `{"label": "<COMPUTERNAME>", "scope": "agent"}`, see
[API keys](server.md#api-keys)) and write a file for it:

```ini
[agent]
server_url = https://print.example.com
api_key = <the key of this PC>
printers = Zebra GK420d ; HP LaserJet|pdf
```

`AGENT_INI` is the path of such a file, or of a folder. In a folder the install takes
`<COMPUTERNAME>.ini`, the file of the PC it runs on, so one folder can hold the files of all PCs. The
path must be absolute. A `name` in the file is used as it is; without one, a PC that has the agent
already keeps its installed name (a renamed PC must not register again under a new name), and a new
one takes the name of the computer. A `SumatraPDF.exe` next to the file is installed for the PDF
printers. It has to be **SumatraPDF 3.6.1, 64 bit**, the build that the setup window downloads:
take `SumatraPDF-3.6.1-64.exe` out of
[SumatraPDF-3.6.1-64.zip](https://www.sumatrapdfreader.org/dl/rel/3.6.1/SumatraPDF-3.6.1-64.zip)
and name it `SumatraPDF.exe`. Any other file, a newer version too, fails the whole install
(`msiexec` 1603; the log line names the SHA-256 that was expected). Without the file the PC has no PDF
printing from this install: its `|pdf` printers need SumatraPDF from somewhere else, next to
`printpapi-agent.exe`. There is no pairing code and no key in a property, on the command line or in
a log.

**Protect the file.** Until the install has read it, the key is in it as plain text, and the
install trusts what it finds. Make the folder where a standard user cannot make anything,
`%SystemRoot%\printpapi-deploy` (`C:\Windows`), in a session that runs as administrator, and let only
SYSTEM and the Administrators have it:

```bat
md %SystemRoot%\printpapi-deploy
icacls %SystemRoot%\printpapi-deploy /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F
```

Do not put the folder in `C:\ProgramData`: every user may create folders there, so somebody can make
`printpapi-deploy` before you, and no `icacls` line makes such a folder safe afterwards.
`/setowner` and `/reset` leave the folder open to whoever holds a handle on it from before, and with
`/T` they also walk through a junction that the user put inside it and change the owner and the rights
of whatever it points to. So a folder that somebody else made is not used and not repaired: the install
refuses it (below), and you delete it and make it again as administrator. Write the file from the same
elevated Command Prompt or PowerShell, so that the Administrators own it (files made from an elevated
Git Bash belong to you, and the install refuses them). The file belongs in a folder of its own, never in
a drive root such as `C:\`: the install refuses that too.

Best is a local folder per PC: the deployment tool copies the file there, runs `msiexec` and deletes
the file afterwards. A network share is discouraged: the install reads the file as the computer
account, so every computer of the domain that may read the share reads every key on it. What the
install checks, as SYSTEM: the path is not a link, the file is at most 256 KiB, and (for a file on
this computer) the owner of the file and the owner of the folder that holds it is SYSTEM or the
Administrators. The owner of the file stops somebody replacing it by a file of their own, the owner
of the folder stops a folder that somebody else made first. The install does not look at the access
list, so the place is what protects the file: a folder that standard users cannot make, as above. A
file on a share is not asked for its owner.

**What it does.** The agent, the tray icon and the scheduled task are installed as the setup app
does it, and the key is stored encrypted ([Where the key is stored](#where-the-key-is-stored)). The
file replaces the whole `agent.ini`: printers, headers and `timeout` that someone set later in the
settings window are replaced too. `updates = off` is written unless the file has an `updates` line:
the deployment tool owns the versions of a managed PC ([Updates](#updates)). An agent that the setup
did not install is not touched: the install stops with "An agent that was not installed by the setup is
already here. Remove it first." so a job is never printed twice. The PC must reach the server while
it installs (the agent registers first); offline, `msiexec` ends with 1603 and the deployment
tool tries again. A PC that already has the agent waits up to two minutes for a running print job to
end before the agent is stopped, so that the job is not printed twice; if it is still printing then,
the install fails with the line "The agent is printing a job" (1603) and the deployment tool tries
again. A stopped agent holds nothing up, an agent that is offline only when it is not printing. The
same wait comes before the agent refresh of an upgrade, and before the removal, which goes on after
two minutes anyway: removing the `.msi` never fails for a print (the log line then says that the agent
was removed while printing and that the job may be printed again). The wait comes after the new files
are unpacked, but it is not the last step: between the last look and the stop the agent still
registers with the server, normally a second or two and longer on a slow connection. A job that the
agent takes in that time is cut off and printed again by the server after five minutes.

**Variables.** Windows Installer starts the install as LocalSystem, but with the environment of whoever
started `msiexec`. The install drops it (and the current folder), takes the machine's and LocalSystem's
own variables and works in LocalSystem's profile folder, so a proxy for the registration has to be set
for the machine, not for your user.

**When it fails.** `msiexec` reports 1603. The install writes one line, without the key, to
`printpapi-managed.log` in the profile folder of the account that ran it: LocalSystem for `msiexec`
(normally `C:\Windows\System32\config\systemprofile\AppData\Local`; there is no log when that folder
cannot be made), your own `%LOCALAPPDATA%` when you typed the call yourself (below). A wrong call, such
as a relative `AGENT_INI`, leaves its usage line there too. The verbose log of `/l*v` shows the
resolved `AGENT_INI`. A first install that fails after the agent was put in place takes the agent out
again.

**Upgrade, repair, remove.**

- Running the same `.msi` again is maintenance mode and changes nothing, with or without
  `AGENT_INI`. A repair (`msiexec /fa`) with another `AGENT_INI` does not replace the configuration
  either. To change a PC's configuration without a new version, run the setup program from its folder as
  administrator with `--managed-install <ini or folder>`, or remove the `.msi` and install it again.
  The setup program has no console window, so a call typed into a prompt returns at once and shows
  nothing. Wait for it and read the exit code (0 done, 1 failed, 2 wrong call) like this:

  ```bat
  start /wait "" "%ProgramFiles%\printpapi Agent Setup\printpapi-setup.exe" --managed-install %SystemRoot%\printpapi-deploy
  echo %ERRORLEVEL%
  ```

  A failure line goes to `printpapi-managed.log` in the `%LOCALAPPDATA%` of the account you typed it
  in, not to the LocalSystem profile.
- A new `.msi` (another product code, the same upgrade code) is a major upgrade. With `AGENT_INI` the
  agent is installed again from that file; an agent that a white-label build took over (or whose marker
  cannot be read) is refused by that install, so the upgrade ends with 1603 and an "other build" line in
  `printpapi-managed.log`. Without it the agent program and the tray program are replaced
  and every setting stays; a newer installed agent is never replaced by an older one. The x64 and the
  ARM64 `.msi` are one product: the other architecture is an upgrade.
- Removing the `.msi` removes the agent that it installed, and only that one. The `.msi` leaves a
  flag file `.managed-install` next to the setup program to remember that it installed the agent. On a
  PC where it never did, removing the `.msi` keeps the agent. The other way round: when somebody
  removes the agent with the window and installs it again by hand, the flag is still there, so
  removing the `.msi` removes that agent too. Do not mix the two ways on one PC. An agent that a
  white-label build took over (its marker names a brand) stays, and so does its key: the removal logs
  "left alone" in `printpapi-managed.log` and succeeds. A refresh without `AGENT_INI` leaves such an
  agent, and one whose marker cannot be read, alone the same way.
- `--managed-refresh` and `--managed-uninstall` are what upgrade and removal run; they are not for
  typing. The `.msi` starts them only on a PC where the flag file `.managed-install` exists: on a PC
  that has the plain `.msi`, removing it never starts the unsigned setup program, so a policy or
  antivirus that blocks that program cannot make the removal fail.
- The install folder is fixed: `INSTALLDIR` on the command line or in a dialog is ignored, and the
  setup program refuses to run as an installer step from any other folder, because it runs as SYSTEM.
- Every managed install, refresh and removal ends all tray icons and open settings windows of the
  PC; they come back at the next logon.
- `AGENT_INI` is deliberately not a secure property: somebody who is not an administrator cannot hand
  it to an elevated install, and the command line of an administrator still can.

**Costs and limits.** The `.msi` adds a desktop shortcut (Tauri's stock template). It does not install
WebView2 (Windows 11 has it; the setup says where to get it otherwise). Neither the `.exe` nor the
`.msi` is code-signed: SmartScreen warns, and Smart App Control and WDAC block both, so there the
[Python agent](#locked-down-windows) stays the answer.

## Install

The agent is a single Rust binary, `printpapi-agent`. No Python, no runtime.

1. Download the archive for the machine from the
   [latest release](https://github.com/LukiPrince/printpapi/releases/latest) and unpack it. Each
   archive holds the binary, `agent.ini.example` and `LICENSE`:
   - `printpapi-agent-windows-x86_64.zip`, `printpapi-agent-windows-aarch64.zip` (ARM64)
   - `printpapi-agent-linux-x86_64.tar.gz`, `printpapi-agent-linux-aarch64.tar.gz`
   - `printpapi-agent-macos-aarch64.tar.gz` (Apple silicon), `printpapi-agent-macos-x86_64.tar.gz`
     (Intel)
2. Platform bits:
   - **Windows:** nothing for raw jobs — the agent talks to the spooler directly (no `pywin32`
     any more). For PDF printing, put `SumatraPDF.exe` next to `printpapi-agent.exe` (or on
     `PATH`) — it silent-prints through the installed driver.
   - **Linux:** CUPS (`lp` must work). Raw jobs go to the queue with `-o raw`; PDFs go through
     the CUPS filter chain.
   - **macOS:** same CUPS path as Linux, nothing to install (`lp` ships with the OS) — but read
     [macOS](#macos) below, raw printing has one setup trap.
3. Create `agent.ini` **next to the executable** (below; copy `agent.ini.example`). The agent finds
   it there whatever its working directory is.
4. Run `printpapi-agent` (`printpapi-agent.exe` on Windows; on Linux/macOS `chmod +x` it first if
   needed). It is a console program. For autostart see [Run as a service](#run-as-a-service).

On a locked-down Windows machine that blocks the exe, see [Locked-down Windows](#locked-down-windows).

Replacing a Python agent from v2? Keep its `agent.ini` (same `name` and `api_key`) so the machine
and its printer ids stay the same; the steps are in the [migration guide](migration.md#upgrade-the-agents).

## agent.ini

```ini
[agent]
server_url = http://yourserver:3460
api_key    = key-from-the-Keys-page
name       = office-pc
; printers: semicolon-separated. Append |pdf for document printers.
; A CUPS queue / Windows printer name, socket://IP:PORT for a raw network printer,
; or file:///path/to/dir to archive the job to disk instead of printing it.
printers   = Zebra GK420d ; HP LaserJet|pdf ; warehouse-label = socket://192.168.1.50:9100
```

### Optional: updates

`updates = notify` (default), `auto` or `off`, and `update_url` for a mirror: see [Updates](#updates).
Both are ignored by older agents and by the Python agent.

### Optional: timeout and extra headers

- `timeout = 60` in `[agent]` - socket timeout per request in seconds (default 60, minimum 30,
  because the server long-polls 25 s). Without it, a NAT flow that is silently dropped mid-poll
  would hang the agent forever while the dashboard already shows it offline.
- A `[headers]` section adds headers to every request. Use it when an auth proxy sits in front of
  the server's `/agent/` paths, for example a Cloudflare Access service-token policy:

  ```ini
  [headers]
  CF-Access-Client-Id     = <client id>
  CF-Access-Client-Secret = <client secret>
  ```

  `Authorization`, `User-Agent` and `Content-Type` are set by the agent and are refused here.

### Unattended machines

On start the agent registers until it succeeds (backoff 1 s … 5 min), so a machine that boots
before its network is up simply waits instead of exiting. Each failed register try, the first poll
error after a working stretch (and the matching "poll recovered"), a rejected `agent.ini` and every
crash are appended to `%LOCALAPPDATA%\print_agent-error.log` (Windows) or the temp dir - that file
is the first thing to read over remote desktop. (An agent the setup installed writes it next to
its `status.json` instead, see [Tray icon and settings](#tray-icon-and-settings), and the settings
window shows it under Log.)

`api_key` is a key issued for the agent — on the dashboard's **Keys** page or with
`POST /apikeys` — and the agent lands in that key's org. A made-up key is refused (`401`) since
v3; agents that enrolled with one under v2 keep working, and `PRINTAPI_OPEN_ENROLLMENT=1` on the
server restores the old rule. Issue one key per machine, with the rights **Agent only**: it can
enrol, poll, download and report its own jobs and list the org's computer names (the setup app
needs that), nothing else, so a lost machine cannot submit jobs or read the history. A key for printing, reading or Star printers is refused at enrolment, and
the setup app says so in step 1. Revoking the key on the Keys page stops that agent at once.

A machine that is already set up keeps its name and printer ids when you narrow its key (**Rights**
on the Keys page, or `PUT /apikeys/{id}`), so there is no need to enrol it again. Narrowing
Everything to Agent only interrupts nothing: the agent keeps its right. Only if you take the agent
right away (Print only, Read only, CloudPRNT only) does the computer stop at its next poll; a job it
has already been handed still finishes, and queued jobs wait until it has the right again or they
expire. A computer named `cloudprnt-...` (reserved for Star printers) can only be narrowed to a
right set that is not Agent only. See [Key rights](api.md#key-rights).

When exposing the server to agents outside your LAN, publishing only `/agent/*` (behind TLS) is
still the smallest surface. `POST /agent/pair` is one of them, so [a pairing from the
terminal](#pair-from-the-terminal) works through it; the setup window also needs `/health` and
`/computers`.

### Pair from the terminal

For a machine without a screen, a Python agent or a locked-down Windows, the same code works
without the window. `printpapi-agent pair` is part of the agent (an agent as new as the server;
an older one just starts and ignores the word). Run it in the agent's folder, with administrator
rights or `sudo` where that folder is not yours (the setup's install: `/opt/printpapi-agent`,
`C:\Program Files\printpapi-agent`):

```bash
sudo ./printpapi-agent pair              # Linux, macOS
```
```powershell
.\printpapi-agent.exe pair                # Windows, from an administrator prompt
```

```text
usage: printpapi-agent pair [--server URL] [--name NAME] [--printers LIST] [--keep-headers] [--yes] [--insecure]
```

It asks for the code or link on the prompt (never as an argument: an argument stands in the process
list and the shell history; a code or link on the command line is refused). It then shows the
account, the person who made the code and the device, and asks `Continue? [y/N]`; `--yes` skips the
question. A bare code uses `--server`, else the `server_url` of the existing `agent.ini`, else asks.
`--name` is the device name (the existing one, else the computer's name); a Reconnect code ignores it.
`--printers` writes the `printers` line (the grammar of [Printer syntax](#printer-syntax)). Plain
`http` outside the private network needs `--insecure`.

- **The file is prepared before the code is spent**: if the new `agent.ini` cannot be made (no
  rights; the message then says to run it as administrator or with `sudo`), nothing is used up. If
  `agent.ini` changes while the command waits for the code (the settings window saved something),
  it stops before the code is spent and asks you to run it again, so that change is not overwritten. After the question the file is replaced in one step, keeping its
  owner and rights (Windows: `ReplaceFileW`, which keeps the access list), comments, timeout and
  printers. A failing replace or an answer of no leaves the old file as it was; the key then ends by
  itself within 24 hours, or at once on the Keys page.
- **Headers** are sent and kept only when the server is the one already in `agent.ini`; for another
  server they are neither sent nor kept, and the command prints which it dropped. `--keep-headers`
  overrides that.
- It registers the key with **no printers** right away: that binds the key (or marks a Reconnect
  takeover as pending) without touching a printer or the running agent. If that call fails the
  command says so; then start the agent within 24 hours, or the unused key is revoked.
- On an install made by the setup (the marker file is there) the key is stored sealed as setup does
  ([Where the key is stored](#where-the-key-is-stored)), unless `seal_key = no`. An `agent.ini` that
  is a link is refused. The command takes the update lock when the installation has one.
- The output holds neither key nor code. The last line names the command that restarts the agent on
  this system: `systemctl restart printpapi-agent`, `schtasks /end` and `/run`, or
  `launchctl kickstart -k`. With no `printers` line it also names the printers found on this computer.

Without the agent's help, with `curl` (Linux, macOS; the Python agent too):

```bash
umask 077
curl -sS https://print.example.com/agent/pair -H 'Content-Type: application/json' -o pairing.json -d @- <<'EOF'
{"code":"7K3M-Q2ZD-9XHT","name":"warehouse-pi"}
EOF
```

and PowerShell (an administrator prompt; the file is readable for SYSTEM and Administrators only):

```powershell
$out = Join-Path $PWD 'pairing.json'
New-Item $out -ItemType File | Out-Null
icacls $out /inheritance:r /grant:r '*S-1-5-18:F' '*S-1-5-32-544:F'
$body = @{ code = (Read-Host 'Pairing code'); name = 'warehouse-pc' } | ConvertTo-Json
Invoke-RestMethod https://print.example.com/agent/pair -Method Post -UseBasicParsing `
  -ContentType 'application/json; charset=utf-8' -Body $body -OutFile $out
```

`-OutFile` keeps the access list of the file that exists. Copy the `key` from `pairing.json` into the
`api_key` line of `agent.ini`, delete `pairing.json`, and start the agent **within 24 hours**, or
the key is revoked. For a Reconnect code the reply says `"replaces": true` and the name of the
device; use that name in `agent.ini`: the server refuses that key under any other name. `Invoke-RestMethod` does not work in PowerShell's
ConstrainedLanguage mode (WDAC): use the `pair` command or the Python agent's own machine.

### Where the key is stored

The agent key is the one secret on the machine. Where it lives depends on how the agent was
installed:

| Installed | The key is in `agent.ini` as | Who can read the file |
|---|---|---|
| Windows, with the setup app | `api_key_dpapi`: encrypted with DPAPI, machine scope (this version and later) | SYSTEM and Administrators |
| By hand on any system, and the Python agent | `api_key`, plain text | what you set; a manual install keeps its plain `api_key` and the agent never rewrites the file |
| Linux, with the setup app | `api_key`, plain text | `root` and the service user (`root:printpapi-agent`, mode 0640) |
| macOS, with the setup app | `api_key`, plain text | `root` (mode 0600) |

**What DPAPI does.** The setup stores the key with the Windows Data Protection API in machine
scope: the key is encrypted for this Windows installation. A copy of `agent.ini` that leaves the
machine by itself (a folder backup, a file collected by remote management, a mail to support, the
folder copied to a second PC) holds no usable key, and two PCs cannot end up fighting over one
device. **What it does not do.** Every program that runs on that computer can ask Windows to
decrypt a machine-scope value, so the rights on the file are still the protection between local
users: only SYSTEM and Administrators may read `agent.ini`. A local administrator can read the key
anyway. A stolen disk is answered by disk encryption (BitLocker on Windows, LUKS on Linux, FileVault on macOS), not by this. The values under
`[headers]` stay plain text in `agent.ini`. If a machine is lost, revoke its key on the **Keys**
page.

**How to see whether the key is stored encrypted.** `agent.ini` (as an administrator) has an
`api_key_dpapi` line and no `api_key` line. If encrypting fails when the setup installs over a file
that was not encrypted yet (rare), the install carries on with the plain `api_key` under the same
rights rather than fail on a print station. A file that was encrypted becomes plain text in three
cases only: you run `unprotect-key` (below), the file says `seal_key = no` (at the next change in
Settings), or you type a new key in Settings, Connection and Windows cannot encrypt it. In the last
case the install still goes through, with the new key as plain text; the `api_key_dpapi` line tells
you which state the file is in. A change of the printers or the timeout never does this: if the key
cannot be encrypted again, nothing is changed.

**Rules the agent follows.**

- A non-empty `api_key` always wins over `api_key_dpapi`. That is also the recovery by hand: add
  `api_key = ...` as an administrator and the agent uses it.
- `seal_key = no` in `[agent]` (also `false`, `off` or `0`) keeps the setup from encrypting the key
  again. An existing `api_key_dpapi` is still read. A change in Settings never sets or removes this
  line; `unprotect-key` (below) sets it.
- Every install and every change in Settings encrypts the key again, unless `seal_key = no` is
  set. An agent that was installed by an earlier version has its key encrypted at the next change
  or update; the key, the device and the printer ids stay the same.
- The agent itself never writes `agent.ini`. Settings shows the header values by having an
  elevated command copy only the `[headers]` into the working folder, never the key.

**Linux and macOS keep the file.** On Linux a key sealed to the host would need `systemd-creds`
(systemd 250 and later; Ubuntu 22.04 ships 249), and the agent runs as an unprivileged user that
cannot decrypt it by itself. A credential file that is missing stops the unit before the agent can
log anything. Disk encryption (LUKS) also covers the print jobs and the headers. On macOS a daemon
can only use the file-based keychain, whose access lists an unsigned program that changes at every
update cannot satisfy unattended
([Apple TN3137](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)); for a
stolen Mac, turn on FileVault. macOS is looked at again together with code signing.

#### Before you replace or reinstall this PC, or rely on a backup of it

A backup of an encrypted `agent.ini` restores only on the same Windows installation. What to do
when the key is lost is in [When the key cannot be read](#when-the-key-cannot-be-read). Before you
reinstall Windows, move to new hardware or rely on a file backup of this folder, do one of these:

- keep the key in a password manager (it is the one from the **Keys** page), or
- run `printpapi-agent unprotect-key` (below) and back up the file afterwards.

A plain backup made before this version stays valid, because the key never changed. To encrypt the
key again later, delete the `seal_key` line as an administrator and make any change in Settings.

#### Going back to an older version or to the Python agent

An older agent and the Python agent read only `api_key`. Before you go back, as an administrator in
a console, run the agent that sealed the file; a freshly downloaded `printpapi-agent.exe` in any
folder will do, it works on the installed folder:

```powershell
& "C:\Program Files\printpapi-agent\printpapi-agent.exe" unprotect-key
```

It writes the key back as plain text, sets `seal_key = no` so later changes keep it that way, and
prints no key. The running agent needs no restart. (An older exe does not know the word and just
starts the agent, so use a current one.)

On a PC that blocks the exe, read the key with the signed Python interpreter from an administrator
console, and put it into `agent.ini` as `api_key`:

```python
# printpapi: show the stored agent key
import ctypes, re
from ctypes import wintypes

class Blob(ctypes.Structure):
    _fields_ = [("size", wintypes.DWORD), ("data", ctypes.c_void_p)]

ini = open(r"C:\Program Files\printpapi-agent\agent.ini", "rb").read()
raw = bytes.fromhex(re.search(rb"(?m)^api_key_dpapi *= *([0-9a-f]+)", ini).group(1).decode())
src, out = Blob(len(raw), ctypes.cast(raw, ctypes.c_void_p)), Blob()
if not ctypes.windll.crypt32.CryptUnprotectData(
        ctypes.byref(src), None, None, None, None, 1, ctypes.byref(out)):
    raise ctypes.WinError()
print(ctypes.string_at(out.data, out.size)[32:].decode())
```

#### When the key cannot be read

This happens when `agent.ini` was copied from another Windows installation, after a Windows
reset, or when the value is damaged. A setup-installed agent does not end then: it shows "offline"
with the reason in Overview and writes it once to its log, and it looks at `agent.ini` again every
minute. So a short DPAPI failure heals by itself, and a key you put in by hand works within a
minute without a restart. `status.json` says so with `key_unreadable`. An agent set up by hand
ends at once with the text on its console.

1. Create a code with **Reconnect** on this device in the dashboard, then Settings, Connection:
   enter it. The device keeps its name and printer ids, the printers are filled in from the status
   file and the timeout stays. Or enter the server and the key again: the window lets you keep the
   old name, because it is the same key. A pairing to the same server keeps the headers, to another
   server it removes them (see above); a typed address and key keep them.
2. By hand: as an administrator add `api_key = ...` to `agent.ini`. This is the way for an agent
   with `[headers]` (an access proxy), because the window's connection check sends no headers. If
   the stored key keeps failing on that PC, add `seal_key = no` too, or the next change in
   Settings encrypts it again.
3. Only when a code is out of the question, create a new key on the **Keys** page and use a **new**
   device name: the old device stays in the dashboard and the printers get new ids. Reconnect (1)
   keeps both.

### Printer syntax

`name [|pdf] [= target]`, semicolon-separated:

- Plain name → a Windows printer name or CUPS queue; jobs print through it.
- `|pdf` → declares the printer PDF-capable. **Default is raw-only**, so a label printer is
  never sent a PDF by accident.
- `= socket://host:port` → the agent opens a raw TCP socket (e.g. a network label printer's
  `:9100`). Always raw-only — a bare socket has no renderer.
- `= file:///path/to/dir` → the agent writes the job into that directory instead of printing it
  (see [File output](#file-output-virtual-print-server)). Always takes both `pdf` and `raw`.

## Labels vs documents (the one rule)

A label printer **cannot parse a PDF** — send it raw PDF bytes and it form-feeds blank labels.
printpapi keeps the paths separate: `raw` jobs (ZPL/ESC-POS) go straight to the printer;
`pdf` jobs are rendered first (SumatraPDF on Windows, CUPS on Linux and macOS). Mark only real document
printers with `|pdf`.

## Printer setup by family

Which path a printer takes follows from **who renders**: a printer with its own page language
(Zebra/ZPL, ESC/POS) takes `raw` and needs no rendering; everything else needs a driver and takes
`pdf`. Get that wrong and you print blanks (gotcha #1).

### Zebra and other ZPL printers — `raw`

The printer parses ZPL itself, so the driver is almost irrelevant.

- **Network model (the easy one):** don't install anything. Point the agent straight at the
  printer: `warehouse-label = socket://192.168.1.50:9100`. No driver, no queue, no spooler — works
  the same on Windows, Linux and macOS.
- **USB model:** install Zebra's driver (*Zebra Setup Utilities* / ZDesigner on Windows,
  `lpadmin -m raw` or the Zebra CUPS driver elsewhere) so the OS has a printer object; the agent
  writes RAW bytes into it, the driver never re-renders. List it **without** `|pdf`.
- **Test it** before wiring up printpapi —
  `printf '^XA^FO50,50^A0N,40,40^FDprintpapi ok^FS^XZ' | lp -d Zebra -o raw` (Linux/macOS), or on
  Windows just use the dashboard's *Devices → Test print*, which sends the equivalent one-line
  ZPL label (`^XA^FO40,40^ADN,36,20^FDprintpapi test^FS^XZ`) to any raw-only printer.
- Label geometry lives **on the printer**, not in the job: `^PW` (width), `^LL` (length), `~SD`
  (darkness), and `~JC` to recalibrate the media sensor after a roll change.

### DYMO LabelWriter — `pdf`, not raw

A LabelWriter does **not** speak ZPL. It is a raster device driven entirely by its driver, so it
belongs on the PDF path:

1. Install the driver — *DYMO Connect* (Windows/macOS), `dymo-cups-drivers` (Linux).
2. In `agent.ini` mark it PDF-capable: `DYMO LabelWriter 550|pdf`.
3. Send a PDF whose page size *is* the label (e.g. 89 × 36 mm for an address label) — scaling a
   sheet-sized PDF onto a label is what produces the tiny-print-in-the-corner classic.
4. Pick the label via `options.paper`. Don't guess the name: `GET /printers` reports the driver's
   own `capabilities.papers`, and only those values are accepted by the driver.
5. If a wide label comes out turned and small on Windows, send `options.orientation: "landscape"`.
   See [api.md](api.md#submitting-a-job).

Brother QL and other raster label printers work the same way — driver + `|pdf`.

### ESC/POS receipt printers — `raw`

Same as Zebra: the printer interprets the byte stream. Network models take
`socket://IP:9100`; USB models go through the vendor driver as a RAW target. The agent passes the
bytes through. You can build the ESC/POS stream yourself, with any `escpos` library, and send it
as `raw_base64`; or send a JSON `receipt` job and let the **server** lay it out (texts, columns,
logo, QR code, barcode, cut, cash drawer, with umlauts and the euro sign drawn right): the agent
then gets ordinary `raw` bytes and needs no change. See [receipts.md](receipts.md). The printer
must support `GS v 0` raster graphics; a driver-based receipt printer that is also marked `|pdf`
should have `"output": "escpos"` in its receipt profile.

### Star CloudPRNT models — no agent at all

Star's network printers (mC-Print2/3, TSP100IV, mC-Label3, HI01X/HI02X) can poll the server
themselves, so they need no agent on any machine: point the printer's CloudPRNT setting at
`https://your-server/cloudprnt/<client-key>` and it enrols itself as a raw-only printer. Built to
the published protocol but not yet verified on a device — see [cloudprnt.md](cloudprnt.md).

## File output (virtual print server)

A `file://` target makes the printer a **directory**: the job is written to disk instead of paper.
For archival (keep a copy of every label), for feeding a document pipeline (drop the PDF into
Paperless-ngx's consume folder), and for testing an integration without burning a roll of labels.

```ini
printers = Zebra GK420d ; archive = file:///srv/paperless/consume
```

- Windows spelling: `archive = file:///C:/printpapi/out` (forward slashes, drive letter after
  `file:///`). `%20` escapes are decoded, so a path with spaces works.
- The directory is created if missing.
- One file per job, named after the job id: `job-42.pdf` for a `pdf` job, `job-42.prn` for a `raw`
  one (the exact bytes you submitted — ZPL, ESC/POS). `copies=3` writes `job-42.pdf`,
  `job-42-2.pdf`, `job-42-3.pdf`.
- A receipt job to a `file://` printer writes a viewable `job-42.pdf` (the printer takes PDF), so a
  directory is a virtual receipt printer to test with; `"output": "escpos"` in its
  [receipt profile](receipts.md#printer-profile) writes the `.prn` instead.
- A file printer accepts **both** modes and needs no `|pdf` tag — a directory has no renderer to
  get wrong. Print `options` (duplex/tray/…) are hardware settings and are ignored here.
- Nothing changes server-side: it is a normal printer on the Devices page, jobs report `done` when
  the file is written, and a write error (permissions, disk full) fails the job with the OS error.

## macOS

macOS printing *is* CUPS, so the agent takes the same path as Linux — it picks `lp` for anything
that isn't Windows, and `lpoptions -p <queue> -l` reports capabilities. Nothing to install.

The one trap is **raw printing**. A label printer added through the normal *Printers & Scanners*
dialog usually lands as a driverless/AirPrint queue, and that queue's filter chain mangles or
rejects ZPL/ESC-POS — the labels come out blank or as literal `^XA` text. Two ways out:

- **Network label printers: skip CUPS.** Use `name = socket://IP:9100` in `agent.ini`. The agent
  opens the socket itself, so no queue, no driver, no filter — the most reliable option on macOS.
- **USB label printers: add a raw queue.** Find the device URI, then create the queue without a
  driver:

  ```sh
  lpinfo -v                                     # e.g. usb://Zebra%20Technologies/ZTC%20GK420d
  sudo lpadmin -p Zebra -E -v 'usb://Zebra%20Technologies/ZTC%20GK420d' -m raw
  lp -d Zebra -o raw label.zpl                  # verify before wiring up the agent
  ```

  If your macOS build rejects `-m raw`, install the vendor's CUPS driver (Zebra/DYMO ship one)
  and keep sending raw — a vendor queue passes ZPL through.

Document printers (`|pdf`) need no special setup: CUPS renders the PDF through the installed
driver, exactly as on Linux.

## Run as a service

**Linux — systemd** (`/etc/systemd/system/printpapi-agent.service`):

```ini
[Unit]
Description=printpapi agent
After=network-online.target cups.service

[Service]
ExecStart=/opt/printpapi/printpapi-agent
Restart=always
RestartSec=5
User=printpapi

[Install]
WantedBy=multi-user.target
```

`agent.ini` lives next to the binary (`/opt/printpapi/agent.ini`), so no `WorkingDirectory` is
needed. `sudo systemctl enable --now printpapi-agent`. The service user needs no special rights —
any local user may submit to CUPS.

The agent keeps its [print journal](#the-print-journal) next to the binary. When the service user
cannot write that folder (the usual case with a root-owned `/opt/printpapi`), undelivered results are
kept in memory only and a restart loses them. To have them survive, add one line under `[Service]` of
your own unit file:

```ini
StateDirectory=printpapi-agent-manual
```

then `sudo systemctl daemon-reload` and restart the service. systemd makes `/var/lib/printpapi-agent-manual` for the
service user and the agent uses it through `$STATE_DIRECTORY`. Put the line into the unit file itself,
not into a drop-in: the setup app uses the same unit name, and a drop-in left behind would apply to its
unit. Do not use the name `printpapi-agent` (the setup's status folder, which systemd would hand to the
service user) or `printpapi` (the server's data folder, [migration](migration.md)).

**macOS — launchd** (`/Library/LaunchDaemons/com.printpapi.agent.plist`, root-owned, mode 644):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.printpapi.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/printpapi/printpapi-agent</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/var/log/printpapi-agent.log</string>
  <key>StandardErrorPath</key><string>/var/log/printpapi-agent.log</string>
</dict>
</plist>
```

`sudo launchctl bootstrap system /Library/LaunchDaemons/com.printpapi.agent.plist`
(`sudo launchctl load -w <plist>` on older macOS). A daemon can print: CUPS queues are
system-wide.

**Windows — Task Scheduler** (in PowerShell run as administrator):

```powershell
$action   = New-ScheduledTaskAction -Execute "C:\printpapi\printpapi-agent.exe"
$trigger  = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries -RestartCount 255 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName printpapi-agent -User "NT AUTHORITY\SYSTEM" -RunLevel Highest `
  -Action $action -Trigger $trigger -Settings $settings
```

`-ExecutionTimeLimit 0` is the setting that matters: without it Windows stops the task 72 hours
after it started ([ExecutionTimeLimit](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-executiontimelimit)),
and `schtasks /create` has no switch for it. The two battery switches keep the agent running on
an unplugged laptop, and the restart settings bring it back a minute after a crash. The setup app
creates the same task.

If you created the task with `schtasks /create`, as this page said before, it stops after three
days. Run the block above again with `-Force` added to `Register-ScheduledTask`, which replaces
the task, then restart the computer or run `Stop-ScheduledTask printpapi-agent` and
`Start-ScheduledTask printpapi-agent`.

Caveat: printers installed *per user* are invisible to `SYSTEM`. Either install the printer for
all machine users, or create the task under the operator's account with "run whether user is
logged on or not". In the Task Scheduler window, also untick "Stop the task if it runs longer
than" on the Settings tab. The agent is a console program; under "run whether user is logged on
or not" no window shows. `nssm install printpapi-agent C:\printpapi\printpapi-agent.exe` works
too if you want real service semantics (auto-restart, `sc` control).

## Locked-down Windows

Smart App Control / WDAC / AppLocker block unsigned executables, and the Rust agent exe is **not
code-signed yet**, so such machines refuse to start it. Until signed builds exist, use the Python
agent, which stays available as the *legacy agent* (`agent/print_agent.py` in the repo, unchanged
and still tested in CI). It runs through the signed Python interpreter — `pythonw.exe` is
PSF-signed, and the `.py` file is data, not an executable:

1. Copy `agent/print_agent.py` to the machine (Python 3.9+; every release also has it as
   `printpapi-agent-python.zip`) and `pip install pywin32`. For PDF
   printing put `SumatraPDF.exe` next to the script (or on `PATH`).
2. Create `agent.ini` next to the script — same format and keys as above.
3. Run `python print_agent.py`, and for autostart create the task from
   [Run as a service](#run-as-a-service) with the interpreter as the program:

   ```powershell
   $action   = New-ScheduledTaskAction -Execute "C:\Program Files\Python312\pythonw.exe" `
     -Argument "C:\printpapi\print_agent.py"
   $trigger  = New-ScheduledTaskTrigger -AtStartup
   $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries `
     -DontStopIfGoingOnBatteries -RestartCount 255 -RestartInterval (New-TimeSpan -Minutes 1)
   Register-ScheduledTask -TaskName printpapi-agent -User "NT AUTHORITY\SYSTEM" -RunLevel Highest `
     -Action $action -Trigger $trigger -Settings $settings
   ```

   (`nssm install printpapi-agent <pythonw.exe> <script>` works too; the per-user printer caveat
   and the note on tasks made with `schtasks /create` above apply.)

A key that the setup stored encrypted has to be turned back into plain text first, see [Going back to an older version or to the Python agent](#going-back-to-an-older-version-or-to-the-python-agent).

Switch to the Rust agent once a code-signed build is released.

## Reliability

### The print journal

A result the agent could not deliver must not make the job print a second time. So the agent keeps
a journal, `print-journal.jsonl`: as soon as at least one copy of a job was handed over to the printer without an error,
the outcome of the job (done, or the error of a later copy) is written to that file and flushed to
disk **before** it is reported to the server. Then:

- The report is tried five times (1, 2, 4 and 8 seconds apart). If it still fails, the entry stays
  and is sent again before every poll, also after the agent was restarted or the machine rebooted.
  After a failed pass the agent waits 30 seconds, then twice as long each time, at most 10 minutes.
- If the server hands that same job out again (its reaper requeued it, because the report never
  arrived), the agent reports the recorded outcome and prints nothing.
- A job that takes longer than the server's five minutes to print is still finished: the server
  accepts the result of a job its reaper has requeued or even failed. See
  [the job lifecycle](api.md#job-lifecycle).

A job is recognised by an identity (`job_uid`) that the server derives from the job's number and creation time, so a
server whose database was reset or restored from a backup cannot make the agent mistake a new job
for an old one. In every doubtful case the agent prints.

The journal holds, per job: its number and identity, whether it worked, the error text (masked,
cut to 500 characters, never the key) and whether the server has confirmed it. Never the
document, the title, a header value or the key. It is a text file of one JSON object per line,
at most 1000 jobs and 16 MiB, compacted by the agent itself; delete it, with the agent stopped,
to make the agent forget. It is only used between a server and an agent of this version: an older
server hands out no identity, and then the agent works as before, as does the legacy Python agent.

Where it lives: an agent that the setup installed keeps it next to `status.json` in its status folder (see
[Tray icon and settings](#tray-icon-and-settings)); an agent you started by hand keeps it in the
folder of the program and `agent.ini`, or under systemd with `StateDirectory=` in that folder. If
the folder cannot be written the agent says so once in the crash log and keeps the results in
memory only, which a restart loses. The journal is never put into the temp folder.

**What is not covered** (the job can still print twice, or a label or a result can be missing):

- A crash or power cut while a job is being printed: the whole job prints again.
- A first copy that failed although paper came out (SumatraPDF stopped after 60 seconds, a socket
  cut in the middle), when the failure report was lost as well: the job is tried again.
- A power cut can lose labels that were handed over but still waited in the spooler, the CUPS queue
  or a printer's buffer. "Printed" means "handed over without an error", for the journal as it
  always did for `done`.
- A lost journal: a folder that cannot be written followed by a restart, a full disk followed by a
  restart, the agent removed and installed again.
- An old server (it hands out no identity) and the legacy Python agent.
- Two installations that share one key: each has its own journal.
- A result that arrives after the server's retention removed the job (the server answers
  `state: null` and the agent drops it from the pending list; an `ok` result stays in the journal
  as confirmed) or cleared its callback (no webhook).
- A failure report that the server accepted but whose reply was lost, followed by setting the job
  back to `queued` by hand in SQLite before the agent sent it again: the agent reports the
  recorded failure and prints nothing. Submit a new job.
- Going back to an agent from before the journal: it does not read the journal and sends no job
  identity, so a job whose result was still pending can print again. Let the current agent run
  with the server reachable first (the crash log says "delivered late" for each result), or accept
  the duplicate.
- A server database that was reset is told apart from the old one by the job number together with
  its creation time, so a frozen clock plus a reset can collide.

A job that is set back to `queued` by hand after it printed is answered from the journal; submit a
new job to print it again. A failure that the server has confirmed to the agent is forgotten, so
a job that failed and is handed out again prints.

### Other safeguards

- If the agent dies mid-job, the server's reaper requeues the job after the visibility timeout.
- Crash log: `%LOCALAPPDATA%\print_agent-error.log` (Windows) / `print_agent-error.log` in the
  temp dir (Linux, macOS) - see [Unattended machines](#unattended-machines) for what gets logged.
