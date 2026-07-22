import os, tempfile
import pytest
from app import store


def _db():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    conn = store.connect(path)
    store.init_db(conn)
    return conn


def test_init_seeds_default_org_and_user_and_is_idempotent():
    conn = _db()
    store.init_db(conn)  # second call must not raise or duplicate
    org = conn.execute("SELECT name FROM orgs WHERE id=?", (store.DEFAULT_ORG,)).fetchone()
    user = conn.execute("SELECT org_id FROM users WHERE id=?", (store.DEFAULT_USER,)).fetchone()
    assert org["name"] == "default"
    assert user["org_id"] == store.DEFAULT_ORG
    assert conn.execute("SELECT COUNT(*) c FROM orgs").fetchone()["c"] == 1


def test_register_is_idempotent_and_authenticates():
    conn = _db()
    r1 = store.register_agent(conn, "win-1", "key-abc",
                              [{"name": "Zebra", "can_pdf": False}])
    r2 = store.register_agent(conn, "win-1", "key-abc",
                              [{"name": "Zebra", "can_pdf": False},
                               {"name": "HP", "can_pdf": True}])
    assert r1["computer_id"] == r2["computer_id"]            # same agent, not duplicated
    assert r1["printer_ids"]["Zebra"] == r2["printer_ids"]["Zebra"]
    assert conn.execute("SELECT COUNT(*) c FROM agents").fetchone()["c"] == 1
    assert conn.execute("SELECT COUNT(*) c FROM printers").fetchone()["c"] == 2
    assert store.authenticate_agent(conn, "key-abc") == r1["computer_id"]
    assert store.authenticate_agent(conn, "wrong") is None


def test_register_rejects_existing_name_with_different_key():
    conn = _db()
    store.register_agent(conn, "win-1", "key-abc", [])
    store.register_agent(conn, "win-1", "key-abc", [])      # same key -> idempotent restart, ok
    with pytest.raises(store.AuthError):
        store.register_agent(conn, "win-1", "other-key", [])  # name<->key binding


def test_enqueue_resolves_agent_and_rejects_unknown_printer():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Zebra", "can_pdf": False}])
    pid = reg["printer_ids"]["Zebra"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"^XA^XZ")
    row = conn.execute("SELECT agent_id, state, payload, mode FROM jobs WHERE id=?",
                       (jid,)).fetchone()
    assert row["agent_id"] == reg["computer_id"]
    assert row["state"] == "queued"
    assert bytes(row["payload"]) == b"^XA^XZ"
    assert row["mode"] == "raw"
    with pytest.raises(store.UnknownPrinter):
        store.enqueue_job(conn, 9999, "raw_base64", "raw", b"x")


def test_claim_returns_one_job_then_none_and_updates_last_seen():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    j1 = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a")
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"b")
    c1 = store.claim_job(conn, aid)
    assert c1["job_id"] == j1 and c1["mode"] == "raw"      # oldest first
    assert conn.execute("SELECT state FROM jobs WHERE id=?", (j1,)).fetchone()["state"] == "claimed"
    assert store.claim_job(conn, aid)["job_id"] != j1       # second job
    assert store.claim_job(conn, aid) is None               # nothing left
    assert conn.execute("SELECT last_seen_at FROM agents WHERE id=?", (aid,)).fetchone()["last_seen_at"]


def test_payload_and_finish_are_agent_scoped():
    conn = _db()
    a = store.register_agent(conn, "a", "ka", [{"name": "PA", "can_pdf": False}])
    b = store.register_agent(conn, "b", "kb", [{"name": "PB", "can_pdf": False}])
    jid = store.enqueue_job(conn, a["printer_ids"]["PA"], "raw_base64", "raw", b"DATA")
    store.claim_job(conn, a["computer_id"])
    assert store.get_payload(conn, jid, a["computer_id"]) == b"DATA"
    assert store.get_payload(conn, jid, b["computer_id"]) is None        # foreign agent denied
    assert store.finish_job(conn, jid, b["computer_id"], ok=True) is False
    assert store.finish_job(conn, jid, a["computer_id"], ok=False, error="boom") is True
    row = conn.execute("SELECT state, error FROM jobs WHERE id=?", (jid,)).fetchone()
    assert row["state"] == "failed" and row["error"] == "boom"
    assert store.finish_job(conn, jid, a["computer_id"], ok=True) is False  # not 'claimed' anymore


