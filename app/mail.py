# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Outbound e-mail — the one thing a password reset needs that printing never did.

Configured entirely from the environment (`SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD`,
`SMTP_FROM`, `SMTP_SSL`, `SMTP_STARTTLS`). With no `SMTP_HOST` the message goes to stderr instead
of nowhere: a self-hosted instance without a mail server still works — the operator reads the reset
link out of the log — and a missing config can never look like a delivered mail.

# ponytail: stdlib smtplib, synchronous, no retry and no queue. Reset mails are one per click; if
# this ever sends bulk, hand it to the webhook dispatcher thread instead.
"""
import os
import smtplib
import sys
from email.message import EmailMessage


def _connect(host, port, ssl_mode):
    return smtplib.SMTP_SSL(host, port, timeout=15) if ssl_mode \
        else smtplib.SMTP(host, port, timeout=15)


def configured(env=None):
    """True if a real mail server is set up. The dashboard uses it to decide whether offering a
    password reset is honest."""
    return bool((os.environ if env is None else env).get("SMTP_HOST"))


def send(to, subject, body, env=None, connect=_connect):
    """Send one plain-text message. Returns True if it went to a mail server, False if it was
    logged instead. Raises nothing on a delivery failure — the caller must not tell an
    unauthenticated user whether the address exists (see POST /password/reset)."""
    env = os.environ if env is None else env
    host = env.get("SMTP_HOST")
    if not host:
        print(f"[mail] SMTP_HOST unset — not delivered. To: {to}\n"
              f"[mail] Subject: {subject}\n{body}", file=sys.stderr)
        return False
    ssl_mode = (env.get("SMTP_SSL") or "").lower() in ("1", "true", "yes")
    port = int(env.get("SMTP_PORT") or (465 if ssl_mode else 587))
    msg = EmailMessage()
    msg["From"] = env.get("SMTP_FROM") or "printpapi@localhost"
    msg["To"] = to
    msg["Subject"] = subject
    msg.set_content(body)
    with connect(host, port, ssl_mode) as smtp:
        if not ssl_mode and (env.get("SMTP_STARTTLS") or "1").lower() not in ("0", "false", "no"):
            smtp.starttls()
        if env.get("SMTP_USER"):
            smtp.login(env["SMTP_USER"], env.get("SMTP_PASSWORD") or "")
        smtp.send_message(msg)
    return True
