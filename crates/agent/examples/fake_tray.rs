// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! A stand-in for the setup program in the CI smoke test: `--version` prints
//! `printpapi-setup <version>` (the version of this build), anything else exits 0. It is what an
//! update puts in place of the tray program, because the real one needs a web view and a desktop.

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("printpapi-setup {}", env!("CARGO_PKG_VERSION"));
    }
}
