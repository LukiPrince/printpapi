// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The agent's life: register (retrying), then poll, print and report forever.

use serde_json::{Value, json};

use crate::client::{Http, HttpError};
use crate::config::Printer;
use crate::print::{Fns, MAX_COPIES, Options, print_job, safe_job_id};
use crate::pyrepr::{py_str, repr};
use crate::{err_line, out_line};

/// Seconds to sleep; injectable so tests do not wait.
pub type Sleep<'a> = &'a dyn Fn(u64);
pub type Log<'a> = &'a dyn Fn(&str);

/// The registered printers by server-side id, in the order the server listed them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrinterMap(pub Vec<(i64, Printer)>);

impl PrinterMap {
    /// `printer_by_id.get(job["printer_id"])`: JSON numbers only, integral floats included.
    pub fn get(&self, id: &Value) -> Option<&Printer> {
        let n = id.as_i64().or_else(|| {
            id.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9e15)
                .map(|f| f as i64)
        })?;
        self.0.iter().find(|(k, _)| *k == n).map(|(_, p)| p)
    }

    /// `{1: {'name': ..}, ..}` the way Python prints the dict (the startup line).
    pub fn repr(&self) -> String {
        let items: Vec<String> = self
            .0
            .iter()
            .map(|(id, p)| format!("{id}: {}", repr(&p.to_json())))
            .collect();
        format!("{{{}}}", items.join(", "))
    }
}

/// `{name: id}` from the register reply -> id -> the entry we registered under that name.
pub fn build_printer_map(printers: &[Printer], reg: &Value) -> Result<PrinterMap, String> {
    let mut map: Vec<(i64, Printer)> = Vec::new();
    let ids = reg
        .get("printer_ids")
        .and_then(Value::as_object)
        .ok_or("register reply has no printer_ids")?;
    for (pname, pid) in ids {
        let entry = printers
            .iter()
            .rev()
            .find(|p| &p.name == pname)
            .ok_or_else(|| {
                format!(
                    "server returned a printer we did not register: {}",
                    crate::pyrepr::repr_str(pname)
                )
            })?;
        let id = pid
            .as_i64()
            .ok_or_else(|| format!("server returned a non-integer printer id: {}", repr(pid)))?;
        match map.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = entry.clone(),
            None => map.push((id, entry.clone())),
        }
    }
    Ok(PrinterMap(map))
}

pub fn register(
    http: &dyn Http,
    base: &str,
    key: &str,
    name: &str,
    printers: &[Printer],
) -> Result<Option<Value>, HttpError> {
    let body = json!({"name": name, "printers": printers.iter().map(Printer::to_json).collect::<Vec<_>>()});
    http.post(&format!("{base}/agent/register"), key, &body)
}

/// `register()` until it works. At boot the network (or an auth proxy) is often not there yet;
/// a one-shot register killed the process, and a logon-triggered task never restarted it.
/// Retries on anything - a status/connection error and a non-JSON reply (a proxy's HTML page)
/// alike; a permanent misconfiguration shows up in the log, once per try. Also retries a reply
/// that parsed but is not what the caller needs (e.g. an empty `{}` from a proxy that ate the
/// body) instead of crashing after only one attempt.
#[allow(clippy::too_many_arguments)] // mirrors the Python signature; a struct would only move the list
pub fn register_with_retry(
    http: &dyn Http,
    base: &str,
    key: &str,
    name: &str,
    printers: &[Printer],
    sleep: Sleep,
    log: Log,
    max_wait: u64,
) -> Value {
    let mut attempt: u32 = 0;
    loop {
        let outcome = register(http, base, key, name, printers)
            .map_err(|e| e.to_string())
            .and_then(|reg| {
                let reg = reg.unwrap_or(Value::Null);
                let ok = reg.get("printer_ids").is_some_and(Value::is_object)
                    && reg.get("computer_id").is_some();
                if ok {
                    Ok(reg)
                } else {
                    let shown: String = repr(&reg).chars().take(200).collect();
                    Err(format!("unexpected register reply: {shown}"))
                }
            });
        let e = match outcome {
            Ok(reg) => return reg,
            Err(e) => e,
        };
        let wait = 1u64.checked_shl(attempt).unwrap_or(u64::MAX).min(max_wait);
        let msg = format!(
            "register failed (try {}), retry in {wait}s: {e}",
            attempt + 1
        );
        // stderr too, not just the log file: an interactive hand test would otherwise sit
        // silently waiting with nothing on the console.
        err_line(&msg);
        log(&msg);
        sleep(wait);
        attempt = attempt.saturating_add(1);
    }
}

