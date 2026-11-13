// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Consent at signup: `PRINTAPI_TERMS_VERSION` makes `POST /signup` require `accept_terms`, and
//! `GET /health` tells the sign-up form which version and links to show.
mod common;

use common::*;
use printpapi_server::server::Terms;
use printpapi_server::store;
use serde_json::json;

fn terms() -> Terms {
    Terms {
        version: "2026-11-13".into(),
        terms_url: "https://example.test/terms".into(),
        dpa_url: Some("https://example.test/dpa".into()),
        privacy_url: None,
    }
}

fn app() -> App {
    App::with(|c| {
        c.signup_open = true;
        c.terms = Some(terms());
    })
}

#[test]
fn health_names_the_terms_only_when_consent_is_required() {
    let (_, h) = app().get("/health", None);
    assert_eq!(
        h["terms"],
        json!({"version": "2026-11-13", "terms_url": "https://example.test/terms",
               "dpa_url": "https://example.test/dpa", "privacy_url": null})
    );
    let (_, h) = App::with(|c| c.signup_open = true).get("/health", None);
    assert!(h.get("terms").is_none());
}

#[test]
fn signup_without_a_matching_acceptance_is_refused() {
    let app = app();
    let base = |extra: serde_json::Value| {
        let mut b = json!({"email": "a@shop.test", "password": PW});
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        b
    };
    for extra in [
        json!({}),
        json!({"accept_terms": false}),
        json!({"accept_terms": true}),
        json!({"accept_terms": 5}),
    ] {
        let (code, body) = app.post("/signup", None, base(extra.clone()));
        assert_eq!(code, 400, "{extra}");
        assert_eq!(body["error"], "accept_terms required");
    }
    let (code, body) = app.post("/signup", None, base(json!({"accept_terms": "2025-01-01"})));
    assert_eq!(
        (code, body["error"].as_str().unwrap()),
        (409, "terms changed, reload")
    );
    assert!(
        app.db
            .with(|c| store::get_user_by_email(c, "a@shop.test"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn signup_with_the_current_version_stores_the_acceptance() {
    let app = app();
    let (code, body) = app.post(
        "/signup",
        None,
        json!({"email": "a@shop.test", "password": PW, "accept_terms": "2026-11-13"}),
    );
    assert_eq!(code, 200, "{body}");
    let (org, user, version) = app.db.with(|c| {
        c.query_row(
            "SELECT org_id, user_id, version FROM terms_acceptances",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap()
    });
    assert_eq!(
        (org, user, version.as_str()),
        (
            body["org_id"].as_i64().unwrap(),
            body["user_id"].as_i64().unwrap(),
            "2026-11-13"
        )
    );
}

#[test]
fn without_the_setting_signup_ignores_accept_terms() {
    let app = App::with(|c| c.signup_open = true);
    let (code, _) = app.post(
        "/signup",
        None,
        json!({"email": "a@shop.test", "password": PW}),
    );
    assert_eq!(code, 200);
    let n: i64 = app.db.with(|c| {
        c.query_row("SELECT COUNT(*) FROM terms_acceptances", [], |r| r.get(0))
            .unwrap()
    });
    assert_eq!(n, 0);
}

#[test]
fn a_forgotten_checkbox_spends_no_signup_slot() {
    let app = App::with(|c| {
        c.signup_open = true;
        c.terms = Some(terms());
        c.max_login_fails = 1;
    });
    for _ in 0..3 {
        let (code, _) = app.post(
            "/signup",
            None,
            json!({"email": "a@shop.test", "password": PW}),
        );
        assert_eq!(code, 400);
    }
    let (code, _) = app.post(
        "/signup",
        None,
        json!({"email": "a@shop.test", "password": PW, "accept_terms": "2026-11-13"}),
    );
    assert_eq!(code, 200);
}