def test_reaper_requeues_then_fails_after_limit():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "P", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["P"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    store.claim_job(conn, aid)
    base = __import__("time").time()
    # not stale yet
    assert store.requeue_stale(conn, timeout_s=60, max_retries=1, now=base) == 0
    # stale, under retry limit -> requeued
    assert store.requeue_stale(conn, timeout_s=60, max_retries=1, now=base + 120) == 1
    assert conn.execute("SELECT state, retries FROM jobs WHERE id=?", (jid,)).fetchone()["state"] == "queued"
    # claim again, go stale again -> now over limit -> failed
    store.claim_job(conn, aid)
    assert store.requeue_stale(conn, timeout_s=60, max_retries=1, now=base + 300) == 0
    assert conn.execute("SELECT state FROM jobs WHERE id=?", (jid,)).fetchone()["state"] == "failed"


def test_get_job_and_list_printers_online_flag():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Z", "can_pdf": True}])
    pid = reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "pdf_base64", "pdf", b"%PDF")
    assert store.get_job(conn, jid)["state"] == "queued"
    assert store.get_job(conn, 123456) is None
    store.claim_job(conn, reg["computer_id"])      # sets last_seen_at to ~now
    ps = store.list_printers(conn, online_window_s=60)
    assert len(ps) == 1 and ps[0]["name"] == "Z" and ps[0]["can_pdf"] is True
    assert ps[0]["online"] is True
    assert store.list_printers(conn, online_window_s=60, now=__import__("time").time() + 600)[0]["online"] is False


def test_api_keys_add_authenticate_list_revoke():
    conn = _db()
    kid = store.add_api_key(conn, "n8n", "secret-key")
    assert store.authenticate_client(conn, "secret-key") == kid
    assert store.authenticate_client(conn, "nope") is None
    keys = store.list_api_keys(conn)
    assert len(keys) == 1 and keys[0]["label"] == "n8n"
    assert "key_hash" not in keys[0] and "key" not in keys[0]   # secrets never returned
    assert store.revoke_api_key(conn, kid) is True
    assert store.authenticate_client(conn, "secret-key") is None  # revoked -> denied
    assert store.list_api_keys(conn)[0]["active"] == 0
    assert store.revoke_api_key(conn, 9999) is False


def test_recent_jobs_newest_first_with_printer_name_and_limit():
    conn = _db()
    reg = store.register_agent(conn, "win-1", "k", [{"name": "Zebra", "can_pdf": False}])
    pid = reg["computer_id"], reg["printer_ids"]["Zebra"]
    aid, pid = pid
    j1 = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a")
    j2 = store.enqueue_job(conn, pid, "pdf_base64", "pdf", b"b")
    store.claim_job(conn, aid)  # claims j1
    store.finish_job(conn, j1, aid, ok=True)
    rows = store.recent_jobs(conn, limit=10)
    assert [r["id"] for r in rows] == [j2, j1]          # newest first
    assert rows[1]["printer_name"] == "Zebra"           # joined name
    assert rows[1]["state"] == "done" and rows[0]["state"] == "queued"
    assert rows[0]["type"] == "pdf_base64" and rows[0]["mode"] == "pdf"
    assert "payload" not in rows[0]                      # never leak bytes to the dashboard
    assert len(store.recent_jobs(conn, limit=1)) == 1   # limit honored


def test_enqueue_and_recent_jobs_carry_title_and_agent_name():
    conn = _db()
    reg = store.register_agent(conn, "pc-1", "k", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"x", title="Versandlabel")
    row = store.recent_jobs(conn)[0]
    assert row["title"] == "Versandlabel"
    assert row["agent_name"] == "pc-1"
    assert row["printer_name"] == "Z"


def test_title_column_migration_is_idempotent():
    conn = _db()
    store.init_db(conn)  # run migration a second time — must not raise or duplicate
    cols = [r["name"] for r in conn.execute("PRAGMA table_info(jobs)").fetchall()]
    assert cols.count("title") == 1


def test_enqueue_stores_copies_and_claim_returns_it():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x", copies=3)
    assert store.claim_job(conn, aid)["copies"] == 3
    assert conn.execute("SELECT copies FROM jobs WHERE id=?", (jid,)).fetchone()["copies"] == 3


def test_enqueue_copies_defaults_to_one():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    assert store.claim_job(conn, aid)["copies"] == 1


