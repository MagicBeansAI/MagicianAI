import importlib.machinery
import importlib.util
from pathlib import Path
import unittest

path = Path(__file__).resolve().parents[1] / "bin" / "agentmail-send-adapter"
loader = importlib.machinery.SourceFileLoader("send_adapter", str(path))
spec = importlib.util.spec_from_loader(loader.name, loader)
adapter = importlib.util.module_from_spec(spec)
loader.exec_module(adapter)


class DeliveryIdentifiers(unittest.TestCase):
    def test_records_the_actual_header_instead_of_assuming_the_api_id(self):
        def fetch(inbox, message):
            self.assertEqual((inbox, message), ("company@example.test", "api-resource-id"))
            return {"inbox_id": inbox, "message_id": message,
                    "headers": {"Message-ID": "<transport-id@relay.test>"}}
        result = adapter.enrich({"message_id": "api-resource-id", "thread_id": "thread-1"},
                                "company@example.test", fetch)
        self.assertEqual(result["message_id"], "api-resource-id")
        self.assertEqual(result["delivery_identifiers"],
                         {"message_id": "<transport-id@relay.test>"})

    def test_an_unavailable_header_preserves_the_accepted_send_without_inventing_an_alias(self):
        for message in [
            {"inbox_id": "company@example.test", "message_id": "api-id"},
            {"inbox_id": "other@example.test", "message_id": "api-id",
             "headers": {"Message-ID": "<different@relay.test>"}},
            {"inbox_id": "company@example.test", "message_id": "wrong-id",
             "headers": {"Message-ID": "<different@relay.test>"}},
            {"inbox_id": "company@example.test", "message_id": "api-id",
             "headers": {"Message-ID": "<first@relay.test>", "message-id": "<second@relay.test>"}},
        ]:
            result = adapter.enrich({"message_id": "api-id", "thread_id": "thread-1"},
                                    "company@example.test", lambda *_: message)
            self.assertEqual(result["message_id"], "api-id")
            self.assertEqual(result["thread_id"], "thread-1")
            self.assertNotIn("delivery_identifiers", result)
            self.assertIn("delivery_correlation_error", result)

    def test_metadata_timeout_does_not_make_an_accepted_send_retryable(self):
        def failed(*_):
            raise TimeoutError("metadata request timed out")
        result = adapter.enrich({"message_id": "api-id"}, "company@example.test", failed)
        self.assertEqual(result["message_id"], "api-id")
        self.assertNotIn("delivery_identifiers", result)
        self.assertIn("delivery_correlation_error", result)


if __name__ == "__main__":
    unittest.main()
