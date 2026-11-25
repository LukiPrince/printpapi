// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Pairing in the store: one-time codes, the key they hand out, registering without touching the
//! device, the takeover at the first poll, the late result of the old machine and the purge. Real
//! SQLite in memory, times passed in.
mod common;

use common::*;
use printpapi_server::auth::{self, Scope};
use printpapi_server::store::{
    self, CreatePairing, DEFAULT_ORG, Db, LATE_RESULT_S, PAIRING_GRACE_S, PAIRING_MAX_OPEN,
    PAIRING_TTL_S, Redeem,
};
use rusqlite::params;
use serde_json::{Value, json};

const T: f64 = 1_000_000.0;
const CODE: &str = "7K3M-Q2ZD-9XHT";

fn code_n(n: u32) -> String {
    format!("{n:04X}-0000-0000")
}

/// A device "office-pc" in the default org with printers A (PDF) and L (label, raw only), enrolled
/// with the full key "old-key".
fn device(db: &Db) -> Registered {
    add_key(db, "office-pc", "old-key", DEFAULT_ORG);
    register(
        db,
        "office-pc",
        "old-key",
        &[decl("A", true), decl("L", false)],
        DEFAULT_ORG,
    )
}

fn create(db: &Db, org: i64, agent: Option<i64>, code: &str, now: f64) -> CreatePairing {
    db.with(|c| store::create_pairing(c, org, agent, None, code, Some(now)))
        .unwrap()
}

fn redeem(db: &Db, code: &str, name: &str, key: &str, now: f64) -> Redeem {
    db.with(|c| store::redeem_pairing(c, code, name, key, Some(now)))
        .unwrap()
}

fn reg_at(db: &Db, name: &str, key: &str, printers: &[store::PrinterDecl], now: f64) -> Value {
    let r = db
        .with(|c| store::register_agent_at(c, name, key, printers, DEFAULT_ORG, now))
        .unwrap();
    json!({"computer": r.computer_id, "printers": r.printer_ids})
}

fn sql_one<T: rusqlite::types::FromSql>(db: &Db, sql: &str) -> T {
    db.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
}

fn authenticates(db: &Db, key: &str) -> Option<i64> {
    db.with(|c| store::authenticate_agent(c, key)).unwrap()
}

fn take(db: &Db, key: &str, now: f64) -> Option<i64> {
    db.with(|c| store::take_over_if_due(c, key, Some(now)))
        .unwrap()
}

fn infos(db: &Db, now: f64) -> Vec<store::PairingInfo> {
    db.with(|c| store::list_pairings(c, None, Some(now)))
        .unwrap()
}

/// Reconnect "office-pc" with a new key "new-key", redeemed at `T`.
fn reconnect(db: &Db, dev: &Registered) {
    reconnect_at(db, dev, T);
}

fn reconnect_at(db: &Db, dev: &Registered, at: f64) {
    assert!(matches!(
        create(db, DEFAULT_ORG, Some(dev.computer_id), CODE, at),
        CreatePairing::Created { .. }
    ));
    assert!(matches!(
        redeem(db, CODE, "ignored", "new-key", at + 1.0),
        Redeem::Paired { replaces: true, .. }
    ));
}

