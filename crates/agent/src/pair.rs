// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Pairing: connect this computer to a server with a one-time code or link instead of a key typed
//! by hand. `parse` is the only parser of a pairing link in the product (the setup window, the tray
//! and the `pair` subcommand all call it, so the address that is shown is the address that is
//! used), `redeem` spends the code at `POST /agent/pair`, and `cli` is `printpapi-agent pair`.
//! Protocol and limits: docs/api.md, "Pairing a device".

use std::fmt;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde_json::{Value, json};

use crate::client::{Http, HttpError, http_status};
use crate::config::{Config, HttpSettings, IniUpdate, parse_printers, update_ini};
use crate::setup::{explain, needs_insecure_confirm, normalize_server_url, same_origin};

/// The URL scheme of a pairing link. Fixed, never branded: the unbranded server mints these links.
pub const SCHEME: &str = "printpapi";

const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const MAX_LINK_BYTES: usize = 2048;
const MAX_SERVER_BYTES: usize = 300;

/// What a link or a typed code says. `code` is `XXXX-XXXX-XXXX`. `server` is the normalised
/// address of a link, `None` for a bare code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub server: Option<String>,
    pub code: String,
}

/// A code as the server reads it (`auth::normalize_pairing_code`; the crates share no library, so
/// the rules are repeated, and the golden values in both test suites keep them in step): spaces and
/// dashes dropped, upper case, `O` is `0`, `I` and `L` are `1`, exactly 12 symbols of the alphabet.
fn normalize_code(s: &str) -> Option<String> {
    let mut symbols = String::with_capacity(12);
    for c in s.chars() {
        if c.is_ascii_whitespace() || c == '-' {
            continue;
        }
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !CODE_ALPHABET.contains(&(c as u8)) || symbols.len() == 12 {
            return None;
        }
        symbols.push(c);
    }
    (symbols.len() == 12).then(|| format!("{}-{}-{}", &symbols[..4], &symbols[4..8], &symbols[8..]))
}

/// `%XX` decoded once, strictly: a `%` without two hex digits, or bytes that are no UTF-8, make it
/// `None`.
fn percent_decode_strict(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%'
            && !(bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return None;
        }
    }
    percent_decode_str(s)
        .decode_utf8()
        .ok()
        .map(|c| c.into_owned())
}

/// The address of a server as a link or a typed field may carry it: what `normalize_server_url`
/// accepts, and in addition ASCII only, at most 300 bytes, and none of `@ ? # % " \\`, whitespace or
/// control characters. No userinfo, so `https://print.example.com@evil.example` cannot show one host
/// and reach another; no `%`, so decoding twice or never gives the same address (Windows hands the
/// URI over decoded or not, depending on the caller). One rule for a link, the window and the
/// command line.
pub fn valid_server(raw: &str) -> Option<String> {
    let url = normalize_server_url(raw).ok()?;
    let clean = url.len() <= MAX_SERVER_BYTES
        && url.is_ascii()
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "@?#%\"\\".contains(c));
    clean.then_some(url)
}

/// A pairing link or a code typed or pasted. At most 2048 bytes, trimmed.
///
/// - Text that does not start with `printpapi:` (any case) is a bare code, read as the server reads
///   one: `7k3m q2zd 9xht` works. `server` is `None`.
/// - A link has no control character, whitespace, `"` or `\`, then `//pair`, an optional `/`, `?`,
///   and exactly the parameters `server` and `code`, once each, in any order. Anything else (another
///   parameter, a fragment, another path) is `None`.
/// - Values are percent-decoded once; the decoded server goes through [`valid_server`].
pub fn parse(text: &str) -> Option<Pairing> {
    let text = text.trim();
    if text.len() > MAX_LINK_BYTES {
        return None;
    }
    let scheme = text.get(..SCHEME.len() + 1);
    if !scheme.is_some_and(|s| s[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) && s.ends_with(':')) {
        return Some(Pairing {
            server: None,
            code: normalize_code(text)?,
        });
    }
    if text
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '"' || c == '\\')
    {
        return None;
    }
    let rest = &text[SCHEME.len() + 1..];
    let rest = rest.strip_prefix("//")?;
    let host = rest.get(..4)?;
    if !host.eq_ignore_ascii_case("pair") {
        return None;
    }
    let rest = &rest[4..];
    let query = rest.strip_prefix('/').unwrap_or(rest).strip_prefix('?')?;
    let (mut server, mut code) = (None, None);
    for part in query.split('&') {
        let (k, v) = part.split_once('=')?;
        let slot = match k {
            "server" => &mut server,
            "code" => &mut code,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        let v = percent_decode_strict(v)?;
        // an encoded space or line break is no more welcome than a plain one
        if v.chars().any(|c| c.is_control() || c.is_whitespace()) && k == "server" {
            return None;
        }
        *slot = Some(v);
    }
    Some(Pairing {
        server: Some(valid_server(&server?)?),
        code: normalize_code(&code?)?,
    })
}

/// Everything but the unreserved characters of RFC 3986 is escaped.
const LINK_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The link a dashboard shows for `server` and `code` (the same bytes as `pairingLink` in
/// `web/lib/pairing.ts`; both tests hold the same golden value).
pub fn link(server: &str, code: &str) -> String {
    format!(
        "{SCHEME}://pair?server={}&code={}",
        utf8_percent_encode(server, LINK_VALUE),
        utf8_percent_encode(code, LINK_VALUE)
    )
}

/// What `POST /agent/pair` gave. No `Debug`: it holds the key.
pub struct Paired {
    pub key: String,
    pub name: String,
    pub org: String,
    pub created_by: Option<String>,
    pub replaces: bool,
}

/// Why the code could not be spent (or the server not reached before that).
#[derive(Debug, Clone, PartialEq)]
pub enum PairError {
    BadUrl,
    /// 401: wrong, expired, used or cancelled; the server tells no more.
    BadCode,
    /// 409: another device of this account has the name. The code is still good.
    NameTaken,
    BadName,
    Throttled,
    /// 404: a server before pairing codes.
    OldServer,
    /// 403, or a login page: something in front of the server answered.
    Proxy,
    Unreachable(String),
    BadReply,
}

impl PairError {
    /// A stable word for the window to branch on.
    pub fn code(&self) -> &'static str {
        match self {
            PairError::BadUrl => "bad_url",
            PairError::BadCode => "bad_code",
            PairError::NameTaken => "name_exists",
            PairError::BadName => "bad_name",
            PairError::Throttled => "throttled",
            PairError::OldServer => "old_server",
            PairError::Proxy => "proxy",
            PairError::Unreachable(_) => "unreachable",
            PairError::BadReply => "bad_reply",
        }
    }
}

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PairError::BadUrl => f.write_str("The server address must start with http:// or https:// and have no user name, ?, # or %."),
            PairError::BadCode => f.write_str("This pairing code is wrong, expired or already used. Create a new one on the Devices page."),
            PairError::NameTaken => f.write_str("A device with this name already exists on this server. Pick another name, or create the code with Reconnect on that device to keep its printers."),
            PairError::BadName => f.write_str("The server does not accept this device name, or this code cannot take over the device it names. Pick another name, or make a new code on the Devices page with Connect device."),
            PairError::Throttled => f.write_str("Too many attempts. Wait 15 minutes and try again."),
            PairError::OldServer => f.write_str("This server is too old for pairing codes. Use an address and a key instead."),
            PairError::Proxy => f.write_str("A proxy in front of the server refused this. Set up the agent by hand with [headers], see docs/agent.md."),
            PairError::Unreachable(why) => write!(f, "Could not reach the server: {why}"),
            PairError::BadReply => f.write_str("This address does not answer like a printpapi server."),
        }
    }
}

/// A character that shows nothing or reorders what is shown around it (zero-width and direction
/// marks, bidi embeddings and overrides, isolates, invisible operators, the BOM, tags). The server
/// refuses them too (`auth::shown_text_ok`); the crates share no library: keep both in step.
fn is_hidden_format(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}'
        | '\u{feff}' | '\u{ffa0}' | '\u{fff0}'..='\u{fffb}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}')
        || (c.is_whitespace() && c != ' ')
}

/// What a name may keep although it shows nothing by itself: the emoji presentation selector, the
/// joiners of joined emoji and of Persian and Indic scripts, and the no-break space (the server
/// keeps the same four: `auth::name_text_ok`).
fn is_benign_format(c: char) -> bool {
    matches!(c, '\u{fe0f}' | '\u{200c}' | '\u{200d}' | '\u{a0}')
}

/// A name that goes on the screen and into a file root reads: the length, no control character, no
/// hidden format character but the few a name can need.
fn text_ok(v: Option<&Value>, min: usize, max: usize) -> Option<String> {
    let s = v?.as_str()?;
    let n = s.chars().count();
    (n >= min
        && n <= max
        && !s
            .chars()
            .any(|c| c.is_control() || (is_hidden_format(c) && !is_benign_format(c))))
    .then(|| s.to_string())
}

/// The creator's address: the rule of a name without the exceptions (`boss@acme.example` plus a
/// zero-width space is another address that reads like the first).
fn address_ok(v: Option<&Value>) -> Option<String> {
    let s = text_ok(v, 0, 200)?;
    (!s.chars().any(is_hidden_format)).then_some(s)
}

