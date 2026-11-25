// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The compile-time brand of this build (docs/white-label.md). Without `PRINTPAPI_BRAND_DIR` there is
//! none ([`current`] is `None`) and the program is the stock printpapi setup. The files and the
//! checks are the build script's; this module holds what the program does with them.
//!
//! Every function takes the brand as an argument (`None` is stock), so that the logic is tested
//! with an injected brand in any build; only [`current`] and the commands read the compiled-in one.
//!
//! The words a person reads about the licence are written here, in code: a brand has no field for
//! a notice, a licence text, a "powered by" line or a footer (LICENSE: additional permission,
//! conditions (a) and (b), and the paragraphs about notices and modified copies).

use std::path::Path;

// ponytail: the names an administrator sees (the scheduled task, the service, the launchd labels,
// the install folder, the macOS app in the Applications folder, the program's file name) stay the
// stock ones, because detection, removal and the link between the window and the tray rest on
// them. Ceiling: a client who looks in the Applications folder or the list of tasks sees the real
// names.
use printpapi_agent::brand as rules;
use printpapi_agent::install::{self, Os};
use printpapi_agent::proc::Cmd;
use printpapi_agent::setup;
use serde::Serialize;

use crate::commands::SetupError;

#[cfg(branded)]
mod assets {
    include!(concat!(env!("OUT_DIR"), "/brand_assets.rs"));
}

/// The title of the window and the error box of the stock build (its Tauri `productName`).
pub const STOCK_TITLE: &str = "printpapi Agent Setup";
/// The name the stock tray tooltip starts with.
pub const STOCK_PRODUCT: &str = "printpapi Agent";

/// The three tray images, one per state of the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayImages {
    pub online: &'static [u8],
    pub offline: &'static [u8],
    pub stopped: &'static [u8],
}

/// A white-label brand as the program holds it. The fields are those of `brand.json`, the images
/// are the brand folder's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brand {
    pub id: String,
    pub name: String,
    pub client: String,
    pub server: Option<String>,
    pub server_fixed: bool,
    pub support_url: Option<String>,
    pub icon: &'static [u8],
    pub tray: TrayImages,
}

impl Brand {
    /// From the file the build script wrote (it has been checked there).
    #[cfg(any(branded, test))]
    pub fn from_json(text: &str, icon: &'static [u8], tray: TrayImages) -> Result<Brand, String> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let text = |key: &str| v.get(key).and_then(|x| x.as_str()).map(str::to_string);
        let need = |key: &str| text(key).ok_or_else(|| format!("brand.json has no {key}"));
        Ok(Brand {
            id: need("id")?,
            name: need("name")?,
            client: need("client")?,
            server: text("server"),
            server_fixed: v
                .get("server_fixed")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            support_url: text("support_url"),
            icon,
            tray,
        })
    }
}

/// The brand compiled into this program: none for the stock build.
#[cfg(branded)]
pub fn current() -> Option<&'static Brand> {
    static BRAND: std::sync::OnceLock<Brand> = std::sync::OnceLock::new();
    Some(BRAND.get_or_init(|| {
        Brand::from_json(
            assets::BRAND_JSON,
            assets::ICON,
            TrayImages {
                online: assets::TRAY_ONLINE,
                offline: assets::TRAY_OFFLINE,
                stopped: assets::TRAY_STOPPED,
            },
        )
        .expect("the build script checked brand.json")
    }))
}

/// The brand compiled into this program: none for the stock build.
#[cfg(not(branded))]
pub fn current() -> Option<&'static Brand> {
    None
}

/// The name the tray and the sidebar call the program.
pub fn product_name(brand: Option<&Brand>) -> &str {
    brand.map_or(STOCK_PRODUCT, |b| &b.name)
}

/// The title of the window and of the box that says why the program cannot start.
pub fn window_title(brand: Option<&Brand>) -> &str {
    brand.map_or(STOCK_TITLE, |b| &b.name)
}

/// What the tray tooltip starts with. With the status line (at most 100 characters) it stays below
/// the 127 characters of a Windows tooltip: `rules::NAME_MAX` is what is left for the name.
pub fn tooltip_prefix(brand: Option<&Brand>) -> String {
    format!("{}: ", product_name(brand))
}

