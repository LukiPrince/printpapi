# printpapi benchmark

`bench.py` (Python 3, stdlib only) measures resource use and speed of the Python server
(`app/`, extracted from a git ref) and of the Rust server binary, and optionally of the Python
and Rust agents. Process metrics come from `ctypes`/kernel32 on Windows and `/proc` on Linux.

```
python tools/bench/bench.py                                   # Python server at v2.5.1
python tools/bench/bench.py --rust target/release/printpapi-server.exe
python tools/bench/bench.py --compare --rust <server-bin> --json out.json
python tools/bench/bench.py --compare --rust <server-bin> --agents --rust-agent <agent-bin>
```

Each server gets a fresh temp DB, `PRINTAPI_TOKEN`, a free port (`PRINT_DB`, `PRINT_PORT`).
`--compare` runs both back to back and prints ratios (rust / python; below 1 is better for
memory, CPU and latency, above 1 for throughput).

Scenarios (`--scenarios 1,2,...`): 1 idle RSS/threads; 2 `--n-agents` (200) concurrent
long-polls for `--idle-window` (20) s, with the server's CPU seconds; 3 submit throughput with
`--clients` (16) threads for `--duration` (10) s; 4 submit-to-agent latency, `--samples` (50);
5 claim/payload/result loop with `--cycle-agents` (4) agents over `--prefill` (1000) queued jobs
each; 6 peak RSS of the whole run.

`--agents` starts the Python agent (from the ref, `--no-python-agent` to skip) and/or
`--rust-agent`, each with its own `agent.ini` and a `file://` printer, idles them for
`--idle-window` s, then pushes `--agent-jobs` (50) jobs through each and times submit-to-file.

Caveats
- The load generator is Python threads (GIL). If it uses more than 90% of a core in scenario 3
  the output says so: req/s is then a lower bound for a fast server.
- Scenario order is fixed (1, 2, drain, 4, 5, 3) so earlier runs do not slow later ones, but the
  Python server's claim query scans the jobs table, so scenario 5 falls as `--prefill` grows.
  Compare runs with identical sizes only.
- Scenario 2 waits 27 s afterwards so the Python server's abandoned long-poll handlers finish.
