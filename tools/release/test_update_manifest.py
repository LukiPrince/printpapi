# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Tests of update_manifest.py. The signing tests need OpenSSL 3 and are skipped without it."""
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import update_manifest as um  # noqa: E402

needs_openssl = pytest.mark.skipif(shutil.which("openssl") is None, reason="no openssl")

CARGO = '[workspace]\nmembers = ["a"]\n\n[workspace.package]\nversion = "3.7.0"\nedition = "2024"\n\n[profile.release]\nversion = "x"\n'


def make_dist(tmp, targets):
    """Files for the named targets: {target: [role, ...]}."""
    dist = Path(tmp) / "dist"
    dist.mkdir()
    for target, roles in targets.items():
        names = um.roles_of(target)
        for role in roles:
            (dist / names[role]).write_bytes(f"{target} {role}".encode())
    return dist


def cargo(tmp, text=CARGO):
    path = Path(tmp) / "Cargo.toml"
    path.write_text(text, encoding="utf-8")
    return path


def test_a_tag_gives_its_version_and_nothing_else_does(capsys):
    assert um.release_version("v3.7.0") == "3.7.0"
    for bad in ["v3.7.0-rc1", "3.7.0", "main", "v3.7", "v3.7.0.1", "", None, "v3.7.0 "]:
        assert um.release_version(bad) is None, bad
    assert um.main(["tag", "v3.7.0"]) == 0
    assert capsys.readouterr().out.strip() == "3.7.0"
    assert um.main(["tag", "v3.7.0-rc1"]) == 1


def test_the_workspace_version_is_the_one_of_workspace_package():
    with tempfile.TemporaryDirectory() as tmp:
        assert um.workspace_version(cargo(tmp)) == "3.7.0"
        with pytest.raises(um.ManifestError):
            um.workspace_version(cargo(tmp, "[package]\nversion = \"1.0.0\"\n"))


def test_targets_with_an_agent_and_a_tray_role_are_listed_and_the_rest_are_left_out():
    with tempfile.TemporaryDirectory() as tmp:
        dist = make_dist(tmp, {
            "windows-x86_64": ["agent", "tray"],
            "linux-x86_64": ["agent", "tray", "appimage"],
            "linux-aarch64": ["agent"],            # no tray: left out
            "macos-aarch64": ["agent", "app"],
            "macos-x86_64": ["app"],               # no agent: left out
        })
        payload = um.build("3.7.0", dist, cargo(tmp))
        assert payload["schema"] == 1 and payload["version"] == "3.7.0"
        assert list(payload["targets"]) == ["windows-x86_64", "linux-x86_64", "macos-aarch64"]
        entry = payload["targets"]["linux-x86_64"]
        assert sorted(entry) == ["agent", "appimage", "tray"]
        agent = entry["agent"]
        assert agent["file"] == "printpapi-update-agent-linux-x86_64"
        assert agent["size"] == len(b"linux-x86_64 agent")
        assert agent["sha256"] == hashlib.sha256(b"linux-x86_64 agent").hexdigest()
        assert payload["targets"]["windows-x86_64"]["agent"]["file"].endswith(".exe")


def test_windows_arm64_is_a_target_from_the_start():
    with tempfile.TemporaryDirectory() as tmp:
        dist = make_dist(tmp, {"windows-aarch64": ["agent", "tray"]})
        assert "windows-aarch64" in um.build("3.7.0", dist, cargo(tmp))["targets"]


def test_a_version_that_is_not_the_workspace_version_or_not_n_n_n_is_refused():
    with tempfile.TemporaryDirectory() as tmp:
        dist = make_dist(tmp, {"windows-x86_64": ["agent", "tray"]})
        for bad in ["3.7.1", "3.7", "3.7.0-rc1", "v3.7.0", ""]:
            with pytest.raises(um.ManifestError):
                um.build(bad, dist, cargo(tmp))


def test_a_release_without_any_complete_target_is_refused():
    with tempfile.TemporaryDirectory() as tmp:
        dist = make_dist(tmp, {"linux-x86_64": ["agent"]})
        with pytest.raises(um.ManifestError):
            um.build("3.7.0", dist, cargo(tmp))


def test_the_payload_is_compact_json_the_agent_parses():
    with tempfile.TemporaryDirectory() as tmp:
        dist = make_dist(tmp, {"linux-x86_64": ["agent", "tray"]})
        data = um.payload_bytes(um.build("3.7.0", dist, cargo(tmp)))
        assert b" " not in data and b"\n" not in data
        assert json.loads(data)["targets"]["linux-x86_64"]["tray"]["size"] > 0


def test_the_key_file_is_read_strictly():
    key = "ab" * 32
    assert um.public_keys(f"# c\n\n{key}\n") == [key]
    for bad in ["AB" * 32, "ab" * 31, f"{key} # x", "zz" * 32]:
        with pytest.raises(um.ManifestError):
            um.public_keys(bad)


