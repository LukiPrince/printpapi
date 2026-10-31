# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Benchmark the Python server against the Rust server (and the Python agent against the Rust agent).

Python 3, stdlib only. See README.md. Examples:

    python tools/bench/bench.py --python-ref v2.5.1
    python tools/bench/bench.py --rust target/release/printpapi-server.exe
    python tools/bench/bench.py --compare --rust target/release/printpapi-server.exe --json out.json
"""
import argparse
import base64
import http.client
import io
import json
import os
import random
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
from pathlib import Path

TOKEN = "bench-admin-token"
HOST = "127.0.0.1"
LONG_POLL_S = 25.0     # the servers long-poll for 25 s; the client must outlast it
JOB_BODY = base64.b64encode(b"^XA^FDbench^FS^XZ").decode()


# --- process metrics (no psutil) ----------------------------------------------------------------

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class _PMC(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]

    class _THREADENTRY32(ctypes.Structure):
        _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                    ("th32ThreadID", wintypes.DWORD), ("th32OwnerProcessID", wintypes.DWORD),
                    ("tpBasePri", wintypes.LONG), ("tpDeltaPri", wintypes.LONG),
                    ("dwFlags", wintypes.DWORD)]

    _k32.OpenProcess.restype = wintypes.HANDLE
    _k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _k32.CloseHandle.argtypes = [wintypes.HANDLE]
    _k32.K32GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(_PMC), wintypes.DWORD]
    _k32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    _k32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    _k32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    _k32.Thread32First.argtypes = [wintypes.HANDLE, ctypes.POINTER(_THREADENTRY32)]
    _k32.Thread32Next.argtypes = [wintypes.HANDLE, ctypes.POINTER(_THREADENTRY32)]
    _INVALID = ctypes.c_void_p(-1).value

    def _ft(f):  # FILETIME (100 ns ticks) -> seconds
        return ((f.dwHighDateTime << 32) | f.dwLowDateTime) / 1e7

    def _win_threads(pid):
        snap = _k32.CreateToolhelp32Snapshot(0x4, 0)  # TH32CS_SNAPTHREAD
        if snap is None or snap == _INVALID:
            return 0
        try:
            te = _THREADENTRY32()
            te.dwSize = ctypes.sizeof(te)
            n = 0
            ok = _k32.Thread32First(snap, ctypes.byref(te))
            while ok:
                if te.th32OwnerProcessID == pid:
                    n += 1
                te.dwSize = ctypes.sizeof(te)
                ok = _k32.Thread32Next(snap, ctypes.byref(te))
            return n
        finally:
            _k32.CloseHandle(snap)

    def proc_metrics(pid):
        """{rss_mb, peak_rss_mb, threads, cpu_s} of a process, or None if it is gone."""
        h = _k32.OpenProcess(0x1410, False, pid)  # QUERY_INFORMATION | VM_READ | QUERY_LIMITED
        if not h:
            return None
        try:
            pmc = _PMC()
            pmc.cb = ctypes.sizeof(pmc)
            if not _k32.K32GetProcessMemoryInfo(h, ctypes.byref(pmc), pmc.cb):
                return None
            c, e, k, u = (wintypes.FILETIME() for _ in range(4))
            if not _k32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(k),
                                        ctypes.byref(u)):
                return None
            return {"rss_mb": pmc.WorkingSetSize / 1048576, "peak_rss_mb": pmc.PeakWorkingSetSize / 1048576,
                    "threads": _win_threads(pid), "cpu_s": _ft(k) + _ft(u)}
        finally:
            _k32.CloseHandle(h)
else:
    def proc_metrics(pid):
        try:
            status = {}
            with open(f"/proc/{pid}/status") as f:
                for line in f:
                    k, _, v = line.partition(":")
                    status[k] = v.strip()
            with open(f"/proc/{pid}/stat") as f:
                stat = f.read()
        except OSError:
            return None
        fields = stat[stat.rindex(")") + 2:].split()  # after "(comm)"; fields[0] is state (field 3)
        ticks = os.sysconf("SC_CLK_TCK")
        kb = lambda k: int(status.get(k, "0 kB").split()[0]) / 1024  # noqa: E731
        return {"rss_mb": kb("VmRSS"), "peak_rss_mb": kb("VmHWM"),
                "threads": int(status.get("Threads", "0")),
                "cpu_s": (int(fields[11]) + int(fields[12])) / ticks}


# --- HTTP client --------------------------------------------------------------------------------

class Client:
    """One thread's connection. http.client reconnects by itself after a server that closes every
    response (the Python server speaks HTTP/1.0); a keep-alive server keeps the socket."""

    def __init__(self, port, timeout=LONG_POLL_S + 15):
        self.port, self.timeout = port, timeout
        self.conn = None

    def request(self, method, path, key=TOKEN, body=None):
        headers = {"Authorization": f"Bearer {key}"}
        data = None
        if body is not None:
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        if self.conn is None:
            self.conn = http.client.HTTPConnection(HOST, self.port, timeout=self.timeout)
        try:
            self.conn.request(method, path, body=data, headers=headers)
            r = self.conn.getresponse()
            return r.status, r.read()
        except Exception:
            self.close()
            raise

    def close(self):
        if self.conn is not None:
            try:
                self.conn.close()
            except Exception:
                pass
            self.conn = None


def pct(sorted_vals, p):
    if not sorted_vals:
        return float("nan")
    return sorted_vals[min(len(sorted_vals) - 1, int(p / 100 * len(sorted_vals)))]


def free_port():
    with socket.socket() as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


def run_threads(n, fn):
    ts = [threading.Thread(target=fn, args=(i,), daemon=True) for i in range(n)]
    for t in ts:
        t.start()
    return ts


# --- processes under test -----------------------------------------------------------------------

def repo_root():
    here = Path(__file__).resolve().parent
    return subprocess.check_output(["git", "rev-parse", "--show-toplevel"], cwd=here, text=True).strip()


def extract_ref(ref, subdir, dest):
    """git archive <ref> <subdir> -> dest (tarfile module, so no `tar` binary is needed)."""
    raw = subprocess.check_output(["git", "archive", ref, subdir], cwd=repo_root())
    with tarfile.open(fileobj=io.BytesIO(raw)) as tf:
        tf.extractall(dest, filter="data")


class Proc:
    def __init__(self, argv, cwd, env, log):
        self.log = open(log, "wb")
        self.p = subprocess.Popen(argv, cwd=cwd, env=env, stdout=self.log, stderr=subprocess.STDOUT,
                                  stdin=subprocess.DEVNULL)

    @property
    def pid(self):
        return self.p.pid

    def metrics(self):
        return proc_metrics(self.p.pid) or {"rss_mb": 0, "peak_rss_mb": 0, "threads": 0, "cpu_s": 0}

    def stop(self):
        if self.p.poll() is None:
            self.p.terminate()
            try:
                self.p.wait(5)
            except subprocess.TimeoutExpired:
                self.p.kill()
                self.p.wait()
        self.log.close()


class Server:
    def __init__(self, kind, label, tmp, python_ref=None, binary=None):
        self.kind, self.label = kind, label
        self.port = free_port()
        self.base = f"http://{HOST}:{self.port}"
        work = Path(tmp) / f"server-{kind}"
        work.mkdir(parents=True)
        env = dict(os.environ, PRINTAPI_TOKEN=TOKEN, PRINT_DB=str(work / "bench.db"),
                   PRINT_PORT=str(self.port), PYTHONUNBUFFERED="1")
        env.pop("LOG_REQUESTS", None)
        if kind == "python":
            extract_ref(python_ref, "app", work)
            argv = [sys.executable, "-m", "app.server"]
        else:
            argv = [str(Path(binary).resolve())]
        self.proc = Proc(argv, work, env, work / "server.log")
        self.log_path = work / "server.log"

    def wait_ready(self, timeout=30):
        end = time.time() + timeout
        while time.time() < end:
            if self.proc.p.poll() is not None:
                raise RuntimeError(f"{self.label} server exited early:\n{self.log_path.read_text(errors='replace')}")
            try:
                c = Client(self.port, timeout=2)
                st, _ = c.request("GET", "/health")
                c.close()
                if st == 200:
                    return
            except Exception:
                time.sleep(0.1)
        raise RuntimeError(f"{self.label} server not ready after {timeout}s")


def register_agent(port, name, key, printer="p"):
    """-> printer id."""
    c = Client(port, timeout=30)
    try:
        st, body = c.request("POST", "/agent/register", key,
                             {"name": name, "printers": [{"name": printer, "can_pdf": False}]})
        if st != 200:
            raise RuntimeError(f"register {name}: {st} {body[:200]!r}")
        return json.loads(body)["printer_ids"][printer]
    finally:
        c.close()


def register_many(port, prefix, n):
    """Register n agents with distinct keys in parallel -> [(key, printer_id)]."""
    out = [None] * n
    errs = []

    def work(i):
        try:
            key = f"{prefix}-key-{i}"
            out[i] = (key, register_agent(port, f"{prefix}-{i}", key))
        except Exception as e:  # noqa: BLE001
            errs.append(e)
    for lo in range(0, n, 16):
        for t in run_threads(min(16, n - lo), lambda j, lo=lo: work(lo + j)):
            t.join()
    if errs:
        raise errs[0]
    return out


def submit_body(printer_id):
    return {"printer_id": printer_id, "type": "raw_base64", "content": JOB_BODY, "title": "bench"}


# --- scenarios ----------------------------------------------------------------------------------

def sc_idle(srv, a):
    time.sleep(3)
    m = srv.proc.metrics()
    return {"rss_mb": m["rss_mb"], "threads": m["threads"]}


def sc_idle_agents(srv, a):
    n, window = a.n_agents, a.idle_window
    agents = register_many(srv.port, "idle", n)
    stop = threading.Event()
    issued = [0]
    clients = [Client(srv.port) for _ in range(n)]
    lock = threading.Lock()
    errors = [0]

    def poll(i):
        key = agents[i][0]
        first = True
        while not stop.is_set():
            try:
                if first:
                    with lock:
                        issued[0] += 1
                    first = False
                clients[i].request("GET", "/agent/jobs", key)
            except Exception:
                if not stop.is_set():
                    with lock:
                        errors[0] += 1
                    time.sleep(0.5)
    run_threads(n, poll)
    deadline = time.time() + 30
    while issued[0] < n and time.time() < deadline:
        time.sleep(0.1)
    time.sleep(1)  # every agent has its poll in flight
    m0, t0 = srv.proc.metrics(), time.perf_counter()
    time.sleep(window)
    m1, t1 = srv.proc.metrics(), time.perf_counter()
    stop.set()
    for c in clients:
        c.close()
    cpu = m1["cpu_s"] - m0["cpu_s"]
    return {"agents": n, "rss_mb": m1["rss_mb"], "threads": m1["threads"], "cpu_s": cpu,
            "cpu_pct": 100 * cpu / (t1 - t0), "client_errors": errors[0], "window_s": t1 - t0}


def sc_submit(srv, a):
    (key, pid), = register_many(srv.port, "sink", 1)
    stop_at = time.perf_counter() + a.duration
    lats, errs = [], [0]
    lock = threading.Lock()
    cpu0, wall0 = time.process_time(), time.perf_counter()

    def work(i):
        c = Client(srv.port, timeout=30)
        mine, bad = [], 0
        body = submit_body(pid)
        while time.perf_counter() < stop_at:
            t = time.perf_counter()
            try:
                st, _ = c.request("POST", "/jobs", TOKEN, body)
                if st == 200:
                    mine.append(time.perf_counter() - t)
                else:
                    bad += 1
            except Exception:
                bad += 1
        c.close()
        with lock:
            lats.extend(mine)
            errs[0] += bad
    for t in run_threads(a.clients, work):
        t.join()
    wall = time.perf_counter() - wall0
    client_cpu = (time.process_time() - cpu0) / wall
    lats.sort()
    return {"clients": a.clients, "req_s": len(lats) / wall, "p50_ms": pct(lats, 50) * 1000,
            "p95_ms": pct(lats, 95) * 1000, "p99_ms": pct(lats, 99) * 1000, "errors": errs[0],
            "client_cpu_pct": client_cpu * 100, "client_bound": client_cpu > 0.9}


def sc_delivery(srv, a):
    (key, pid), = register_many(srv.port, "deliv", 1)
    got = threading.Event()
    recv = [0.0]
    stop = threading.Event()

    def agent(_):
        c = Client(srv.port)
        while not stop.is_set():
            try:
                st, _b = c.request("GET", "/agent/jobs", key)
            except Exception:
                if stop.is_set():
                    return
                time.sleep(0.2)
                continue
            if st == 200:
                recv[0] = time.perf_counter()
                got.set()
        c.close()
    run_threads(1, agent)
    time.sleep(0.5)
    sub, lats = Client(srv.port, timeout=30), []
    for _ in range(a.samples):
        time.sleep(random.uniform(0.05, 1.1))  # random phase against the Python server's 1 s DB poll
        got.clear()
        t0 = time.perf_counter()
        st, _b = sub.request("POST", "/jobs", TOKEN, submit_body(pid))
        if st != 200:
            raise RuntimeError(f"delivery submit: {st}")
        if not got.wait(15):
            raise RuntimeError("delivery: job never reached the agent")
        lats.append(recv[0] - t0)
    stop.set()
    sub.close()
    lats.sort()
    return {"samples": len(lats), "p50_ms": pct(lats, 50) * 1000, "p95_ms": pct(lats, 95) * 1000,
            "max_ms": lats[-1] * 1000}


def sc_cycle(srv, a):
    k = a.cycle_agents
    agents = register_many(srv.port, "cyc", k)
    per = a.prefill
    lock = threading.Lock()
    prefill_err = [0]

    def fill(i):
        c = Client(srv.port, timeout=30)
        body = submit_body(agents[i % k][1])
        for _ in range(per):
            try:
                st, _b = c.request("POST", "/jobs", TOKEN, body)
                if st != 200:
                    with lock:
                        prefill_err[0] += 1
            except Exception:
                with lock:
                    prefill_err[0] += 1
        c.close()
    for t in run_threads(k, fill):
        t.join()
    done = [0] * k
    last = [0.0] * k
    errs = [0]
    start = time.perf_counter()
    stop_at = start + a.duration

    def agent(i):
        key = agents[i][0]
        c = Client(srv.port, timeout=30)
        while time.perf_counter() < stop_at:
            try:
                st, body = c.request("GET", "/agent/jobs", key)
                if st == 204:
                    return                       # queue drained
                if st != 200:
                    errs[0] += 1
                    continue
                jid = json.loads(body)["job_id"]
                st, _b = c.request("GET", f"/agent/jobs/{jid}/payload", key)
                st2, _b = c.request("POST", f"/agent/jobs/{jid}/result", key, {"ok": True})
                if st == 200 and st2 == 200:
                    done[i] += 1
                    last[i] = time.perf_counter()
                else:
                    errs[0] += 1
            except Exception:
                errs[0] += 1
        c.close()
    for t in run_threads(k, agent):
        t.join()
    total = sum(done)
    elapsed = (max(last) if total else time.perf_counter()) - start
    drained = elapsed < a.duration - 0.5
    return {"agents": k, "jobs": total, "jobs_s": total / elapsed if elapsed > 0 else 0,
            "errors": errs[0] + prefill_err[0], "drained_early": drained}


SCENARIOS = {
    "1": ("idle", sc_idle), "2": ("idle_agents", sc_idle_agents), "3": ("submit", sc_submit),
    "4": ("delivery", sc_delivery), "5": ("cycle", sc_cycle),
}


# --- agents mode --------------------------------------------------------------------------------

def sc_agent(srv, a, tmp, label, kind, ref=None, exe=None):
    """Start an agent against `srv`, idle-poll, then push jobs through it to a file:// printer."""
    work = Path(tmp) / f"agent-{label}"
    out = work / "out"
    work.mkdir(parents=True)
    out.mkdir()
    name, key = f"bench-agent-{label}", f"bench-agent-key-{label}"
    ini = ("[agent]\nserver_url = %s\napi_key = %s\nname = %s\nprinters = bench=%s\n"
           % (srv.base, key, name, out.as_uri().replace("%", "%%")))
    env = dict(os.environ, LOCALAPPDATA=str(work))  # keep the agent's crash log out of the real one
    if kind == "python":
        extract_ref(ref, "agent", work)
        script = work / "agent" / "print_agent.py"
        (script.parent / "agent.ini").write_text(ini)
        argv, cwd = [sys.executable, str(script)], script.parent
    else:
        dst = work / Path(exe).name
        shutil.copy2(exe, dst)
        (work / "agent.ini").write_text(ini)
        argv, cwd = [str(dst)], work
    proc = Proc(argv, cwd, env, work / "agent.log")
    try:
        c = Client(srv.port, timeout=10)
        pid, end = None, time.time() + 30
        while pid is None and time.time() < end:
            if proc.p.poll() is not None:
                raise RuntimeError(f"{label} agent exited:\n{(work / 'agent.log').read_text(errors='replace')}")
            try:
                st, body = c.request("GET", "/printers")
                for p in json.loads(body).get("printers", []):
                    if p["agent_name"] == name:
                        pid = p["id"]
            except Exception:
                c.close()
            time.sleep(0.2)
        if pid is None:
            raise RuntimeError(f"{label} agent never registered")
        time.sleep(1)
        m0 = proc.metrics()
        time.sleep(a.idle_window)
        m1 = proc.metrics()
        lats = []
        for _ in range(a.agent_jobs):
            time.sleep(random.uniform(0.05, 1.1))  # random phase against a 1 s DB-polling server
            t0 = time.perf_counter()
            st, body = c.request("POST", "/jobs", TOKEN, submit_body(pid))
            if st != 200:
                raise RuntimeError(f"agent job submit: {st}")
            f = out / f"job-{json.loads(body)['job_id']}.prn"
            while not (f.exists() and f.stat().st_size > 0):
                if time.perf_counter() - t0 > 30:
                    raise RuntimeError("job file never appeared")
                time.sleep(0.002)
            lats.append(time.perf_counter() - t0)
        c.close()
        m2 = proc.metrics()
        lats.sort()
        return {"rss_mb": m1["rss_mb"], "threads": m1["threads"], "idle_cpu_s": m1["cpu_s"] - m0["cpu_s"],
                "jobs": len(lats), "e2e_p50_ms": pct(lats, 50) * 1000, "e2e_p95_ms": pct(lats, 95) * 1000,
                "e2e_max_ms": lats[-1] * 1000, "total_s": sum(lats), "peak_rss_mb": m2["peak_rss_mb"]}
    finally:
        proc.stop()


