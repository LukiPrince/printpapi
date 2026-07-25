import os, tempfile, time
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


def test_register_stores_capabilities_and_list_printers_returns_them():
    conn = _db()
    caps = {"papers": ["A4", "Letter"], "bins": ["Tray 1"], "duplex": True, "color": False}
    store.register_agent(conn, "a", "k", [{"name": "HP", "can_pdf": True, "capabilities": caps},
                                          {"name": "Z", "can_pdf": False}])
    ps = {p["name"]: p for p in store.list_printers(conn, online_window_s=60)}
    assert ps["HP"]["capabilities"] == caps
    assert ps["Z"]["capabilities"] is None
    # re-register updates capabilities in place
    caps2 = dict(caps, duplex=False)
    store.register_agent(conn, "a", "k", [{"name": "HP", "can_pdf": True, "capabilities": caps2}])
    ps = {p["name"]: p for p in store.list_printers(conn, online_window_s=60)}
    assert ps["HP"]["capabilities"] == caps2


def test_enqueue_and_claim_roundtrip_options():
    conn = _db()
    reg = store.register_agent(conn, "a", "k", [{"name": "HP", "can_pdf": True}])
    opts = {"duplex": "long-edge", "paper": "A4", "bin": "Tray 1"}
    jid = store.enqueue_job(conn, reg["printer_ids"]["HP"], "pdf_base64", "pdf", b"%PDF",
                            options=opts)
    c = store.claim_job(conn, reg["computer_id"])
    assert c["job_id"] == jid and c["options"] == opts


def test_claim_without_options_is_none():
    conn = _db()
    reg = store.register_agent(conn, "a", "k", [{"name": "Z", "can_pdf": False}])
    store.enqueue_job(conn, reg["printer_ids"]["Z"], "raw_base64", "raw", b"x")
    assert store.claim_job(conn, reg["computer_id"])["options"] is None


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
    assert store.authenticate_client(conn, "secret-key")["id"] == kid
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


# --- multi-tenancy -----------------------------------------------------------------


def test_create_org_and_list_orgs():
    conn = _db()
    oid = store.create_org(conn, "acme")
    assert oid != store.DEFAULT_ORG
    assert {o["name"]: o["id"] for o in store.list_orgs(conn)} == {"default": store.DEFAULT_ORG,
                                                                  "acme": oid}
    assert store.org_exists(conn, oid) and not store.org_exists(conn, 4242)


def test_api_key_resolves_to_its_org():
    conn = _db()
    oid = store.create_org(conn, "acme")
    kid = store.add_api_key(conn, "acme", "k-acme", org_id=oid)
    assert store.authenticate_client(conn, "k-acme") == {"id": kid, "org_id": oid}
    assert store.authenticate_client(conn, "nope") is None
    store.add_api_key(conn, "legacy", "k-legacy")          # no org -> the default one
    assert store.authenticate_client(conn, "k-legacy")["org_id"] == store.DEFAULT_ORG


def test_list_api_keys_filters_by_org_and_reports_it():
    conn = _db()
    oid = store.create_org(conn, "acme")
    store.add_api_key(conn, "acme", "ka", org_id=oid)
    store.add_api_key(conn, "legacy", "kd")
    scoped = store.list_api_keys(conn, org_id=oid)
    assert [k["label"] for k in scoped] == ["acme"] and scoped[0]["org_id"] == oid
    assert len(store.list_api_keys(conn)) == 2             # unfiltered = root view
    assert "key_hash" not in scoped[0]


def _two_orgs(conn):
    """Two orgs with an identically named agent + printer in each — proves nothing is global."""
    other = store.create_org(conn, "acme")
    a = store.register_agent(conn, "pc", "ka", [{"name": "Z", "can_pdf": False}])
    b = store.register_agent(conn, "pc", "kb", [{"name": "Z", "can_pdf": False}], org_id=other)
    return other, a, b