def new_key(tmp, name):
    pem = Path(tmp) / f"{name}.pem"
    subprocess.run(["openssl", "genpkey", "-algorithm", "ed25519", "-out", str(pem)], check=True,
                   capture_output=True)
    der = subprocess.run(["openssl", "pkey", "-in", str(pem), "-pubout", "-outform", "DER"],
                         check=True, capture_output=True).stdout
    return pem.read_text(), der[-32:].hex()


def verify(signed, public_hex):
    """What the agent does, with OpenSSL: the payload verifies under the key with one of the lines."""
    head, payload = signed.split(b"\n\n", 1)
    lines = head.decode().split("\n")
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        (tmp / "payload").write_bytes(payload)
        # a raw Ed25519 public key as SubjectPublicKeyInfo: the fixed prefix and the 32 bytes
        spki = bytes.fromhex("302a300506032b6570032100") + bytes.fromhex(public_hex)
        (tmp / "pub.der").write_bytes(spki)
        for line in lines:
            assert line.startswith("sig ") and len(line) == 4 + 128
            (tmp / "sig").write_bytes(bytes.fromhex(line[4:]))
            ok = subprocess.run(["openssl", "pkeyutl", "-verify", "-pubin", "-keyform", "DER",
                                 "-inkey", str(tmp / "pub.der"), "-rawin", "-in", str(tmp / "payload"),
                                 "-sigfile", str(tmp / "sig")], capture_output=True)
            if ok.returncode == 0:
                return True
    return False


@needs_openssl
def test_the_signed_file_verifies_and_keeps_the_payload_byte_for_byte():
    with tempfile.TemporaryDirectory() as tmp:
        pem, public = new_key(tmp, "k")
        payload = b'{"schema":1,"version":"3.7.0","targets":{}}'
        signed = um.sign(payload, f"# keys\n{public}\n", pem)
        assert signed.endswith(b"\n\n" + payload)
        assert signed.count(b"\nsig ") == 0 and signed.startswith(b"sig ")
        assert verify(signed, public)
        # another key does not verify it
        assert not verify(signed, "00" * 32)


@needs_openssl
def test_two_keys_give_two_lines_and_either_key_verifies():
    with tempfile.TemporaryDirectory() as tmp:
        pem1, pub1 = new_key(tmp, "a")
        pem2, pub2 = new_key(tmp, "b")
        signed = um.sign(b"payload", f"{pub1}\n{pub2}\n", pem1 + "\n" + pem2)
        assert signed.split(b"\n\n", 1)[0].count(b"\n") == 1
        assert verify(signed, pub1) and verify(signed, pub2)


@needs_openssl
def test_a_key_that_is_not_in_the_key_file_is_refused_without_naming_it():
    with tempfile.TemporaryDirectory() as tmp:
        pem, public = new_key(tmp, "k")
        other = "11" * 32
        with pytest.raises(um.ManifestError) as e:
            um.sign(b"x", f"{other}\n", pem)
        assert "not in the key file" in str(e.value)
        assert public not in str(e.value) and "BEGIN" not in str(e.value)
        # a key file without any key signs nothing
        with pytest.raises(um.ManifestError):
            um.sign(b"x", "# comments only\n", pem)
        with pytest.raises(um.ManifestError):
            um.sign(b"x", f"{public}\n", "")


@needs_openssl
def test_the_temporary_key_files_are_always_removed():
    with tempfile.TemporaryDirectory() as tmp, tempfile.TemporaryDirectory() as root:
        pem, public = new_key(tmp, "k")
        um.sign(b"x", f"{public}\n", pem, tmp_root=root)
        assert list(Path(root).iterdir()) == []
        with pytest.raises(um.ManifestError):
            um.sign(b"x", "11" * 32 + "\n", pem, tmp_root=root)
        assert list(Path(root).iterdir()) == []


@needs_openssl
def test_the_command_line_reads_the_secret_from_the_environment_only(monkeypatch):
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        pem, public = new_key(tmp, "k")
        (tmp / "keys.txt").write_text(public + "\n")
        (tmp / "payload.json").write_bytes(b'{"schema":1}')
        monkeypatch.setenv("UPDATE_SIGNING_KEY", pem)
        out = tmp / "printpapi-update.txt"
        assert um.main(["sign", "--payload", str(tmp / "payload.json"), "--keys", str(tmp / "keys.txt"),
                        "--out", str(out)]) == 0
        assert verify(out.read_bytes(), public)
        # an empty secret signs nothing and writes nothing
        monkeypatch.setenv("UPDATE_SIGNING_KEY", "")
        out.unlink()
        assert um.main(["sign", "--payload", str(tmp / "payload.json"), "--keys", str(tmp / "keys.txt"),
                        "--out", str(out)]) == 1
        assert not out.exists()
