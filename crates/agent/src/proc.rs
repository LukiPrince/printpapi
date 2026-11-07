// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Running `lp`, `lpoptions` and SumatraPDF: one injectable [`Runner`] so the command lines can be
//! tested without a printer. Never a shell; argv only.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::pyrepr::repr_str;

/// One external command. `check=True` semantics: a non-zero exit is an error.
#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub argv: Vec<String>,
    /// Bytes for the child's stdin (`lp` reads the job from it).
    pub input: Option<Vec<u8>>,
    pub timeout: Duration,
    /// Capture stdout (`lpoptions`); otherwise the child inherits the console.
    pub capture: bool,
}

impl Cmd {
    pub fn new(argv: Vec<String>, timeout_secs: u64) -> Cmd {
        Cmd {
            argv,
            input: None,
            timeout: Duration::from_secs(timeout_secs),
            capture: false,
        }
    }
}

pub type Runner = Rc<dyn Fn(&Cmd) -> Result<Vec<u8>, String>>;

/// Python's `str(list)` for an argv, as it appears in `subprocess` error texts.
fn argv_repr(argv: &[String]) -> String {
    format!(
        "[{}]",
        argv.iter()
            .map(|a| repr_str(a))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The real [`Runner`]: spawn, feed stdin, wait up to the timeout (killing the child after it),
/// error on a non-zero exit. Error texts follow `subprocess`' (`CalledProcessError`,
/// `TimeoutExpired`).
pub fn real_runner() -> Runner {
    Rc::new(run_command)
}

pub fn run_command(cmd: &Cmd) -> Result<Vec<u8>, String> {
    run_command_with(cmd, &|_| {})
}

/// [`run_command`] with a last look at the [`Command`] before it starts: the setup window uses it
/// to keep a console window from flashing up for every `schtasks` and `tasklist` (Windows
/// `CREATE_NO_WINDOW`).
pub fn run_command_with(cmd: &Cmd, configure: &dyn Fn(&mut Command)) -> Result<Vec<u8>, String> {
    let Some((program, args)) = cmd.argv.split_first() else {
        return Err("empty command".into());
    };
    let mut c = Command::new(program);
    c.args(args)
        .stdin(if cmd.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if cmd.capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stderr(if cmd.capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
    configure(&mut c);
    let mut child = c
        .spawn()
        .map_err(|e| format!("{e}: {}", repr_str(program)))?;
    // Feed stdin and drain stdout from threads: a child that fills its pipe while we are still
    // writing (or vice versa) must not deadlock us.
    let writer = child
        .stdin
        .take()
        .zip(cmd.input.clone())
        .map(|(mut stdin, data)| {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&data); // a child that exits early closes the pipe; its status tells
            })
        });
    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_drain = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = err.read_to_end(&mut sink);
        })
    });
    let status = match child.wait_timeout(cmd.timeout).map_err(|e| e.to_string())? {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Command '{}' timed out after {} seconds",
                argv_repr(&cmd.argv),
                cmd.timeout.as_secs_f64()
            ));
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let stdout = reader
        .map(|r| r.join().unwrap_or_default())
        .unwrap_or_default();
    if let Some(d) = stderr_drain {
        let _ = d.join();
    }
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        return Err(format!(
            "Command '{}' returned non-zero exit status {code}.",
            argv_repr(&cmd.argv)
        ));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), script.into()]
        } else {
            vec!["sh".into(), "-c".into(), script.into()]
        }
    }

    #[test]
    fn captures_stdout_and_succeeds_on_zero() {
        let mut c = Cmd::new(shell("echo hi"), 10);
        c.capture = true;
        assert_eq!(
            String::from_utf8_lossy(&run_command(&c).unwrap()).trim(),
            "hi"
        );
    }

    #[test]
    fn configure_sees_the_command_before_it_starts() {
        let script = if cfg!(windows) {
            "echo %PP_PROC_TEST%"
        } else {
            "echo $PP_PROC_TEST"
        };
        let mut c = Cmd::new(shell(script), 10);
        c.capture = true;
        let out = run_command_with(&c, &|command| {
            command.env("PP_PROC_TEST", "configured");
        })
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "configured");
    }

    #[test]
    fn non_zero_exit_is_called_process_error_text() {
        let e = run_command(&Cmd::new(shell("exit 3"), 10)).unwrap_err();
        assert!(e.starts_with("Command '['"), "{e}");
        assert!(e.ends_with("returned non-zero exit status 3."), "{e}");
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        let e =
            run_command(&Cmd::new(vec!["definitely-not-a-program-xyz".into()], 10)).unwrap_err();
        assert!(e.contains("definitely-not-a-program-xyz"), "{e}");
    }

    #[test]
    fn stdin_reaches_the_child() {
        let script = if cfg!(windows) { "findstr x*" } else { "cat" };
        let mut c = Cmd::new(shell(script), 10);
        c.input = Some(b"ZPL-bytes\r\n".to_vec());
        c.capture = true;
        assert!(String::from_utf8_lossy(&run_command(&c).unwrap()).contains("ZPL-bytes"));
    }

    #[test]
    fn a_hanging_child_is_killed_at_the_timeout() {
        let script = if cfg!(windows) {
            "ping -n 20 127.0.0.1 >nul"
        } else {
            "sleep 20"
        };
        let mut c = Cmd::new(shell(script), 1);
        c.timeout = Duration::from_millis(300);
        let started = std::time::Instant::now();
        let e = run_command(&c).unwrap_err();
        assert!(e.contains("timed out after"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
