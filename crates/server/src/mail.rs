// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Outbound e-mail for password resets — the port of `app/mail.py`.
//!
//! Configured from the environment (`SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD`,
//! `SMTP_FROM`, `SMTP_SSL`, `SMTP_STARTTLS`). No `SMTP_HOST` => the message goes to stderr, so a
//! self-host without a mail server still works and a missing config never looks delivered.
//! ponytail: synchronous, no retry and no queue — one mail per reset click.
//!
//! Known deviations from smtplib: lettre offers only AUTH PLAIN/LOGIN (a relay that advertises only
//! CRAM-MD5 fails), it rejects addresses it cannot parse (quoted local parts, `a..b@x`; the reset
//! endpoint swallows the Err), and `SMTP_HOST=host:port` with `SMTP_PORT=0` is not split.

use lettre::message::{
    Mailbox,
    header::{ContentType, MIME_VERSION_1_0},
};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use std::time::Duration;

/// How a message would be sent, derived purely from the environment (testable without SMTP).
#[derive(Debug, Clone, PartialEq)]
pub struct SmtpPlan {
    pub host: String,
    pub port: u16,
    /// Implicit TLS (`SMTP_SSL` in 1/true/yes), default port 465.
    pub ssl: bool,
    /// STARTTLS required on a plain connection (default on; `SMTP_STARTTLS` 0/false/no = off).
    pub starttls: bool,
    pub user: Option<String>,
    pub password: String,
    pub from: String,
}

/// Env lookup, injectable for tests (`|k| std::env::var(k).ok()` in production).
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

fn non_empty(env: Env<'_>, key: &str) -> Option<String> {
    env(key).filter(|v| !v.is_empty())
}

pub fn configured(env: Env<'_>) -> bool {
    non_empty(env, "SMTP_HOST").is_some()
}

/// Python `int()` for a port: optional sign and surrounding spaces, ASCII digits. Port 0 means
/// "the protocol default" (smtplib: 465 with implicit TLS, 25 otherwise).
fn parse_port(raw: &str, ssl: bool) -> Result<u16, String> {
    let out_of_range = || format!("SMTP_PORT out of range: {}", crate::pyfmt::repr_str(raw));
    let t = raw.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "invalid literal for int() with base 10: {}",
            crate::pyfmt::repr_str(raw)
        ));
    }
    // ponytail: Python accepts any int and fails at connect; a port over 65535 (or negative) is
    // refused up front.
    let port = digits.parse::<u16>().map_err(|_| out_of_range())?;
    if port == 0 {
        return Ok(if ssl { 465 } else { 25 });
    }
    if t.starts_with('-') {
        return Err(out_of_range());
    }
    Ok(port)
}

/// None when `SMTP_HOST` is unset/empty. Err on an unparseable `SMTP_PORT`.
pub fn plan(env: Env<'_>) -> Result<Option<SmtpPlan>, String> {
    let Some(host) = non_empty(env, "SMTP_HOST") else {
        return Ok(None);
    };
    let ssl = matches!(
        non_empty(env, "SMTP_SSL")
            .map(|v| v.to_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes")
    );
    let port = match non_empty(env, "SMTP_PORT") {
        Some(p) => parse_port(&p, ssl)?,
        None if ssl => 465,
        None => 587,
    };
    let starttls_raw = non_empty(env, "SMTP_STARTTLS")
        .unwrap_or_else(|| "1".into())
        .to_lowercase();
    let starttls = !ssl && !matches!(starttls_raw.as_str(), "0" | "false" | "no");
    Ok(Some(SmtpPlan {
        host,
        port,
        ssl,
        starttls,
        user: non_empty(env, "SMTP_USER"),
        password: non_empty(env, "SMTP_PASSWORD").unwrap_or_default(),
        from: non_empty(env, "SMTP_FROM").unwrap_or_else(|| "printpapi@localhost".into()),
    }))
}

/// Sends mail. Injectable so the server tests capture messages instead of sending them.
pub trait Mailer: Send + Sync {
    /// Ok(true) = handed to a mail server, Ok(false) = logged to stderr instead.
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<bool, String>;
}

/// What goes to stderr when no mail server is configured.
fn unsent_log(to: &str, subject: &str, body: &str) -> String {
    format!("[mail] SMTP_HOST unset — not delivered. To: {to}\n[mail] Subject: {subject}\n{body}")
}

/// Send one plain-text message using `env` for the configuration. Ok(true) = went to a mail
/// server, Ok(false) = logged instead. The caller must not tell an unauthenticated user whether
/// the address exists (see POST /password/reset), so it swallows the Err.
pub fn send_with_env(env: Env<'_>, to: &str, subject: &str, body: &str) -> Result<bool, String> {
    let Some(plan) = plan(env)? else {
        eprintln!("{}", unsent_log(to, subject, body));
        return Ok(false);
    };
    let message = Message::builder()
        .from(plan.from.parse::<Mailbox>().map_err(|e| e.to_string())?)
        .to(to.parse::<Mailbox>().map_err(|e| e.to_string())?)
        .subject(subject)
        .header(MIME_VERSION_1_0)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|e| e.to_string())?;
    let mut builder = SmtpTransport::builder_dangerous(plan.host.as_str())
        .port(plan.port)
        .timeout(Some(Duration::from_secs(15)));
    // ponytail: like smtplib (starttls()/SMTP_SSL without a context use an unverified context),
    // the certificate is not checked — self-signed relays (Debian Postfix's snakeoil cert, a relay
    // addressed by IP or docker service name) keep working. Traffic is encrypted, not authenticated.
    let tls = || {
        TlsParameters::builder(plan.host.clone())
            .dangerous_accept_invalid_certs(true)
            .dangerous_accept_invalid_hostnames(true)
            .build()
            .map_err(|e| e.to_string())
    };
    if plan.ssl {
        builder = builder.tls(Tls::Wrapper(tls()?));
    } else if plan.starttls {
        builder = builder.tls(Tls::Required(tls()?));
    }
    if let Some(user) = &plan.user {
        builder = builder.credentials(Credentials::new(user.clone(), plan.password.clone()));
    }
    builder.build().send(&message).map_err(|e| e.to_string())?;
    Ok(true)
}

