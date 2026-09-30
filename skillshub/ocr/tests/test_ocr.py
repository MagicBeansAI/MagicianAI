from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path


PACKAGE = Path(__file__).resolve().parents[1]
BIN = PACKAGE / "bin" / "ocr"
SKILL = PACKAGE / "SKILL.md"
FIXTURE = PACKAGE / "canary-fixtures" / "canary-scan.png"


def frontmatter() -> str:
    return SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]


def declared_bin_blocks() -> list[list[str]]:
    """Every `bins:` list in the manifest frontmatter, as declared.

    Read by indentation rather than through a YAML parser so the tests carry
    no dependency the governed adapter does not already need.
    """
    blocks: list[list[str]] = []
    lines = frontmatter().splitlines()
    for index, line in enumerate(lines):
        if line.strip() != "bins:":
            continue
        indent = len(line) - len(line.lstrip())
        entries: list[str] = []
        for candidate in lines[index + 1 :]:
            stripped = candidate.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if len(candidate) - len(candidate.lstrip()) < indent or not stripped.startswith("- "):
                break
            entries.append(stripped[2:].strip())
        blocks.append(entries)
    return blocks


class GovernedPathContractTests(unittest.TestCase):
    """Pin that the default engine's binaries are reachable at run time.

    The defect this guards: the governed child receives a cleared environment
    whose PATH is assembled only from the directories that resolve the
    manifest's declared `bins`. This package declared just its own entry
    point while spawning `tesseract` and `pdftoppm` by bare name, so every
    call on the default engine died with
    `[Errno 2] No such file or directory: 'tesseract'` — a package whose
    documented default mode could not run at all under the runtime that
    actually invokes it.
    """

    def test_the_default_engine_binaries_are_declared(self):
        blocks = declared_bin_blocks()
        self.assertTrue(blocks, "the manifest declares no bins at all")
        for entries in blocks:
            self.assertIn("ocr", entries)
            for companion in ("tesseract", "pdftoppm"):
                self.assertIn(
                    companion,
                    entries,
                    f"a bins list omits `{companion}`, which the adapter spawns "
                    "by bare name; without it the governed PATH cannot resolve it",
                )

    def test_the_optional_vlm_engines_stay_undeclared(self):
        # agy and codex are operator-installed CLIs the runtime neither ships
        # nor vendors, and the adapter already reports their absence in its own
        # words. `tesseract` and `pdftoppm` are declared because the runtime
        # can resolve them and the governed PATH is the only way the child
        # reaches them; a host without them keeps the skill, because a
        # companion is never bound as execution authority (see
        # `governed_executable_directories`).
        for entries in declared_bin_blocks():
            self.assertNotIn("agy", entries)
            self.assertNotIn("codex", entries)

    def test_the_engine_companions_do_not_displace_the_declared_entry_point(self):
        """Naming a second binary obliges the contract to name the first.

        `validate_requirements` in tool-runtime-core refuses a CLI contract
        with more than one `bins` entry and no exact `entrypoint`. The refusal
        drops the whole pack at load, so `unknown inner-loop pack` was the
        answer for EVERY engine — including the two VLM engines that never
        needed tesseract — until this line existed. That is the failure the
        companion declaration was supposed to prevent, produced by the
        companion declaration itself.
        """
        entrypoints = [
            line.split(":", 1)[1].strip()
            for line in frontmatter().splitlines()
            if line.strip().startswith("entrypoint:")
        ]
        self.assertTrue(
            entrypoints,
            "a multi-binary contract declares no entrypoint, so the pack cannot load",
        )
        for entrypoint in entrypoints:
            self.assertEqual(entrypoint, "ocr")


class CanaryDeclarationTests(unittest.TestCase):
    def test_the_packaged_fixture_the_canary_names_is_shipped(self):
        self.assertTrue(
            FIXTURE.is_file(),
            "the canary reads a PNG that cannot be authored as a manifest "
            "fixture string, so the package must ship it",
        )
        self.assertGreater(FIXTURE.stat().st_size, 0)

    def test_the_canary_pins_the_free_deterministic_engine(self):
        # tesseract is the only engine that is free, offline and
        # deterministic, so the probe reports on this package rather than on a
        # VLM provider's mood or quota.
        text = frontmatter()
        self.assertIn('- "--engine"', text)
        self.assertIn('- "tesseract"', text)

    def test_the_canary_call_writes_nothing(self):
        # `--output-file` would make the probe create a file in the operator's
        # scope on every run. The canary's argv must stay read-only.
        canary = frontmatter().split("runtime_canary:", 1)[1]
        canary = canary.split("runtime_contract:", 1)[0]
        self.assertNotIn('- "--output-file"', canary)

    def test_the_canary_reads_the_adapters_own_error_envelope(self):
        # The adapter answers a missing binary or an unreadable input with
        # `{"error": ...}` on stderr and a non-zero exit, which is exactly
        # when the governed runtime parses stderr. Without this pointer the
        # lane could only report which assertion went missing.
        self.assertIn('error_pointer: "/error"', frontmatter())

    def test_the_working_directory_divergence_is_recorded_rather_than_papered_over(self):
        # `profile_selection.mode: implicit` routes this skill through the lane
        # whose working directory is the calling process's cwd, not the scope
        # root that a static-secret CLI gets. No relative spelling names both
        # bases, so the manifest must keep saying which one it means, and must
        # name where the pairing is actually resolved — the canary runner reads
        # the same three auth fields and stages this package's fixture under the
        # base the skill is really launched in. Neither half may be quietly
        # dropped, and the canary must not have been converted into an exemption
        # to make the lane green.
        text = frontmatter()
        self.assertIn("implicit-CLI lane", text)
        self.assertIn("governed_working_directory", text)
        self.assertIn("stage_packaged_fixtures", text)
        self.assertNotIn("exempt:", text)


class InputResolutionTests(unittest.TestCase):
    """The failure the canary actually reported, reproduced without OCR."""

    def run_extract(self, *arguments: str) -> tuple[int, dict]:
        completed = subprocess.run(
            [sys.executable, str(BIN), "extract", *arguments],
            capture_output=True,
            text=True,
        )
        # The envelope is on stderr, paired with a non-zero exit. That is the
        # correct pairing rather than an oversight: the governed runtime
        # parses stdout only when the process exits zero and parses stderr
        # otherwise, so a failure envelope belongs on stderr.
        stream = completed.stdout if completed.returncode == 0 else completed.stderr
        return completed.returncode, json.loads(stream or "{}")

    def test_a_missing_input_is_an_error_envelope_naming_the_resolved_path(self):
        status, result = self.run_extract(
            "--input-file",
            "skills/ocr/canary-fixtures/canary-scan.png",
            "--engine",
            "tesseract",
        )

        self.assertNotEqual(status, 0)
        self.assertIn("error", result)
        self.assertIn("input file does not exist", result["error"])
        # Relative inputs resolve against the process working directory. That
        # is the whole mechanism behind the doubled path segment the canary
        # reported, and it is why no relative spelling in the manifest can
        # name a fixture installed under the scope root.
        self.assertTrue(result["error"].rstrip().endswith("canary-scan.png"))
        self.assertIn(str(Path.cwd()), result["error"])

    def test_the_packaged_fixture_resolves_when_named_from_its_own_directory(self):
        _, result = self.run_extract(
            "--input-file", str(FIXTURE), "--engine", "tesseract"
        )

        # Either tesseract ran, or it is absent from this host's PATH. Both
        # are answers about the environment; neither may be "file not found".
        self.assertNotIn("input file does not exist", json.dumps(result))


if __name__ == "__main__":
    unittest.main()
