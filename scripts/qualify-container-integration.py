#!/usr/bin/env python3
"""Attach-only container -> Tauri / extension / Linux skill qualification.

Host and in-container worker use only the Python standard library. No builds,
installs, service lifecycle calls, LLM calls, or runtime-root writes. See
docs/components/scripts/container-integration-harness.md for preparation.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zlib


SCHEMA = "magician.container-integration.v1"
STAGES = ("host", "browser", "skills")
CHECKS = {
    "host": ("endpoints", "automation", "capture"),
    "browser": ("shared_directory", "proxy_identity", "cli_core", "navigate_snapshot",
                "client_reconnect", "download", "cleanup"),
    "skills": ("pdf", "ocr", "python_html", "node_rg"),
}


class ProbeError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ProbeError(message)


def run_command(argv, *, timeout=30, stdin=b"", cwd=None, env=None, limit=2 * 1024 * 1024):
    """Bound wall time and output; kill only this command's new process group.

    File-backed stdin avoids a blocked pipe writer. Output is never copied into
    a report automatically (CLI diagnostics may contain URLs or credentials).
    """
    program = shutil.which(str(argv[0]), path=(env or os.environ).get("PATH"))
    require(program is not None, f"missing executable: {Path(argv[0]).name}")
    argv = [program, *map(str, argv[1:])]
    with tempfile.TemporaryFile() as source, tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        source.write(stdin)
        source.seek(0)
        proc = subprocess.Popen(argv, stdin=source, stdout=out, stderr=err,
                                cwd=cwd, env=env, start_new_session=True)
        deadline = time.monotonic() + timeout
        try:
            while proc.poll() is None:
                require(time.monotonic() < deadline, f"{Path(program).name} timed out after {timeout}s")
                require(os.fstat(out.fileno()).st_size + os.fstat(err.fileno()).st_size <= limit,
                        f"{Path(program).name} exceeded output limit")
                time.sleep(0.02)
            require(os.fstat(out.fileno()).st_size + os.fstat(err.fileno()).st_size <= limit,
                    f"{Path(program).name} exceeded output limit")
            require(proc.returncode == 0, f"{Path(program).name} exited {proc.returncode}")
            out.seek(0)
            return out.read(limit).decode("utf-8")
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()


def origin(value):
    parsed = urllib.parse.urlsplit(value)
    require(parsed.scheme in ("http", "https") and bool(parsed.hostname)
            and parsed.username is None and parsed.password is None
            and parsed.path in ("", "/") and not parsed.query and not parsed.fragment,
            "expected an HTTP origin without credentials, path, query or fragment")
    try:
        _ = parsed.port
    except ValueError:
        raise ProbeError("invalid origin port") from None
    return value.rstrip("/")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ProbeError("probe endpoint redirected; use the exact service origin")


def http_json(url, *, method="GET", body=None, timeout=10):
    # Host aliases and loopback services must not pass through an inherited proxy.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method,
                                     headers={"Content-Type": "application/json"})
    try:
        with opener.open(request, timeout=timeout) as response:
            payload = response.read(8 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        raise ProbeError(f"HTTP {error.code}; verify pairing/permissions and service origin") from None
    except (urllib.error.URLError, TimeoutError):
        raise ProbeError("service unreachable or timed out from inside the container") from None
    require(len(payload) <= 8 * 1024 * 1024, "HTTP response exceeded 8 MiB")
    result = json.loads(payload)
    require(isinstance(result, dict), "expected a JSON object")
    return result


def png_metadata(payload):
    require(payload.get("content_type") == "image/png", "capture is not PNG")
    try:
        raw = base64.b64decode(payload["image_b64"], validate=True)
    except (KeyError, ValueError, TypeError):
        raise ProbeError("capture contains invalid base64") from None
    require(raw.startswith(b"\x89PNG\r\n\x1a\n"), "capture has invalid PNG signature")
    offset, dimensions, has_data, ended = 8, None, False, False
    while offset + 12 <= len(raw):
        size = struct.unpack_from(">I", raw, offset)[0]
        kind = raw[offset + 4:offset + 8]
        end = offset + size + 12
        require(end <= len(raw), "truncated PNG chunk")
        chunk = raw[offset + 8:end - 4]
        require(zlib.crc32(kind + chunk) == struct.unpack_from(">I", raw, end - 4)[0],
                "invalid PNG checksum")
        if kind == b"IHDR":
            require(offset == 8 and size == 13, "invalid PNG header")
            dimensions = struct.unpack_from(">II", chunk)
        elif kind == b"IDAT":
            has_data = has_data or bool(chunk)
        elif kind == b"IEND":
            require(size == 0 and end == len(raw), "invalid PNG end")
            ended = True
            break
        offset = end
    require(dimensions and has_data and ended, "incomplete PNG capture")
    width, height = dimensions
    require(0 < width <= 1024 and 0 < height <= 1024, "unexpected size for 64x64 region capture")
    return {"width": width, "height": height, "bytes": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(), "pixels_retained": False}


def fixture_pdf(text):
    stream = f"BT /F1 18 Tf 40 100 Td ({text}) Tj ET\n".encode("ascii")
    objects = [b"<< /Type /Catalog /Pages 2 0 R >>",
               b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
               b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
               f"<< /Length {len(stream)} >>\nstream\n".encode() + stream + b"endstream",
               b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    result, offsets = b"%PDF-1.4\n", [0]
    for index, obj in enumerate(objects, 1):
        offsets.append(len(result))
        result += f"{index} 0 obj\n".encode() + obj + b"\nendobj\n"
    xref = len(result)
    result += b"xref\n0 6\n0000000000 65535 f \n"
    result += b"".join(f"{offset:010d} 00000 n \n".encode() for offset in offsets[1:])
    return result + f"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()


def download_ref(data):
    for ref, value in data.get("refs", {}).items():
        if value.get("role") == "link" and value.get("name") == "Download integration fixture":
            require(re.fullmatch(r"e\d+", ref) is not None, "invalid snapshot ref")
            return "@" + ref
    match = re.search(r'link "Download integration fixture" \[ref=(e\d+)\]', data.get("snapshot", ""))
    require(match is not None, "fixture download link missing from interactive snapshot")
    return "@" + match.group(1)


def result_for(checks, stages):
    if not checks or any(item["status"] == "fail" for item in checks):
        return "fail"
    if set(stages) != set(STAGES) or any(item["status"] != "pass" for item in checks):
        return "partial"
    return "pass"


def validate_worker_report(report, config):
    require(isinstance(report, dict) and report.get("schema") == SCHEMA
            and report.get("run_id") == config["run_id"]
            and report.get("stages") == config["stages"], "invalid worker report identity")
    checks = report.get("checks")
    require(isinstance(checks, list) and all(isinstance(item, dict) for item in checks),
            "invalid worker checks")
    expected = {stage + "." + name for stage in config["stages"] for name in CHECKS[stage]}
    expected.add("runtime.nonroot_linux")
    allowed = expected | {stage + ".setup" for stage in config["stages"]}
    names = []
    for item in checks:
        require(isinstance(item.get("name"), str) and item["name"] in allowed
                and item.get("status") in ("pass", "fail", "skip"), "invalid worker check")
        names.append(item["name"])
    require(len(names) == len(set(names)) and expected <= set(names),
            "worker returned incomplete or duplicate check coverage")


class Probes:
    def __init__(self, config):
        self.config = config
        self.checks = []
        self.timeout = config["timeout"]
        self.root = Path(config["skillshub_root"])
        self.thread = config["run_id"]

    def check(self, name, action):
        started = time.monotonic()
        try:
            evidence = action() or {}
            item = {"name": name, "status": "pass", "evidence": evidence}
        except Exception as error:
            # Deliberately omit arbitrary upstream output and screenshot payloads.
            detail = str(error) if isinstance(error, ProbeError) else type(error).__name__
            item = {"name": name, "status": "fail", "detail": detail[:600]}
        item["elapsed_ms"] = round((time.monotonic() - started) * 1000)
        self.checks.append(item)
        return item["status"] == "pass"

    def skip(self, name, reason):
        self.checks.append({"name": name, "status": "skip", "detail": reason})

    def cmd(self, argv, **kwargs):
        return run_command(argv, timeout=self.timeout, **kwargs)

    def http(self, url, **kwargs):
        return http_json(url, timeout=min(self.timeout, 15), **kwargs)

    def host(self):
        gateway = self.config.get("gateway_url") or os.environ.get("MAGICIAN_HOST_GATEWAY_URL", "")
        def endpoints():
            data = self.http(origin(gateway) + "/host/runtime/endpoints")
            require(data.get("schemaVersion") == 1, "unsupported host endpoint contract")
            for key in ("magicianApiBase", "magicianHealthUrl", "magicutorApiBase", "magicutorBridgeUrl"):
                parsed = urllib.parse.urlsplit(data.get(key, ""))
                require(parsed.hostname in ("127.0.0.1", "localhost", "::1")
                        and parsed.scheme in ("http", "ws") and not parsed.username
                        and not parsed.password and not parsed.query and not parsed.fragment,
                        f"invalid loopback endpoint: {key}")
            return {"schema_version": 1}
        self.check("host.endpoints", endpoints)
        def automation():
            data = self.http(origin(gateway) + "/host/automation/status")
            require(data.get("available") is True, "host automation unavailable; verify desktop TCC grants")
            return {"available": True}
        available = self.check("host.automation", automation)
        if self.config.get("skip_capture"):
            self.skip("host.capture", "operator selected --skip-capture; host stage incomplete")
        elif not available:
            self.skip("host.capture", "host automation prerequisite failed")
        else:
            self.check("host.capture", lambda: png_metadata(self.http(
                origin(gateway) + "/host/screen/capture", method="POST", body={"region": "0,0,64,64"})))

    def browser(self, temp):
        base = origin(self.config["magicutor_url"])
        shared = Path(self.config["shared_run_dir"])
        nonce = self.config["nonce"]
        def shared_directory():
            require((shared / "host-marker").read_text() == nonce,
                    "shared directory marker mismatch; mount the same absolute host path")
            (shared / "container-marker").write_text(nonce)
            (shared / "downloads").mkdir(exist_ok=False)
            return {"host_to_container": True}
        shared_ok = self.check("browser.shared_directory", shared_directory)
        def identity():
            data = self.http(base + "/json/version")
            require(data.get("Browser") == "Chrome/magicutor-bridge", "endpoint is not the Magicutor CDP proxy")
            return {"browser": "Chrome/magicutor-bridge"}
        proxy_ok = self.check("browser.proxy_identity", identity)
        binary = self.config["agent_browser"]
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("AGENT_BROWSER_") and key not in ("NODE_OPTIONS", "PYTHONPATH")}
        home = temp / "home"
        home.mkdir()
        empty_config = temp / "agent-browser.json"
        empty_config.write_text("{}")
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(home),
                   AGENT_BROWSER_SOCKET_DIR=str(temp / "s"),
                   AGENT_BROWSER_DEFAULT_TIMEOUT=str(self.timeout * 1000),
                   AGENT_BROWSER_MAX_OUTPUT="262144", NO_PROXY="*", no_proxy="*")
        parsed = urllib.parse.urlsplit(base)
        ws = urllib.parse.urlunsplit(("wss" if parsed.scheme == "https" else "ws", parsed.netloc,
                                     "/devtools/browser/" + self.thread, "", ""))
        common = [binary, "--config", str(empty_config), "--session", self.thread, "--cdp", ws, "--json"]
        def cli(*args):
            output = self.cmd([*common, *args], cwd=temp, env=env)
            data = json.loads(output)
            require(data.get("success") is True, "agent-browser returned an unsuccessful result")
            return data.get("data", {})
        def core():
            output = self.cmd([binary, "--config", str(empty_config), "skills", "get", "core", "--full"],
                              cwd=temp, env=env)
            require("snapshot" in output and "agent-browser" in output, "bundled core skill is missing")
            return {"bundled_core": True}
        core_ok = self.check("browser.cli_core", core)
        if not (shared_ok and proxy_ok and core_ok):
            for name in ("navigate_snapshot", "client_reconnect", "download", "cleanup"):
                self.skip("browser." + name, "browser prerequisites failed; no browser session opened")
            return
        content = f"Magician container download {nonce}\n".encode()
        title = "Magician integration " + nonce
        url = self.config["browser_fixture_url"]
        def navigate():
            cli("open", url)
            require(cli("get", "title").get("title") == title, "fixture title did not round-trip through extension")
            download_ref(cli("snapshot", "-i"))
            return {"fixture_title_verified": True, "thread_id": self.thread}
        def reconnect():
            cli("close", "--keep-browser")
            require(cli("get", "title").get("title") == title, "CDP reconnect lost the fixture page")
            download_ref(cli("snapshot", "-i"))
            return {"kind": "client_disconnect_reconnect", "fixture_preserved": True}
        def download():
            ref = download_ref(cli("snapshot", "-i"))
            dest = shared / "downloads" / "fixture.txt"
            cli("download", ref, str(dest))
            require(dest.is_file() and dest.read_bytes() == content,
                    "download not readable with exact expected bytes inside Linux")
            return {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}
        try:
            navigated = self.check("browser.navigate_snapshot", navigate)
            if navigated:
                self.check("browser.client_reconnect", reconnect)
                self.check("browser.download", download)
            else:
                self.skip("browser.client_reconnect", "fixture navigation failed")
                self.skip("browser.download", "fixture navigation failed")
        finally:
            def cleanup():
                # Attempt the thread deletion even if stopping the CLI fails.
                try:
                    cli("close", "--keep-browser")
                finally:
                    ack = self.http(base + "/cdp/threads/" + self.thread, method="DELETE")
                    require(ack.get("cleared") is True and ack.get("thread_id") == self.thread,
                            "Magicutor did not acknowledge test-thread cleanup")
                return {"thread_cleanup_acknowledged": True}
            self.check("browser.cleanup", cleanup)

    def skills(self, temp):
        # Match the installed dependency roots without inheriting credentials,
        # Python/Node startup injection or an unconstrained OCR thread count.
        home = temp / "skill-home"
        home.mkdir()
        env = {"HOME": str(home), "TMPDIR": str(temp), "LANG": "C.UTF-8",
               "PATH": os.pathsep.join(map(str, (self.root / ".node/bin", self.root / ".venv/bin",
                    self.root / "pdftotext/bin", self.root / "ocr/bin", "/usr/local/bin", "/usr/bin", "/bin"))),
               "OMP_THREAD_LIMIT": "1", "OPENBLAS_NUM_THREADS": "1", "PYTHONDONTWRITEBYTECODE": "1"}
        def command(argv, **kwargs):
            return self.cmd(argv, cwd=temp, env=env, **kwargs)
        nonce = "MAGICIAN " + self.config["nonce"]
        pdf = temp / "fixture.pdf"
        pdf.write_bytes(fixture_pdf(nonce))
        html = temp / "fixture.html"
        html.write_text(f"<html><body><article><h1>Integration fixture</h1><p>{nonce}</p>"
                        "<p>This deterministic local document verifies that the installed HTML extraction dependencies work.</p>"
                        "</article></body></html>")
        source = temp / "fixture.txt"
        source.write_text(nonce + "\n")
        def pdf_probe():
            output = command([str(self.root / "pdftotext/bin/pdftotext"), str(pdf), "-"])
            require(nonce in output, "PDF extraction did not return fixture text")
            return {"fixture_text_verified": True}
        self.check("skills.pdf", pdf_probe)
        python = str(self.root / ".venv/bin/python")
        def ocr():
            output = json.loads(command([python, str(self.root / "ocr/bin/ocr"), "extract",
                "--input-file", str(self.root / "ocr/canary-fixtures/canary-scan.png"), "--engine", "tesseract"]))
            require(not output.get("error") and "Tesseract fixture line 4821" in output.get("content", ""),
                    "OCR did not extract the shipped fixture phrase")
            return {"engine": "tesseract", "fixture_text_verified": True}
        self.check("skills.ocr", ocr)
        def python_html():
            output = json.loads(command([python, str(self.root / "htmltotext/bin/htmltotext")],
                stdin=json.dumps({"input_file": str(html)}).encode()))
            require(not output.get("error") and output.get("chars_extracted", 0) > 0
                    and nonce in output.get("content", ""), "Python HTML extractor failed or returned empty content")
            return {"fixture_text_verified": True}
        self.check("skills.python_html", python_html)
        def node_rg():
            js = """const {rgPath}=require(process.argv[1]);