/// Whether an address is a bare e-mail address: one `@`, a local part of the characters an address
/// may use unquoted (no leading, trailing or doubled dot) and a domain of ASCII letters, digits and
/// hyphens in labels joined by single dots. The rest can read like another person's address:
/// `<boss@acme.example>`, `boss@acme.example.`, `mailto:boss@acme.example` or a Cyrillic `а`
/// for the Latin one. Nothing is refused for that; the confirmation says so.
pub fn is_plain_address(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let dots_ok = |part: &str| !part.is_empty() && part.split('.').all(|piece| !piece.is_empty());
    dots_ok(local)
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c))
        && dots_ok(domain)
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && domain
            .split('.')
            .all(|label| !label.starts_with('-') && !label.ends_with('-'))
}

/// A name as the confirmation shows it: runs of white space become one space.
fn collapse_white_space(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The address as it is shown in the confirmation: every character outside printable ASCII as its
/// code point (`boss@\u{430}cme.example`), so a look-alike can be told from the real one. Nothing
/// is refused that way: an address with an umlaut is shown the same, only spelled out.
pub fn shown_address(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, ' '..='~') {
                c.to_string()
            } else {
                c.escape_unicode().to_string()
            }
        })
        .collect()
}

/// Spend `code` at `{base}/agent/pair` for a device called `name` (a reconnect code ignores it).
/// No credential goes out; the code is in the body, never in the URL. The reply is foreign data
/// that ends up in a file root reads, so every field is checked.
pub fn redeem(http: &dyn Http, base: &str, code: &str, name: &str) -> Result<Paired, PairError> {
    let reply = http
        .post(
            &format!("{base}/agent/pair"),
            "",
            &json!({ "code": code, "name": name }),
        )
        .map_err(|e| match http_status(&e) {
            Some(401) => PairError::BadCode,
            Some(409) => PairError::NameTaken,
            Some(400) => PairError::BadName,
            Some(429) => PairError::Throttled,
            Some(404) => PairError::OldServer,
            Some(403) => PairError::Proxy,
            _ if matches!(e, HttpError::Value(_)) => PairError::BadReply,
            _ => PairError::Unreachable(explain(&e)),
        })?;
    let v = reply.ok_or(PairError::BadReply)?;
    let key = v
        .get("key")
        .and_then(Value::as_str)
        .filter(|k| {
            (16..=200).contains(&k.len())
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or(PairError::BadReply)?;
    let created_by = match v.get("created_by") {
        Some(Value::Null) | None => None,
        other => Some(address_ok(other).ok_or(PairError::BadReply)?),
    };
    Ok(Paired {
        key: key.to_string(),
        name: text_ok(v.get("name"), 1, 200).ok_or(PairError::BadReply)?,
        org: text_ok(v.get("org"), 0, 200).ok_or(PairError::BadReply)?,
        created_by,
        replaces: v
            .get("replaces")
            .and_then(Value::as_bool)
            .ok_or(PairError::BadReply)?,
    })
}

// --- printpapi-agent pair ------------------------------------------------------------------------

const USAGE: &str = "usage: printpapi-agent pair [--server URL] [--name NAME] [--printers LIST] [--keep-headers] [--yes] [--insecure]";
const CODE_NOT_ARGUMENT: &str = "The code is read from the prompt, not from the command line.";
const MAX_LINE: u64 = 4096;

#[derive(Default)]
struct Opts {
    server: Option<String>,
    name: Option<String>,
    printers: Option<String>,
    keep_headers: bool,
    yes: bool,
    insecure: bool,
}

/// `None`: not a call we know (including a code or link as an argument: it would stand in the
/// process list and the shell history).
fn parse_opts(args: &[String]) -> Option<Opts> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => o.server = Some(it.next()?.clone()),
            "--name" => o.name = Some(it.next()?.clone()),
            "--printers" => o.printers = Some(it.next()?.clone()),
            "--keep-headers" => o.keep_headers = true,
            "--yes" => o.yes = true,
            "--insecure" => o.insecure = true,
            _ => return None,
        }
    }
    Some(o)
}

/// What the command needs from the machine, so a test can hand in fakes.
pub struct Deps<'a> {
    pub os: crate::install::Os,
    pub sys: PathBuf,
    /// Runs `icacls` (Windows) on the new file.
    pub run: crate::proc::Runner,
    pub make_http: &'a dyn Fn(&HttpSettings) -> Box<dyn Http>,
    /// The system's store for the key (DPAPI on Windows).
    pub store: Option<crate::keystore::KeyStore>,
    pub default_name: &'a dyn Fn() -> String,
    pub sleep: &'a dyn Fn(u64),
    /// The printers this machine knows, for the hint when agent.ini names none.
    pub found_printers: &'a dyn Fn() -> Vec<String>,
}

pub struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// One line of `input`, at most 4096 bytes, without its line break. `None` at the end of the input
/// or for a longer line.
fn read_line(input: &mut dyn BufRead) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    let n = (&mut *input)
        .take(MAX_LINE + 1)
        .read_until(b'\n', &mut buf)
        .ok()?;
    if n == 0 || buf.len() as u64 > MAX_LINE {
        return None;
    }
    Some(
        String::from_utf8_lossy(&buf)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    )
}

fn ask(io: &mut Io, prompt: &str) -> Option<String> {
    let _ = write!(io.err, "{prompt}");
    let _ = io.err.flush();
    read_line(io.input)
}

/// The sentence that is missing from "Permission denied" and "Access is denied": on an install made
/// by the setup agent.ini and the update lock belong to the administrator.
fn with_rights_hint(message: String) -> String {
    let denied = if cfg!(windows) { 5 } else { 13 };
    if message.ends_with(&format!("(os error {denied})")) {
        format!("{message} Run this as administrator, or with sudo.")
    } else {
        message
    }
}

/// agent.ini as it is now: `None` for none, an error text for a link (a symbolic link or, on
/// Windows, any reparse point) or anything that is no file. Unix opens with `O_NOFOLLOW`, so a link
/// swapped in after the check is refused as well.
fn read_existing(path: &Path) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot look at agent.ini: {e}")),
        Ok(m) if crate::apply::is_link(&m) || !m.is_file() => {
            return Err("agent.ini is a link or not a file. pair does not follow it.".into());
        }
        Ok(_) => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut opts
            .open(path)
            .map_err(|e| format!("cannot read agent.ini: {e}"))?,
        &mut bytes,
    )
    .map_err(|e| format!("cannot read agent.ini: {e}"))?;
    Ok(Some(crate::config::decode_ini(&bytes)))
}

