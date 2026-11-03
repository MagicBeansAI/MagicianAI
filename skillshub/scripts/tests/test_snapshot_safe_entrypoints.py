"""Entrypoints the governed runtime snapshots must survive being copied alone.

The runtime copies a skill's entrypoint by itself into a private temporary
directory and runs that copy with a curated environment (PATH, LANG, TERM, TZ;
no HOME) from an unrelated working directory. A launcher that loads sibling
files cannot be that entrypoint, so these skills ship a tracked adapter that
execs the real CLI as a PATH companion, in place. Stub companions stand in for
the real CLIs, so no account, network or installed package is needed.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import yaml

SKILLSHUB = Path(__file__).resolve().parents[2]
ADAPTERS = {
    "web-via-tinyfish": ("tinyfish-adapter", "tinyfish"),
    "telegram-self": ("tgcli-adapter", "tgcli"),
    "higgsfield": ("higgsfield-adapter", "higgsfield"),
    "agentmail-read": ("agentmail-read-adapter", "agentmail"),
    "dugite": ("git-adapter", "git"),
}
ARGV = ["send", "text", "--message", "two words", "--", "--looks-like-a-flag"]
ECHO = '#!/bin/sh\nprintf "%s\\n" "${GIT_EXEC_PATH-}" "${GIT_TEMPLATE_DIR-}" "$@"\n'


def executable(path, body=ECHO):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(0o755)


def frontmatter(skill):
    text = (SKILLSHUB / skill / "SKILL.md").read_text()
    return yaml.safe_load(text.split("---", 2)[1])["metadata"]["magician"]


class SnapshotSafeEntrypointTests(unittest.TestCase):
    def setUp(self):
        self.temp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.temp)

    def run_snapshot(self, skill, path_dirs, argv=ARGV):
        adapter = SKILLSHUB / skill / "bin" / ADAPTERS[skill][0]
        snapshot = Path(tempfile.mkdtemp(dir=self.temp, prefix="governed-executable-"))
        shutil.copy2(adapter, snapshot)
        cwd = Path(tempfile.mkdtemp(dir=self.temp, prefix="cwd-"))
        path = os.pathsep.join([*map(str, path_dirs), "/usr/bin", "/bin"])
        return subprocess.run(
            [str(snapshot / adapter.name), *argv], cwd=cwd, capture_output=True, text=True,
            env={"PATH": path, "LANG": "en_US.UTF-8", "TERM": "dumb", "TZ": "UTC"}, timeout=20)

    def test_contracts_name_the_tracked_adapter_as_entrypoint(self):
        for skill, (adapter, companion) in ADAPTERS.items():
            with self.subTest(skill=skill):
                magician = frontmatter(skill)
                contract = magician["runtime_contract"]
                self.assertEqual(contract["requires"]["entrypoint"], adapter)
                # A path-shaped prefix would resolve against the task workdir.
                self.assertFalse([token for token in contract["runtime"]["command_prefix"]
                                  if "/" in token])
                for bins in (magician["requires"]["bins"], contract["requires"]["bins"]):
                    self.assertIn(adapter, bins)
                    self.assertIn(companion, bins)
                path = SKILLSHUB / skill / "bin" / adapter
                self.assertFalse(path.is_symlink(), "the adapter is source, not a link")
                self.assertTrue(os.access(path, os.X_OK))
                self.assertTrue(path.read_text().startswith("#!/bin/sh\n"))

    def test_node_cli_adapters_exec_their_companion_with_unchanged_argv(self):
        for skill, (_, companion) in ADAPTERS.items():
            if skill == "dugite":
                continue
            with self.subTest(skill=skill):
                bin_dir = self.temp / skill / "bin"
                executable(bin_dir / companion)
                result = self.run_snapshot(skill, [bin_dir])
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), ["", "", *ARGV])

    def test_missing_companion_fails_with_not_found(self):
        for skill, (_, companion) in ADAPTERS.items():
            if skill == "dugite":
                continue  # /usr/bin/git is a host git; covered below.
            with self.subTest(skill=skill):
                result = self.run_snapshot(skill, [self.temp / "empty"])
                self.assertEqual(result.returncode, 127)
                self.assertIn(f"pinned {companion} CLI is not installed", result.stderr)

    def dugite_distribution(self):
        dist = self.temp / "node_modules/dugite/git"
        executable(dist / "bin/git")
        executable(dist / "libexec/git-core/git")
        (dist / "share/git-core/templates").mkdir(parents=True)
        return Path(os.path.realpath(dist))

    def test_dugite_adapter_binds_exec_path_and_templates_from_the_real_binary(self):
        dist = self.dugite_distribution()
        skill_bin = self.temp / "dugite/bin"
        skill_bin.mkdir(parents=True)
        (skill_bin / "git").symlink_to(dist / "bin/git")
        # The skill's own bin/ link, or the package's libexec/git-core.
        for path_dir in (skill_bin, dist / "libexec/git-core"):
            with self.subTest(path_dir=path_dir):
                result = self.run_snapshot("dugite", [path_dir])
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), [
                    str(dist / "libexec/git-core"), str(dist / "share/git-core/templates"), *ARGV])

    def test_dugite_identity_falls_back_to_magician_and_yields_to_the_repository(self):
        git = shutil.which("git")
        if git is None:
            self.skipTest("no git to exercise identity resolution")
        dist = self.temp / "node_modules/dugite/git"
        (dist / "share/git-core/templates").mkdir(parents=True)
        # Real files, as dugite ships them: the adapter derives the
        # distribution root from where the binary really lives.
        for name in ("bin/git", "libexec/git-core/git"):
            (dist / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(os.path.realpath(git), dist / name)
        skill_bin = self.temp / "dugite/bin"
        skill_bin.mkdir(parents=True)
        (skill_bin / "git").symlink_to(dist / "bin/git")
        shutil.copy2(SKILLSHUB / "dugite/bin/dugite.gitconfig", skill_bin)
        repo = self.temp / "repo"
        subprocess.run([git, "init", "-q", str(repo)], check=True, env={"PATH": "/usr/bin:/bin"})
        ident = ["-C", str(repo), "var", "GIT_AUTHOR_IDENT"]
        # The skill's bin/, and the package's libexec/git-core with the config
        # found through the skill's tree.
        for path_dir in (skill_bin, dist / "libexec/git-core"):
            with self.subTest(path_dir=path_dir):
                result = self.run_snapshot("dugite", [path_dir], ident)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(result.stdout.startswith("Magician <magician@localhost> "), result.stdout)
        subprocess.run([git, "-C", str(repo), "config", "user.name", "Owner"], check=True)
        subprocess.run([git, "-C", str(repo), "config", "user.email", "owner@example.com"], check=True)
        result = self.run_snapshot("dugite", [skill_bin], ident)
        self.assertTrue(result.stdout.startswith("Owner <owner@example.com> "), result.stdout)

    def test_dugite_adapter_refuses_a_git_that_is_not_the_dugite_distribution(self):
        executable(self.temp / "other/bin/git")
        result = self.run_snapshot("dugite", [self.temp / "other/bin"])
        self.assertEqual(result.returncode, 127)
        self.assertIn("is not a dugite git distribution", result.stderr)
        if Path("/usr/bin/git").exists():
            result = self.run_snapshot("dugite", [self.temp / "empty"])
            self.assertEqual(result.returncode, 127)
            self.assertIn("refusing host git", result.stderr)


if __name__ == "__main__":
    unittest.main()
