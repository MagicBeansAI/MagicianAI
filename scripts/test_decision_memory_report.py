import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from decision_memory_report import report, answer, reconcile_ledger


def record(index=0, truth=True, prediction=.99):
    call_id=f'call-{index}'
    return {'schema_version':1, 'comparison_id':f'comparison-{index}', 'case_id':f'case-{index}',
        'operation':'memory_applicability', 'projection_version':'projection-1', 'reference_version':'ref-1',
        'behavior_fingerprint':'behavior-1', 'policy_revision':'policy-1', 'engine_instance':'boot-1',
        'offered':1, 'offered_item_ids':['0'], 'threshold_fingerprints':{'0':'fixture-engine-threshold-hash'}, 'items':[{'item_id':'0','status':'answered',
            'eligible_answers':{'applicable':{'status':'qualified'}},
            'thresholds':{'applicable':.9}, 'response':{'model':{'adapter':'typesafe','model':'jev-1'},
                'pack_id':'memory_applicability','pack_version':'1.0.0','answers':{'applicable':{'type':'noul','noul':prediction}}}}],
        'calls':[{'call_id':call_id,'item_ids':['0'],'provider':'decision:typesafe','model':'jev-1','status':'succeeded'}],
        'pricing':{call_id:{'cost_usd':.00001}}, 'reference':{'labels':{'0':{'applicable':truth}},
            'call':{'model':'incumbent-1','provider':'provider','cost_usd':.001,'attempts_complete':True,
                'receipt':{'context':{'llm_call_id':f'llm-{index}'},'provider_attempt_count':1,'response_reused':False}}},
        'accounting_complete':True, 'application_cache_hit':False}


def complete_inputs():
    records=[record(i,i<100,.99 if i<100 else .01) for i in range(200)]
    identity=report(records)['groups'][0]['qualification_id']
    reviews={identity:{'review_ref':'review-document','reviewer':'operator','dataset_id':'held-out-v1',
        'held_out_cases':[f'case-{i}:0' for i in range(200)],
        'cases':{'case-0:0:applicable':{'expected':True,'held_out':True},'case-100:0:applicable':{'expected':False,'held_out':True}}}}
    paired={identity:{'evidence_id':'paired-live-v1','budget_ms':3000,
        'invariants':dict.fromkeys(('rollback','cache_invalidation','provider_outage_recovery','receipts_reconciled','mutation_invariants'),True),
        'cases':[{'case_id':f'paired-{i}','expected_outcome_passed':True,'receipts_complete':True,'gated_total_usd':.0001,'incumbent_total_usd':.001,'foreground_ms':100} for i in range(10)]}}
    return records, reviews, paired


