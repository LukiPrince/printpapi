// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Star CloudPRNT protocol — the port of `app/cloudprnt.py`. Pure translation, no IO.
//!
//! Star's printers poll an HTTP URL by themselves (a `POST` with their status, a `GET` for the job
//! data, a `DELETE` to confirm), so the printer is the agent. One printer = one pseudo-agent:
//! poll = `claim_job`, GET = `get_payload`, DELETE = `finish_job`. Job bytes are served exactly as
//! submitted; the media type is only the label the printer honours.
//!
//! "Star", "CloudPRNT" and "Star Micronics" are trademarks of Star Micronics Co., Ltd., used here
//! descriptively to say which protocol this speaks. This is an independent implementation from the
//! published protocol documentation; no Star code or documentation text was copied.
use crate::printnode::{py_is_space, py_trim};
use serde_json::{Value, json};

/// What we are willing to label a raw job's bytes as, best first. All are byte streams the printer
/// executes as-is; image and PDF types are absent on purpose, since a CloudPRNT printer cannot
/// render (never send PDF to a label printer).
pub const MEDIA_TYPES: &[&str] = &[
    "application/vnd.star.starprnt",
    "application/vnd.star.starprntcore",
    "application/vnd.star.line",
    "application/vnd.star.linematrix",
    "application/vnd.star.raster",
    "text/plain",
];
pub const MEDIA_DEFAULT: &str = "application/vnd.star.starprnt";

/// The type to serve, from the client's `Accept` header. Falls back to Star PRNT mode, which the
/// current models speak and in which a plain-text receipt is a valid stream anyway.
pub fn media_type(accept: Option<&str>) -> &'static str {
    let offered: Vec<String> = accept
        .unwrap_or("")
        .split(',')
        .map(|p| py_trim(p.split(';').next().unwrap_or("")).to_lowercase())
        .collect();
    MEDIA_TYPES
        .iter()
        .find(|t| offered.iter().any(|o| o == *t))
        .copied()
        .unwrap_or(MEDIA_DEFAULT)
}

/// `{"jobReady": false}` or the offer of `job_id` under `media`.
pub fn poll_response(job_id: Option<i64>, media: &str) -> Value {
    match job_id {
        None => json!({"jobReady": false}),
        // jobToken is echoed back on the GET and the DELETE, which is how those two name the job.
        Some(id) => json!({"jobReady": true, "mediaTypes": [media], "jobToken": id.to_string()}),
    }
}

/// Did the printer's confirmation code report a successful print? Every code beginning with 2
/// means online and printed (201 and 211 are successes carrying a warning); the bare "OK" is the
/// guide's own plain success. Anything else, including no code, is a failure: reporting a job as
/// printed when it was not is the worse mistake.
pub fn job_ok(code: Option<&str>) -> bool {
    let c = py_trim(code.unwrap_or(""));
    c.eq_ignore_ascii_case("OK") || c.starts_with('2')
}

/// The documented status codes an operator can act on. Anything else passes through as sent.
fn status_desc(code: &str) -> Option<&'static str> {
    Some(match code {
        "410" => "out of paper",
        "411" => "paper jam",
        "412" => "roll position error",
        "420" => "cover open",
        "511" => "image conversion failed",
        "520" => "job download failed",
        "521" => "job too large (512 KB, or 2 MB on mC-Label3/HI01X/HI02X)",
        _ => return None,
    })
}

/// The printer's confirmation code with a plain-language reason where we know one — the only
/// diagnostic an operator gets for a device with no agent and no log of its own.
pub fn status_text(code: Option<&str>) -> String {
    let c = py_trim(code.unwrap_or(""));
    if c.is_empty() {
        return "no status".into();
    }
    match c
        .split(py_is_space)
        .find(|w| !w.is_empty())
        .and_then(status_desc)
    {
        Some(desc) => format!("{c} ({desc})"),
        None => c.to_string(),
    }
}

/// The agent/printer name a device enrols under, keyed on the normalized MAC address: the one
/// identifier the printer sends on every request of all three methods.
pub fn device_name(mac: &str) -> String {
    format!("cloudprnt-{}", py_trim(mac).to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_type_picks_the_best_type_the_client_accepts() {
        let accept = "application/vnd.star.starprnt, application/vnd.star.starconfiguration, \
                      application/vnd.star.starprntcore; q=0.9, image/png; q=0.1";
        assert_eq!(media_type(Some(accept)), "application/vnd.star.starprnt");
        // an interface board that speaks line mode but not starprnt
        assert_eq!(
            media_type(Some("application/vnd.star.line, image/png")),
            "application/vnd.star.line"
        );
        assert_eq!(media_type(Some("text/plain")), "text/plain");
        // nothing we can label our bytes with, or no header at all -> the default
        assert_eq!(media_type(Some("image/png, image/jpeg")), MEDIA_DEFAULT);
        assert_eq!(media_type(None), MEDIA_DEFAULT);
        assert_eq!(media_type(Some("TEXT/PLAIN;q=1")), "text/plain");
    }

    #[test]
    fn poll_response_shapes() {
        assert_eq!(
            poll_response(None, MEDIA_DEFAULT),
            json!({"jobReady": false})
        );
        assert_eq!(
            poll_response(Some(7), "text/plain"),
            json!({"jobReady": true, "mediaTypes": ["text/plain"], "jobToken": "7"})
        );
        let r = poll_response(Some(7), "x");
        let keys: Vec<_> = r.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["jobReady", "mediaTypes", "jobToken"]);
    }

    #[test]
    fn job_ok_reads_the_printers_status_code() {
        assert!(job_ok(Some("200 OK")));
        assert!(job_ok(Some("OK")));
        // every 2xx means the printer is online and printed, including the ones with a warning
        assert!(job_ok(Some("201 Output paper taken")));
        assert!(job_ok(Some("211 Paper low")));
        assert!(!job_ok(Some("420 Cover open")));
        assert!(!job_ok(Some("521")));
        assert!(!job_ok(Some(""))); // no status is not a success
        assert!(!job_ok(None));
    }

    #[test]
    fn status_text_explains_the_codes_an_operator_can_act_on() {
        assert_eq!(
            status_text(Some("410 Out of paper")),
            "410 Out of paper (out of paper)"
        );
        assert!(status_text(Some("521")).contains("too large"));
        assert_eq!(status_text(Some("999 whatever")), "999 whatever"); // unknown passes through
        assert_eq!(status_text(Some("")), "no status");
        assert_eq!(status_text(None), "no status");
        assert_eq!(
            status_text(Some("  420\tCover open ")),
            "420\tCover open (cover open)"
        );
    }

    #[test]
    fn device_name_is_stable_across_mac_spelling() {
        assert_eq!(
            device_name(" 00:11:62:0F:97:1A "),
            "cloudprnt-00:11:62:0f:97:1a"
        );
    }

    #[test]
    fn python_whitespace_u001c_to_u001f_counts_as_space() {
        assert!(job_ok(Some("\u{1c}OK")));
        assert_eq!(
            status_text(Some("410\u{1c}foo")),
            "410\u{1c}foo (out of paper)"
        );
        assert_eq!(device_name("\u{1c}AB\u{1f}"), "cloudprnt-ab");
        assert_eq!(media_type(Some("\u{1c}text/plain")), "text/plain");
    }
}
