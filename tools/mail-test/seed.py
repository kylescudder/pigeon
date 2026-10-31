from pathlib import Path
from email.message import EmailMessage
from email import policy

root = Path('/mail/Maildir')
for folder in ['', '.Sent', '.Archive', '.Junk', '.Trash']:
    base = root / folder
    for part in ['cur', 'new', 'tmp']:
        (base / part).mkdir(parents=True, exist_ok=True)

fixtures = [
    ('', 'invoice', 'Invoice 4471', 'Please find the cafe receipt and the building invoice attached.'),
    ('', 'lunch', 'Lunch on Friday', 'Sandwiches at noon. Bring the quarterly report.'),
    ('.Archive', 'archive', 'Project notes', 'The archived pigeon migration notes are ready.'),
    ('.Junk', 'spam', 'Unwanted invoice', 'This invoice belongs in junk.'),
]
for folder, name, subject, body in fixtures:
    path = root / folder / 'new' / f'fixture-{name}'
    if not path.exists():
        message = (f'From: Alice Example <alice@pigeon.test>\r\n'
                   f'To: Kyle <mail@pigeon.test>\r\n'
                   f'Subject: {subject}\r\n'
                   f'Message-ID: <{name}@pigeon.test>\r\n'
                   'Date: Mon, 26 Oct 2026 09:00:00 +0000\r\n'
                   'MIME-Version: 1.0\r\n'
                   'Content-Type: text/plain; charset=utf-8\r\n\r\n'
                   f'{body}\r\n')
        path.write_bytes(message.encode())

mixed = EmailMessage(policy=policy.SMTP)
mixed['From'] = 'Alice Example <alice@pigeon.test>'
mixed['To'] = 'mail@pigeon.test'
mixed['Subject'] = 'Unicode café attachment'
mixed['Date'] = 'Mon, 26 Oct 2026 10:00:00 +0000'
mixed['Message-ID'] = '<large-attachment@pigeon.test>'
mixed.set_content('The résumé is ready. Text should sync without the large attachment.', cte='quoted-printable')
mixed.add_attachment((b'ATTACHMENT_PAYLOAD_CANARY-' * 360000), maintype='application', subtype='pdf', filename='report.pdf')
(root / 'new' / 'fixture-attachment').write_bytes(mixed.as_bytes())

html = EmailMessage(policy=policy.SMTP)
html['From'] = 'Build service <build@pigeon.test>'
html['To'] = 'mail@pigeon.test'
html['Subject'] = 'HTML status update'
html['Date'] = 'Mon, 26 Oct 2026 11:00:00 +0000'
html['Message-ID'] = '<html@pigeon.test>'
html.set_content('<h1>Launch status</h1><p>The roadrunner build is ready.</p><script>hidden_secret_script</script>', subtype='html')
(root / 'new' / 'fixture-html').write_bytes(html.as_bytes())

# An HTML newsletter with an embedded (cid:) logo and a separate attachment,
# for on-request part download. The PNG is a real 1x1 image.
PNG = bytes.fromhex(
    '89504e470d0a1a0a0000000d4948445200000001000000010806000000'
    '1f15c4890000000d49444154789c63f8cfc0f01f00050001ff89993d1d'
    '0000000049454e44ae426082')
related = EmailMessage(policy=policy.SMTP)
related['From'] = 'Pelican News <news@pigeon.test>'
related['To'] = 'mail@pigeon.test'
related['Subject'] = 'Embedded logo newsletter'
related['Date'] = 'Mon, 26 Oct 2026 12:00:00 +0000'
related['Message-ID'] = '<embedded@pigeon.test>'
related.set_content('Plain pelican newsletter fallback.')
related.add_alternative(
    '<p>Pelican newsletter</p><img src="cid:logo@pigeon.test" alt="logo">'
    '<img src="https://tracker.example/pixel.gif" alt="remote">', subtype='html')
related.get_payload()[1].add_related(PNG, maintype='image', subtype='png', cid='<logo@pigeon.test>')
related.add_attachment(b'MANUAL_PAYLOAD_CANARY-' * 100000, maintype='application', subtype='pdf', filename='manual.pdf')
(root / 'new' / 'fixture-embedded').write_bytes(related.as_bytes())