class QualificationTests(unittest.TestCase):
    def test_confident_restricted_output_can_be_observed_without_authority(self):
        row = record()
        row['items'][0]['eligible_answers'] = {}
        result = report([row])['groups'][0]['questions']['applicable']
        self.assertEqual(result['eligible'], 1)
        self.assertEqual(result['authorized'], 0)
        self.assertEqual(result['authorization_known'], 1)

    def test_sampled_reference_stage_joins_without_losing_gate_completion(self):
        row = record()
        row['reference'] = None
        row['reference_attempted'] = False
        row['requires_completion'] = True
        queued = {'stage':'reference', 'comparison_id':row['comparison_id'],
                  'status':'queued', 'reference_attempted':False, 'reference':None}
        completed = dict(queued, status='completed', reference_attempted=True,
                         reference=record()['reference'])
        text = {'stage':'completion', 'comparison_id':row['comparison_id'],
                'result_validated':True, 'text_attempted':False}
        result = report([completed, text, row, queued])
        group = result['groups'][0]
        self.assertEqual(group['questions']['applicable']['compared'], 1)
        self.assertEqual(group['reference_sampling'], {'completed': 1})
        self.assertEqual(group['reference_attempted_cases'], 1)
        self.assertNotIn('complete_incumbent_receipts_and_cost', group['missing'])
        self.assertNotIn('complete_text_stage_receipts_and_cost', group['missing'])
        self.assertNotIn('conflicting_comparison_records', group['failures'])
        forward = report([queued, row, text, completed])['groups'][0]
        self.assertEqual(forward['reference_sampling'], {'completed': 1})
        self.assertNotIn('conflicting_comparison_records', forward['failures'])

    def test_orphan_reference_stage_is_counted_without_creating_evidence(self):
        row = record()
        orphan = {'stage':'reference', 'comparison_id':'missing-primary',
                  'status':'completed', 'reference_attempted':True,
                  'reference':record()['reference']}
        result = report([orphan, row])
        self.assertEqual(result['orphan_reference_stages'], 1)
        self.assertEqual(result['groups'][0]['distinct_cases'], 1)

    def test_unknown_reference_dispatch_is_not_reported_as_an_attempt(self):
        row = record()
        row.update(mode='gate', reference=None, reference_attempted=False)
        failed = {'stage':'reference', 'comparison_id':row['comparison_id'],
                  'status':'inference_expired', 'reference_attempted':None,
                  'reference':None}
        group = report([row, failed])['groups'][0]
        self.assertEqual(group['reference_sampling'], {'inference_expired': 1})
        self.assertEqual(group['reference_attempted_cases'], 0)
        self.assertEqual(group['reference_attempt_unknown_cases'], 1)
        self.assertIn('complete_incumbent_receipts_and_cost', group['missing'])

    def test_late_receipt_reconciles_expired_call_without_using_late_labels(self):
        row = record()
        row.update(mode='gate', reference=None, reference_attempted=False)
        expired = {'stage':'reference', 'comparison_id':row['comparison_id'],
                   'status':'inference_expired', 'reference_attempted':None,
                   'reference':None}
        late = dict(expired, status='inference_expired_late',
                    reference_attempted=True,
                    reference=dict(record()['reference'], labels={}))
        for stages in ([expired, late], [late, expired]):
            group = report([row, *stages])['groups'][0]
            self.assertEqual(group['reference_sampling'], {'inference_expired_late':1})
            self.assertEqual(group['reference_attempted_cases'], 1)
            self.assertEqual(group['questions']['applicable']['compared'], 0)
            self.assertNotIn('complete_incumbent_receipts_and_cost', group['missing'])
            self.assertNotIn('conflicting_comparison_records', group['failures'])

    def test_revoked_source_retains_physical_cost_without_comparison_labels(self):
        row = record()
        row.update(mode='gate', reference=None, reference_attempted=False)
        revoked = {'stage':'reference', 'comparison_id':row['comparison_id'],
                   'status':'source_or_policy_changed_after_replay',
                   'reference_attempted':True,
                   'reference':dict(record()['reference'], labels={})}
        group = report([row, revoked])['groups'][0]
        self.assertEqual(group['questions']['applicable']['compared'], 0)
        self.assertEqual(group['reference_sampling'], {'source_or_policy_changed_after_replay':1})
        self.assertEqual(group['reference_calls'], 1)
        self.assertEqual(group['reference_cost_usd'], .001)
        self.assertNotIn('complete_incumbent_receipts_and_cost', group['missing'])

    def test_queue_timing_deduplicates_retries_and_preserves_unknown_legacy_wait(self):
        row = record()
        row['discovery_latency_ms'] = 35
        primary = dict(row['calls'][0], retry_group_id='attempts', queue_wait_ms=120,
                       latency_ms=400)
        row['calls'] = [primary, dict(primary, call_id='retry', latency_ms=200),
                        dict(primary, call_id='local', model='kev-4b',
                             queue_wait_ms=20, latency_ms=800),
                        dict(row['calls'][0], call_id='legacy'),
                        dict(primary, call_id='invalid', retry_group_id='invalid',
                             queue_wait_ms=-1, latency_ms=-1)]
        timings = report([row])['groups'][0]['timing_ms']
        self.assertEqual(timings['discovery'], {'p50':35, 'p95':35, 'count':1})
        self.assertEqual(timings['model_queue_wait'], {'p50':20, 'p95':120, 'count':2})
        self.assertEqual(timings['model_attempt'], {'p50':400, 'p95':800, 'count':3})
        legacy = report([record()])['groups'][0]['timing_ms']['model_queue_wait']
        self.assertEqual(legacy, {'p50':None, 'p95':None, 'count':0})

    def test_score_uses_only_the_reviewed_consumer_mapping(self):
        value={'type':'score','score':2.6,'confidence':.99}
        self.assertEqual(answer(value, 'episode_quality_v1_score_round_0_6'), ('3',.99))
        self.assertEqual(answer(value, 'unreviewed'), (None,None))
        value['score']=6.1
        self.assertEqual(answer(value, 'episode_quality_v1_score_round_0_6'), (None,None))

    def test_evidence_quarters_match_reference_numeric_labels(self):
        for score, expected in [(0, '0'), (.6, '0.25'), (2.1, '0.5'), (3.9, '1')]:
            self.assertEqual(answer({'type':'score', 'score':score, 'confidence':.99},
                'evidence_promote_v1_importance_quarters'), (expected,.99))
        self.assertEqual(answer({'type':'score','score':4.1,'confidence':.99},
            'evidence_promote_v1_importance_quarters'), (None,None))

    def test_diagnostic_head_does_not_block_required_head(self):
        row=record(); row['required_questions']=['applicable']
        row['items'][0]['response']['answers']['diagnostic']={'type':'noul','noul':.99}
        group=report([row])['groups'][0]
        self.assertEqual(set(group['questions']),{'applicable'})

    def test_missing_engine_threshold_hash_is_not_recomputed_with_another_algorithm(self):
        row=record(); row.pop('threshold_fingerprints')
        result=report([row])['groups'][0]
        self.assertIsNone(result['identity']['threshold_fingerprint'])
        self.assertIn('exact_identity',result['missing'])

    def test_empty_never_qualifies(self):
        self.assertEqual(report([])['status'],'INCOMPLETE')
    def test_agreement_alone_is_incomplete(self):
        records,_,_=complete_inputs()
        result=report(records)
        self.assertEqual(result['status'],'INCOMPLETE')
        self.assertIn('human_review',result['groups'][0]['missing'])
    def test_complete_fixture_evidence_can_pass_without_changing_gate(self):
        result=report(*complete_inputs())
        self.assertEqual(result['status'],'PASS',result)
        self.assertFalse(result['gate_changed'])
    def test_absent_required_question_cannot_disappear_from_qualification(self):
        records,reviews,paired=complete_inputs()
        for row in records:
            row['required_questions']=['applicable','required_but_absent']
        identity=report(records)['groups'][0]['qualification_id']
        result=report(records,{identity:next(iter(reviews.values()))},
            {identity:next(iter(paired.values()))})
        self.assertIn('required_but_absent:answered_question',result['groups'][0]['missing'])
    def test_replays_and_cache_hits_never_inflate_sample(self):
        row=record()
        cached=copy.deepcopy(row); cached['comparison_id']='cached';cached['application_cache_hit']=True
        result=report([row]*300+[cached])
        self.assertEqual(result['groups'][0]['distinct_cases'],1)
        self.assertEqual(result['groups'][0]['decision_calls'],1)
        self.assertEqual(result['skipped_cache_records'],1)
    def test_new_comparison_id_of_same_case_is_not_new_held_out_case(self):
        records=[record(i) for i in range(200)]
        for row in records: row['case_id']='same'
        group = report(records)['groups'][0]
        self.assertEqual(group['distinct_cases'],1)
        self.assertEqual(group['reference_sampling'], {'shadow':1})
    def test_behavior_revisions_do_not_pool_but_sampling_revisions_do(self):
        records=[record(i) for i in range(200)]
        for row in records[100:]:row['policy_revision']='different'
        self.assertEqual(len(report(records)['groups']),1)
        for row in records[100:]:row['behavior_fingerprint']='different'
        groups=report(records)['groups']
        self.assertEqual(len(groups),2)
        self.assertTrue(all(g['distinct_cases']==100 for g in groups))
    def test_boot_id_does_not_change_qualification_identity(self):
        a=record(0);b=record(1);b['engine_instance']='restart'
        self.assertEqual(len(report([a,b])['groups']),1)
    def test_unknown_cost_blocks_ready(self):
        records,reviews,paired=complete_inputs()
        records[0]['pricing']['call-0']['cost_usd']=None
        self.assertEqual(report(records,reviews,paired)['status'],'INCOMPLETE')
    def test_unanswered_items_remain_in_coverage_denominator(self):
        a=record(0); b=record(1); b['items'][0]['response']=None;b['items'][0]['status']='cancelled'
        groups=report([a,b])['groups']
        answered=next(g for g in groups if g['identity']['model']=='jev-1')
        self.assertEqual(answered['questions']['applicable']['coverage'],.5)
        self.assertEqual(answered['questions']['applicable']['unanswered'],1)
    def test_missing_reference_is_not_false(self):
        a=record();a['reference']=None
        result=report([a])['groups'][0]['questions']['applicable']
        self.assertEqual(result['compared'],0)
        self.assertEqual(result['missing_reference'],1)
    def test_missing_holdout_manifest_blocks_ready(self):
        records,reviews,paired=complete_inputs()
        next(iter(reviews.values())).pop('held_out_cases')
        self.assertEqual(report(records,reviews,paired)['status'],'INCOMPLETE')
    def test_training_cases_cannot_supply_held_out_eligibility(self):
        records,reviews,paired=complete_inputs()
        training=copy.deepcopy(records)
        for row in training:
            row['case_id']='training-'+row['case_id']
            row['comparison_id']='training-'+row['comparison_id']
        for row in records:
            row['items'][0]['response']['answers']['applicable']['noul']=.5
        result=report(records+training,reviews,paired)
        self.assertNotEqual(result['status'],'PASS')
        self.assertIn('applicable:200_distinct_eligible_held_out_cases',result['groups'][0]['missing'])

    def test_empty_human_annotations_do_not_count_as_review(self):
        records,reviews,paired=complete_inputs()
        review=next(iter(reviews.values()))
        review['cases']={key:{} for key in review['cases']}
        result=report(records,reviews,paired)
        self.assertIn('applicable:human_positive_negative_samples',result['groups'][0]['missing'])

    def test_training_agreement_cannot_hide_held_out_failure(self):
        records,reviews,paired=complete_inputs()
        for row in records[:30]:
            row['items'][0]['response']['answers']['applicable']['noul']=.01
        records.extend(record(i,i%2==0,.99 if i%2==0 else .01) for i in range(200,1000))
        group=report(records,reviews,paired)['groups'][0]
        self.assertEqual(group['questions']['applicable']['agreement'],.97)
        self.assertEqual(group['questions']['applicable']['held_out_agreement'],.85)
        self.assertIn('applicable:held_out_agreement_below_90_percent',group['failures'])

    def test_review_outside_manifest_does_not_satisfy_samples(self):
        records,reviews,paired=complete_inputs()
        extra=record(200)
        records.append(extra)
        review=next(iter(reviews.values()))
        review['cases']['case-200:0:applicable']=review['cases'].pop('case-0:0:applicable')
        group=report(records,reviews,paired)['groups'][0]
        self.assertIn('applicable:human_positive_negative_samples',group['missing'])

    def test_canonical_cli_matches_module_without_modifying_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            source=root/'records.json'
            row=record()
            row['scope']={'scope':{'principal':'p','workspace':'w'}}
            original=json.dumps([row])
            source.write_text(original)
            outputs=[]
            for name in ('decision-shadow-report.py','decision_memory_report.py'):
                output=root/(name+'.json')
                subprocess.run([sys.executable,str(Path(__file__).with_name(name)),str(source),
                    '--principal','p','--workspace','w','--output',str(output)],check=True,capture_output=True)
                outputs.append(json.loads(output.read_text()))
            self.assertEqual(outputs[0],outputs[1])
            self.assertFalse(outputs[0]['gate_changed'])
            self.assertEqual(source.read_text(),original)
    def test_expensive_gate_and_latency_fail(self):
        records,reviews,paired=complete_inputs()
        for case in next(iter(paired.values()))['cases']: case['gated_total_usd']=1;case['foreground_ms']=4000
        self.assertEqual(report(records,reviews,paired)['status'],'FAIL')
    def test_receipt_conflict_fails(self):
        a=record();b=record(1);b['calls'][0]['call_id']='call-0';b['calls'][0]['input_tokens']=999
        self.assertEqual(report([a,b])['status'],'FAIL')
    def test_nonfinite_price_is_unknown(self):
        a=record();a['pricing']['call-0']['cost_usd']=float('nan')
        self.assertIsNone(report([a])['groups'][0]['decision_cost_usd'])