/// Make the new file take the owner of the one it replaces (an existing file), or, as root, the
/// owner and group of the folder, so the service user can read what root writes. Unix only.
#[cfg(unix)]
fn give_rights(file: &std::fs::File, ini: &Path, existing: bool) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, fchown};
    let (reference, copy_mode) = if existing {
        (ini, true)
    } else if unsafe { libc::geteuid() } == 0 {
        (ini.parent().unwrap_or(Path::new(".")), false)
    } else {
        return Ok(());
    };
    let m = std::fs::metadata(reference)
        .map_err(|e| format!("cannot read the owner of agent.ini: {e}"))?;
    fchown(file, Some(m.uid()), Some(m.gid()))
        .map_err(|e| format!("cannot give the new agent.ini its owner: {e}"))?;
    if copy_mode {
        file.set_permissions(std::fs::Permissions::from_mode(m.mode() & 0o7777))
            .map_err(|e| format!("cannot give the new agent.ini its rights: {e}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn give_rights(_: &std::fs::File, _: &Path, _: bool) -> Result<(), String> {
    Ok(())
}

/// `tmp` takes the place of `dest`. A new file is renamed. An existing file on Windows is replaced
/// with `ReplaceFileW` and a backup name: it keeps the DACL of the file it replaces (a rename would
/// give it the folder's), and with a backup name every documented failure leaves the original or the
/// backup in place. Docs: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew
fn put_in_place(tmp: &Path, dest: &Path, existed: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    if existed {
        return replace_file(tmp, dest);
    }
    let _ = existed;
    std::fs::rename(tmp, dest)
}

#[cfg(windows)]
fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let backup = dest.with_file_name(".agent.ini.old");
    let _ = std::fs::remove_file(&backup);
    let (d, t, b) = (wide(dest), wide(tmp), wide(&backup));
    // SAFETY: three NUL-terminated wide strings that outlive the call, null for the two reserved
    // parameters, flags 0.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            d.as_ptr(),
            t.as_ptr(),
            b.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok != 0 {
        let _ = std::fs::remove_file(&backup);
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    // 1176/1177: the original may be under the backup name. Put it back.
    if !dest.exists() && backup.exists() {
        let _ = std::fs::rename(&backup, dest);
    }
    Err(e)
}

/// The command that restarts the agent on this system, for the last line.
fn restart_hint(os: crate::install::Os) -> String {
    use crate::install::{LAUNCHD_LABEL, Os, TASK_NAME};
    match os {
        Os::Linux => format!("systemctl restart {TASK_NAME}"),
        Os::Macos => format!("launchctl kickstart -k system/{LAUNCHD_LABEL}"),
        Os::Windows => format!("schtasks /end /tn {TASK_NAME}, then schtasks /run /tn {TASK_NAME}"),
    }
}

/// `printpapi-agent pair`. Returns the exit code: 0 paired, 1 not, 2 a wrong call.
///
/// Order, so that nothing is lost by a failure: the new file is made (and its rights set) **before
/// the code is spent**; then the code is spent; then the person confirms the account and the
/// device; then the file is replaced in one step; then the key is registered (with no printers: it
/// binds the key, or marks a takeover as pending, without touching any printer). The code and the
/// key never appear in the output.
pub fn run_pair(args: &[String], base_dir: &Path, deps: &Deps, io: &mut Io) -> u8 {
    let Some(o) = parse_opts(args) else {
        let _ = writeln!(io.err, "{USAGE}");
        let _ = writeln!(io.err, "{CODE_NOT_ARGUMENT}");
        return 2;
    };
    if let Some(p) = &o.printers
        && let Err(e) = parse_printers(p)
    {
        let _ = writeln!(io.err, "--printers: {e}");
        return 2;
    }
    let fail = |io: &mut Io, m: &str| -> u8 {
        let _ = writeln!(io.err, "{m}");
        1
    };
    let ini = base_dir.join("agent.ini");
    let existing = match read_existing(&ini) {
        Ok(t) => t,
        Err(m) => return fail(io, &with_rights_hint(m)),
    };
    let cfg = match existing.as_deref().map(Config::from_text).transpose() {
        Ok(c) => c,
        Err(_) => {
            return fail(
                io,
                "agent.ini cannot be read. Fix it or move it away, then run pair again.",
            );
        }
    };
    let old_server = cfg
        .as_ref()
        .and_then(|c| c.get("server_url").ok().flatten());
    let old_name = cfg.as_ref().and_then(|c| c.get("name").ok().flatten());

    // the code or link: one line, never an argument
    let Some(line) = ask(io, "Pairing code or link: ") else {
        return fail(io, "No pairing code was entered.");
    };
    let Some(pairing) = parse(&line) else {
        return fail(io, "That is not a pairing code or link.");
    };
    // the server: the link's, else --server, else the installed one, else asked
    let typed = match (&pairing.server, &o.server) {
        (Some(s), _) => s.clone(),
        (None, Some(s)) => s.clone(),
        (None, None) => match old_server.clone() {
            Some(s) => s,
            None => match ask(io, "Server address: ") {
                Some(s) => s,
                None => return fail(io, "No server address was entered."),
            },
        },
    };
    let Some(server) = valid_server(&typed) else {
        return fail(
            io,
            "The server address must start with http:// or https:// and have no user name, ?, # or %.",
        );
    };
    if pairing.server.is_some()
        && o.server
            .as_deref()
            .is_some_and(|s| !same_origin(s, &server))
    {
        return fail(io, "The link names another server than --server.");
    }
    if needs_insecure_confirm(&server) && !o.insecure {
        return fail(
            io,
            "This address is plain http outside your private network: the key would cross it readable. Use https, or add --insecure if you know what you are doing.",
        );
    }
    let name = o
        .name
        .clone()
        .or(old_name)
        .unwrap_or_else(|| (deps.default_name)());
    if name.trim().is_empty() || name.chars().count() > 200 || name.chars().any(char::is_control) {
        return fail(
            io,
            "The device name must have 1 to 200 characters and no control character.",
        );
    }

    // headers belong to the origin they were set for
    let mut settings = cfg
        .as_ref()
        .and_then(|c| c.http_settings().ok())
        .unwrap_or_default();
    let dropped = match (&existing, o.keep_headers) {
        (Some(text), false) => crate::config::dropped_headers(text, &server),
        _ => Vec::new(),
    };
    if !dropped.is_empty() {
        settings.headers.clear();
        let _ = writeln!(
            io.out,
            "The headers of the old server ({}) are not sent to {server} and are removed from agent.ini. Add --keep-headers to keep them.",
            dropped.join(", ")
        );
    }

    // the update lock, when this installation has one
    let _lock = match crate::update::lock_in_dir(base_dir, deps.sleep) {
        Ok(l) => l,
        Err(e) => return fail(io, &with_rights_hint(e.to_string())),
    };
    // The lock comes after the prompt, and the settings window may have written agent.ini in the
    // meantime (it holds the same lock while it does). What is replaced below must be what was
    // read above, or that change would be lost.
    match read_existing(&ini) {
        Ok(now) if now == existing => {}
        Ok(_) => {
            return fail(
                io,
                "agent.ini changed while pair was waiting. Nothing was changed. Run pair again.",
            );
        }
        Err(m) => return fail(io, &with_rights_hint(m)),
    }

    // the file first: a failure here must leave the code unspent
    let tmp = ini.with_file_name(".agent.ini.new");
    let marker = base_dir.join(crate::install::MARKER_NAME).is_file();
    let mut file = match crate::apply::create_new_file(&tmp, 0o600) {
        Ok(f) => f,
        Err(e) => {
            return fail(
                io,
                &format!(
                    "cannot create the new agent.ini ({e}). Run this as administrator, or with sudo."
                ),
            );
        }
    };
    let prepared = give_rights(&file, &ini, existing.is_some()).and_then(|()| {
        if marker && deps.os == crate::install::Os::Windows {
            for c in crate::install::ini_acl_cmds(deps.os, &tmp, &deps.sys) {
                (deps.run)(&c).map_err(|_| "cannot restrict the new agent.ini".to_string())?;
            }
        }
        Ok(())
    });
    if let Err(m) = prepared {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        return fail(io, &m);
    }
    let cleanup = |tmp: &Path| {
        let _ = std::fs::remove_file(tmp);
    };

    let http = (deps.make_http)(&settings);
    let paired = match redeem(&*http, &server, &pairing.code, &name) {
        Ok(p) => p,
        Err(e) => {
            drop(file);
            cleanup(&tmp);
            return fail(io, &e.to_string());
        }
    };
    let who = paired
        .created_by
        .as_deref()
        .map_or("the server administrator".to_string(), shown_address);
    // Each value on its own line, as the window shows them: a name that reads like the end of a
    // sentence and the start of another one (the account name is free text) cannot pass for the
    // creator or the server. Runs of white space collapse, so nothing hides at the end of a line.
    let _ = writeln!(
        io.out,
        "{}\n  Account:    {}\n  Created by: {who}\n  Server:     {server}\n  Device:     {}",
        if paired.replaces {
            "This takes over an existing device:"
        } else {
            "This adds the computer to a printpapi account:"
        },
        collapse_white_space(&paired.org),
        collapse_white_space(&paired.name),
    );
    if paired.replaces {
        let _ = writeln!(
            io.out,
            "The computer that is using this name now stops printing."
        );
    }
    if paired
        .created_by
        .as_deref()
        .is_some_and(|a| !is_plain_address(a))
    {
        let _ = writeln!(
            io.out,
            "The address of the person who made the code is not a plain e-mail address (it has punctuation around it, or characters outside plain ASCII that are shown above as \\u{{...}}). It can imitate another address. Continue only if it is the address you expect."
        );
    }
    if !o.yes {
        let answer = ask(io, "Continue? [y/N] ").unwrap_or_default();
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            drop(file);
            cleanup(&tmp);
            let _ = writeln!(
                io.out,
                "Nothing was changed. The key ends by itself within 24 hours, or at once on the Keys page."
            );
            return 1;
        }
    }

    // the text of the new file; the key is sealed where the installation seals it
    let removals: Vec<(String, Option<String>)> =
        dropped.iter().map(|n| (n.clone(), None)).collect();
    let mut text = update_ini(
        existing.as_deref(),
        &IniUpdate {
            server_url: Some(&server),
            api_key: Some(&paired.key),
            name: Some(&paired.name),
            printers: o.printers.as_deref(),
            timeout: None,
            headers: Some(&removals),
            updates: None,
        },
    );
    if marker {
        match crate::keystore::seal(&text, deps.store.as_ref()) {
            Ok(Some(sealed)) => text = sealed,
            Ok(None) => {}
            Err(m) => {
                let _ = writeln!(io.err, "{m} The key is stored as text in agent.ini.");
            }
        }
    }
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            put_in_place(&tmp, &ini, existing.is_some())
        });
    if let Err(e) = written {
        cleanup(&tmp);
        return fail(
            io,
            &format!(
                "cannot write agent.ini ({e}). Nothing was changed. Create a new code and try again."
            ),
        );
    }

    // bind the key without touching a printer
    let registered = crate::run::register(&*http, &server, &paired.key, &paired.name, &[])
        .map_err(|e| explain(&e))
        .and_then(|r| {
            r.filter(|v| v.get("computer_id").is_some())
                .ok_or_else(|| "this address does not answer like a printpapi server".to_string())
        });
    if let Err(why) = registered {
        let _ = writeln!(
            io.out,
            "The key is saved but not registered yet ({why}). Start the agent within 24 hours, or the key is revoked."
        );
    }
    let _ = writeln!(io.out, "Paired with {server} as {}.", paired.name);
    let _ = writeln!(
        io.out,
        "Restart the agent to use it: {}",
        restart_hint(deps.os)
    );
    let has_printers = Config::from_text(&text)
        .ok()
        .and_then(|c| c.get("printers").ok().flatten())
        .is_some_and(|p| !p.trim().is_empty());
    if !has_printers {
        let found = (deps.found_printers)();
        let _ = writeln!(
            io.out,
            "Add a printers line to {}.{}",
            ini.display(),
            if found.is_empty() {
                String::new()
            } else {
                format!(" Found here: {}", found.join(", "))
            }
        );
    }
    0
}

