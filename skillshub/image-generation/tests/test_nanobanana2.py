import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest


SCRIPT_PATH = pathlib.Path(__file__).parents[1] / "bin" / "nanobanana2"
LOADER = SourceFileLoader("nanobanana2_router", str(SCRIPT_PATH))
SPEC = importlib.util.spec_from_loader("nanobanana2_router", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class NanoBananaRoutingTest(unittest.TestCase):
    def test_manual_model_override_wins(self):
        model, tier = MODULE.resolve_model(
            model_override="gemini-3-pro-image",
            quality_tier="fast",
            prompt="quick draft of a reading nook",
            input_image_paths=[],
            thinking="",
            use_search=False,
        )

        self.assertEqual(model, "gemini-3-pro-image")
        self.assertEqual(tier, "manual")

    def test_auto_defaults_to_balanced(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="auto",
            prompt="quick draft concept sketch of a cafe mascot",
            input_image_paths=[],
            thinking="",
            use_search=False,
        )

        self.assertEqual(model, "gemini-3.1-flash-image")
        self.assertEqual(tier, "balanced")

    def test_explicit_pro_selects_pro_model(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="pro",
            prompt="Design a fintech landing page hero image with crisp typography, logo lockup, and editorial poster styling",
            input_image_paths=[],
            thinking="",
            use_search=False,
        )

        self.assertEqual(model, "gemini-3-pro-image")
        self.assertEqual(tier, "pro")

    def test_explicit_fast_selects_fast_model(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="fast",
            prompt="A photorealistic portrait of a ceramic mug on a marble countertop",
            input_image_paths=[],
            thinking="",
            use_search=False,
        )

        self.assertEqual(model, "gemini-3.1-flash-lite-image")
        self.assertEqual(tier, "fast")

    def test_fast_tier_rejects_non_1k_resolution(self):
        with self.assertRaisesRegex(ValueError, "supports resolution: 1K"):
            MODULE.validate_model_options(
                model="gemini-3.1-flash-lite-image",
                resolution="2K",
                aspect_ratio="1:1",
                use_search=False,
            )

    def test_fast_tier_rejects_search_grounding(self):
        with self.assertRaisesRegex(ValueError, "does not support Google Search grounding"):
            MODULE.validate_model_options(
                model="gemini-3.1-flash-lite-image",
                resolution="1K",
                aspect_ratio="1:1",
                use_search=True,
            )

    def test_balanced_tier_accepts_extended_output_options(self):
        MODULE.validate_model_options(
            model="gemini-3.1-flash-image",
            resolution="4K",
            aspect_ratio="1:8",
            use_search=True,
        )

    def test_manual_unknown_model_defers_validation_to_provider(self):
        MODULE.validate_model_options(
            model="future-image-model",
            resolution="8K",
            aspect_ratio="custom",
            use_search=True,
        )


if __name__ == "__main__":
    unittest.main()
