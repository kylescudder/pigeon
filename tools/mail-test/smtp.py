"""Disposable SMTP submission server with deterministic failures.

The recipient's local part chooses what happens, so each test controls the
outcome without extra configuration:

  reject-rcpt@         550 to RCPT TO; nothing is spooled
  drop-before-data@    connection closed at RCPT TO; nothing is spooled
  temp-fail-data@      451 after the message; nothing is spooled
  perm-fail-data@      554 after the message; nothing is spooled
  drop-after-data@     message spooled, then the connection closes unanswered
  stall-after-data@    message spooled, then no reply until the client gives up
  anything else        250; message spooled

Spooled messages land in /pki/smtp-spool so tests can count deliveries.
Listens on 465 (implicit TLS) and 587 (STARTTLS required); both require AUTH.
"""

import asyncio
import os
import ssl
import uuid

from aiosmtpd.smtp import SMTP, AuthResult

SPOOL = "/pki/smtp-spool"
USER = b"mail@pigeon.test"
# Synthetic credential for an isolated disposable server, never a user account.
PASSWORD = b"pigeon-test-only"


def authenticate(server, session, envelope, mechanism, auth_data):
    return AuthResult(
        success=auth_data.login == USER and auth_data.password == PASSWORD
    )


def mode(envelope):
    for recipient in envelope.rcpt_tos:
        local = recipient.split("@", 1)[0].lower()
        if local in (
            "temp-fail-data",
            "perm-fail-data",
            "drop-after-data",
            "stall-after-data",
        ):
            return local
    return "accept"


class Handler:
    async def handle_RCPT(self, server, session, envelope, address, rcpt_options):
        local = address.split("@", 1)[0].lower()
        if local == "reject-rcpt":
            return "550 5.1.1 No such user here"
        if local == "drop-before-data":
            server.transport.abort()
            return "421 closing"
        envelope.rcpt_tos.append(address)
        return "250 2.1.5 OK"

    async def handle_DATA(self, server, session, envelope):
        behaviour = mode(envelope)
        if behaviour == "temp-fail-data":
            return "451 4.3.0 Try again later"
        if behaviour == "perm-fail-data":
            return "554 5.6.0 Message rejected"
        os.makedirs(SPOOL, exist_ok=True)
        path = os.path.join(SPOOL, f"{uuid.uuid4().hex}.eml")
        with open(path + ".tmp", "wb") as spooled:
            spooled.write(envelope.original_content or envelope.content)
        os.replace(path + ".tmp", path)
        if behaviour == "drop-after-data":
            server.transport.abort()
            return "250 2.0.0 never delivered to the client"
        if behaviour == "stall-after-data":
            await asyncio.sleep(3600)
        return "250 2.0.0 Queued"


def tls_context():
    context = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain("/pki/server.pem", "/pki/server.key")
    return context


async def main():
    loop = asyncio.get_running_loop()
    handler = Handler()
    common = dict(
        hostname="smtp.pigeon.test",
        authenticator=authenticate,
        auth_required=True,
    )
    implicit = await loop.create_server(
        lambda: SMTP(handler, auth_require_tls=False, **common),
        "127.0.0.1",
        465,
        ssl=tls_context(),
    )
    starttls = await loop.create_server(
        lambda: SMTP(
            handler,
            tls_context=tls_context(),
            require_starttls=True,
            auth_require_tls=True,
            **common,
        ),
        "127.0.0.1",
        587,
    )
    async with implicit, starttls:
        await asyncio.gather(implicit.serve_forever(), starttls.serve_forever())


asyncio.run(main())
