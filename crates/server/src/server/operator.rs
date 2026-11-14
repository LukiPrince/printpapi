// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The operator API under `/operator/`: one service-level credential (`PRINTAPI_OPERATOR_TOKEN`)
//! for the console of a hosted deployment. Inert without the token: every `/operator/` path is a
//! 404. The token is never a `Principal`: only `operator_gate` reads it, and on every other route
//! `without_operator_credential` takes it out of the request before any lookup runs.
use super::*;
use crate::settings;
use std::net::IpAddr;
use std::time::Duration;

/// How long `GET /operator/overview` is served from memory.
/// ponytail: one cached value for every caller; a console polling every few seconds must not turn
/// into a count over `jobs` per poll.
const OVERVIEW_TTL: Duration = Duration::from_secs(10);

/// Newest first, at most this many audit rows per page.
const AUDIT_MAX_LIMIT: i64 = 200;
const AUDIT_DEFAULT_LIMIT: i64 = 50;

/// The same paging bounds for the org and job lists.
const LIST_MAX_LIMIT: i64 = 200;
const LIST_DEFAULT_LIMIT: i64 = 50;

/// The longest reason the audit log takes: it is kept for ever.
const REASON_MAX: usize = 500;
/// The shortest reason a lock or a support grant needs.
const LOCK_REASON_MIN: usize = 8;
/// The lengths a support grant may have, in minutes.
const SUPPORT_MINUTES: [i64; 3] = [15, 30, 60];
/// The answer when a support read has no active grant behind it.
const NO_SUPPORT_ACCESS: &str = "no active support access";
/// The longest org name, in characters.
const NAME_MAX: usize = 200;

/// `/operator/orgs/{id}` with its suffix (`""`, `"jobs"`, `"lock"`, `"unlock"` or `"support"`),
/// the id all digits.
fn org_route(path: &str) -> Option<(i64, &str)> {
    let rest = path.strip_prefix("/operator/orgs/")?;
    let (id, suffix) = match rest.split_once('/') {
        Some((id, s @ ("jobs" | "lock" | "unlock" | "support"))) => (id, s),
        Some(_) => return None,
        None => (rest, ""),
    };
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((id.parse().ok()?, suffix))
}

/// The optional `reason` of a write body: absent, null or blank is none; anything but a string,
/// or a string over `REASON_MAX` characters, is a 400. `min` characters make it required.
fn reason_of(body: &Obj, min: usize) -> Gate<Option<String>> {
    let reason = match body.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(printnode::py_trim(s).to_string()).filter(|s| !s.is_empty()),
        Some(_) => return Err(jerr(400, "reason must be a string")),
    };
    if reason
        .as_ref()
        .is_some_and(|r| r.chars().count() > REASON_MAX)
    {
        return Err(jerr(400, "reason is too long"));
    }
    if min > 0 && reason.as_ref().is_none_or(|r| r.chars().count() < min) {
        return Err(jerr(
            400,
            format!("reason of at least {min} characters required"),
        ));
    }
    Ok(reason)
}

/// A required, trimmed, non-empty string field of at most `NAME_MAX` characters.
fn name_of(v: Option<&Value>, what: &str) -> Gate<String> {
    match v {
        Some(Value::String(s))
            if !printnode::py_trim(s).is_empty() && s.chars().count() <= NAME_MAX =>
        {
            Ok(printnode::py_trim(s).to_string())
        }
        _ => Err(jerr(
            400,
            format!("{what} must be a non-empty string of at most {NAME_MAX} characters"),
        )),
    }
}

/// The `org_id` a write names to prove its page is not stale.
/// The `org_created_at` the console's page showed (the org's `created_at` of the read endpoints).
/// Missing or not a number: `None`, which matches no org, so the write is refused as stale.
fn seen_of(body: &Obj) -> Option<f64> {
    body.get("org_created_at").and_then(Value::as_f64)
}

fn org_changed() -> Resp {
    jerr(409, "the org changed, reload")
}

fn org_id_of(body: &Obj) -> Gate<i64> {
    body.get("org_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| jerr(400, "org_id required"))
}

/// The date of day `n` of the month starting at `month_start`, `YYYY-MM-DD` (UTC).
fn day_label(month_start: f64, n: i64) -> String {
    chrono::DateTime::from_timestamp(month_start as i64 + n * 86_400, 0)
        .map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string())
}