# --- running, reporting -------------------------------------------------------------------------

def run_server(kind, label, a, tmp):
    srv = Server(kind, label, tmp, python_ref=a.python_ref, binary=a.rust)
    res = {"label": label}
    try:
        srv.wait_ready()
        wanted = a.scenarios.split(",")
        # Order: 2 drains for one long-poll period (the Python server's waiting handlers outlive their
        # clients), and 3 goes last because it leaves a job backlog behind.
        for sid in [s for s in ("1", "2", "4", "5", "3") if s in wanted]:
            name, fn = SCENARIOS[sid]
            print(f"[{label}] scenario {sid}: {name} ...", file=sys.stderr, flush=True)
            res[name] = fn(srv, a)
            if sid == "2" and any(s in wanted for s in ("4", "5", "3")):
                print(f"[{label}] draining abandoned long-polls ({LONG_POLL_S + 2:.0f} s) ...",
                      file=sys.stderr, flush=True)
                time.sleep(LONG_POLL_S + 2)
        res["peak_rss_mb"] = srv.proc.metrics()["peak_rss_mb"]
        if a.agents:
            res["agents"] = {}
            if a.python_agent:
                print(f"[{label}] python agent ...", file=sys.stderr, flush=True)
                res["agents"]["python-agent"] = sc_agent(srv, a, tmp, f"{label}-py", "python", ref=a.python_ref)
            if a.rust_agent:
                print(f"[{label}] rust agent ...", file=sys.stderr, flush=True)
                res["agents"]["rust-agent"] = sc_agent(srv, a, tmp, f"{label}-rs", "rust", exe=a.rust_agent)
        return res
    finally:
        srv.proc.stop()


