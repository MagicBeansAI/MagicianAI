from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


BIN = Path(__file__).resolve().parents[1] / "bin" / "minimax-music"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"

# A value that could only reach the CLI by being passed there deliberately.
SENTINEL_KEY = "sentinel-minimax-key-2f1ec940"
GOVERNED_ENVELOPE = {"prompt": "a jingle", "output_path": "/tmp/minimax_test.mp3", "instrumental": True}


def load_adapter():
    loader = importlib.machinery.SourceFileLoader(
        "minimax_music_adapter", str(BIN)
    )
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def invoke(module, request, env, cli_stdout="{\"saved\": \"/tmp/minimax_test.mp3\"}"):
    """Run the adapter's main() against a stubbed `mmx` and return its argv.

    The adapter builds argv and calls subprocess.run before it parses
    anything, so argv is captured whether or not the (irrelevant here)
    response-parsing branch then exits.
    """
    captured = {}

    def fake_run(argv, **_kwargs):
        captured["argv"] = list(argv)
        return SimpleNamespace(returncode=0, stdout=cli_stdout, stderr="")

    stdin = io.StringIO(json.dumps(request))
    with mock.patch.dict(os.environ, env, clear=True):
        with mock.patch.object(module.shutil, "which", return_value="/usr/bin/mmx"):
            with mock.patch.object(module.subprocess, "run", fake_run):
                with mock.patch.object(sys, "stdin", stdin):
                    with contextlib.redirect_stdout(io.StringIO()) as out:
                        with contextlib.redirect_stderr(io.StringIO()):
                            with contextlib.suppress(SystemExit):
                                module.main()
    return captured.get("argv"), out.getvalue()


class MinimaxMusicCredentialContractTests(unittest.TestCase):
    """Pin that auth is now config-directory-based, not passed on argv."""

    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def test_credential_is_handed_to_the_cli_not_merely_checked(self):
        argv, _ = invoke(
            self.adapter, GOVERNED_ENVELOPE, {"MMX_CONFIG_DIR": SENTINEL_KEY}
        )
        self.assertIsNotNone(argv, "the adapter never spawned the CLI")
        self.assertNotIn(
            "--api-key",
            argv,
            "credentials are now supplied through MMX_CONFIG_DIR/config.json",
        )

    def test_non_interactive_is_pinned_so_env_only_auth_can_never_be_relied_on(self):
        # These two flags are a pair. --non-interactive is exactly what makes
        # an env-only key resolve to nothing, so it may only ever be sent
        # alongside config-directory auth.
        argv, _ = invoke(
            self.adapter, GOVERNED_ENVELOPE, {"MMX_CONFIG_DIR": SENTINEL_KEY}
        )
        self.assertIn("--non-interactive", argv)
        self.assertNotIn("--api-key", argv)

    def test_credential_is_not_leaked_into_the_adapter_result(self):
        _, stdout = invoke(
            self.adapter, GOVERNED_ENVELOPE, {"MMX_CONFIG_DIR": SENTINEL_KEY}
        )
        self.assertNotIn(SENTINEL_KEY, stdout)

    def test_missing_credential_fails_closed(self):
        # The weak assertion that used to be the only one. Kept because the
        # behaviour still matters -- but on its own it proved nothing.
        argv, stdout = invoke(self.adapter, GOVERNED_ENVELOPE, {})
        self.assertIsNone(argv, "the CLI must not be spawned without a credential")
        self.assertIn("MMX_CONFIG_DIR", stdout)

    def test_manifest_declares_the_secret_binding_the_adapter_consumes(self):
        frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        self.assertIn("secret_ref: MINIMAX_API_KEY", frontmatter)
        self.assertIn("name: MINIMAX_API_KEY", frontmatter)
        self.assertIn("kind: config_directory", frontmatter)
        self.assertIn("name: MMX_CONFIG_DIR", frontmatter)

    def test_documentation_describes_the_real_credential_path(self):
        # A manifest that misdescribes its own credential path is what let
        # this survive: two adapters carried a comment asserting the key was
        # "injected only in the child environment", which was never true.
        skill_text = SKILL.read_text(encoding="utf-8")
        source = BIN.read_text(encoding="utf-8")
        self.assertIn("MMX_CONFIG_DIR", skill_text)
        self.assertIn("config.json", skill_text)
        self.assertNotIn("--api-key", source)
        self.assertNotIn("--api-key", skill_text)