fn printer_row(db: &Db, name: &str) -> (bool, Option<String>, String) {
    db.with(|c| {
        c.query_row(
            "SELECT can_pdf, capabilities, state FROM printers WHERE name=?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    })
    .unwrap()
}

// --- creating, redeeming, cancelling ------------------------------------------------------------

#[test]
fn create_stores_only_the_hash_of_the_dashless_code() {
    let db = mem();
    assert!(matches!(
        create(&db, DEFAULT_ORG, None, CODE, T),
        CreatePairing::Created { expires_at, .. } if expires_at == T + PAIRING_TTL_S
    ));
    let hash: String = sql_one(&db, "SELECT code_hash FROM pairing_codes");
    assert_eq!(hash, store::hash_key("7K3MQ2ZD9XHT"));
    let dump: String = sql_one(
        &db,
        "SELECT group_concat(coalesce(created_by,'') || code_hash, ',') FROM pairing_codes",
    );
    assert!(!dump.contains("7K3M") && !dump.contains("9XHT"));
}

#[test]
fn a_code_redeems_once_and_never_after_expiry_or_cancel() {
    let db = mem();
    create(&db, DEFAULT_ORG, None, CODE, T);
    // typed any way a person types it
    assert!(matches!(
        redeem(&db, "7k3m q2zd 9xht", "pc", "k1", T + 10.0),
        Redeem::Paired {
            replaces: false,
            ..
        }
    ));
    assert_eq!(redeem(&db, CODE, "pc2", "k2", T + 11.0), Redeem::BadCode);

    create(&db, DEFAULT_ORG, None, "AAAA-BBBB-CCCC", T);
    assert_eq!(
        redeem(&db, "AAAA-BBBB-CCCC", "pc3", "k3", T + PAIRING_TTL_S),
        Redeem::BadCode,
        "expired at the second it ends"
    );
    let CreatePairing::Created { id, .. } = create(&db, DEFAULT_ORG, None, "DDDD-EEEE-FFFF", T)
    else {
        panic!()
    };
    assert!(db.with(|c| store::cancel_pairing(c, id, None)).unwrap());
    assert_eq!(
        redeem(&db, "DDDD-EEEE-FFFF", "pc4", "k4", T + 1.0),
        Redeem::BadCode
    );
    assert!(!db.with(|c| store::cancel_pairing(c, id, None)).unwrap());
    // not a code at all
    assert_eq!(redeem(&db, "nonsense", "pc", "k5", T), Redeem::BadCode);
    assert_eq!(redeem(&db, "", "pc", "k5", T), Redeem::BadCode);
}

#[test]
fn cancel_is_org_scoped_and_refuses_a_used_code() {
    let db = mem();
    let other = create_org(&db, "Other");
    let CreatePairing::Created { id, .. } = create(&db, other, None, CODE, T) else {
        panic!()
    };
    assert!(
        !db.with(|c| store::cancel_pairing(c, id, Some(DEFAULT_ORG)))
            .unwrap()
    );
    redeem(&db, CODE, "pc", "k1", T + 1.0);
    assert!(
        !db.with(|c| store::cancel_pairing(c, id, Some(other)))
            .unwrap()
    );
    assert!(!db.with(|c| store::cancel_pairing(c, id, None)).unwrap());
}

#[test]
fn redeem_issues_an_agent_only_key_in_the_codes_org_and_forgets_the_creator() {
    let db = mem();
    let org = create_org(&db, "Acme");
    db.with(|c| store::create_pairing(c, org, None, Some("a@acme.example"), CODE, Some(T)))
        .unwrap();
    let got = redeem(&db, CODE, "warehouse-pc", "the-key", T + 5.0);
    assert_eq!(
        got,
        Redeem::Paired {
            name: "warehouse-pc".into(),
            org: "Acme".into(),
            created_by: Some("a@acme.example".into()),
            replaces: false,
        }
    );
    let (label, key_org, scope): (String, i64, String) = db
        .with(|c| {
            c.query_row(
                "SELECT label, org_id, scope FROM api_keys WHERE key_hash=?",
                [store::hash_key("the-key")],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!(
        (label.as_str(), key_org, scope.as_str()),
        ("Device warehouse-pc", org, "agent")
    );
    let creator: Option<String> = sql_one(&db, "SELECT created_by FROM pairing_codes");
    assert_eq!(creator, None);
    let key = db
        .with(|c| store::authenticate_client(c, "the-key"))
        .unwrap()
        .unwrap();
    assert_eq!((key.org_id, key.scope), (org, Scope::Agent));
}

#[test]
fn the_creator_is_a_copy_and_survives_a_deleted_user_root_has_none() {
    let db = mem();
    let uid = db
        .with(|c| store::create_user(c, DEFAULT_ORG, "gone@shop.test", "x"))
        .unwrap();
    db.with(|c| store::create_pairing(c, DEFAULT_ORG, None, Some("gone@shop.test"), CODE, Some(T)))
        .unwrap();
    db.with(|c| store::delete_user(c, uid, None)).unwrap();
    let Redeem::Paired { created_by, .. } = redeem(&db, CODE, "pc", "k", T + 1.0) else {
        panic!()
    };
    assert_eq!(created_by.as_deref(), Some("gone@shop.test"));
    create(&db, DEFAULT_ORG, None, "AAAA-BBBB-CCCC", T);
    let Redeem::Paired { created_by, .. } = redeem(&db, "AAAA-BBBB-CCCC", "pc2", "k2", T + 1.0)
    else {
        panic!()
    };
    assert_eq!(created_by, None);
}

#[test]
fn a_taken_name_is_refused_and_the_code_stays_usable() {
    let db = mem();
    let _ = device(&db);
    create(&db, DEFAULT_ORG, None, CODE, T);
    assert_eq!(
        redeem(&db, CODE, "office-pc", "k1", T + 1.0),
        Redeem::NameTaken
    );
    assert_eq!(
        sql_one::<i64>(&db, "SELECT COUNT(*) FROM api_keys"),
        1,
        "no key issued"
    );
    assert!(matches!(
        redeem(&db, CODE, "other-pc", "k1", T + 2.0),
        Redeem::Paired { .. }
    ));
}

#[test]
fn a_device_code_ignores_the_sent_name_and_returns_the_stored_one() {
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), CODE, T);
    let got = redeem(&db, CODE, "something else", "new-key", T + 1.0);
    assert!(matches!(&got, Redeem::Paired { name, replaces: true, .. } if name == "office-pc"));
    let label: String = sql_one(&db, "SELECT label FROM api_keys WHERE scope='agent'");
    assert_eq!(label, "Device office-pc");
    // a name that is taken does not matter for a device code, and it is no new device
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM agents"), 1);
}

#[test]
fn create_refuses_foreign_unknown_and_cloudprnt_devices() {
    let db = mem();
    let dev = device(&db);
    let other = create_org(&db, "Other");
    assert_eq!(
        create(&db, other, Some(dev.computer_id), CODE, T),
        CreatePairing::UnknownDevice,
        "a foreign device reads like a missing one"
    );
    assert_eq!(
        create(&db, DEFAULT_ORG, Some(999), CODE, T),
        CreatePairing::UnknownDevice
    );
    let star = db
        .with(|c| store::register_cloudprnt(c, DEFAULT_ORG, "cloudprnt-star"))
        .unwrap();
    assert_eq!(
        create(&db, DEFAULT_ORG, Some(star.agent_id), CODE, T),
        CreatePairing::Cloudprnt
    );
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"), 0);
}

#[test]
fn a_second_code_for_a_device_replaces_the_first_unused_one() {
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), CODE, T);
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "AAAA-BBBB-CCCC",
        T + 1.0,
    );
    assert_eq!(redeem(&db, CODE, "x", "k1", T + 2.0), Redeem::BadCode);
    assert!(matches!(
        redeem(&db, "AAAA-BBBB-CCCC", "x", "k2", T + 2.0),
        Redeem::Paired { .. }
    ));
}

#[test]
fn the_twenty_first_open_code_is_refused_per_org() {
    let db = mem();
    let other = create_org(&db, "Other");
    for n in 0..PAIRING_MAX_OPEN as u32 {
        assert!(matches!(
            create(&db, DEFAULT_ORG, None, &code_n(n), T),
            CreatePairing::Created { .. }
        ));
    }
    assert_eq!(
        create(&db, DEFAULT_ORG, None, &code_n(99), T),
        CreatePairing::TooMany
    );
    assert!(matches!(
        create(&db, other, None, &code_n(100), T),
        CreatePairing::Created { .. }
    ));
    // an expired code does not count
    assert!(matches!(
        create(
            &db,
            DEFAULT_ORG,
            None,
            &code_n(101),
            T + PAIRING_TTL_S + 1.0
        ),
        CreatePairing::Created { .. }
    ));
}

// --- register without touching the device -------------------------------------------------------

#[test]
fn a_reconnect_key_registers_without_changing_the_device() {
    let db = mem();
    let dev = device(&db);
    // the old agent ran with a caps JSON on L
    db.with(|c| {
        c.execute(
            "UPDATE printers SET capabilities='{\"zpl\": 1}' WHERE name='L'",
            [],
        )
    })
    .unwrap();
    let seen: f64 = sql_one(&db, "SELECT last_seen_at FROM agents");
    reconnect(&db, &dev);

    let declared = [decl("A", false), decl("L", true), decl("Z", true)];
    let r = reg_at(&db, "office-pc", "new-key", &declared, T + 100.0);
    assert_eq!(r["computer"], dev.computer_id);
    for name in ["A", "L", "Z"] {
        assert!(r["printers"][name].as_i64().is_some(), "{name}");
    }
    assert_eq!(r["printers"]["A"], dev.pid("A"));
    assert_eq!(r["printers"]["L"], dev.pid("L"));
    // the device is untouched
    assert_eq!(sql_one::<f64>(&db, "SELECT last_seen_at FROM agents"), seen);
    assert_eq!(authenticates(&db, "old-key"), Some(dev.computer_id));
    assert_eq!(authenticates(&db, "new-key"), None);
    assert_eq!(printer_row(&db, "A"), (true, None, "active".into()));
    assert!(
        !printer_row(&db, "L").0,
        "an early can_pdf would send a PDF to a label printer"
    );
    assert_eq!(printer_row(&db, "L").1.as_deref(), Some("{\"zpl\": 1}"));
    // a new printer exists, but nothing may be queued for it yet
    assert_eq!(printer_row(&db, "Z").2, "pending");
    // twice is the same
    assert_eq!(reg_at(&db, "office-pc", "new-key", &declared, T + 200.0), r);
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM printers"), 3);
}

#[test]
fn a_job_for_a_pending_printer_is_refused_until_the_swap() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    let r = reg_at(&db, "office-pc", "new-key", &[decl("Z", false)], T + 100.0);
    let zid = r["printers"]["Z"].as_i64().unwrap();
    let job = |db: &Db| {
        let pid = json!(zid);
        db.with(|c| {
            store::enqueue_job(
                c,
                &store::NewJob {
                    printer_id: &pid,
                    type_: "raw_base64",
                    mode: "raw",
                    payload: b"x",
                    user_id: store::DEFAULT_USER,
                    title: None,
                    copies: 1,
                    callback_url: None,
                    options: None,
                    org_id: None,
                    idempotency_key: None,
                    expire_after: None,
                    api_key_id: None,
                },
            )
        })
    };
    assert!(matches!(
        job(&db),
        Err(store::StoreError::UnknownPrinter(_))
    ));
    assert_eq!(take(&db, "new-key", T + 200.0), Some(dev.computer_id));
    assert_eq!(printer_row(&db, "Z").2, "active");
    assert!(job(&db).is_ok());
}

