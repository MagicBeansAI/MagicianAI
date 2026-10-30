"""Fault-inject setup/activation mismatches without accounts or network access."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import link_runtime
import shared_skill_bins
import verify_runtime

MAKEFILE = Path(__file__).resolve().parents[2] / "Makefile"


def executable(path, body="#!/bin/sh\nexit 0\n"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(0o755)


def fixture(root, bins=("wu", "node")):
    skill = root / "whatsapp-mcp"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text(
        "---\nname: whatsapp-mcp\nmetadata:\n  magician:\n    requires:\n      bins: "
        + json.dumps(bins) + "\n---\n")
    (root / "package-lock.json").write_text(json.dumps({
        "packages": {"node_modules/wu": {"bin": {"wu": "index.js"}}}}))
    executable(root / "node_modules/.bin/wu")
    executable(root / ".node/bin/node")
    return skill


class SharedBinTests(unittest.TestCase):
    def test_missing_runtime_companion_does_not_block_install_or_scope_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill = fixture(root)
            (skill / "SKILL.md").write_text(
                "---\nname: whatsapp-mcp\nmetadata:\n  magician:\n"
                "    requires:\n      bins: [wu, node]\n"
                "    runtime_contract:\n      requires:\n"
                "        bins: [wu, node, missing-optional-provider]\n        entrypoint: wu\n---\n")
            executable(root / ".venv/bin/python")
            (root / ".node-version").write_text("24.20.0\n")
            executable(root / ".node/bin/node", "#!/bin/sh\necho v24.20.0\n")
            shared_skill_bins.Resolver(root).wire(skill)
            audit = verify_runtime.Audit(root)
            audit.skills(media=False)
            self.assertEqual(audit.errors, [])
            scope = root / "data/scopes/anonymous/default"
            with patch.object(link_runtime, "ROOT", root):
                link_runtime.link_scope(scope, None, root / "store")
            audit.scope(scope)
            self.assertEqual(audit.errors, [])
            # Losing a mandatory entrypoint must still fail verification.
            (skill / "bin/wu").unlink()
            audit.skills(media=False)
            self.assertTrue(any("wu" in error for error in audit.errors))

    def test_shared_install_is_not_activation_readiness_and_repairs_without_host_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill = fixture(root, ("wu",))
            with patch.dict(os.environ, {"PATH": ""}):
                audit = verify_runtime.Audit(root)
                audit.binary(skill, "wu")
                self.assertTrue(audit.errors)
                resolver = shared_skill_bins.Resolver(root)
                self.assertEqual(resolver.wire(skill), 2)  # Includes npm's interpreter.
                self.assertEqual(resolver.wire(skill), 0)
                audit = verify_runtime.Audit(root)
                audit.binary(skill, "wu")
                audit.binary(skill, "node")
                self.assertEqual(audit.errors, [])

    def test_missing_managed_dependency_never_selects_host_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill = fixture(root)
            (root / "node_modules/.bin/wu").unlink()
            executable(root / "host/wu")
            with patch.dict(os.environ, {"PATH": str(root / "host")}):
                shared_skill_bins.Resolver(root).wire(skill)
                audit = verify_runtime.Audit(root)
                audit.binary(skill, "wu")
                self.assertTrue(audit.errors)
                self.assertFalse((skill / "bin/wu").exists())

    def test_both_missing_bin_trees_are_reported_then_new_workspace_gets_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hub = root / "hub"
            skill = fixture(hub)
            scope = root / "data/scopes/alice/new"
            scoped = scope / "skills/whatsapp-mcp"
            scoped.mkdir(parents=True)
            (scoped / "SKILL.md").symlink_to(skill / "SKILL.md")
            audit = verify_runtime.Audit(hub)
            audit.scope(scope)
            self.assertEqual(len(audit.errors), 2)
            with patch.object(link_runtime, "ROOT", hub):
                link_runtime.link_scope(scope, None, root / "data/system/skill-bin")
            audit = verify_runtime.Audit(hub)
            audit.scope(scope)
            self.assertEqual(audit.errors, [])

    def test_preserves_source_wrappers_and_user_owned_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hub = root / "hub"
            skill = fixture(hub)
            executable(skill / "bin/wu", "#!/bin/sh\necho own-wrapper\n")
            scope = root / "data/scopes/alice/new"
            manifest = scope / "skills/whatsapp-mcp/SKILL.md"
            manifest.parent.mkdir(parents=True)
            manifest.write_text("owner manifest")
            shared_skill_bins.Resolver(hub).wire(skill)
            self.assertIn("own-wrapper", (skill / "bin/wu").read_text())
            with patch.object(link_runtime, "ROOT", hub):
                self.assertEqual(link_runtime.link_scope(scope, None, root / "store"), (0, 1))
            self.assertEqual(manifest.read_text(), "owner manifest")

    def test_moved_package_rebases_generated_links_to_its_own_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "original"
            skill = fixture(source)
            shared_skill_bins.Resolver(source).wire(skill)
            moved = root / "moved package"
            shutil.copytree(source, moved, symlinks=True)
            moved_skill = moved / skill.name
            shared_skill_bins.Resolver(moved).wire(moved_skill)
            for name, target in (("wu", "node_modules/.bin/wu"), ("node", ".node/bin/node")):
                self.assertTrue((moved_skill / "bin" / name).samefile(moved / target))

    def test_declared_bin_cannot_escape_pack(self):
        with tempfile.TemporaryDirectory() as directory:
            skill = fixture(Path(directory), ("../../escape",))
            with self.assertRaisesRegex(ValueError, "invalid CLI basename"):
                shared_skill_bins.Resolver(skill.parent).wire(skill)

    def test_missing_pack_native_does_not_link_back_to_a_global_launcher(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill = fixture(root, ("agent-browser",))
            executable(root / "host/agent-browser", '#!/bin/sh\nexec /missing/pack/bin/agent-browser "$@"\n')
            with patch.dict(os.environ, {"PATH": str(root / "host")}):
                shared_skill_bins.Resolver(root).wire(skill)
                audit = verify_runtime.Audit(root)
                audit.binary(skill, "agent-browser")
                self.assertTrue(audit.errors)
                self.assertFalse((skill / "bin/agent-browser").exists())

    def test_optional_engine_does_not_block_base_skill_software(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill = fixture(root, ())
            (skill / "SKILL.md").write_text(
                "---\nname: fixture\nmetadata:\n  magician:\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n---\n")
            for name in ("lightpanda", "browser"):
                engine = root / name
                engine.mkdir()
                (engine / "SKILL.md").write_text(
                    f"---\nname: {name}\nmetadata:\n  magician:\n    requires:\n      bins: [missing-engine]\n"
                    "    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n---\n")
            executable(root / ".venv/bin/python")
            (root / ".node-version").write_text("24.20.0\n")
            executable(root / ".node/bin/node", "#!/bin/sh\necho v24.20.0\n")
            audit = verify_runtime.Audit(root)
            audit.skills(media=False)
            self.assertEqual(audit.errors, [])

    def test_unprovisioned_optional_engine_does_not_fail_scope_repair(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            skill = root / "lightpanda"
            skill.mkdir()
            (skill / "SKILL.md").write_text("---\nmetadata:\n  magician:\n    requires:\n      bins: [lightpanda]\n---\n")
            scope = root / "scope"
            installed = scope / "skills/lightpanda"
            installed.mkdir(parents=True)
            (installed / "SKILL.md").symlink_to(skill / "SKILL.md")
            audit = verify_runtime.Audit(root)
            audit.scope(scope, exclude=frozenset({"whatsapp-mcp"}))
            self.assertEqual(audit.errors, [])
            audit.scope(scope, only={"lightpanda"})
            self.assertTrue(audit.errors, "explicit engine verification still requires the native binary")

    def test_node_pin_is_checked_instead_of_accepting_any_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".node-version").write_text("24.20.0\n")
            executable(root / ".node/bin/node", "#!/bin/sh\necho v22.0.0\n")
            audit = verify_runtime.Audit(root)
            audit.node()
            self.assertTrue(any("Node version differs" in error for error in audit.errors))

    def test_failed_recheck_replaces_old_success_and_records_freshness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable(root / "tool")
            audit = verify_runtime.Audit(root)
            audit.file(root / "tool", executable=True)
            verify_runtime.write_inventory(root, "bots", audit)
            dest = root / ".runtime/bots.yaml"
            previous = json.loads(dest.read_text())
            self.assertEqual(previous["verification_revision"], 3)
            self.assertEqual(previous["files"][0]["modified_ns"], (root / "tool").stat().st_mtime_ns)
            audit.errors.append("required CLI missing")
            verify_runtime.write_inventory(root, "bots", audit)
            self.assertEqual(json.loads(dest.read_text())["files"], [])
            self.assertEqual(list(dest.parent.glob(".bots-*")), [])

    def test_malformed_input_cannot_leave_an_old_success_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            (root / "package-lock.json").write_text("not json")
            executable(root / ".venv/bin/python")
            (root / ".runtime").mkdir()
            dest = root / ".runtime/skills.yaml"
            dest.write_text('{"schema_version": 1, "files": [{"path": "old"}]}')
            result = subprocess.run([sys.executable, str(MAKEFILE.parent / "scripts/verify_runtime.py"),
                                     "--root", str(root), "--group", "skills", "--write-inventory"],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads(dest.read_text())["files"], [])
            self.assertTrue(json.loads(dest.read_text())["errors"])

    def test_browser_verifies_version_and_core_without_environment_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Makefile").write_text("AGENT_BROWSER_VERSION ?= 1.2.3\n")
            command = root / "browser/bin/agent-browser"
            executable(command, '#!/bin/sh\nif [ "$1" = --version ]; then echo "agent-browser 1.2.3"; '
                       'elif [ -n "$AGENT_BROWSER_SKILLS_DIR" ]; then echo masked; else exit 1; fi\n')
            with patch.dict(os.environ, {"AGENT_BROWSER_SKILLS_DIR": "/mask/broken/layout"}):
                audit = verify_runtime.Audit(root)
                audit.browser_cli(command)
            self.assertEqual(len(audit.errors), 1)
            self.assertIn("skills get core --full", audit.errors[0])
            executable(command, '#!/bin/sh\nif [ "$1" = --version ]; then echo "agent-browser 0.0.1"; else echo core; fi\n')
            audit = verify_runtime.Audit(root)
            audit.browser_cli(command)
            self.assertEqual(len(audit.errors), 1)
            self.assertIn("--version", audit.errors[0])

    def test_bot_wizard_verifies_after_linking_including_scopes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Makefile").write_text(f"include {MAKEFILE}\n")
            executable(root / ".node/bin/node")
            executable(root / "node_modules/.bin/wu")
            scripts = root / "scripts"
            scripts.mkdir()
            for name in ("seed_scope", "install_bot_bundles"):
                (scripts / f"{name}.py").write_text("pass\n")
            (scripts / "link_runtime.py").write_text("from pathlib import Path; Path('linked').touch()\n")
            (scripts / "verify_runtime.py").write_text(
                "from pathlib import Path\nimport sys\nassert Path('linked').exists()\n"
                "assert '--all-scopes' in sys.argv and '--versions' in sys.argv\n"
                "assert sys.argv[sys.argv.index('--group') + 1] == 'bots'\n"
                "Path('verified').touch()\n")
            result = subprocess.run(["make", "setup-wizard-bots", "-o", "setup-bots"], cwd=root,
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue((root / "verified").exists())

    def test_setup_uses_venv_yaml_when_host_python_lacks_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Makefile").write_text(f"include {MAKEFILE}\n")
            executable(root / "host/python3", "#!/bin/sh\nexit 77\n")
            executable(root / ".venv/bin/python", '#!/bin/sh\nif [ "$1" = -c ]; then exit 0; fi\n'
                       + f'exec "{sys.executable}" "$@"\n')
            script = root / "scripts/verify_runtime.py"
            script.parent.mkdir()
            script.write_text("from pathlib import Path; Path('verified').touch()\n")
            env = {**os.environ, "PATH": str(root / "host") + ":/usr/bin:/bin"}
            result = subprocess.run([shutil.which("make"), "check-scopes"], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue((root / "verified").exists())

    def test_bot_preflight_cannot_publish_an_unscoped_success_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Makefile").write_text(f"include {MAKEFILE}\n")
            script = root / "scripts/verify_runtime.py"
            script.parent.mkdir()
            script.write_text("import sys\nassert '--write-inventory' not in sys.argv\n")
            result = subprocess.run(["make", "verify-bot-runtime"], cwd=root,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_install_scope_does_not_rewire_wu_to_legacy_bot_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Makefile").write_text(f"include {MAKEFILE}\n")
            data = root / "notes"
            old = data / "scopes/alice/test/bots/whatsapp/node_modules/.bin/wu"
            executable(old, "#!/bin/sh\necho old-version\n")
            executable(root / "node_modules/.bin/wu")
            script = root / "scripts/install_skill_layer.py"
            script.parent.mkdir()
            script.write_text(
                "from pathlib import Path\nimport sys\n"
                "dest=Path(sys.argv[sys.argv.index('--dest')+1])/'whatsapp/bin/wu'\n"
                "dest.parent.mkdir(parents=True)\n"
                "dest.symlink_to(Path.cwd()/'node_modules/.bin/wu')\n")
            result = subprocess.run(["make", "install-scope", "-o", "validate", "-o", "build",
                                     "SCOPE=alice/test", f"DATA_ROOT={data}"], cwd=root,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            installed = data / "scopes/alice/test/skills/whatsapp/bin/wu"
            self.assertTrue(installed.samefile(root / "node_modules/.bin/wu"))


if __name__ == "__main__":
    unittest.main()
