FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    dovecot-imapd openssl python3 python3-aiosmtpd ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 1000 vmail && useradd -u 1000 -g vmail -d /mail vmail
COPY dovecot.conf /etc/dovecot/dovecot.conf
COPY start.sh seed.py proxy.py smtp.py /usr/local/bin/
ENTRYPOINT ["/bin/sh", "/usr/local/bin/start.sh"]