#[test]
fn without_a_pairing_row_register_refuses_a_foreign_key_as_ever() {
    let db = mem();
    let _ = device(&db);
    add_key(&db, "stranger", "stranger-key", DEFAULT_ORG);
    let e = db
        .with(|c| store::register_agent_at(c, "office-pc", "stranger-key", &[], DEFAULT_ORG, T))
        .unwrap_err();
    assert!(matches!(e, store::StoreError::Auth(_)));
}

#[test]
fn a_plain_new_device_key_cannot_register_over_an_existing_name() {
    let db = mem();
    let _ = device(&db);
    create(&db, DEFAULT_ORG, None, CODE, T);
    redeem(&db, CODE, "fresh-pc", "fresh-key", T + 1.0);
    // a key fetched for the new device "fresh-pc" says "office-pc": no right, no takeover
    let e = db
        .with(|c| store::register_agent_at(c, "office-pc", "fresh-key", &[], DEFAULT_ORG, T + 2.0))
        .unwrap_err();
    assert!(matches!(e, store::StoreError::Auth(_)));
    assert_eq!(take(&db, "fresh-key", T + 3.0), None);
}

// --- the swap -----------------------------------------------------------------------------------

#[test]
fn the_swap_keeps_ids_applies_flags_and_ends_the_old_key() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    // the old key is an unscoped (full) key: it keeps working for integrations
    reg_at(
        &db,
        "office-pc",
        "new-key",
        &[decl("A", false), decl("L", true)],
        T + 100.0,
    );
    assert_eq!(take(&db, "new-key", T + 200.0), Some(dev.computer_id));
    assert_eq!(authenticates(&db, "new-key"), Some(dev.computer_id));
    assert_eq!(
        authenticates(&db, "old-key"),
        None,
        "no longer polls as this device"
    );
    assert!(!printer_row(&db, "A").0);
    assert!(printer_row(&db, "L").0);
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM agents"), 1);
    assert_eq!(
        sql_one::<f64>(&db, "SELECT last_seen_at FROM agents"),
        T + 200.0
    );
    // a second call does nothing
    assert_eq!(take(&db, "new-key", T + 300.0), None);
    // the pairing row says so and drops the declared list
    let printers: Option<String> = sql_one(&db, "SELECT printers FROM pairing_codes");
    assert_eq!(printers, None);
}

#[test]
fn an_agent_only_old_key_is_revoked_at_the_swap_any_other_stays_and_is_listed() {
    // unscoped old key
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    let before = &infos(&db, T + 2.0)[0];
    assert_eq!(
        before.old_key.as_ref().map(|k| k.label.as_str()),
        Some("office-pc")
    );
    reg_at(&db, "office-pc", "new-key", &[], T + 3.0);
    take(&db, "new-key", T + 4.0);
    assert_eq!(
        sql_one::<i64>(&db, "SELECT active FROM api_keys WHERE label='office-pc'"),
        1
    );
    let after = &infos(&db, T + 5.0)[0];
    assert_eq!(after.state, "connected");
    assert_eq!(
        after.old_key.as_ref().map(|k| k.label.as_str()),
        Some("office-pc")
    );
    // revoked, it is not listed any more
    db.with(|c| c.execute("UPDATE api_keys SET active=0 WHERE label='office-pc'", []))
        .unwrap();
    assert_eq!(infos(&db, T + 6.0)[0].old_key, None);

    // an agent-only old key (a device that was paired before)
    let db = mem();
    create(&db, DEFAULT_ORG, None, CODE, T);
    redeem(&db, CODE, "pc", "first-key", T + 1.0);
    let reg = reg_at(&db, "pc", "first-key", &[decl("A", false)], T + 2.0);
    let agent = reg["computer"].as_i64().unwrap();
    create(&db, DEFAULT_ORG, Some(agent), "AAAA-BBBB-CCCC", T + 10.0);
    assert_eq!(
        infos(&db, T + 11.0)[0].old_key,
        None,
        "agent-only: nothing to offer"
    );
    redeem(&db, "AAAA-BBBB-CCCC", "x", "second-key", T + 11.0);
    reg_at(&db, "pc", "second-key", &[decl("A", false)], T + 12.0);
    assert_eq!(take(&db, "second-key", T + 13.0), Some(agent));
    assert_eq!(authenticates(&db, "first-key"), None);
    assert_eq!(
        sql_one::<i64>(
            &db,
            "SELECT active FROM api_keys WHERE label='Device pc' AND id=1"
        ),
        0
    );
    assert_eq!(infos(&db, T + 14.0)[0].old_key, None);
}

#[test]
fn take_over_without_a_right_writes_nothing() {
    let db = mem();
    let dev = device(&db);
    add_key(&db, "plain", "plain-key", DEFAULT_ORG);
    let snapshot = |db: &Db| -> String {
        db.with(|c| {
            c.query_row(
                "SELECT (SELECT group_concat(api_key_hash || last_seen_at) FROM agents) || \
                 (SELECT count(*) FROM pairing_codes) || (SELECT count(*) FROM printers)",
                [],
                |r| r.get(0),
            )
        })
        .unwrap()
    };
    let before = snapshot(&db);
    for key in ["plain-key", "nothing", "", "old-key"] {
        assert_eq!(take(&db, key, T), None, "{key:?}");
    }
    assert_eq!(snapshot(&db), before);
    let _ = dev;
}

#[test]
fn no_takeover_for_a_key_that_never_registered_or_is_revoked_or_narrowed() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    // fetched but not registered: nothing to swap yet
    assert_eq!(take(&db, "new-key", T + 5.0), None);
    reg_at(&db, "office-pc", "new-key", &[], T + 6.0);
    // narrowed to a scope without the agent right
    let kid: i64 = sql_one(&db, "SELECT id FROM api_keys WHERE scope='agent'");
    assert!(
        db.with(|c| store::set_api_key_scope(c, kid, None, Scope::Read))
            .unwrap()
    );
    assert_eq!(take(&db, "new-key", T + 7.0), None);
    assert_eq!(authenticates(&db, "old-key"), Some(dev.computer_id));
    // widened to full it still takes over: the right to act as an agent is what counts
    assert!(
        db.with(|c| store::set_api_key_scope(c, kid, None, Scope::Full))
            .unwrap()
    );
    // revoked
    db.with(|c| c.execute("UPDATE api_keys SET active=0 WHERE id=?", [kid]))
        .unwrap();
    assert_eq!(take(&db, "new-key", T + 8.0), None);
    db.with(|c| c.execute("UPDATE api_keys SET active=1 WHERE id=?", [kid]))
        .unwrap();
    assert_eq!(take(&db, "new-key", T + 9.0), Some(dev.computer_id));
}

