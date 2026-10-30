import copy
import unittest
from scripts.check_clippy_baseline import ROOT, counts, finding, new_findings


class ClippyBaselineTests(unittest.TestCase):
    def message(self):
        return {"code": {"code": "clippy::too_many_arguments"}, "message": "too many arguments",
                "spans": [{"is_primary": True, "file_name": "crate/src/lib.rs", "line_start": 10,
                           "text": [{"text": "fn operation(a: u8, b: u8) {}"}]}]}

    def entry(self):
        return dict(finding(self.message()), count=1)

    def test_line_movement_does_not_reset_identity(self):
        moved = self.message()
        moved["spans"][0]["line_start"] = 50
        self.assertEqual(finding(self.message()), finding(moved))

    def test_changed_source_requires_review(self):
        changed = self.message()
        changed["spans"][0]["text"][0]["text"] = "fn another_operation(a: u8, b: u8) {}"
        self.assertNotEqual(finding(self.message()), finding(changed))

    def test_macro_warning_uses_workspace_call_site_across_toolchains(self):
        for toolchain in ["/toolchains/macos", "/runner/rustup/toolchains/linux"]:
            with self.subTest(toolchain=toolchain):
                message = self.message()
                call_site = copy.deepcopy(message["spans"][0])
                message["spans"][0] = {
                    "is_primary": True, "file_name": toolchain + "/core/macros.rs",
                    "text": [{"text": "internal macro implementation"}],
                    "expansion": {"span": {
                        "file_name": "<nested macro>",
                        "expansion": {"span": call_site},
                    }},
                }
                self.assertEqual(finding(self.message()), finding(message))

    def test_workspace_path_spellings_share_identity(self):
        for path in [str(ROOT / "crate/src/lib.rs"), "crate/tests/../src/lib.rs", r"crate\src\lib.rs"]:
            with self.subTest(path=path):
                message = self.message()
                message["spans"][0]["file_name"] = path
                self.assertEqual(finding(self.message()), finding(message))

    def test_new_warning_and_extra_occurrence_are_blocking(self):
        baseline = counts([self.entry()])
        self.assertFalse(new_findings(baseline, baseline))
        extra = copy.deepcopy(self.entry())
        extra["count"] = 2
        self.assertEqual(sum(new_findings(counts([extra]), baseline).values()), 1)
        extra["file"] = "other/src/lib.rs"
        self.assertEqual(sum(new_findings(counts([extra]), baseline).values()), 2)

    def test_removed_warning_is_allowed(self):
        self.assertFalse(new_findings(counts([]), counts([self.entry()])))

    def test_invalid_or_duplicate_baseline_is_rejected(self):
        with self.assertRaises(ValueError):
            counts([self.entry(), self.entry()])
        bad = self.entry()
        bad["count"] = 0
        with self.assertRaises(ValueError):
            counts([bad])

    def test_unidentified_diagnostics_fail_closed(self):
        message = self.message()
        message["spans"] = []
        with self.assertRaises(ValueError):
            finding(message)
        message = self.message()
        message["spans"][0]["file_name"] = "/toolchain/core/macros.rs"
        with self.assertRaises(ValueError):
            finding(message)


if __name__ == "__main__":
    unittest.main()
