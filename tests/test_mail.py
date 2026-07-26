"""The SMTP sender: what it builds, and what it does when no mail server is configured."""
from app import mail


class FakeSMTP:
    """Records what a send did instead of talking to a server."""
    last = None

    def __init__(self, host, port, ssl_mode):
        self.host, self.port, self.ssl_mode = host, port, ssl_mode
        self.tls, self.login_as, self.messages = False, None, []
        FakeSMTP.last = self

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def starttls(self):
        self.tls = True

    def login(self, user, password):
        self.login_as = (user, password)

    def send_message(self, msg):
        self.messages.append(msg)


def test_without_smtp_host_the_message_is_logged_not_sent(capsys):
    assert mail.send("ops@shop.test", "Reset", "link", env={}) is False
    assert mail.configured(env={}) is False
    err = capsys.readouterr().err
    assert "ops@shop.test" in err and "link" in err


def test_a_configured_sender_starttls_logs_in_and_sends():
    env = {"SMTP_HOST": "smtp.test", "SMTP_USER": "u", "SMTP_PASSWORD": "p",
           "SMTP_FROM": "print@shop.test"}
    assert mail.configured(env=env) is True
    assert mail.send("ops@shop.test", "Reset your password", "https://x/reset?token=abc",
                     env=env, connect=FakeSMTP) is True
    s = FakeSMTP.last
    assert (s.host, s.port, s.ssl_mode, s.tls) == ("smtp.test", 587, False, True)
    assert s.login_as == ("u", "p")
    msg = s.messages[0]
    assert msg["To"] == "ops@shop.test" and msg["From"] == "print@shop.test"
    assert msg["Subject"] == "Reset your password"
    assert "token=abc" in msg.get_content()


def test_implicit_tls_uses_465_and_does_not_starttls():
    mail.send("a@b.test", "s", "b", env={"SMTP_HOST": "smtp.test", "SMTP_SSL": "1"},
              connect=FakeSMTP)
    assert (FakeSMTP.last.port, FakeSMTP.last.ssl_mode, FakeSMTP.last.tls) == (465, True, False)
    assert FakeSMTP.last.login_as is None            # no SMTP_USER — anonymous relay