class MinimaxMusicOutputContractTests(unittest.TestCase):
    """Pin that a paid, completed generation is reported as a success.

    The defect this guards: the adapter sent `--output json` and `--quiet`
    together. On this subcommand they are not composable — `music generate
    --out` ends in a shared writer whose last argument is the quiet flag:
    `if (quiet) print(path); else print(json(...))`. The quiet branch won and
    printed a bare filesystem path, discarding the requested format.
    `json.loads` then threw and the adapter reported `{"error": "mmx returned
    non-JSON stdout.", "stdout": "/private/tmp/mmxprobe.mp3"}` on EVERY
    successful run — while that mp3 sat on disk and MiniMax had already
    charged for it.

    The cost of the bug was carried entirely by the caller: money spent, asset
    written, result thrown away. So these assert both halves — the flag is
    gone, and the bare-path shape would survive it coming back.
    """

    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def written_audio(self, size: int) -> str:
        with tempfile.NamedTemporaryFile(suffix=".mp3", delete=False) as handle:
            handle.write(b"\x00" * size)
            path = handle.name
        self.addCleanup(os.unlink, path)
        return path

    def test_quiet_is_never_sent_alongside_a_requested_output_format(self):
        argv, _ = invoke(
            self.adapter, GOVERNED_ENVELOPE, {"MMX_CONFIG_DIR": SENTINEL_KEY}
        )
        self.assertIn("--output", argv)
        self.assertEqual(argv[argv.index("--output") + 1], "json")
        self.assertNotIn(
            "--quiet",
            argv,
            "--quiet overrides --output json on this subcommand and prints a "
            "bare path, which is the billed-then-reported-as-failed regression",
        )

    def test_the_json_envelope_is_parsed_and_its_saved_path_is_honoured(self):
        written = self.written_audio(13)
        envelope = json.dumps(
            {
                "saved": written,
                "duration_ms": 42000,
                "size_bytes": 13,
                "sample_rate": 44100,
            }
        )

        _, stdout = invoke(
            self.adapter,
            {"prompt": "a jingle", "output_path": written, "instrumental": True},
            {"MMX_CONFIG_DIR": SENTINEL_KEY},
            cli_stdout=envelope,
        )
        result = json.loads(stdout)

        self.assertNotIn("error", result)
        self.assertEqual(result["path"], written)
        self.assertEqual(result["size_bytes"], 13)
        self.assertEqual(result["mode"], "instrumental")

    def test_a_bare_path_is_still_understood_as_a_completed_generation(self):
        # Defence in depth against the CLI reintroducing the quiet branch: a
        # generation that was paid for and written must never be reported as
        # a failure over its stdout formatting.
        written = self.written_audio(5)

        _, stdout = invoke(
            self.adapter,
            {"prompt": "a jingle", "output_path": written, "instrumental": True},
            {"MMX_CONFIG_DIR": SENTINEL_KEY},
            cli_stdout=f"{written}\n",
        )
        result = json.loads(stdout)

        self.assertNotIn("error", result)
        self.assertEqual(result["path"], written)
        self.assertEqual(result["size_bytes"], 5)

    def test_a_saved_list_is_still_normalized(self):
        # `music generate` has been observed reporting `saved` as a list. The
        # normalization predates this change and must survive it.
        written = self.written_audio(3)

        _, stdout = invoke(
            self.adapter,
            {"prompt": "a jingle", "output_path": written, "instrumental": True},
            {"MMX_CONFIG_DIR": SENTINEL_KEY},
            cli_stdout=json.dumps({"saved": [written]}),
        )
        result = json.loads(stdout)

        self.assertEqual(result["path"], written)

    def test_genuinely_unparseable_stdout_still_fails(self):
        # The tolerance must not become a shrug. Text that names no written
        # file is a real failure and has to stay one.
        _, stdout = invoke(
            self.adapter,
            GOVERNED_ENVELOPE,
            {"MMX_CONFIG_DIR": SENTINEL_KEY},
            cli_stdout="[Model: music-2.6]\nsomething went sideways\n",
        )
        result = json.loads(stdout)

        self.assertIn("error", result)

    def test_a_path_that_was_never_written_is_not_treated_as_success(self):
        _, stdout = invoke(
            self.adapter,
            GOVERNED_ENVELOPE,
            {"MMX_CONFIG_DIR": SENTINEL_KEY},
            cli_stdout="/tmp/minimax-generation-that-does-not-exist.mp3\n",
        )
        result = json.loads(stdout)

        self.assertIn("error", result)

    def test_the_adapter_documents_why_the_two_flags_are_incompatible(self):
        source = BIN.read_text(encoding="utf-8")
        self.assertIn("--quiet", source, "the reasoning must survive in the file")
        self.assertIn("--output json", source)



