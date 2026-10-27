"""Test-only TCP relay. Counts encrypted server bytes without inspecting TLS."""
import os
import selectors
import socket
import socketserver
import threading
from pathlib import Path

counter = Path('/pki/server-to-client-bytes')
counter.write_text('0')
lock = threading.Lock()
total = 0

class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        global total
        with socket.create_connection(('127.0.0.1', 993)) as remote:
            with selectors.DefaultSelector() as selector:
                selector.register(self.request, selectors.EVENT_READ, remote)
                selector.register(remote, selectors.EVENT_READ, self.request)
                while True:
                    for key, _ in selector.select(timeout=30):
                        try:
                            data = key.fileobj.recv(65536)
                            if not data:
                                return
                            if key.fileobj is remote:
                                with lock:
                                    total += len(data)
                                    temp = counter.with_suffix('.tmp')
                                    temp.write_text(str(total))
                                    os.replace(temp, counter)
                            key.data.sendall(data)
                        except (ConnectionError, OSError):
                            return

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

with Server(('0.0.0.0', 1993), Handler) as server:
    server.serve_forever()
