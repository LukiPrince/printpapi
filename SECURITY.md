# Security Policy

## Supported versions

The latest state of `main` is the supported version.

## Reporting a vulnerability

Please report vulnerabilities **privately** via GitHub's
[security advisories](../../security/advisories/new) ("Report a vulnerability" on the repo's
Security tab). Do not open a public issue for security problems.

You can expect an initial response within a week. There is no bug bounty.

## Scope notes

- The server is designed to run behind your own reverse proxy / TLS termination; it speaks
  plain HTTP itself.
- Anything reachable with the bootstrap `PRINTAPI_TOKEN` is root-equivalent for the print
  system — treat that token like a password.
