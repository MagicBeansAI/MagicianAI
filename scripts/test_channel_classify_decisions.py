import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import subprocess
import sys

from channel_classify_review import freeze, private_path, write_private, PROJECTION_VERSION, review_counts
import channel_classify_qualification as qualification
spec=importlib.util.spec_from_file_location('classify_eval',Path(__file__).with_name('eval-channel-classify-decisions.py'))
evaluation=importlib.util.module_from_spec(spec);spec.loader.exec_module(evaluation)


class ClassifyQualificationTests(unittest.TestCase):
    def case(self):
        return {'id':'case','projection_version':PROJECTION_VERSION,'input':{'state':{}},
            'expected':{'lane':'needs_reply','follow_up_kind':'needs_reply','action_owner':'owner','urgency':'normal','repeat_of_recently_handled':False},
            'review':{'status':'approved'}}

    def test_machine_state_does_not_substitute_for_explicit_human_review(self):
        row=self.case();row['review']['status']='pending';row['annotation_state']='needs_approval'
        with self.assertRaises(ValueError):freeze([row])
        row['review']['status']='approved';self.assertEqual(freeze([row]),[row])
        row['expected'].pop('repeat_of_recently_handled')
        with self.assertRaises(ValueError):freeze([row])

    def test_private_output_rejects_repo_and_symlink_escape(self):
        repo=Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);allowed=root/'evals/channel_classify'
            write_private(allowed/'review.jsonl',[self.case()],root)
            with self.assertRaises(FileExistsError):write_private(allowed/'review.jsonl',[self.case()],root)
            with self.assertRaises(ValueError):private_path(repo/'data/review.jsonl',root)
            (allowed/'escape').symlink_to(repo,target_is_directory=True)
            with self.assertRaises(ValueError):private_path(allowed/'escape/review.jsonl',root)

    def test_delegated_review_requires_authorization_and_has_separate_counts(self):
        row=self.case();row['review']['mode']='delegated'
        with self.assertRaises(ValueError):freeze([row])
        row['review']['authorization_ref']='owner instruction to adjudicate the review'
        self.assertEqual(freeze([row]),[row])
        self.assertEqual(review_counts([row]),{'human_reviewed':0,'delegated_reviewed':1,'approved_cases':1})

    def test_missing_baseline_cannot_fit_a_threshold(self):
        row={'expected':{'lane':'needs_reply'},'baseline':{},'answers':{'lane':{'type':'choice','choice':'needs_reply','confidence':.99}}}
        self.assertIsNone(evaluation.fit_threshold([row],'lane')['threshold'])

    def test_threshold_comparison_uses_matching_subset_and_needs_reply_recall(self):
        good={'expected':{'lane':'needs_reply'},'baseline':{'lane':'needs_reply'},'answers':{'lane':{'type':'choice','choice':'needs_reply','confidence':.95}}}
        wrong={'expected':{'lane':'needs_reply'},'baseline':{'lane':'needs_reply'},'answers':{'lane':{'type':'choice','choice':'no_action','confidence':.7}}}
        fit=evaluation.fit_threshold([good,wrong],'lane');self.assertEqual(fit['threshold'],.95);self.assertEqual(fit['coverage'],.5)
        self.assertEqual(fit['needs_reply_recall'],1)
        self.assertIsNone(evaluation.fit_threshold([wrong],'lane')['threshold'])

    def test_no_positive_lane_evidence_cannot_establish_recall(self):
        row={'expected':{'lane':'fyi'},'baseline':{'lane':'fyi'},'answers':{'lane':{'type':'choice','choice':'fyi','confidence':.99}}}
        self.assertIsNone(evaluation.fit_threshold([row],'lane')['threshold'])

    def test_false_noul_uses_its_confidence_and_nonfinite_values_are_unusable(self):
        self.assertEqual(evaluation.normalized({'type':'noul','noul':.02}),(False,.98))
        for value in [float('nan'),float('inf'),True,-.1,1.1]:self.assertEqual(evaluation.normalized({'type':'noul','noul':value}),(None,None))

    def test_held_out_recall_failure_does_not_refit_threshold(self):
        row=self.case();row['baseline']=copy.deepcopy(row['expected'])
        row['answers']={q:({'type':'noul','noul':.01} if type(v) is bool else {'type':'choice','choice':v,'confidence':.95}) for q,v in row['expected'].items()}
        thresholds={q:.7 for q in evaluation.REQUIRED}
        self.assertTrue(evaluation.validate_thresholds([row],thresholds)['bar_met'])
        row['answers']['lane']['choice']='no_action'
        validation=evaluation.validate_thresholds([row],thresholds)
        self.assertFalse(validation['bar_met']);self.assertEqual(validation['needs_reply_recall'],0)
        self.assertEqual(thresholds['lane'],.7)

    def test_partial_and_invalid_answers_never_resolve_a_thread(self):
        row=self.case();row['answers']={'lane':{'type':'choice','choice':'needs_reply','confidence':float('nan')}}
        self.assertFalse(evaluation.accepted(row,{q:.7 for q in evaluation.REQUIRED}))

    def test_pending_predictions_never_become_reviewed_gold(self):
        row=self.case();row['review']['status']='pending';row['expected']={}
        evaluation.validate_cases([row],predictions_only=True)
        with self.assertRaises(ValueError):evaluation.validate_cases([row])
        with self.assertRaises(ValueError):evaluation.validate_cases([row,row],predictions_only=True)
        row['review']['status']='synthetic'
        with self.assertRaises(ValueError):evaluation.validate_cases([row],predictions_only=True)

    def test_replay_requires_exact_input_model_operation_and_holdout_partition(self):
        from types import SimpleNamespace
        case=self.case();case['partition']='held_out'
        request={'operation':'channel_classify','projection_version':PROJECTION_VERSION,
            'expected_policy_revision':'test-policy',
            'context':None,'items':[{'item_id':case['id'],'state':case['input']['state']}]}
        response={'model':{'model':'jev'},'pack_id':'channel_classify','pack_version':'1.0.0','answers':{'lane':{'type':'choice','choice':'needs_reply','confidence':.95}}}
        row={'id':case['id'],'input_sha256':evaluation.input_fingerprint(case),'partition':'held_out',
            'request':request,'reply':{'items':[{'item_id':case['id'],'response':response}]}}
        helper=SimpleNamespace(reply_issue=lambda *args:None)
        policy={'policy_revision':'test-policy','operations':[{'name':'channel_classify','classification':{'pack':'channel_classify','pack_version':'1.0.0'}}]}
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/'rows.jsonl'
            def write(value):path.write_text(json.dumps(value)+'\n')
            write(row)
            replay=evaluation.replay_rows(path,[case],policy,helper,'jev')
            self.assertEqual(replay[0]['expected'],case['expected'])
            self.assertEqual(replay[0]['answers'],response['answers'])
            for edit in (lambda r:r.update(input_sha256='changed'),
                         lambda r:r.update(partition='development'),
                         lambda r:r['request'].update(operation='memory_applicability'),
                         lambda r:r['request']['items'][0].update(state={'summary':'changed'}),
                         lambda r:r['reply']['items'][0]['response']['model'].update(model='other')):
                changed=copy.deepcopy(row);edit(changed);write(changed)
                with self.assertRaises(ValueError):evaluation.replay_rows(path,[case],policy,helper,'jev')

    def test_review_render_requires_missing_heads_and_escapes_html(self):
        from channel_classify_review import render
        row=self.case();row['input']['state']={'summary':'<script>bad</script>'}
        text=render([row]);self.assertIn('REVIEW REQUIRED',text);self.assertNotIn('<script>',text)

    def test_replay_preserves_dated_prices_and_matches_physical_receipts(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/'rows.jsonl'
            path.write_text(json.dumps({'reply':{'model_calls':[{'call_id':'a'},{'call_id':'b'}]}})+'\n')
            prices=[{'cost_usd':.001,'pricing_version':'old-a','cost_source':'computed'},
                    {'cost_usd':0,'pricing_version':'old-local','cost_source':'local'}]
            (path.parent/'pricing.json').write_text(json.dumps(prices))
            self.assertEqual(evaluation.dated_replay_prices(path,[{'call_id':'b'},{'call_id':'a'}]),prices[::-1])
            with self.assertRaises(ValueError):evaluation.dated_replay_prices(path,[{'call_id':'missing'}])
            prices[1]['cost_usd']=1
            (path.parent/'pricing.json').write_text(json.dumps(prices))
            with self.assertRaises(ValueError):evaluation.dated_replay_prices(path,[{'call_id':'a'},{'call_id':'b'}])

    def test_sixteen_committed_fixtures_cover_every_required_head_and_lane(self):
        rows=[json.loads(line) for line in (Path(__file__).resolve().parents[1]/'data/magician_v2/evals/channel_classify/synthetic.jsonl').read_text().splitlines()]
        self.assertEqual(len(rows),16);self.assertEqual({r['expected']['lane'] for r in rows},{'needs_reply','follow_up','fyi','no_action'})
        for row in rows:
            row=copy.deepcopy(row);row['review']['status']='approved';freeze([row])

class LiveClassifyQualificationTests(unittest.TestCase):
    def fixtures(self, count=200):
        expected=ClassifyQualificationTests().case()['expected']
        outputs={q:[str(value).lower()] for q,value in expected.items()}
        evidence={'model':'test-model','adapter':'test-adapter','provider':'decision:test',
            'profile':'test-profile','pack_version':'1.0.0','thresholds':{q:.75 for q in evaluation.REQUIRED},
            'held_out':{'bar_met':True},'reviewed_outputs':outputs,
            'baseline_agreement':{q:1 for q in evaluation.REQUIRED},
            'output_agreement':{q:{values[0]:1} for q,values in outputs.items()},'evidence_digest':'offline-digest'}
        records=[]
        for n in range(count):
            call_id=f'call-{n}';item_id=f'item-{n}'
            answers={q:({'type':'noul','noul':.01} if type(value) is bool else {'type':'choice','choice':value,'confidence':.95}) for q,value in expected.items()}
            records.append({'schema_version':1,'operation':'channel_classify','mode':'shadow',
                'comparison_id':f'comparison-{n}','case_id':f'case-{n}',
                'projection_version':PROJECTION_VERSION,'reference_version':'reference','behavior_fingerprint':'behavior',
                'required_questions':list(evaluation.REQUIRED),'threshold_fingerprints':{item_id:'threshold-fingerprint'},
                'accounting_complete':True,'pricing':{call_id:{'cost_usd':0,'cost_source':'local','pricing_version':'dated'}},
                'calls':[{'call_id':call_id,'model':'test-model','adapter':'test-adapter','provider':'decision:test','status':'succeeded','item_ids':[item_id]}],
                'reference':{'labels':{item_id:copy.deepcopy(expected)},'calls':[{'attempts_complete':True,'cost_usd':.0001,'pricing_version':'dated',
                    'receipt':{'context':{'llm_call_id':f'llm-{n}'}}}]},
                'items':[{'item_id':item_id,'thresholds':copy.deepcopy(evidence['thresholds']),
                    'response':{'model':{'model':'test-model','adapter':'test-adapter'},'pack_id':'channel_classify','pack_version':'1.0.0','answers':answers}}]})
        return evidence,records

    def test_exact_reviewed_two_hundred_live_cases_emit_bound_candidates_without_activation(self):
        evidence,records=self.fixtures()
        result=qualification.candidates([evidence],records,'owner-review')
        self.assertEqual(result['status'],'PASS');self.assertEqual(result['distinct_live_comparisons'],200)
        self.assertFalse(result['gate_changed'])
        certificates=result['groups'][0]['qualifications'];self.assertEqual(len(certificates),5)
        required={'question','output','model','provider','pack_version','threshold_fingerprint',
            'projection_version','reference_version','behavior_fingerprint','evidence_id','human_review_ref'}
        self.assertTrue(all(set(item)==required for item in certificates))
        self.assertEqual(next(c['output'] for c in certificates if c['question']=='repeat_of_recently_handled'),'false')
        changed=copy.deepcopy(evidence);changed['evidence_digest']='corrected-gold'
        updated=qualification.candidates([changed],records,'owner-review')
        self.assertNotEqual(certificates[0]['evidence_id'],updated['groups'][0]['qualifications'][0]['evidence_id'])

    def test_offline_failure_does_not_erase_complete_live_measurements(self):
        evidence,records=self.fixtures();evidence['held_out']['bar_met']=False
        result=qualification.candidates([evidence],records,'owner-review')
        self.assertEqual(result['distinct_live_comparisons'],200)
        group=result['groups'][0]
        self.assertIn('held_out_offline_bar',group['missing'])
        self.assertNotIn('200_distinct_live_comparisons',group['missing'])
        self.assertEqual(group['metrics']['lane']['compared'],200)
        self.assertEqual(group['qualifications'],[])

    def test_stale_thresholds_and_missing_prices_never_pad_minimum(self):
        evidence,records=self.fixtures()
        for mutation in (lambda r:r['items'][0]['thresholds'].update(lane=.9),
                         lambda r:r['pricing']['call-199'].update(cost_usd=None),
                         lambda r:r['pricing']['call-199'].update(cost_usd=-1),
                         lambda r:r['pricing']['call-199'].pop('pricing_version'),
                         lambda r:r['reference']['calls'][0].update(attempts_complete=False),
                         lambda r:r['items'][0]['response'].update(pack_id='other')):
            rows=copy.deepcopy(records);mutation(rows[-1])
            result=qualification.candidates([evidence],rows,'owner-review')
            self.assertEqual(result['status'],'INCOMPLETE');self.assertEqual(result['distinct_live_comparisons'],199)
            self.assertTrue(all(not g['qualifications'] for g in result['groups']))

    def test_different_reference_or_behavior_cohorts_do_not_combine(self):
        evidence,records=self.fixtures()
        for row in records[100:]:row['reference_version']='other-reference'
        result=qualification.candidates([evidence],records,'owner-review')
        self.assertEqual(result['status'],'INCOMPLETE');self.assertEqual(result['distinct_live_comparisons'],100)
        current=qualification.candidates([evidence],records,'owner-review',current_behavior='new-behavior',reference_version='reference')
        self.assertEqual(current['distinct_live_comparisons'],0);self.assertEqual(current['status'],'INCOMPLETE')

    def test_pending_review_is_rejected_before_engine_access_or_output(self):
        case=ClassifyQualificationTests().case();case['review']['status']='pending'
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);gold=root/'review.jsonl';gold.write_text(json.dumps(case)+'\n')
            output=root/'evals/channel_classify/qualification.json'
            result=subprocess.run([sys.executable,str(Path(__file__).with_name('channel_classify_qualification.py')),
                '--gold',str(gold),'--baseline',str(root/'missing-baseline'),
                '--offline',str(root/'missing-offline'),'--records',str(root/'missing-records'),
                '--review-ref','pending','--principal','p','--workspace','w','--root',str(root),
                '--socket',str(root/'missing-socket'),'--reference-version','reference','--output',str(output)],capture_output=True,text=True)
            self.assertEqual(result.returncode,2);self.assertIn('explicit approved human review',result.stderr)
            self.assertFalse(output.exists())

    def test_duplicates_do_not_pad_counts_and_conflicting_replays_fail(self):
        evidence,records=self.fixtures(199)
        result=qualification.candidates([evidence],records+[copy.deepcopy(records[-1])],'owner-review')
        self.assertEqual(result['distinct_live_comparisons'],199)
        changed=copy.deepcopy(records[-1]);changed['comparison_id']='another-comparison'
        changed['items'][0]['response']['answers']['lane']['choice']='no_action'
        result=qualification.candidates([evidence],records+[changed],'owner-review')
        self.assertEqual(result['status'],'FAIL')

    def test_needs_reply_recall_loss_blocks_certificate(self):
        evidence,records=self.fixtures()
        records[-1]['items'][0]['response']['answers']['lane']['choice']='no_action'
        result=qualification.candidates([evidence],records,'owner-review')
        self.assertEqual(result['status'],'FAIL')
        self.assertIn('live_needs_reply_recall_loss',result['groups'][0]['failures'])

    def test_unseen_output_cannot_leave_pass_without_usable_joint_answers(self):
        evidence,records=self.fixtures();evidence['reviewed_outputs']['urgency']=[]
        result=qualification.candidates([evidence],records,'owner-review')
        self.assertEqual(result['status'],'INCOMPLETE');self.assertEqual(result['groups'][0]['qualifications'],[])

    def test_offline_failure_and_duplicate_model_evidence_fail_closed(self):
        evidence,records=self.fixtures();evidence['held_out']['bar_met']=False
        result=qualification.candidates([evidence],records,'owner-review')
        self.assertEqual(result['status'],'INCOMPLETE');self.assertEqual(result['distinct_live_comparisons'],200)
        with self.assertRaises(ValueError):qualification.candidates([evidence,evidence],records,'owner-review')
        with self.assertRaises(ValueError):qualification.candidates([evidence],records,'')

    def test_rare_wrong_output_does_not_inherit_aggregate_accuracy(self):
        row=ClassifyQualificationTests().case();row['baseline']=copy.deepcopy(row['expected'])
        row['answers']={q:({'type':'noul','noul':.01} if type(v) is bool else {'type':'choice','choice':v,'confidence':.95}) for q,v in row['expected'].items()}
        wrong=copy.deepcopy(row);wrong['answers']['lane']['choice']='no_action'
        compensation=copy.deepcopy(row);compensation['baseline']['lane']='no_action'
        rows=[row,compensation,wrong]
        self.assertTrue(evaluation.validate_thresholds(rows,{q:.75 for q in evaluation.REQUIRED})['bar_met'])
        labels,_=qualification.reviewed_outputs(evaluation,rows)
        self.assertEqual(labels['lane'],['needs_reply'])


if __name__=='__main__':unittest.main()
