# Security Policy

## Supported versions

The latest state of `main` is the supported version.

## Reporting a vulnerability

Please report vulnerabilities **privately** via GitHub's
[security advisories](https://github.com/LukiPrince/printpapi/security/advisories/new)
("Report a vulnerability" on the repo's Security tab). Do not open a public issue for security
problems.

You can expect an initial response within a week. There is no bug bounty.

## Scope notes

- The server is designed to run behind your own reverse proxy / TLS termination; it speaks
  plain HTTP itself.
- The agent key lives in `agent.ini`: encrypted with DPAPI (machine scope) on Windows for a setup
  install, plain text with restricted file rights elsewhere. What that protects and what it does
  not (a local administrator, other programs on the same computer, `[headers]` values) is in
  [Where the key is stored](docs/agent.md#where-the-key-is-stored).
- Anything reachable with the bootstrap `PRINTAPI_TOKEN` is root-equivalent for the print
  system — treat that token like a password.

## Signed updates

The setup's agent and tray program can update themselves ([docs](docs/agent.md#updates)). What makes
that safe, and what it does not do:

- **One Ed25519 key signs one file.** `printpapi-update.txt` lists the SHA-256 and size of every
  program file of a release. The agent accepts the file when any signature line in it verifies under
  any public key in `crates/agent/update-keys.txt`. TLS, the host and a mirror are not trusted. The
  key file is committed and built into the agent; there is no environment variable and no runtime
  override. A fork that publishes its own releases edits that file.
- **The private key is only in a protected GitHub environment.** It is the secret
  `UPDATE_SIGNING_KEY` of the environment `release-signing` (required reviewer, deployments limited to
  tags `v*`), never a repository secret, because every writer of the repository can read those. Only
  the last step of the publish job sees it. Without the secret, or for a tag that is not `vN.N.N`,
  no update is published.
- **The elevated program is the installed agent**, which only administrators can write, not a file
  from the user's staging folder. It reads each file once into memory, checks those bytes and
  writes only those bytes. It starts both new programs once from a private folder, then swaps two
  files by renaming; it never writes `agent.ini`, the unit, the task or the rights. An update
  needs the agent to be connected and keeps the old files until the new version proved itself.
- **No downgrade.** The version in the file has to be higher than the installed one. A hostile
  mirror can hold a machine back, or offer an older release that is still newer than the installed
  version, but never one below it. There is no expiry of the signed file and no revocation that
  reaches a machine that does not update (a documented limit).
- **If the key leaks:** remove its public half from `crates/agent/update-keys.txt`, publish a release
  signed with a key that stays in the file (a rotation can sign with the old and the new key for some
  releases), and tell the people who run `updates = auto` with the leaked key in their build: such an
  agent cannot verify a release signed with another key and has to be installed once by hand.
- The first build of this feature ships with no key in the file, so it cannot update until the owner
  of a release adds one. In this order: (1) create the GitHub environment `release-signing` with
  yourself as required reviewer, no administrator bypass, and deployments limited to tags `v*`;
  (2) create the key and keep an offline copy, and put it only into that environment as the secret
  `UPDATE_SIGNING_KEY`:
  `openssl genpkey -algorithm ed25519 -out printpapi-update.pem`;
  (3) add the public half, as one line of 64 hex characters, to `crates/agent/update-keys.txt`:
  `openssl pkey -in printpapi-update.pem -pubout -outform DER | tail -c 32 | od -An -v -tx1 | tr -d ' \n'`.
  Until then the release workflow skips the update files with a notice, the window says that this
  build cannot check for updates, and no request is made. A build made for tests can be given the
  public half of a test seed with the Cargo feature `update-test-key`; the release workflow never
  enables it.
