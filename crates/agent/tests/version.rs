// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The real binary: `--version` is what an update runs on the new program before it swaps anything.

use std::process::Command;

fn agent() -> Command {
    Command::new(env!("CARGO_BIN_EXE_printpapi-agent"))
}

#[test]
fn version_prints_one_line_and_exits_zero() {
    let out = agent().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("printpapi-agent {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn only_exactly_this_argument_is_a_version_request() {
    // anything else is no version request: without an agent.ini next to the binary it stops with
    // its usual message and exit code 1, which proves it ran the agent and printed no version
    for args in [
        &["--version", "x"][..],
        &["x", "--version"],
        &["--VERSION"],
        &["-V"],
    ] {
        let out = agent().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            !String::from_utf8_lossy(&out.stdout).starts_with("printpapi-agent "),
            "{args:?}"
        );
    }
}