#[test]
fn a_key_that_holds_another_device_takes_nothing_over() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    reg_at(&db, "office-pc", "new-key", &[], T + 6.0);
    // the same key becomes the key of another device (the UNIQUE hash would refuse a swap)
    db.with(|c| {
        c.execute(
            "INSERT INTO agents(org_id, name, api_key_hash, created_at) VALUES(1,'other',?,0)",
            [store::hash_key("new-key")],
        )
    })
    .unwrap();
    assert_eq!(take(&db, "new-key", T + 7.0), None);
}

#[test]
fn a_fetched_key_older_than_24_hours_cannot_register_but_a_registered_one_swaps_after_a_week() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    let late = T + 1.0 + PAIRING_GRACE_S + 1.0;
    let e = db
        .with(|c| store::register_agent_at(c, "office-pc", "new-key", &[], DEFAULT_ORG, late))
        .unwrap_err();
    assert!(matches!(e, store::StoreError::Auth(_)));
    // registered in time, restarted a week later
    reg_at(&db, "office-pc", "new-key", &[], T + 100.0);
    let week = T + 7.0 * 86_400.0;
    db.with(|c| store::purge_pairings(c, Some(week))).unwrap();
    assert_eq!(take(&db, "new-key", week), Some(dev.computer_id));
}

#[test]
fn a_newer_code_survives_the_swap_of_an_older_one_and_the_reverse_ends_the_older() {
    // code 1 redeemed, code 2 created, key 1 swaps: code 2 still redeems, registers and swaps
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), "1111-1111-1111", T);
    redeem(&db, "1111-1111-1111", "x", "key-1", T + 1.0);
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 2.0,
    );
    reg_at(&db, "office-pc", "key-1", &[], T + 3.0);
    assert_eq!(take(&db, "key-1", T + 4.0), Some(dev.computer_id));
    assert!(matches!(
        redeem(&db, "2222-2222-2222", "x", "key-2", T + 5.0),
        Redeem::Paired { .. }
    ));
    reg_at(&db, "office-pc", "key-2", &[], T + 6.0);
    assert_eq!(take(&db, "key-2", T + 7.0), Some(dev.computer_id));
    assert_eq!(authenticates(&db, "key-2"), Some(dev.computer_id));
    assert_eq!(authenticates(&db, "key-1"), None);

    // code 2's key first: key 1 is revoked and cannot take the device back
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), "1111-1111-1111", T);
    redeem(&db, "1111-1111-1111", "x", "key-1", T + 1.0);
    reg_at(&db, "office-pc", "key-1", &[], T + 2.0);
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 3.0,
    );
    redeem(&db, "2222-2222-2222", "x", "key-2", T + 4.0);
    reg_at(&db, "office-pc", "key-2", &[], T + 5.0);
    assert_eq!(take(&db, "key-2", T + 6.0), Some(dev.computer_id));
    assert_eq!(take(&db, "key-1", T + 7.0), None);
    assert_eq!(authenticates(&db, "key-1"), None);
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"), 1);
    assert_eq!(
        sql_one::<i64>(
            &db,
            &format!(
                "SELECT active FROM api_keys WHERE key_hash='{}'",
                store::hash_key("key-1")
            )
        ),
        0,
        "the older key was revoked with its row"
    );
}

