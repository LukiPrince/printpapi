// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! White-label builds of the setup program: the pure rules. A *brand* is a folder (see
//! docs/white-label.md) that the setup's build script checks with [`validate`]; the same file
//! (`#[path]`) is compiled into `crates/setup/build.rs`, so this module may use `std` and
//! `serde_json` only and must not name anything else of this crate.
//!
//! The words a person reads (licence notice, About text) are not here: they are written by the
//! setup program's own code, so that a brand cannot configure them away.

use serde_json::{Map, Value};

/// The longest product name. 20 comes from the tray tooltip: a Windows tooltip holds 127 UTF-16
/// units (`NOTIFYICONDATAW.szTip`, 128 with the end mark), the status line is at most 100 characters,
/// ": " takes 2, which leaves 25, and 5 stay free for status text outside the BMP (a character
/// counts once for the line's limit but twice in the tooltip).
/// https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataw
pub const NAME_MAX: usize = 20;
/// The longest client name.
pub const CLIENT_MAX: usize = 64;
/// The longest server address (the limit of `pair::valid_server`).
pub const SERVER_MAX: usize = 300;
/// The product name of the stock build; a brand must not use it.
pub const STOCK_PRODUCT_NAME: &str = "printpapi Agent Setup";
/// The Debian package name of the stock build (the kebab case of its product name). The brand's
/// package replaces and conflicts with it, because both ship the same files.
pub const STOCK_DEB: &str = "printpapi-agent-setup";
/// The environment variable that names the brand folder.
pub const BRAND_DIR_ENV: &str = "PRINTPAPI_BRAND_DIR";
/// Words that look like the scheme of a link. The id is written into marker files and may become a
/// link scheme one day, so these are kept free from the start (never loosened later).
const RESERVED_IDS: [&str; 15] = [
    "http",
    "https",
    "file",
    "ftp",
    "mailto",
    "tel",
    "ssh",
    "data",
    "blob",
    "javascript",
    "ws",
    "wss",
    "tauri",
    "ipc",
    "asset",
];

/// A brand id as a marker file holds it: `^[a-z][a-z0-9-]{1,30}$`, and neither `printpapi` nor
/// `stock`. This is the frozen rule of the marker format; [`valid_id`] adds to it.
pub fn valid_brand_id(id: &str) -> bool {
    let b = id.as_bytes();
    (2..=31).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && id != "printpapi"
        && id != "stock"
}

/// The id of a new brand: [`valid_brand_id`] and not a word that looks like a link scheme.
pub fn valid_id(id: &str) -> bool {
    valid_brand_id(id) && !RESERVED_IDS.contains(&id)
}

/// The product name: ASCII letters, digits, space, dot and hyphen, a letter first, no space at
/// the end, at most [`NAME_MAX`] characters. ASCII only keeps it a valid Debian package name, safe in
/// a `.desktop` file written by root (no control character, backslash or percent) and short
/// enough for the sidebar.
pub fn valid_name(name: &str) -> bool {
    name.len() <= NAME_MAX
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && !name.ends_with(' ')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-'))
}

/// The client the build runs for: 1 to [`CLIENT_MAX`] characters, no control character, no space at
/// either end.
pub fn valid_client(client: &str) -> bool {
    (1..=CLIENT_MAX).contains(&client.chars().count())
        && client == client.trim()
        && !client.chars().any(char::is_control)
}

/// The text after `http://` or `https://` (any case), `None` for every other start.
fn after_scheme(url: &str) -> Option<&str> {
    ["https://", "http://"].into_iter().find_map(|scheme| {
        let head = url.get(..scheme.len())?;
        head.eq_ignore_ascii_case(scheme)
            .then(|| &url[scheme.len()..])
    })
}

/// An address that may be opened or used as a server: `http://` or `https://` with a host, and
/// without whitespace, control characters or quotes (it ends up in a command line). The tray's
/// "Open dashboard" and the brand's support link use this one rule.
pub fn valid_http_url(url: &str) -> bool {
    let Some(rest) = after_scheme(url) else {
        return false;
    };
    let bad = |c: char| {
        c.is_whitespace() || c.is_control() || matches!(c, '"' | '\\' | '<' | '>' | '^' | '|' | '`')
    };
    !(rest.is_empty() || rest.starts_with(['/', '?', '#']) || url.chars().any(bad))
}

/// The default server of a brand: [`valid_http_url`] and the rule that the pairing code and the
/// setup's address field apply to every server (`pair::valid_server`, which this file cannot
/// call because the build script shares it): a host after the optional port, ASCII only (an
/// international name goes in as punycode), at most 300 bytes and none of `@ ? # %`. A server
/// that fails here would make `server_fixed` a window that can never connect.
pub fn valid_server(url: &str) -> bool {
    let Some(rest) = after_scheme(url) else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    // a host (a bracketed IPv6 address or a name) and an optional all-digit port up to 65535
    let (host, port, name_chars): (&str, Option<&str>, fn(char) -> bool) =
        match authority.strip_prefix('[') {
            Some(v6) => match v6.split_once(']') {
                Some((host, "")) => (host, None, |c| {
                    c.is_ascii_hexdigit() || matches!(c, ':' | '.')
                }),
                Some((host, after)) if after.starts_with(':') => (host, Some(&after[1..]), |c| {
                    c.is_ascii_hexdigit() || matches!(c, ':' | '.')
                }),
                _ => return false,
            },
            None => {
                let (host, port) = authority
                    .split_once(':')
                    .map_or((authority, None), |(h, p)| (h, Some(p)));
                (host, port, |c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
                })
            }
        };
    let port_ok = port.is_none_or(|p| {
        (1..=5).contains(&p.len())
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| n <= 65535)
    });
    valid_http_url(url)
        && url.len() <= SERVER_MAX
        && url.is_ascii()
        && !host.is_empty()
        && host.chars().all(name_chars)
        && port_ok
        && !url.contains(['@', '?', '#', '%'])
}

