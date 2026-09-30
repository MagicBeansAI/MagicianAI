import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest


SCRIPT_PATH = pathlib.Path(__file__).parents[1] / "bin" / "veo31"
LOADER = SourceFileLoader("veo31_router", str(SCRIPT_PATH))
SPEC = importlib.util.spec_from_loader("veo31_router", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class Veo31RoutingTest(unittest.TestCase):
    def test_manual_model_override_wins(self):
        model, tier = MODULE.resolve_model(
            model_override="veo-3.1-fast-generate-preview",
            quality_tier="balanced",
        )

        self.assertEqual(model, "veo-3.1-fast-generate-preview")
        self.assertEqual(tier, "manual")

    def test_auto_defaults_to_balanced(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="auto",
        )

        self.assertEqual(model, "veo-3.1-generate-preview")
        self.assertEqual(tier, "balanced")

    def test_explicit_fast_selects_fast_model(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="fast",
        )

        self.assertEqual(model, "veo-3.1-fast-generate-preview")
        self.assertEqual(tier, "fast")

    def test_invalid_tier_raises(self):
        with self.assertRaises(ValueError):
            MODULE.resolve_model(
                model_override="",
                quality_tier="pro",
            )


if __name__ == "__main__":
    unittest.main()