class GovernedPathContractTests(unittest.TestCase):
    """Pin the interpreter that `mmx` runs under.

    The defect this guards: the governed child receives a cleared environment
    whose PATH is assembled only from the directories that resolve the
    manifest's declared `bins`. `mmx` is a `#!/usr/bin/env node` script, so
    without `node` on that PATH the kernel's interpreter lookup failed and
    every call died before reaching the provider:

        env: node: No such file or directory   (exit 127)

    Nothing in that message names this package, and no request was ever sent,
    so the failure was indistinguishable from a provider outage. The runtime
    vendors its own Node at `skillshub/.node/bin`, which is one of the
    dependency roots the governed PATH resolves; naming it here binds that
    interpreter rather than whatever the operator's shell happens to expose.
    """

    def declared_bin_blocks(self) -> list[list[str]]:
        """Every `bins:` list in the frontmatter, read by indentation so the
        tests carry no dependency the governed adapter does not need."""
        frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        blocks: list[list[str]] = []
        lines = frontmatter.splitlines()
        for index, line in enumerate(lines):
            if line.strip() != "bins:":
                continue
            indent = len(line) - len(line.lstrip())
            entries: list[str] = []
            for candidate in lines[index + 1 :]:
                stripped = candidate.strip()
                if not stripped or stripped.startswith("#"):
                    continue
                if (
                    len(candidate) - len(candidate.lstrip()) < indent
                    or not stripped.startswith("- ")
                ):
                    break
                entries.append(stripped[2:].strip())
            blocks.append(entries)
        return blocks

    def test_the_node_interpreter_is_declared_beside_the_cli_it_launches(self):
        blocks = self.declared_bin_blocks()
        self.assertTrue(blocks, "the manifest declares no bins at all")
        for entries in blocks:
            self.assertIn("mmx", entries)
            self.assertIn(
                "node",
                entries,
                "a bins list names `mmx` without the interpreter its shebang "
                "requires, so the governed PATH cannot launch it",
            )

    def test_the_entry_point_stays_named_now_that_several_binaries_are(self):
        # `validate_requirements` in tool-runtime-core refuses a CLI contract
        # with several `bins` and no exact `entrypoint`: the contract fails
        # validation, the loader drops the pack, and the tool answers
        # `unknown inner-loop pack`. Adding `node` is only safe while this
        # line exists.
        frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        entrypoints = [
            line.split(":", 1)[1].strip()
            for line in frontmatter.splitlines()
            if line.strip().startswith("entrypoint:")
        ]
        self.assertTrue(entrypoints, "a multi-binary contract declares no entrypoint")
        for entrypoint in entrypoints:
            self.assertEqual(entrypoint, "minimax-music")
            self.assertNotEqual(
                entrypoint,
                "mmx",
                "the entry point is this package's own adapter; `mmx` and `node` "
                "are PATH companions the adapter shells out to",
            )


if __name__ == "__main__":
    unittest.main()