/// [`valid_http_url`] with `https` only.
pub fn valid_https_url(url: &str) -> bool {
    valid_http_url(url)
        && url
            .get(..8)
            .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
}

/// Width and height from the header of a PNG (signature, then the `IHDR` chunk, bytes 16 to 23,
/// big endian). Nothing else is looked at, so a file that has a good header and a damaged body
/// passes here: the setup's own tests decode every image of a brand.
/// https://www.w3.org/TR/png-3/
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let word =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((word(16), word(20)))
}

/// What the window tells the elevated install about its brand (`staging/brand.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBrand {
    pub id: String,
    pub name: String,
}

/// More than this is not a staged brand.
const STAGED_MAX: usize = 4096;

/// Read `staging/brand.json`: exactly `{"id": ..., "name": ...}`, both checked again with the rules
/// of a brand. The folder is writable by the user who started the setup, so nothing in it is
/// trusted; this keeps an accident out (a stray newline in a name would break the `.desktop`
/// file), it is no border.
pub fn parse_staged(bytes: &[u8]) -> Result<StagedBrand, String> {
    let bad = |why: &str| Err(format!("brand.json: {why}"));
    if bytes.len() > STAGED_MAX {
        return bad("too large");
    }
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(bytes) else {
        return bad("not a JSON object");
    };
    if map.len() != 2 {
        return bad("it holds id and name, nothing else");
    }
    let (Some(Value::String(id)), Some(Value::String(name))) = (map.get("id"), map.get("name"))
    else {
        return bad("id and name must be text");
    };
    if !valid_id(id) {
        return bad("the id is not valid");
    }
    if !valid_name(name) {
        return bad("the name is not valid");
    }
    Ok(StagedBrand {
        id: id.clone(),
        name: name.clone(),
    })
}

/// A brand that passed [`validate`].
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    pub id: String,
    pub name: String,
    pub client: String,
    pub server: Option<String>,
    pub server_fixed: bool,
    pub support_url: Option<String>,
}

impl Validated {
    /// What the setup program reads at run time (`OUT_DIR/brand.json`).
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "client": self.client,
            "server": self.server,
            "server_fixed": self.server_fixed,
            "support_url": self.support_url,
        })
    }
}

/// The files named by `bundle.icon` of a brand's Tauri patch (text entries only; relative ones are
/// relative to the folder of `tauri.conf.json`). The build script watches them: `cargo tauri icon`
/// overwrites them under the same names, and the Windows exe takes its icon from them.
pub fn icon_paths(patch_json: &[u8]) -> Vec<String> {
    let Ok(patch) = serde_json::from_slice::<Value>(patch_json) else {
        return Vec::new();
    };
    patch
        .pointer("/bundle/icon")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Is this path absolute on Windows or on a Unix system? Written out so that the answer does not
/// depend on the system that runs the check.
fn is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches(['/', '\\']))
}

const FILE_KEYS: [&str; 6] = [
    "id",
    "name",
    "client",
    "server",
    "server_fixed",
    "support_url",
];
const TRAY_FILES: [&str; 3] = ["tray-online.png", "tray-offline.png", "tray-stopped.png"];

/// The keys of the Tauri configuration patch that a brand may set, and nothing else. Everything
/// outside this list changes what the program is called on the machine (identifier, binary name,
/// bundle targets) or where its web content comes from (app, build, plugins).
const BUNDLE_KEYS: [&str; 6] = [
    "icon",
    "publisher",
    "copyright",
    "shortDescription",
    "longDescription",
    "linux",
];

/// Why a Tauri configuration patch is not allowed, one text per fault.
fn patch_faults(patch: &Value, what: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(top) = patch.as_object() else {
        return vec![format!("{what} must be a JSON object")];
    };
    for key in top.keys() {
        if key != "productName" && key != "bundle" {
            out.push(format!(
                "{what}: the key \"{key}\" is not allowed (only productName and bundle)"
            ));
        }
    }
    let Some(bundle) = top.get("bundle") else {
        return out;
    };
    let Some(bundle) = bundle.as_object() else {
        out.push(format!("{what}: bundle must be an object"));
        return out;
    };
    for (key, value) in bundle {
        if !BUNDLE_KEYS.contains(&key.as_str()) {
            out.push(format!("{what}: the key \"bundle.{key}\" is not allowed"));
        } else if key == "linux" {
            let Some(linux) = value.as_object() else {
                out.push(format!("{what}: bundle.linux must be an object"));
                continue;
            };
            for (k, v) in linux {
                if k != "deb" {
                    out.push(format!(
                        "{what}: the key \"bundle.linux.{k}\" is not allowed (only deb.replaces and deb.conflicts; a brand has no rpm)"
                    ));
                    continue;
                }
                let Some(deb) = v.as_object() else {
                    out.push(format!("{what}: bundle.linux.deb must be an object"));
                    continue;
                };
                for dk in deb.keys() {
                    if dk != "replaces" && dk != "conflicts" {
                        out.push(format!(
                            "{what}: the key \"bundle.linux.deb.{dk}\" is not allowed (only replaces and conflicts)"
                        ));
                    }
                }
            }
        }
    }
    out
}

/// The three places a build and the brand folder must agree on: productName, bundle.icon and the
/// Debian package relations.
fn agreed(patch: &Value) -> [Option<&Value>; 4] {
    [
        patch.get("productName"),
        patch.pointer("/bundle/icon"),
        patch.pointer("/bundle/linux/deb/replaces"),
        patch.pointer("/bundle/linux/deb/conflicts"),
    ]
}