/// The production mailer: reads the environment on every send (like the Python).
pub struct SmtpMailer;

impl Mailer for SmtpMailer {
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<bool, String> {
        send_with_env(&|k| std::env::var(k).ok(), to, subject, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn without_smtp_host_the_message_is_logged_not_sent() {
        let env = env_of(&[]);
        let get = |k: &str| env.get(k).cloned();
        assert_eq!(
            send_with_env(&get, "ops@shop.test", "Reset", "link"),
            Ok(false)
        );
        assert!(!configured(&get));
        assert_eq!(plan(&get), Ok(None));
        let empty = env_of(&[("SMTP_HOST", "")]);
        assert!(!configured(&|k: &str| empty.get(k).cloned()));
    }

    #[test]
    fn a_configured_sender_starttls_logs_in_and_sends() {
        let env = env_of(&[
            ("SMTP_HOST", "smtp.test"),
            ("SMTP_USER", "u"),
            ("SMTP_PASSWORD", "p"),
            ("SMTP_FROM", "print@shop.test"),
        ]);
        let get = |k: &str| env.get(k).cloned();
        assert!(configured(&get));
        let p = plan(&get).unwrap().unwrap();
        assert_eq!(
            (p.host.as_str(), p.port, p.ssl, p.starttls),
            ("smtp.test", 587, false, true)
        );
        assert_eq!(p.user.as_deref(), Some("u"));
        assert_eq!(p.password, "p");
        assert_eq!(p.from, "print@shop.test");
    }

    #[test]
    fn implicit_tls_uses_465_and_does_not_starttls() {
        let env = env_of(&[("SMTP_HOST", "smtp.test"), ("SMTP_SSL", "1")]);
        let p = plan(&|k: &str| env.get(k).cloned()).unwrap().unwrap();
        assert_eq!((p.port, p.ssl, p.starttls), (465, true, false));
        assert_eq!(p.user, None); // no SMTP_USER — anonymous relay
        assert_eq!(p.password, "");
        assert_eq!(p.from, "printpapi@localhost");
    }

    #[test]
    fn plan_flags_and_port_parsing() {
        let plan_of = |pairs: &[(&str, &str)]| {
            let env = env_of(pairs);
            plan(&|k: &str| env.get(k).cloned())
        };
        let p = plan_of(&[
            ("SMTP_HOST", "h"),
            ("SMTP_SSL", "YES"),
            ("SMTP_PORT", "2465"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!((p.port, p.ssl), (2465, true));
        for off in ["0", "false", "No", "FALSE"] {
            let p = plan_of(&[("SMTP_HOST", "h"), ("SMTP_STARTTLS", off)])
                .unwrap()
                .unwrap();
            assert!(!p.starttls, "{off}");
        }
        let p = plan_of(&[("SMTP_HOST", "h"), ("SMTP_STARTTLS", "")])
            .unwrap()
            .unwrap();
        assert!(p.starttls); // empty falls back to the default, like `or "1"`
        let p = plan_of(&[("SMTP_HOST", "h"), ("SMTP_SSL", "0"), ("SMTP_PORT", "")])
            .unwrap()
            .unwrap();
        assert_eq!((p.port, p.ssl), (587, false));
        let err = plan_of(&[("SMTP_HOST", "h"), ("SMTP_PORT", "abc")]).unwrap_err();
        assert_eq!(err, "invalid literal for int() with base 10: 'abc'");
        assert!(plan_of(&[("SMTP_HOST", "h"), ("SMTP_PORT", "70000")]).is_err());
        let err = plan_of(&[("SMTP_HOST", "h"), ("SMTP_PORT", "-1")]).unwrap_err();
        assert_eq!(err, "SMTP_PORT out of range: '-1'");
    }

    #[test]
    fn port_zero_means_the_protocol_default() {
        let port_of = |pairs: &[(&str, &str)]| {
            let env = env_of(pairs);
            plan(&|k: &str| env.get(k).cloned()).unwrap().unwrap().port
        };
        assert_eq!(port_of(&[("SMTP_HOST", "h"), ("SMTP_PORT", "0")]), 25);
        assert_eq!(port_of(&[("SMTP_HOST", "h"), ("SMTP_PORT", "-0")]), 25);
        let ssl = [("SMTP_HOST", "h"), ("SMTP_PORT", "0"), ("SMTP_SSL", "1")];
        assert_eq!(port_of(&ssl), 465);
    }

    #[test]
    fn the_unsent_log_names_recipient_and_body() {
        let log = unsent_log("ops@shop.test", "Reset", "https://x/link");
        assert!(
            log.contains("ops@shop.test") && log.contains("link"),
            "{log}"
        );
        assert!(log.contains("Subject: Reset"), "{log}");
    }

    /// Minimal SMTP server: one connection, returns the transcript of what the client sent.
    fn fake_smtp() -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut out = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut transcript = String::new();
            out.write_all(b"220 fake ESMTP\r\n").unwrap();
            let mut in_data = false;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                transcript.push_str(&line);
                if in_data {
                    if line == ".\r\n" {
                        in_data = false;
                        out.write_all(b"250 queued\r\n").unwrap();
                    }
                    continue;
                }
                let cmd = line.trim().to_uppercase();
                let reply: &[u8] = if cmd.starts_with("EHLO") {
                    b"250-fake\r\n250 AUTH PLAIN LOGIN\r\n"
                } else if cmd.starts_with("AUTH PLAIN") {
                    b"235 ok\r\n"
                } else if cmd.starts_with("DATA") {
                    in_data = true;
                    b"354 go ahead\r\n"
                } else if cmd.starts_with("QUIT") {
                    out.write_all(b"221 bye\r\n").unwrap();
                    break;
                } else {
                    b"250 ok\r\n"
                };
                out.write_all(reply).unwrap();
            }
            transcript
        });
        (port, handle)
    }

    #[test]
    fn a_message_reaches_a_plain_smtp_server_end_to_end() {
        let (port, server) = fake_smtp();
        let port = port.to_string();
        let env = env_of(&[
            ("SMTP_HOST", "127.0.0.1"),
            ("SMTP_PORT", &port),
            ("SMTP_STARTTLS", "0"),
            ("SMTP_FROM", "print@shop.test"),
        ]);
        let sent = send_with_env(
            &|k: &str| env.get(k).cloned(),
            "ops@shop.test",
            "Reset your password",
            "https://x/reset?token=abc",
        );
        assert_eq!(sent, Ok(true));
        let transcript = server.join().unwrap();
        assert!(
            transcript.contains("MAIL FROM:<print@shop.test>"),
            "{transcript}"
        );
        assert!(
            transcript.contains("RCPT TO:<ops@shop.test>"),
            "{transcript}"
        );
        assert!(transcript.contains("From: print@shop.test"), "{transcript}");
        assert!(transcript.contains("To: ops@shop.test"), "{transcript}");
        assert!(
            transcript.contains("Subject: Reset your password"),
            "{transcript}"
        );
        assert!(
            transcript.contains("https://x/reset?token=abc"),
            "{transcript}"
        );
        assert!(
            transcript.to_lowercase().contains("text/plain"),
            "{transcript}"
        );
        assert!(transcript.contains("MIME-Version: 1.0"), "{transcript}");
    }

    #[test]
    fn smtp_user_and_password_are_sent_as_auth() {
        let (port, server) = fake_smtp();
        let port = port.to_string();
        let env = env_of(&[
            ("SMTP_HOST", "127.0.0.1"),
            ("SMTP_PORT", &port),
            ("SMTP_STARTTLS", "0"),
            ("SMTP_USER", "u"),
            ("SMTP_PASSWORD", "p"),
        ]);
        let sent = send_with_env(&|k: &str| env.get(k).cloned(), "ops@shop.test", "s", "b");
        assert_eq!(sent, Ok(true));
        let transcript = server.join().unwrap();
        // AUTH PLAIN of "\0u\0p"
        assert!(transcript.contains("AUTH PLAIN AHUAcA=="), "{transcript}");
    }

    #[test]
    fn a_refused_connection_is_an_err_not_a_panic() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port().to_string()
        };
        let env = env_of(&[
            ("SMTP_HOST", "127.0.0.1"),
            ("SMTP_PORT", &port),
            ("SMTP_STARTTLS", "0"),
        ]);
        assert!(send_with_env(&|k: &str| env.get(k).cloned(), "a@b.test", "s", "b").is_err());
        let bad_to = send_with_env(&|k: &str| env.get(k).cloned(), "not an address", "s", "b");
        assert!(bad_to.is_err());
    }
}