pub fn report_result(
    http: &dyn Http,
    base: &str,
    key: &str,
    job_id: &str,
    ok: bool,
    error: Option<&str>,
) -> Result<(), HttpError> {
    http.post(
        &format!("{base}/agent/jobs/{job_id}/result"),
        key,
        &json!({"ok": ok, "error": error}),
    )
    .map(|_| ())
}

/// A lost result makes the server's reaper requeue the job -> duplicate print. Retry hard.
/// `Ok(false)` once the attempts are used up; only a non-JSON reply (not retried) is an `Err`.
#[allow(clippy::too_many_arguments)] // mirrors the Python signature; a struct would only move the list
pub fn report_with_retry(
    http: &dyn Http,
    base: &str,
    key: &str,
    job_id: &str,
    ok: bool,
    error: Option<&str>,
    attempts: u32,
    sleep: Sleep,
) -> Result<bool, HttpError> {
    for i in 0..attempts {
        match report_result(http, base, key, job_id, ok, error) {
            Ok(()) => return Ok(true),
            Err(e @ HttpError::Value(_)) => return Err(e),
            Err(e) => {
                err_line(&format!(
                    "report_result failed (try {}/{attempts}): {e}",
                    i + 1
                ));
                if i + 1 < attempts {
                    sleep((1u64 << i.min(6)).min(30));
                }
            }
        }
    }
    // ponytail: after 5 tries we drop the result; the reaper requeues -> possible duplicate print
    Ok(false)
}

/// A job id usable in a URL and a file name: a JSON integer, or a plain `[A-Za-z0-9_-]` word.
fn job_id_text(v: &Value) -> Result<String, String> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(n.to_string()),
        Value::String(s) if !s.is_empty() && safe_job_id(s) == *s => Ok(s.clone()),
        other => Err(format!("bad job_id in job reply: {}", repr(other))),
    }
}

/// Everything after the job is claimed and before its result is reported; any `Err` becomes the
/// failure text the server shows.
fn execute(
    job: &Value,
    job_id: &str,
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
) -> Result<(), String> {
    let payload = http
        .get_bytes(&format!("{base}/agent/jobs/{job_id}/payload"), key)
        .map_err(|e| e.to_string())?
        .ok_or("job payload is empty (server returned 204)")?;
    let printer_id = &job["printer_id"];
    let printer = printers
        .get(printer_id)
        .ok_or_else(|| format!("unknown printer id: {}", py_str(printer_id)))?;
    let mode = match job.get("mode") {
        None => return Err("'mode'".into()),
        Some(Value::String(s)) => s.as_str(),
        Some(other) => return Err(format!("bad mode: {}", py_str(other))),
    };
    let copies = match job.get("copies") {
        None => 1,
        Some(v) => v.as_i64().ok_or_else(|| {
            format!(
                "copies must be an integer 1..{MAX_COPIES}, got {}",
                py_str(v)
            )
        })?,
    };
    let options: Option<&Options> = match job.get("options") {
        None | Some(Value::Null) => None,
        Some(Value::Object(o)) => Some(o),
        Some(other) => return Err(format!("options must be an object, got {}", py_str(other))),
    };
    print_job(mode, printer, &payload, copies, options, fns, Some(job_id))
}