/// The messages for `TAURI_CONFIG` against the brand folder's patch; the command to run is
/// the remedy for every one of them.
fn env_faults(env: &Value, patch: &Value, dir: &str) -> Vec<String> {
    let mut out = patch_faults(env, "TAURI_CONFIG");
    let command = format!(
        "Build with `cargo tauri build --config {}`, or set TAURI_CONFIG to the content of that file.",
        join(dir, "tauri.conf.json")
    );
    if !out.is_empty() {
        out.push(command);
    } else if agreed(env) != agreed(patch) {
        out.push(format!(
            "TAURI_CONFIG does not carry the same productName, bundle.icon and bundle.linux.deb \
             replaces/conflicts as the brand's tauri.conf.json. {command}"
        ));
    }
    out
}

/// Check a brand and everything the build brings to it, collecting every fault.
///
/// - `brand_dir`: the value of `PRINTPAPI_BRAND_DIR` (`None` or empty: a stock build).
/// - `tauri_config`: the value of `TAURI_CONFIG`.
/// - `read_file`: the bytes of a file by path (a file of the brand folder, or an entry of
///   `bundle.icon`, which the caller resolves against the Tauri config folder when it is relative).
///
/// `Ok(None)` is a stock build. A `TAURI_CONFIG` with productName or bundle.icon and no brand folder
/// is an error: it would give a build that behaves like stock under another name and icon.
pub fn validate(
    brand_dir: Option<&str>,
    tauri_config: Option<&str>,
    read_file: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Option<Validated>, Vec<String>> {
    let brand_dir = brand_dir.map(str::trim).filter(|d| !d.is_empty());
    let tauri_config = tauri_config.map(str::trim).filter(|c| !c.is_empty());
    let mut errors: Vec<String> = Vec::new();

    let env_value = match tauri_config.map(serde_json::from_str::<Value>) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            errors.push(format!("TAURI_CONFIG is not valid JSON: {e}"));
            None
        }
    };
    let Some(dir) = brand_dir else {
        if let Some(env) = &env_value
            && (env.get("productName").is_some() || env.pointer("/bundle/icon").is_some())
        {
            errors.push(format!(
                "TAURI_CONFIG sets productName or bundle.icon, but {BRAND_DIR_ENV} is not set. \
                 That would be a stock-behaving build under a brand's name and icon. Set \
                 {BRAND_DIR_ENV} to the absolute path of the brand folder, or leave TAURI_CONFIG out."
            ));
        }
        return if errors.is_empty() {
            Ok(None)
        } else {
            Err(errors)
        };
    };
    if !is_absolute(dir) {
        // a relative path would resolve against another folder for `--config` than for this variable
        return Err(vec![format!(
            "{BRAND_DIR_ENV} must be an absolute path (it is \"{dir}\")"
        )]);
    }

    let text_of = |name: &str| read_file(&join(dir, name));
    let mut brand = None;
    match text_of("brand.json") {
        None => errors.push(format!("{dir}/brand.json cannot be read")),
        Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(map)) => brand = Some(map),
            _ => errors.push("brand.json is not a JSON object".into()),
        },
    }
    let mut validated = None;
    if let Some(map) = &brand {
        validated = check_brand_json(map, &mut errors);
    }

    // the three tray images and the window icon
    match text_of("icon.png") {
        None => errors.push("icon.png cannot be read".into()),
        Some(b) if b.len() > 256 * 1024 => errors.push("icon.png is larger than 256 KiB".into()),
        Some(b) => match png_size(&b) {
            Some((w, h)) if w == h && (256..=1024).contains(&w) => {}
            Some((w, h)) => errors.push(format!(
                "icon.png must be square, 256 to 1024 pixels (it is {w}x{h})"
            )),
            None => errors.push("icon.png is not a PNG".into()),
        },
    }
    for file in TRAY_FILES {
        match text_of(file) {
            None => errors.push(format!("{file} cannot be read")),
            Some(b) => match png_size(&b) {
                Some((64, 64)) => {}
                Some((w, h)) => errors.push(format!("{file} must be 64x64 pixels (it is {w}x{h})")),
                None => errors.push(format!("{file} is not a PNG")),
            },
        }
    }

    // the Tauri patch of the folder
    let mut patch = None;
    match text_of("tauri.conf.json") {
        None => errors.push("tauri.conf.json cannot be read".into()),
        Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                let faults = patch_faults(&v, "tauri.conf.json");
                if faults.is_empty() {
                    patch_content(&v, validated.as_ref(), read_file, &mut errors);
                    patch = Some(v);
                } else {
                    errors.extend(faults);
                }
            }
            Err(e) => errors.push(format!("tauri.conf.json is not valid JSON: {e}")),
        },
    }

    // the build must carry the same patch
    match (&env_value, &patch) {
        (None, _) if tauri_config.is_none() => errors.push(format!(
            "TAURI_CONFIG is not set. Build with `cargo tauri build --config {dir}/tauri.conf.json`, or set \
             TAURI_CONFIG to the content of that file: Tauri takes the product name and the icons from it."
        )),
        (Some(env), Some(patch)) => errors.extend(env_faults(env, patch, dir)),
        (Some(env), None) => errors.extend(patch_faults(env, "TAURI_CONFIG")),
        _ => {}
    }

    if errors.is_empty() {
        Ok(validated)
    } else {
        Err(errors)
    }
}

