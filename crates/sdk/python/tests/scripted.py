"""Shared scripted-TCP-server plumbing for transport-level SDK tests.

The correlation tests (D-06) script fixed frame sequences; the transaction
tests (D-08) need per-request dispatch — one optional `respond(req)`
callable covers both. No aikoql binary is needed: the transport is the
unit under test.
"""

import json
import socket
import threading
import time


def frame_bytes(f):
    if isinstance(f, str):
        return (f + "\n").encode()
    return (json.dumps(f) + "\n").encode()


class ScriptedServer:
    """A one-connection TCP server that answers requests from a script.

    Script mode: one entry per expected request; the entry's frames go out
    in ONE sendall (an empty entry holds the connection open 0.8s — past
    the client's deadline). Respond mode (`respond` given): every parsed
    request line is answered by the callable instead, until the client
    closes.
    """

    def __init__(self, script=None, respond=None):
        self.script = script
        self.respond = respond
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        conn, _ = self.sock.accept()
        try:
            if self.respond is not None:
                while True:
                    data = conn.recv(4096)
                    if not data:
                        break
                    frames = self.respond(json.loads(data.decode().strip()))
                    if frames:
                        conn.sendall(b"".join(frame_bytes(f) for f in frames))
                    else:
                        time.sleep(0.8)  # hold past the client deadline
                return
            for frames in self.script:
                data = conn.recv(4096)
                if not data:
                    break
                if frames:
                    conn.sendall(b"".join(frame_bytes(f) for f in frames))
                else:
                    time.sleep(0.8)
        finally:
            conn.close()

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *a):
        self.sock.close()
        self.thread.join(timeout=2)
