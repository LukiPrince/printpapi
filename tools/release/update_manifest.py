#!/usr/bin/env python3
# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""The signed update file of a release: `printpapi-update.txt`.

  tag TAG                       print N.N.N for a tag `vN.N.N`, exit 1 for anything else (a release
                                candidate, a branch name): such a tag publishes no update.
  build --version V --dist DIR --out FILE
                                the JSON payload (schema 1) of the files in DIR. V must be plain
                                N.N.N and equal the workspace version in Cargo.toml. A target needs
                                its agent and at least one tray role, else it is left out.
  sign --payload FILE --keys FILE --out FILE [--secret-env NAME]
                                one `sig <128 hex>` line per private key in the environment variable
                                (default UPDATE_SIGNING_KEY: one or more PKCS#8 PEM blocks), an
                                empty line, the payload. Every key's public half has to be in the
                                key file (crates/agent/update-keys.txt), and every signature is
                                verified before it is written.

The private keys come from the environment only, never from an argument, and are written to a
0600 file in a temporary folder that is always removed. Nothing here prints a key.

Needs OpenSSL 3 (`openssl pkeyutl -sign -rawin` signs Ed25519 over the bytes themselves,
https://docs.openssl.org/3.0/man1/openssl-pkeyutl/). Python 3 stdlib only.
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = 1
# std::env::consts::OS-ARCH of the targets an update can name; one without files is left out
TARGETS = [
    "windows-x86_64",
    "windows-aarch64",
    "linux-x86_64",
    "linux-aarch64",
    "macos-aarch64",
    "macos-x86_64",
]
SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
PEM = re.compile(r"-----BEGIN PRIVATE KEY-----.*?-----END PRIVATE KEY-----", re.S)
MAX_FILE = 300 * 1024 * 1024


class ManifestError(Exception):
    pass


def release_version(tag):
    """N.N.N for a tag vN.N.N, else None (rc tags and branch names publish no update)."""
    m = re.fullmatch(r"v(\d+\.\d+\.\d+)", tag or "")
    return m.group(1) if m else None


def workspace_version(cargo_toml):
    """The version of [workspace.package] in Cargo.toml."""
    text = Path(cargo_toml).read_text(encoding="utf-8")
    section = re.search(r"(?ms)^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)", text)
    m = re.search(r'(?m)^version\s*=\s*"([^"]+)"', section.group(1)) if section else None
    if not m:
        raise ManifestError(f"no workspace version in {cargo_toml}")
    return m.group(1)


def roles_of(target):
    """The files an update names for `target`, as {role: file name}."""
    exe = ".exe" if target.startswith("windows") else ""
    roles = {"agent": f"printpapi-update-agent-{target}{exe}"}
    if target.startswith("windows"):
        roles["tray"] = f"printpapi-update-tray-{target}.exe"
    elif target.startswith("linux"):
        roles["tray"] = f"printpapi-update-tray-{target}"
        roles["appimage"] = f"printpapi-update-appimage-{target}"
    else:
        roles["app"] = f"printpapi-update-app-{target}.zip"
    return roles


def build(version, dist, cargo_toml=None):
    """The payload of a release: only targets with their agent and a tray role."""
    if not SEMVER.match(version):
        raise ManifestError(f"{version!r} is not N.N.N")
    own = workspace_version(cargo_toml or ROOT / "Cargo.toml")
    if version != own:
        raise ManifestError(f"the tag says {version}, the workspace version is {own}")
    dist = Path(dist)
    targets = {}
    for target in TARGETS:
        found = {}
        for role, name in roles_of(target).items():
            path = dist / name
            if not path.is_file():
                continue
            data = path.read_bytes()
            if not 0 < len(data) <= MAX_FILE:
                raise ManifestError(f"{name} is empty or over 300 MiB")
            found[role] = {"file": name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        if "agent" in found and len(found) > 1:
            targets[target] = found
    if not targets:
        raise ManifestError(f"no target in {dist} has an agent and a tray file")
    return {"schema": SCHEMA, "version": version, "targets": targets}


def payload_bytes(payload):
    return json.dumps(payload, separators=(",", ":"), sort_keys=False).encode("utf-8")


def public_keys(keys_text):
    """The hex keys of crates/agent/update-keys.txt (comments and blank lines skipped)."""
    keys = []
    for line in keys_text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if not re.fullmatch(r"[0-9a-f]{64}", line):
            raise ManifestError("the key file has a line that is no 64 digit lower-case hex key")
        keys.append(line)
    return keys


def openssl(*args, check=True):
    try:
        return subprocess.run(["openssl", *args], capture_output=True, check=check)
    except FileNotFoundError:
        raise ManifestError("openssl is not installed")
    except subprocess.CalledProcessError as e:
        # the message of OpenSSL can name a path but never holds the key
        raise ManifestError(f"openssl {args[0]} failed: {e.stderr.decode(errors='replace').strip()[:200]}")


def sign(payload, keys_text, secret_text, tmp_root=None):
    """The signed file: `sig` lines, an empty line, the payload (a copy of `payload`, byte for byte)."""
    blocks = PEM.findall(secret_text or "")
    if not blocks:
        raise ManifestError("the signing secret holds no PKCS#8 private key")
    listed = set(public_keys(keys_text))
    lines = []
    with tempfile.TemporaryDirectory(prefix="update-sign-", dir=tmp_root) as tmp:
        tmp = Path(tmp)
        data_file = tmp / "payload"
        data_file.write_bytes(payload)
        for n, block in enumerate(blocks, 1):
            key_file = tmp / f"key{n}.pem"
            fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w") as f:
                f.write(block + "\n")
            der = openssl("pkey", "-in", str(key_file), "-pubout", "-outform", "DER").stdout
            public = der[-32:].hex()
            if len(der) < 32 or public not in listed:
                raise ManifestError(f"key {n} of the secret is not in the key file (crates/agent/update-keys.txt)")
            sig_file = tmp / f"sig{n}"
            openssl("pkeyutl", "-sign", "-inkey", str(key_file), "-rawin", "-in", str(data_file),
                    "-out", str(sig_file))
            sig = sig_file.read_bytes()
            if len(sig) != 64:
                raise ManifestError(f"key {n} did not make a 64 byte Ed25519 signature")
            pub_pem = tmp / f"pub{n}.pem"
            openssl("pkey", "-in", str(key_file), "-pubout", "-out", str(pub_pem))
            ok = openssl("pkeyutl", "-verify", "-pubin", "-inkey", str(pub_pem), "-rawin", "-in",
                         str(data_file), "-sigfile", str(sig_file), check=False)
            if ok.returncode != 0:
                raise ManifestError(f"the signature of key {n} does not verify")
            lines.append(f"sig {sig.hex()}")
    return ("\n".join(lines) + "\n\n").encode("ascii") + payload


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("tag")
    t.add_argument("tag")
    b = sub.add_parser("build")
    b.add_argument("--version", required=True)
    b.add_argument("--dist", required=True)
    b.add_argument("--out", required=True)
    b.add_argument("--cargo", default=str(ROOT / "Cargo.toml"))
    s = sub.add_parser("sign")
    s.add_argument("--payload", required=True)
    s.add_argument("--keys", default=str(ROOT / "crates" / "agent" / "update-keys.txt"))
    s.add_argument("--out", required=True)
    s.add_argument("--secret-env", default="UPDATE_SIGNING_KEY")
    args = ap.parse_args(argv)
    try:
        if args.cmd == "tag":
            version = release_version(args.tag)
            if version is None:
                return 1
            print(version)
        elif args.cmd == "build":
            payload = build(args.version, args.dist, args.cargo)
            Path(args.out).write_bytes(payload_bytes(payload))
            print(f"{len(payload['targets'])} targets: {', '.join(payload['targets'])}")
        else:
            signed = sign(Path(args.payload).read_bytes(),
                          Path(args.keys).read_text(encoding="utf-8"),
                          os.environ.get(args.secret_env, ""))
            Path(args.out).write_bytes(signed)
            print(f"signed with {signed.count(b'sig ')} key(s)")
    except ManifestError as e:
        print(f"update_manifest: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