/// The rules of brand.json. Fills `errors`; the brand comes back when every field is fine.
fn check_brand_json(map: &Map<String, Value>, errors: &mut Vec<String>) -> Option<Validated> {
    let before = errors.len();
    for key in map.keys() {
        if !FILE_KEYS.contains(&key.as_str()) {
            errors.push(format!("brand.json: unknown key \"{key}\""));
        }
    }
    let text = |key: &str, errors: &mut Vec<String>| -> Option<String> {
        match map.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                errors.push(format!("brand.json: {key} must be text"));
                None
            }
            None => None,
        }
    };
    let id = text("id", errors);
    match &id {
        Some(id) if valid_id(id) => {}
        Some(id) => errors.push(format!(
            "brand.json: id \"{id}\" is not valid (a-z, 0-9 and -, a letter first, 2 to 31 characters, not printpapi, stock or a word like a link scheme such as http)"
        )),
        None if !map.contains_key("id") => errors.push("brand.json: id is required".into()),
        None => {}
    }
    let name = text("name", errors);
    match &name {
        Some(n) if valid_name(n) => {}
        Some(n) => errors.push(format!(
            "brand.json: name \"{n}\" is not valid (ASCII letters, digits, space, dot and hyphen, starting with a letter, no space at the end, at most {NAME_MAX} characters)"
        )),
        None if !map.contains_key("name") => errors.push("brand.json: name is required".into()),
        None => {}
    }
    let client = text("client", errors);
    match &client {
        Some(c) if valid_client(c) => {}
        Some(_) => errors.push(format!(
            "brand.json: client must be 1 to {CLIENT_MAX} characters, with no control character and no space at either end"
        )),
        None if !map.contains_key("client") => errors.push("brand.json: client is required".into()),
        None => {}
    }
    let server = text("server", errors);
    if let Some(s) = &server
        && !valid_server(s)
    {
        errors.push(
            "brand.json: server must be an http:// or https:// address with a host, in ASCII (an international name as punycode), at most 300 characters, without @ ? # or %".into(),
        );
    }
    let support_url = text("support_url", errors);
    if let Some(s) = &support_url
        && !valid_https_url(s)
    {
        errors.push("brand.json: support_url must be an https:// address".into());
    }
    let server_fixed = match map.get("server_fixed") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            errors.push("brand.json: server_fixed must be true or false".into());
            false
        }
    };
    if server_fixed && server.is_none() {
        errors.push("brand.json: server_fixed needs a server".into());
    }
    if errors.len() > before {
        return None;
    }
    Some(Validated {
        id: id?,
        name: name?,
        client: client?,
        server,
        server_fixed,
        support_url,
    })
}