fn org_json(o: &store::OperatorOrg) -> Value {
    json!({
        "id": o.id, "name": o.name, "created_at": o.created_at, "plan": o.plan,
        "job_quota": o.job_quota, "jobs_this_month": o.jobs_this_month,
        "queued_jobs": o.queued_jobs, "failed_today": o.failed_today,
        "last_job_at": o.last_job_at, "agents_online": o.agents_online,
        "agents_total": o.agents_total, "newest_agent_seen_at": o.newest_agent_seen_at,
        "printers": o.printers, "locked": o.locked, "first_user_email": o.first_user_email,
        "terms_version": o.terms_version, "terms_accepted_at": o.terms_accepted_at,
        "status": o.status,
    })
}

/// A job of the operator's list; `title` only on a support read.
fn job_json(j: &store::OperatorJob, title: Option<&Option<String>>) -> Value {
    let mut v = json!({
        "id": j.id, "created_at": j.created_at, "claimed_at": j.claimed_at,
        "finished_at": j.finished_at, "type": j.type_, "mode": j.mode,
        "state": j.state, "printer_id": j.printer_id,
        "printer_name": j.printer_name, "copies": j.copies, "error": j.error,
    });
    if let Some(t) = title {
        v["title"] = json!(t);
    }
    v
}

/// Is `key` the operator token (constant-time)? An empty key never is.
pub fn is_operator_token(cfg: &Config, key: &str) -> bool {
    cfg.operator_token
        .as_deref()
        .is_some_and(|t| !key.is_empty() && auth::ct_eq(key, t))
}

/// The request with the operator token removed, or `None` when it carried none. The token can
/// arrive as a Bearer value, as the HTTP Basic username (the PrintNode layer's way) or as the
/// key in a CloudPRNT URL; it is no credential for any of them.
pub(super) fn without_operator_credential(req: &Req, token: &str) -> Option<Req> {
    let header = req.header("authorization");
    let presented = match header.strip_prefix("Bearer ") {
        Some(b) => b.to_string(),
        None => printnode::basic_key(header),
    };
    let in_header = !presented.is_empty() && auth::ct_eq(&presented, token);
    let in_path = cloudprnt_route(req.path())
        .flatten()
        .is_some_and(|k| auth::ct_eq(k, token));
    if !in_header && !in_path {
        return None;
    }
    let mut clean = req.clone();
    if in_header {
        clean.headers.remove("authorization");
    }
    if in_path {
        // an unknown key, answered like any other
        clean.target = clean.target.replacen(token, "_", 1);
    }
    Some(clean)
}

