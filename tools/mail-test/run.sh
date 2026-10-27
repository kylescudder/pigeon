#!/usr/bin/env bash
# Real IMAP/TLS integration on an isolated Docker network. Uses synthetic mail
# only; no host ports, user accounts, or production credentials are needed.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
server="pigeon-imap-test-$$"
pki="$server-pki"
cleanup() {
  docker rm -f "$server" >/dev/null 2>&1 || true
  docker volume rm "$pki" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
if ! docker image inspect pigeon-linux-dev >/dev/null 2>&1; then
  docker build -t pigeon-linux-dev "$repo/tools/linux-dev"
fi
docker build -t pigeon-mail-test "$repo/tools/mail-test"
docker volume create "$pki" >/dev/null
docker run -d --name "$server" -v "$pki":/pki pigeon-mail-test >/dev/null
ready=false
for attempt in {1..30}; do
  if docker exec "$server" python3 -c 'import socket; socket.create_connection(("127.0.0.1",993),2).close()' >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
if [ "$ready" != true ]; then
  docker logs "$server"
  exit 1
fi
docker run --rm --network "container:$server" \
  -v "$repo":/src -v "$pki":/pki:ro \
  -v pigeon-cargo:/usr/local/cargo/registry -v pigeon-target:/src/target \
  pigeon-linux-dev cargo test --locked -p pigeon-core --test imap_server \
  -- --ignored --test-threads=1 --nocapture
