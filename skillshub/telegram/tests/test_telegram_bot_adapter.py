import importlib.machinery
import importlib.util
import io
import json
import os
import pathlib
import unittest
from unittest import mock


ADAPTER = pathlib.Path(__file__).parents[1] / "bin" / "telegram-bot-adapter"
LOADER = importlib.machinery.SourceFileLoader("telegram_bot_adapter", str(ADAPTER))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class TelegramBotAdapterTests(unittest.TestCase):
    def test_method_rejects_url_and_shell_syntax(self):
        for method in ["../getMe", "sendMessage?x=y", "sendMessage;echo", ""]:
            self.assertIsNone(MODULE.METHOD_RE.fullmatch(method))

    def test_json_validation_is_iterative_and_bounded(self):
        value = {}
        for _ in range(MODULE.MAX_JSON_DEPTH + 1):
            value = {"next": value}
        with self.assertRaises(SystemExit):
            MODULE.validate_tree(value)

    def test_redirects_are_rejected(self):
        handler = MODULE.NoRedirect()
        request = mock.Mock(full_url="https://api.telegram.org/example")
        with self.assertRaises(Exception):
            handler.redirect_request(request, None, 302, "redirect", {}, "https://evil.test")

    def test_request_body_never_contains_the_token(self):
        payload = {"method": "sendMessage", "data": json.dumps({"chat_id": 1, "text": "hi"})}
        stdin = mock.Mock(buffer=io.BytesIO(json.dumps(payload).encode()))
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = b'{"ok":true,"result":{}}'
        response.__enter__.return_value.__exit__.return_value = False
        opener = mock.Mock()
        opener.open.return_value = response
        # argv is pinned to what the runtime actually passes on the `run` route:
        # nothing. `main` reads argv[1] to select the send route and refuses any
        # other token, so an unpinned argv lets the test runner's own arguments
        # decide which route this exercises.
        with mock.patch.object(MODULE.sys, "argv", ["telegram-bot-adapter"]), mock.patch.object(
            MODULE.sys, "stdin", stdin
        ), mock.patch.object(
            MODULE.urllib.request, "build_opener", return_value=opener
        ), mock.patch.dict(os.environ, {"TELEGRAM_TOKEN": "123456:secret-token"}, clear=True), mock.patch.object(
            MODULE.sys, "stdout", mock.Mock(buffer=io.BytesIO())
        ):
            MODULE.main()
        request = opener.open.call_args.args[0]
        self.assertNotIn(b"123456:secret-token", request.data)
        self.assertIn("123456:secret-token", request.full_url)


SENT_OK = b'{"ok":true,"result":{"message_id":4242,"chat":{"id":123456789},"text":"hi"}}'


