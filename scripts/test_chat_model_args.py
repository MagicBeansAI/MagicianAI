import argparse
from contextlib import redirect_stderr
import io
import importlib.util
from pathlib import Path
import sys
import unittest

from chat_model_args import add_chat_model_arguments, chat_model_request_fields, resolve_chat_model_selections


class ChatModelArgumentsTest(unittest.TestCase):
    stages = ("ingest", "answer", "judge")
    catalog = {"models": [
        {"id": "custom/optional", "choices": {"auto": "optional/auto", "none": "optional/none", "high": "optional/high"}},
        {"id": "custom/mandatory", "choices": {"auto": "mandatory/auto", "low": "mandatory/low"}},
    ]}

    def parser(self):
        parser = argparse.ArgumentParser()
        add_chat_model_arguments(parser, self.stages)
        return parser

    def test_defaults_follow_server_without_fetch_or_override(self):
        args = self.parser().parse_args([])
        def unexpected_fetch():
            self.fail("server-default requests need no explicit model catalog")
        resolve_chat_model_selections(args, self.stages, unexpected_fetch)
        for stage in self.stages:
            self.assertEqual(chat_model_request_fields(getattr(args, f"{stage}_model_selection")), {})

    def test_off_fixed_and_auto_send_typed_choices_for_each_stage(self):
        args = self.parser().parse_args([
            "--ingest-model", "custom/optional", "--ingest-thinking", "none",
            "--answer-model", "custom/optional", "--answer-thinking", "high",
            "--judge-model", "custom/mandatory",
        ])
        fetches = []
        resolve_chat_model_selections(args, self.stages, lambda: fetches.append(1) or self.catalog)
        self.assertEqual(fetches, [1])
        for stage, thinking in zip(self.stages, ("none", "high", "auto")):
            body = chat_model_request_fields(getattr(args, f"{stage}_model_selection"))
            self.assertEqual(body["harness_engine"], "magician")
            self.assertEqual(body["model_selection"]["thinking"], thinking)
            self.assertNotIn("profile", body)

    def test_unavailable_model_or_thinking_refuses_before_send(self):
        for model, thinking in [("retired-alias", "auto"), ("custom/mandatory", "none"), ("", "high")]:
            with self.subTest(model=model, thinking=thinking):
                args = self.parser().parse_args(["--answer-model", model, "--answer-thinking", thinking])
                with self.assertRaises(ValueError):
                    resolve_chat_model_selections(args, self.stages, lambda: self.catalog)

    def test_retired_profile_options_are_not_compatibility_aliases(self):
        for stage in self.stages:
            with self.subTest(stage=stage), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                self.parser().parse_args([f"--{stage}-profile", "balanced"])

    def test_benchmark_drivers_send_the_new_wire_contract(self):
        class Client:
            body = None
            def json(self, method, path, body=None, **kwargs):
                self.body = body
                return {"assistant_message": {"content": {"type": "text", "text": "done"}}}
        for script in ("eval-longmemeval-live.py", "eval-tau2-live.py"):
            with self.subTest(script=script):
                name = script.replace("-", "_").replace(".py", "")
                spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(script))
                module = importlib.util.module_from_spec(spec)
                sys.modules[name] = module
                spec.loader.exec_module(module)
                client = Client()
                selection = {"model_id": "custom/optional", "thinking": "none"}
                module.send_message(client, "session", "Hello", model_selection=selection)
                self.assertEqual(client.body["model_selection"], selection)
                self.assertEqual(client.body["harness_engine"], "magician")
                self.assertNotIn("profile", client.body)
                with self.assertRaises(TypeError):
                    module.send_message(client, "session", "Hello", profile="balanced")


if __name__ == "__main__":
    unittest.main()
