// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The agent's life: register (retrying), then poll, print and report forever.

use serde_json::{Value, json};

use crate::client::{Http, HttpError, http_status};
use crate::config::Printer;
use crate::journal::{Entry, Journal, valid_uid};
use crate::print::{Fns, MAX_COPIES, Options, print_copies, safe_job_id};
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

/// The body of `POST /agent/jobs/{id}/result`: `{ok, error}`, and `job_uid` when the claim came
/// with one, so a server whose database was reset since cannot take the result for another job.
fn result_body(ok: bool, error: Option<&str>, job_uid: Option<&str>) -> Value {
    let mut body = json!({"ok": ok, "error": error});
    if let (Some(uid), Some(o)) = (job_uid, body.as_object_mut()) {
        o.insert("job_uid".into(), json!(uid));
    }
    body
}

pub fn report_result(
    http: &dyn Http,
    base: &str,
    key: &str,
    job_id: &str,
    ok: bool,
    error: Option<&str>,
    job_uid: Option<&str>,
) -> Result<(), HttpError> {
    http.post(
        &format!("{base}/agent/jobs/{job_id}/result"),
        key,
        &result_body(ok, error, job_uid),
    )
    .map(|_| ())
}

/// A lost result makes the server's reaper requeue the job -> duplicate print. Retry hard.
/// `Ok(false)` once the attempts are used up; only a non-JSON reply (not retried) is an `Err`.
/// This is the path of a job that handed over no copy, and of one that has no `job_uid` (an old
/// server, a proxy that strips `X-Printapi-Features`); a job with an identity that did hand over
/// a copy goes through the journal ([`Outbox`]).
#[allow(clippy::too_many_arguments)] // mirrors the Python signature; a struct would only move the list
pub fn report_with_retry(
    http: &dyn Http,
    base: &str,
    key: &str,
    job_id: &str,
    ok: bool,
    error: Option<&str>,
    job_uid: Option<&str>,
    attempts: u32,
    sleep: Sleep,
) -> Result<bool, HttpError> {
    for i in 0..attempts {
        match report_result(http, base, key, job_id, ok, error, job_uid) {
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
    // ponytail: after 5 tries we drop the result; the reaper requeues and the job prints again.
    // Ceiling: with no copy handed over that is the right outcome. A job without a `job_uid` may
    // have printed, and there five lost reports still mean a second print (nothing is journaled
    // without an identity).
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

/// A lost connection or a `5xx`; "payload larger than N MB" and a `4xx` will not change by asking again.
fn transient(e: &HttpError) -> bool {
    match e {
        HttpError::Os(m) => http_status(e).map_or(m.starts_with("connection failed"), |s| s >= 500),
        HttpError::Value(_) => false,
    }
}

/// The payload download. Nothing has been handed to a printer yet, so asking again cannot print
/// twice: a lost connection or a 5xx (a server restart, a proxy's 503) is tried again after the
/// pauses the report uses (1, 2, 4, 8 s) instead of ending the job as failed for good. A 4xx or a
/// reply that is not what we asked for is final.
fn fetch_payload(
    http: &dyn Http,
    url: &str,
    key: &str,
    sleep: Sleep,
) -> Result<Option<Vec<u8>>, HttpError> {
    const TRIES: u32 = 5;
    for i in 0..TRIES {
        match http.get_bytes(url, key) {
            Err(e @ HttpError::Os(_)) if i + 1 < TRIES && transient(&e) => {
                err_line(&format!(
                    "payload download failed (try {}/{TRIES}): {e}",
                    i + 1
                ));
                sleep(1u64 << i);
            }
            other => return other,
        }
    }
    unreachable!("the last try returns above")
}

/// Everything after the job is claimed and before its result is reported; any `Err` becomes the
/// failure text the server shows.
/// `sent` counts the copies that were handed over without an error, whatever happens later.
#[allow(clippy::too_many_arguments)] // the loop's environment; a struct would only move the list
fn execute(
    job: &Value,
    job_id: &str,
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
    sleep: Sleep,
    sent: &mut u32,
) -> Result<(), String> {
    let payload = fetch_payload(
        http,
        &format!("{base}/agent/jobs/{job_id}/payload"),
        key,
        sleep,
    )
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
    print_copies(
        mode,
        printer,
        &payload,
        copies,
        options,
        fns,
        Some(job_id),
        sent,
    )
}

/// A job this poll handled, for the status file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobDone {
    pub id: i64,
    pub ok: bool,
}

/// What one poll came to: `Ok(None)` = no job, `Ok(Some(_))` = a job was handled.
pub type PollOutcome = Result<Option<JobDone>, String>;

/// Results waiting to reach the server, and the journal behind them (docs/agent.md,
/// Reliability).
pub struct Outbox {
    journal: Journal,
    /// Monotonic seconds, so a wall clock that jumps blocks nothing.
    clock: Box<dyn Fn() -> f64>,
    /// Seconds to wait after a failed flush pass (0 = the last pass worked).
    pause: f64,
    not_before: f64,
}

/// Posts of late results per pass.
const FLUSH_MAX: usize = 20;
/// The first wait after a failed pass; doubled per further failure up to [PAUSE_MAX].
const PAUSE_FIRST: f64 = 30.0;
const PAUSE_MAX: f64 = 600.0;
/// Tries to deliver one result right after printing.
const DELIVER_TRIES: u32 = 5;

/// The reply of `POST /agent/results` that counts as delivered: `{"ok": true, "state": text or
/// null}`. A proxy's `{}`, an empty body or a page is none, whatever its status.
fn late_state(reply: Option<Value>) -> Result<Option<String>, String> {
    if let Some(Value::Object(o)) = &reply
        && o.get("ok") == Some(&Value::Bool(true))
    {
        match o.get("state") {
            Some(Value::String(s)) => return Ok(Some(s.clone())),
            Some(Value::Null) => return Ok(None),
            _ => {}
        }
    }
    Err("unexpected reply from /agent/results".into())
}

/// `POST /agent/results`: the result of a job by id and uid, accepted whatever the server did to
/// the job since (it was requeued, failed by the reaper). The old server answers 404.
fn post_late(http: &dyn Http, base: &str, key: &str, e: &Entry) -> Result<Option<String>, String> {
    let body = json!({"job_id": e.job_id, "job_uid": e.uid, "ok": e.ok, "error": e.error});
    http.post(&format!("{base}/agent/results"), key, &body)
        .map_err(|err| err.to_string())
        .and_then(late_state)
}

impl Outbox {
    pub fn new(journal: Journal, clock: Box<dyn Fn() -> f64>) -> Outbox {
        Outbox {
            journal,
            clock,
            pause: 0.0,
            not_before: 0.0,
        }
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// One try to deliver `e` right after printing it: the old route first with today's body (the
    /// server handed this id out moments ago, and behind every proxy that route is known), and
    /// only when it answers 404, which is all it says about a job it no longer holds as claimed,
    /// the new one in the same try. Anything else (another status, no connection, `{}`, a page) is
    /// a failed try.
    fn try_deliver(&self, http: &dyn Http, base: &str, key: &str, e: &Entry) -> Result<(), String> {
        let url = format!("{base}/agent/jobs/{}/result", e.job_id);
        match http.post(
            &url,
            key,
            &result_body(e.ok, e.error.as_deref(), Some(&e.uid)),
        ) {
            Ok(Some(Value::Object(o))) if o.get("ok") == Some(&Value::Bool(true)) => Ok(()),
            Ok(_) => Err("unexpected reply".into()),
            Err(err) if http_status(&err) == Some(404) => post_late(http, base, key, e).map(|_| ()),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Report a journaled result: five tries, 1, 2, 4, 8 s between them. Delivered = acknowledged
    /// in the journal. Not delivered: the entry stays pending and [`Outbox::flush`] sends it later.
    #[allow(clippy::too_many_arguments)] // the run loop's environment
    fn deliver(
        &mut self,
        http: &dyn Http,
        base: &str,
        key: &str,
        e: &Entry,
        sleep: Sleep,
        log: Log,
    ) -> bool {
        for i in 0..DELIVER_TRIES {
            match self.try_deliver(http, base, key, e) {
                Ok(()) => {
                    self.journal.delivered(&e.uid, log);
                    return true;
                }
                Err(err) => {
                    err_line(&format!(
                        "report_result failed (try {}/{DELIVER_TRIES}): {err}",
                        i + 1
                    ));
                    if i + 1 < DELIVER_TRIES {
                        sleep(1u64 << i);
                    }
                }
            }
        }
        log(&format!(
            "result of job {} could not be delivered; it is kept and sent later",
            e.job_id
        ));
        false
    }

    /// Send what is still pending, oldest first, before the next poll. Only the new route, one
    /// post per entry, at most [FLUSH_MAX] per pass. A server that has the route answers every
    /// well-formed call with 200, so any other reply says something about the path and not the
    /// entry: the pass ends, the entry goes to the back (one a WAF refuses must not block the
    /// others) and nothing is tried for 30 s, doubling to 10 minutes; nothing is ever discarded
    /// because of a status code.
    // ponytail: one post per entry, no batch; a batch endpoint if a site ever has hundreds pending.
    pub fn flush(&mut self, http: &dyn Http, base: &str, key: &str, log: Log) {
        let pending = self.journal.pending();
        if pending.is_empty() {
            // whatever failed was delivered another way (the job was handed out again): the next
            // outage starts with the first pause again
            self.pause = 0.0;
            self.not_before = 0.0;
            return;
        }
        if (self.clock)() < self.not_before {
            return;
        }
        for e in pending.into_iter().take(FLUSH_MAX) {
            match post_late(http, base, key, &e) {
                Ok(state) => {
                    self.journal.delivered(&e.uid, log);
                    self.pause = 0.0;
                    self.not_before = 0.0;
                    log(&format!(
                        "result of job {} delivered late ({})",
                        e.job_id,
                        state.as_deref().unwrap_or("the server has no such job")
                    ));
                }
                Err(err) => {
                    self.journal.defer(&e.uid);
                    if self.pause == 0.0 {
                        log(&format!(
                            "result of job {} could not be sent ({err}); kept, trying again later",
                            e.job_id
                        ));
                    }
                    self.pause = if self.pause == 0.0 {
                        PAUSE_FIRST
                    } else {
                        (self.pause * 2.0).min(PAUSE_MAX)
                    };
                    self.not_before = (self.clock)() + self.pause;
                    return;
                }
            }
        }
    }
}

/// The identity of a claim: its `job_uid`, only when it is a safe word and the job id is a JSON
/// integer. Without it the job takes the old path.
fn claim_identity(job: &Value) -> Option<(String, i64)> {
    let uid = job.get("job_uid")?.as_str().filter(|u| valid_uid(u))?;
    Some((uid.to_string(), job.get("job_id")?.as_i64()?))
}

/// Poll once; print and report if there was a job.
#[allow(clippy::too_many_arguments)] // the loop's whole environment; a struct would only move the list
pub fn run_once(
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
    report_sleep: Sleep,
    outbox: &mut Outbox,
    log: Log,
) -> PollOutcome {
    run_once_with(
        base,
        key,
        printers,
        http,
        fns,
        report_sleep,
        outbox,
        &|| {},
        log,
    )
}

/// [`run_once`] with `claimed`, called once right after a job with an id arrived and before it is
/// printed: the status file then says `busy`, which is what an update waits for. Not called for
/// a job that is answered from the journal: nothing is printed.
///
/// First the results that could not be delivered earlier. Then the poll; a job the journal knows
/// is not printed again, its recorded result is reported. Otherwise it is printed; when at least
/// one copy was handed over, the outcome is written to the journal before it is reported (a crash
/// or a lost report then costs no second print). Every other case (no identity, nothing printed)
/// is the old path: report, and if that fails the reaper hands the job out again.
#[allow(clippy::too_many_arguments)] // the loop's whole environment; a struct would only move the list
pub fn run_once_with(
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
    report_sleep: Sleep,
    outbox: &mut Outbox,
    claimed: &dyn Fn(),
    log: Log,
) -> PollOutcome {
    outbox.flush(http, base, key, log);
    let Some(job) = http
        .get(&format!("{base}/agent/jobs"), key)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    // a literal `null` body means the same as 204
    if job.is_null() {
        return Ok(None);
    }
    let job_id = job_id_text(job.get("job_id").unwrap_or(&Value::Null))?;
    if job.get("printer_id").is_none() {
        return Err("job reply has no printer_id".into());
    }
    let identity = claim_identity(&job);
    // ponytail: the server's ids are integers; a plain-word id (which `job_id_text` still lets
    // through) shows as 0 in the status file
    let id = job_id.parse().unwrap_or(0);
    if let Some((uid, _)) = &identity
        && let Some(entry) = outbox.journal.get(uid).cloned()
    {
        log(&format!(
            "job {job_id} was handed out again; it was already printed here, the recorded result is reported"
        ));
        outbox.journal.reopen(uid, log);
        outbox.deliver(http, base, key, &entry, report_sleep, log);
        return Ok(Some(JobDone { id, ok: entry.ok }));
    }
    claimed();
    let mut sent = 0;
    let outcome = execute(
        &job,
        &job_id,
        base,
        key,
        printers,
        http,
        fns,
        report_sleep,
        &mut sent,
    );
    let (ok, error) = match &outcome {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.as_str())),
    };
    match identity {
        Some((uid, jid)) if sent > 0 => {
            outbox.journal.record(&uid, jid, ok, error, log);
            let entry = outbox.journal.get(&uid).cloned().unwrap_or(Entry {
                uid,
                job_id: jid,
                ok,
                error: error.map(str::to_string),
                acked: false,
            });
            outbox.deliver(http, base, key, &entry, report_sleep, log);
        }
        identity => {
            let uid = identity.as_ref().map(|(u, _)| u.as_str());
            report_with_retry(http, base, key, &job_id, ok, error, uid, 5, report_sleep)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(JobDone { id, ok }))
}

/// The loop behind [`poll_forever`]: `next` returns the outcome of one poll, or `None` to stop.
/// Logs only on a failing/recovered transition, not every error - at the 2 s retry interval a
/// down server or a revoked token would otherwise write about 43k lines/day and bury the one
/// thing an operator needs from the crash log. Still prints every error to the console.
/// `observe` sees every outcome first (the status file).
pub fn poll_while(
    next: &mut dyn FnMut() -> Option<PollOutcome>,
    observe: &mut dyn FnMut(&PollOutcome),
    sleep: Sleep,
    log: Log,
) {
    let mut failing = false;
    while let Some(outcome) = next() {
        observe(&outcome);
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
#[allow(clippy::too_many_arguments)] // the loop's whole environment; a struct would only move the list
pub fn poll_forever(
    base: &str,
    key: &str,
    printers: &PrinterMap,
    http: &dyn Http,
    fns: &Fns,
    sleep: Sleep,
    log: Log,
    outbox: &mut Outbox,
    observe: &mut dyn FnMut(&PollOutcome),
    claimed: &dyn Fn(),
) {
    poll_while(
        &mut || {
            Some(run_once_with(
                base, key, printers, http, fns, sleep, outbox, claimed, log,
            ))
        },
        observe,
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
        /// Answers of the next payload downloads, before `payload` applies.
        payload_script: RefCell<VecDeque<Result<Option<Vec<u8>>, HttpError>>>,
        posts: RefCell<Vec<(String, Value)>>,
        post_script: RefCell<VecDeque<Result<Option<Value>, HttpError>>>,
        post_default: Result<Option<Value>, HttpError>,
    }

    impl FakeHttp {
        fn new(poll_result: Option<Value>, payload: &[u8]) -> FakeHttp {
            FakeHttp {
                poll_result,
                payload: Some(payload.to_vec()),
                payload_script: RefCell::new(VecDeque::new()),
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
            match self.payload_script.borrow_mut().pop_front() {
                Some(r) => r,
                None => Ok(self.payload.clone()),
            }
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

    /// An outbox over a journal in memory and a clock that only moves when a test says so.
    fn outbox() -> Outbox {
        Outbox::new(Journal::memory(), Box::new(|| 0.0))
    }

    fn silent(_: &str) {}

    /// [`run_once`] with a fresh outbox, for the tests that do not look at it.
    fn once(
        base: &str,
        key: &str,
        printers: &PrinterMap,
        http: &dyn Http,
        fns: &Fns,
        sleep: Sleep,
    ) -> PollOutcome {
        run_once(
            base,
            key,
            printers,
            http,
            fns,
            sleep,
            &mut outbox(),
            &silent,
        )
    }

    fn once_with(
        base: &str,
        key: &str,
        printers: &PrinterMap,
        http: &dyn Http,
        fns: &Fns,
        sleep: Sleep,
        claimed: &dyn Fn(),
    ) -> PollOutcome {
        run_once_with(
            base,
            key,
            printers,
            http,
            fns,
            sleep,
            &mut outbox(),
            claimed,
            &silent,
        )
    }

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
        let handled = once(
            "http://x",
            "k",
            &pm(1, entry("Zebra", false, "Zebra")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(handled, Some(JobDone { id: 7, ok: true }));
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
        let handled = once(
            "http://x",
            "k",
            &pm(1, entry("Zebra", false, "Zebra")),
            &http,
            &fns,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(handled, Some(JobDone { id: 9, ok: false }));
        let (url, body) = http.last_post();
        assert!(url.ends_with("/agent/jobs/9/result"));
        assert_eq!(body["ok"], json!(false));
        assert!(body["error"].as_str().unwrap().contains("spooler down"));
    }

    #[test]
    fn a_null_poll_body_is_no_job() {
        let http = FakeHttp::new(Some(Value::Null), b"");
        let r = once(
            "http://x",
            "k",
            &PrinterMap::default(),
            &http,
            &Fns::refusing(),
            &no_sleep,
        );
        assert_eq!(r, Ok(None));
    }

    #[test]
    fn test_run_once_no_job_returns_false() {
        let http = FakeHttp::new(None, b"");
        assert!(
            once(
                "http://x",
                "k",
                &PrinterMap::default(),
                &http,
                &Fns::refusing(),
                &no_sleep
            )
            .unwrap()
            .is_none()
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
            once(
                "http://x",
                "k",
                &pm(1, entry("Zebra", true, "Zebra")),
                &http,
                &fns,
                &no_sleep
            )
            .unwrap()
            .is_some()
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
        once(
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
        once(
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
        once(
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
        assert!(
            report_with_retry(&html, "http://x", "k", "1", true, None, None, 5, &no_sleep).is_err()
        );
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
        let ret = once(
            "http://x",
            "k",
            &pm(5, entry("p", false, "p")),
            &http,
            &fns,
            &no_sleep,
        );
        assert_eq!(ret, Ok(Some(JobDone { id: 1, ok: true })));
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
            once(
                "http://x",
                "k",
                &pm(1, entry("archive", true, &target)),
                &http,
                &fns,
                &no_sleep
            )
            .unwrap()
            .is_some()
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
            once(
                "http://x",
                "k",
                &PrinterMap::default(),
                &http,
                &Fns::refusing(),
                &no_sleep
            )
            .unwrap()
            .is_some()
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
            assert!(
                once("http://x", "k", &p, &http, &Fns::refusing(), &no_sleep)
                    .unwrap()
                    .is_some_and(|d| !d.ok)
            );
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
            assert!(once("http://x", "k", &p, &http, &Fns::refusing(), &no_sleep).is_err());
        }
        let http = FakeHttp::new(Some(json!({"job_id": 1, "mode": "raw"})), b"x");
        assert!(once("http://x", "k", &p, &http, &Fns::refusing(), &no_sleep).is_err());
    }

    #[test]
    fn an_empty_payload_fails_the_job() {
        let mut http = FakeHttp::new(
            Some(json!({"job_id": 4, "printer_id": 1, "mode": "raw"})),
            b"",
        );
        http.payload = None;
        once(
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

    #[test]
    fn a_payload_download_cut_off_by_a_restart_or_a_503_is_asked_for_again() {
        let job = json!({"job_id": 4, "printer_id": 1, "mode": "raw"});
        let http = FakeHttp::new(Some(job), b"DATA");
        *http.payload_script.borrow_mut() = VecDeque::from([
            Err(HttpError::Os(
                "connection failed: io: Peer disconnected".into(),
            )),
            Err(HttpError::Os("server returned 503".into())),
        ]);
        let waits = std::rc::Rc::new(RefCell::new(Vec::new()));
        let printed = std::rc::Rc::new(RefCell::new(0));
        let n = printed.clone();
        let mut fns = Fns::refusing();
        fns.raw = Box::new(move |_, _| {
            *n.borrow_mut() += 1;
            Ok(())
        });
        once(
            "http://x",
            "k",
            &pm(1, entry("Z", false, "Z")),
            &http,
            &fns,
            &*collect(&waits),
        )
        .unwrap();
        assert_eq!(*waits.borrow(), vec![1, 2]);
        assert_eq!(*printed.borrow(), 1, "printed once");
        assert_eq!(http.last_post().1["ok"], json!(true));
    }

    #[test]
    fn a_payload_download_that_keeps_failing_ends_the_job_and_a_4xx_is_not_retried() {
        let job = json!({"job_id": 4, "printer_id": 1, "mode": "raw"});
        let p = pm(1, entry("Z", false, "Z"));
        let http = FakeHttp::new(Some(job.clone()), b"DATA");
        *http.payload_script.borrow_mut() = (0..9)
            .map(|_| Err(HttpError::Os("server returned 503".into())))
            .collect();
        let waits = std::rc::Rc::new(RefCell::new(Vec::new()));
        once(
            "http://x",
            "k",
            &p,
            &http,
            &Fns::refusing(),
            &*collect(&waits),
        )
        .unwrap();
        assert_eq!(*waits.borrow(), vec![1, 2, 4, 8], "five tries, four pauses");
        let (_, body) = http.last_post();
        assert_eq!(body["ok"], json!(false));
        assert!(body["error"].as_str().unwrap().contains("503"), "{body}");
        // a 404 or 403 and a payload over the cap are final
        for text in ["server returned 404", "payload larger than 128 MB"] {
            let http = FakeHttp::new(Some(job.clone()), b"DATA");
            *http.payload_script.borrow_mut() = VecDeque::from([Err(HttpError::Os(text.into()))]);
            let waits = std::rc::Rc::new(RefCell::new(Vec::new()));
            once(
                "http://x",
                "k",
                &p,
                &http,
                &Fns::refusing(),
                &*collect(&waits),
            )
            .unwrap();
            assert!(waits.borrow().is_empty(), "{text}");
            assert_eq!(http.last_post().1["ok"], json!(false));
        }
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
                3 => Some(Ok(None)), // recovered
                _ => None,           // end the otherwise-infinite loop
            }
        };
        let (logged, slept) = (
            std::rc::Rc::new(RefCell::new(Vec::<String>::new())),
            std::rc::Rc::new(RefCell::new(Vec::<u64>::new())),
        );
        let l = logged.clone();
        let mut observed = Vec::new();
        poll_while(
            &mut next,
            &mut |o| observed.push(o.clone()),
            &*collect(&slept),
            &move |m| l.borrow_mut().push(m.to_string()),
        );
        // the status file sees every outcome, not only the transitions
        assert_eq!(
            observed,
            vec![Err("down".to_string()), Err("down".to_string()), Ok(None)]
        );
        assert_eq!(*logged.borrow(), vec!["poll error: down", "poll recovered"]);
        assert_eq!(*slept.borrow(), vec![2, 2]); // only the two failing calls sleep
    }

    #[test]
    fn the_claim_hook_fires_once_per_job_before_printing_and_not_for_an_empty_poll() {
        let order = std::rc::Rc::new(RefCell::new(Vec::<&str>::new()));
        let (o1, o2) = (order.clone(), order.clone());
        let http = FakeHttp::new(
            Some(json!({"job_id": 7, "printer_id": 1, "mode": "raw"})),
            b"Z",
        );
        let fns = Fns {
            raw: Box::new(move |_, _| {
                o1.borrow_mut().push("print");
                Ok(())
            }),
            ..Fns::refusing()
        };
        let p = pm(1, entry("Zebra", false, "Zebra"));
        let hook = || o2.borrow_mut().push("claimed");
        let r = once_with("http://x", "k", &p, &http, &fns, &no_sleep, &hook);
        assert_eq!(r, Ok(Some(JobDone { id: 7, ok: true })));
        assert_eq!(*order.borrow(), vec!["claimed", "print"]);
        // an empty poll: the hook stays quiet
        order.borrow_mut().clear();
        let empty = FakeHttp::new(None, b"");
        assert_eq!(
            once_with("http://x", "k", &p, &empty, &fns, &no_sleep, &hook),
            Ok(None)
        );
        assert!(order.borrow().is_empty());
        // run_once is the same call without a hook
        assert_eq!(
            once("http://x", "k", &p, &http, &fns, &no_sleep),
            Ok(Some(JobDone { id: 7, ok: true }))
        );
    }

    // --- the journal: results that outlive a lost report --------------------------------------

    type Reply = Result<Option<Value>, HttpError>;
    type Answer = Box<dyn FnMut(&str, &Value) -> Reply>;

    /// A scripted server: the claim it hands out, the payload, and how it answers each post.
    struct Net {
        claim: RefCell<Option<Value>>,
        /// Hand the claim out on every poll (the reaper handing the same job out again).
        keep: std::cell::Cell<bool>,
        payload: RefCell<Result<(), HttpError>>,
        payload_gets: std::cell::Cell<u32>,
        posts: RefCell<Vec<(String, Value)>>,
        reply: RefCell<Answer>,
    }

    fn healthy(url: &str, _: &Value) -> Reply {
        Ok(Some(if url.ends_with("/agent/results") {
            json!({"ok": true, "state": "done"})
        } else {
            json!({"ok": true})
        }))
    }

    impl Net {
        fn new(claim: Option<Value>) -> Net {
            Net {
                claim: RefCell::new(claim),
                keep: std::cell::Cell::new(false),
                payload: RefCell::new(Ok(())),
                payload_gets: std::cell::Cell::new(0),
                posts: RefCell::new(Vec::new()),
                reply: RefCell::new(Box::new(healthy)),
            }
        }
        fn answer(&self, f: impl FnMut(&str, &Value) -> Reply + 'static) {
            *self.reply.borrow_mut() = Box::new(f);
        }
        fn urls(&self) -> Vec<String> {
            self.posts
                .borrow()
                .iter()
                .map(|(u, _)| u.trim_start_matches("http://x").to_string())
                .collect()
        }
    }

    impl Http for Net {
        fn get(&self, _: &str, _: &str) -> Reply {
            if self.keep.get() {
                Ok(self.claim.borrow().clone())
            } else {
                Ok(self.claim.borrow_mut().take())
            }
        }
        fn get_bytes(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, HttpError> {
            self.payload_gets.set(self.payload_gets.get() + 1);
            match &*self.payload.borrow() {
                Ok(()) => Ok(Some(b"DATA".to_vec())),
                Err(e) => Err(e.clone()),
            }
        }
        fn post(&self, url: &str, _: &str, body: &Value) -> Reply {
            self.posts
                .borrow_mut()
                .push((url.to_string(), body.clone()));
            (self.reply.borrow_mut())(url, body)
        }
    }

    const UID: &str = "9f0123456789abcdef0123456789abcd";

    fn claim_of(id: i64, uid: &str, copies: i64) -> Value {
        json!({"job_id": id, "printer_id": 1, "mode": "raw", "copies": copies, "job_uid": uid})
    }

    /// A printer that counts the copies it was handed and fails from copy `fail_from` on (0 = never).
    fn printer(fail_from: u32) -> (Fns, std::rc::Rc<std::cell::Cell<u32>>) {
        let n = std::rc::Rc::new(std::cell::Cell::new(0));
        let c = n.clone();
        let fns = Fns {
            raw: Box::new(move |_, _| {
                c.set(c.get() + 1);
                if fail_from != 0 && c.get() >= fail_from {
                    Err("jam".into())
                } else {
                    Ok(())
                }
            }),
            ..Fns::refusing()
        };
        (fns, n)
    }

    fn zebra() -> PrinterMap {
        pm(1, entry("Z", false, "Z"))
    }

    fn file_outbox(dir: &std::path::Path, clock: std::rc::Rc<std::cell::Cell<f64>>) -> Outbox {
        let journal = Journal::open(dir.join(crate::journal::FILE_NAME), "", &silent);
        Outbox::new(journal, Box::new(move || clock.get()))
    }

    fn at_zero() -> std::rc::Rc<std::cell::Cell<f64>> {
        std::rc::Rc::new(std::cell::Cell::new(0.0))
    }

    fn step(net: &Net, fns: &Fns, ob: &mut Outbox) -> PollOutcome {
        run_once("http://x", "k", &zebra(), net, fns, &no_sleep, ob, &silent)
    }

    fn refuse(status: u16) -> Reply {
        Err(HttpError::Os(format!("server returned {status}")))
    }

    #[test]
    fn a_claim_without_job_uid_is_never_journaled_and_posts_only_to_the_old_route() {
        let net = Net::new(Some(json!({"job_id": 7, "printer_id": 1, "mode": "raw"})));
        let (fns, n) = printer(0);
        let dir = tempfile::tempdir().unwrap();
        let mut ob = file_outbox(dir.path(), at_zero());
        assert_eq!(
            step(&net, &fns, &mut ob),
            Ok(Some(JobDone { id: 7, ok: true }))
        );
        assert_eq!(n.get(), 1);
        assert_eq!(net.urls(), ["/agent/jobs/7/result"]);
        assert_eq!(net.posts.borrow()[0].1, json!({"ok": true, "error": null}));
        assert!(!dir.path().join(crate::journal::FILE_NAME).exists());
        // a bad uid or a job id that is no integer: the same
        for claim in [
            json!({"job_id": 7, "printer_id": 1, "mode": "raw", "job_uid": "bad uid"}),
            json!({"job_id": 7, "printer_id": 1, "mode": "raw", "job_uid": 5}),
            json!({"job_id": "7", "printer_id": 1, "mode": "raw", "job_uid": UID}),
        ] {
            let net = Net::new(Some(claim));
            step(&net, &fns, &mut ob).unwrap();
            assert_eq!(net.urls(), ["/agent/jobs/7/result"]);
            assert!(!dir.path().join(crate::journal::FILE_NAME).exists());
        }
    }

    #[test]
    fn the_result_is_in_the_journal_file_before_the_first_post() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::journal::FILE_NAME);
        let seen = std::rc::Rc::new(RefCell::new(Vec::<String>::new()));
        let net = Net::new(Some(claim_of(7, UID, 1)));
        let (s, p) = (seen.clone(), path.clone());
        net.answer(move |_, _| {
            s.borrow_mut()
                .push(std::fs::read_to_string(&p).unwrap_or_default());
            Ok(Some(json!({"ok": true})))
        });
        let (fns, _) = printer(0);
        step(&net, &fns, &mut file_outbox(dir.path(), at_zero())).unwrap();
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].contains(UID) && seen[0].contains("\"acked\":false"),
            "{}",
            seen[0]
        );
    }

    #[test]
    fn the_normal_report_is_one_post_with_todays_body_and_the_job_uid() {
        let net = Net::new(Some(claim_of(7, UID, 1)));
        let (fns, _) = printer(0);
        let mut ob = outbox();
        step(&net, &fns, &mut ob).unwrap();
        assert_eq!(net.urls(), ["/agent/jobs/7/result"]);
        assert_eq!(
            net.posts.borrow()[0].1,
            json!({"ok": true, "error": null, "job_uid": UID})
        );
        assert!(ob.journal().get(UID).unwrap().acked);
    }

    #[test]
    fn only_a_404_on_the_old_route_goes_on_to_the_new_one_in_the_same_try() {
        for (answer, goes_on) in [
            (refuse(404), true),
            (refuse(403), false),
            (refuse(405), false),
            (refuse(503), false),
            (
                Err(HttpError::Os("connection failed: refused".into())),
                false,
            ),
            (Ok(Some(json!({}))), false),
            (Ok(None), false),
            (Err(HttpError::Value("<html>login</html>".into())), false),
        ] {
            let net = Net::new(Some(claim_of(7, UID, 1)));
            let a = answer.clone();
            net.answer(move |url, b| {
                if url.ends_with("/agent/results") {
                    assert_eq!(
                        b,
                        &json!({"job_id": 7, "job_uid": UID, "ok": true, "error": null})
                    );
                    Ok(Some(json!({"ok": true, "state": "done"})))
                } else {
                    a.clone()
                }
            });
            let (fns, n) = printer(0);
            let mut ob = outbox();
            assert_eq!(
                step(&net, &fns, &mut ob),
                Ok(Some(JobDone { id: 7, ok: true }))
            );
            assert_eq!(n.get(), 1);
            let urls = net.urls();
            if goes_on {
                // in the same try: old route, new route, and it is delivered
                assert_eq!(
                    urls,
                    ["/agent/jobs/7/result", "/agent/results"],
                    "{answer:?}"
                );
                assert!(ob.journal().get(UID).unwrap().acked);
            } else {
                assert!(
                    urls.iter().all(|u| u == "/agent/jobs/7/result"),
                    "{answer:?}: {urls:?}"
                );
                assert_eq!(urls.len(), 5, "{answer:?}");
                assert!(!ob.journal().get(UID).unwrap().acked, "{answer:?}");
            }
        }
    }

    #[test]
    fn a_reply_to_the_new_route_that_is_no_acknowledgement_is_none() {
        for reply in [
            Ok(Some(json!({}))),
            Ok(Some(json!({"ok": true}))),
            Ok(Some(json!({"ok": true, "state": 5}))),
            Ok(Some(json!({"ok": false, "state": "done"}))),
            Ok(None),
            refuse(400),
            Err(HttpError::Value("page".into())),
        ] {
            let net = Net::new(Some(claim_of(7, UID, 1)));
            let r = reply.clone();
            net.answer(move |url, _| {
                if url.ends_with("/agent/results") {
                    r.clone()
                } else {
                    refuse(404)
                }
            });
            let (fns, _) = printer(0);
            let mut ob = outbox();
            step(&net, &fns, &mut ob).unwrap();
            assert!(!ob.journal().get(UID).unwrap().acked, "{reply:?}");
        }
        // state null is an answer: the server has no such job, and that is final
        let net = Net::new(Some(claim_of(7, UID, 1)));
        net.answer(|url, _| {
            if url.ends_with("/agent/results") {
                Ok(Some(json!({"ok": true, "state": null})))
            } else {
                refuse(404)
            }
        });
        let (fns, _) = printer(0);
        let mut ob = outbox();
        step(&net, &fns, &mut ob).unwrap();
        assert!(ob.journal().get(UID).unwrap().acked);
    }

    #[test]
    fn five_failed_tries_leave_the_entry_pending_and_the_next_run_delivers_it_through_the_new_route()
     {
        let net = Net::new(Some(claim_of(7, UID, 1)));
        net.answer(|_, _| refuse(503));
        let (fns, n) = printer(0);
        let dir = tempfile::tempdir().unwrap();
        let slept = std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut ob = file_outbox(dir.path(), at_zero());
        let r = run_once(
            "http://x",
            "k",
            &zebra(),
            &net,
            &fns,
            &*collect(&slept),
            &mut ob,
            &silent,
        );
        assert_eq!(r, Ok(Some(JobDone { id: 7, ok: true })));
        assert_eq!(net.posts.borrow().len(), 5);
        assert_eq!(*slept.borrow(), vec![1, 2, 4, 8]);
        assert_eq!(ob.journal().pending().len(), 1);
        // the server is back; nothing new to claim
        net.answer(healthy);
        assert_eq!(step(&net, &fns, &mut ob), Ok(None));
        assert_eq!(n.get(), 1, "printed once");
        assert_eq!(net.urls().last().unwrap(), "/agent/results");
        assert!(ob.journal().pending().is_empty());
        // and it is still delivered after the agent restarted
        let ob = file_outbox(dir.path(), at_zero());
        assert!(ob.journal().pending().is_empty());
        assert!(ob.journal().get(UID).unwrap().acked);
    }

    #[test]
    fn a_job_handed_out_again_is_answered_from_the_journal_without_download_or_print() {
        for (fail_from, copies) in [(0, 1), (2, 3)] {
            let net = Net::new(Some(claim_of(7, UID, copies)));
            net.answer(|_, _| refuse(503)); // the first report is lost
            let (fns, n) = printer(fail_from);
            let dir = tempfile::tempdir().unwrap();
            let mut ob = file_outbox(dir.path(), at_zero());
            step(&net, &fns, &mut ob).unwrap();
            let printed = n.get();
            assert_eq!(printed, if fail_from == 0 { 1 } else { 2 });
            let gets = net.payload_gets.get();
            // the reaper hands the same job out again; the flush path is blocked, the old route works
            net.answer(|url, b| {
                if url.ends_with("/agent/results") {
                    refuse(403)
                } else {
                    healthy(url, b)
                }
            });
            *net.claim.borrow_mut() = Some(claim_of(7, UID, copies));
            let claimed = std::cell::Cell::new(0);
            let r = run_once_with(
                "http://x",
                "k",
                &zebra(),
                &net,
                &fns,
                &no_sleep,
                &mut ob,
                &|| claimed.set(claimed.get() + 1),
                &silent,
            );
            assert_eq!(
                r,
                Ok(Some(JobDone {
                    id: 7,
                    ok: fail_from == 0
                }))
            );
            assert_eq!(n.get(), printed, "nothing printed");
            assert_eq!(net.payload_gets.get(), gets, "nothing downloaded");
            assert_eq!(
                claimed.get(),
                0,
                "no claim hook for a job that is not printed"
            );
            let last = net.posts.borrow().last().unwrap().clone();
            assert_eq!(last.0, "http://x/agent/jobs/7/result");
            assert_eq!(last.1["ok"], json!(fail_from == 0));
            if fail_from != 0 {
                assert_eq!(last.1["error"], json!("jam"));
            }
        }
    }

    #[test]
    fn an_acked_ok_handed_out_again_is_pending_again_so_a_lost_report_is_flushed() {
        let net = Net::new(Some(claim_of(7, UID, 1)));
        let (fns, n) = printer(0);
        let dir = tempfile::tempdir().unwrap();
        let mut ob = file_outbox(dir.path(), at_zero());
        step(&net, &fns, &mut ob).unwrap();
        assert!(ob.journal().get(UID).unwrap().acked);
        // handed out again (a restored backup), and now every post fails
        net.answer(|_, _| refuse(503));
        *net.claim.borrow_mut() = Some(claim_of(7, UID, 1));
        step(&net, &fns, &mut ob).unwrap();
        assert_eq!(n.get(), 1);
        assert_eq!(ob.journal().pending().len(), 1);
        assert!(
            !file_outbox(dir.path(), at_zero())
                .journal()
                .get(UID)
                .unwrap()
                .acked
        );
        // the flush of the next run delivers it
        net.answer(healthy);
        step(&net, &fns, &mut ob).unwrap();
        assert!(ob.journal().pending().is_empty());
        assert_eq!(net.urls().last().unwrap(), "/agent/results");
    }

    #[test]
    fn an_acked_failure_handed_out_again_prints() {
        let net = Net::new(Some(claim_of(7, UID, 3)));
        let (fns, n) = printer(2);
        let mut ob = outbox();
        step(&net, &fns, &mut ob).unwrap();
        assert_eq!(n.get(), 2);
        assert!(
            ob.journal().get(UID).is_none(),
            "the server has the failure: forgotten"
        );
        *net.claim.borrow_mut() = Some(claim_of(7, UID, 3));
        let (fns2, n2) = printer(0);
        step(&net, &fns2, &mut ob).unwrap();
        assert_eq!(n2.get(), 3);
    }

    #[test]
    fn the_same_job_id_with_another_uid_is_another_job_and_prints() {
        let net = Net::new(Some(claim_of(7, UID, 1)));
        let (fns, n) = printer(0);
        let mut ob = outbox();
        step(&net, &fns, &mut ob).unwrap();
        *net.claim.borrow_mut() = Some(claim_of(7, "another-database", 1));
        step(&net, &fns, &mut ob).unwrap();
        assert_eq!(n.get(), 2);
        assert!(ob.journal().get("another-database").is_some());
    }

    #[test]
    fn what_printed_nothing_is_not_journaled_and_prints_on_the_next_hand_out() {
        // payload download fails
        let net = Net::new(Some(claim_of(7, UID, 1)));
        *net.payload.borrow_mut() = Err(HttpError::Os("server returned 502".into()));
        let (fns, n) = printer(0);
        let mut ob = outbox();
        assert_eq!(
            step(&net, &fns, &mut ob),
            Ok(Some(JobDone { id: 7, ok: false }))
        );
        assert_eq!(
            (n.get(), net.urls()),
            (0, vec!["/agent/jobs/7/result".to_string()])
        );
        assert!(ob.journal().get(UID).is_none());
        // the old route carries the uid, too, so a reset server cannot take it for another job
        assert_eq!(net.posts.borrow()[0].1["job_uid"], json!(UID));
        // an unknown printer
        let net2 = Net::new(Some(
            json!({"job_id": 7, "printer_id": 99, "mode": "raw", "job_uid": UID}),
        ));
        step(&net2, &fns, &mut ob).unwrap();
        assert!(ob.journal().get(UID).is_none());
        // zero copies send nothing and take the old path
        let net3 = Net::new(Some(claim_of(7, UID, 0)));
        step(&net3, &fns, &mut ob).unwrap();
        assert_eq!(
            (n.get(), net3.urls()),
            (0, vec!["/agent/jobs/7/result".to_string()])
        );
        assert!(ob.journal().get(UID).is_none());
        // a first copy that failed
        let (failing, n2) = printer(1);
        let net4 = Net::new(Some(claim_of(7, UID, 2)));
        step(&net4, &failing, &mut ob).unwrap();
        assert_eq!(n2.get(), 1);
        assert_eq!(net4.urls(), ["/agent/jobs/7/result"]);
        assert!(ob.journal().get(UID).is_none());
        // and when it is handed out again it prints
        let net5 = Net::new(Some(claim_of(7, UID, 2)));
        step(&net5, &fns, &mut ob).unwrap();
        assert_eq!(n.get(), 2);
    }

    #[test]
    fn a_later_copy_that_fails_is_journaled_and_not_printed_again_when_the_report_was_lost() {
        let net = Net::new(Some(claim_of(7, UID, 3)));
        net.answer(|_, _| refuse(503));
        let (fns, n) = printer(2);
        let dir = tempfile::tempdir().unwrap();
        step(&net, &fns, &mut file_outbox(dir.path(), at_zero())).unwrap();
        assert_eq!(n.get(), 2);
        // the agent restarts while the server is still unreachable, the reaper hands the job out again
        let net = Net::new(Some(claim_of(7, UID, 3)));
        net.answer(|_, _| refuse(503));
        let mut ob = file_outbox(dir.path(), at_zero());
        let r = step(&net, &fns, &mut ob);
        assert_eq!(r, Ok(Some(JobDone { id: 7, ok: false })));
        assert_eq!(n.get(), 2, "not printed again");
        assert_eq!(net.posts.borrow()[0].1["error"], json!("jam"));
    }

    #[test]
    fn a_crash_while_printing_leaves_no_record_and_the_whole_job_prints_again() {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new(Some(claim_of(7, UID, 3)));
        let died = Fns {
            raw: Box::new(|_, _| panic!("power cut")),
            ..Fns::refusing()
        };
        let hit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = step(&net, &died, &mut file_outbox(dir.path(), at_zero()));
        }));
        assert!(hit.is_err());
        let net = Net::new(Some(claim_of(7, UID, 3)));
        let (fns, n) = printer(0);
        step(&net, &fns, &mut file_outbox(dir.path(), at_zero())).unwrap();
        assert_eq!(n.get(), 3);
    }

    // --- the flush ----------------------------------------------------------------------------

    fn pending_outbox(clock: &std::rc::Rc<std::cell::Cell<f64>>, jobs: &[i64]) -> Outbox {
        let c = clock.clone();
        let mut ob = Outbox::new(Journal::memory(), Box::new(move || c.get()));
        for id in jobs {
            ob.journal
                .record(&format!("uid{id}"), *id, true, None, &silent);
        }
        ob
    }

    fn flush_with(net: &Net, ob: &mut Outbox, log: Log) {
        ob.flush(net, "http://x", "k", log);
    }

    /// A key that has a right to take its device over but has not yet (pairing, Reconnect) is
    /// refused on `/agent/results`. The poll that makes it the device's key must still go out, or
    /// a machine with a journal entry would never take over.
    #[test]
    fn a_refused_flush_does_not_stop_the_poll_that_takes_a_device_over() {
        let net = Net::new(Some(claim_of(9, UID, 1)));
        net.answer(|url, _| {
            if url.ends_with("/agent/results") {
                refuse(401)
            } else {
                healthy(url, &Value::Null)
            }
        });
        let clock = at_zero();
        let mut ob = pending_outbox(&clock, &[5]);
        let (fns, printed) = printer(0);
        let got = step(&net, &fns, &mut ob);
        assert!(got.is_ok());
        assert_eq!(
            printed.get(),
            1,
            "the poll was made and the job it returned was printed"
        );
        assert!(
            net.urls().iter().any(|u| u.ends_with("/agent/results")),
            "the flush was tried first"
        );
        assert_eq!(
            ob.journal()
                .pending()
                .iter()
                .filter(|e| e.job_id == 5)
                .count(),
            1,
            "the old entry waits"
        );
    }

    #[test]
    fn flush_never_uses_the_old_route_and_delivers_oldest_first() {
        let net = Net::new(None);
        let clock = at_zero();
        let mut ob = pending_outbox(&clock, &[5, 6, 7]);
        flush_with(&net, &mut ob, &silent);
        assert_eq!(net.urls(), ["/agent/results"; 3]);
        let ids: Vec<i64> = net
            .posts
            .borrow()
            .iter()
            .map(|(_, b)| b["job_id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [5, 6, 7]);
        assert!(ob.journal().pending().is_empty());
    }

    #[test]
    fn flush_stops_at_the_first_non_acknowledgement_and_backs_off_30_60_to_600_seconds() {
        let net = Net::new(None);
        net.answer(|_, _| refuse(403));
        let clock = at_zero();
        let mut ob = pending_outbox(&clock, &[5, 6]);
        let logged = std::rc::Rc::new(RefCell::new(Vec::<String>::new()));
        let l = logged.clone();
        let log = move |m: &str| l.borrow_mut().push(m.to_string());
        flush_with(&net, &mut ob, &log);
        assert_eq!(net.posts.borrow().len(), 1, "the pass ends");
        // that entry went to the back
        assert_eq!(ob.journal().pending()[0].job_id, 6);
        // nothing is tried before the pause is over
        clock.set(29.0);
        flush_with(&net, &mut ob, &log);
        assert_eq!(net.posts.borrow().len(), 1);
        clock.set(30.0);
        flush_with(&net, &mut ob, &log);
        assert_eq!(net.posts.borrow().len(), 2);
        // the series, with the pause read off the clock directly
        let mut ob = pending_outbox(&clock, &[9]);
        let mut pauses = Vec::new();
        clock.set(1000.0);
        for _ in 0..7 {
            flush_with(&net, &mut ob, &silent);
            pauses.push(ob.not_before - clock.get());
            clock.set(ob.not_before);
        }
        assert_eq!(pauses, [30.0, 60.0, 120.0, 240.0, 480.0, 600.0, 600.0]);
        // logged once for the series, not once per pass
        assert_eq!(logged.borrow().len(), 1, "{:?}", logged.borrow());
        // a success resets it
        net.answer(healthy);
        flush_with(&net, &mut ob, &silent);
        assert_eq!((ob.pause, ob.not_before), (0.0, 0.0));
        assert!(ob.journal().pending().is_empty());
    }

    #[test]
    fn a_result_delivered_another_way_ends_the_back_off_of_the_flush() {
        let net = Net::new(None);
        net.answer(|_, _| refuse(403));
        let clock = at_zero();
        let mut ob = pending_outbox(&clock, &[7]);
        flush_with(&net, &mut ob, &silent);
        assert_eq!(ob.pause, 30.0, "an outage: the first pause is 30 s");
        flush_with(&net, &mut ob, &silent);
        clock.set(ob.not_before);
        flush_with(&net, &mut ob, &silent);
        assert_eq!(ob.pause, 60.0);
        // the server is back and hands job 7 out again: answered over the old route
        let healthy_net = Net::new(None);
        let e = ob.journal().get("uid7").cloned().unwrap();
        assert!(ob.deliver(&healthy_net, "http://x", "k", &e, &no_sleep, &silent));
        // the next poll's flush finds nothing pending
        flush_with(&healthy_net, &mut ob, &silent);
        assert_eq!((ob.pause, ob.not_before), (0.0, 0.0));
        // a day later another report is lost and its flush fails
        clock.set(100_000.0);
        ob.journal.record("uid8", 8, true, None, &silent);
        let logged = std::rc::Rc::new(RefCell::new(Vec::<String>::new()));
        let l = logged.clone();
        flush_with(&net, &mut ob, &move |m| l.borrow_mut().push(m.to_string()));
        assert_eq!(ob.not_before - clock.get(), 30.0);
        assert_eq!(logged.borrow().len(), 1, "{:?}", logged.borrow());
    }

    #[test]
    fn flush_posts_at_most_twenty_per_pass() {
        let net = Net::new(None);
        let clock = at_zero();
        let ids: Vec<i64> = (1..=30).collect();
        let mut ob = pending_outbox(&clock, &ids);
        flush_with(&net, &mut ob, &silent);
        assert_eq!(net.posts.borrow().len(), 20);
        assert_eq!(ob.journal().pending().len(), 10);
        flush_with(&net, &mut ob, &silent);
        assert!(ob.journal().pending().is_empty());
    }

    #[test]
    fn flush_logs_a_late_delivery() {
        let net = Net::new(None);
        let mut ob = pending_outbox(&at_zero(), &[7]);
        let logged = std::rc::Rc::new(RefCell::new(Vec::<String>::new()));
        let l = logged.clone();
        flush_with(&net, &mut ob, &move |m| l.borrow_mut().push(m.to_string()));
        assert_eq!(*logged.borrow(), ["result of job 7 delivered late (done)"]);
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