class TelegramSendRouteTests(unittest.TestCase):
    """The explicit send route — the door the outward gate can actually see.

    Before it, `run` was the only action: the method lived in a free-text field
    and the recipient inside an opaque `data` blob, so nothing keyed on the
    action could tell a send from a read or say who a send reached. Every
    assertion here is about a property that made that true.
    """

    def drive(self, argv, payload, response_body=SENT_OK):
        stdin = mock.Mock(buffer=io.BytesIO(json.dumps(payload).encode()))
        captured = io.BytesIO()
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = response_body
        response.__enter__.return_value.__exit__.return_value = False
        opener = mock.Mock()
        opener.open.return_value = response
        with mock.patch.object(MODULE.sys, "argv", argv), mock.patch.object(
            MODULE.sys, "stdin", stdin
        ), mock.patch.object(
            MODULE.urllib.request, "build_opener", return_value=opener
        ), mock.patch.dict(
            os.environ, {"TELEGRAM_TOKEN": "123456:secret-token"}, clear=True
        ), mock.patch.object(
            MODULE.sys, "stdout", mock.Mock(buffer=captured)
        ):
            MODULE.main()
        return captured.getvalue(), opener

    def test_the_send_route_fixes_the_method_and_types_the_recipient(self):
        """The caller cannot choose the method, and the chat id reaches the wire
        as the integer every live `run` send has always used."""
        _, opener = self.drive(
            ["telegram-bot-adapter", "send"],
            {"chat_id": "123456789", "text": "hi"},
        )
        request = opener.open.call_args.args[0]
        self.assertTrue(request.full_url.endswith("/sendMessage"), request.full_url)
        self.assertEqual(json.loads(request.data), {"chat_id": 123456789, "text": "hi"})

    def test_an_at_username_reaches_the_wire_unchanged(self):
        _, opener = self.drive(
            ["telegram-bot-adapter", "send"],
            {"chat_id": " @somechannel ", "text": "hi"},
        )
        request = opener.open.call_args.args[0]
        self.assertEqual(
            json.loads(request.data), {"chat_id": "@somechannel", "text": "hi"}
        )

    def test_the_message_id_is_lifted_to_the_top_level(self):
        """The Bot API buries the id at `result.message_id`, one level below any
        receipt reader keyed on a top-level field. The lift is what makes this
        channel reconcilable at all."""
        out, _ = self.drive(
            ["telegram-bot-adapter", "send"],
            {"chat_id": "123456789", "text": "hi"},
        )
        receipt = json.loads(out)
        self.assertEqual(receipt["ok"], True)
        self.assertEqual(receipt["method"], "sendMessage")
        self.assertEqual(receipt["chat_id"], 123456789)
        self.assertEqual(receipt["message_id"], 4242)
        self.assertEqual(receipt["result"]["message_id"], 4242)

    def test_an_absent_message_id_is_left_absent_rather_than_invented(self):
        """Unreconcilable is a failure to KNOW which message this became, never
        evidence that nothing was sent. A defaulted id would bind a real
        complaint to the wrong act."""
        out, _ = self.drive(
            ["telegram-bot-adapter", "send"],
            {"chat_id": "123456789", "text": "hi"},
            response_body=b'{"ok":true,"result":{}}',
        )
        receipt = json.loads(out)
        self.assertEqual(receipt["ok"], True)
        self.assertNotIn("message_id", receipt)

    def test_a_boolean_is_not_a_message_id(self):
        out, _ = self.drive(
            ["telegram-bot-adapter", "send"],
            {"chat_id": "123456789", "text": "hi"},
            response_body=b'{"ok":true,"result":{"message_id":true}}',
        )
        self.assertNotIn("message_id", json.loads(out))

    def test_a_refused_send_shows_the_provider_reason_and_not_a_receipt(self):
        """A receipt shape for a send the provider refused would read as a
        message that exists."""
        with self.assertRaises(SystemExit):
            self.drive(
                ["telegram-bot-adapter", "send"],
                {"chat_id": "123456789", "text": "hi"},
                response_body=b'{"ok":false,"description":"chat not found"}',
            )

    def test_the_send_route_refuses_a_parameter_it_does_not_model(self):
        """`method` above all: the send route must not be steerable into
        getUpdates by a key nobody expected."""
        for extra in [{"method": "getUpdates"}, {"data": "{}"}, {"parse_mode": "HTML"}]:
            payload = {"chat_id": "123456789", "text": "hi"}
            payload.update(extra)
            with self.assertRaises(SystemExit):
                self.drive(["telegram-bot-adapter", "send"], payload)

    def test_the_send_route_refuses_a_chat_id_it_cannot_classify(self):
        """A recipient this cannot classify is one the gate upstream could not
        canonicalise either, so it is refused rather than forwarded."""
        for chat_id in ["", "   ", "not-a-chat", "123abc", "@", "12345678901234567890"]:
            with self.assertRaises(SystemExit):
                self.drive(
                    ["telegram-bot-adapter", "send"],
                    {"chat_id": chat_id, "text": "hi"},
                )

    def test_the_send_route_refuses_an_empty_or_oversized_body(self):
        for text in ["", "   ", "x" * (MODULE.MAX_TEXT_BYTES + 1)]:
            with self.assertRaises(SystemExit):
                self.drive(
                    ["telegram-bot-adapter", "send"],
                    {"chat_id": "123456789", "text": text},
                )

    def test_a_non_string_recipient_is_refused(self):
        """The skill types `chat_id` as a string so the outward gate can read it
        — an integer arriving here means something bypassed that typing."""
        with self.assertRaises(SystemExit):
            self.drive(
                ["telegram-bot-adapter", "send"],
                {"chat_id": 123456789, "text": "hi"},
            )

    def test_an_unknown_route_never_falls_back_to_the_ungated_path(self):
        with self.assertRaises(SystemExit):
            self.drive(
                ["telegram-bot-adapter", "sendmessage"],
                {"method": "getMe"},
            )

    def test_the_run_route_still_echoes_the_provider_body_verbatim(self):
        """`run` is the diagnostic escape hatch and narrowing it would break
        existing use. Its output shape must not have moved."""
        body = b'{"ok":true,"result":{"id":7,"username":"deepact_bot"}}'
        out, opener = self.drive(
            ["telegram-bot-adapter"], {"method": "getMe"}, response_body=body
        )
        self.assertEqual(out, body + b"\n")
        self.assertTrue(opener.open.call_args.args[0].full_url.endswith("/getMe"))


if __name__ == "__main__":
    unittest.main()
