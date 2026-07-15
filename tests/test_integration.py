import base64, json, threading, urllib.request
from app import store, server
from agent import print_agent


def _req(method, url, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method)
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(r) as resp:
        return resp.status, resp.read()


def test_register_submit_poll_print_result_end_to_end():
    conn = store.connect(":memory:")
    store.init_db(conn)
    httpd = server.create_server(conn, "clienttoken", port=0,
                                 long_poll_timeout=2.0, poll_interval=0.05)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{httpd.server_address[1]}"
    try:
        # 1. agent registers over real HTTP
        entry = {"name": "Zebra", "can_pdf": False, "target": "Zebra"}
        reg = print_agent.register(base, "agentkey", "win-1", [entry])
        printer_by_id = {pid: entry for name, pid in reg["printer_ids"].items()}
        pid = reg["printer_ids"]["Zebra"]

        # 2. client submits a job (base64 "HELLO")
        code, raw = _req("POST", base + "/jobs", token="clienttoken",
                         body={"printer_id": pid, "type": "raw_base64",
                               "content": base64.b64encode(b"HELLO").decode()})
        assert code == 200
        jid = json.loads(raw)["job_id"]

        # 3. agent runs one real poll cycle; capture what got "printed"
        printed = {}
        handled = print_agent.run_once(
            base, "agentkey", printer_by_id,
            raw_fn=lambda p, d: printed.update(p=p, d=d),
            pdf_fn=lambda p, d: None)
        assert handled is True
        assert printed == {"p": "Zebra", "d": b"HELLO"}

        # 4. client sees it done
        code, raw = _req("GET", base + f"/jobs/{jid}", token="clienttoken")
        assert json.loads(raw)["state"] == "done"
    finally:
        httpd.shutdown()
