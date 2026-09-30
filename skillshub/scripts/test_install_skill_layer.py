from __future__ import annotations

import importlib.util
import tempfile
import threading
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("install_skill_layer.py")
SPEC = importlib.util.spec_from_file_location("install_skill_layer", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
INSTALLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALLER)

UNINSTALL_MODULE_PATH = Path(__file__).with_name("uninstall_skill_layer.py")
UNINSTALL_SPEC = importlib.util.spec_from_file_location(
    "uninstall_skill_layer", UNINSTALL_MODULE_PATH
)
assert UNINSTALL_SPEC is not None and UNINSTALL_SPEC.loader is not None
UNINSTALLER = importlib.util.module_from_spec(UNINSTALL_SPEC)
UNINSTALL_SPEC.loader.exec_module(UNINSTALLER)


class InstallSkillLayerTests(unittest.TestCase):
    def test_layer_classifier_surfaces_child_stderr(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            classifier = root / "classifier.py"
            classifier.write_text(
                "import sys\n"
                "print('missing bootstrap dependency', file=sys.stderr)\n"
                "raise SystemExit(7)\n",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(
                RuntimeError,
                "(?s)exit status 7:.*missing bootstrap dependency",
            ):
                INSTALLER._layer_names(classifier, "system", root)

    def test_reinstall_preserves_all_real_scoped_config_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            destination = root / "runtime" / "example"
            (source / "config").mkdir(parents=True)
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            (source / "config" / ".env.example").write_text(
                "TOKEN=", encoding="utf-8"
            )

            (destination / "config" / "oauth").mkdir(parents=True)
            (destination / "config" / ".env").write_text(
                "TOKEN=secret", encoding="utf-8"
            )
            (destination / "config" / "oauth" / "tokens.json").write_text(
                '{"access":"secret"}', encoding="utf-8"
            )
            (destination / "stale-wrapper.py").write_text(
                "retired", encoding="utf-8"
            )

            INSTALLER.install_skill(source, destination)

            self.assertTrue((destination / "SKILL.md").is_symlink())
            self.assertTrue((destination / "config").is_symlink())
            self.assertFalse((destination / "config").readlink().is_absolute())
            self.assertTrue((destination / "config" / ".env.example").is_symlink())
            self.assertEqual(
                (destination / "config" / ".env").read_text(encoding="utf-8"),
                "TOKEN=secret",
            )
            self.assertEqual(
                (destination / "config" / "oauth" / "tokens.json").read_text(
                    encoding="utf-8"
                ),
                '{"access":"secret"}',
            )
            self.assertFalse((destination / "stale-wrapper.py").exists())

    def test_source_env_never_replaces_scoped_env(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            destination = root / "runtime" / "example"
            (source / "config").mkdir(parents=True)
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            (source / "config" / ".env").write_text(
                "TOKEN=source", encoding="utf-8"
            )
            (destination / "config").mkdir(parents=True)
            (destination / "config" / ".env").write_text(
                "TOKEN=scoped", encoding="utf-8"
            )

            INSTALLER.install_skill(source, destination)

            installed_env = destination / "config" / ".env"
            self.assertFalse(installed_env.is_symlink())
            self.assertEqual(installed_env.read_text(encoding="utf-8"), "TOKEN=scoped")

    def test_source_only_and_macos_metadata_are_not_materialized(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            destination = root / "runtime" / "example"
            (source / "_vendor").mkdir(parents=True)
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            (source / ".DS_Store").write_bytes(b"metadata")
            (source / "_vendor" / "source.rs").write_text(
                "vendored", encoding="utf-8"
            )

            INSTALLER.install_skill(source, destination)

            self.assertTrue((destination / "SKILL.md").is_symlink())
            self.assertFalse((destination / ".DS_Store").exists())
            self.assertFalse((destination / "_vendor").exists())

    def test_install_never_follows_a_top_level_destination_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            external = root / "external"
            destination = root / "runtime" / "example"
            source.mkdir(parents=True)
            external.mkdir()
            destination.parent.mkdir()
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            (external / "keep").write_text("external", encoding="utf-8")
            destination.symlink_to(external, target_is_directory=True)

            with self.assertRaisesRegex(ValueError, "real directory"):
                INSTALLER.install_skill(source, destination)

            self.assertTrue(destination.is_symlink())
            self.assertEqual((external / "keep").read_text(encoding="utf-8"), "external")

    def test_install_never_follows_an_installer_state_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            runtime = root / "runtime"
            external = root / "external"
            source.mkdir(parents=True)
            runtime.mkdir()
            external.mkdir()
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            (external / "keep").write_text("external", encoding="utf-8")
            (runtime / ".skill-state").symlink_to(
                external, target_is_directory=True
            )

            with self.assertRaisesRegex(ValueError, "real directory"):
                INSTALLER.install_skill(source, runtime / "example")

            self.assertEqual((external / "keep").read_text(encoding="utf-8"), "external")
            self.assertFalse((external / "example").exists())

    def test_provider_state_written_through_installed_config_survives_reinstall(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            destination = root / "runtime" / "example"
            (source / "config").mkdir(parents=True)
            (source / "SKILL.md").write_text("v1", encoding="utf-8")

            INSTALLER.install_skill(source, destination)
            token = destination / "config" / "oauth" / "tokens.json"
            token.parent.mkdir(parents=True)
            token.write_text('{"refresh":"new"}', encoding="utf-8")
            (source / "SKILL.md").write_text("v2", encoding="utf-8")
            INSTALLER.install_skill(source, destination)

            self.assertEqual(token.read_text(encoding="utf-8"), '{"refresh":"new"}')
            stable = destination.parent / ".skill-state" / "example" / "config"
            self.assertEqual(token.resolve(), (stable / "oauth" / "tokens.json").resolve())

    def test_parallel_reinstalls_never_expose_a_missing_skill_tree(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            destination = root / "runtime" / "example"
            source.mkdir(parents=True)
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            INSTALLER.install_skill(source, destination)

            stop = threading.Event()
            missing: list[str] = []
            failures: list[BaseException] = []

            def observe() -> None:
                while not stop.is_set():
                    if not (destination / "SKILL.md").exists():
                        missing.append("missing")

            def reinstall() -> None:
                try:
                    for _ in range(30):
                        INSTALLER.install_skill(source, destination)
                except BaseException as error:  # pragma: no cover - reported below
                    failures.append(error)

            observer = threading.Thread(target=observe)
            workers = [threading.Thread(target=reinstall) for _ in range(2)]
            observer.start()
            for worker in workers:
                worker.start()
            for worker in workers:
                worker.join()
            stop.set()
            observer.join()

            self.assertEqual(failures, [])
            self.assertEqual(missing, [])
            self.assertTrue((destination / "SKILL.md").is_symlink())

    def test_uninstall_removes_package_links_but_retains_stable_provider_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source" / "example"
            destination = root / "runtime" / "example"
            source.mkdir(parents=True)
            (source / "SKILL.md").write_text("source", encoding="utf-8")
            INSTALLER.install_skill(source, destination)
            token = destination / "config" / "oauth" / "tokens.json"
            token.parent.mkdir(parents=True)
            token.write_text("token", encoding="utf-8")

            removed, kept = UNINSTALLER.uninstall_skill_dir(destination)

            self.assertGreaterEqual(removed, 2)
            self.assertEqual(kept, 0)
            self.assertFalse(destination.exists())
            stable_token = (
                destination.parent
                / ".skill-state"
                / "example"
                / "config"
                / "oauth"
                / "tokens.json"
            )
            self.assertEqual(stable_token.read_text(encoding="utf-8"), "token")

    def test_uninstall_all_never_enumerates_hidden_installer_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skills = root / "skills"
            skills.mkdir()
            (skills / "visible-skill").mkdir()
            (skills / ".skill-state" / "visible-skill" / "config").mkdir(
                parents=True
            )
            (skills / ".skill-install-locks").mkdir()
            external = root / "external"
            external.mkdir()
            (skills / "linked-skill").symlink_to(external, target_is_directory=True)

            self.assertEqual(
                UNINSTALLER.installed_skill_targets(skills),
                [skills / "visible-skill"],
            )

            removed, kept = UNINSTALLER.uninstall_skill_dir(skills / "linked-skill")
            self.assertEqual((removed, kept), (1, 0))
            self.assertTrue(external.is_dir())


if __name__ == "__main__":
    unittest.main()
