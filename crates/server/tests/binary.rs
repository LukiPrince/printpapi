// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Smoke test of the built binary: environment handling and a real request over TCP.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_printpapi"));
    for k in [
        "PRINTAPI_TOKEN",
        "PRINT_DB",
        "PRINT_PORT",
        "PRINTAPI_PLANS",
        "PRINTAPI_SIGNUP",
        "PRINTAPI_TRUSTED_PROXIES",
        "PRINTAPI_SIGNUP_QUOTA",
        "PRINTAPI_FETCHES_PER_ORG",
        "PRINTAPI_JOB_RETENTION_DAYS",
        "PRINTAPI_TERMS_VERSION",
        "PRINTAPI_TERMS_URL",
        "PRINTAPI_DPA_URL",
        "PRINTAPI_PRIVACY_URL",
        "LOG_REQUESTS",
    ] {
        c.env_remove(k);
    }
    c
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn missing_token_exits_1() {
    let out = bin().output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_TOKEN is required"));
}

#[test]
fn blank_token_exits_1() {
    let out = bin().env("PRINTAPI_TOKEN", "  ").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_TOKEN must not be empty"));
}

#[test]
fn bad_port_exits_1() {
    let out = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINT_PORT", "abc")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn bad_plans_exit_1() {
    let out = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINTAPI_PLANS", "[]")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("non-empty JSON array"));
}

#[test]
fn bad_trusted_proxies_exit_1() {
    let out = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINTAPI_TRUSTED_PROXIES", "127.0.0.1,172.16.0.0/40")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_TRUSTED_PROXIES"));
}

#[test]
fn bad_signup_quota_exits_1() {
    for bad in ["-1", "ten", "1.5"] {
        let out = bin()
            .env("PRINTAPI_TOKEN", "t")
            .env("PRINTAPI_SIGNUP_QUOTA", bad)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_SIGNUP_QUOTA"));
    }
}

#[test]
fn bad_fetches_per_org_exits_1() {
    for bad in ["-1", "ten", "1.5"] {
        let out = bin()
            .env("PRINTAPI_TOKEN", "t")
            .env("PRINTAPI_FETCHES_PER_ORG", bad)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_FETCHES_PER_ORG"));
    }
}

#[test]
fn bad_job_retention_exits_1() {
    for bad in ["-1", "ten", "1.5"] {
        let out = bin()
            .env("PRINTAPI_TOKEN", "t")
            .env("PRINTAPI_JOB_RETENTION_DAYS", bad)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_JOB_RETENTION_DAYS"));
    }
}