def test_register_agent_scopes_name_and_printers_to_its_org():
    conn = _db()
    other, a, b = _two_orgs(conn)
    assert a["computer_id"] != b["computer_id"]              # same name, different org -> two agents
    assert a["printer_ids"]["Z"] != b["printer_ids"]["Z"]
    orgs = {r["id"]: r["org_id"] for r in conn.execute("SELECT id, org_id FROM printers")}
    assert orgs[a["printer_ids"]["Z"]] == store.DEFAULT_ORG
    assert orgs[b["printer_ids"]["Z"]] == other


def test_reads_and_writes_are_org_scoped_and_unfiltered_means_root():
    conn = _db()
    other, a, b = _two_orgs(conn)
    ja = store.enqueue_job(conn, a["printer_ids"]["Z"], "raw_base64", "raw", b"a")
    jb = store.enqueue_job(conn, b["printer_ids"]["Z"], "raw_base64", "raw", b"b")
    assert conn.execute("SELECT org_id FROM jobs WHERE id=?", (jb,)).fetchone()["org_id"] == other

    assert [p["id"] for p in store.list_printers(conn, 60, org_id=other)] == [b["printer_ids"]["Z"]]
    assert len(store.list_printers(conn, 60)) == 2                       # root view
    assert [j["id"] for j in store.recent_jobs(conn, org_id=other)] == [jb]
    assert len(store.recent_jobs(conn)) == 2

    assert store.get_job(conn, jb, org_id=store.DEFAULT_ORG) is None      # cross-org read denied
    assert store.get_job(conn, jb, org_id=other)["state"] == "queued"
    assert store.get_job(conn, jb) is not None                           # root sees it

    assert store.metrics(conn, 60, org_id=other)["printers_total"] == 1
    assert store.metrics(conn, 60, org_id=other)["jobs"]["queued"] == 1
    assert store.metrics(conn, 60)["printers_total"] == 2

    with pytest.raises(store.UnknownPrinter):                            # foreign printer
        store.enqueue_job(conn, b["printer_ids"]["Z"], "raw_base64", "raw", b"x",
                          org_id=store.DEFAULT_ORG)


def test_cancel_of_a_foreign_job_is_not_found_and_leaves_it_alone():
    conn = _db()
    other, a, b = _two_orgs(conn)
    jb = store.enqueue_job(conn, b["printer_ids"]["Z"], "raw_base64", "raw", b"b")
    assert store.cancel_job(conn, jb, org_id=store.DEFAULT_ORG) == "not_found"
    assert store.get_job(conn, jb)["state"] == "queued"
    assert store.cancel_job(conn, jb, org_id=other) == "cancelled"


def test_legacy_single_org_db_keeps_working_unchanged():
    """A DB written before multi-tenancy has org_id=1 everywhere — org-1 callers still see it all."""
    conn = _db()
    reg = store.register_agent(conn, "old", "k", [{"name": "Z", "can_pdf": False}])
    jid = store.enqueue_job(conn, reg["printer_ids"]["Z"], "raw_base64", "raw", b"x")
    kid = store.add_api_key(conn, "legacy", "legacy-key")
    assert conn.execute("SELECT org_id FROM jobs WHERE id=?", (jid,)).fetchone()["org_id"] == 1
    assert store.authenticate_client(conn, "legacy-key") == {"id": kid, "org_id": 1}
    assert store.get_job(conn, jid, org_id=1)["state"] == "queued"
    assert len(store.list_printers(conn, 60, org_id=1)) == 1
    assert len(store.recent_jobs(conn, org_id=1)) == 1
    assert store.claim_job(conn, reg["computer_id"])["job_id"] == jid    # agent path untouched


# --- computer status + liveness events ----------------------------------------------


def test_list_agents_reports_liveness_printer_count_and_org_scope():
    conn = _db()
    other, a, b = _two_orgs(conn)
    now = time.time()
    conn.execute("UPDATE agents SET last_seen_at=? WHERE id=?", (now - 300, b["computer_id"]))
    conn.commit()
    scoped = store.list_agents(conn, 60, now=now, org_id=other)
    assert [x["id"] for x in scoped] == [b["computer_id"]]
    assert scoped[0]["online"] is False and scoped[0]["printers"] == 1 and scoped[0]["name"] == "pc"
    root = {x["id"]: x for x in store.list_agents(conn, 60, now=now)}
    assert len(root) == 2 and root[a["computer_id"]]["online"] is True