impl Ctx<'_> {
    /// Everything under `/operator/`. Never cached, whatever the answer.
    pub(super) fn operator(&self) -> Resp {
        let mut r = self.operator_route();
        r.headers.push(("Cache-Control", "no-store".into()));
        r
    }

    /// The operator token, presented from a socket peer in `PRINTAPI_OPERATOR_FROM`. The peer is
    /// the raw socket address: `X-Forwarded-For` (which `peer_ip` resolves) is never consulted,
    /// or anyone who can reach a trusted proxy could name the console's address. Root does not
    /// pass: its token is a different credential with a different reach.
    fn operator_gate(&self) -> bool {
        let cfg = self.cfg();
        let Some(token) = cfg.operator_token.as_deref() else {
            return false;
        };
        let Ok(peer) = self.req.socket_peer.parse::<IpAddr>() else {
            return false;
        };
        // both sides are always evaluated: no early exit on a wrong token
        let token_ok = auth::ct_eq(self.presented_key(), token);
        let peer_ok = cfg.operator_from.contains(peer);
        token_ok & peer_ok
    }

    fn operator_route(&self) -> Resp {
        if self.cfg().operator_token.is_none() {
            return not_found();
        }
        // a 401 before any 404: the gate does not tell which paths exist
        if !self.operator_gate() {
            return unauthorized();
        }
        let path = self.req.path();
        let r = match (self.req.method.as_str(), path) {
            ("GET", "/operator/overview") => self.operator_overview(),
            ("GET", "/operator/audit") => self.operator_audit(),
            ("GET", "/operator/settings") => self.operator_settings(),
            ("PUT", "/operator/settings") => self.operator_settings_put(),
            ("GET", "/operator/orgs") => self.operator_orgs(),
            ("POST", "/operator/orgs") => self.operator_org_create(),
            (m, _) => match (m, org_route(path)) {
                ("GET", Some((id, ""))) => self.operator_org(id),
                ("GET", Some((id, "jobs"))) => self.operator_org_jobs(id),
                ("PUT", Some((id, ""))) => self.operator_org_update(id),
                ("DELETE", Some((id, ""))) => self.operator_org_delete(id),
                ("POST", Some((id, "lock"))) => self.operator_org_lock(id),
                ("POST", Some((id, "unlock"))) => self.operator_org_unlock(id),
                ("POST", Some((id, "support"))) => self.operator_support_grant(id),
                ("POST", None) => {
                    if let Some(uid) = id_of(path, "/operator/users/", "/reset") {
                        self.operator_user_reset(uid)
                    } else if let Some(jid) = id_of(path, "/operator/jobs/", "/payload") {
                        self.operator_support_payload(jid)
                    } else {
                        Ok(not_found())
                    }
                }
                ("DELETE", None) => {
                    if let Some(kid) = id_of(path, "/operator/keys/", "") {
                        self.operator_key_revoke(kid)
                    } else if let Some(gid) = id_of(path, "/operator/support/", "") {
                        self.operator_support_end(gid)
                    } else {
                        Ok(not_found())
                    }
                }
                _ => Ok(not_found()),
            },
        };
        r.unwrap_or_else(|e| {
            eprintln!("internal error: {e}");
            jerr(500, "internal error")
        })
    }

    /// `limit` (1..=200, default 50) and an optional integer, from the query string.
    fn paging(&self, cursor: &str) -> Result<(i64, Option<i64>), Resp> {
        let q = query_params(self.target());
        let int = |name: &str| -> Result<Option<i64>, Resp> {
            qfirst(&q, name)
                .map(|v| {
                    v.parse::<i64>()
                        .map_err(|_| jerr(400, format!("bad {name}")))
                })
                .transpose()
        };
        let limit = int("limit")?.unwrap_or(LIST_DEFAULT_LIMIT);
        if limit < 1 {
            return Err(jerr(400, "bad limit"));
        }
        Ok((limit.min(LIST_MAX_LIMIT), int(cursor)?))
    }

    /// Reset mails and invitations need a mail server and a `PUBLIC_URL` to link to.
    fn mail_ready(&self) -> bool {
        self.cfg().reset_enabled && !self.cfg().public_url.is_empty()
    }

    /// Send `text` to `to`. A mail that did not go out writes a second audit row (`row`, as
    /// `user.reset_mail_failed`); it names ids only.
    fn send_operator_mail(
        &self,
        to: &str,
        subject: &str,
        text: &str,
        row: &store::AuditRow,
    ) -> bool {
        let sent = matches!(self.s.mailer.send(to, subject, text), Ok(true));
        if !sent {
            eprintln!("operator mail failed");
            let note = store::AuditRow {
                action: "user.reset_mail_failed",
                ..row.clone()
            };
            if let Err(e) = self.db(|c| store::operator_audit_note(c, &note)) {
                eprintln!("audit row for a failed mail not written: {e}");
            }
        }
        sent
    }

    fn operator_org_update(&self, id: i64) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, 0));
        let mut update = store::OrgUpdate::default();
        let mut detail = serde_json::Map::new();
        let name;
        if body.contains_key("name") {
            name = gate!(name_of(body.get("name"), "name"));
            update.name = Some(&name);
            detail.insert("name".into(), json!(true));
        }
        if let Some(plan_v) = body.get("plan") {
            let Some(plan) = billing::find(&self.cfg().plans, plan_v) else {
                return Ok(jerr(400, format!("unknown plan: {}", repr(plan_v))));
            };
            update.plan = Some((&plan.id, plan.jobs));
            detail.insert("plan".into(), json!(plan.id));
            detail.insert("job_quota".into(), json!(plan.jobs));
        }
        if let Some(quota) = body.get("job_quota") {
            let quota = match parse_job_quota(quota) {
                Ok(q) => q,
                Err(msg) => return Ok(jerr(400, msg)),
            };
            update.job_quota = Some(quota);
            detail.insert("job_quota".into(), json!(quota));
        }
        if detail.is_empty() {
            return Ok(jerr(400, "nothing to update"));
        }
        let detail = Value::Object(detail);
        let seen = seen_of(&body);
        let out = self
            .db(|c| store::operator_update_org(c, id, seen, &update, reason.as_deref(), &detail))?;
        Ok(match out {
            store::Outcome::Done(()) => ok_true(),
            store::Outcome::Refused(_) => org_changed(),
            store::Outcome::NotFound => not_found(),
        })
    }

    fn operator_org_lock(&self, id: i64) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, LOCK_REASON_MIN)).unwrap_or_default();
        let seen = seen_of(&body);
        Ok(
            match self.db(|c| store::operator_lock_org(c, id, seen, &reason))? {
                store::Outcome::Done(n) => {
                    Resp::json(200, &json!({ "ok": true, "cancelled_jobs": n }))
                }
                store::Outcome::Refused(store::Refusal::OrgChanged) => org_changed(),
                store::Outcome::Refused(_) => jerr(409, "org is already locked"),
                store::Outcome::NotFound => not_found(),
            },
        )
    }

    fn operator_org_unlock(&self, id: i64) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, 0));
        let seen = seen_of(&body);
        Ok(
            match self.db(|c| store::operator_unlock_org(c, id, seen, reason.as_deref()))? {
                store::Outcome::Done(()) => ok_true(),
                store::Outcome::Refused(store::Refusal::OrgChanged) => org_changed(),
                store::Outcome::Refused(_) => jerr(409, "org is not locked"),
                store::Outcome::NotFound => not_found(),
            },
        )
    }

    fn operator_support_grant(&self, org_id: i64) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, LOCK_REASON_MIN)).unwrap_or_default();
        let minutes = body
            .get("minutes")
            .and_then(Value::as_i64)
            .filter(|m| SUPPORT_MINUTES.contains(m));
        let Some(minutes) = minutes else {
            return Ok(jerr(400, "minutes must be 15, 30 or 60"));
        };
        let seen = seen_of(&body);
        let out = self.db(|c| store::operator_grant_support(c, org_id, seen, &reason, minutes))?;
        Ok(match out {
            store::Outcome::Done(g) => Resp::json(
                200,
                &json!({
                    "id": g.id, "reason": g.reason, "granted_at": g.granted_at,
                    "expires_at": g.expires_at,
                }),
            ),
            store::Outcome::Refused(store::Refusal::OrgChanged) => org_changed(),
            store::Outcome::Refused(_) => jerr(409, "the org already has an active support grant"),
            store::Outcome::NotFound => not_found(),
        })
    }

    fn operator_support_end(&self, grant_id: i64) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, 0));
        Ok(
            match self.db(|c| store::operator_end_support(c, grant_id, reason.as_deref()))? {
                store::Outcome::Done(()) => ok_true(),
                store::Outcome::Refused(_) => jerr(409, "the support grant is not active"),
                store::Outcome::NotFound => not_found(),
            },
        )
    }

    /// The raw bytes of one job's payload, if its org has an active grant. POST, so nothing that
    /// prefetches or embeds a URL can trigger the (audited) read. The bytes go out as a download
    /// no browser renders: customer data may be a hostile PDF or HTML.
    fn operator_support_payload(&self, job_id: i64) -> R {
        Ok(match self.db(|c| store::support_read_payload(c, job_id))? {
            store::PayloadRead::Bytes { bytes, mode } => Resp {
                status: 200,
                headers: vec![
                    ("Content-Type", "application/octet-stream".into()),
                    (
                        "Content-Disposition",
                        format!(
                            "attachment; filename=\"job-{job_id}.{}\"",
                            if mode == "pdf" { "pdf" } else { "bin" }
                        ),
                    ),
                    ("X-Content-Type-Options", "nosniff".into()),
                    (
                        "Content-Security-Policy",
                        "sandbox; default-src 'none'".into(),
                    ),
                ],
                body: RespBody::Bytes(bytes.into()),
            },
            store::PayloadRead::NotFound => not_found(),
            store::PayloadRead::Refused(_) => jerr(403, NO_SUPPORT_ACCESS),
            store::PayloadRead::Scrubbed => jerr(410, "the payload was removed by retention"),
        })
    }

    fn operator_org_delete(&self, id: i64) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, 1)).unwrap_or_default();
        let Some(Value::String(confirm)) = body.get("confirm") else {
            return Ok(jerr(400, "confirm must be the org name"));
        };
        let seen = seen_of(&body);
        let out = self.db(|c| store::operator_delete_org(c, id, seen, confirm, &reason))?;
        Ok(match out {
            store::Outcome::Done(()) => {
                // secure_delete zeroed the pages; this clears the WAL frames too. A reader can
                // block it: logged, never a failed request (the org is gone either way).
                self.db(|c| store::truncate_wal_logged(c, "operator delete org"));
                ok_true()
            }
            store::Outcome::Refused(store::Refusal::DefaultOrg) => {
                jerr(400, "the default org cannot be removed")
            }
            store::Outcome::Refused(store::Refusal::OrgChanged) => org_changed(),
            store::Outcome::Refused(_) => jerr(400, "confirm must be the org name"),
            store::Outcome::NotFound => not_found(),
        })
    }

    fn operator_org_create(&self) -> R {
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, 0));
        let name = gate!(name_of(body.get("name"), "name"));
        let email = str_field(&body, "email")
            .map(store::normalize_email)
            .unwrap_or_default();
        if !email.contains('@') {
            return Ok(jerr(400, "email required"));
        }
        if !self.mail_ready() {
            return Ok(jerr(409, "mail is not configured"));
        }
        // a hash of a random password, never a NULL one: login timing reveals nothing about the
        // invited account, and nobody can log in before the invitation is used
        let pw_hash = match auth::hash_password(Some(&auth::token_urlsafe(32))) {
            Ok(h) => h,
            Err(e) => return Ok(jerr(500, e)),
        };
        let tok = auth::new_session_token();
        let ttl = self.cfg().reset_ttl_s;
        let quota = self.live.signup_quota.value;
        let new = match self.db(|c| {
            store::operator_create_org(
                c,
                &name,
                &email,
                &pw_hash,
                quota,
                &tok,
                ttl,
                reason.as_deref(),
            )
        }) {
            Ok(n) => n,
            Err(StoreError::Conflict) => return Ok(jerr(409, "email already registered")),
            Err(e) => return Err(e),
        };
        let text = self.reset_mail_text(
            "You have been invited to printpapi. Choose a password with the link below.",
            &tok,
        );
        let created = self
            .db(|c| store::org_created_at(c, new.org_id))
            .unwrap_or(None);
        let row = store::AuditRow {
            org_id: Some(new.org_id),
            org_created_at: created,
            target: Some(&format!("user:{}", new.user_id)),
            ..Default::default()
        };
        let sent = self.send_operator_mail(&email, "Your printpapi invitation", &text, &row);
        Ok(Resp::json(
            200,
            &json!({ "ok": true, "org_id": new.org_id, "user_id": new.user_id, "mail_sent": sent }),
        ))
    }

    fn operator_user_reset(&self, user_id: i64) -> R {
        let body = gate!(self.read_obj());
        let org_id = gate!(org_id_of(&body));
        let reason = gate!(reason_of(&body, 0));
        if !self.mail_ready() {
            return Ok(jerr(409, "mail is not configured"));
        }
        let tok = auth::new_session_token();
        let ttl = self.cfg().reset_ttl_s;
        let seen = seen_of(&body);
        let out = self.db(|c| {
            store::operator_reset_user(c, user_id, org_id, seen, &tok, ttl, reason.as_deref())
        })?;
        let email = match out {
            store::Outcome::Done(e) => e,
            store::Outcome::Refused(store::Refusal::OrgChanged) => return Ok(org_changed()),
            store::Outcome::Refused(_) => return Ok(jerr(409, "user belongs to another org")),
            store::Outcome::NotFound => return Ok(not_found()),
        };
        let text = self.reset_mail_text(
            "An operator started a password reset for your printpapi account.",
            &tok,
        );
        let created = self
            .db(|c| store::org_created_at(c, org_id))
            .unwrap_or(None);
        let row = store::AuditRow {
            org_id: Some(org_id),
            org_created_at: created,
            target: Some(&format!("user:{user_id}")),
            ..Default::default()
        };
        if self.send_operator_mail(&email, "Reset your printpapi password", &text, &row) {
            Ok(ok_true())
        } else {
            Ok(jerr(502, "mail could not be sent"))
        }
    }

    fn operator_key_revoke(&self, key_id: i64) -> R {
        let body = gate!(self.read_obj());
        let org_id = gate!(org_id_of(&body));
        let reason = gate!(reason_of(&body, 0));
        let seen = seen_of(&body);
        let out =
            self.db(|c| store::operator_revoke_key(c, key_id, org_id, seen, reason.as_deref()))?;
        Ok(match out {
            store::Outcome::Done(()) => {
                // a long-poll of an agent enrolled with this key re-checks it and ends with 401
                self.s.waker.wake_all();
                ok_true()
            }
            store::Outcome::Refused(store::Refusal::OrgChanged) => org_changed(),
            store::Outcome::Refused(_) => jerr(409, "key belongs to another org"),
            store::Outcome::NotFound => not_found(),
        })
    }

    fn operator_orgs(&self) -> R {
        let (limit, after) = gate!(self.paging("after"));
        let q = query_params(self.target());
        let status = qfirst(&q, "status");
        if status.is_some_and(|s| !store::ORG_STATUSES.contains(&s)) {
            return Ok(jerr(400, "bad status"));
        }
        let query = store::OrgQuery {
            status,
            q: qfirst(&q, "q"),
            after: after.unwrap_or(0),
            only: None,
            // one more than asked says whether there is a next page
            limit: limit + 1,
        };
        let win = self.cfg().online_window_s;
        let mut orgs = self.db(|c| store::operator_list_orgs(c, crate::now(), win, &query))?;
        let more = orgs.len() as i64 > limit;
        orgs.truncate(limit as usize);
        let next = if more {
            orgs.last().map(|o| o.id)
        } else {
            None
        };
        let orgs: Vec<Value> = orgs.iter().map(org_json).collect();
        Ok(Resp::json(
            200,
            &json!({ "orgs": orgs, "next_after": next }),
        ))
    }

    fn operator_org(&self, id: i64) -> R {
        let now = crate::now();
        let win = self.cfg().online_window_s;
        let month = store::month_start(now);
        let found = self.db(|c| {
            let Some(org) = store::operator_get_org(c, now, win, id)? else {
                return Ok::<_, StoreError>(None);
            };
            Ok(Some((
                org,
                store::org_jobs_by_day(c, id, month)?,
                store::operator_org_parts(c, id)?,
                store::list_agents(c, win, Some(now), Some(id))?,
                store::active_support_grant(c, id)?,
            )))
        })?;
        let Some((org, by_day, parts, agents, support)) = found else {
            return Ok(not_found());
        };
        // every day of the month so far, quiet ones as zeros
        let days = ((now - month) / 86_400.0).floor() as i64 + 1;
        let usage: Vec<Value> = (0..days)
            .map(|d| {
                let (jobs, failed) = by_day
                    .iter()
                    .find(|(i, _, _)| *i == d)
                    .map_or((0, 0), |(_, j, f)| (*j, *f));
                json!({ "date": day_label(month, d), "jobs": jobs, "failed": failed })
            })
            .collect();
        let mut v = org_json(&org);
        let o = v.as_object_mut().expect("an object");
        o.insert("usage_by_day".into(), json!(usage));
        o.insert(
            "users".into(),
            json!(
                parts
                    .users
                    .iter()
                    .map(|u| json!({
                        "id": u.id, "email": u.email, "created_at": u.created_at
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        o.insert(
            "agents".into(),
            json!(agents.iter().map(|a| json!({
                "id": a.id, "name": a.name, "last_seen_at": a.last_seen_at, "online": a.online
            })).collect::<Vec<_>>()),
        );
        o.insert(
            "printers".into(),
            json!(
                parts
                    .printers
                    .iter()
                    .map(|p| json!({
                        "id": p.id, "name": p.name, "can_pdf": p.can_pdf, "state": p.state,
                        "agent_id": p.agent_id
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        o.insert(
            "keys".into(),
            json!(
                parts
                    .keys
                    .iter()
                    .map(|k| json!({
                        "id": k.id, "label": k.label, "active": k.active, "created_at": k.created_at
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        o.insert(
            "terms".into(),
            json!(
                parts
                    .terms
                    .iter()
                    .map(|(version, at)| json!({
                        "version": version, "accepted_at": at
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        o.insert(
            "lock".into(),
            json!(parts.lock.as_ref().map(|l| json!({
                "reason": l.reason, "locked_at": l.locked_at
            }))),
        );
        o.insert(
            "support".into(),
            json!(support.map(|g| json!({
                "id": g.id, "reason": g.reason, "granted_at": g.granted_at,
                "expires_at": g.expires_at, "reads": g.reads,
            }))),
        );
        Ok(Resp::json(200, &v))
    }

    fn operator_org_jobs(&self, id: i64) -> R {
        let (limit, before_id) = gate!(self.paging("before_id"));
        // titles are opt-in: a page without `titles=1` is metadata only, whatever the grant state
        if qfirst(&query_params(self.target()), "titles") == Some("1") {
            return self.operator_org_jobs_titled(id, limit, before_id);
        }
        let found = self.db(|c| {
            if !store::org_exists(c, id)? {
                return Ok::<_, StoreError>(None);
            }
            Ok(Some(store::operator_org_jobs(c, id, before_id, limit + 1)?))
        })?;
        let Some(mut jobs) = found else {
            return Ok(not_found());
        };
        let more = jobs.len() as i64 > limit;
        jobs.truncate(limit as usize);
        let next = if more {
            jobs.last().map(|j| j.id)
        } else {
            None
        };
        let jobs: Vec<Value> = jobs.iter().map(|j| job_json(j, None)).collect();
        Ok(Resp::json(
            200,
            &json!({ "jobs": jobs, "next_before_id": next }),
        ))
    }

    /// The same page with titles, for an org with an active support grant (one audit row).
    fn operator_org_jobs_titled(&self, id: i64, limit: i64, before_id: Option<i64>) -> R {
        // exactly `limit` rows: the audit row names every id the store read, so no peeked extra
        // row may be among them. A full page therefore always offers a cursor (the next page may
        // be empty, and an empty page writes no row).
        let out = self.db(|c| store::support_org_jobs(c, id, before_id, limit))?;
        let jobs = match out {
            store::Outcome::Done(j) => j,
            store::Outcome::Refused(_) => return Ok(jerr(403, NO_SUPPORT_ACCESS)),
            store::Outcome::NotFound => return Ok(not_found()),
        };
        let next = if jobs.len() as i64 == limit {
            jobs.last().map(|(j, _)| j.id)
        } else {
            None
        };
        let jobs: Vec<Value> = jobs
            .iter()
            .map(|(j, title)| job_json(j, Some(title)))
            .collect();
        Ok(Resp::json(
            200,
            &json!({ "jobs": jobs, "next_before_id": next }),
        ))
    }

    fn operator_audit(&self) -> R {
        let q = query_params(self.target());
        let int = |name: &str| -> Result<Option<i64>, Resp> {
            qfirst(&q, name)
                .map(|v| {
                    v.parse::<i64>()
                        .map_err(|_| jerr(400, format!("bad {name}")))
                })
                .transpose()
        };
        let org = gate!(int("org"));
        let before_id = gate!(int("before_id"));
        let limit = gate!(int("limit")).unwrap_or(AUDIT_DEFAULT_LIMIT);
        if limit < 1 {
            return Ok(jerr(400, "bad limit"));
        }
        let limit = limit.min(AUDIT_MAX_LIMIT);
        let action = qfirst(&q, "action");
        // one row more than asked says whether there is a next page
        let mut rows = self.db(|c| store::list_audit(c, org, action, before_id, limit + 1))?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        let next = if more {
            rows.last().map(|r| r.id)
        } else {
            None
        };
        let entries: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.id, "at": r.at, "actor": r.actor, "action": r.action,
                    "org_id": r.org_id, "org_created_at": r.org_created_at,
                    "target": r.target, "reason": r.reason,
                    // written as JSON by the actions; anything else is shown as the text it is
                    "detail": r.detail.as_deref().map(|d| {
                        serde_json::from_str::<Value>(d).unwrap_or_else(|_| Value::String(d.into()))
                    }),
                })
            })
            .collect();
        Ok(Resp::json(
            200,
            &json!({ "entries": entries, "next_before_id": next }),
        ))
    }

    /// The live values with their source, and the startup-only values beside them (never a
    /// secret: no token, no password, no key).
    fn settings_json(&self, live: &settings::Live) -> Value {
        let cfg = self.cfg();
        let terms = live.terms.value.as_ref().map(settings::terms_json);
        // `ignored` (only when a row exists but is not applied) is the stored value, so a stale
        // row can be seen and cleared.
        let entry = |key: &str, value: Value, source: settings::Source| {
            let mut m = serde_json::Map::new();
            m.insert("value".into(), value);
            m.insert("source".into(), json!(source.as_str()));
            if let Some((_, stored)) = live.ignored.iter().find(|(k, _)| *k == key) {
                m.insert("ignored".into(), stored.clone());
            }
            Value::Object(m)
        };
        json!({
            "settings": {
                "signup": entry(
                    "signup",
                    json!(if live.signup.value { "open" } else { "closed" }),
                    live.signup.source,
                ),
                "signup_quota": entry(
                    "signup_quota", json!(live.signup_quota.value), live.signup_quota.source,
                ),
                "job_retention_days": entry(
                    "job_retention_days",
                    json!(live.job_retention_days.value),
                    live.job_retention_days.source,
                ),
                "terms": entry("terms", json!(terms), live.terms.source),
            },
            "env_only": {
                "fetches_per_org": cfg.fetch_cap(),
                "private_urls": if cfg.destinations == dispatch::Destinations::Any {
                    "allow"
                } else {
                    "deny"
                },
                "trusted_proxies": cfg.trusted_proxies.entries(),
                "operator_from": cfg.operator_from.entries(),
                "public_url": cfg.public_url,
                "mail": cfg.reset_enabled,
            },
        })
    }

    fn operator_settings(&self) -> R {
        Ok(Resp::json(200, &self.settings_json(&self.live)))
    }

    fn operator_settings_put(&self) -> R {
        use settings::{Conflict, PutError};
        let body = gate!(self.read_obj());
        let reason = gate!(reason_of(&body, 0));
        let env = self.cfg().env_start();
        let any = self.cfg().destinations == dispatch::Destinations::Any;
        let decide = |live: &settings::Live| {
            let change = settings::validate_put(&body, reason.as_deref(), live, &env, any)?;
            if settings::unset_needs_reason(&change, live, &env)
                && !settings::reason_is_enough(reason.as_deref())
            {
                return Err(PutError::ReasonNeeded);
            }
            Ok(change)
        };
        Ok(
            match self.s.try_apply_settings(decide, reason.as_deref())? {
                Ok(live) => Resp::json(200, &self.settings_json(&live)),
                Err(PutError::Invalid(m)) => jerr(400, m),
                Err(PutError::ReasonNeeded) => jerr(
                    400,
                    format!(
                        "reason of at least {} characters required: this scrubs payloads and titles for good",
                        settings::REASON_MIN
                    ),
                ),
                Err(PutError::Conflict(Conflict::SignupLocked)) => jerr(
                    409,
                    "signup is locked closed by PRINTAPI_SIGNUP in the environment: only closing it is accepted",
                ),
                Err(PutError::Conflict(Conflict::OpenWithPrivateUrls)) => jerr(
                    409,
                    "signup cannot be opened while private URLs are allowed (PRINTAPI_PRIVATE_URLS=allow)",
                ),
            },
        )
    }

    fn operator_overview(&self) -> R {
        let mut cache = self
            .s
            .overview_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = cache.as_ref()
            && at.elapsed() < OVERVIEW_TTL
        {
            return Ok(Resp::json(200, v));
        }
        let now = crate::now();
        let (n, (db_bytes, wal_bytes)) = self
            .db(|c| Ok::<_, StoreError>((store::overview_counts(c, now)?, store::db_sizes(c)?)))?;
        let purge = self
            .s
            .last_purge
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let today: serde_json::Map<String, Value> = store::JOB_STATES
            .iter()
            .zip(n.jobs_today)
            .map(|(s, n)| (s.to_string(), json!(n)))
            .collect();
        let v = json!({
            "orgs": n.orgs,
            "orgs_active_7d": n.orgs_active_7d,
            "signups_7d": n.signups_7d,
            "jobs_this_month": n.jobs_this_month,
            "failed_jobs_30d": n.failed_jobs_30d,
            "jobs_today": today,
            "server": {
                "version": env!("CARGO_PKG_VERSION"),
                "uptime_s": self.s.started.elapsed().as_secs_f64().floor(),
            },
            "db": { "size_bytes": db_bytes, "wal_bytes": wal_bytes },
            "retention": {
                "last_pass_at": purge.as_ref().map(|p| p.0),
                "abandoned": purge.as_ref().map(|p| p.1.abandoned),
                "scrubbed": purge.as_ref().map(|p| p.1.scrubbed),
                "deleted": purge.as_ref().map(|p| p.1.deleted),
            },
            // the console shows a section only when the server reports its feature
            "features": ["support", "settings"],
        });
        *cache = Some((std::time::Instant::now(), v.clone()));
        Ok(Resp::json(200, &v))
    }
}