/// Starts the server with `envs`, waits for the listening line and returns what it wrote to stderr
/// until then.
fn stderr_at_startup(envs: &[(&str, &str)]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = bin();
    cmd.env("PRINTAPI_TOKEN", "t")
        .env("PRINT_DB", dir.path().join("p.db"))
        .env("PRINT_PORT", free_port().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    child.kill().unwrap();
    let _ = child.wait();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    err
}

#[test]
fn open_signup_without_a_quota_warns_on_stderr() {
    let err = stderr_at_startup(&[("PRINTAPI_SIGNUP", "open")]);
    assert!(err.contains("PRINTAPI_SIGNUP_QUOTA"), "{err:?}");
    let err = stderr_at_startup(&[("PRINTAPI_SIGNUP", "open"), ("PRINTAPI_SIGNUP_QUOTA", "50")]);
    assert!(!err.contains("PRINTAPI_SIGNUP_QUOTA"), "{err:?}");
    // a closed door needs no quota
    assert!(!stderr_at_startup(&[]).contains("PRINTAPI_SIGNUP_QUOTA"));
}

#[test]
fn version_flag_prints_the_version() {
    let out = bin().arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("printpapi {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn serves_health_over_tcp() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut child = bin()
        .env("PRINTAPI_TOKEN", "t")
        .env("PRINT_DB", dir.path().join("p.db"))
        .env("PRINT_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let result = std::panic::catch_unwind(|| {
        assert!(line.contains(&format!("listening on :{port}")), "{line:?}");
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            s,
            "GET /health HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    });
    child.kill().unwrap();
    let _ = child.wait();
    result.unwrap();
}

#[test]
fn open_signup_without_retention_warns_on_stderr() {
    let err = stderr_at_startup(&[("PRINTAPI_SIGNUP", "open")]);
    assert!(err.contains("PRINTAPI_JOB_RETENTION_DAYS"), "{err:?}");
    let err = stderr_at_startup(&[
        ("PRINTAPI_SIGNUP", "open"),
        ("PRINTAPI_JOB_RETENTION_DAYS", "30"),
    ]);
    assert!(!err.contains("PRINTAPI_JOB_RETENTION_DAYS"), "{err:?}");
    // 0 means "keep for ever", which is the unset case: still warned
    let err = stderr_at_startup(&[
        ("PRINTAPI_SIGNUP", "open"),
        ("PRINTAPI_JOB_RETENTION_DAYS", "0"),
    ]);
    assert!(err.contains("PRINTAPI_JOB_RETENTION_DAYS"), "{err:?}");
    assert!(!stderr_at_startup(&[]).contains("PRINTAPI_JOB_RETENTION_DAYS"));
}

#[test]
fn bad_terms_settings_exit_1() {
    let cases: [&[(&str, &str)]; 6] = [
        &[("PRINTAPI_TERMS_URL", "https:example.com/terms")], // bad link, no version
        &[
            ("PRINTAPI_TERMS_VERSION", "v1"),
            ("PRINTAPI_TERMS_URL", "https://"),
        ],
        &[("PRINTAPI_TERMS_VERSION", "v1")], // no terms URL
        &[
            ("PRINTAPI_TERMS_VERSION", "v1"),
            ("PRINTAPI_TERMS_URL", "ftp://x/t"),
        ],
        &[
            ("PRINTAPI_TERMS_VERSION", "v1"),
            ("PRINTAPI_TERMS_URL", "https://x/t"),
            ("PRINTAPI_DPA_URL", "javascript:alert(1)"),
        ],
        &[
            ("PRINTAPI_TERMS_VERSION", "v1"),
            ("PRINTAPI_TERMS_URL", "https://x/t"),
            ("PRINTAPI_PRIVACY_URL", "/privacy"),
        ],
    ];
    for envs in cases {
        let mut cmd = bin();
        cmd.env("PRINTAPI_TOKEN", "t");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{envs:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("PRINTAPI_"),
            "{envs:?}"
        );
    }
}

#[test]
fn open_signup_without_terms_warns_on_stderr() {
    let err = stderr_at_startup(&[("PRINTAPI_SIGNUP", "open")]);
    assert!(err.contains("PRINTAPI_TERMS_VERSION"), "{err:?}");
    let err = stderr_at_startup(&[
        ("PRINTAPI_SIGNUP", "open"),
        ("PRINTAPI_TERMS_VERSION", "v1"),
        ("PRINTAPI_TERMS_URL", "https://x.test/terms"),
    ]);
    assert!(!err.contains("PRINTAPI_TERMS_VERSION"), "{err:?}");
    assert!(!stderr_at_startup(&[]).contains("PRINTAPI_TERMS_VERSION"));
}

#[test]
fn locks_without_the_operator_token_are_reported_at_startup() {
    use printpapi_server::store::{self, Db};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locks.db");
    {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        db.with(|c| {
            let s = store::signup(c, "Acme", "boss@acme.test", "PWHASH", None, None).unwrap();
            let created: f64 = c
                .query_row("SELECT created_at FROM orgs WHERE id=?", [s.org_id], |r| {
                    r.get(0)
                })
                .unwrap();
            c.execute(
                "INSERT INTO org_locks(org_id, org_created_at, reason, locked_at) VALUES(?,?,?,?)",
                rusqlite::params![s.org_id, created, "support ticket", 1.0],
            )
            .unwrap();
        });
    }
    let db = path.to_str().unwrap();
    let err = stderr_at_startup(&[("PRINT_DB", db)]);
    assert!(
        err.contains("1 org locks are not enforced: PRINTAPI_OPERATOR_TOKEN is unset"),
        "{err:?}"
    );
    // with the token the locks hold: no line
    let err = stderr_at_startup(&[
        ("PRINT_DB", db),
        (
            "PRINTAPI_OPERATOR_TOKEN",
            "operator-token-0123456789abcdef-xyz",
        ),
        ("PRINTAPI_OPERATOR_FROM", "10.9.0.2"),
    ]);
    assert!(!err.contains("not enforced"), "{err:?}");
    // and a database without locks says nothing
    assert!(!stderr_at_startup(&[]).contains("not enforced"));
}
