// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The print server as a library, so the integration tests drive the same code `main` runs.
//!
//! Module map (each mirrors the Python module it replaced, see git history before v3.0.0):
//! - pure logic: `dispatch`, `auth`, `billing`, `cloudprnt`, `label`, `orders`, `packing_slip`,
//!   `printnode`, `proxy` (client address), `mail` (env -> SMTP plan), `settings` (env parsers, live values), `pyfmt` (Python-compatible value formatting)
//! - IO: `store` (SQLite), `server` (routing, sync), `http` (hyper glue, long-poll), `web`
//!   (embedded dashboard bundle)

pub mod auth;
pub mod billing;
pub mod cloudprnt;
pub mod dispatch;
pub mod http;
pub mod label;
pub mod mail;
pub mod orders;
pub mod packing_slip;
pub mod printnode;
pub mod proxy;
pub mod pyfmt;
pub mod server;
pub mod settings;
pub mod store;
pub mod web;

/// Wall-clock seconds since the epoch, as `time.time()` — every timestamp column is a REAL of
/// this, so a DB written by the Python server and one written here stay interchangeable.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