def test_cancel_queued_job_marks_cancelled():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    assert store.cancel_job(conn, jid) == "cancelled"
    row = conn.execute("SELECT state, finished_at FROM jobs WHERE id=?", (jid,)).fetchone()
    assert row["state"] == "cancelled" and row["finished_at"] is not None
    # a cancelled job is never claimable
    assert store.claim_job(conn, aid) is None


def test_cancel_claimed_job_is_refused():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"x")
    store.claim_job(conn, aid)
    assert store.cancel_job(conn, jid) == "not_cancellable"
    assert conn.execute("SELECT state FROM jobs WHERE id=?", (jid,)).fetchone()["state"] == "claimed"


def test_cancel_unknown_job_is_not_found():
    conn = _db()
    assert store.cancel_job(conn, 999999) == "not_found"


def test_metrics_snapshot_counts_jobs_agents_printers():
    conn = _db()
    reg = store.register_agent(conn, "w", "k",
                               [{"name": "Z", "can_pdf": False}, {"name": "Y", "can_pdf": True}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    j1 = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a")
    j2 = store.enqueue_job(conn, pid, "raw_base64", "raw", b"b")
    store.claim_job(conn, aid)                 # claims j1 -> claimed; last_seen ~now
    store.finish_job(conn, j1, aid, ok=True)   # j1 -> done
    store.cancel_job(conn, j2)                 # j2 -> cancelled
    m = store.metrics(conn, online_window_s=60)
    assert m["jobs"]["done"] == 1 and m["jobs"]["cancelled"] == 1
    assert m["jobs"].get("queued", 0) == 0
    assert m["agents_total"] == 1 and m["agents_online"] == 1
    assert m["printers_total"] == 2
    # aged past the window -> offline
    assert store.metrics(conn, online_window_s=60,
                         now=__import__("time").time() + 600)["agents_online"] == 0


def test_pending_webhooks_selects_only_terminal_undelivered_with_callback():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    a = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", callback_url="https://h/a", title="A")
    store.claim_job(conn, aid); store.finish_job(conn, a, aid, ok=True)          # done + cb
    store.enqueue_job(conn, pid, "raw_base64", "raw", b"b", callback_url="https://h/b")  # queued + cb
    c = store.enqueue_job(conn, pid, "raw_base64", "raw", b"c")                  # failed, NO cb
    store.claim_job(conn, aid); store.finish_job(conn, c, aid, ok=False, error="boom")
    d = store.enqueue_job(conn, pid, "raw_base64", "raw", b"d", callback_url="https://h/d")
    store.cancel_job(conn, d)                                                    # cancelled + cb
    pend = store.pending_webhooks(conn, max_attempts=5)
    assert [p["job_id"] for p in pend] == [a, d]
    assert pend[0]["callback_url"] == "https://h/a" and pend[0]["state"] == "done"
    assert pend[0]["title"] == "A" and pend[0]["printer_id"] == pid
    assert pend[1]["state"] == "cancelled"


def test_mark_webhook_delivered_removes_from_pending():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    a = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", callback_url="https://h/a")
    store.claim_job(conn, aid); store.finish_job(conn, a, aid, ok=True)
    assert len(store.pending_webhooks(conn, max_attempts=5)) == 1
    store.mark_webhook_delivered(conn, a)
    assert store.pending_webhooks(conn, max_attempts=5) == []


def test_bump_webhook_attempt_caps_retries():
    conn = _db()
    reg = store.register_agent(conn, "w", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    a = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", callback_url="https://h/a")
    store.claim_job(conn, aid); store.finish_job(conn, a, aid, ok=True)
    for _ in range(3):
        store.bump_webhook_attempt(conn, a)
    assert store.pending_webhooks(conn, max_attempts=3) == []          # attempts >= cap -> given up
    assert store.pending_webhooks(conn, max_attempts=4)[0]["job_id"] == a


def test_webhook_columns_migration_is_idempotent():
    conn = _db()
    store.init_db(conn)
    cols = [r["name"] for r in conn.execute("PRAGMA table_info(jobs)").fetchall()]
    for col in ("callback_url", "hook_attempts", "hook_delivered_at"):
        assert cols.count(col) == 1


def test_copies_column_migration_is_idempotent():
    conn = _db()
    store.init_db(conn)  # run migration a second time — must not raise or duplicate
    cols = [r["name"] for r in conn.execute("PRAGMA table_info(jobs)").fetchall()]
    assert cols.count("copies") == 1