class AdditionalQualificationTests(unittest.TestCase):
    def test_cache_and_skipped_invocations_do_not_invent_inference_evidence(self):
        rows=[{'stage':'invocation','comparison_id':str(i),'status':status} for i,status in enumerate(
            ['application_cache_hit','application_cache_hit','policy_unavailable'])]
        result=report(rows)
        self.assertEqual(result['groups'],[])
        self.assertEqual(result['status'],'INCOMPLETE')
        self.assertEqual(result['invocation_outcomes'],{'application_cache_hit':2,'policy_unavailable':1})

    def test_invalid_required_text_cannot_qualify_even_with_complete_cost(self):
        row=record(); row['requires_completion']=True
        completion={'stage':'completion','comparison_id':row['comparison_id'],
            'text_attempted':False,'result_validated':False}
        group=report([row,completion])['groups'][0]
        self.assertIn('required_text_validation_failed',group['failures'])
        self.assertEqual(group['status'],'FAIL')
        completion.pop('result_validated')
        self.assertIn('required_text_validation',report([row,completion])['groups'][0]['missing'])

    def test_high_overall_agreement_cannot_hide_poor_minority_recall(self):
        records=[record(i, i < 950, .99 if i < 975 else .01) for i in range(1000)]
        group=report(records)['groups'][0]
        self.assertEqual(group['questions']['applicable']['agreement'], .975)
        self.assertIn('applicable/false:precision_or_recall_below_90_percent', group['failures'])
        self.assertEqual(group['status'], 'FAIL')

    def test_text_completion_joins_out_of_order_and_is_not_a_second_case(self):
        row=record(); row['mode']='gate'; row['requires_completion']=True
        completion={'stage':'completion','comparison_id':row['comparison_id'], 'text_attempted':True,
            'text_call':copy.deepcopy(row['reference']['call'])}
        completion['text_call']['receipt']['context']['llm_call_id']='text-call'
        completion['text_call']['cost_usd']=.002
        result=report([completion,row])['groups'][0]
        self.assertEqual(result['distinct_cases'],1)
        self.assertEqual(result['text_calls'],1)
        self.assertAlmostEqual(result['observed_total_cost_usd'],.00301)
        self.assertIsNone(result['shadow_total_cost_usd'])

    def test_cancelled_or_unknown_text_stage_cost_stays_incomplete(self):
        row=record(); row['requires_completion']=True
        group=report([row])['groups'][0]
        self.assertIn('complete_text_stage_receipts_and_cost',group['missing'])
        self.assertIsNone(group['observed_total_cost_usd'])

    def test_engine_only_gate_does_not_invent_an_incumbent_call(self):
        row=record(); row['reference_attempted']=False; row['reference']=None
        group=report([row])['groups'][0]
        self.assertEqual(group['reference_calls'],0)
        self.assertEqual(group['reference_cost_usd'],0)
        self.assertIn('human_review',group['missing'])

    def test_unknown_incumbent_cost_blocks_ready(self):
        records,reviews,paired=complete_inputs()
        records[0]['reference']['call']['cost_usd']=None
        result=report(records,reviews,paired)
        self.assertEqual(result['status'],'INCOMPLETE')
        self.assertIsNone(result['groups'][0]['shadow_total_cost_usd'])
    def test_repeated_paired_workflow_does_not_count_as_ten(self):
        records,reviews,paired=complete_inputs()
        for case in next(iter(paired.values()))['cases']:case['case_id']='same'
        self.assertEqual(report(records,reviews,paired)['status'],'INCOMPLETE')
    def test_destructive_label_requires_one_hundred_human_checked_proposals(self):
        records,_,_=complete_inputs()
        for row in records:
            row['operation']='procedure_feedback'
            row['items'][0]['thresholds']={'deprecate':.9}
            row['items'][0]['response']['answers']={'deprecate':row['items'][0]['response']['answers']['applicable']}
            row['reference']['labels']['0']={'deprecate':row['reference']['labels']['0']['applicable']}
        result=report(records)
        group=result['groups'][0]
        self.assertIn('deprecate/true:100_human_checked_destructive_proposals',group['missing'])
        identity=group['qualification_id']
        reviews={identity:{'review_ref':'review','reviewer':'operator','dataset_id':'heldout',
            'held_out_cases':[f'case-{i}:0' for i in range(200)],
            'cases':{f'case-{i}:0:deprecate':{'expected':i!=0,'held_out':True} for i in range(100)}}}
        result=report(records,reviews)
        self.assertEqual(result['status'],'FAIL')
        self.assertIn('deprecate/true:incorrect_destructive_proposal',result['groups'][0]['failures'])