/// Poll once; print and report if there was a job. `Ok(false)` = no job.
pub fn run_once(
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
    report_sleep: Sleep,
) -> Result<bool, String> {
    let Some(job) = http
        .get(&format!("{base}/agent/jobs"), key)
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    // a literal `null` body means the same as 204
    if job.is_null() {
        return Ok(false);
    }
    let job_id = job_id_text(job.get("job_id").unwrap_or(&Value::Null))?;
    if job.get("printer_id").is_none() {
        return Err("job reply has no printer_id".into());
    }
    let outcome = execute(&job, &job_id, base, key, printers, http, fns);
    let (ok, error) = match &outcome {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.as_str())),
    };
    report_with_retry(http, base, key, &job_id, ok, error, 5, report_sleep)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// The loop behind [`poll_forever`]: `next` returns the outcome of one poll, or `None` to stop.
/// Logs only on a failing/recovered transition, not every error - at the 2 s retry interval a
/// down server or a revoked token would otherwise write about 43k lines/day and bury the one
/// thing an operator needs from the crash log. Still prints every error to the console.
pub fn poll_while(next: &mut dyn FnMut() -> Option<Result<bool, String>>, sleep: Sleep, log: Log) {
    let mut failing = false;
    while let Some(outcome) = next() {
        match outcome {
            Err(e) => {
                if !failing {
                    log(&format!("poll error: {e}"));
                    failing = true;
                }
                out_line(&format!("poll error: {e}"));
                sleep(2);
            }
            Ok(_) => {
                if failing {
                    log("poll recovered");
                    failing = false;
                }
            }
        }
    }
}