# metric key -> (title, format, lower_is_better)
ROWS = [
    ("idle.rss_mb", "1 idle: RSS (MB)", "{:.1f}"),
    ("idle.threads", "1 idle: threads", "{:.0f}"),
    ("idle_agents.rss_mb", "2 idle agents: RSS (MB)", "{:.1f}"),
    ("idle_agents.threads", "2 idle agents: threads", "{:.0f}"),
    ("idle_agents.cpu_s", "2 idle agents: CPU s in window", "{:.2f}"),
    ("idle_agents.cpu_pct", "2 idle agents: CPU % of one core", "{:.1f}"),
    ("submit.req_s", "3 submit: req/s", "{:.0f}"),
    ("submit.p50_ms", "3 submit: p50 (ms)", "{:.2f}"),
    ("submit.p95_ms", "3 submit: p95 (ms)", "{:.2f}"),
    ("submit.p99_ms", "3 submit: p99 (ms)", "{:.2f}"),
    ("submit.errors", "3 submit: errors", "{:.0f}"),
    ("delivery.p50_ms", "4 delivery: p50 (ms)", "{:.1f}"),
    ("delivery.p95_ms", "4 delivery: p95 (ms)", "{:.1f}"),
    ("delivery.max_ms", "4 delivery: max (ms)", "{:.1f}"),
    ("cycle.jobs_s", "5 cycle: jobs/s", "{:.0f}"),
    ("cycle.errors", "5 cycle: errors", "{:.0f}"),
    ("peak_rss_mb", "6 peak RSS (MB)", "{:.1f}"),
]


