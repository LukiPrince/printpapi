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
        let r = match (self.req.method.as_str(), self.req.path()) {
            ("GET", "/operator/overview") => self.operator_overview(),
            ("GET", "/operator/audit") => self.operator_audit(),
            _ => Ok(not_found()),
        };
        r.unwrap_or_else(|e| {
            eprintln!("internal error: {e}");
            jerr(500, "internal error")
        })
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
