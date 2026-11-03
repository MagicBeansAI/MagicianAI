"""The office-powerpoint PDF exporter plugin, run as OfficeCLI runs it.

OfficeCLI finds `officecli-exporter-pdf` on PATH and calls
`export <source> --out <target>`. Stub `officecli` and Chrome stand in for the
real programs, so the print rules and the error contract are checked without
rendering anything.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[2] / "office-powerpoint/bin/officecli-exporter-pdf"

OFFICECLI = """#!/bin/sh
# view <file> html -o <out>
cat > "$5" <<'HTML'
<html><head><style>:root{--slide-design-w:720pt;--slide-design-h:405pt;--slide-aspect:1.7778;}</style></head>
<body><div class="main"><div class="slide-container"><div class="slide">One</div></div></div></body></html>
HTML
"""

CHROME = """#!/bin/sh
for arg; do
  case $arg in
    --print-to-pdf=*) out=${arg#--print-to-pdf=} ;;
    file://*) page=${arg#file://} ;;
  esac
done
cp "$page" "$CAPTURE"
printf '%%PDF-1.7 stub\\n' > "$out"
"""


def executable(path, body):
    path.write_text(body)
    path.chmod(0o755)


class OfficeCliPdfExporterTests(unittest.TestCase):
    def setUp(self):
        self.temp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.temp)
        self.bin = self.temp / "bin"
        self.bin.mkdir()
        executable(self.bin / "officecli", OFFICECLI)
        executable(self.bin / "chrome", CHROME)
        self.source = self.temp / "deck.pptx"
        self.source.write_bytes(b"PK\x03\x04 deck")
        self.capture = self.temp / "printed.html"

    def run_plugin(self, *argv, chrome=None):
        env = {"PATH": f"{self.bin}:/usr/bin:/bin", "LANG": "en_US.UTF-8", "TZ": "UTC",
               "CAPTURE": str(self.capture),
               "OFFICECLI_PDF_CHROME": str(chrome or self.bin / "chrome")}
        return subprocess.run([str(PLUGIN), *argv], cwd=self.temp, env=env,
                              capture_output=True, text=True, timeout=30)

    def test_info_is_a_protocol_v1_exporter_manifest(self):
        result = self.run_plugin("--info")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(result.stdout)
        self.assertEqual(manifest["protocol"], 1)
        self.assertEqual(manifest["kinds"], ["exporter"])
        self.assertEqual(manifest["extensions"], [".pdf"])
        self.assertGreater(manifest["idle_timeout_seconds"]["default"], 0)

    def test_export_prints_one_page_per_slide_at_the_slide_size(self):
        result = self.run_plugin("export", "deck.pptx", "--out", "deck.pdf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.temp / "deck.pdf").read_bytes().startswith(b"%PDF-"))
        page = self.capture.read_text()
        self.assertIn("@page{size:720pt 405pt;margin:0}", page)
        self.assertIn(".slide-container{break-after:page}", page)
        self.assertIn(".sidebar,.file-title,.slide-label,.slide-notes", page)
        self.assertEqual(self.source.read_bytes(), b"PK\x03\x04 deck")

    def test_a_missing_browser_is_unsupported_and_writes_nothing(self):
        result = self.run_plugin("export", "deck.pptx", "--out", "deck.pdf",
                                 chrome=self.temp / "no-chrome")
        self.assertEqual(result.returncode, 3)
        self.assertIn("needs Google Chrome", result.stderr)
        self.assertFalse((self.temp / "deck.pdf").exists())

    def test_only_pptx_sources_and_existing_files_are_accepted(self):
        (self.temp / "doc.docx").write_text("x")
        self.assertEqual(self.run_plugin("export", "doc.docx", "--out", "a.pdf").returncode, 3)
        self.assertEqual(self.run_plugin("export", "gone.pptx", "--out", "a.pdf").returncode, 2)
        self.assertEqual(self.run_plugin("export", "deck.pptx").returncode, 64)


if __name__ == "__main__":
    unittest.main()
