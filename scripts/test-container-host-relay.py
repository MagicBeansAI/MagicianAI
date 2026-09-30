#!/usr/bin/env python3
"""Provider-free wire/lifecycle tests for the private Linux relay worker."""
import base64
import concurrent.futures
import http.client
import json
from pathlib import Path
import selectors
import socket
import subprocess
import sys
import unittest

WORKER = Path(__file__).resolve().parents[1] / "desktop/src-tauri/src/container_host_relay.py"


class RelayTests(unittest.TestCase):
    def setUp(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        code = (f"import runpy; worker=runpy.run_path({str(WORKER)!r});"
                f"worker['serve']({self.port})")
        self.proc = subprocess.Popen([sys.executable, "-u", "-c", code],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(self.frame(), {"ready": 1})

    def tearDown(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=3)
        finally:
            if self.proc.poll() is None:
                self.proc.kill()
                self.proc.wait()
            self.proc.stdout.close()
            self.proc.stderr.close()

    def frame(self):
        with selectors.DefaultSelector() as selector:
            selector.register(self.proc.stdout, selectors.EVENT_READ)
            self.assertTrue(selector.select(timeout=3), "worker did not emit a frame")
        return json.loads(self.proc.stdout.readline())

    def reply(self, status, body):
        self.proc.stdin.write(json.dumps({"status": status, "body": base64.b64encode(body).decode()}).encode() + b"\n")
        self.proc.stdin.flush()

    def request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=3)
        try:
            connection.request(method, path, body, headers or {})
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    def test_json_and_binary_content_round_trip_without_header_forwarding(self):
        with concurrent.futures.ThreadPoolExecutor(1) as pool:
            future = pool.submit(self.request, "POST", "/host/ax/snapshot", b'{"x":"\\n"}', {"X-Injection": "ignored"})
            frame = self.frame()
            self.assertEqual(set(frame), {"method", "path", "body"})
            self.assertEqual(base64.b64decode(frame["body"]), b'{"x":"\\n"}')
            self.reply(409, b'{"bytes":"\\u0000","message":"fixture"}')
            self.assertEqual(future.result(), (409, b'{"bytes":"\\u0000","message":"fixture"}'))

    def test_browser_origin_and_oversized_requests_are_rejected_before_pipe(self):
        self.assertEqual(self.request("POST", "/host/applescript", b"{}", {"Origin": "http://example.test"})[0], 400)
        self.assertEqual(self.request("POST", "/host/applescript", b"", {"Content-Length": "1048577"})[0], 413)

    def test_duplicate_lengths_and_chunked_encoding_are_rejected(self):
        for headers in [b"Content-Length: 0\r\nContent-Length: 0", b"Transfer-Encoding: chunked"]:
            with socket.create_connection(("127.0.0.1", self.port), timeout=3) as client:
                client.sendall(b"POST /host/applescript HTTP/1.1\r\nHost: localhost\r\n" + headers + b"\r\n\r\n")
                self.assertIn(b"400", client.recv(4096).split(b"\r\n")[0])

    def test_private_pipe_eof_revokes_listener(self):
        self.proc.stdin.close()
        self.assertEqual(self.proc.wait(timeout=3), 0)
        with self.assertRaises(OSError):
            socket.create_connection(("127.0.0.1", self.port), timeout=1)

    def test_malformed_reply_terminates_instead_of_desynchronizing(self):
        self.proc.stdin.write(b"not-json\n")
        self.proc.stdin.flush()
        self.assertEqual(self.proc.wait(timeout=3), 1)


if __name__ == "__main__":
    unittest.main()
