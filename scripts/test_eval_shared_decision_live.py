import importlib.util
from pathlib import Path
import unittest
import tempfile
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location('rail_eval', Path(__file__).with_name('eval-shared-decision-live.py'))
evaluation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluation)


class QualificationAccounting(unittest.TestCase):
    def test_gate_probe_uses_v4_and_preserves_memory_rollout(self):
        original = ('operations:\n  tool_action_judge:\n    gate:\n      enabled: true\n'
                    '    shadow:\n      enabled: false\n  memory_attach:\n'
                    '    gate:\n      enabled: false\n    shadow:\n      enabled: true\n')
        with tempfile.TemporaryDirectory() as root:
            client = evaluation.Client.__new__(evaluation.Client)
            client.args = SimpleNamespace(config=Path(root)/'decision-engine.yaml')
            def probe(path, body):
                self.assertEqual(path, '/v1/action')
                self.assertEqual(body['contract_version'], 4)
                return {'reason':'structured_gate_disabled'}
            client.decision = probe
            client.gate(original, 'off')
            updated = client.args.config.read_text()
            self.assertEqual(updated.split('  memory_attach:')[1], original.split('  memory_attach:')[1])
            self.assertIn('tool_action_judge:\n    gate:\n      enabled: false', updated)

    def test_unknown_cost_and_cache_do_not_become_zero_or_complete(self):
        events = [{'event_type': 'llm.succeeded', 'payload': {'llm_call_id': 'a', 'input_tokens': 100, 'output_tokens': 5, 'cost': .25, 'cache_read_tokens': 80}},
                  {'event_type': 'llm.succeeded', 'payload': {'llm_call_id': 'b', 'input_tokens': 50, 'output_tokens': 2, 'cost': 0, 'cache_read_tokens': 0, 'usage_availability': {'tokens': True, 'cost': False, 'cache_read': False}}}]
        result = evaluation.usage_summary(events + [events[0]])
        self.assertEqual(result['calls'], 2)
        self.assertEqual(result['input_tokens']['reported_sum'], 150)
        self.assertEqual(result['cost_usd']['reported_sum'], .25)
        self.assertFalse(result['cost_usd']['complete'])
        self.assertIsNone(result['cache_hit_rate'])

    def test_answer_text_alone_cannot_pass(self):
        record = {'execution_result': {'state': {'status': 'completed', 'completion_kind': 'partial'}}, 'final_output': 'RAIL_X RAIL_Y'}
        self.assertFalse(evaluation.grade_file(record, ['RAIL_X', 'RAIL_Y']))
        record['execution_result']['state']['completion_kind'] = 'full'
        self.assertFalse(evaluation.grade_file(record, ['RAIL_X', 'RAIL_Y']))
        record['events'] = [{'event_type': 'tool.succeeded', 'payload': {'target': 'read_file'}}] * 2
        self.assertFalse(evaluation.grade_file(record, ['RAIL_X', 'RAIL_Y']))
        record['fixture_paths'] = ['/tmp/first', '/tmp/second']
        record['events'][0] = {'event_type': 'tool.succeeded', 'payload': {'target': 'read_file', 'path': '/tmp/first'}}
        self.assertFalse(evaluation.grade_file(record, ['RAIL_X', 'RAIL_Y']))
        record['events'][1] = {'event_type': 'tool.succeeded', 'payload': {'target': 'read_file', 'path': '/tmp/second'}}
        self.assertTrue(evaluation.grade_file(record, ['RAIL_X', 'RAIL_Y']))

    def test_browser_requires_post_receipt_and_successful_click(self):
        record = {'execution_result': {'state': {'status': 'completed', 'completion_kind': 'full'}}, 'final_output': 'BROWSER_PROOF_X'}
        self.assertFalse(evaluation.grade_browser(record, 'BROWSER_PROOF_X'))
        record['events'] = [{'event_type': 'tool.succeeded', 'payload': {'target': 'browser__click'}}]
        self.assertFalse(evaluation.grade_browser(record, None))
        self.assertFalse(evaluation.grade_browser(record, 'BROWSER_PROOF_OTHER'))
        self.assertTrue(evaluation.grade_browser(record, 'BROWSER_PROOF_X'))
        record['execution_result']['state']['completion_kind'] = 'partial'
        self.assertFalse(evaluation.grade_browser(record, 'BROWSER_PROOF_X'))

    def test_pair_order_alternates_across_engines_and_repetitions(self):
        self.assertEqual(evaluation.pair_order(0, 0), ['on', 'off'])
        self.assertEqual(evaluation.pair_order(1, 0), ['off', 'on'])
        self.assertEqual(evaluation.pair_order(0, 1), ['off', 'on'])


if __name__ == '__main__':
    unittest.main()