/// The real thing, for `main`: `None` when `args` is not `pair`.
pub fn cli(args: &[String], base_dir: &Path) -> Option<ExitCode> {
    if args.first().map(String::as_str) != Some("pair") {
        return None;
    }
    let os = crate::install::Os::host();
    let make_http = |s: &HttpSettings| -> Box<dyn Http> { Box::new(crate::client::Client::new(s)) };
    let sleep = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));
    let run = crate::proc::real_runner();
    let found = {
        let run = run.clone();
        move || -> Vec<String> {
            crate::discover::discover(crate::Platform::host(), &run)
                .into_iter()
                .filter(|p| p.usable && !p.per_user)
                .map(|p| p.name)
                .collect()
        }
    };
    let deps = Deps {
        os,
        sys: crate::install::system_dir(),
        run,
        make_http: &make_http,
        store: crate::keystore::host(),
        default_name: &crate::setup::default_device_name,
        sleep: &sleep,
        found_printers: &found,
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let mut io = Io {
        input: &mut input,
        out: &mut out,
        err: &mut err,
    };
    Some(ExitCode::from(run_pair(
        &args[1..],
        base_dir,
        &deps,
        &mut io,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    const GOLDEN: &str =
        "printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT";
    const CODE: &str = "7K3M-Q2ZD-9XHT";
    const SERVER: &str = "https://print.example.com";

    fn p(s: &str) -> Option<Pairing> {
        parse(s)
    }

    // --- parse -------------------------------------------------------------------------------

    #[test]
    fn the_golden_link_is_what_the_dashboard_makes_and_parses_back() {
        // the same value stands in web/lib/pairing.test.ts
        assert_eq!(link(SERVER, CODE), GOLDEN);
        assert_eq!(
            p(GOLDEN),
            Some(Pairing {
                server: Some(SERVER.into()),
                code: CODE.into()
            })
        );
    }

    #[test]
    fn a_link_is_read_decoded_or_encoded_with_a_slash_and_in_any_order() {
        let want = Some(Pairing {
            server: Some(SERVER.into()),
            code: CODE.into(),
        });
        for l in [
            "printpapi://pair?server=https://print.example.com&code=7K3M-Q2ZD-9XHT",
            "printpapi://pair/?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT",
            "printpapi://pair?code=7k3mq2zd9xht&server=https%3a%2f%2fprint.example.com%2F",
            "PRINTPAPI://PAIR?server=https%3A%2F%2Fprint.example.com&code=7K3M%2DQ2ZD%2D9XHT",
            "  printpapi://pair?server=https%3A%2F%2Fprint.example.com&code=7K3M-Q2ZD-9XHT\n",
        ] {
            assert_eq!(p(l), want, "{l}");
        }
        let with_port =
            p("printpapi://pair?server=http%3A%2F%2F192.168.1.5%3A3460&code=7K3M-Q2ZD-9XHT");
        assert_eq!(
            with_port.unwrap().server.as_deref(),
            Some("http://192.168.1.5:3460")
        );
    }

    #[test]
    fn a_bare_code_is_read_like_the_server_reads_it() {
        for t in [
            "7K3M-Q2ZD-9XHT",
            "7k3m q2zd 9xht",
            "7K3MQ2ZD9XHT",
            " 7k3m-q2zd-9xht\n",
        ] {
            assert_eq!(
                p(t),
                Some(Pairing {
                    server: None,
                    code: CODE.into()
                }),
                "{t:?}"
            );
        }
        // O is 0, I and L are 1
        assert_eq!(p("OIL0-OIL0-OIL0").unwrap().code, "0110-0110-0110");
        for bad in [
            "",
            "7K3M",
            "7K3M-Q2ZD-9XHU",
            "7K3M-Q2ZD-9XHT7",
            "7K3M-Q2ZD-9XH\u{ff34}",
            "printpapi",
        ] {
            assert_eq!(p(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_hostile_link_is_refused() {
        let code = "code=7K3M-Q2ZD-9XHT";
        let srv = "server=https%3A%2F%2Fprint.example.com";
        let cases: Vec<(String, &str)> = vec![
            (
                format!("printpapi://pair?{srv}@evil.example&{code}"),
                "userinfo",
            ),
            (
                format!(
                    "printpapi://pair?server=https%3A%2F%2Fprint.example.com%40evil.example&{code}"
                ),
                "encoded userinfo",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%2F%3Fx%3D1&{code}"),
                "query in the server",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%23x&{code}"),
                "fragment in the server",
            ),
            (format!("printpapi://pair?{srv}&{code}#frag"), "fragment"),
            (
                format!("printpapi://pair?{srv}&{code}&extra=1"),
                "unknown parameter",
            ),
            (
                format!("printpapi://pair?{srv}&{srv}&{code}"),
                "repeated server",
            ),
            (
                format!("printpapi://pair?{srv}&{code}&{code}"),
                "repeated code",
            ),
            (format!("printpapi://pair?{srv}"), "no code"),
            (format!("printpapi://pair?{code}"), "no server"),
            (format!("printpapi://other?{srv}&{code}"), "other path"),
            (format!("printpapi://pair/x?{srv}&{code}"), "path below"),
            (format!("printpapi:pair?{srv}&{code}"), "no slashes"),
            (format!("other://pair?{srv}&{code}"), "other scheme"),
            (
                "printpapi://pair?server=ftp%3A%2F%2Fx&code=7K3M-Q2ZD-9XHT".into(),
                "ftp",
            ),
            (
                format!("printpapi://pair?server=https%253A%252F%252Fa.example&{code}"),
                "decoded twice",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%25&{code}"),
                "percent in the server",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%22&{code}"),
                "quote",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%5C&{code}"),
                "backslash",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%20x&{code}"),
                "space",
            ),
            (
                format!("printpapi://pair?server=%20https%3A%2F%2Fa.example&{code}"),
                "leading space",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%0A&{code}"),
                "line break",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2F%C3%A4.example&{code}"),
                "non-ASCII host",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%zz&{code}"),
                "bad escape",
            ),
            (
                format!("printpapi://pair?server=https%3A%2F%2Fa.example%FF&{code}"),
                "bad UTF-8",
            ),
            (
                format!(
                    "printpapi://pair?server=https%3A%2F%2F{}&{code}",
                    "a".repeat(300)
                ),
                "long server",
            ),
            (
                format!("printpapi://pair?{srv} &{code}"),
                "space in the link",
            ),
            (
                format!("printpapi://pair?{srv}&{code}\"x"),
                "quote in the link",
            ),
            (
                format!("printpapi://pair?{srv}&{code}\\x"),
                "backslash in the link",
            ),
            (
                format!("printpapi://pair?{srv}&{code}\u{1}"),
                "control character",
            ),
            (
                format!("printpapi://pair?{srv}&code=7K3M-Q2ZD-9XHU"),
                "bad code",
            ),
            (
                format!("printpapi://pair?{srv}&{code}{}", "x".repeat(3000)),
                "3 KB",
            ),
            (
                format!("printpapi://pair?server=%20&{code}"),
                "empty server",
            ),
        ];
        for (l, why) in cases {
            assert_eq!(p(&l), None, "{why}: {l:?}");
        }
    }

    #[test]
    fn valid_server_is_one_rule_for_links_and_typed_addresses() {
        assert_eq!(
            valid_server(" https://print.example.com/ ").as_deref(),
            Some(SERVER)
        );
        assert_eq!(
            valid_server("http://localhost:3460").as_deref(),
            Some("http://localhost:3460")
        );
        assert_eq!(
            valid_server("https://print.example.com/sub").as_deref(),
            Some("https://print.example.com/sub")
        );
        for bad in [
            "print.example.com",
            "https://user@print.example.com",
            "https://print.example.com?x=1",
            "https://print.example.com#x",
            "https://print.example.com/%41",
            "https://pr\u{e4}nt.example",
            "https://a.example/\"",
            "https://a.example\\x",
            "https://a b",
        ] {
            assert_eq!(valid_server(bad), None, "{bad}");
        }
    }

    // --- redeem ------------------------------------------------------------------------------

    type PostReply = Result<Option<Value>, HttpError>;

    /// `/agent/pair` and `/agent/register` answered with canned replies, every call recorded.
    struct Fake {
        pair: RefCell<PostReply>,
        register: RefCell<PostReply>,
        seen: RefCell<Vec<(String, String, Value)>>,
    }

    impl Fake {
        fn new() -> Rc<Fake> {
            Rc::new(Fake {
                pair: RefCell::new(Ok(Some(good_reply(false)))),
                register: RefCell::new(Ok(Some(json!({"computer_id": 7, "printer_ids": {}})))),
                seen: RefCell::new(Vec::new()),
            })
        }
        fn calls(&self, tail: &str) -> usize {
            self.seen
                .borrow()
                .iter()
                .filter(|c| c.0.ends_with(tail))
                .count()
        }
    }

    struct Shared(Rc<Fake>);

    impl Http for Shared {
        fn get(&self, _: &str, _: &str) -> PostReply {
            Err(HttpError::Os("no GET in these tests".into()))
        }
        fn get_bytes(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, HttpError> {
            Err(HttpError::Os("no GET in these tests".into()))
        }
        fn post(&self, url: &str, key: &str, body: &Value) -> PostReply {
            self.0
                .seen
                .borrow_mut()
                .push((url.to_string(), key.to_string(), body.clone()));
            if url.ends_with("/agent/pair") {
                self.0.pair.borrow().clone()
            } else {
                self.0.register.borrow().clone()
            }
        }
    }

    const KEY: &str = "k3y-0123456789abcdefghijklmnopqrstuvwxyz";

    fn good_reply(replaces: bool) -> Value {
        json!({"key": KEY, "name": "warehouse-pc", "org": "Acme", "created_by": "boss@acme.example", "replaces": replaces})
    }

    fn status(n: u16) -> PostReply {
        Err(HttpError::Os(format!("server returned {n}")))
    }

    #[test]
    fn redeem_maps_every_status_to_its_error() {
        let fake = Fake::new();
        let h = Shared(fake.clone());
        for (st, want) in [
            (401, PairError::BadCode),
            (409, PairError::NameTaken),
            (400, PairError::BadName),
            (429, PairError::Throttled),
            (404, PairError::OldServer),
            (403, PairError::Proxy),
        ] {
            *fake.pair.borrow_mut() = status(st);
            assert_eq!(redeem(&h, SERVER, CODE, "pc").err(), Some(want), "{st}");
        }
        *fake.pair.borrow_mut() = status(502);
        assert_eq!(
            redeem(&h, SERVER, CODE, "pc").err(),
            Some(PairError::Unreachable("server returned 502".into()))
        );
        *fake.pair.borrow_mut() = Err(HttpError::Value("html".into()));
        assert_eq!(
            redeem(&h, SERVER, CODE, "pc").err(),
            Some(PairError::BadReply)
        );
        *fake.pair.borrow_mut() = Ok(None);
        assert_eq!(
            redeem(&h, SERVER, CODE, "pc").err(),
            Some(PairError::BadReply)
        );
    }

    #[test]
    fn redeem_sends_the_code_in_the_body_and_no_credential() {
        let fake = Fake::new();
        let got = redeem(&Shared(fake.clone()), SERVER, CODE, "warehouse-pc")
            .ok()
            .unwrap();
        assert_eq!(
            (got.name.as_str(), got.org.as_str(), got.replaces),
            ("warehouse-pc", "Acme", false)
        );
        assert_eq!(got.created_by.as_deref(), Some("boss@acme.example"));
        let seen = fake.seen.borrow();
        assert_eq!(seen[0].0, format!("{SERVER}/agent/pair"));
        assert_eq!(seen[0].1, "", "no key");
        assert!(!seen[0].0.contains("7K3M"), "the URL holds no code");
        assert_eq!(seen[0].2, json!({"code": CODE, "name": "warehouse-pc"}));
    }

    #[test]
    fn a_reply_with_odd_fields_is_a_bad_reply() {
        let fake = Fake::new();
        let h = Shared(fake.clone());
        let bad = |f: &dyn Fn(&mut Value)| {
            let mut v = good_reply(false);
            f(&mut v);
            *fake.pair.borrow_mut() = Ok(Some(v));
            redeem(&h, SERVER, CODE, "pc").err()
        };
        assert_eq!(
            bad(&|v| v["key"] = json!("short")),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["key"] = json!("a".repeat(201))),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["key"] = json!("k".repeat(20) + "\nevil = 1")),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["key"] = json!("k".repeat(20) + " ")),
            Some(PairError::BadReply)
        );
        assert_eq!(bad(&|v| v["key"] = json!(5)), Some(PairError::BadReply));
        assert_eq!(bad(&|v| v["name"] = json!("")), Some(PairError::BadReply));
        assert_eq!(
            bad(&|v| v["name"] = json!("a".repeat(201))),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["name"] = json!("a\nb")),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["org"] = json!("a\u{7}b")),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["org"] = json!("a".repeat(201))),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["created_by"] = json!("a\nb")),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| v["created_by"] = json!(3)),
            Some(PairError::BadReply)
        );
        // invisible and direction characters: another person's address that reads like the first
        for hidden in [
            "\u{200b}",
            "\u{200d}",
            "\u{200c}",
            "\u{202e}",
            "\u{2066}",
            "\u{feff}",
            "\u{ad}",
            "\u{fe0f}",
            "\u{34f}",
            "\u{3164}",
            "\u{ffa0}",
            "\u{2800}",
            "\u{a0}",
            "\u{3000}",
            "\u{e0100}",
        ] {
            let text = format!("boss@acme.example{hidden}");
            assert_eq!(
                bad(&|v| v["created_by"] = json!(text)),
                Some(PairError::BadReply),
                "created_by {hidden:?}"
            );
            // a name or an account may keep what phones and scripts put in them (the server
            // keeps the same four), the creator's address may not
            let kept = ["\u{200d}", "\u{200c}", "\u{fe0f}", "\u{a0}"].contains(&hidden);
            let want = if kept {
                None
            } else {
                Some(PairError::BadReply)
            };
            assert_eq!(
                bad(&|v| v["org"] = json!(format!("Acme{hidden}"))),
                want,
                "org {hidden:?}"
            );
            assert_eq!(
                bad(&|v| v["name"] = json!(format!("pc{hidden}"))),
                want,
                "name {hidden:?}"
            );
        }
        // ordinary non-ASCII text is fine
        assert!(bad(&|v| v["org"] = json!("Müller & Söhne")).is_none());
        assert_eq!(
            bad(&|v| v["replaces"] = json!("yes")),
            Some(PairError::BadReply)
        );
        assert_eq!(
            bad(&|v| {
                v.as_object_mut().unwrap().remove("replaces");
            }),
            Some(PairError::BadReply)
        );
        // a null creator is fine: root made the code
        assert!(bad(&|v| v["created_by"] = Value::Null).is_none());
    }

    #[test]
    fn the_error_texts_are_the_documented_ones() {
        assert_eq!(PairError::BadCode.code(), "bad_code");
        assert_eq!(PairError::NameTaken.code(), "name_exists");
        assert!(PairError::BadName.to_string().contains("take over"));
        assert_eq!(
            PairError::Throttled.to_string(),
            "Too many attempts. Wait 15 minutes and try again."
        );
        assert_eq!(
            PairError::OldServer.to_string(),
            "This server is too old for pairing codes. Use an address and a key instead."
        );
        // the window and the command line say the same about an address with a user name, ? or #
        assert_eq!(
            PairError::BadUrl.to_string(),
            "The server address must start with http:// or https:// and have no user name, ?, # or %."
        );
        assert_eq!(
            PairError::BadReply.to_string(),
            "This address does not answer like a printpapi server."
        );
    }

    // --- the command -------------------------------------------------------------------------

    struct Run {
        code: u8,
        out: String,
        err: String,
        fake: Rc<Fake>,
        settings: Rc<RefCell<Vec<HttpSettings>>>,
    }

    const INI: &str = "; set up by hand\n[agent]\nserver_url = https://print.example.com\napi_key = OLDKEY\nname = office-pc\nprinters = Zebra|pdf ; Label\ntimeout = 45\n\n[headers]\nCF-Access-Client-Id = id1\n";

    fn rev(b: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(b.iter().rev().copied().collect())
    }

    fn pair_in(
        dir: &Path,
        args: &[&str],
        input: &str,
        fake: Rc<Fake>,
        store: Option<crate::keystore::KeyStore>,
    ) -> Run {
        let mut input = Cursor::new(input.as_bytes().to_vec());
        pair_io(dir, args, &mut input, fake, store)
    }

    /// The input of a person at the keyboard: one line at a time, and `before(n)` runs just before
    /// line `n` is handed over, so a test can change the world while the command waits for it.
    struct Typed<'a> {
        lines: std::collections::VecDeque<Vec<u8>>,
        current: Cursor<Vec<u8>>,
        next: usize,
        before: Box<dyn FnMut(usize) + 'a>,
    }

    impl<'a> Typed<'a> {
        fn new(text: &str, before: impl FnMut(usize) + 'a) -> Self {
            Typed {
                lines: text
                    .split_inclusive('\n')
                    .map(|l| l.as_bytes().to_vec())
                    .collect(),
                current: Cursor::new(Vec::new()),
                next: 0,
                before: Box::new(before),
            }
        }
    }

    impl std::io::Read for Typed<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let got = std::io::BufRead::fill_buf(self)?;
            let n = got.len().min(buf.len());
            buf[..n].copy_from_slice(&got[..n]);
            std::io::BufRead::consume(self, n);
            Ok(n)
        }
    }

    impl std::io::BufRead for Typed<'_> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            if self.current.position() as usize >= self.current.get_ref().len()
                && let Some(line) = self.lines.pop_front()
            {
                (self.before)(self.next);
                self.next += 1;
                self.current = Cursor::new(line);
            }
            self.current.fill_buf()
        }
        fn consume(&mut self, n: usize) {
            self.current.consume(n);
        }
    }

    fn pair_io(
        dir: &Path,
        args: &[&str],
        input: &mut dyn BufRead,
        fake: Rc<Fake>,
        store: Option<crate::keystore::KeyStore>,
    ) -> Run {
        let settings = Rc::new(RefCell::new(Vec::new()));
        let (f2, s2) = (fake.clone(), settings.clone());
        let make_http = move |s: &HttpSettings| -> Box<dyn Http> {
            s2.borrow_mut().push(s.clone());
            Box::new(Shared(f2.clone()))
        };
        let deps = Deps {
            os: crate::install::Os::Linux,
            sys: PathBuf::from("/usr/bin"),
            run: Rc::new(|_| Ok(Vec::new())),
            make_http: &make_http,
            store,
            default_name: &|| "default-pc".to_string(),
            sleep: &|_| {},
            found_printers: &|| vec!["HP LaserJet".into(), "Zebra".into()],
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let code = run_pair(
            &args,
            dir,
            &deps,
            &mut Io {
                input,
                out: &mut out,
                err: &mut err,
            },
        );
        Run {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            fake,
            settings,
        }
    }

    fn dir_with(ini: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        if let Some(t) = ini {
            std::fs::write(d.path().join("agent.ini"), t).unwrap();
        }
        d
    }

    fn ini_of(d: &tempfile::TempDir) -> String {
        std::fs::read_to_string(d.path().join("agent.ini")).unwrap()
    }

    fn no_temp(d: &tempfile::TempDir) {
        assert!(!d.path().join(".agent.ini.new").exists());
        assert!(!d.path().join(".agent.ini.old").exists());
    }

    fn never_shows(r: &Run) {
        for secret in [KEY, "7K3M", "Q2ZD", "9XHT"] {
            assert!(
                !r.out.contains(secret) && !r.err.contains(secret),
                "{secret}"
            );
        }
    }

    #[test]
    fn a_code_or_link_as_an_argument_is_refused() {
        let d = dir_with(Some(INI));
        for args in [
            vec![CODE],
            vec![GOLDEN],
            vec!["--yes", CODE],
            vec!["--server"],
            vec!["--unknown"],
        ] {
            let r = pair_in(d.path(), &args, "", Fake::new(), None);
            assert_eq!(r.code, 2, "{args:?}");
            assert!(r.err.starts_with("usage: printpapi-agent pair"));
            assert!(
                r.err
                    .contains("The code is read from the prompt, not from the command line.")
            );
            assert_eq!(r.fake.seen.borrow().len(), 0);
        }
        assert_eq!(ini_of(&d), INI);
        let r = pair_in(
            d.path(),
            &["--printers", "Z=socket://nowhere"],
            "",
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 2);
    }

    #[test]
    fn pairing_keeps_comments_timeout_printers_and_the_headers_of_the_same_origin() {
        let d = dir_with(Some(INI));
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0, "{}{}", r.out, r.err);
        let text = ini_of(&d);
        assert!(text.starts_with("; set up by hand\n"));
        assert!(text.contains(&format!("api_key = {KEY}")) && !text.contains("OLDKEY"));
        assert!(
            text.contains("name = warehouse-pc") && text.contains("printers = Zebra|pdf ; Label")
        );
        assert!(text.contains("timeout = 45") && text.contains("CF-Access-Client-Id = id1"));
        // the call carried the header and the timeout of the installed file
        let s = r.settings.borrow();
        assert_eq!(s[0].timeout, 45.0);
        assert_eq!(
            s[0].headers.get("cf-access-client-id").map(String::as_str),
            Some("id1")
        );
        no_temp(&d);
        never_shows(&r);
        assert!(
            r.out
                .contains("Paired with https://print.example.com as warehouse-pc.")
        );
        assert!(
            r.out
                .contains("Restart the agent to use it: systemctl restart printpapi-agent")
        );
        assert!(
            !r.out.contains("Add a printers line"),
            "agent.ini has printers"
        );
    }

    #[test]
    fn a_link_to_another_server_sends_and_keeps_no_header() {
        let d = dir_with(Some(INI));
        let link = link("https://other.example.org", CODE);
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{link}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0, "{}{}", r.out, r.err);
        assert!(
            r.settings.borrow().iter().all(|s| s.headers.is_empty()),
            "no header went out"
        );
        assert!(
            r.fake
                .seen
                .borrow()
                .iter()
                .all(|c| c.0.starts_with("https://other.example.org/"))
        );
        let text = ini_of(&d);
        assert!(!text.contains("CF-Access") && !text.contains("[headers]\nCF"));
        assert!(text.contains("server_url = https://other.example.org"));
        assert!(
            r.out.contains("cf-access-client-id"),
            "the dropped names are printed: {}",
            r.out
        );
        // with --keep-headers they stay and go along
        let d = dir_with(Some(INI));
        let r = pair_in(
            d.path(),
            &["--yes", "--keep-headers"],
            &format!("{link}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0);
        assert!(ini_of(&d).contains("CF-Access-Client-Id = id1"));
        assert_eq!(r.settings.borrow()[0].headers.len(), 1);
    }

    #[test]
    fn a_bare_code_uses_the_server_from_the_flag_the_file_or_the_prompt() {
        // from the file
        let d = dir_with(Some(INI));
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(
            r.fake.seen.borrow()[0].0,
            "https://print.example.com/agent/pair"
        );
        // the flag wins
        let d = dir_with(Some(INI));
        let r = pair_in(
            d.path(),
            &["--yes", "--server", "https://flag.example.org/"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(
            r.fake.seen.borrow()[0].0,
            "https://flag.example.org/agent/pair"
        );
        // no file: asked, and a new file is made with the default name
        let d = dir_with(None);
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\nhttps://asked.example.org\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0, "{}{}", r.out, r.err);
        assert_eq!(
            r.fake.seen.borrow()[0].0,
            "https://asked.example.org/agent/pair"
        );
        assert_eq!(r.fake.seen.borrow()[0].2["name"], "default-pc");
        let text = ini_of(&d);
        assert!(
            text.contains("server_url = https://asked.example.org")
                && text.contains(&format!("api_key = {KEY}"))
        );
        // and it says what to add, with what this machine has
        assert!(r.out.contains("Add a printers line to"), "{}", r.out);
        assert!(r.out.contains("Found here: HP LaserJet, Zebra"));
        // --printers writes the line
        let d = dir_with(None);
        let r = pair_in(
            d.path(),
            &["--yes", "--server", SERVER, "--printers", "Zebra|pdf"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert!(
            ini_of(&d).contains("printers = Zebra|pdf") && !r.out.contains("Add a printers line")
        );
        // a link and a --server that disagree
        let d = dir_with(None);
        let r = pair_in(
            d.path(),
            &["--yes", "--server", "https://x.example"],
            &format!("{GOLDEN}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert_eq!(r.fake.seen.borrow().len(), 0);
    }

    #[test]
    fn plain_http_outside_the_private_network_needs_insecure() {
        let d = dir_with(None);
        let r = pair_in(
            d.path(),
            &["--yes", "--server", "http://print.example.com"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert!(r.err.contains("--insecure"));
        assert_eq!(
            r.fake.seen.borrow().len(),
            0,
            "nothing was sent, the code is unspent"
        );
        assert!(!d.path().join("agent.ini").exists());
        let r = pair_in(
            d.path(),
            &[
                "--yes",
                "--insecure",
                "--server",
                "http://print.example.com",
            ],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0);
        // the private network and this machine need no flag
        for url in ["http://192.168.1.5:3460", "http://localhost:3460"] {
            let d = dir_with(None);
            let r = pair_in(
                d.path(),
                &["--yes", "--server", url],
                &format!("{CODE}\n"),
                Fake::new(),
                None,
            );
            assert_eq!(r.code, 0, "{url}");
        }
    }

    #[test]
    fn the_code_is_not_spent_when_the_new_file_cannot_be_made() {
        let d = dir_with(Some(INI));
        // a folder where the temporary file goes
        std::fs::create_dir(d.path().join(".agent.ini.new")).unwrap();
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert_eq!(r.fake.calls("/agent/pair"), 0);
        assert!(r.err.contains("administrator") || r.err.contains("sudo"));
        assert_eq!(ini_of(&d), INI);
    }

    #[test]
    fn a_failing_call_or_a_no_leaves_the_file_byte_for_byte() {
        for (status_err, input, args) in [
            (Some(status(401)), format!("{CODE}\n"), vec!["--yes"]),
            (Some(status(409)), format!("{CODE}\n"), vec!["--yes"]),
            (None, format!("{CODE}\nn\n"), vec![]),
            (None, format!("{CODE}\n"), vec![]), // the end of the input is a no
            (None, format!("{CODE}\n\n"), vec![]),
        ] {
            let d = dir_with(Some(INI));
            let fake = Fake::new();
            if let Some(e) = status_err {
                *fake.pair.borrow_mut() = e;
            }
            let r = pair_in(d.path(), &args, &input, fake, None);
            assert_eq!(r.code, 1, "{input:?}");
            assert_eq!(ini_of(&d), INI);
            no_temp(&d);
            never_shows(&r);
            assert_eq!(r.fake.calls("/agent/register"), 0);
        }
        let d = dir_with(Some(INI));
        let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), Fake::new(), None);
        assert!(
            r.out
                .contains("Nothing was changed. The key ends by itself within 24 hours")
        );
    }

    #[test]
    fn the_confirmation_names_the_account_the_creator_and_what_is_taken_over() {
        let d = dir_with(Some(INI));
        let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), Fake::new(), None);
        assert!(
            r.out
                .contains("This adds the computer to a printpapi account:"),
            "{}",
            r.out
        );
        assert!(r.out.contains("\n  Account:    Acme\n"), "{}", r.out);
        assert!(
            r.out.contains("\n  Created by: boss@acme.example\n"),
            "{}",
            r.out
        );
        assert!(r.out.contains("\n  Server:     "), "{}", r.out);
        assert!(
            r.out.contains("\n  Device:     warehouse-pc\n"),
            "{}",
            r.out
        );
        let fake = Fake::new();
        *fake.pair.borrow_mut() = Ok(Some(
            json!({"key": KEY, "name": "office-pc", "org": "Acme", "created_by": null, "replaces": true}),
        ));
        let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), fake, None);
        assert!(
            r.out.contains("This takes over an existing device:"),
            "{}",
            r.out
        );
        assert!(
            r.out.contains("\n  Created by: the server administrator\n"),
            "{}",
            r.out
        );
        assert!(r.out.contains("\n  Device:     office-pc\n"), "{}", r.out);
        assert!(
            r.out
                .contains("The computer that is using this name now stops printing.")
        );
    }

    #[test]
    fn an_account_name_cannot_pass_for_the_creator_in_the_confirmation() {
        let d = dir_with(Some(INI));
        let fake = Fake::new();
        let org = format!(
            "Acme (code made by boss@acme.example) on https://print.example.com as pc.{}",
            " ".repeat(120)
        );
        *fake.pair.borrow_mut() = Ok(Some(
            json!({"key": KEY, "name": "pc", "org": org, "created_by": "evil@evil.example", "replaces": false}),
        ));
        let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), fake, None);
        assert!(
            r.out.contains(
                "\n  Account:    Acme (code made by boss@acme.example) on https://print.example.com as pc.\n"
            ),
            "{}",
            r.out
        );
        assert!(
            r.out.contains("\n  Created by: evil@evil.example\n"),
            "{}",
            r.out
        );
    }

    #[test]
    fn a_creator_that_is_not_plain_ascii_is_shown_escaped_and_the_confirmation_says_so() {
        let d = dir_with(Some(INI));
        let fake = Fake::new();
        // a Cyrillic a in the domain: the address of the victim, as far as the eye can tell
        *fake.pair.borrow_mut() = Ok(Some(
            json!({"key": KEY, "name": "pc", "org": "Acme", "created_by": "boss@\u{430}cme.example", "replaces": false}),
        ));
        let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), fake, None);
        assert!(
            r.out.contains("\n  Created by: boss@\\u{430}cme.example\n"),
            "{}",
            r.out
        );
        assert!(
            !r.out.contains('\u{430}'),
            "the letter itself is never printed"
        );
        assert!(r.out.contains("not a plain e-mail address"), "{}", r.out);
        // a plain address says nothing more
        let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), Fake::new(), None);
        assert!(!r.out.contains("not a plain e-mail address"), "{}", r.out);
    }

    #[test]
    fn shown_address_escapes_every_character_outside_printable_ascii() {
        assert_eq!(shown_address("boss@acme.example"), "boss@acme.example");
        assert_eq!(
            shown_address("boss@\u{430}cme.example"),
            "boss@\\u{430}cme.example"
        );
        // an unassigned code point that draws nothing is not on the list of hidden characters
        assert_eq!(
            shown_address("boss@acme.example\u{1cbb}"),
            "boss@acme.example\\u{1cbb}"
        );
        assert_eq!(
            shown_address("Jörg@acme.example"),
            "J\\u{f6}rg@acme.example"
        );
        assert_eq!(shown_address("a b"), "a b");
        assert!(is_plain_address("boss+tag@acme.example"));
        assert!(!is_plain_address("boss@\u{430}cme.example"));
        assert!(!is_plain_address("boss@acme.example\u{1cbb}"));
    }

    #[test]
    fn only_the_shape_of_a_bare_address_is_plain() {
        for ok in [
            "boss@acme.example",
            "boss+tag@acme.example",
            "o'neil@sub.acme-corp.example",
            "a.b_c@x.io",
            "Boss@ACME.example",
        ] {
            assert!(is_plain_address(ok), "{ok}");
        }
        // the same letters as the victim's address, with plain ASCII punctuation around them
        for bad in [
            "<boss@acme.example>",
            "boss@acme.example.",
            "\"boss@acme.example\"",
            "boss@acme.example,",
            "mailto:boss@acme.example",
            "boss@acme.example (owner)",
            "boss@acme.example;",
            "boss@@acme.example",
            "@acme.example",
            "boss@",
            "boss",
            "",
            ".boss@acme.example",
            "boss.@acme.example",
            "boss..x@acme.example",
            "boss@acme..example",
            "boss@.acme.example",
            "boss@-acme.example",
            "boss @acme.example",
            "boss@\u{430}cme.example",
        ] {
            assert!(!is_plain_address(bad), "{bad}");
        }
    }

    #[test]
    fn a_creator_with_plain_ascii_punctuation_gets_the_warning_too() {
        let d = dir_with(Some(INI));
        for who in [
            "<boss@acme.example>",
            "boss@acme.example.",
            "mailto:boss@acme.example",
        ] {
            let fake = Fake::new();
            *fake.pair.borrow_mut() = Ok(Some(
                json!({"key": KEY, "name": "pc", "org": "Acme", "created_by": who, "replaces": false}),
            ));
            let r = pair_in(d.path(), &[], &format!("{CODE}\nn\n"), fake, None);
            assert!(
                r.out.contains("not a plain e-mail address"),
                "{who}: {}",
                r.out
            );
        }
    }

    #[test]
    fn it_registers_with_an_empty_printer_list_and_a_failure_keeps_the_file() {
        let d = dir_with(Some(INI));
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        let seen = r.fake.seen.borrow();
        let reg = seen
            .iter()
            .find(|c| c.0.ends_with("/agent/register"))
            .unwrap();
        assert_eq!(reg.1, KEY, "the new key");
        assert_eq!(reg.2, json!({"name": "warehouse-pc", "printers": []}));
        drop(seen);
        // register fails: the file stays paired, the person is told the deadline
        let d = dir_with(Some(INI));
        let fake = Fake::new();
        *fake.register.borrow_mut() = status(503);
        let r = pair_in(d.path(), &["--yes"], &format!("{CODE}\n"), fake, None);
        assert_eq!(r.code, 0);
        assert!(ini_of(&d).contains(&format!("api_key = {KEY}")));
        assert!(r.out.contains("The key is saved but not registered yet (server returned 503). Start the agent within 24 hours, or the key is revoked."), "{}", r.out);
        never_shows(&r);
    }

    #[test]
    fn an_installation_seals_the_key_unless_it_opts_out() {
        let store = crate::keystore::KeyStore {
            protect: rev,
            unprotect: rev,
        };
        // next to the setup's marker: sealed
        let d = dir_with(Some(INI));
        std::fs::write(d.path().join(crate::install::MARKER_NAME), "").unwrap();
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            Some(store),
        );
        assert_eq!(r.code, 0);
        let text = ini_of(&d);
        assert!(
            text.contains("api_key_dpapi = ") && !text.contains(KEY),
            "{text}"
        );
        // the opt-out of the installation is honoured
        let d = dir_with(Some(
            &INI.replace("timeout = 45", "timeout = 45\nseal_key = no"),
        ));
        std::fs::write(d.path().join(crate::install::MARKER_NAME), "").unwrap();
        pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            Some(store),
        );
        assert!(ini_of(&d).contains(&format!("api_key = {KEY}")));
        // no marker: a hand-made install keeps its plain key
        let d = dir_with(Some(INI));
        pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            Some(store),
        );
        assert!(ini_of(&d).contains(&format!("api_key = {KEY}")));
    }

    #[test]
    fn a_held_update_lock_stops_pair_before_the_code_is_spent() {
        let d = dir_with(Some(INI));
        let private = d.path().join(".update");
        std::fs::create_dir(&private).unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(private.join("lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert!(
            r.err.contains("An update or another change is running"),
            "{}",
            r.err
        );
        assert_eq!(r.fake.calls("/agent/pair"), 0);
        drop(lock);
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    }

    #[test]
    fn a_broken_agent_ini_is_refused_before_anything_is_asked() {
        let d = dir_with(Some("not an ini at all"));
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert_eq!(r.fake.seen.borrow().len(), 0);
        assert_eq!(ini_of(&d), "not an ini at all");
        // no input at all, and not a code
        let d = dir_with(Some(INI));
        assert_eq!(pair_in(d.path(), &["--yes"], "", Fake::new(), None).code, 1);
        assert_eq!(
            pair_in(d.path(), &["--yes"], "hello\n", Fake::new(), None).code,
            1
        );
        // a line longer than 4096 bytes is no code
        let long = "x".repeat(5000);
        assert_eq!(
            pair_in(
                d.path(),
                &["--yes"],
                &format!("{long}\n"),
                Fake::new(),
                None
            )
            .code,
            1
        );
    }

    #[test]
    fn a_change_made_while_pair_waits_for_the_code_stops_it_before_the_code_is_spent() {
        let d = dir_with(Some(INI));
        let ini = d.path().join("agent.ini");
        // the administrator goes to the dashboard to make the code; meanwhile the settings window
        // rewrites agent.ini (a printer was added)
        let changed = format!("{INI}; a printer was added\n");
        let mut input = Typed::new(&format!("{CODE}\ny\n"), |n| {
            if n == 0 {
                std::fs::write(&ini, &changed).unwrap();
            }
        });
        let r = pair_io(d.path(), &[], &mut input, Fake::new(), None);
        assert_eq!(r.code, 1, "{}{}", r.out, r.err);
        assert!(r.err.contains("agent.ini changed"), "{}", r.err);
        assert_eq!(r.fake.calls("/agent/pair"), 0, "the code is not spent");
        assert_eq!(ini_of(&d), changed, "the new text is not overwritten");
        no_temp(&d);
        // an agent.ini that appears meanwhile (a setup finished) is just as much a change
        let d = dir_with(None);
        let ini = d.path().join("agent.ini");
        let mut input = Typed::new(&format!("{CODE}\ny\n"), |n| {
            if n == 0 {
                std::fs::write(&ini, INI).unwrap();
            }
        });
        let r = pair_io(
            d.path(),
            &["--server", SERVER],
            &mut input,
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1, "{}{}", r.out, r.err);
        assert_eq!(r.fake.calls("/agent/pair"), 0);
        assert_eq!(ini_of(&d), INI);
    }

    #[test]
    fn a_refusal_for_missing_rights_says_to_run_it_as_administrator() {
        let denied = if cfg!(windows) { 5 } else { 13 };
        let text = format!("cannot read agent.ini: Access is denied. (os error {denied})");
        assert!(
            with_rights_hint(text.clone()).ends_with("Run this as administrator, or with sudo."),
            "{}",
            with_rights_hint(text)
        );
        let other = "cannot read agent.ini: The system cannot find the file. (os error 2)";
        assert_eq!(with_rights_hint(other.to_string()), other);
        assert_eq!(
            with_rights_hint("agent.ini is a link".to_string()),
            "agent.ini is a link"
        );
    }

    #[test]
    fn the_hint_also_fits_the_text_the_update_lock_gives() {
        let denied = if cfg!(windows) { 5 } else { 13 };
        // how update::lock_in_dir words a refused open: `system(what, io::Error)`
        let e = crate::apply::system(
            "cannot open the update lock",
            std::io::Error::from_raw_os_error(denied),
        );
        let hinted = with_rights_hint(e.to_string());
        assert!(
            hinted.starts_with("cannot open the update lock: "),
            "{hinted}"
        );
        assert!(
            hinted.ends_with("Run this as administrator, or with sudo."),
            "{hinted}"
        );
        // a busy lock is no rights problem
        let busy = crate::apply::system(
            "cannot take the update lock",
            std::io::Error::from_raw_os_error(if cfg!(windows) { 32 } else { 11 }),
        );
        assert_eq!(with_rights_hint(busy.to_string()), busy.to_string());
    }

    /// Unix only, and not as root (root can read everything).
    #[cfg(unix)]
    #[test]
    fn an_agent_ini_that_cannot_be_read_names_the_rights_not_only_the_error() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = dir_with(Some(INI));
        std::fs::set_permissions(
            d.path().join("agent.ini"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert!(r.err.contains("administrator, or with sudo"), "{}", r.err);
        assert_eq!(r.fake.calls("/agent/pair"), 0);
    }

    /// The file another program holds open without sharing delete rights: `ReplaceFileW` fails,
    /// and the old file is still there byte for byte, with no backup and no temporary file left.
    #[cfg(windows)]
    #[test]
    fn a_failing_replace_keeps_the_old_file_on_windows() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = dir_with(Some(INI));
        let ini = d.path().join("agent.ini");
        let tmp = d.path().join(".agent.ini.new");
        std::fs::write(&tmp, "the new text").unwrap();
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ: nobody may delete or rename it
            .open(&ini)
            .unwrap();
        let err = put_in_place(&tmp, &ini, true).expect_err("the replace must fail");
        assert!(
            matches!(err.raw_os_error(), Some(32 | 5 | 1175 | 1176 | 1177)),
            "a sharing violation, not a missing temporary file: {err:?}"
        );
        drop(holder);
        assert_eq!(ini_of(&d), INI);
        assert!(!d.path().join(".agent.ini.old").exists());
        // without the holder the same call works, so the failure above was the holder's
        put_in_place(&tmp, &ini, true).unwrap();
        assert_eq!(ini_of(&d), "the new text");
    }

    #[cfg(windows)]
    #[test]
    fn a_failing_replace_in_the_command_says_so_and_changes_nothing() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = dir_with(Some(INI));
        let ini = d.path().join("agent.ini");
        let mut holder = None;
        // the file is taken after the code was spent and the question was asked
        let mut input = Typed::new(&format!("{CODE}\ny\n"), |n| {
            if n == 1 {
                holder = Some(
                    std::fs::OpenOptions::new()
                        .read(true)
                        .share_mode(1)
                        .open(&ini)
                        .unwrap(),
                );
            }
        });
        let r = pair_io(d.path(), &[], &mut input, Fake::new(), None);
        drop(input);
        drop(holder);
        assert_eq!(r.code, 1, "{}{}", r.out, r.err);
        assert!(r.err.contains("cannot write agent.ini"), "{}", r.err);
        assert!(r.err.contains("Create a new code"), "{}", r.err);
        assert_eq!(ini_of(&d), INI);
        no_temp(&d);
        assert_eq!(r.fake.calls("/agent/register"), 0, "no key was bound");
        never_shows(&r);
    }

    /// The reason for `ReplaceFileW`: a plain rename would give the new file the folder's access
    /// list. Here the file has its own (no inheritance, the user, and read for the group Users, which
    /// no default list of a new file has without inheritance), and it is still the one the new file
    /// has after the replace.
    #[cfg(windows)]
    #[test]
    fn the_access_list_of_an_existing_file_is_kept_on_windows() {
        let acl = |path: &Path| -> String {
            let out = std::process::Command::new("icacls")
                .arg(path)
                .output()
                .expect("icacls runs");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            // the first line starts with the path, which the other file name does not share
            String::from_utf8_lossy(&out.stdout).replace(path.to_string_lossy().as_ref(), "FILE")
        };
        let d = dir_with(Some(INI));
        let ini = d.path().join("agent.ini");
        let me = format!("{}:F", std::env::var("USERNAME").expect("a user name"));
        let set = std::process::Command::new("icacls")
            .arg(&ini)
            .args([
                "/inheritance:r",
                "/grant:r",
                &me,
                "/grant",
                "*S-1-5-32-545:R",
            ])
            .output()
            .expect("icacls runs");
        assert!(
            set.status.success(),
            "{}",
            String::from_utf8_lossy(&set.stdout)
        );
        let own = acl(&ini);
        // a new file in this folder gets the folder's list, which is not this one
        std::fs::write(d.path().join("fresh.txt"), b"x").unwrap();
        assert_ne!(
            acl(&d.path().join("fresh.txt")),
            own,
            "the file's list is its own"
        );
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0, "{}{}", r.out, r.err);
        assert!(ini_of(&d).contains(KEY));
        assert_eq!(acl(&ini), own, "the replaced file kept its own access list");
        no_temp(&d);
    }

    #[cfg(windows)]
    #[test]
    fn replacing_an_existing_file_on_windows_leaves_no_backup() {
        let d = dir_with(Some(INI));
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0);
        no_temp(&d);
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_ini_that_is_a_link_is_refused() {
        let d = dir_with(None);
        let real = d.path().join("real.ini");
        std::fs::write(&real, INI).unwrap();
        std::os::unix::fs::symlink(&real, d.path().join("agent.ini")).unwrap();
        let r = pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 1);
        assert!(r.err.contains("link"));
        assert_eq!(r.fake.seen.borrow().len(), 0);
        assert_eq!(std::fs::read_to_string(&real).unwrap(), INI);
    }

    #[cfg(unix)]
    #[test]
    fn the_owner_of_the_old_file_stays_and_a_new_file_takes_the_owner_of_the_folder() {
        use std::os::unix::fs::{MetadataExt, chown};
        // only root may hand a file to somebody else; the Docker run of the gates is root
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let d = dir_with(Some(INI));
        chown(d.path(), Some(4242), Some(4343)).unwrap();
        chown(d.path().join("agent.ini"), Some(5252), Some(5353)).unwrap();
        pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        let m = std::fs::metadata(d.path().join("agent.ini")).unwrap();
        assert_eq!((m.uid(), m.gid()), (5252, 5353), "the old file's owner");
        // no agent.ini yet: the service user that owns the folder must be able to read what root writes
        let d = dir_with(None);
        chown(d.path(), Some(4242), Some(4343)).unwrap();
        let r = pair_in(
            d.path(),
            &["--yes", "--server", SERVER],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        assert_eq!(r.code, 0, "{}{}", r.out, r.err);
        let m = std::fs::metadata(d.path().join("agent.ini")).unwrap();
        assert_eq!((m.uid(), m.gid()), (4242, 4343), "the folder's owner");
    }

    #[cfg(unix)]
    #[test]
    fn the_new_file_keeps_the_mode_of_the_old_one() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir_with(Some(INI));
        std::fs::set_permissions(
            d.path().join("agent.ini"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        pair_in(
            d.path(),
            &["--yes"],
            &format!("{CODE}\n"),
            Fake::new(),
            None,
        );
        let mode = std::fs::metadata(d.path().join("agent.ini"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[test]
    fn cli_ignores_every_other_word() {
        assert!(cli(&[], Path::new(".")).is_none());
        assert!(cli(&["restart".to_string()], Path::new(".")).is_none());
        assert!(cli(&["--version".to_string()], Path::new(".")).is_none());
    }
}
