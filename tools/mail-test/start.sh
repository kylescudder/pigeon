#!/bin/sh
set -eu
mkdir -p /pki /mail
if [ ! -f /pki/ca.pem ]; then
  openssl req -x509 -newkey rsa:2048 -nodes -keyout /pki/ca.key -out /pki/ca.pem \
    -days 2 -subj '/CN=Pigeon disposable test CA' >/dev/null 2>&1
  openssl req -newkey rsa:2048 -nodes -keyout /pki/server.key -out /pki/server.csr \
    -subj '/CN=localhost' >/dev/null 2>&1
  printf 'subjectAltName=DNS:localhost,DNS:imap.pigeon.test,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > /pki/server.ext
  openssl x509 -req -in /pki/server.csr -CA /pki/ca.pem -CAkey /pki/ca.key -CAcreateserial \
    -out /pki/server.pem -days 2 -extfile /pki/server.ext >/dev/null 2>&1
fi
# Synthetic credential for an isolated disposable test server, never a user account.
printf 'mail@pigeon.test:{PLAIN}pigeon-test-only\n' > /etc/dovecot/test-users
python3 /usr/local/bin/seed.py
chown -R vmail:vmail /mail
python3 /usr/local/bin/proxy.py &
exec dovecot -F