/// The main loop: poll, print, repeat. Never returns.
pub fn poll_forever(
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
    sleep: Sleep,
    log: Log,
) {
    poll_while(
        &mut || Some(run_once(base, key, printers, http, fns, sleep)),
        sleep,
        log,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::tests::{entry, file_uri, obj};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Records posts; serves a scripted poll result + payload, or per-call post behaviour.
    struct FakeHttp {
        poll_result: Option<Value>,
        payload: Option<Vec<u8>>,
        posts: RefCell<Vec<(String, Value)>>,
        post_script: RefCell<VecDeque<Result<Option<Value>, HttpError>>>,
        post_default: Result<Option<Value>, HttpError>,
    }

    impl FakeHttp {
        fn new(poll_result: Option<Value>, payload: &[u8]) -> FakeHttp {
            FakeHttp {
                poll_result,
                payload: Some(payload.to_vec()),
                posts: RefCell::new(Vec::new()),
                post_script: RefCell::new(VecDeque::new()),
                post_default: Ok(Some(json!({"ok": true}))),
            }
        }

        fn scripted(replies: Vec<Result<Option<Value>, HttpError>>) -> FakeHttp {
            let f = FakeHttp::new(None, b"");
            *f.post_script.borrow_mut() = replies.into();
            f
        }

        fn last_post(&self) -> (String, Value) {
            self.posts.borrow().last().cloned().unwrap()
        }
    }

    impl Http for FakeHttp {
        fn get(&self, _: &str, _: &str) -> Result<Option<Value>, HttpError> {
            Ok(self.poll_result.clone())
        }
        fn get_bytes(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, HttpError> {
            Ok(self.payload.clone())
        }
        fn post(&self, url: &str, _: &str, body: &Value) -> Result<Option<Value>, HttpError> {
            self.posts
                .borrow_mut()
                .push((url.to_string(), body.clone()));
            self.post_script
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| self.post_default.clone())
        }
    }

    fn pm(id: i64, p: Printer) -> PrinterMap {
        PrinterMap(vec![(id, p)])
    }

    fn no_sleep(_: u64) {}

    fn collect<T: 'static>(store: &std::rc::Rc<RefCell<Vec<T>>>) -> Box<dyn Fn(T)> {
        let s = store.clone();
        Box::new(move |v| s.borrow_mut().push(v))
    }

    #[test]
    fn test_run_once_prints_and_reports_success() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 7, "printer_id": 1, "mode": "raw"})),
            b"ZPLDATA",
        );
        let printed = std::rc::Rc::new(RefCell::new(Vec::new()));
        let p = printed.clone();
        let fns = Fns {
            raw: Box::new(move |t, d| {
                p.borrow_mut().push((t.to_string(), d.to_vec()));
                Ok(())
            }),
            ..Fns::refusing()
        };
        let handled = run_once(
            "http://x",
            "k",
            &pm(1, entry("Zebra", false, "Zebra")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert!(handled);
        assert_eq!(
            *printed.borrow(),
            vec![("Zebra".to_string(), b"ZPLDATA".to_vec())]
        );
        let (url, body) = http.last_post();
        assert!(url.ends_with("/agent/jobs/7/result"));
        assert_eq!(body, json!({"ok": true, "error": null}));
    }

    #[test]
    fn test_run_once_reports_failure_on_print_error() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 9, "printer_id": 1, "mode": "raw"})),
            b"x",
        );
        let fns = Fns {
            raw: Box::new(|_, _| Err("spooler down".into())),
            ..Fns::refusing()
        };
        let handled = run_once(
            "http://x",
            "k",
            &pm(1, entry("Zebra", false, "Zebra")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert!(handled);
        let (url, body) = http.last_post();
        assert!(url.ends_with("/agent/jobs/9/result"));
        assert_eq!(body["ok"], json!(false));
        assert!(body["error"].as_str().unwrap().contains("spooler down"));
    }

    #[test]
    fn a_null_poll_body_is_no_job() {
        let http = FakeHttp::new(Some(Value::Null), b"");
        let r = run_once(
            "http://x",
            "k",
            &PrinterMap::default(),
            &http,
            &Fns::refusing(),
            &no_sleep,
        );
        assert_eq!(r, Ok(false));
    }

    #[test]
    fn test_run_once_no_job_returns_false() {
        let http = FakeHttp::new(None, b"");
        assert!(
            !run_once(
                "http://x",
                "k",
                &PrinterMap::default(),
                &http,
                &Fns::refusing(),
                &no_sleep
            )
            .unwrap()
        );
        assert!(http.posts.borrow().is_empty());
    }

    #[test]
    fn test_run_once_prints_pdf_and_reports_success() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 5, "printer_id": 1, "mode": "pdf"})),
            b"%PDF",
        );
        let printed = std::rc::Rc::new(RefCell::new(Vec::new()));
        let fns = Fns {
            pdf: {
                let p = printed.clone();
                Box::new(move |t, d, _| {
                    p.borrow_mut().push((t.to_string(), d.to_vec()));
                    Ok(())
                })
            },
            ..Fns::refusing()
        };
        assert!(
            run_once(
                "http://x",
                "k",
                &pm(1, entry("Zebra", true, "Zebra")),
                &http,
                &fns,
                &no_sleep
            )
            .unwrap()
        );
        assert_eq!(
            *printed.borrow(),
            vec![("Zebra".to_string(), b"%PDF".to_vec())]
        );
        let (url, body) = http.last_post();
        assert!(url.ends_with("/agent/jobs/5/result"));
        assert_eq!(body, json!({"ok": true, "error": null}));
    }

    #[test]
    fn test_run_once_applies_job_copies() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 7, "printer_id": 1, "mode": "raw", "copies": 3})),
            b"D",
        );
        let sends = std::rc::Rc::new(RefCell::new(Vec::new()));
        let fns = Fns {
            raw: {
                let s = sends.clone();
                Box::new(move |_, d| {
                    s.borrow_mut().push(d.to_vec());
                    Ok(())
                })
            },
            ..Fns::refusing()
        };
        run_once(
            "http://x",
            "k",
            &pm(1, entry("Z", false, "Z")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(*sends.borrow(), vec![b"D".to_vec(); 3]);
    }

    #[test]
    fn test_run_once_defaults_copies_to_one_for_old_server() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 8, "printer_id": 1, "mode": "raw"})),
            b"D",
        );
        let sends = std::rc::Rc::new(RefCell::new(Vec::new()));
        let fns = Fns {
            raw: {
                let s = sends.clone();
                Box::new(move |_, d| {
                    s.borrow_mut().push(d.to_vec());
                    Ok(())
                })
            },
            ..Fns::refusing()
        };
        run_once(
            "http://x",
            "k",
            &pm(1, entry("Z", false, "Z")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(*sends.borrow(), vec![b"D".to_vec()]);
    }

    #[test]
    fn test_run_once_passes_job_options_to_pdf() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 3, "printer_id": 1, "mode": "pdf", "options": {"paper": "A4"}})),
            b"%PDF",
        );
        let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
        let fns = Fns {
            pdf: {
                let s = seen.clone();
                Box::new(move |_, _, o| {
                    s.borrow_mut().push(o.cloned());
                    Ok(())
                })
            },
            ..Fns::refusing()
        };
        run_once(
            "http://x",
            "k",
            &pm(1, entry("HP", true, "HP")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(*seen.borrow(), vec![Some(obj(json!({"paper": "A4"})))]);
        assert_eq!(http.last_post().1, json!({"ok": true, "error": null}));
    }

    #[test]
    fn test_report_retries_then_succeeds() {
        let http = FakeHttp::scripted(vec![
            Err(HttpError::Os("net down".into())),
            Err(HttpError::Os("net down".into())),
            Ok(None),
        ]);
        let waits = std::rc::Rc::new(RefCell::new(Vec::new()));
        let ok = report_with_retry(
            &http,
            "http://x",
            "k",
            "1",
            true,
            None,
            5,
            &*collect(&waits),
        )
        .unwrap();
        assert!(ok);
        assert_eq!(http.posts.borrow().len(), 3);
        assert_eq!(*waits.borrow(), vec![1, 2]);
    }

    #[test]
    fn report_gives_up_after_the_attempts_and_a_non_json_reply_propagates() {
        let mut dead = FakeHttp::new(None, b"");
        dead.post_default = Err(HttpError::Os("net down".into()));
        let waits = std::rc::Rc::new(RefCell::new(Vec::new()));
        assert!(
            !report_with_retry(
                &dead,
                "http://x",
                "k",
                "1",
                true,
                None,
                5,
                &*collect(&waits)
            )
            .unwrap()
        );
        assert_eq!(dead.posts.borrow().len(), 5);
        assert_eq!(*waits.borrow(), vec![1, 2, 4, 8]); // no sleep after the last try

        let mut html = FakeHttp::new(None, b"");
        html.post_default = Err(HttpError::Value("Expecting value".into()));
        assert!(report_with_retry(&html, "http://x", "k", "1", true, None, 5, &no_sleep).is_err());
        assert_eq!(html.posts.borrow().len(), 1);
    }

    #[test]
    fn test_run_once_survives_report_failure() {
        let mut http = FakeHttp::new(
            Some(json!({"job_id": 1, "printer_id": 5, "mode": "raw"})),
            b"DATA",
        );
        http.post_default = Err(HttpError::Os("net down".into()));
        let printed = std::rc::Rc::new(RefCell::new(Vec::new()));
        let fns = Fns {
            raw: {
                let p = printed.clone();
                Box::new(move |_, d| {
                    p.borrow_mut().push(d.to_vec());
                    Ok(())
                })
            },
            ..Fns::refusing()
        };
        let ret = run_once(
            "http://x",
            "k",
            &pm(5, entry("p", false, "p")),
            &http,
            &fns,
            &no_sleep,
        );
        assert_eq!(ret, Ok(true));
        assert_eq!(*printed.borrow(), vec![b"DATA".to_vec()]); // printed, and no error escaped
    }

    #[test]
    fn test_run_once_writes_a_file_job_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let http = FakeHttp::new(
            Some(json!({"job_id": 11, "printer_id": 1, "mode": "pdf"})),
            b"%PDF-1.7",
        );
        let target = file_uri(&dir.path().join("out"));
        let fns = Fns::native(Box::new(|_, _| Ok(())), Box::new(|_, _, _| Ok(())));
        assert!(
            run_once(
                "http://x",
                "k",
                &pm(1, entry("archive", true, &target)),
                &http,
                &fns,
                &no_sleep
            )
            .unwrap()
        );
        assert_eq!(
            std::fs::read(dir.path().join("out").join("job-11.pdf")).unwrap(),
            b"%PDF-1.7"
        );
        let (url, body) = http.last_post();
        assert!(url.ends_with("/agent/jobs/11/result"));
        assert_eq!(body, json!({"ok": true, "error": null}));
    }

    #[test]
    fn an_unknown_printer_id_is_reported_as_a_failed_job() {
        let http = FakeHttp::new(
            Some(json!({"job_id": 2, "printer_id": 99, "mode": "raw"})),
            b"x",
        );
        assert!(
            run_once(
                "http://x",
                "k",
                &PrinterMap::default(),
                &http,
                &Fns::refusing(),
                &no_sleep
            )
            .unwrap()
        );
        assert_eq!(
            http.last_post().1,
            json!({"ok": false, "error": "unknown printer id: 99"})
        );
    }

    #[test]
    fn garbage_job_fields_become_clean_failures_not_panics() {
        let p = pm(1, entry("Z", false, "Z"));
        let cases = [
            (json!({"job_id": 1, "printer_id": 1}), "'mode'"),
            (
                json!({"job_id": 1, "printer_id": 1, "mode": 5}),
                "bad mode: 5",
            ),
            (
                json!({"job_id": 1, "printer_id": 1, "mode": "raw", "copies": "x"}),
                "copies must be an integer",
            ),
            (
                json!({"job_id": 1, "printer_id": 1, "mode": "raw", "options": "x"}),
                "options must be an object",
            ),
        ];
        for (job, want) in cases {
            let http = FakeHttp::new(Some(job), b"x");
            assert!(run_once("http://x", "k", &p, &http, &Fns::refusing(), &no_sleep).unwrap());
            let (_, body) = http.last_post();
            assert_eq!(body["ok"], json!(false));
            assert!(body["error"].as_str().unwrap().contains(want), "{body}");
        }
        // no usable job id: nothing to report against, the poll loop logs it
        for job in [
            json!({"printer_id": 1, "mode": "raw"}),
            json!({"job_id": "../x", "printer_id": 1}),
        ] {
            let http = FakeHttp::new(Some(job), b"x");
            assert!(run_once("http://x", "k", &p, &http, &Fns::refusing(), &no_sleep).is_err());
        }
        let http = FakeHttp::new(Some(json!({"job_id": 1, "mode": "raw"})), b"x");
        assert!(run_once("http://x", "k", &p, &http, &Fns::refusing(), &no_sleep).is_err());
    }

    #[test]
    fn an_empty_payload_fails_the_job() {
        let mut http = FakeHttp::new(
            Some(json!({"job_id": 4, "printer_id": 1, "mode": "raw"})),
            b"",
        );
        http.payload = None;
        run_once(
            "http://x",
            "k",
            &pm(1, entry("Z", false, "Z")),
            &http,
            &Fns::refusing(),
            &no_sleep,
        )
        .unwrap();
        assert_eq!(http.last_post().1["ok"], json!(false));
    }

    // --- register with retry (unattended remote machines) ---

    fn reg_ok(id: i64) -> Result<Option<Value>, HttpError> {
        Ok(Some(json!({"computer_id": id, "printer_ids": {"p": 1}})))
    }

    fn retry(
        http: &FakeHttp,
        waits: &std::rc::Rc<RefCell<Vec<u64>>>,
        logged: &std::rc::Rc<RefCell<Vec<String>>>,
        max_wait: u64,
    ) -> Value {
        let l = logged.clone();
        register_with_retry(
            http,
            "http://x",
            "k",
            "pc",
            &[],
            &*collect(waits),
            &move |m| l.borrow_mut().push(m.to_string()),
            max_wait,
        )
    }

    #[test]
    fn test_register_with_retry_prints_to_stderr_for_interactive_runs() {
        let http = FakeHttp::scripted(vec![
            Err(HttpError::Os(
                "connection failed: [Errno 11001] getaddrinfo failed".into(),
            )),
            reg_ok(1),
        ]);
        let (waits, logged) = (Default::default(), Default::default());
        crate::ERR_LINES.with(|l| l.borrow_mut().clear());
        retry(&http, &waits, &logged, 300);
        let lines = crate::ERR_LINES.with(|l| l.borrow().clone());
        assert!(
            lines.iter().any(|l| l.contains("register failed")),
            "{lines:?}"
        );
    }

    #[test]
    fn test_register_with_retry_retries_until_success() {
        let down = || {
            Err(HttpError::Os(
                "connection failed: [Errno 11001] getaddrinfo failed".into(),
            ))
        };
        let http = FakeHttp::scripted(vec![down(), down(), reg_ok(7)]);
        let (waits, logged) = (Default::default(), Default::default());
        let reg = retry(&http, &waits, &logged, 300);
        assert_eq!(reg["computer_id"], json!(7));
        let posts = http.posts.borrow();
        assert_eq!(
            posts.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
            vec!["http://x/agent/register"; 3]
        );
        assert_eq!(*waits.borrow(), vec![1, 2]);
        let logged = logged.borrow();
        assert_eq!(logged.len(), 2);
        assert!(logged[0].contains("register failed"));
        assert_eq!(
            logged[0],
            "register failed (try 1), retry in 1s: connection failed: [Errno 11001] getaddrinfo failed"
        );
        assert_eq!(posts[0].1, json!({"name": "pc", "printers": []}));
    }

    #[test]
    fn test_register_with_retry_caps_the_wait() {
        let mut replies: Vec<_> = (0..12).map(|_| Err(HttpError::Os("down".into()))).collect();
        replies.push(Ok(Some(json!({"computer_id": 1, "printer_ids": {}}))));
        let http = FakeHttp::scripted(replies);
        let (waits, logged) = (Default::default(), Default::default());
        retry(&http, &waits, &logged, 300);
        let waits = waits.borrow();
        assert_eq!(waits[..3], [1, 2, 4]);
        assert_eq!(*waits.iter().max().unwrap(), 300);
        assert_eq!(waits.len(), 12);
    }

    #[test]
    fn test_register_with_retry_survives_a_non_json_proxy_page() {
        let http = FakeHttp::scripted(vec![
            Err(HttpError::Value(
                "Expecting value: line 1 column 1 (char 0)".into(),
            )),
            Ok(Some(json!({"computer_id": 2, "printer_ids": {}}))),
        ]);
        let (waits, logged) = (Default::default(), Default::default());
        assert_eq!(retry(&http, &waits, &logged, 300)["computer_id"], json!(2));
    }

    #[test]
    fn test_register_with_retry_retries_a_reply_missing_expected_fields() {
        let http = FakeHttp::scripted(vec![Ok(Some(json!({}))), reg_ok(3)]);
        let (waits, logged) = (Default::default(), Default::default());
        let reg = retry(&http, &waits, &logged, 300);
        assert_eq!(reg, json!({"computer_id": 3, "printer_ids": {"p": 1}}));
        assert_eq!(http.posts.borrow().len(), 2);
        assert!(logged.borrow()[0].ends_with("unexpected register reply: {}"));
    }

    #[test]
    fn test_register_with_retry_retries_a_reply_with_wrong_printer_ids_type() {
        let http = FakeHttp::scripted(vec![
            Ok(Some(json!({"computer_id": 1, "printer_ids": null}))),
            reg_ok(1),
        ]);
        let (waits, logged) = (Default::default(), Default::default());
        let reg = retry(&http, &waits, &logged, 300);
        assert_eq!(reg, json!({"computer_id": 1, "printer_ids": {"p": 1}}));
        assert_eq!(http.posts.borrow().len(), 2);
        // a 204 (no body at all) is retried too
        let http = FakeHttp::scripted(vec![Ok(None), reg_ok(1)]);
        retry(&http, &waits, &logged, 300);
        assert!(
            logged
                .borrow()
                .iter()
                .any(|m| m.ends_with("unexpected register reply: None"))
        );
    }

    #[test]
    fn an_unexpected_register_reply_is_truncated_to_200_chars_in_the_message() {
        let big = "x".repeat(500);
        let http = FakeHttp::scripted(vec![Ok(Some(json!({"junk": big}))), reg_ok(1)]);
        let (waits, logged) = (Default::default(), Default::default());
        retry(&http, &waits, &logged, 300);
        let m = logged.borrow()[0].clone();
        let shown = m.split("unexpected register reply: ").nth(1).unwrap();
        assert_eq!(shown.chars().count(), 200);
        assert!(shown.starts_with("{'junk': 'xxx"));
    }

    // --- poll loop: log only on a failing/recovered transition, not every 2s retry ---

    #[test]
    fn test_poll_forever_logs_only_on_failing_and_recovered_transitions() {
        let mut calls = 0;
        let mut next = || {
            calls += 1;
            match calls {
                1 | 2 => Some(Err("down".to_string())),
                3 => Some(Ok(false)), // recovered
                _ => None,            // end the otherwise-infinite loop
            }
        };
        let (logged, slept) = (
            std::rc::Rc::new(RefCell::new(Vec::<String>::new())),
            std::rc::Rc::new(RefCell::new(Vec::<u64>::new())),
        );
        let l = logged.clone();
        poll_while(&mut next, &*collect(&slept), &move |m| {
            l.borrow_mut().push(m.to_string())
        });
        assert_eq!(*logged.borrow(), vec!["poll error: down", "poll recovered"]);
        assert_eq!(*slept.borrow(), vec![2, 2]); // only the two failing calls sleep
    }

    // --- start-up: printers by id ---

    #[test]
    fn build_printer_map_binds_a_duplicate_name_to_the_last_entry() {
        // Python's entry_by_name is a last-wins dict, and so is the server's register UPDATE loop.
        let printers = vec![
            entry("Zebra", false, "Zebra"),
            entry("Zebra", false, "socket://10.0.0.5:9100"),
        ];
        let map = build_printer_map(&printers, &json!({"printer_ids": {"Zebra": 1}})).unwrap();
        assert_eq!(map.0[0].1.target, "socket://10.0.0.5:9100");
    }

    #[test]
    fn build_printer_map_follows_the_server_order_and_rejects_strangers() {
        let printers = vec![entry("a", false, "a"), entry("b", true, "b")];
        let map = build_printer_map(&printers, &json!({"printer_ids": {"b": 2, "a": 1}})).unwrap();
        assert_eq!(
            map.0.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            map.repr(),
            "{2: {'name': 'b', 'can_pdf': True, 'target': 'b'}, 1: {'name': 'a', 'can_pdf': False, 'target': 'a'}}"
        );
        assert!(build_printer_map(&printers, &json!({"printer_ids": {"zzz": 1}})).is_err());
        assert!(build_printer_map(&printers, &json!({"printer_ids": {"a": "one"}})).is_err());
        assert_eq!(map.get(&json!(2.0)).map(|p| p.name.as_str()), Some("b"));
        assert!(map.get(&json!("2")).is_none());
    }
}
