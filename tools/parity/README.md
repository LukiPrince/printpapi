# Parity harness

`parity.py` is a black-box check that the Rust server answers exactly like the Python one. Python 3
stdlib only; it needs `git` (for the reference server) and, for the real comparison, a built
`printpapi` binary.

## What it proves

Server A is the Python server taken from a git ref (default `v2.5.1`, extracted with
`git archive`, so it keeps working after `app/` is gone from the tree). Server B is the Rust binary.
Both get a free port, a fresh temp DB and identical environment (token, open signup, two plans,
billing secret, `PUBLIC_URL`, no `SMTP_HOST`, so reset mails land on stderr).

One scripted scenario (about 790 steps, every route family of `app/server.py`) runs against both.
Per step it compares:

- the status code exactly,
- the Content-Type main type,
- JSON bodies structurally (key order ignored, int vs float kept), after replacing timestamps,
  issued keys/tokens and per-server URLs with placeholders (the real values are carried forward
  inside each run),
- text bodies (`/metrics`, the reset mail from stderr) and binary bodies (agent payloads, packing
  slip PDFs) byte for byte,
- what a local stub server saw (payload fetches incl. User-Agent, `*_uri_post` bodies, webhook
  deliveries).

Ids match because both databases start empty and see the same calls in the same order.
Dashboard steps compare status, content type and Cache-Control only.

The 25 s long-poll timeout is not tested (only the immediate-job case).

## Running

```
python tools/parity/parity.py                      # Python v2.5.1 vs target/release/printpapi(.exe)
python tools/parity/parity.py --rust path/to/printpapi
python tools/parity/parity.py --python-dir ../old-checkout   # A from a checkout instead of a ref
python tools/parity/parity.py --a-only             # self-validation: B is a second Python server
python tools/parity/parity.py --b "cmd args"       # custom B command (runs in A's directory)
python tools/parity/parity.py --b-python-dir DIR   # B = Python server of another checkout
python tools/parity/parity.py --json out.json      # machine-readable results
python tools/parity/parity.py --events             # also wait ~75 s for the agent-offline event_url POSTs
python tools/parity/parity.py --db-compat          # Python -> Rust -> Python on one DB file
```

Output is one line per step (`PASS` / `DIFF` / `KNOWN`), unified diffs for every `DIFF`, and a
summary. Exit code 1 on any unexpected `DIFF`. A run takes about 13 s (about 75 s with `--events`).
Without a Rust binary the run is skipped with a message (exit 0).

## Known differences

`KNOWN_DIFFS` at the top of `parity.py` maps a step name to the reason for a deliberate deviation;
such diffs are printed but do not fail the run. Steps flagged `crash_ok` are inputs on which the
Python server crashes without answering (garbage JSON types, unbindable printer ids): if B answers
cleanly there, the step is reported as `KNOWN` automatically.

## `--db-compat`

Python server on a DB (org, user, key, agent, jobs with options) -> stop -> Rust server on the same
file (login with the Python scrypt hash, session and key auth, job list, claim, writes of its own)
-> stop -> Python server again (login with the Rust hash, key auth, claims and results). Proves the
schema and every stored format (hashes, JSON columns, BLOB payloads) round-trip both ways.