#[test]
fn the_swap_moves_claimed_jobs_so_the_reaper_waits_for_the_late_result() {
    let db = mem();
    let dev = device(&db);
    let job = enqueue(&db, dev.pid("A"), b"x", |_| {});
    claim(&db, dev.computer_id).unwrap();
    let t = store_now();
    reconnect_at(&db, &dev, t - 10.0);
    reg_at(&db, "office-pc", "new-key", &[], t);
    take(&db, "new-key", t + 50.0);
    let claimed: f64 = db
        .with(|c| {
            c.query_row("SELECT claimed_at FROM jobs WHERE id=?", [job], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(claimed, t + 50.0);
    // the reaper's rule: nothing is requeued before 300 s after the swap
    assert_eq!(
        db.with(|c| store::requeue_stale(c, 300.0, 2, Some(t + 50.0 + 299.0), false))
            .unwrap(),
        0
    );
    assert_eq!(
        db.with(|c| store::requeue_stale(c, 300.0, 2, Some(t + 50.0 + 301.0), false))
            .unwrap(),
        1
    );
}

fn store_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// --- the late result ----------------------------------------------------------------------------

/// A device that claimed a job with "old-key", then was taken over at `swap`.
fn swapped(swap: f64) -> (Db, Registered, i64) {
    let db = mem();
    let dev = device(&db);
    let job = enqueue(&db, dev.pid("A"), b"x", |_| {});
    claim(&db, dev.computer_id).unwrap();
    reconnect_at(&db, &dev, swap - 10.0);
    reg_at(&db, "office-pc", "new-key", &[], swap - 1.0);
    take(&db, "new-key", swap);
    (db, dev, job)
}

#[test]
fn a_late_result_is_for_the_old_key_a_job_claimed_before_the_swap_within_300_seconds() {
    let swap = store_now();
    let (db, dev, job) = swapped(swap);
    let late = |key: &str, id: i64, now: f64| {
        db.with(|c| store::late_result_agent(c, key, id, Some(now)))
            .unwrap()
    };
    assert!(
        db.with(|c| store::late_result_open(c, "old-key", Some(swap + 10.0)))
            .unwrap()
    );
    assert_eq!(late("old-key", job, swap + 10.0), Some(dev.computer_id));
    assert_eq!(
        late("old-key", job, swap + LATE_RESULT_S + 1.0),
        None,
        "too late"
    );
    assert!(
        !db.with(|c| store::late_result_open(c, "old-key", Some(swap + 301.0)))
            .unwrap()
    );
    assert_eq!(
        late("new-key", job, swap + 10.0),
        None,
        "not the key that was given up"
    );
    assert_eq!(late("stranger", job, swap + 10.0), None);
    assert!(
        !db.with(|c| store::late_result_open(c, "stranger", Some(swap + 10.0)))
            .unwrap()
    );
    assert_eq!(late("old-key", job + 1, swap + 10.0), None, "no such job");

    // a job the new machine claimed after the swap is not the old one's to finish
    let job2 = enqueue(&db, dev.pid("A"), b"y", |_| {});
    let c = claim(&db, dev.computer_id).unwrap();
    assert_eq!(c.job_id, job2);
    assert_eq!(late("old-key", job2, swap + 10.0), None);
    // nor one that went back to the queue
    db.with(|c| c.execute("UPDATE jobs SET state='queued' WHERE id=?", [job]))
        .unwrap();
    assert_eq!(late("old-key", job, swap + 10.0), None);
}

// --- purge --------------------------------------------------------------------------------------

#[test]
fn purge_revokes_a_never_registered_key_after_24_hours_and_keeps_the_rest() {
    let db = mem();
    let dev = device(&db);
    // never registered, new device
    create(&db, DEFAULT_ORG, None, "1111-1111-1111", T);
    redeem(&db, "1111-1111-1111", "never", "k-never", T + 1.0);
    // registered, reconnect, not swapped
    create(&db, DEFAULT_ORG, Some(dev.computer_id), "2222-2222-2222", T);
    redeem(&db, "2222-2222-2222", "x", "k-pending", T + 1.0);
    reg_at(&db, "office-pc", "k-pending", &[], T + 2.0);
    // an unused code
    create(&db, DEFAULT_ORG, None, "3333-3333-3333", T);

    db.with(|c| store::purge_pairings(c, Some(T + 1.0 + PAIRING_GRACE_S - 1.0)))
        .unwrap();
    assert_eq!(
        sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"),
        3,
        "nothing yet"
    );
    assert_eq!(
        sql_one::<i64>(
            &db,
            "SELECT active FROM api_keys WHERE label='Device never'"
        ),
        1
    );
    let t = T + 1.0 + PAIRING_GRACE_S + 5.0;
    db.with(|c| store::purge_pairings(c, Some(t))).unwrap();
    let active = |label: &str| -> i64 {
        db.with(|c| {
            c.query_row("SELECT active FROM api_keys WHERE label=?", [label], |r| {
                r.get(0)
            })
        })
        .unwrap()
    };
    assert_eq!(active("Device never"), 0, "a key nobody registered ends");
    assert_eq!(
        active("Device office-pc"),
        1,
        "a registered key waits for its poll"
    );
    let left: Vec<String> = db
        .with(|c| {
            c.prepare("SELECT coalesce(device_name,'-') FROM pairing_codes ORDER BY id")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()
        })
        .unwrap();
    // the revoked row is gone; the unused code goes when it has expired for a day
    assert_eq!(left, ["office-pc", "-"], "the pending swap stays");
    // a connected row goes 24 h after its takeover, the key lives on
    assert_eq!(take(&db, "k-pending", t), Some(dev.computer_id));
    db.with(|c| store::purge_pairings(c, Some(t + PAIRING_GRACE_S - 1.0)))
        .unwrap();
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"), 1);
    assert_eq!(
        sql_one::<i64>(
            &db,
            "SELECT COUNT(*) FROM pairing_codes WHERE taken_over_at IS NOT NULL"
        ),
        1
    );
    db.with(|c| store::purge_pairings(c, Some(t + PAIRING_GRACE_S + 1.0)))
        .unwrap();
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"), 0);
    assert_eq!(authenticates(&db, "k-pending"), Some(dev.computer_id));
}

#[test]
fn the_clean_up_of_pending_printers_reads_the_waiting_devices_once() {
    // a check per pending printer would scan the whole pairing table for each of them (50,000
    // printers and 10,000 rows took 13 s under the one database lock): no correlated subquery
    let db = mem();
    let plan = db
        .with(store::explain_drop_orphan_pending_printers)
        .unwrap()
        .join("\n");
    assert!(!plan.contains("CORRELATED"), "{plan}");
}

#[test]
fn purge_leaves_a_key_that_an_agent_already_holds() {
    let db = mem();
    create(&db, DEFAULT_ORG, None, CODE, T);
    redeem(&db, CODE, "pc", "k", T + 1.0);
    // registered by a path that does not mark the row (a key form after a failed pairing)
    db.with(|c| store::register_agent_at(c, "pc", "k", &[], DEFAULT_ORG, T + 2.0))
        .unwrap();
    db.with(|c| store::purge_pairings(c, Some(T + 2.0 * PAIRING_GRACE_S)))
        .unwrap();
    assert!(authenticates(&db, "k").is_some());
}

// --- list ---------------------------------------------------------------------------------------

#[test]
fn list_shows_the_six_states_and_no_hash_or_creator() {
    let db = mem();
    let dev = device(&db);
    let with_creator = |code: &str, agent: Option<i64>, now: f64| {
        db.with(|c| {
            store::create_pairing(
                c,
                DEFAULT_ORG,
                agent,
                Some("boss@shop.test"),
                code,
                Some(now),
            )
        })
        .unwrap();
    };
    // expired, open
    with_creator("1111-1111-1111", None, T - 5000.0);
    // fetched
    with_creator("2222-2222-2222", None, T);
    redeem(&db, "2222-2222-2222", "fetched-pc", "k-fetched", T + 1.0);
    // registered
    with_creator("3333-3333-3333", Some(dev.computer_id), T + 2.0);
    redeem(&db, "3333-3333-3333", "x", "k-registered", T + 3.0);
    reg_at(&db, "office-pc", "k-registered", &[], T + 4.0);
    // revoked
    with_creator("4444-4444-4444", None, T + 5.0);
    redeem(&db, "4444-4444-4444", "revoked-pc", "k-revoked", T + 6.0);
    db.with(|c| {
        c.execute(
            "UPDATE api_keys SET active=0 WHERE label='Device revoked-pc'",
            [],
        )
    })
    .unwrap();
    // connected: a device that registered with its key
    with_creator("5555-5555-5555", None, T + 7.0);
    redeem(&db, "5555-5555-5555", "bound-pc", "k-bound", T + 8.0);
    reg_at(&db, "bound-pc", "k-bound", &[], T + 9.0);
    // open
    with_creator("6666-6666-6666", None, T + 10.0);

    let states: Vec<(Option<String>, &str)> = infos(&db, T + 20.0)
        .into_iter()
        .map(|i| (i.device_name, i.state))
        .collect();
    assert_eq!(
        states,
        [
            (None, "open"),
            (Some("bound-pc".into()), "connected"),
            (Some("revoked-pc".into()), "revoked"),
            (Some("office-pc".into()), "registered"),
            (Some("fetched-pc".into()), "fetched"),
            (None, "expired"),
        ]
    );
    let json = serde_json::to_string(&infos(&db, T + 20.0)).unwrap();
    for needle in ["boss@shop.test", "created_by", "hash", "k-fetched"] {
        assert!(!json.contains(needle), "{needle}");
    }
    // org scoped
    let other = create_org(&db, "Other");
    assert!(
        db.with(|c| store::list_pairings(c, Some(other), Some(T)))
            .unwrap()
            .is_empty()
    );
    let first = infos(&db, T + 20.0)[0].id;
    assert!(
        db.with(|c| store::get_pairing(c, first, Some(other), Some(T)))
            .unwrap()
            .is_none()
    );
    assert!(
        db.with(|c| store::get_pairing(c, first, Some(DEFAULT_ORG), Some(T)))
            .unwrap()
            .is_some()
    );
}

#[test]
fn list_is_newest_first_and_stops_at_100() {
    let db = mem();
    // direct inserts: the open limit is per org and not what is tested here
    for n in 0..105 {
        db.with(|c| {
            c.execute(
                "INSERT INTO pairing_codes(org_id, code_hash, created_at, expires_at) VALUES(1,?,0,1)",
                [format!("h{n}")],
            )
        })
        .unwrap();
    }
    let list = infos(&db, T);
    assert_eq!(list.len(), 100);
    assert!(list[0].id > list[99].id);
}

#[test]
fn delete_org_removes_its_pairing_codes() {
    let db = mem();
    let org = create_org(&db, "Gone");
    create(&db, org, None, CODE, T);
    create(&db, DEFAULT_ORG, None, "AAAA-BBBB-CCCC", T);
    assert!(db.with(|c| store::delete_org(c, org)).unwrap());
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"), 1);
}

#[test]
fn the_code_alphabet_of_the_server_is_what_it_hashes() {
    let db = mem();
    let code = auth::new_pairing_code();
    create(&db, DEFAULT_ORG, None, &code, T);
    assert!(matches!(
        redeem(&db, &code.to_lowercase(), "pc", "k", T + 1.0),
        Redeem::Paired { .. }
    ));
}

// --- review findings ----------------------------------------------------------------------------

#[test]
fn a_reconnect_key_cannot_enrol_a_new_name_and_is_never_shown_as_connected() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    // the new machine kept its own name in agent.ini: refused, no second device
    let e = db
        .with(|c| store::register_agent_at(c, "new-pc", "new-key", &[], DEFAULT_ORG, T + 5.0))
        .unwrap_err();
    assert!(matches!(e, store::StoreError::Auth(_)), "{e:?}");
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM agents"), 1);
    assert_eq!(infos(&db, T + 6.0)[0].state, "fetched");
    // registered under the device's name first, then under another one: still refused, still pending
    reg_at(&db, "office-pc", "new-key", &[], T + 7.0);
    assert!(
        db.with(|c| store::register_agent_at(c, "new-pc", "new-key", &[], DEFAULT_ORG, T + 8.0))
            .is_err()
    );
    let row = &infos(&db, T + 9.0)[0];
    assert_eq!(row.state, "registered");
    assert_eq!(authenticates(&db, "old-key"), Some(dev.computer_id));
    // a device other than the one being reconnected that holds the key does not make it "connected"
    db.with(|c| {
        c.execute(
            "INSERT INTO agents(org_id, name, api_key_hash, created_at) VALUES(1,'other',?,0)",
            [store::hash_key("new-key")],
        )
    })
    .unwrap();
    assert_ne!(infos(&db, T + 9.0)[0].state, "connected");
}

#[test]
fn a_takeover_stays_connected_when_its_key_ends_and_the_old_full_key_is_still_offered() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    reg_at(&db, "office-pc", "new-key", &[], T + 5.0);
    take(&db, "new-key", T + 10.0);
    // reconnected again within the day: the second swap revokes the agent-only first key
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 20.0,
    );
    redeem(&db, "2222-2222-2222", "x", "k2", T + 21.0);
    reg_at(&db, "office-pc", "k2", &[], T + 22.0);
    take(&db, "k2", T + 30.0);
    assert_eq!(authenticates(&db, "new-key"), None);
    let rows = infos(&db, T + 40.0);
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.state, "connected");
    }
    // row 1 still names the full key of the device that works
    assert_eq!(
        rows[1].old_key.as_ref().map(|k| k.label.as_str()),
        Some("office-pc")
    );
    assert!(rows[0].old_key.is_none());
}