/// The licence notice of a modified build, in the words the licence asks for (a modified copy
/// carries a prominent notice, the licensor's notices are not removed, recipients get the terms).
/// Empty for the stock build, which is not modified.
pub fn notice(brand: Option<&Brand>) -> String {
    brand.map_or_else(String::new, |b| {
        format!(
            "Modified build of printpapi {}, (c) 2026 Lukas Wotzka, source available under the Elastic License 2.0. Operated for {}.",
            env!("CARGO_PKG_VERSION"),
            b.client
        )
    })
}

/// The disabled line of the tray menu of a modified build; nothing for the stock build.
pub fn tray_notice(brand: Option<&Brand>) -> Option<&'static str> {
    brand.map(|_| "Based on printpapi, Elastic License 2.0 (modified)")
}

/// The full text of LICENSE (compiled in for a brand, so About can show it); empty for stock.
#[cfg(branded)]
pub const LICENSE: &str = include_str!("../../../LICENSE");
#[cfg(not(branded))]
pub const LICENSE: &str = "";

/// May this build look for, download and install updates? Every brand says no: the updater's
/// files and keys are the stock build's, and a branded build has no update source of its own (an
/// updater has to ask this first). The agent side refuses on a branded marker as well.
pub fn updates_enabled(brand: Option<&Brand>) -> bool {
    brand.is_none()
}

/// `supported` (this build has an update key) and the brand allows updates. The one question that
/// `update_info` and the download command ask, so that a brand is never offered one of them.
pub fn updates_possible(supported: bool, brand: Option<&Brand>) -> bool {
    supported && updates_enabled(brand)
}

/// The fixed server of a brand: every address that the window or a pairing link brings must be
/// that server (compared after `normalize_server_url` and lower-casing, so a trailing slash or a
/// capital in the host does not matter). A convenience for the person, not a security control:
/// `agent.ini` is editable by an administrator.
pub fn enforce_server(brand: Option<&Brand>, url: &str) -> Result<(), SetupError> {
    let Some(fixed) = brand
        .filter(|b| b.server_fixed)
        .and_then(|b| b.server.as_deref())
    else {
        return Ok(());
    };
    let same = |text: &str| setup::normalize_server_url(text).map(|u| u.to_ascii_lowercase());
    match (same(url), same(fixed)) {
        (Ok(asked), Ok(wanted)) if asked == wanted => Ok(()),
        _ => Err(SetupError::new(
            "server_fixed",
            format!("This setup connects to {fixed} only."),
        )),
    }
}

/// What the window is told about the brand (no images: the icon is fetched as raw bytes).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandView {
    pub id: String,
    pub name: String,
    pub client: String,
    pub server: Option<String>,
    pub server_fixed: bool,
    pub has_support: bool,
    pub version: String,
    pub notice: String,
}