/// What the brand's own patch has to hold: the product name, the icon files and the package
/// relations.
fn patch_content(
    patch: &Value,
    brand: Option<&Validated>,
    read_file: &dyn Fn(&str) -> Option<Vec<u8>>,
    errors: &mut Vec<String>,
) {
    match patch.get("productName").and_then(Value::as_str) {
        None => errors.push("tauri.conf.json: productName is required".into()),
        Some(STOCK_PRODUCT_NAME) => errors.push(format!(
            "tauri.conf.json: productName must differ from \"{STOCK_PRODUCT_NAME}\""
        )),
        Some(p) => {
            if let Some(b) = brand
                && p != b.name
            {
                errors.push(format!(
                    "tauri.conf.json: productName \"{p}\" must equal the name in brand.json (\"{}\")",
                    b.name
                ));
            }
        }
    }
    match patch.pointer("/bundle/icon").and_then(Value::as_array) {
        None => errors.push(
            "tauri.conf.json: bundle.icon is required (the files that `cargo tauri icon` makes)"
                .into(),
        ),
        Some(list) => {
            let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            if names.is_empty() || names.len() != list.len() {
                errors.push("tauri.conf.json: bundle.icon must be a list of file names".into());
            } else {
                if !names
                    .iter()
                    .any(|n| n.to_ascii_lowercase().ends_with(".ico"))
                {
                    errors.push(
                        "tauri.conf.json: bundle.icon needs an .ico file (Windows takes the program's icon from it)"
                            .into(),
                    );
                }
                for n in names {
                    if read_file(n).is_none() {
                        errors.push(format!("tauri.conf.json: the icon file {n} cannot be read"));
                    }
                }
            }
        }
    }
    for key in ["replaces", "conflicts"] {
        let want = serde_json::json!([STOCK_DEB]);
        if patch.pointer(&format!("/bundle/linux/deb/{key}")) != Some(&want) {
            errors.push(format!(
                "tauri.conf.json: bundle.linux.deb.{key} must be exactly [\"{STOCK_DEB}\"]: the brand's package ships the same files as the stock package"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A PNG header with the given size (all that the validator reads).
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    const DIR: &str = "/brands/acme";

    fn brand_json() -> Value {
        serde_json::json!({
            "id": "acme", "name": "Acme Print", "client": "Acme GmbH",
            "server": "https://print.acme.example", "server_fixed": true,
            "support_url": "https://acme.example/help"
        })
    }

    fn patch() -> Value {
        serde_json::json!({
            "productName": "Acme Print",
            "bundle": {
                "icon": ["icons/icon.ico", "icons/icon.png"],
                "publisher": "Acme GmbH",
                "linux": { "deb": { "replaces": ["printpapi-agent-setup"], "conflicts": ["printpapi-agent-setup"] } }
            }
        })
    }

    struct Folder(HashMap<String, Vec<u8>>);

    impl Folder {
        fn good() -> Folder {
            let mut m = HashMap::new();
            m.insert(
                format!("{DIR}/brand.json"),
                brand_json().to_string().into_bytes(),
            );
            m.insert(format!("{DIR}/icon.png"), png(512, 512));
            for f in TRAY_FILES {
                m.insert(format!("{DIR}/{f}"), png(64, 64));
            }
            m.insert(
                format!("{DIR}/tauri.conf.json"),
                patch().to_string().into_bytes(),
            );
            m.insert("icons/icon.ico".into(), b"ico".to_vec());
            m.insert("icons/icon.png".into(), b"png".to_vec());
            Folder(m)
        }

        fn set(mut self, name: &str, bytes: impl Into<Vec<u8>>) -> Folder {
            self.0.insert(format!("{DIR}/{name}"), bytes.into());
            self
        }

        fn without(mut self, name: &str) -> Folder {
            self.0.remove(&format!("{DIR}/{name}"));
            self
        }

        fn brand(self, edit: impl FnOnce(&mut Value)) -> Folder {
            let mut v = brand_json();
            edit(&mut v);
            self.set("brand.json", v.to_string())
        }

        fn patch(self, edit: impl FnOnce(&mut Value)) -> Folder {
            let mut v = patch();
            edit(&mut v);
            self.set("tauri.conf.json", v.to_string())
        }

        fn run(
            &self,
            dir: Option<&str>,
            env: Option<&str>,
        ) -> Result<Option<Validated>, Vec<String>> {
            validate(dir, env, &|p| self.0.get(p).cloned())
        }

        fn errors(&self, env: &str) -> Vec<String> {
            self.run(Some(DIR), Some(env)).unwrap_err()
        }
    }

    fn env() -> String {
        patch().to_string()
    }

    fn has(errors: &[String], needle: &str) -> bool {
        errors.iter().any(|e| e.contains(needle))
    }

    #[test]
    fn a_good_brand_passes_and_a_stock_build_is_none() {
        let v = Folder::good()
            .run(Some(DIR), Some(&env()))
            .unwrap()
            .unwrap();
        assert_eq!(v.id, "acme");
        assert_eq!(v.name, "Acme Print");
        assert_eq!(v.client, "Acme GmbH");
        assert_eq!(v.server.as_deref(), Some("https://print.acme.example"));
        assert!(v.server_fixed);
        assert_eq!(v.support_url.as_deref(), Some("https://acme.example/help"));
        assert_eq!(v.to_json()["id"], "acme");
        for none in [None, Some(""), Some("  ")] {
            assert_eq!(Folder::good().run(none, None), Ok(None));
        }
        // a patch without the two brand keys is fine for a stock build (the .msi has one)
        assert_eq!(
            Folder::good().run(None, Some(r#"{"bundle":{"windows":{}}}"#)),
            Ok(None)
        );
    }

    #[test]
    fn ids_follow_the_marker_rule_and_keep_scheme_like_words_free() {
        for ok in ["ab", "acme", "a-b-c", "a1", &"a".repeat(31)] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in [
            "a",
            "",
            "Acme",
            "-a",
            "a_b",
            "printpapi",
            "stock",
            &"a".repeat(32),
            "http",
            "https",
            "file",
            "ftp",
            "mailto",
            "tel",
            "ssh",
            "data",
            "blob",
            "javascript",
            "ws",
            "wss",
            "tauri",
            "ipc",
            "asset",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        // the marker rule alone (frozen format) does not know the scheme words
        assert!(valid_brand_id("http"));
        assert!(!valid_brand_id("stock"));
    }

    #[test]
    fn names_are_ascii_trimmed_and_at_most_twenty_characters() {
        for ok in [
            "Acme",
            "Acme Print",
            "A",
            "a.b-c 1",
            &"a".repeat(20),
            "Twenty chars name 20",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        let long = "a".repeat(21);
        for bad in [
            "",
            " ",
            "  ",
            " Acme",
            "Acme ",
            "1Acme",
            "-Acme",
            "Ac_me",
            "Acmé",
            "Ac\tme",
            "Ac\nme",
            "Ac\\me",
            "Ac%me",
            "Ac/me",
            "Ac:me",
            "Ac\u{0}me",
            &long,
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert_eq!(NAME_MAX, 20);
    }

    #[test]
    fn urls_are_http_or_https_with_a_host_and_no_odd_characters() {
        for ok in [
            "http://a",
            "HTTPS://A.example/x?y=1#z",
            "https://a.example:8443/p",
        ] {
            assert!(valid_http_url(ok), "{ok}");
        }
        for bad in [
            "",
            "ftp://a",
            "javascript:alert(1)",
            "https://",
            "https:///x",
            "https://?x",
            "https://#",
            "https://a b",
            "https://a\n",
            "https://a\"b",
            "https://a\\b",
            "https://a<b",
            "https://a|b",
            "https://a^b",
            "https://a`b",
            "//a.example",
            "a.example",
            "file:///c",
        ] {
            assert!(!valid_http_url(bad), "{bad:?}");
        }
        assert!(valid_https_url("https://a.example"));
        assert!(valid_https_url("HTTPS://a.example"));
        assert!(!valid_https_url("http://a.example"));
        assert!(!valid_https_url("https://"));
    }

    #[test]
    fn a_brand_server_is_one_the_setup_can_connect_to() {
        for ok in [
            "http://a",
            "https://print.acme.example",
            "HTTPS://Print.Acme.example/",
            "https://print.acme.example/sub/path",
            "https://print.acme.example:8443",
            "http://localhost:3460",
            "https://[::1]:8443",
            "https://xn--druck-mller-3ib.example",
        ] {
            assert!(valid_server(ok), "{ok}");
            // the same address is one the pairing code and the address field accept
            assert!(crate::pair::valid_server(ok).is_some(), "{ok}");
            assert!(crate::setup::normalize_server_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "ftp://a",
            "https://",
            "https://:8443",
            "https://@",
            "https://[]:1",
            "https://druck.m\u{fc}ller.example",
            "https://print.acme.example/?x=1",
            "https://print.acme.example?x=1",
            "https://print.acme.example/a%20b",
            "https://u@print.acme.example",
            "https://u:pw@print.acme.example",
            "https://print.acme.example/#x",
            "https://print.acme.example#x",
            "https://a b",
            // a mistyped host or port: the URI parser behind the connection refuses these
            "https://[::1",
            "https://[::1:8443",
            "https://[::1]x",
            "https://[::1]:",
            "https://print.acme.example]",
            "https://print.acme.example:",
            "https://print.acme.example:80a",
            "https://print.acme.example:65536",
            "https://print.acme.example:80:90",
            "https://print.acme.example:-1",
            "https://print.acme.example:-",
            "https://[::1]:-",
            "https://pri*nt.example",
            "https://[g::1]",
        ] {
            assert!(!valid_server(bad), "{bad:?}");
        }
        let long = format!("https://a.example/{}", "x".repeat(SERVER_MAX));
        assert!(!valid_server(&long));
        let edge = format!("https://a.example/{}", "x".repeat(SERVER_MAX - 18));
        assert_eq!(edge.len(), SERVER_MAX);
        assert!(valid_server(&edge) && crate::pair::valid_server(&edge).is_some());
    }

    #[test]
    fn the_icon_files_of_the_patch_are_listed_for_the_build_to_watch() {
        assert_eq!(
            icon_paths(patch().to_string().as_bytes()),
            patch()["bundle"]["icon"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        );
        assert!(!icon_paths(patch().to_string().as_bytes()).is_empty());
        assert!(icon_paths(b"{}").is_empty());
        assert!(icon_paths(b"{").is_empty());
        assert!(icon_paths(br#"{"bundle":{"icon":[1,"a.ico"]}}"#) == ["a.ico"]);
    }

    #[test]
    fn png_size_reads_the_header_and_nothing_else() {
        assert_eq!(png_size(&png(64, 64)), Some((64, 64)));
        assert_eq!(png_size(&png(256, 1024)), Some((256, 1024)));
        assert_eq!(
            png_size(b"GIF89a not a png at all, but long enough to look"),
            None
        );
        assert_eq!(png_size(&png(64, 64)[..23]), None);
        assert_eq!(png_size(b""), None);
        // the chunk after the signature must be IHDR
        let mut b = png(64, 64);
        b[12..16].copy_from_slice(b"IDAT");
        assert_eq!(png_size(&b), None);
    }

    #[test]
    fn a_staged_brand_has_an_id_and_a_name_and_nothing_else() {
        let ok = parse_staged(br#"{"id":"acme","name":"Acme Print"}"#).unwrap();
        assert_eq!(
            ok,
            StagedBrand {
                id: "acme".into(),
                name: "Acme Print".into()
            }
        );
        for bad in [
            &br#"{"id":"acme","name":"Acme\nPrint"}"#[..],
            br#"{"id":"acme","name":"Acme Print","x":1}"#,
            br#"{"id":"acme"}"#,
            br#"{"id":"Acme","name":"Acme"}"#,
            br#"{"id":"http","name":"Acme"}"#,
            br#"{"id":"acme","name":1}"#,
            br#"["acme"]"#,
            b"acme",
            b"",
        ] {
            assert!(
                parse_staged(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let big = format!(r#"{{"id":"acme","name":"{}"}}"#, "a".repeat(5000));
        assert!(parse_staged(big.as_bytes()).is_err());
    }

    #[test]
    fn brand_json_rules() {
        let f = Folder::good;
        // unknown key
        assert!(has(
            &f().brand(|v| v["notice"] = "x".into()).errors(&env()),
            "unknown key \"notice\""
        ));
        // required
        for key in ["id", "name", "client"] {
            let errors = f()
                .brand(|v| {
                    v.as_object_mut().unwrap().remove(key);
                })
                .errors(&env());
            assert!(has(&errors, &format!("{key} is required")), "{errors:?}");
        }
        // wrong types
        assert!(has(
            &f().brand(|v| v["name"] = 5.into()).errors(&env()),
            "name must be text"
        ));
        assert!(has(
            &f().brand(|v| v["server_fixed"] = "yes".into())
                .errors(&env()),
            "server_fixed must be true or false"
        ));
        // each rule
        assert!(has(
            &f().brand(|v| v["id"] = "http".into()).errors(&env()),
            "id \"http\""
        ));
        assert!(has(
            &f().brand(|v| v["name"] = "a".repeat(21).into())
                .errors(&env()),
            "name \""
        ));
        assert!(has(
            &f().brand(|v| v["client"] = "".into()).errors(&env()),
            "client must be"
        ));
        assert!(has(
            &f().brand(|v| v["client"] = " x".into()).errors(&env()),
            "client must be"
        ));
        assert!(has(
            &f().brand(|v| v["client"] = "x".repeat(65).into())
                .errors(&env()),
            "client must be"
        ));
        assert!(has(
            &f().brand(|v| v["server"] = "ftp://x".into()).errors(&env()),
            "server must be"
        ));
        for odd in [
            "https://druck.m\u{fc}ller.example",
            "https://:8443",
            "https://a.example/?x=1",
        ] {
            assert!(
                has(
                    &f().brand(|v| v["server"] = odd.into()).errors(&env()),
                    "server must be"
                ),
                "{odd}"
            );
        }
        assert!(has(
            &f().brand(|v| v["support_url"] = "http://x.example".into())
                .errors(&env()),
            "support_url must be an https"
        ));
        // server_fixed without a server
        let errors = f()
            .brand(|v| {
                v.as_object_mut().unwrap().remove("server");
            })
            .errors(&env());
        assert!(has(&errors, "server_fixed needs a server"), "{errors:?}");
        // optional fields may be missing
        let min = f().brand(|v| {
            let m = v.as_object_mut().unwrap();
            for k in ["server", "server_fixed", "support_url"] {
                m.remove(k);
            }
        });
        let v = min.run(Some(DIR), Some(&env())).unwrap().unwrap();
        assert!(v.server.is_none() && !v.server_fixed && v.support_url.is_none());
        // not an object, not readable
        assert!(has(
            &f().set("brand.json", "[]").errors(&env()),
            "not a JSON object"
        ));
        assert!(has(
            &f().without("brand.json").errors(&env()),
            "brand.json cannot be read"
        ));
    }

    #[test]
    fn the_images_are_checked_by_size() {
        let f = Folder::good;
        for name in TRAY_FILES {
            let wrong = f().set(name, png(32, 32)).errors(&env());
            assert!(has(&wrong, &format!("{name} must be 64x64")), "{wrong:?}");
            assert!(has(
                &f().set(name, "nope").errors(&env()),
                &format!("{name} is not a PNG")
            ));
            assert!(has(
                &f().without(name).errors(&env()),
                &format!("{name} cannot be read")
            ));
        }
        assert!(has(
            &f().set("icon.png", png(255, 255)).errors(&env()),
            "256 to 1024"
        ));
        assert!(has(
            &f().set("icon.png", png(1025, 1025)).errors(&env()),
            "256 to 1024"
        ));
        assert!(has(
            &f().set("icon.png", png(512, 256)).errors(&env()),
            "square"
        ));
        assert!(has(
            &f().set("icon.png", "x").errors(&env()),
            "icon.png is not a PNG"
        ));
        let mut big = png(512, 512);
        big.resize(256 * 1024 + 1, 0);
        assert!(has(
            &f().set("icon.png", big).errors(&env()),
            "larger than 256 KiB"
        ));
        let mut limit = png(512, 512);
        limit.resize(256 * 1024, 0);
        assert!(
            f().set("icon.png", limit)
                .run(Some(DIR), Some(&env()))
                .is_ok()
        );
        assert!(has(
            &f().without("icon.png").errors(&env()),
            "icon.png cannot be read"
        ));
    }

    #[test]
    fn the_tauri_patch_may_set_only_what_a_brand_needs() {
        let f = Folder::good;
        let refused = |edit: fn(&mut Value), needle: &str| {
            let mut p = patch();
            edit(&mut p);
            // refused in the folder's patch and in TAURI_CONFIG alike
            let in_folder = f().patch(edit).errors(&p.to_string());
            assert!(has(&in_folder, needle), "{needle}: {in_folder:?}");
            let in_env = f().errors(&p.to_string());
            assert!(has(&in_env, needle), "{needle}: {in_env:?}");
        };
        refused(
            |p| p["identifier"] = "com.acme.setup".into(),
            "\"identifier\" is not allowed",
        );
        refused(
            |p| p["mainBinaryName"] = "acme".into(),
            "\"mainBinaryName\" is not allowed",
        );
        refused(
            |p| p["app"] = serde_json::json!({"windows": []}),
            "\"app\" is not allowed",
        );
        refused(
            |p| p["plugins"] = serde_json::json!({"updater": {}}),
            "\"plugins\" is not allowed",
        );
        refused(
            |p| p["build"] = serde_json::json!({}),
            "\"build\" is not allowed",
        );
        refused(
            |p| p["bundle"]["targets"] = serde_json::json!(["msi"]),
            "bundle.targets",
        );
        refused(
            |p| p["bundle"]["linux"]["rpm"] = serde_json::json!({}),
            "bundle.linux.rpm",
        );
        refused(
            |p| p["bundle"]["linux"]["appimage"] = serde_json::json!({}),
            "bundle.linux.appimage",
        );
        refused(
            |p| p["bundle"]["linux"]["deb"]["depends"] = serde_json::json!(["x"]),
            "bundle.linux.deb.depends",
        );
        refused(
            |p| p["bundle"]["windows"] = serde_json::json!({}),
            "bundle.windows",
        );
        refused(
            |p| p["bundle"]["macOS"] = serde_json::json!({}),
            "bundle.macOS",
        );
        refused(|p| p["bundle"] = "x".into(), "bundle must be an object");
        // not an object at all
        assert!(has(
            &f().set("tauri.conf.json", "[]").errors(&env()),
            "must be a JSON object"
        ));
        assert!(has(
            &f().set("tauri.conf.json", "{").errors(&env()),
            "not valid JSON"
        ));
        assert!(has(
            &f().without("tauri.conf.json").errors(&env()),
            "tauri.conf.json cannot be read"
        ));
    }

    #[test]
    fn the_patch_needs_name_icons_and_the_package_relations() {
        let f = Folder::good;
        let fail = |edit: fn(&mut Value), needle: &str| {
            let mut p = patch();
            edit(&mut p);
            let errors = f().patch(edit).errors(&p.to_string());
            assert!(has(&errors, needle), "{needle}: {errors:?}");
        };
        fail(
            |p| {
                p.as_object_mut().unwrap().remove("productName");
            },
            "productName is required",
        );
        fail(
            |p| p["productName"] = "printpapi Agent Setup".into(),
            "must differ from",
        );
        fail(
            |p| p["productName"] = "Other".into(),
            "must equal the name in brand.json",
        );
        fail(
            |p| {
                p["bundle"].as_object_mut().unwrap().remove("icon");
            },
            "bundle.icon is required",
        );
        fail(
            |p| p["bundle"]["icon"] = serde_json::json!(["icons/icon.png"]),
            "needs an .ico",
        );
        fail(
            |p| p["bundle"]["icon"] = serde_json::json!(["icons/icon.ico", "icons/missing.png"]),
            "missing.png cannot be read",
        );
        fail(
            |p| p["bundle"]["icon"] = serde_json::json!([]),
            "list of file names",
        );
        fail(
            |p| p["bundle"]["icon"] = serde_json::json!(["icons/icon.ico", 1]),
            "list of file names",
        );
        for key in ["replaces", "conflicts"] {
            let missing = |p: &mut Value| {
                p["bundle"]["linux"]["deb"]
                    .as_object_mut()
                    .unwrap()
                    .remove("replaces");
                p["bundle"]["linux"]["deb"]
                    .as_object_mut()
                    .unwrap()
                    .remove("conflicts");
            };
            fail(missing, &format!("bundle.linux.deb.{key} must be exactly"));
        }
        fail(
            |p| p["bundle"]["linux"]["deb"]["replaces"] = serde_json::json!(["other"]),
            "deb.replaces must be exactly",
        );
        fail(
            |p| {
                p["bundle"]["linux"]["deb"]["conflicts"] =
                    serde_json::json!(["printpapi-agent-setup", "x"])
            },
            "deb.conflicts must be exactly",
        );
    }

    #[test]
    fn the_build_must_carry_the_patch_of_the_folder() {
        let f = Folder::good;
        // no TAURI_CONFIG at all: the message gives the command
        let errors = f().run(Some(DIR), None).unwrap_err();
        assert!(
            has(&errors, "TAURI_CONFIG is not set")
                && has(
                    &errors,
                    "cargo tauri build --config /brands/acme/tauri.conf.json"
                ),
            "{errors:?}"
        );
        assert!(f().run(Some(DIR), Some("  ")).is_err());
        // a different name, icon list or package relation
        for edit in [
            (|p: &mut Value| p["productName"] = "Other".into()) as fn(&mut Value),
            |p| p["bundle"]["icon"] = serde_json::json!(["icons/icon.ico"]),
            |p| p["bundle"]["linux"]["deb"]["replaces"] = serde_json::json!(["x"]),
            |p| {
                p["bundle"]["linux"]["deb"]
                    .as_object_mut()
                    .unwrap()
                    .remove("conflicts");
            },
        ] {
            let mut p = patch();
            edit(&mut p);
            let errors = f().errors(&p.to_string());
            assert!(
                has(&errors, "does not carry the same productName"),
                "{errors:?}"
            );
            // the remedy is the exact command, not a placeholder
            assert!(
                has(
                    &errors,
                    "cargo tauri build --config /brands/acme/tauri.conf.json`"
                ),
                "{errors:?}"
            );
            assert!(!has(&errors, "<brand dir>"), "{errors:?}");
        }
        // a refused key in TAURI_CONFIG names the command as well
        let errors = f().errors(r#"{"mainBinaryName":"x","productName":"Acme"}"#);
        assert!(
            has(&errors, "the key \"mainBinaryName\" is not allowed")
                && has(
                    &errors,
                    "cargo tauri build --config /brands/acme/tauri.conf.json`"
                ),
            "{errors:?}"
        );
        // other keys of the patch (publisher) may differ
        let mut p = patch();
        p["bundle"]["publisher"] = "Other GmbH".into();
        assert!(f().run(Some(DIR), Some(&p.to_string())).is_ok());
        // not JSON
        assert!(has(&f().errors("{"), "TAURI_CONFIG is not valid JSON"));
    }

    #[test]
    fn a_brand_config_without_the_brand_folder_fails_naming_the_variable() {
        for cfg in [
            r#"{"productName":"Acme"}"#,
            r#"{"bundle":{"icon":["a.ico"]}}"#,
        ] {
            let errors = Folder::good().run(None, Some(cfg)).unwrap_err();
            assert!(has(&errors, "PRINTPAPI_BRAND_DIR is not set"), "{errors:?}");
            let errors = Folder::good().run(Some(""), Some(cfg)).unwrap_err();
            assert!(has(&errors, "PRINTPAPI_BRAND_DIR is not set"), "{errors:?}");
        }
        assert!(Folder::good().run(None, Some("{")).is_err());
    }

    #[test]
    fn the_brand_folder_must_be_an_absolute_path() {
        for ok in ["/a/b", "C:\\a\\b", "c:/a/b", "\\\\server\\share\\b"] {
            assert!(is_absolute(ok), "{ok}");
        }
        for bad in ["a/b", "./a", "brands/acme", "..\\a", "C:a", ""] {
            assert!(!is_absolute(bad), "{bad}");
        }
        let errors = Folder::good()
            .run(Some("brands/acme"), Some(&env()))
            .unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(has(&errors, "must be an absolute path"), "{errors:?}");
        // a trailing separator is fine
        let mut folder = Folder::good();
        let moved: Vec<(String, Vec<u8>)> = folder.0.drain().collect();
        for (k, v) in moved {
            folder.0.insert(k.replacen(DIR, "C:/brands/acme", 1), v);
        }
        assert!(folder.run(Some("C:/brands/acme/"), Some(&env())).is_ok());
    }

    #[test]
    fn every_fault_is_reported_at_once() {
        let errors = Folder::good()
            .brand(|v| {
                v["id"] = "http".into();
                v["notice"] = "x".into();
            })
            .set("tray-online.png", png(32, 32))
            .without("icon.png")
            .patch(|p| p["identifier"] = "x".into())
            .errors(r#"{"mainBinaryName":"x"}"#);
        for needle in [
            "id \"http\"",
            "unknown key \"notice\"",
            "tray-online.png must be 64x64",
            "icon.png cannot be read",
            "\"identifier\" is not allowed",
            "TAURI_CONFIG: the key \"mainBinaryName\" is not allowed",
        ] {
            assert!(has(&errors, needle), "{needle}: {errors:?}");
        }
    }
}