const r=require('node:child_process').spawnSync(rgPath,['--fixed-strings','--',process.argv[2],process.argv[3]],{encoding:'utf8',timeout:10000,maxBuffer:65536});
if(r.status!==0)process.exit(1);process.stdout.write(JSON.stringify({match:r.stdout.trim()}));"""
            output = json.loads(command([str(self.root / ".node/bin/node"), "-e", js,
                str(self.root / "node_modules/@vscode/ripgrep"), nonce, str(source)]))
            require(output.get("match") == nonce, "Node-backed ripgrep did not return fixture text")
            return {"package": "@vscode/ripgrep", "fixture_text_verified": True}
        self.check("skills.node_rg", node_rg)

    def run(self):
        def runtime():
            require(sys.platform.startswith("linux"), "worker must run inside Linux")
            require(os.geteuid() != 0, "container executes as root; use the image's non-root user")
            return {"platform": sys.platform, "uid": os.geteuid()}
        if self.check("runtime.nonroot_linux", runtime):
            with tempfile.TemporaryDirectory(prefix="mgi-", dir="/tmp") as directory:
                temp = Path(directory)
                for stage in self.config["stages"]:
                    before = len(self.checks)
                    try:
                        if stage == "host":
                            self.host()
                        else:
                            getattr(self, stage)(temp)
                    except Exception as error:
                        detail = str(error) if isinstance(error, ProbeError) else type(error).__name__
                        self.checks.append({"name": stage + ".setup", "status": "fail", "detail": detail})
                    recorded = {item["name"] for item in self.checks[before:]}
                    for name in CHECKS[stage]:
                        if stage + "." + name not in recorded:
                            self.skip(stage + "." + name, "stage setup failed")
        else:
            for stage in self.config["stages"]:
                for name in CHECKS[stage]:
                    self.skip(stage + "." + name, "Linux non-root prerequisite failed")
        return {"schema": SCHEMA, "run_id": self.thread, "stages": self.config["stages"],
                "checks": self.checks, "result": result_for(self.checks, self.config["stages"])}


def browser_fixture_server(nonce):
    """Only the host browser visits this ephemeral loopback HTTP fixture.

    Chrome can abort a top-level data: navigation issued via the extension.
    Serve exactly two nonce-scoped resources, never a directory or user files.
    """
    title = "Magician integration " + nonce
    page = (f'<title>{title}</title><h1>{title}</h1>'
            f'<a download="fixture.txt" href="/{nonce}/fixture.txt">Download integration fixture</a>').encode()
    content = f"Magician container download {nonce}\n".encode()
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_GET(self):
            if self.path == f"/{nonce}/index.html":
                payload, mime = page, "text/html; charset=utf-8"
            elif self.path == f"/{nonce}/fixture.txt":
                payload, mime = content, "text/plain"
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(payload)))
            if self.path.endswith("fixture.txt"):
                self.send_header("Content-Disposition", 'attachment; filename="fixture.txt"')
            self.end_headers()
            self.wfile.write(payload)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_port}/{nonce}/index.html"


def prepare_extension(destination, magician_port, magicutor_port, source=None):
    """Create a separate unpacked extension; never edit the installed source.

    Pin discovery as well as initial defaults. Otherwise the live desktop's
    periodic discovery contract silently moves the test profile to live ports.
    """
    source = source or Path(__file__).resolve().parent.parent / "magicutor/extension"
    require(destination.is_absolute() and not destination.exists(),
            "extension destination must be a new absolute directory")
    require(1024 <= magician_port <= 65535 and 1024 <= magicutor_port <= 65535
            and magician_port != magicutor_port and magician_port not in (3002, 3003)
            and magicutor_port not in (3002, 3003), "use distinct non-live service ports (for example 13002/13003)")
    config = (source / "config.js").read_text()
    signature = "export async function refreshRuntimeEndpoints("
    require(config.count(signature) == 1, "extension discovery interface changed; review test overlay")
    endpoints = {"schemaVersion": 1,
                 "magicianApiBase": f"http://127.0.0.1:{magician_port}/api/magician/v2",
                 "magicianHealthUrl": f"http://127.0.0.1:{magician_port}/health",
                 "magicutorApiBase": f"http://127.0.0.1:{magicutor_port}",
                 "magicutorBridgeUrl": f"ws://127.0.0.1:{magicutor_port}/bridge/native"}
    config = config.replace(signature, "async function productionRefreshRuntimeEndpoints(")
    config += "\n// Generated integration copy: disable live discovery/cache redirection.\n"
    config += "const integrationEndpoints = " + json.dumps(endpoints) + ";\n"
    config += """applyRuntimeEndpoints(integrationEndpoints);
