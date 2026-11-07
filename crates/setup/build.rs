// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Tells the compiler which agent binary to embed (`AGENT_BIN`) and runs the Tauri build step.
//! The agent is built first: `cargo build --release -p printpapi-agent` from the repo root, or
//! point `PRINTPAPI_AGENT_BIN` at another build (a cross-compiled one, for instance).

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PRINTPAPI_AGENT_BIN");
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
    tauri_build::build();
}