def test_agent_transitions_fire_once_per_liveness_edge():
    conn = _db()
    store.set_org_event_url(conn, store.DEFAULT_ORG, "https://hooks.example/x")
    reg = store.register_agent(conn, "pc", "k", [])
    now = time.time()
    assert store.claim_agent_transitions(conn, 60, now=now) == []      # just seen -> no edge
    late = now + 300
    evs = store.claim_agent_transitions(conn, 60, now=late)
    assert [(e["agent_id"], e["event"], e["event_url"]) for e in evs] == [
        (reg["computer_id"], "offline", "https://hooks.example/x")]
    assert evs[0]["name"] == "pc" and evs[0]["org_id"] == store.DEFAULT_ORG
    assert store.claim_agent_transitions(conn, 60, now=late) == []     # edge consumed, not repeated
    store.register_agent(conn, "pc", "k", [])                          # agent comes back
    assert [e["event"] for e in store.claim_agent_transitions(conn, 60)] == ["online"]


def test_transitions_of_an_org_without_an_event_url_are_dropped_not_queued():
    conn = _db()
    store.register_agent(conn, "pc", "k", [])
    late = time.time() + 300
    assert store.claim_agent_transitions(conn, 60, now=late) == []     # nowhere to send it
    store.set_org_event_url(conn, store.DEFAULT_ORG, "https://hooks.example/x")
    assert store.claim_agent_transitions(conn, 60, now=late) == []     # and it is not replayed
    assert store.set_org_event_url(conn, 4242, "https://h") is False   # unknown org


# --- idempotency + expiry ------------------------------------------------------------


def test_same_idempotency_key_enqueues_once_and_returns_the_first_job():
    conn = _db()
    reg = store.register_agent(conn, "pc", "k", [{"name": "Z", "can_pdf": False}])
    pid = reg["printer_ids"]["Z"]
    first = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", idempotency_key="order-42")
    again = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", idempotency_key="order-42")
    assert again == first
    assert len(store.recent_jobs(conn)) == 1                      # no second print
    other = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", idempotency_key="order-43")
    assert other != first
    assert store.enqueue_job(conn, pid, "raw_base64", "raw", b"a") != first   # no key -> no dedupe


def test_idempotency_keys_are_scoped_to_the_org():
    conn = _db()
    other, a, b = _two_orgs(conn)
    ja = store.enqueue_job(conn, a["printer_ids"]["Z"], "raw_base64", "raw", b"a",
                           idempotency_key="order-42")
    jb = store.enqueue_job(conn, b["printer_ids"]["Z"], "raw_base64", "raw", b"b",
                           idempotency_key="order-42", org_id=other)
    assert ja != jb                                               # same key, different orgs


def test_an_expired_job_is_never_claimed_and_the_reaper_fails_it():
    conn = _db()
    reg = store.register_agent(conn, "pc", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    stale = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", expire_after=60)
    fresh = store.enqueue_job(conn, pid, "raw_base64", "raw", b"b")
    later = time.time() + 61
    assert store.claim_job(conn, aid, now=later)["job_id"] == fresh    # skips the expired one
    assert store.expire_jobs(conn, now=later) == 1
    job = store.get_job(conn, stale)
    assert job["state"] == "failed" and job["error"] == "expired"
    assert store.expire_jobs(conn, now=later) == 0                     # already terminal
    assert store.get_job(conn, fresh)["state"] == "claimed"            # untouched


def test_a_job_inside_its_window_still_prints():
    conn = _db()
    reg = store.register_agent(conn, "pc", "k", [{"name": "Z", "can_pdf": False}])
    aid, pid = reg["computer_id"], reg["printer_ids"]["Z"]
    jid = store.enqueue_job(conn, pid, "raw_base64", "raw", b"a", expire_after=3600)
    assert store.expire_jobs(conn) == 0
    assert store.claim_job(conn, aid)["job_id"] == jid
