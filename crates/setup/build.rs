// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Tells the compiler which agent binary to embed (`AGENT_BIN`), checks a white-label brand and runs
//! the Tauri build step.
//!
//! The agent is built first: `cargo build --release -p printpapi-agent` from the repo root, or
//! point `PRINTPAPI_AGENT_BIN` at another build (a cross-compiled one, for instance).
//!
//! A brand is a folder named by the absolute path `PRINTPAPI_BRAND_DIR` (docs/white-label.md). It is
//! checked here by the rules of the agent's `brand` module (the same file, shared with `#[path]`),
//! together with `TAURI_CONFIG`, which has to carry the brand's `tauri.conf.json`: Tauri takes the
//! product name and the icons of the program from that variable, so a build with only one of the two
//! would be half a brand. A valid brand turns on `cfg(branded)` and its files are embedded.

use std::path::{Path, PathBuf};

#[allow(dead_code)] // the setup program uses other parts of the module than this script does
#[path = "../agent/src/brand.rs"]
mod brand;

/// The files of a brand folder that go into the program, as `(file, constant)`.
const EMBEDDED: [(&str, &str); 4] = [
    ("icon.png", "ICON"),
    ("tray-online.png", "TRAY_ONLINE"),
    ("tray-offline.png", "TRAY_OFFLINE"),
    ("tray-stopped.png", "TRAY_STOPPED"),
];

fn main() {
    println!("cargo:rerun-if-env-changed=PRINTPAPI_AGENT_BIN");
    println!("cargo:rerun-if-env-changed={}", brand::BRAND_DIR_ENV);
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    println!("cargo:rustc-check-cfg=cfg(branded)");
    let exe = if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        "printpapi-agent.exe"
    } else {
        "printpapi-agent"
    };
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let agent = std::env::var_os("PRINTPAPI_AGENT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../target/release").join(exe));
    let agent = agent.canonicalize().unwrap_or_else(|e| {
        panic!(
            "the agent binary to embed is missing ({}: {e}). Build it first with \
             `cargo build --release -p printpapi-agent`, or set PRINTPAPI_AGENT_BIN.",
            agent.display()
        )
    });
    println!("cargo:rerun-if-changed={}", agent.display());
    println!("cargo:rustc-env=AGENT_BIN={}", agent.display());
    embed_brand(&manifest);
    tauri_build::build();
}

/// A relative entry of bundle.icon is relative to the folder of tauri.conf.json, this one.
fn from_config_folder(manifest: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest.join(path)
    }
}

/// Check the brand (or the absence of one) and embed its files. Stops the build with every fault
/// at once.
fn embed_brand(manifest: &Path) {
    let dir = std::env::var(brand::BRAND_DIR_ENV).ok();
    let tauri_config = std::env::var("TAURI_CONFIG").ok();
    let read_file = |path: &str| std::fs::read(from_config_folder(manifest, path)).ok();
    let checked = brand::validate(dir.as_deref(), tauri_config.as_deref(), &read_file)
        .unwrap_or_else(|errors| {
            panic!(
                "the brand is not valid:\n  - {}\n(see docs/white-label.md)",
                errors.join("\n  - ")
            )
        });
    let Some(brand) = checked else {
        return;
    };
    let dir = PathBuf::from(dir.expect("a brand has a folder"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets this"));
    println!("cargo:rustc-cfg=branded");
    for file in ["brand.json", "tauri.conf.json"] {
        println!("cargo:rerun-if-changed={}", dir.join(file).display());
    }
    // the files of bundle.icon: `cargo tauri icon` overwrites them under the same names
    let patch = std::fs::read(dir.join("tauri.conf.json")).unwrap_or_default();
    for icon in brand::icon_paths(&patch) {
        println!(
            "cargo:rerun-if-changed={}",
            from_config_folder(manifest, &icon).display()
        );
    }
    std::fs::write(out.join("brand.json"), brand.to_json().to_string())
        .expect("cannot write brand.json to OUT_DIR");
    let mut assets = String::from(
        "pub const BRAND_JSON: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/brand.json\"));\n",
    );
    for (file, constant) in EMBEDDED {
        let path = dir
            .join(file)
            .canonicalize()
            .unwrap_or_else(|e| panic!("{file} of the brand cannot be found: {e}"));
        println!("cargo:rerun-if-changed={}", path.display());
        // {:?} writes the path as a Rust string literal, backslashes escaped
        assets.push_str(&format!(
            "pub static {constant}: &[u8] = include_bytes!({:?});\n",
            path.display().to_string()
        ));
    }
    std::fs::write(out.join("brand_assets.rs"), assets)
        .expect("cannot write brand_assets.rs to OUT_DIR");
}
