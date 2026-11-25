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
