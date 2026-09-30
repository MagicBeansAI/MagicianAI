import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('review', Path(__file__).with_name('eval-memory-human-review.py'))
review = importlib.util.module_from_spec(spec)
spec.loader.exec_module(review)

class ReviewTests(unittest.TestCase):
    def test_partitioned_review_requires_explicit_split(self):
        cases = [{'id': 'A', 'partition': 'development'},
                 {'id': 'B', 'partition': 'held_out'}]
        with self.assertRaisesRegex(ValueError, 'requires --partition'):
            review.select_cases(cases, None)
        self.assertEqual([c['id'] for c in review.select_cases(cases, 'development')], ['A'])
        self.assertEqual([c['id'] for c in review.select_cases(cases, 'held_out')], ['B'])
        with self.assertRaisesRegex(ValueError, 'Unpartitioned'):
            review.select_cases([{'id': 'C'}], 'held_out')
        with self.assertRaisesRegex(ValueError, 'Invalid review partitions'):
            review.select_cases([{'id': 'C'}, *cases], 'development')

    def test_grouping_requires_same_operation_and_context(self):
        cases = [
            {'id': 'A', 'operation': 'one', 'input': {'context': {'goal': 'same'}}},
            {'id': 'B', 'operation': 'one', 'input': {'context': {'goal': 'same'}}},
            {'id': 'C', 'operation': 'one', 'input': {'context': {'goal': 'other'}}},
            {'id': 'D', 'operation': 'two', 'input': {'context': {'goal': 'other'}}},
        ]
        self.assertEqual([[v['id'] for v in group] for group in review.grouped_cases(cases, 3)],
                         [['A', 'B'], ['C'], ['D']])
        self.assertEqual(len(review.grouped_cases(cases, 1)), 4)
        with self.assertRaises(ValueError):
            review.grouped_cases(cases, 0)

    def test_one_physical_request_is_counted_once_for_shared_items(self):
        rows = [{'case_id': case, 'request': {'request_id': request_id}}
                for case, request_id in [('A', 'one'), ('B', 'one'), ('C', 'two')]]
        self.assertEqual([row['case_id'] for row in review.unique_request_rows(rows)],
                         ['A', 'C'])

    def test_strict_local_replay_checks_calls_and_answer_models(self):
        reply = {'model_calls': [{'call_id': 'a', 'model': 'kev-4b-mlx', 'local': True}],
                 'items': [{'item_id': 'U16', 'response': {'model': {'model': 'kev-4b-mlx'}}}]}
        review.require_local_calls(reply, {'kev-4b-mlx'})
        with self.assertRaisesRegex(ValueError, 'Nonlocal model call'):
            review.require_local_calls({'model_calls': [{'call_id': 'b', 'model': 'jev-1.13.0', 'local': False}]},
                                       {'kev-4b-mlx'})
        with self.assertRaisesRegex(ValueError, 'Nonlocal answer'):
            review.require_local_calls({'items': [{'item_id': 'U16', 'response': {'model': {'model': 'jev-1.13.0'}}}]},
                                       {'kev-4b-mlx'})

    def test_reply_membership_identity_and_transport_are_required(self):
        request={'request_id':'req','items':[{'item_id':'A'},{'item_id':'B'}]}
        policy={'engine_instance':'engine','policy_revision':'revision'}
        reply={'contract_version':5,'request_id':'req','engine_instance':'engine',
               'policy_revision':'revision','status':'answered','error':None,
               'items':[{'item_id':'A'},{'item_id':'B'}]}
        self.assertIsNone(review.reply_issue(request,reply,policy))
        for change, expected in [({'status':'transport_error'},'transport_error'),
                                 ({'policy_revision':'old'},'policy_revision_mismatch'),
                                 ({'error':'policy revision changed'},'request_error'),
                                 ({'items':[{'item_id':'A'}]},'item_membership_mismatch'),
                                 ({'items':[{'item_id':'A'},{'item_id':'A'}]},'item_membership_mismatch')]:
            self.assertEqual(review.reply_issue(request,{**reply,**change},policy),expected)

    def test_replay_rejects_incompatible_discovery_before_calls(self):
        for version in (None, 4, 6):
            with self.assertRaisesRegex(ValueError, 'Decision contract mismatch'):
                review.require_current_wire({'contract_version': version})
        review.require_current_wire({'contract_version': 5})
    def test_consumer_score_mapping(self):
        self.assertEqual(review.normalized({'type':'score','score':2.98},'importance'),0.75)
        self.assertEqual(review.normalized({'type':'score','score':5.51},'score'),6)
    def test_correct_but_deferred_is_not_fully_eligible(self):
        case={'id':'H1','operation':'x','review':{'expected_labels':{'a':True}}}
        reply={'items':[{'item_id':'H1','response':{'answers':{'a':{'type':'noul','noul':0.6}}},'eligible_answers':{}}]}
        grade=review.grade(case,reply)
        self.assertTrue(grade['correct']);self.assertFalse(grade['fully_eligible'])
    def test_missing_response_is_failure(self):
        case={'id':'H1','operation':'x','review':{'expected_labels':{'a':True}}}
        grade=review.grade(case,{})
        self.assertFalse(grade['correct']);self.assertFalse(grade['fully_eligible'])
    def test_wrong_accepted_is_reported(self):
        case={'id':'H1','operation':'x','review':{'expected_labels':{'a':False}}}
        reply={'items':[{'item_id':'H1','response':{'answers':{'a':{'type':'noul','noul':0.95}}},'eligible_answers':{'a':'approved'}}]}
        self.assertEqual(review.grade(case,reply)['accepted_wrong'],['a'])
    def test_unknown_failed_usage_is_unpriced_and_local_is_free(self):
        rows = [{'call_id':'remote','pricing':{'cost_source':'unknown','cost_usd':None}},
                {'call_id':'local','pricing':{'cost_source':'local','cost_usd':0,'pricing_version':'v1'}}]
        with patch.object(review.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(rows))) as run:
            facts = review.price_calls([{'call_id':'local'}, {'call_id':'remote'}], Path('/pricing-helper'))
        self.assertEqual(facts[0]['cost_usd'], 0)
        self.assertIsNone(facts[1]['cost_usd'])
        self.assertEqual(run.call_args.args[0], ['/pricing-helper'])
    def test_pricing_receipts_cannot_be_missing_foreign_or_duplicated(self):
        for rows in [[], [{'call_id':'foreign','pricing':{}}],
                     [{'call_id':'a','pricing':{}}, {'call_id':'a','pricing':{}}]]:
            with patch.object(review.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(rows))):
                with self.assertRaises(ValueError):
                    review.price_calls([{'call_id':'a'}], Path('/pricing-helper'))
    def test_duplicate_physical_calls_do_not_get_priced_twice(self):
        with patch.object(review.subprocess, 'run') as run:
            with self.assertRaises(ValueError):
                review.price_calls([{'call_id':'a'}, {'call_id':'a'}], Path('/pricing-helper'))
        run.assert_not_called()
    def test_invalid_prices_are_not_reported_as_measured_cost(self):
        for source, cost in [('computed', -1), ('computed', float('nan')),
                             ('unknown', 0), ('local', 1), ('computed', True)]:
            rows=[{'call_id':'a','pricing':{'cost_source':source,'cost_usd':cost,'pricing_version':'v1'}}]
            with patch.object(review.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(rows))):
                with self.assertRaises(ValueError):
                    review.price_calls([{'call_id':'a'}], Path('/pricing-helper'))

if __name__=='__main__': unittest.main()