#[test]
fn a_late_result_survives_a_second_takeover_of_the_same_device() {
    let swap = store_now();
    let (db, dev, job) = swapped(swap);
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        swap + 5.0,
    );
    redeem(&db, "2222-2222-2222", "x", "k2", swap + 6.0);
    reg_at(&db, "office-pc", "k2", &[], swap + 7.0);
    take(&db, "k2", swap + 20.0);
    let late = db
        .with(|c| store::late_result_agent(c, "old-key", job, Some(swap + 50.0)))
        .unwrap();
    assert_eq!(
        late,
        Some(dev.computer_id),
        "inside the 300 s of its own swap"
    );
    // and the key of the first takeover is accepted for its own window as well
    let late = db
        .with(|c| store::late_result_agent(c, "new-key", job, Some(swap + 50.0)))
        .unwrap();
    assert_eq!(late, Some(dev.computer_id));
}

#[test]
fn the_swap_of_an_older_code_keeps_the_pending_printers_of_a_newer_registered_code() {
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), "1111-1111-1111", T);
    redeem(&db, "1111-1111-1111", "x", "key-1", T + 1.0);
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 2.0,
    );
    redeem(&db, "2222-2222-2222", "x", "key-2", T + 3.0);
    let a = decl("A", true);
    reg_at(
        &db,
        "office-pc",
        "key-1",
        &[a.clone(), decl("New1", false)],
        T + 4.0,
    );
    let second = reg_at(
        &db,
        "office-pc",
        "key-2",
        &[a, decl("New2", false)],
        T + 5.0,
    );
    let new2 = second["printers"]["New2"].as_i64().unwrap();

    // key-1 polls first: its own printer is a printer now, the one key-2 told the server about stays
    assert_eq!(take(&db, "key-1", T + 6.0), Some(dev.computer_id));
    assert_eq!(printer_row(&db, "New1").2, "active");
    assert_eq!(printer_row(&db, "New2").2, "pending");
    assert_eq!(
        sql_one::<i64>(&db, "SELECT id FROM printers WHERE name='New2'"),
        new2
    );

    // key-2 swaps: the printer keeps the id its agent was told, and nothing stays pending
    assert_eq!(take(&db, "key-2", T + 7.0), Some(dev.computer_id));
    assert_eq!(printer_row(&db, "New2").2, "active");
    assert_eq!(
        sql_one::<i64>(&db, "SELECT id FROM printers WHERE name='New2'"),
        new2
    );
    assert_eq!(ghost_count(&db), 0);
}

#[test]
fn after_a_second_swap_the_first_key_is_still_refused_a_job_the_second_key_claimed() {
    let swap = store_now();
    let (db, dev, old_job) = swapped(swap);
    // the second key's machine takes a job after the first swap
    let new_job = enqueue(&db, dev.pid("A"), b"y", |_| {});
    assert_eq!(claim(&db, dev.computer_id).unwrap().job_id, new_job);
    // a third key swaps in: every claimed job of the device moves to this time
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        swap + 5.0,
    );
    redeem(&db, "2222-2222-2222", "x", "key-3", swap + 6.0);
    reg_at(&db, "office-pc", "key-3", &[], swap + 7.0);
    assert_eq!(take(&db, "key-3", swap + 20.0), Some(dev.computer_id));
    let late = |key: &str, id: i64| {
        db.with(|c| store::late_result_agent(c, key, id, Some(swap + 25.0)))
            .unwrap()
    };
    assert_eq!(
        late("old-key", old_job),
        Some(dev.computer_id),
        "it had this one"
    );
    assert_eq!(late("old-key", new_job), None, "it never had this one");
    assert_eq!(late("new-key", new_job), Some(dev.computer_id));
}

