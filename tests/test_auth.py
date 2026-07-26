import pytest
from app import auth


def test_hash_verify_roundtrip_and_wrong_password():
    stored = auth.hash_password("correct horse battery")
    assert stored.startswith("scrypt$")
    assert auth.verify_password("correct horse battery", stored)
    assert not auth.verify_password("wrong horse battery", stored)


def test_hash_is_salted_so_the_same_password_hashes_differently():
    a = auth.hash_password("same password")
    b = auth.hash_password("same password")
    assert a != b
    assert auth.verify_password("same password", a)
    assert auth.verify_password("same password", b)


def test_verify_rejects_missing_or_malformed_stored_hash():
    # A legacy user row has password_hash NULL — it must never authenticate.
    assert not auth.verify_password("x", None)
    assert not auth.verify_password("x", "")
    assert not auth.verify_password("x", "scrypt$junk")
    assert not auth.verify_password("x", "bcrypt$2$3$4$5")


def test_short_password_is_refused_at_hash_time():
    with pytest.raises(ValueError):
        auth.hash_password("short")


def test_session_tokens_are_prefixed_and_unique():
    a, b = auth.new_session_token(), auth.new_session_token()
    assert a.startswith("sess_") and b.startswith("sess_")
    assert a != b


def test_limiter_blocks_after_max_failures_and_forgets_after_the_window():
    lim = auth.LoginLimiter(max_fails=3, window_s=900)
    for i in range(3):
        assert lim.allow("a@b.c", now=100 + i)
        lim.fail("a@b.c", now=100 + i)
    assert not lim.allow("a@b.c", now=110)
    assert lim.allow("other@b.c", now=110)          # per-identity, not global
    assert lim.allow("a@b.c", now=100 + 900)        # window rolled over


def test_limiter_clears_the_count_on_a_successful_login():
    lim = auth.LoginLimiter(max_fails=2, window_s=900)
    lim.fail("a@b.c", now=100)
    lim.succeed("a@b.c")
    lim.fail("a@b.c", now=101)
    assert lim.allow("a@b.c", now=102)              # only one failure since the success


def test_an_absurdly_long_password_is_refused_instead_of_hashed():
    # Bounded work per login attempt: a multi-megabyte "password" must not buy CPU time.
    huge = "x" * (auth.MAX_PASSWORD_LEN + 1)
    with pytest.raises(ValueError):
        auth.hash_password(huge)
    assert not auth.verify_password(huge, auth.hash_password("a fine password"))


def test_the_limiter_forgets_stale_identities_instead_of_growing_forever():
    lim = auth.LoginLimiter(max_fails=3, window_s=60, max_tracked=10)
    for i in range(50):
        lim.fail(f"spray-{i}@b.c", now=100)
    assert lim.tracked() <= 10
    lim.fail("victim@b.c", now=100)
    lim.fail("victim@b.c", now=1000)                 # window rolled over -> counting restarts
    assert lim.allow("victim@b.c", now=1000)
