# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Build python_v2.sqlite3: a database written by the Python server's own store (app/store.py,
app/auth.py), so the Rust store can be tested against rows it did not write.

Run from anywhere:  python crates/server/tests/fixtures/make_python_db.py
The file is committed; regenerate it only when the Python schema changes. Timestamps are pinned
(the jobs to 2023, credentials to year 2100) so the assertions in store_compat.rs stay stable.
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
sys.path.insert(0, ROOT)

from app import auth, store  # noqa: E402

OUT = os.path.join(HERE, "python_v2.sqlite3")
FAR = 4102444800.0  # 2100-01-01
BASE = 1_700_000_000.0  # 2023-11-14

for suffix in ("", "-wal", "-shm"):
    if os.path.exists(OUT + suffix):
        os.remove(OUT + suffix)

conn = store.connect(OUT)
store.init_db(conn)

org = store.create_org(conn, "Acme GmbH")
uid = store.create_user(conn, org, "Boss@Acme.test", auth.hash_password("correct horse battery"))
store.create_session(conn, uid, "sess_known", ttl_s=3600)
store.add_api_key(conn, "acme-key", "pk_known_test_key", org_id=org)

caps = {"papers": ["A4", "Letter"], "bins": ["Tray 1"], "duplex": True, "color": False}
reg = store.register_agent(
    conn, "win-1", "agent-key-1",
    [{"name": "Zebra ZD420", "can_pdf": False},
     {"name": "HP LaserJet", "can_pdf": True, "capabilities": caps}],
    org_id=org)
aid = reg["computer_id"]
zebra, hp = reg["printer_ids"]["Zebra ZD420"], reg["printer_ids"]["HP LaserJet"]

kw = dict(org_id=org, user_id=uid)
done = store.enqueue_job(conn, zebra, "raw_base64", "raw", b"^XA^XZ", title="done label", **kw)
store.claim_job(conn, aid)
store.finish_job(conn, done, aid, True)
failed = store.enqueue_job(conn, hp, "pdf_base64", "pdf", b"%PDF-failed", title="failed doc",
                           callback_url="https://hooks.acme.test/failed", **kw)
store.claim_job(conn, aid)
store.finish_job(conn, failed, aid, False, "printer offline")
claimed = store.enqueue_job(conn, hp, "pdf_base64", "pdf", b"%PDF-claimed", title="in flight",
                            copies=2, options={"duplex": "long-edge", "paper": "A4"}, **kw)
store.claim_job(conn, aid)
cancelled = store.enqueue_job(conn, zebra, "raw_base64", "raw", b"^XA^XZ", title="cancelled", **kw)
store.cancel_job(conn, cancelled, org_id=org)
queued = store.enqueue_job(
    conn, hp, "pdf_base64", "pdf", b"%PDF-queued", title="waiting", copies=3,
    callback_url="https://hooks.acme.test/queued", idempotency_key="order-100",
    options={"duplex": "short-edge", "paper": "Letter", "bin": "Tray 1"},
    expire_after=10, **kw)

# Pin the clock-dependent columns. Some are written as INTEGER on purpose: servers older than the
# REAL-timestamp convention stored whole seconds, and the Rust side must read both.
for n, jid in enumerate((done, failed, claimed, cancelled, queued)):
    conn.execute("UPDATE jobs SET created_at=? WHERE id=?", (BASE + n * 10, jid))
conn.execute("UPDATE jobs SET claimed_at=?, finished_at=? WHERE id=?", (BASE + 1, BASE + 2, done))
conn.execute("UPDATE jobs SET claimed_at=?, finished_at=? WHERE id=?", (BASE + 11, BASE + 12, failed))
conn.execute("UPDATE jobs SET claimed_at=? WHERE id=?", (BASE + 21, claimed))
conn.execute("UPDATE jobs SET finished_at=? WHERE id=?", (BASE + 31, cancelled))
conn.execute("UPDATE jobs SET expires_at=? WHERE id=?", (FAR, queued))
conn.execute("UPDATE sessions SET created_at=?, expires_at=?", (BASE, FAR))
conn.execute("UPDATE agents SET last_seen_at=?, created_at=?", (BASE + 100, int(BASE)))
conn.execute("UPDATE orgs SET created_at=? WHERE id=?", (int(BASE), org))  # INTEGER, not REAL
conn.commit()

# Fold the WAL into the main file and leave the database in rollback-journal mode, so the
# committed fixture is a single self-contained file.
conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchall()
conn.execute("PRAGMA journal_mode=DELETE").fetchall()
conn.close()
for suffix in ("-wal", "-shm"):
    if os.path.exists(OUT + suffix):
        os.remove(OUT + suffix)

print(f"wrote {OUT}: org={org} user={uid} agent={aid} printers=({zebra},{hp}) "
      f"jobs=done:{done} failed:{failed} claimed:{claimed} cancelled:{cancelled} queued:{queued}")