def dig(d, dotted):
    for part in dotted.split("."):
        if not isinstance(d, dict) or part not in d:
            return None
        d = d[part]
    return d


def table(headers, rows):
    lines = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    lines += ["| " + " | ".join(r) + " |" for r in rows]
    return "\n".join(lines)


def ratio(a, b):
    return "" if a is None or b is None or not a else f"{b / a:.2f}x"


def report(results):
    out = []
    labels = [r["label"] for r in results]
    rows = []
    for key, title, fmt in ROWS:
        vals = [dig(r, key) for r in results]
        if all(v is None for v in vals):
            continue
        cells = [fmt.format(v) if v is not None else "-" for v in vals]
        if len(results) == 2:
            cells.append(ratio(vals[0], vals[1]))
        rows.append([title] + cells)
    out.append(table(["metric"] + labels + (["ratio (" + labels[1] + " / " + labels[0] + ")"]
                                            if len(results) == 2 else []), rows))
    notes = []
    for r in results:
        if dig(r, "submit.client_bound"):
            notes.append(f"NOTE [{r['label']}]: the load generator used {r['submit']['client_cpu_pct']:.0f}% "
                         "of one core in scenario 3 (Python client, GIL) - req/s is a lower bound, "
                         "the client may be the bottleneck.")
        if dig(r, "cycle.drained_early"):
            notes.append(f"NOTE [{r['label']}]: scenario 5 drained its backlog before the timer ran out; "
                         "jobs/s is over the drain time (raise --prefill for a longer run).")
    agent_rows = []
    for r in results:
        for name, m in (r.get("agents") or {}).items():
            agent_rows.append([f"{name} on {r['label']}", f"{m['rss_mb']:.1f}", f"{m['threads']}",
                               f"{m['idle_cpu_s']:.3f}", f"{m['e2e_p50_ms']:.1f}", f"{m['e2e_p95_ms']:.1f}",
                               f"{m['e2e_max_ms']:.1f}"])
    if agent_rows:
        out.append("\n" + table(["agent", "RSS (MB)", "threads", "idle CPU s", "e2e p50 (ms)", "e2e p95 (ms)",
                                 "e2e max (ms)"], agent_rows))
    return "\n".join(out + ([""] + notes if notes else []))


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--python-ref", default="v2.5.1", help="git ref of the Python server/agent (default v2.5.1)")
    p.add_argument("--rust", help="Rust server binary")
    p.add_argument("--compare", action="store_true", help="run the Python and the Rust server back to back")
    p.add_argument("--json", help="write the raw results here")
    p.add_argument("--scenarios", default="1,2,3,4,5", help="comma list of scenario numbers (default all)")
    p.add_argument("--n-agents", type=int, default=200, help="scenario 2: concurrent long-polling agents")
    p.add_argument("--idle-window", type=float, default=20, help="scenarios 2 and --agents: seconds to idle")
    p.add_argument("--clients", type=int, default=16, help="scenario 3: client threads")
    p.add_argument("--duration", type=float, default=10, help="scenarios 3 and 5: seconds")
    p.add_argument("--samples", type=int, default=50, help="scenario 4: delivery samples")
    p.add_argument("--cycle-agents", type=int, default=4, help="scenario 5: agents (K)")
    p.add_argument("--prefill", type=int, default=1000, help="scenario 5: queued jobs per agent")
    p.add_argument("--agents", action="store_true", help="also benchmark agents against the server")
    p.add_argument("--no-python-agent", dest="python_agent", action="store_false")
    p.add_argument("--rust-agent", help="Rust agent executable (enables the Rust agent run)")
    p.add_argument("--agent-jobs", type=int, default=50, help="--agents: jobs pushed through each agent")
    a = p.parse_args(argv)
    if a.compare and not a.rust:
        p.error("--compare needs --rust")
    runs = []
    if a.compare or not a.rust:
        runs.append(("python", f"python {a.python_ref}"))
    if a.rust:
        runs.append(("rust", "rust"))
    tmp = tempfile.mkdtemp(prefix="printpapi-bench-")
    try:
        results = [run_server(kind, label, a, tmp) for kind, label in runs]
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    print(report(results))
    if a.json:
        Path(a.json).write_text(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