pub fn view(brand: Option<&Brand>) -> Option<BrandView> {
    brand.map(|b| BrandView {
        id: b.id.clone(),
        name: b.name.clone(),
        client: b.client.clone(),
        server: b.server.clone(),
        server_fixed: b.server_fixed,
        has_support: b.support_url.is_some(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        notice: notice(brand),
    })
}

/// The command that opens the support page of the brand: `https` only, through the one rule
/// the tray's "Open dashboard" uses.
pub fn support_cmd(brand: Option<&Brand>, os: Os, sys: &Path) -> Result<Cmd, SetupError> {
    let none = || SetupError::new("system", "This build has no support page.");
    let url = brand
        .and_then(|b| b.support_url.as_deref())
        .ok_or_else(none)?;
    if !rules::valid_https_url(url) {
        return Err(none());
    }
    crate::tray::open_url_cmd(os, sys, url).ok_or_else(none)
}

/// Tell the elevated install which brand this window is, so that it writes the brand into the
/// marker and the Linux menu entries (`brand.json` of the staging folder). The stock build writes
/// nothing: its staging folder is what it always was.
pub fn stage_brand(brand: Option<&Brand>, dir: &Path) -> Result<(), SetupError> {
    let Some(b) = brand else {
        return Ok(());
    };
    let json = serde_json::json!({"id": b.id, "name": b.name}).to_string();
    crate::staging::write_private(dir, "brand.json", json.as_bytes())
        .map(|_| ())
        .map_err(|e| SetupError::io("cannot write the brand", e))
}

// ---- commands

/// The brand of this build, `null` for the stock build.
#[tauri::command(async)]
pub fn get_brand() -> Option<BrandView> {
    view(current())
}

/// The brand's window icon as the raw bytes of the PNG (the window makes the data address itself:
/// there is no base64 encoder in this program, and `blob:` is outside the content security policy).
#[tauri::command(async)]
pub fn brand_icon() -> Result<tauri::ipc::Response, SetupError> {
    current()
        .map(|b| tauri::ipc::Response::new(b.icon.to_vec()))
        .ok_or_else(|| SetupError::new("system", "This build has no brand."))
}

/// The text of LICENSE for the About dialog; empty for the stock build.
#[tauri::command(async)]
pub fn license_text() -> String {
    LICENSE.to_string()
}

#[tauri::command(async)]
pub fn open_support() -> Result<(), SetupError> {
    let cmd = support_cmd(current(), Os::host(), &install::system_dir())?;
    std::thread::spawn(move || {
        let _ = printpapi_agent::proc::run_command(&cmd);
    });
    Ok(())
}

/// Show the window that a branded build makes hidden (so that the stock page is never seen first).
#[tauri::command(async)]
pub fn show_window(window: tauri::WebviewWindow) {
    let _ = window.show();
}

/// A brand with a fixed server, for the tests of this module and of the commands that enforce it.
#[cfg(test)]
pub(crate) fn acme() -> Brand {
    Brand {
        id: "acme".into(),
        name: "Acme Print".into(),
        client: "Acme GmbH".into(),
        server: Some("https://print.acme.example".into()),
        server_fixed: true,
        support_url: Some("https://acme.example/help".into()),
        icon: b"icon",
        tray: TrayImages {
            online: b"on",
            offline: b"off",
            stopped: b"stop",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stock_names_are_what_the_program_always_said() {
        assert_eq!(window_title(None), "printpapi Agent Setup");
        assert_eq!(product_name(None), "printpapi Agent");
        assert_eq!(tooltip_prefix(None), "printpapi Agent: ");
        assert_eq!(notice(None), "");
        assert_eq!(tray_notice(None), None);
        assert!(updates_enabled(None));
        assert_eq!(view(None), None);
        assert!(enforce_server(None, "https://anything.example").is_ok());
    }

    #[cfg(not(branded))]
    #[test]
    fn a_stock_build_has_no_brand_no_licence_text_and_no_support_page() {
        assert!(current().is_none());
        assert_eq!(LICENSE, "");
        assert_eq!(license_text(), "");
        assert!(brand_icon().is_err());
        assert!(get_brand().is_none());
        assert!(open_support().is_err());
        let tmp = tempfile::tempdir().unwrap();
        stage_brand(current(), tmp.path()).unwrap();
        assert!(!tmp.path().join("brand.json").exists());
        // the tray images of the stock build are the files of crates/setup/icons
        assert_eq!(
            crate::tray::icon_bytes(None, crate::tray::TrayState::Online),
            include_bytes!("../icons/tray-online.png")
        );
    }

    #[test]
    fn a_brand_names_the_window_the_tooltip_and_the_licence_duties() {
        let b = acme();
        assert_eq!(window_title(Some(&b)), "Acme Print");
        assert_eq!(product_name(Some(&b)), "Acme Print");
        assert_eq!(tooltip_prefix(Some(&b)), "Acme Print: ");
        let n = notice(Some(&b));
        for word in [
            "Modified",
            "Elastic License 2.0",
            "Lukas Wotzka",
            env!("CARGO_PKG_VERSION"),
            "Acme GmbH",
            "source available",
        ] {
            assert!(n.contains(word), "{word}: {n}");
        }
        assert!(!n.to_lowercase().contains("open source"), "{n}");
        assert!(
            tray_notice(Some(&b))
                .unwrap()
                .contains("Elastic License 2.0 (modified)")
        );
        // the tooltip stays inside what a Windows tooltip holds (127 UTF-16 units)
        let longest = "a".repeat(rules::NAME_MAX);
        let units = tooltip_prefix(Some(&Brand { name: longest, ..b }))
            .encode_utf16()
            .count();
        assert!(units + crate::tray::LINE_MAX <= 127, "{units}");
    }

    #[test]
    fn a_brand_is_never_offered_an_update_whatever_the_build_supports() {
        // the question `update_info` and the download command ask
        assert!(updates_possible(true, None));
        assert!(!updates_possible(false, None));
        assert!(!updates_possible(true, Some(&acme())));
        assert!(!updates_possible(false, Some(&acme())));
    }

    #[test]
    fn every_brand_switches_updates_off() {
        assert!(!updates_enabled(Some(&acme())));
        let mut other = acme();
        other.server_fixed = false;
        other.support_url = None;
        assert!(!updates_enabled(Some(&other)));
    }

    #[test]
    fn the_view_carries_what_the_window_shows_and_no_image() {
        let v = view(Some(&acme())).unwrap();
        assert_eq!(v.id, "acme");
        assert_eq!(v.name, "Acme Print");
        assert_eq!(v.client, "Acme GmbH");
        assert_eq!(v.server.as_deref(), Some("https://print.acme.example"));
        assert!(v.server_fixed && v.has_support);
        assert_eq!(v.version, env!("CARGO_PKG_VERSION"));
        assert!(v.notice.contains("Modified"));
        let json = serde_json::to_value(&v).unwrap();
        for key in [
            "id",
            "name",
            "client",
            "server",
            "serverFixed",
            "hasSupport",
            "version",
            "notice",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        // the support address itself is not sent: the window asks for it to be opened
        assert!(!json.to_string().contains("acme.example/help"));
    }

    #[test]
    fn a_fixed_server_accepts_only_itself() {
        let b = acme();
        for ok in [
            "https://print.acme.example",
            "https://print.acme.example/",
            "  https://print.acme.example  ",
            "HTTPS://PRINT.ACME.EXAMPLE",
            "https://Print.Acme.example//",
        ] {
            assert!(enforce_server(Some(&b), ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://print.other.example",
            "http://print.acme.example",
            "https://print.acme.example:8443",
            "https://print.acme.example/sub",
            "https://print.acme.example.evil.example",
            "",
            "not an address",
            "ftp://print.acme.example",
        ] {
            let e = enforce_server(Some(&b), bad).unwrap_err();
            assert_eq!(e.code, "server_fixed", "{bad}");
            assert_eq!(
                e.message,
                "This setup connects to https://print.acme.example only."
            );
        }
        // a server that is only the default does not restrict anything
        let mut unfixed = acme();
        unfixed.server_fixed = false;
        assert!(enforce_server(Some(&unfixed), "https://print.other.example").is_ok());
        // and without a server there is nothing to enforce
        let mut none = acme();
        none.server = None;
        assert!(enforce_server(Some(&none), "https://print.other.example").is_ok());
    }

    #[test]
    fn the_support_page_opens_only_over_https_through_the_shared_rule() {
        let sys = Path::new("C:\\Windows\\System32");
        let cmd = support_cmd(Some(&acme()), Os::Windows, sys).unwrap();
        assert_eq!(
            cmd.argv,
            ["C:\\Windows\\explorer.exe", "https://acme.example/help"]
        );
        let linux = support_cmd(Some(&acme()), Os::Linux, Path::new("/")).unwrap();
        assert_eq!(linux.argv, ["xdg-open", "https://acme.example/help"]);
        let mut b = acme();
        for url in [
            "http://acme.example/help",
            "ftp://x",
            "https://",
            "https://a b",
            "javascript:alert(1)",
        ] {
            b.support_url = Some(url.into());
            assert!(
                support_cmd(Some(&b), Os::Linux, Path::new("/")).is_err(),
                "{url}"
            );
        }
        b.support_url = None;
        assert!(support_cmd(Some(&b), Os::Linux, Path::new("/")).is_err());
        assert!(support_cmd(None, Os::Linux, Path::new("/")).is_err());
    }

    #[test]
    fn staging_the_brand_writes_the_id_and_the_name_that_the_install_reads() {
        let tmp = tempfile::tempdir().unwrap();
        stage_brand(Some(&acme()), tmp.path()).unwrap();
        let bytes = std::fs::read(tmp.path().join("brand.json")).unwrap();
        let staged = rules::parse_staged(&bytes).unwrap();
        assert_eq!(
            (staged.id.as_str(), staged.name.as_str()),
            ("acme", "Acme Print")
        );
        // nothing else of the brand (server, client, support address) goes to the elevated side
        assert_eq!(bytes, br#"{"id":"acme","name":"Acme Print"}"#);
        // the stock build stages nothing
        let stock = tempfile::tempdir().unwrap();
        stage_brand(None, stock.path()).unwrap();
        assert!(!stock.path().join("brand.json").exists());
    }

    #[test]
    fn the_brand_file_of_the_build_script_is_read_back() {
        let text = acme_json();
        let b = Brand::from_json(
            &text,
            b"i",
            TrayImages {
                online: b"a",
                offline: b"b",
                stopped: b"c",
            },
        )
        .unwrap();
        assert_eq!(b.id, "acme");
        assert_eq!(b.server.as_deref(), Some("https://print.acme.example"));
        assert!(b.server_fixed);
        let minimal = r#"{"id":"acme","name":"Acme","client":"A","server":null,"server_fixed":false,"support_url":null}"#;
        let m = Brand::from_json(minimal, b"i", b.tray.clone()).unwrap();
        assert!(m.server.is_none() && !m.server_fixed && m.support_url.is_none());
        assert!(Brand::from_json("{}", b"i", b.tray.clone()).is_err());
        assert!(Brand::from_json("x", b"i", b.tray.clone()).is_err());
    }

    // ---- the tests of the compiled-in brand: they run for ANY brand (docs/white-label.md asks a
    // provider to run `cargo test` with their brand before shipping)

    #[cfg(branded)]
    #[test]
    fn the_images_of_the_brand_decode_with_the_sizes_the_build_script_checked() {
        use tauri::image::Image;
        let b = current().expect("a branded build has a brand");
        let icon = Image::from_bytes(b.icon).expect("icon.png decodes");
        assert_eq!(icon.width(), icon.height());
        assert!((256..=1024).contains(&icon.width()), "{}", icon.width());
        for (name, bytes) in [
            ("tray-online.png", b.tray.online),
            ("tray-offline.png", b.tray.offline),
            ("tray-stopped.png", b.tray.stopped),
        ] {
            let image = Image::from_bytes(bytes).unwrap_or_else(|e| panic!("{name} decodes: {e}"));
            assert_eq!((image.width(), image.height()), (64, 64), "{name}");
        }
        // the icons are the brand's, not the stock ones
        for (stock, brand) in [
            (
                &include_bytes!("../icons/tray-online.png")[..],
                b.tray.online,
            ),
            (
                &include_bytes!("../icons/tray-offline.png")[..],
                b.tray.offline,
            ),
            (
                &include_bytes!("../icons/tray-stopped.png")[..],
                b.tray.stopped,
            ),
        ] {
            assert_ne!(stock, brand);
        }
    }

    #[cfg(branded)]
    #[test]
    fn a_branded_build_carries_the_notice_the_licence_and_no_updates() {
        let b = current().expect("a branded build has a brand");
        let n = notice(Some(b));
        assert!(
            n.contains(&b.client) && n.contains("Modified") && n.contains("Elastic License 2.0"),
            "{n}"
        );
        assert!(
            LICENSE.contains("Elastic License 2.0"),
            "the whole LICENSE is embedded"
        );
        assert!(LICENSE.contains("Lukas Wotzka"));
        assert!(!updates_enabled(Some(b)));
        assert_eq!(window_title(Some(b)), b.name);
        assert!(rules::valid_id(&b.id) && rules::valid_name(&b.name));
        assert_eq!(get_brand().map(|v| v.id), Some(b.id.clone()));
        assert!(!license_text().is_empty());
        assert!(brand_icon().is_ok());
    }

    /// Only the example brand of this repository: any other brand returns early.
    #[cfg(branded)]
    #[test]
    fn example_brand_is_compiled_in() {
        let b = current().expect("a branded build has a brand");
        if b.id != "example" {
            return;
        }
        assert_eq!(b.name, "Example Print");
        assert_eq!(b.client, "Example Client GmbH");
        assert_eq!(b.server.as_deref(), Some("https://print.example.com"));
        assert!(!b.server_fixed);
        assert_eq!(
            b.support_url.as_deref(),
            Some("https://example.com/support")
        );
    }

    fn acme_json() -> String {
        serde_json::json!({
            "id": "acme", "name": "Acme Print", "client": "Acme GmbH",
            "server": "https://print.acme.example", "server_fixed": true,
            "support_url": "https://acme.example/help"
        })
        .to_string()
    }
}
