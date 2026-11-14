// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The operator API under `/operator/`: one service-level credential (`PRINTAPI_OPERATOR_TOKEN`)
//! for the console of a hosted deployment. Inert without the token: every `/operator/` path is a
//! 404. The token is never a `Principal`: only `operator_gate` reads it, and on every other route
//! `without_operator_credential` takes it out of the request before any lookup runs.
use super::*;
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

/// `/operator/orgs/{id}` (false) or `/operator/orgs/{id}/jobs` (true), the id all digits.
fn org_route(path: &str) -> Option<(i64, bool)> {
    let rest = path.strip_prefix("/operator/orgs/")?;
    let (id, jobs) = match rest.split_once('/') {
        Some((id, "jobs")) => (id, true),
        Some(_) => return None,
        None => (rest, false),
    };
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((id.parse().ok()?, jobs))
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
            ("GET", "/operator/orgs") => self.operator_orgs(),
            ("GET", _) => match org_route(path) {
                Some((id, false)) => self.operator_org(id),
                Some((id, true)) => self.operator_org_jobs(id),
                None => Ok(not_found()),
            },
            _ => Ok(not_found()),
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
            )))
        })?;
        let Some((org, by_day, parts, agents)) = found else {
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
        Ok(Resp::json(200, &v))
    }

    fn operator_org_jobs(&self, id: i64) -> R {
        let (limit, before_id) = gate!(self.paging("before_id"));
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
        let jobs: Vec<Value> = jobs
            .iter()
            .map(|j| {
                json!({
                    "id": j.id, "created_at": j.created_at, "claimed_at": j.claimed_at,
                    "finished_at": j.finished_at, "type": j.type_, "mode": j.mode,
                    "state": j.state, "printer_id": j.printer_id,
                    "printer_name": j.printer_name, "copies": j.copies, "error": j.error,
                })
            })
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
            "features": [],
        });
        *cache = Some((std::time::Instant::now(), v.clone()));
        Ok(Resp::json(200, &v))
    }
}