fn ghost_count(db: &Db) -> i64 {
    sql_one(db, "SELECT COUNT(*) FROM printers WHERE state='pending'")
}

#[test]
fn the_swap_removes_a_pending_printer_the_last_declaration_no_longer_names() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    let a = decl("A", true);
    reg_at(
        &db,
        "office-pc",
        "new-key",
        &[a.clone(), decl("Ghost", false)],
        T + 2.0,
    );
    assert_eq!(printer_row(&db, "Ghost").2, "pending");
    // the agent registers again without it: the row stays pending until the swap
    reg_at(&db, "office-pc", "new-key", &[a], T + 3.0);
    assert_eq!(ghost_count(&db), 1);
    assert_eq!(take(&db, "new-key", T + 4.0), Some(dev.computer_id));
    assert_eq!(ghost_count(&db), 0, "nothing could ever be queued for it");
}

#[test]
fn a_pending_printer_stays_while_a_newer_registered_code_waits_to_swap() {
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), "1111-1111-1111", T);
    redeem(&db, "1111-1111-1111", "x", "key-1", T + 1.0);
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 2.0,
    );
    redeem(&db, "2222-2222-2222", "x", "key-2", T + 3.0);
    let a = decl("A", true);
    reg_at(
        &db,
        "office-pc",
        "key-1",
        &[a.clone(), decl("Ghost", false)],
        T + 4.0,
    );
    reg_at(&db, "office-pc", "key-1", std::slice::from_ref(&a), T + 5.0);
    reg_at(&db, "office-pc", "key-2", &[a], T + 6.0);
    // the older code swaps: the newer one may still declare what is pending, so it stays
    assert_eq!(take(&db, "key-1", T + 7.0), Some(dev.computer_id));
    assert_eq!(printer_row(&db, "Ghost").2, "pending");
    // the newer one swaps and names nothing pending: now it goes
    assert_eq!(take(&db, "key-2", T + 8.0), Some(dev.computer_id));
    assert_eq!(ghost_count(&db), 0);
}

#[test]
fn printers_of_a_takeover_that_ends_without_a_swap_go_with_it() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    let declared = [
        decl("A", true),
        decl("Ghost 1", true),
        decl("Ghost 2", true),
    ];
    reg_at(&db, "office-pc", "new-key", &declared, T + 5.0);
    assert_eq!(ghost_count(&db), 2);
    // revoking the key ends the right: the ghosts go at once
    let key_id: i64 = sql_one(
        &db,
        "SELECT id FROM api_keys WHERE label='Device office-pc'",
    );
    assert!(db.with(|c| store::revoke_api_key(c, key_id, None)).unwrap());
    assert_eq!(ghost_count(&db), 0);
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM printers"), 2);

    // a second code whose key is ended some other way: the purge cleans up
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 10.0,
    );
    redeem(&db, "2222-2222-2222", "x", "k2", T + 11.0);
    reg_at(&db, "office-pc", "k2", &[decl("Ghost 3", false)], T + 12.0);
    assert_eq!(ghost_count(&db), 1);
    db.with(|c| {
        c.execute(
            "UPDATE api_keys SET active=0 WHERE label='Device office-pc'",
            [],
        )
    })
    .unwrap();
    db.with(|c| store::purge_pairings(c, Some(T + 13.0)))
        .unwrap();
    assert_eq!(ghost_count(&db), 0);
    // a live right keeps its printers through a purge
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "3333-3333-3333",
        T + 20.0,
    );
    redeem(&db, "3333-3333-3333", "x", "k3", T + 21.0);
    reg_at(&db, "office-pc", "k3", &[decl("Ghost 4", false)], T + 22.0);
    db.with(|c| store::purge_pairings(c, Some(T + 23.0)))
        .unwrap();
    assert_eq!(ghost_count(&db), 1);
}

#[test]
fn purge_keeps_a_paired_key_that_was_widened_and_registered_once() {
    let db = mem();
    let dev = device(&db);
    create(&db, DEFAULT_ORG, None, CODE, T);
    redeem(&db, CODE, "pc", "k1", T + 1.0);
    reg_at(&db, "pc", "k1", &[], T + 2.0);
    // an integration uses it: the manager widens it to full
    db.with(|c| {
        c.execute(
            "UPDATE api_keys SET scope='full' WHERE label='Device pc'",
            [],
        )
    })
    .unwrap();
    // the device is reconnected and k2 takes over: k1 is no agent's key any more, and stays active
    create(
        &db,
        DEFAULT_ORG,
        Some(dev.computer_id),
        "2222-2222-2222",
        T + 10.0,
    );
    redeem(&db, "2222-2222-2222", "x", "k2", T + 11.0);
    reg_at(&db, "office-pc", "k2", &[], T + 12.0);
    // k1 holds "pc", not office-pc: swap the pc device away instead
    db.with(|c| c.execute("DELETE FROM agents WHERE name='pc'", []))
        .unwrap();
    db.with(|c| store::purge_pairings(c, Some(T + 1.0 + 2.0 * PAIRING_GRACE_S)))
        .unwrap();
    assert_eq!(
        sql_one::<i64>(&db, "SELECT active FROM api_keys WHERE label='Device pc'"),
        1,
        "a key somebody widened is not ours to cut"
    );
}

