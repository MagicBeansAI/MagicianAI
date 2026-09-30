"""Provider-free regressions; no browser, container, gateway, or build is started."""
from __future__ import annotations

import base64
import contextlib
import importlib.util
import io
import json
import os
import shutil
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch
import zlib


SPEC = importlib.util.spec_from_file_location("container_integration", Path(__file__).with_name("qualify-container-integration.py"))
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)


def png():
    def chunk(kind, value):
        return struct.pack(">I", len(value)) + kind + value + struct.pack(">I", zlib.crc32(kind + value))
    raw = b"\x89PNG\r\n\x1a\n"
    raw += chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
    raw += chunk(b"IDAT", zlib.compress(b"\0\xff\xff\xff")) + chunk(b"IEND", b"")
    return {"content_type": "image/png", "image_b64": base64.b64encode(raw).decode()}


class FakeProbes(m.Probes):
    def __init__(self, config, faults=()):
        super().__init__(config)
        self.faults = set(faults)
        self.commands = []
        self.requests = []

    def http(self, url, **kwargs):
        self.requests.append((url, kwargs))
        if "unreachable" in self.faults:
            raise m.ProbeError("service unreachable")
        if url.endswith("/host/runtime/endpoints"):
            return {"schemaVersion": 1, "magicianApiBase": "http://127.0.0.1:13002/api/magician/v2",
                    "magicianHealthUrl": "http://127.0.0.1:13002/health",
                    "magicutorApiBase": "http://127.0.0.1:13003",
                    "magicutorBridgeUrl": "ws://127.0.0.1:13003/bridge/native"}
        if url.endswith("/host/automation/status"):
            return {"available": "no_automation" not in self.faults}
        if url.endswith("/host/screen/capture"):
            return {"content_type": "image/png", "image_b64": "not-png"} if "bad_png" in self.faults else png()
        if url.endswith("/json/version"):
            return {"Browser": "Chrome" if "wrong_proxy" in self.faults else "Chrome/magicutor-bridge"}
        if "/cdp/threads/" in url:
            if "cleanup" in self.faults:
                raise m.ProbeError("cleanup unavailable")
            return {"cleared": True, "thread_id": self.thread}
        raise AssertionError("unexpected HTTP request")

    def cmd(self, argv, **kwargs):
        args = list(map(str, argv))
        self.commands.append((args, kwargs))
        nonce = self.config["nonce"]
        if "skills" in args:
            return "agent-browser snapshot core"
        if "--json" in args:
            action = args[args.index("--json") + 1:]
            if action[0] in self.faults:
                raise m.ProbeError("browser command failed")
            data = {}
            if action[:2] == ["get", "title"]:
                data = {"title": "Magician integration " + nonce}
            if action[0] == "snapshot":
                data = {"refs": {"e3": {"role": "link", "name": "Download integration fixture"}}}
            if action[0] == "download":
                Path(action[2]).write_text("wrong" if "wrong_download" in self.faults else f"Magician container download {nonce}\n")
            if "json_error" in self.faults:
                return json.dumps({"success": False, "error": "failed despite exit zero"})
            return json.dumps({"success": True, "data": data})
        if args[0].endswith("pdftotext"):
            return "MAGICIAN " + nonce
        if "/ocr/bin/ocr" in args[1]:
            return json.dumps({"content": "Tesseract fixture line 4821", "pages": 1})
        if "/htmltotext/bin/htmltotext" in args[1]:
            if "python_error" in self.faults:
                return json.dumps({"error": "missing dependency", "chars_extracted": 0, "content": ""})
            return json.dumps({"content": "MAGICIAN " + nonce, "chars_extracted": 50})
        if args[0].endswith("node"):
            if "missing_node" in self.faults:
                raise m.ProbeError("missing executable: node")
            return json.dumps({"match": "MAGICIAN " + nonce})
        raise AssertionError("unexpected command")


class IntegrationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.shared = Path(self.tmp.name) / "shared"
        self.shared.mkdir()
        self.config = {"run_id": "mgi-fixture", "nonce": "1234abcd", "timeout": 1,
                       "stages": list(m.STAGES), "gateway_url": "http://host.container.internal:3017",
                       "magicutor_url": "http://127.0.0.1:3003", "skillshub_root": "/app/skillshub",
                       "browser_fixture_url": "http://127.0.0.1:43210/1234abcd/index.html",
                       "agent_browser": "/scope/browser/bin/agent-browser", "shared_run_dir": str(self.shared)}
        (self.shared / "host-marker").write_text(self.config["nonce"])

    def run_probes(self, faults=()):
        probes = FakeProbes(self.config, faults)
        with patch.object(m.sys, "platform", "linux"), patch.object(m.os, "geteuid", return_value=1000):
            return probes, probes.run()

    def test_complete_fixture_and_scoped_cleanup(self):
        probes, report = self.run_probes()
        self.assertEqual(report["result"], "pass")
        self.assertTrue(all(item["status"] == "pass" for item in report["checks"]))
        self.assertEqual(len(report["checks"]), 1 + sum(map(len, m.CHECKS.values())))
        self.assertNotIn("image_b64", json.dumps(report))
        self.assertTrue(probes.requests[-1][0].endswith("/cdp/threads/mgi-fixture"))
        self.assertEqual(probes.requests[-1][1]["method"], "DELETE")
        browser_commands = [a for a, kw in probes.commands if "--json" in a]
        self.assertTrue(all("ws://127.0.0.1:3003/devtools/browser/mgi-fixture" in a for a in browser_commands))
        self.assertTrue(any("@e3" in a for a in browser_commands))
        self.assertFalse(any("--all" in a or "--auto-connect" in a for a in browser_commands))

    def test_download_requires_actual_bytes_not_cli_success(self):
        _, report = self.run_probes(["wrong_download"])
        self.assertEqual(report["result"], "fail")
        self.assertEqual(next(x for x in report["checks"] if x["name"] == "browser.download")["status"], "fail")

    def test_gateway_failure_does_not_hide_skill_results(self):
        _, report = self.run_probes(["unreachable"])
        self.assertEqual(report["result"], "fail")
        self.assertEqual(len([x for x in report["checks"] if x["name"].startswith("skills.") and x["status"] == "pass"]), 4)

    def test_root_or_non_linux_worker_does_not_touch_services(self):
        for platform, uid in (("darwin", 1000), ("linux", 0)):
            probes = FakeProbes(self.config)
            with patch.object(m.sys, "platform", platform), patch.object(m.os, "geteuid", return_value=uid):
                self.assertEqual(probes.run()["result"], "fail")
            self.assertEqual(probes.commands, [])
            self.assertEqual(probes.requests, [])

    def test_no_mount_means_no_browser_actions(self):
        (self.shared / "host-marker").write_text("different")
        probes, report = self.run_probes()
        self.assertEqual(report["result"], "fail")
        self.assertFalse(any("--json" in a for a, kw in probes.commands))

    def test_ordinary_cdp_is_rejected_without_opening_browser(self):
        probes, report = self.run_probes(["wrong_proxy"])
        self.assertEqual(report["result"], "fail")
        self.assertFalse(any("--json" in a for a, kw in probes.commands))

    def test_navigation_failure_still_cleans_own_thread(self):
        probes, report = self.run_probes(["open"])
        self.assertEqual(report["result"], "fail")
        self.assertEqual(probes.requests[-1][1]["method"], "DELETE")

    def test_cli_zero_exit_error_envelope_is_failure(self):
        _, report = self.run_probes(["json_error"])
        self.assertEqual(report["result"], "fail")

    def test_detach_failure_still_requests_thread_cleanup(self):
        probes, report = self.run_probes(["close"])
        self.assertEqual(report["result"], "fail")
        self.assertEqual(probes.requests[-1][1]["method"], "DELETE")

    def test_cleanup_failure_is_not_green(self):
        _, report = self.run_probes(["cleanup"])
        self.assertEqual(report["result"], "fail")

    def test_python_error_envelope_fails_even_with_zero_exit(self):
        _, report = self.run_probes(["python_error", "missing_node"])
        self.assertEqual(report["result"], "fail")
        failed = {x["name"] for x in report["checks"] if x["status"] == "fail"}
        self.assertEqual(failed, {"skills.python_html", "skills.node_rg"})

    def test_skip_capture_is_partial(self):
        self.config["skip_capture"] = True
        probes, report = self.run_probes()
        self.assertEqual(report["result"], "partial")
        self.assertFalse(any(url.endswith("/screen/capture") for url, kw in probes.requests))

    def test_selected_stage_never_claims_complete_qualification(self):
        self.config["stages"] = ["skills"]
        _, report = self.run_probes()
        self.assertEqual(report["result"], "partial")

    def test_browser_environment_is_isolated(self):
        with patch.dict(os.environ, {"AGENT_BROWSER_AUTO_CONNECT": "1", "AGENT_BROWSER_PROFILE": "Default", "NODE_OPTIONS": "bad"}):
            probes, _ = self.run_probes()
        env = next(kw["env"] for a, kw in probes.commands if "--json" in a)
        self.assertNotIn("AGENT_BROWSER_AUTO_CONNECT", env)
        self.assertNotIn("AGENT_BROWSER_PROFILE", env)
        self.assertNotIn("NODE_OPTIONS", env)

    def test_png_checksum_and_payload_validation(self):
        self.assertEqual(m.png_metadata(png())["width"], 1)
        for value in ({"content_type": "image/png", "image_b64": "broken"},
                      {"content_type": "image/png", "image_b64": base64.b64encode(b"PNGDATA").decode()}):
            with self.assertRaises(m.ProbeError):
                m.png_metadata(value)
        value = png()
        raw = bytearray(base64.b64decode(value["image_b64"]))
        raw[30] ^= 1
        value["image_b64"] = base64.b64encode(raw).decode()
        with self.assertRaises(m.ProbeError):
            m.png_metadata(value)

    def test_origin_rejects_secrets_and_paths(self):
        for value in ("http://user:secret@localhost:3017", "http://localhost/path", "file:///tmp/x", "http://localhost/?token=x"):
            with self.assertRaises(m.ProbeError):
                m.origin(value)
        self.assertEqual(m.origin("http://host.container.internal:3017/"), "http://host.container.internal:3017")

    def test_pdf_offsets_point_to_objects(self):
        pdf = m.fixture_pdf("MAGICIAN fixture")
        position = int(pdf.split(b"startxref\n")[1].splitlines()[0])
        self.assertTrue(pdf[position:].startswith(b"xref"))
        for number, line in enumerate(pdf[position:].splitlines()[3:8], 1):
            offset = int(line[:10])
            self.assertTrue(pdf[offset:].startswith(f"{number} 0 obj".encode()))

    def test_process_timeout_output_limit_and_exit(self):
        self.assertEqual(m.run_command([sys.executable, "-c", "print('ok')"]), "ok\n")
        for code, timeout, limit in (("import time; time.sleep(5)", 0.05, 1024),
                                     ("print('x'*10000)", 1, 1024), ("raise SystemExit(7)", 1, 1024)):
            with self.assertRaises(m.ProbeError):
                m.run_command([sys.executable, "-c", code], timeout=timeout, limit=limit)

    def test_host_invokes_only_exec_and_retains_worker_failure(self):
        calls = []
        def fake_runtime(argv, **kwargs):
            calls.append(argv)
            config = json.loads(kwargs["stdin"])
            checks = [{"name": "runtime.nonroot_linux", "status": "pass"}]
            checks += [{"name": "skills." + name, "status": "pass"} for name in m.CHECKS["skills"]]
            checks[-1]["status"] = "fail"
            return json.dumps({"schema": m.SCHEMA, "run_id": config["run_id"], "stages": ["skills"], "checks": checks})
        with patch.object(m, "run_command", fake_runtime), contextlib.redirect_stdout(io.StringIO()):
            status = m.host_main(["--runtime", "apple-container", "--container", "isolated-test",
                                  "--stages", "skills", "--report-dir", self.tmp.name])
        self.assertEqual(status, 1)
        self.assertEqual(calls[0][:4], ["container", "exec", "-i", "isolated-test"])
        self.assertEqual(len(calls), 1)
        report = json.loads(next(Path(self.tmp.name).glob("mgi-*/report.json")).read_text())
        self.assertEqual(report["result"], "fail")

    def test_missing_runtime_still_produces_failure_report(self):
        with patch.object(m, "run_command", side_effect=m.ProbeError("missing executable: docker")), contextlib.redirect_stdout(io.StringIO()):
            status = m.host_main(["--container", "test", "--stages", "skills", "--report-dir", self.tmp.name])
        self.assertEqual(status, 1)
        report = json.loads(next(Path(self.tmp.name).glob("mgi-*/report.json")).read_text())
        self.assertEqual(report["checks"][-1]["name"], "harness.execution")

    def test_malformed_worker_report_still_has_failure_evidence(self):
        for output in ("not json", "null", '{"checks":[{}]}'):
            with patch.object(m, "run_command", return_value=output), contextlib.redirect_stdout(io.StringIO()):
                status = m.host_main(["--container", "test", "--stages", "skills", "--report-dir", self.tmp.name])
            self.assertEqual(status, 1)
        for path in Path(self.tmp.name).glob("mgi-*/report.json"):
            self.assertEqual(json.loads(path.read_text())["result"], "fail")

    def test_complete_host_run_checks_reverse_mount_and_reports_pass(self):
        def fake_runtime(argv, **kwargs):
            config = json.loads(kwargs["stdin"])
            worker = FakeProbes(config)
            with patch.object(m.sys, "platform", "linux"), patch.object(m.os, "geteuid", return_value=1000):
                return json.dumps(worker.run())
        with patch.object(m, "run_command", side_effect=fake_runtime), \
                patch.object(m, "browser_fixture_server", return_value=(MagicMock(), self.config["browser_fixture_url"])), \
                contextlib.redirect_stdout(io.StringIO()):
            status = m.host_main(["--container", "isolated-test", "--shared-dir", str(self.shared),
                                  "--gateway-url", "http://host.container.internal:3017", "--report-dir", self.tmp.name])
        self.assertEqual(status, 0)
        report = json.loads(next(Path(self.tmp.name).glob("mgi-*/report.json")).read_text())
        self.assertEqual(report["checks"][-1], {"name": "browser.container_to_host", "status": "pass"})

    def test_skill_children_have_bounded_clean_environment(self):
        probes, _ = self.run_probes()
        env = next(kw["env"] for a, kw in probes.commands if a[0].endswith("pdftotext"))
        self.assertEqual(env["OMP_THREAD_LIMIT"], "1")
        self.assertNotIn("OPENAI_API_KEY", env)
        self.assertNotIn("NODE_OPTIONS", env)

    def test_ref_and_empty_evidence_fail_closed(self):
        with self.assertRaises(m.ProbeError):
            m.download_ref({"refs": {}})
        self.assertEqual(m.result_for([], list(m.STAGES)), "fail")
        self.assertEqual(m.download_ref({"snapshot": '- link "Download integration fixture" [ref=e2]'}), "@e2")

    @unittest.skipUnless(shutil.which("node"), "Node is required to execute the generated extension module")
    def test_test_extension_stays_on_test_ports_despite_live_discovery_and_cache(self):
        source = Path(m.__file__).parent.parent / "magicutor/extension"
        original = (source / "config.js").read_bytes()
        dest = Path(self.tmp.name) / "extension"
        m.prepare_extension(dest, 13002, 13003)
        self.assertEqual((source / "config.js").read_bytes(), original)
        module = "data:text/javascript;base64," + base64.b64encode((dest / "config.js").read_bytes()).decode()
        js = """globalThis.fetch=()=>{throw new Error('live discovery called')};
globalThis.chrome={storage:{local:{get(){throw new Error('live cache read')}}}};
const m=await import(process.argv[1]);
for(let i=0;i<3;i++){const r=await m.refreshRuntimeEndpoints();if(r.source!=='integration-fixture')process.exit(1)}
if(m.MAGICUTOR_BRIDGE_URL!=='ws://127.0.0.1:13003/bridge/native'||m.MAGICIAN_HEALTH_URL!=='http://127.0.0.1:13002/health')process.exit(2);
console.log('isolated');"""
        self.assertEqual(m.run_command([shutil.which("node"), "--input-type=module", "-e", js, module]).strip(), "isolated")
        self.assertIn("[Integration test]", json.loads((dest / "manifest.json").read_text())["name"])

    def test_extension_prep_refuses_overwrite_and_live_ports(self):
        for dest, magician, magicutor in ((self.shared, 13002, 13003),
                                          (Path(self.tmp.name) / "extension", 3002, 3003)):
            with self.assertRaises(m.ProbeError):
                m.prepare_extension(dest, magician, magicutor)


if __name__ == "__main__":
    unittest.main()
