#!/usr/bin/env python3
"""Exercise real macOS launchd with isolated fixture services, never live ports/data."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("native_service", REPO / "scripts/native-runtime-service.py")
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


def snapshots(root):
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in root.rglob("*") if path.is_file() and "logs" not in path.parts}


def qualify():
    if sys.platform != "darwin":
        raise RuntimeError("this qualification requires macOS")
    native.SERVICE_ID = "ai.magicbeans.magican.source-fixture." + uuid.uuid4().hex
    target = f"gui/{os.geteuid()}/{native.SERVICE_ID}"
    ports = []
    for _ in range(3):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            ports.append(listener.getsockname()[1])
    translations = dict(zip((8081, 3002, 3003), ports))

    def runner(args, **kwargs):
        # Reuse production ownership checks against fixture-only sockets.
        args = [f"-iTCP:{translations[int(arg[6:])]}" if arg.startswith("-iTCP:") else arg for arg in args]
        return subprocess.run(args, timeout=15, **kwargs)

    with tempfile.TemporaryDirectory(prefix="magican-native-service-fixture-") as temporary:
        base = Path(temporary)
        prefix = base / "software with spaces"
        prefix.mkdir()
        for name in ("magician.bin", "magicutor.bin", "magic-supervisor.bin", "MANIFEST.yaml", "tool-runtime-config.yaml"):
            (prefix / name).write_text("fixture\n")
        (prefix / "scripts").mkdir()
        (prefix / "scripts/run-ollama.sh").write_text("# fixture: no model lifecycle changes\n")
        (prefix / "scripts/run-native-backend.sh").write_text(
            "#!/bin/bash\nexec " + shlex.quote(sys.executable) + " ./fixture_service.py\n")
        (prefix / "fixture_service.py").write_text('''import http.server, json, os, threading
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"root": os.environ["MAGICIAN_ROOT_DIR"], "pid": os.getpid()}).encode()
        self.send_response(200); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *args): pass
for port in ''' + repr(ports) + ''':
    server = http.server.HTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
threading.Event().wait()
''')
        root = base / "first notes"
        native.seed(REPO, root)
        (root / "owner-note.md").write_text("owner fixture: preserve me\n")
        before = snapshots(root)
        service = base / "fixture.plist"

        def ready(expected_root):
            deadline = time.monotonic() + 20
            error = None
            while time.monotonic() < deadline:
                try:
                    native.verify_binding(prefix, expected_root, runner=runner, service_path=service)
                    with urllib.request.urlopen(f"http://127.0.0.1:{ports[1]}/health", timeout=2) as response:
                        state = json.load(response)
                    if state["root"] != str(expected_root):
                        raise RuntimeError("old service answered for the new root")
                    return state["pid"]
                except (OSError, ValueError, RuntimeError) as failure:
                    error = failure
                    time.sleep(0.2)
            raise RuntimeError(f"fixture service did not become ready: {error}")

        try:
            native.start(prefix, root, runner=runner, service_path=service)
            first_pid = ready(root)
            native.start(prefix, root, runner=runner, service_path=service)
            assert ready(root) == first_pid, "unchanged service start restarted the fixture"
            assert snapshots(root) == before, "native service changed owner configuration/data"
            second = base / "second notes"
            native.seed(REPO, second)
            native.start(prefix, second, runner=runner, service_path=service)
            second_pid = ready(second)
            assert second_pid != first_pid, "root change did not restart the service"
            with (second / ".env.development").open("a") as file:
                file.write("\nFIXTURE_REVISION=changed\n")
            native.start(prefix, second, runner=runner, service_path=service)
            assert ready(second) != second_pid, "runtime input change did not restart the service"
            assert snapshots(root) == before, "rebinding changed the first data folder"
            return {"kind": "macos_launchd_fixture", "passed": True,
                    "checks": ["registration", "listener_ownership", "root_binding", "unchanged_start",
                               "data_preservation", "root_rebind", "runtime_input_restart"],
                    "production_services_changed": False}
        finally:
            subprocess.run(["/bin/launchctl", "bootout", target], capture_output=True, timeout=15)


if __name__ == "__main__":
    result = qualify()
    if len(sys.argv) == 2:
        Path(sys.argv[1]).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
