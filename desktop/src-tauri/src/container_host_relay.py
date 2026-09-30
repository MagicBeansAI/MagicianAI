"""Linux half of the desktop-owned stdio relay (Python standard library only).

The desktop supplies this program through container exec, never a network
listener on the Mac. Closing that private stdin revokes access immediately.
HTTP is deliberately sequential, bounded, loopback-only and connection-close.
"""
import base64
import http.server
import json
import os
import queue
import sys
import threading

MAX_BODY = 1024 * 1024
MAX_FRAME = 48 * 1024 * 1024


def serve(port=3017):
    replies = queue.Queue(maxsize=1)

    def receive():
        while True:
            line = sys.stdin.buffer.readline(MAX_FRAME + 1)
            if not line or len(line) > MAX_FRAME or not line.endswith(b"\n"):
                # Also kills an in-flight HTTP request; never leave an orphan
                # gateway behind after the desktop/exec connection goes away.
                os._exit(0)
            try:
                replies.put(json.loads(line), timeout=1)
            except Exception:
                os._exit(1)

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def setup(self):
            super().setup()
            self.connection.settimeout(15)

        def log_message(self, *args):
            pass  # No request contents, scripts, pixels or message text in logs.

        def forward(self):
            self.close_connection = True
            lengths = self.headers.get_all("Content-Length", [])
            if (len(lengths) > 1 or self.headers.get("Transfer-Encoding")
                    or self.headers.get("Origin") or self.headers.get("Upgrade")
                    or self.headers.get("Expect")):
                self.send_error(400)
                return
            length = lengths[0] if lengths else "0"
            if not length.isascii() or not length.isdigit() or int(length) > MAX_BODY:
                self.send_error(413)
                return
            body = self.rfile.read(int(length))
            if len(body) != int(length):
                self.send_error(400)
                return
            request = {"method": self.command, "path": self.path,
                       "body": base64.b64encode(body).decode("ascii")}
            sys.stdout.write(json.dumps(request, separators=(",", ":")) + "\n")
            sys.stdout.flush()
            try:
                response = replies.get(timeout=145)
                data = base64.b64decode(response["body"], validate=True)
                status = response["status"]
                if not isinstance(status, int) or not 200 <= status <= 599:
                    raise ValueError("invalid status")
            except Exception:
                # A late response must never be consumed by the next request.
                os._exit(1)
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)

        do_GET = forward
        do_POST = forward

    class Server(http.server.HTTPServer):
        allow_reuse_address = True
        request_queue_size = 8

        def handle_error(self, request, client_address):
            pass

    # Bind before signalling readiness. An existing listener is an error, never
    # something to kill or silently adopt.
    server = Server(("127.0.0.1", port), Handler)
    threading.Thread(target=receive, daemon=True).start()
    print('{"ready":1}', flush=True)
    server.serve_forever()


if __name__ == "__main__":
    serve()
