// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! `printpapi-agent`: reads agent.ini next to the executable, registers, then polls for jobs.

use std::process::ExitCode;

use printpapi_agent::{Fatal, err_line, install_panic_hook, log, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // exactly this one argument, before anything else: the updater starts the new program with it
    // to learn that it runs at all and which version it is. Every other word still runs the agent.
    if args == ["--version"] {
        printpapi_agent::out_line(&printpapi_agent::version_line());
        return ExitCode::SUCCESS;
    }
    install_panic_hook();
    if let Some(code) = printpapi_agent::apply::cli(&args) {
        return code;
    }
    let base_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match run(&base_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(fatal) => {
            // A bad agent.ini is logged as "exit: ..." (it would otherwise leave no trace when
            // there is no console); anything else is logged as the crash it is.
            log::log_error(&fatal.log_line());
            let (Fatal::Exit(m) | Fatal::Crash(m)) = &fatal;
            err_line(m);
            ExitCode::from(1)
        }
    }
}
