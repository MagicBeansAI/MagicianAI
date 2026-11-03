from pathlib import Path
import shutil
import sys
import tempfile
import unittest

import yaml

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import migrate_metabase_mcp as migration
import scope_env


class MetabaseMcpTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        shutil.copytree(SCRIPTS.parent / "metabase-mcp", self.source / "metabase-mcp")

    def legacy(self, name, origin):
        scope = self.root / "scopes" / name
        old = scope / "skills/metabase"
        (old / "bin").mkdir(parents=True)
        (old / "bin/metabase-pp-cli").write_text("old executable")
        (old / "SKILL.md").write_text("generated metabase-pp-cli")
        (old / "config").mkdir()
        (old / "config/.env").write_text(f"METABASE_BASE_URL={origin}\nMETABASE_API_KEY=private-fixture\n")
        return scope

    def test_scopes_keep_their_own_origins_and_preserve_credentials(self):
        for name, origin in [("alice/work", "https://alice.example"), ("bob/work", "https://bob.example")]:
            scope = self.legacy(name, origin)
            migration.migrate_scope(scope, self.source)
            new = scope / "skills/metabase-mcp"
            self.assertEqual(scope_env.read_scope_env(new), {"METABASE_BASE_URL": origin})
            # One shared manifest: the origin is resolved from the env at load.
            self.assertTrue((new / "SKILL.md").is_symlink())
            manifest = (new / "SKILL.md").read_text()
            self.assertIn("${METABASE_BASE_URL}/api/metabase-mcp", manifest)
            self.assertNotIn(origin, manifest)
            self.assertNotIn("private-fixture", manifest)
            self.assertFalse((scope / "skills/metabase").exists())
            state = scope / "skills/.skill-state/metabase"
            self.assertTrue((state / "removed").is_file())
            self.assertIn("private-fixture", next(state.glob("retired-*/config/.env")).read_text())
            self.assertEqual(list(state.glob("retired-*/bin")), [])
            self.assertEqual(migration.migrate_scope(scope, self.source), "no legacy package")

    def test_current_mcp_origin_wins(self):
        scope = self.legacy("alice/work", "https://old.example")
        config = scope / "skills/metabase-mcp/config"
        config.mkdir(parents=True)
        (config / ".env").write_text("METABASE_BASE_URL=https://current.example\n")
        migration.migrate_scope(scope, self.source)
        self.assertEqual(scope_env.read_scope_env(config.parent), {"METABASE_BASE_URL": "https://current.example"})

    def test_invalid_origin_keeps_legacy_intact(self):
        scope = self.legacy("alice/work", "https://user:secret@outside.example")
        with self.assertRaisesRegex(ValueError, "invalid legacy origin"):
            migration.migrate_scope(scope, self.source)
        self.assertTrue((scope / "skills/metabase/bin/metabase-pp-cli").is_file())

    def test_package_symlink_does_not_delete_or_copy_shared_state(self):
        shared = self.legacy("shared/work", "https://shared.example") / "skills/metabase"
        scope = self.root / "scopes/alice/work"
        (scope / "skills").mkdir(parents=True)
        (scope / "skills/metabase").symlink_to(shared, target_is_directory=True)
        migration.migrate_scope(scope, self.source)
        self.assertTrue((shared / "bin/metabase-pp-cli").is_file())
        self.assertFalse((scope / "skills/metabase").exists())
        self.assertEqual(scope_env.read_scope_env(scope / "skills/metabase-mcp"), {})

    def test_sql_and_delivery_are_approval_gated(self):
        text = (self.source / "metabase-mcp/SKILL.md").read_text()
        runtime = yaml.safe_load(text.split("---", 2)[1])["metadata"]["magician"]["runtime_contract"]
        discovery = runtime["runtime"]["discovery"]
        self.assertEqual(set(discovery["oauth"]["scopes"]), {
            "agent:content:read", "agent:content:write", "agent:query:run",
            "agent:sql:run", "agent:delivery:write",
        })
        policies = discovery["tool_policies"]
        for name in ("execute_sql", "question_write", "alert_write", "subscription_write"):
            self.assertEqual(policies[name]["risk"], "external_side_effect")
        for name in ("get_content", "browse_data", "run_saved_question", "execute_query"):
            self.assertEqual(policies[name]["risk"], "read_only")
        self.assertNotIn("execute_question", policies)
        self.assertNotIn("read_resource", policies)

    def test_endpoint_and_issuer_are_scope_env_references(self):
        text = (self.source / "metabase-mcp/SKILL.md").read_text()
        runtime = yaml.safe_load(text.split("---", 2)[1])["metadata"]["magician"]["runtime_contract"]["runtime"]
        self.assertEqual(runtime["transport"]["endpoint"], "${METABASE_BASE_URL}/api/metabase-mcp")
        self.assertEqual(runtime["discovery"]["oauth"]["authorization_issuer"], "${METABASE_BASE_URL}")
        self.assertFalse((self.source / "metabase-mcp/render.yaml").exists())

    def test_binary_cleanup_keeps_other_tools(self):
        store = self.root / "system/skill-bin/pin"
        store.mkdir(parents=True)
        (store / "metabase-pp-cli").write_text("retired")
        (store / "other-cli").write_text("keep")
        migration.remove_legacy_binaries(self.root)
        self.assertFalse((store / "metabase-pp-cli").exists())
        self.assertEqual((store / "other-cli").read_text(), "keep")

    def test_workspace_root_symlink_cannot_modify_another_scope(self):
        shared = self.legacy("shared/work", "https://shared.example")
        scope = self.root / "scopes/alice/work"
        scope.mkdir(parents=True)
        (scope / "skills").symlink_to(shared / "skills", target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "real directories"):
            migration.migrate_scope(scope, self.source)
        self.assertTrue((shared / "skills/metabase/bin/metabase-pp-cli").is_file())

    def test_custom_mcp_named_metabase_is_not_removed_for_a_prose_mention(self):
        scope = self.root / "scopes/alice/work"
        custom = scope / "skills/metabase"
        custom.mkdir(parents=True)
        manifest = "---\nmetadata:\n  magician:\n    runtime_contract:\n      runtime: {protocol: mcp}\n---\nReplaces metabase-pp-cli.\n"
        (custom / "SKILL.md").write_text(manifest)
        with self.assertRaisesRegex(ValueError, "unrecognized"):
            migration.migrate_scope(scope, self.source)
        self.assertEqual((custom / "SKILL.md").read_text(), manifest)

    def test_cross_workspace_config_and_state_links_are_rejected(self):
        for entry in ("metabase/config", "metabase-mcp", "metabase-mcp/config", ".skill-state/metabase"):
            with self.subTest(entry=entry):
                scope = self.legacy(entry.replace("/", "-") + "/work", "https://local.example")
                other = self.root / "outside" / entry.replace("/", "-")
                other.mkdir(parents=True)
                secret = other / ".env"
                secret.write_text("METABASE_BASE_URL=https://other.example\nPRIVATE=keep\n")
                link = scope / "skills" / entry
                if link.exists():
                    shutil.rmtree(link)
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to(other, target_is_directory=True)
                with self.assertRaisesRegex(ValueError, "leaves its workspace"):
                    migration.migrate_scope(scope, self.source)
                self.assertEqual(secret.read_text(), "METABASE_BASE_URL=https://other.example\nPRIVATE=keep\n")
                self.assertTrue((scope / "skills/metabase/bin/metabase-pp-cli").is_file())
                self.assertFalse((other / "removed").exists())

    def test_binary_cleanup_rejects_redirected_store(self):
        store = self.root / "outside-store/pin"
        store.mkdir(parents=True)
        binary = store / "metabase-pp-cli"
        binary.write_text("keep")
        (self.root / "system").mkdir()
        (self.root / "system/skill-bin").symlink_to(store.parent, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "real managed directory"):
            migration.remove_legacy_binaries(self.root)
        self.assertTrue(binary.is_file())


if __name__ == "__main__":
    unittest.main()