export async function refreshRuntimeEndpoints() {
    const previous = currentRuntimeEndpoints();
    applyRuntimeEndpoints(integrationEndpoints);
    return {source: 'integration-fixture', changed: endpointsChanged(previous), endpoints: integrationEndpoints};
}
"""
    manifest = json.loads((source / "manifest.json").read_text())
    manifest["name"] += " [Integration test]"
    shutil.copytree(source, destination, ignore=shutil.ignore_patterns("node_modules", ".git", "__pycache__"))
    (destination / "config.js").write_text(config)
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (destination / "integration-endpoints.json").write_text(json.dumps(endpoints, indent=2) + "\n")
    return destination


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--runtime", choices=("docker", "apple-container"), default="docker")
    p.add_argument("--container", help="existing running test container; never started or stopped")
    p.add_argument("--stages", default="host,browser,skills")
    p.add_argument("--gateway-url", default="", help="origin as seen INSIDE Linux; defaults to its MAGICIAN_HOST_GATEWAY_URL")
    p.add_argument("--magicutor-url", default="http://127.0.0.1:3003", help="origin inside Linux")
    p.add_argument("--shared-dir", type=Path, help="browser: fresh run directory is created here; mount at SAME absolute path in Linux")
    p.add_argument("--report-dir", type=Path, help="parent for immutable per-run JSON evidence")
    p.add_argument("--skillshub-root", default="/app/skillshub")
    p.add_argument("--agent-browser", default="/data/scopes/anonymous/default/skills/browser/bin/agent-browser")
    p.add_argument("--timeout-seconds", type=int, default=30)
    p.add_argument("--skip-capture", action="store_true", help="report partial instead of taking a 64x64 screen sample")
    p.add_argument("--prepare-extension-dir", type=Path, help="prepare a separate test extension copy and exit; does not launch Chrome")
    p.add_argument("--extension-magician-port", type=int, default=13002)
    p.add_argument("--extension-magicutor-port", type=int, default=13003)
    return p


def host_main(argv=None):
    p = parser()
    args = p.parse_args(argv)
    if args.prepare_extension_dir is not None:
        try:
            dest = prepare_extension(args.prepare_extension_dir, args.extension_magician_port, args.extension_magicutor_port)
        except (ProbeError, OSError) as error:
            p.error(str(error))
        print(f"Test extension: {dest}\nLoad unpacked in a separate Chrome profile. No browser was started.")
        return 0
    if not args.container or args.report_dir is None:
        p.error("qualification requires --container and --report-dir")
    stages = args.stages.split(",")
    if not stages or len(set(stages)) != len(stages) or any(stage not in STAGES for stage in stages):
        p.error("--stages must be a unique comma-separated subset of host,browser,skills")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", args.container):
        p.error("invalid container name")
    if not 1 <= args.timeout_seconds <= 120:
        p.error("--timeout-seconds must be between 1 and 120")
    if "browser" in stages and (args.shared_dir is None or not args.shared_dir.is_absolute()):
        p.error("browser stage requires an absolute --shared-dir mounted at the same path in Linux")
    try:
        origin(args.magicutor_url)
        if args.gateway_url:
            origin(args.gateway_url)
    except ProbeError as error:
        p.error(str(error))
    run_id = "mgi-" + uuid.uuid4().hex[:16]
    nonce = uuid.uuid4().hex
    evidence_dir = args.report_dir.resolve() / run_id
    evidence_dir.mkdir(parents=True, exist_ok=False)
    shared = None
    source = Path(__file__).read_text()
    config = {"run_id": run_id, "nonce": nonce, "stages": stages,
              "gateway_url": args.gateway_url, "magicutor_url": args.magicutor_url,
              "skillshub_root": args.skillshub_root, "agent_browser": args.agent_browser,
              "timeout": args.timeout_seconds, "skip_capture": args.skip_capture}
    report = {"schema": SCHEMA, "run_id": run_id, "result": "fail", "checks": [], "stages": stages}
    fixture_server = None
    try:
        if "browser" in stages:
            shared = args.shared_dir.resolve() / run_id
            shared.mkdir(parents=True, exist_ok=False)
            (shared / "host-marker").write_text(nonce)
            config["shared_run_dir"] = str(shared)
            fixture_server, config["browser_fixture_url"] = browser_fixture_server(nonce)
        runtime = "container" if args.runtime == "apple-container" else "docker"
        # Send source directly: even an older image can run the new probes. The
        # explicit command overrides no container configuration or service.
        output = run_command([runtime, "exec", "-i", args.container, "python3", "-c", source, "--worker"],
                             stdin=json.dumps(config).encode(), timeout=args.timeout_seconds * 30 + 60)
        candidate = json.loads(output)
        validate_worker_report(candidate, config)
        report = candidate
        if shared and (shared / "container-marker").is_file():
            require((shared / "container-marker").read_text() == nonce, "container-to-host marker mismatch")
            report["checks"].append({"name": "browser.container_to_host", "status": "pass"})
        elif "browser" in stages:
            report["checks"].append({"name": "browser.container_to_host", "status": "fail", "detail": "container marker not visible on host"})
    except Exception as error:
        detail = str(error) if isinstance(error, ProbeError) else type(error).__name__
        # A malformed report must not break failure evidence publication.
        if not isinstance(report, dict) or not isinstance(report.get("checks"), list):
            report = {"schema": SCHEMA, "run_id": run_id, "stages": stages, "checks": []}
        report["checks"].append({"name": "harness.execution", "status": "fail", "detail": detail})
    finally:
        if fixture_server:
            fixture_server.shutdown()
            fixture_server.server_close()
    report.update(schema=SCHEMA, run_id=run_id, stages=stages,
                  runtime=args.runtime, container=args.container, source_sha256=hashlib.sha256(source.encode()).hexdigest(),
                  shared_run_dir=str(shared) if shared else None,
                  scope="direct integration probes; not governed task execution or extension restart qualification")
    report["result"] = result_for(report["checks"], stages)
    path = evidence_dir / "report.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    for item in report["checks"]:
        print(f"[{item['status'].upper()}] {item['name']}: {item.get('detail', '')}")
    print(f"Result: {report['result']}\nEvidence: {path}")
    return {"pass": 0, "partial": 2, "fail": 1}[report["result"]]


if __name__ == "__main__":
    if sys.argv[1:] == ["--worker"]:
        print(json.dumps(Probes(json.load(sys.stdin)).run()))
    else:
        raise SystemExit(host_main())