class LedgerTests(unittest.TestCase):
    def test_retry_cost_requires_every_scoped_attempt(self):
        row=record()
        row['reference']['call']['receipt']['provider_attempt_count']=2
        call={'llm_call_id':'llm-0','principal':'p','workspace':'w','model':'incumbent-1','provider':'provider','provider_attempt_count':2}
        attempts=[dict(call, provider_attempt_id=f'a{i}',provider_attempt_index=i,cost_usd=.002,pricing_version='dated',cost_source='computed',success=i==1) for i in range(2)]
        ledger={'calls':[call], 'attempts':attempts}
        joined=reconcile_ledger([row],ledger,'p','w')[0]
        self.assertEqual(joined['reference']['call']['cost_usd'],.004)
        self.assertTrue(joined['reference']['call']['attempts_complete'])
        ledger['attempts'].pop()
        joined=reconcile_ledger([row],ledger,'p','w')[0]
        self.assertIsNone(joined['reference']['call']['cost_usd'])
        self.assertFalse(joined['reference']['call']['attempts_complete'])
        ledger['calls'][0]['principal']='other'
        self.assertFalse(reconcile_ledger([row],ledger,'p','w')[0]['ledger_reconciled'])

    def test_conflicting_comparison_replay_cannot_silently_win(self):
        a=record();b=copy.deepcopy(a);b['items'][0]['response']['answers']['applicable']['noul']=.01
        self.assertIn('conflicting_comparison_records', report([a,b])['groups'][0]['failures'])

    def test_conflicting_duplicate_prices_never_win(self):
        a=record(); b=copy.deepcopy(a); b['comparison_id']='different'
        b['pricing']['call-0']['cost_usd']=1
        self.assertIn('conflicting_receipts', report([a,b])['groups'][0]['failures'])

    def test_unknown_cache_counters_do_not_become_zero(self):
        group=report([record()])['groups'][0]
        self.assertIsNone(group['decision_usage']['cache_read_fraction'])
        self.assertIsNone(group['decision_usage']['cache_read_tokens']['reported_sum'])

if __name__=='__main__': unittest.main()