#[test]
fn what_cannot_be_shown_refuses_the_redeem_and_keeps_the_code_but_an_account_name_is_only_cleaned()
{
    let db = mem();
    // an account name nobody can change is cut and cleaned for the confirmation, never refused
    let org = create_org(&db, "Acme");
    assert!(matches!(
        create(&db, org, None, CODE, T),
        CreatePairing::Created { .. }
    ));
    let long = format!("{}\u{202e}\u{200b}", "A".repeat(300));
    db.with(|c| c.execute("UPDATE orgs SET name=? WHERE id=?", params![long, org]))
        .unwrap();
    let Redeem::Paired { org: shown, .. } = redeem(&db, CODE, "pc", "k", T + 1.0) else {
        panic!("an account name is no reason to refuse");
    };
    assert_eq!(shown, "A".repeat(200));
    // what phones add on their own stays
    let org = create_org(&db, "Café ☕\u{fe0f}");
    create(&db, org, None, "3333-3333-3333", T);
    let Redeem::Paired { org: shown, .. } = redeem(&db, "3333-3333-3333", "pc3", "k3", T + 1.0)
    else {
        panic!()
    };
    assert_eq!(shown, "Café ☕\u{fe0f}");

    // an invisible character in the creator's address would show the victim's address
    let org = create_org(&db, "Acme 2");
    // (a code made by an older version: the creator's address was copied unchecked)
    create(&db, org, None, "2222-2222-2222", T);
    db.with(|c| {
        c.execute(
            "UPDATE pairing_codes SET created_by=? WHERE org_id=?",
            params!["boss@acme.example\u{200b}", org],
        )
    })
    .unwrap();
    assert_eq!(
        redeem(&db, "2222-2222-2222", "pc2", "k2", T + 1.0),
        Redeem::Unshowable(store::PAIRING_ADDRESS_UNSHOWABLE)
    );
    assert_eq!(
        sql_one::<i64>(
            &db,
            "SELECT COUNT(*) FROM api_keys WHERE label='Device pc2'"
        ),
        0
    );

    // a stored device name that is not fit to be shown or that Star printers own: the code stays
    let dev = device(&db);
    create(&db, DEFAULT_ORG, Some(dev.computer_id), "4444-4444-4444", T);
    for (stored, why) in [
        ("office\u{202e}pc", store::PAIRING_DEVICE_UNSHOWABLE),
        ("cloudprnt-desk", store::PAIRING_DEVICE_RESERVED),
        // the agent trims the name it reads back, so a key made for these could never register
        ("", store::PAIRING_DEVICE_BLANK),
        (" ", store::PAIRING_DEVICE_BLANK),
        ("pc ", store::PAIRING_DEVICE_BLANK),
        (" pc2", store::PAIRING_DEVICE_BLANK),
        ("pc3\u{a0}", store::PAIRING_DEVICE_BLANK),
    ] {
        db.with(|c| {
            c.execute(
                "UPDATE agents SET name=? WHERE id=?",
                params![stored, dev.computer_id],
            )
        })
        .unwrap();
        assert_eq!(
            redeem(&db, "4444-4444-4444", "ignored", "k4", T + 1.0),
            Redeem::Unshowable(why),
            "{stored}"
        );
    }
    // a joined emoji in the stored name is no reason: the new key gets that very name
    db.with(|c| {
        c.execute(
            "UPDATE agents SET name='desk \u{1f468}\u{200d}\u{1f469}' WHERE id=?",
            [dev.computer_id],
        )
    })
    .unwrap();
    assert!(matches!(
        redeem(&db, "4444-4444-4444", "ignored", "k4", T + 2.0),
        Redeem::Paired { name, replaces: true, .. } if name == "desk \u{1f468}\u{200d}\u{1f469}"
    ));
}

#[test]
fn a_pending_printer_is_not_listed_counted_or_offered_until_the_swap() {
    let db = mem();
    let dev = device(&db);
    reconnect(&db, &dev);
    reg_at(
        &db,
        "office-pc",
        "new-key",
        &[decl("A", true), decl("Z", false)],
        T + 5.0,
    );
    assert_eq!(printer_row(&db, "Z").2, "pending");
    let names = |db: &Db| -> Vec<String> {
        db.with(|c| store::list_printers(c, 60.0, None, None))
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    // nothing can be queued for it, so nothing shows it: not the list (the dashboard, the shop
    // plugins, the PrintNode list), not the count, not the computer's printer count
    assert_eq!(names(&db), ["A", "L"]);
    assert_eq!(
        db.with(|c| store::metrics(c, 60.0, None, None))
            .unwrap()
            .printers_total,
        2
    );
    let agents = db
        .with(|c| store::list_agents(c, 60.0, None, None))
        .unwrap();
    assert_eq!(agents[0].printers, 2);
    // nor the operator's org list, which counts what the documents say are printers
    let counted = |db: &Db| -> i64 {
        db.with(|c| {
            store::operator_list_orgs(
                c,
                T,
                60.0,
                &store::OrgQuery {
                    status: None,
                    q: None,
                    after: 0,
                    only: Some(DEFAULT_ORG),
                    limit: 10,
                },
            )
        })
        .unwrap()[0]
            .printers
    };
    assert_eq!(counted(&db), 2);
    // the swap makes it a printer
    take(&db, "new-key", T + 10.0);
    assert_eq!(counted(&db), 3);
    assert_eq!(names(&db), ["A", "L", "Z"]);
    assert_eq!(
        db.with(|c| store::metrics(c, 60.0, None, None))
            .unwrap()
            .printers_total,
        3
    );
}

#[test]
fn a_code_is_not_made_for_a_creator_or_a_device_that_cannot_be_shown_or_used() {
    let db = mem();
    let made = |org: i64, agent: Option<i64>, by: Option<&str>| {
        db.with(|c| store::create_pairing(c, org, agent, by, CODE, Some(T)))
            .unwrap()
    };
    // the creator's address with an invisible character
    assert_eq!(
        made(DEFAULT_ORG, None, Some("boss@acme.example\u{200b}")),
        CreatePairing::Unshowable(store::PAIRING_ADDRESS_UNSHOWABLE)
    );
    // an account name is cleaned for the confirmation, so it is no reason to refuse a code
    let long = create_org(&db, &"A".repeat(300));
    assert!(matches!(
        made(long, None, None),
        CreatePairing::Created { .. }
    ));
    db.with(|c| c.execute("DELETE FROM pairing_codes", []))
        .unwrap();
    let phone = create_org(&db, "Café ☕\u{fe0f}");
    assert!(matches!(
        made(phone, None, None),
        CreatePairing::Created { .. }
    ));
    db.with(|c| c.execute("DELETE FROM pairing_codes", []))
        .unwrap();

    let dev = device(&db);
    let rename = |name: &str| {
        db.with(|c| {
            c.execute(
                "UPDATE agents SET name=? WHERE id=?",
                params![name, dev.computer_id],
            )
        })
        .unwrap();
    };
    // a device whose stored name is not fit to be shown, or that Star printers own
    for (stored, why) in [
        ("office\u{202e}pc", store::PAIRING_DEVICE_UNSHOWABLE),
        ("cloudprnt-desk", store::PAIRING_DEVICE_RESERVED),
        ("", store::PAIRING_DEVICE_BLANK),
        ("pc ", store::PAIRING_DEVICE_BLANK),
        (" pc2", store::PAIRING_DEVICE_BLANK),
        ("pc3\u{a0}", store::PAIRING_DEVICE_BLANK),
    ] {
        rename(stored);
        assert_eq!(
            made(DEFAULT_ORG, Some(dev.computer_id), None),
            CreatePairing::Unshowable(why),
            "{stored}"
        );
    }
    assert_eq!(sql_one::<i64>(&db, "SELECT COUNT(*) FROM pairing_codes"), 0);
    // nothing wrong, or only what phones and scripts add: made
    for name in ["office-pc", "office \u{2615}\u{fe0f}"] {
        rename(name);
        assert!(
            matches!(
                made(
                    DEFAULT_ORG,
                    Some(dev.computer_id),
                    Some("boss@acme.example")
                ),
                CreatePairing::Created { .. }
            ),
            "{name}"
        );
        db.with(|c| c.execute("DELETE FROM pairing_codes", []))
            .unwrap();
    }
}
