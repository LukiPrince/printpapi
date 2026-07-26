# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Password hashing, session tokens and login throttling — pure, no IO, no DB.

Passwords use stdlib `hashlib.scrypt` (memory-hard, ~16 MB / ~50 ms per attempt at these
parameters), so no bcrypt/argon2 dependency enters a stdlib-only server. The stored string
carries its own parameters, so raising the cost later still verifies old hashes.
"""
import base64
import hashlib
import hmac
import secrets
import threading
import time

MIN_PASSWORD_LEN = 8
MAX_PASSWORD_LEN = 1024                 # bounds the work an unauthenticated login can buy
SESSION_TTL_S = 30 * 24 * 3600          # fixed lifetime; no sliding renewal
_N, _R, _P, _DKLEN = 2 ** 14, 8, 1, 32


def hash_password(password, *, n=_N, r=_R, p=_P):
    """`scrypt$n$r$p$salt$hash`, both parts base64. Raises ValueError on a too-short password —
    the one place every caller (root create, self-serve create, password change) routes through."""
    if not isinstance(password, str) or len(password) < MIN_PASSWORD_LEN:
        raise ValueError(f"password must be at least {MIN_PASSWORD_LEN} characters")
    if len(password) > MAX_PASSWORD_LEN:
        raise ValueError(f"password must be at most {MAX_PASSWORD_LEN} characters")
    salt = secrets.token_bytes(16)
    dk = hashlib.scrypt(password.encode(), salt=salt, n=n, r=r, p=p, dklen=_DKLEN)
    return "$".join(("scrypt", str(n), str(r), str(p),
                     base64.b64encode(salt).decode(), base64.b64encode(dk).decode()))


def verify_password(password, stored):
    """False for a wrong password *and* for a NULL/garbage stored hash (a legacy user row has
    password_hash NULL and must never authenticate)."""
    if not password or not isinstance(stored, str) or len(password) > MAX_PASSWORD_LEN:
        return False
    try:
        scheme, n, r, p, salt_b64, hash_b64 = stored.split("$")
        if scheme != "scrypt":
            return False
        salt, expected = base64.b64decode(salt_b64), base64.b64decode(hash_b64)
        dk = hashlib.scrypt(password.encode(), salt=salt, n=int(n), r=int(r), p=int(p),
                            dklen=len(expected))
    except (ValueError, TypeError):
        return False
    return hmac.compare_digest(dk, expected)


def new_session_token():
    """The browser session credential. The `sess_` prefix keeps it recognizable in a log or a
    bug report as a session rather than a machine API key."""
    return "sess_" + secrets.token_urlsafe(32)


class LoginLimiter:
    """Failed-login throttle, keyed by e-mail.

    # ponytail: per-process and in-memory (a restart forgets, several server processes each keep
    # their own count), no IP dimension, and bounded — a big enough spray of invented addresses
    # evicts counters rather than growing memory. scrypt already caps a single process at ~20
    # guesses/s; move the counters into SQLite if a deployment ever runs more than one process.
    """

    def __init__(self, max_fails=10, window_s=900, max_tracked=10_000):
        self.max_fails, self.window_s, self.max_tracked = max_fails, window_s, max_tracked
        self._fails = {}                    # key -> (count, window_start)
        self._lock = threading.Lock()

    def tracked(self):
        return len(self._fails)

    def _now(self, now):
        return time.time() if now is None else now

    def allow(self, key, now=None):
        now = self._now(now)
        with self._lock:
            count, start = self._fails.get(key, (0, now))
            return now - start >= self.window_s or count < self.max_fails

    def fail(self, key, now=None):
        now = self._now(now)
        with self._lock:
            count, start = self._fails.get(key, (0, now))
            if now - start >= self.window_s:
                count, start = 0, now       # window rolled over — start counting again
            self._fails[key] = (count + 1, start)
            if len(self._fails) > self.max_tracked:
                # Spraying invented addresses must not grow this dict without bound. Rolled-over
                # entries are worthless (they no longer block anything), so they go first; if
                # every entry is live, the oldest windows go — those expire soonest anyway.
                stale = [k for k, (_, s) in self._fails.items() if now - s >= self.window_s]
                for k in stale or sorted(self._fails, key=lambda k: self._fails[k][1])[
                        :len(self._fails) - self.max_tracked + 1]:
                    if k != key:
                        del self._fails[k]

    def succeed(self, key):
        with self._lock:
            self._fails.pop(key, None)
