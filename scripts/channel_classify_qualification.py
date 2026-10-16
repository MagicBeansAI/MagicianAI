#!/usr/bin/env python3
"""Build private Channel Assist qualification candidates from reviewed evidence.

The classify plan has two separate bars: frozen owner-reviewed offline gold and
200 live shadow comparisons. Memory qualification floors remain unchanged.
This command writes an artifact; it never changes settings or enables a gate.
"""
import argparse
from collections import defaultdict
import importlib.util
import json
from pathlib import Path

from channel_classify_review import REQUIRED, PROJECTION_VERSION, VOCAB, freeze, private_path
from decision_memory_report import answer, canonical, digest, finite, label, load_records, reference_physical_calls


def dated_cost(fact):
    cost=fact.get('cost_usd')
    return (finite(cost) and cost>=0 and bool(fact.get('pricing_version'))
        and (fact.get('cost_source')!='local' or cost==0))


def complete_reference(reference):
    calls=reference_physical_calls(reference)
    return bool(calls) and all(call.get('attempts_complete') is True and dated_cost(call)
        and (call.get('receipt') or {}).get('context',{}).get('llm_call_id') for call in calls)


def valid_labels(values):
    return all(values.get(q) in options for q,options in VOCAB.items()) and type(values.get('repeat_of_recently_handled')) is bool


def reviewed_outputs(evaluation, resolved):
    labels=defaultdict(set);agreements=defaultdict(dict)
    for q in REQUIRED:
        for value in {evaluation.normalized(row['answers'][q])[0] for row in resolved}:
            subset=[row for row in resolved if evaluation.normalized(row['answers'][q])[0]==value]
            correct=sum(value==row['expected'][q] for row in subset)
            baseline_correct=sum(row['baseline'][q]==row['expected'][q] for row in subset)
            if correct and correct>=baseline_correct:
                labels[q].add(label(value))
                agreements[q][label(value)]=sum(value==row['baseline'][q] for row in subset)/len(subset)
    return {q:sorted(values) for q,values in labels.items()},dict(agreements)


def evaluation_module(name):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(name+'.py'))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module


def offline_evidence(directory, gold, baseline):
    """Recompute the bar from raw receipts; report flags are not approval."""
    evaluation=evaluation_module('eval-channel-classify-decisions')
    helper=evaluation_module('eval-memory-human-review')
    policy=json.loads((directory/'policy.json').read_text())
    helper.require_current_wire(policy)
    summary=json.loads((directory/'report.json').read_text())
    rows=evaluation.replay_rows(directory/'rows.jsonl',gold,policy,helper,summary['model'])
    calls=[call for row in rows for call in row['reply'].get('model_calls',[])]
    prices=evaluation.dated_replay_prices(directory/'rows.jsonl',calls)
    if not calls or not all(dated_cost(price) for price in prices):
        raise ValueError('offline physical costs are incomplete')
    producers={(call.get('model'),call.get('adapter'),call.get('provider')) for call in calls if call.get('status')=='succeeded'}
    if len(producers)!=1 or next(iter(producers))[0]!=summary['model'] or not all(next(iter(producers))):
        raise ValueError('offline model identity is incomplete or mixed')
    for row in rows:
        reference=baseline.get(row['id'],{})
        case=next(case for case in gold if case['id']==row['id'])
        if reference.get('input_sha256')!=evaluation.input_fingerprint(case):
            raise ValueError('baseline differs from frozen reviewed gold')
        if not valid_labels(reference.get('labels',{})) or not complete_reference(reference.get('reference')):
            raise ValueError('baseline labels or physical costs are incomplete')
        row['baseline']=reference.get('labels',{})
    fits={q:evaluation.fit_threshold([row for row in rows if row['partition']=='development'],q) for q in REQUIRED}
    thresholds={q:fit['threshold'] for q,fit in fits.items() if fit['threshold'] is not None}
    held=[row for row in rows if row['partition']=='held_out']
    validation=evaluation.validate_thresholds(held,thresholds)
    resolved=[row for row in held if evaluation.accepted(row,thresholds)]
    operation=next(op for op in policy['operations'] if op['name']=='channel_classify')
    routes=operation['route_cloud']
    if len(routes)!=1 or operation['route_local']!=routes:
        raise ValueError('offline evidence must come from one pinned model profile')
    if operation['classification']['pack']!='channel_classify' or operation['classification']['pack_version'] not in ('1.0.0','1.1.0'):
        raise ValueError('unsupported classify pack')
    # A rare wrong output must not inherit a certificate from other outputs
    # that rescued the aggregate question accuracy.
    labels,output_agreement=reviewed_outputs(evaluation,resolved)
    agreement={q:sum(evaluation.normalized(row['answers'][q])[0]==row['baseline'][q] for row in resolved)/len(resolved)
               for q in REQUIRED} if resolved else {}
    _,adapter,provider=next(iter(producers))
    return {'model':summary['model'],'adapter':adapter,'provider':provider,'pack_version':operation['classification']['pack_version'],
        'profile':routes[0],'thresholds':thresholds,
        'held_out':validation,'reviewed_outputs':labels,
        'baseline_agreement':agreement,'output_agreement':output_agreement,
        'evidence_digest':digest({'gold':gold,'rows':rows,'pricing':prices,
            'baseline':[baseline[row['id']] for row in rows]})}


def candidates(evidence, records, review_ref, minimum_live=200, tolerance=.02, current_behavior=None, reference_version=None):
    """Only current, complete, distinct shadow comparisons can qualify output."""
    by_model={item['model']:item for item in evidence}
    if len(by_model)!=len(evidence):raise ValueError('duplicate offline model evidence')
    if not review_ref or len(review_ref)>512:raise ValueError('a bounded human review reference is required')
    groups=defaultdict(list);comparisons={};conflicts=set();cohort_cases=defaultdict(set)
    repeated={};repeat_conflicts=set();rejected=defaultdict(int)
    for record in records:
        if record.get('operation')!='channel_classify' or record.get('stage') or record.get('mode')!='shadow' or record.get('application_cache_hit'):
            continue
        if current_behavior is not None and record.get('behavior_fingerprint')!=current_behavior:continue
        if reference_version is not None and record.get('reference_version')!=reference_version:continue
        comparison=record.get('comparison_id')
        if not comparison or not record.get('case_id'):continue
        if comparison in comparisons:
            if canonical(record)!=canonical(comparisons[comparison]):conflicts.add(comparison)
            continue
        comparisons[comparison]=record
        references=(record.get('reference') or {}).get('labels',{})
        for item in record.get('items',[]):
            response=item.get('response') or {};model=response.get('model',{}).get('model')
            reviewed=by_model.get(model)
            if not reviewed:continue
            item_id=item.get('item_id');truth=references.get(item_id,{})
            answers=response.get('answers',{})
            if not item_id or not valid_labels(truth) or any(answer(answers.get(q,{}))[0] is None for q in REQUIRED):continue
            calls=[call for call in record.get('calls',[]) if call.get('model')==model and call.get('adapter')==response.get('model',{}).get('adapter') and item_id in call.get('item_ids',[]) and call.get('status')=='succeeded']
            identity={'model':model,'provider':calls[-1].get('provider') if calls else None,
                'pack_version':response.get('pack_version'),'projection_version':record.get('projection_version'),
                'reference_version':record.get('reference_version'),'behavior_fingerprint':record.get('behavior_fingerprint'),
                'threshold_fingerprint':(record.get('threshold_fingerprints') or {}).get(item_id)}
            group=digest(identity)
            thresholds=item.get('thresholds') or {}
            missing=[]
            # A failed offline bar blocks the group certificate below, but it
            # does not erase otherwise complete live comparisons or their
            # measured agreement. Keep completeness and rollout authority separate.
            if (any(not value for value in identity.values()) or identity['projection_version']!=PROJECTION_VERSION
                    or response.get('pack_id')!='channel_classify' or identity['pack_version']!=reviewed['pack_version']
                    or response.get('model',{}).get('adapter')!=reviewed['adapter'] or identity['provider']!=reviewed['provider']
                    or set(record.get('required_questions',[]))!=set(REQUIRED)):
                missing.append('exact_identity')
            if set(reviewed['thresholds'])!=set(REQUIRED) or any(thresholds.get(q)!=reviewed['thresholds'].get(q) for q in REQUIRED):
                missing.append('live_thresholds_differ_from_fitted_thresholds')
            physical=record.get('calls',[])
            if (record.get('accounting_complete') is not True or not physical
                    or any(not call.get('call_id') or not dated_cost((record.get('pricing') or {}).get(call.get('call_id'),{})) for call in physical)):
                missing.append('complete_live_decision_cost')
            if not complete_reference(record.get('reference')):missing.append('complete_live_incumbent_cost')
            groups[group].append((identity,record,item,truth,missing))
            if missing:
                for reason in missing:rejected[reason]+=1
                continue
            cohort=digest({key:identity[key] for key in ('projection_version','reference_version','behavior_fingerprint')})
            key=(cohort,model,record['case_id'],item_id)
            sample=digest({'answers':answers,'truth':truth,'thresholds':thresholds})
            if key in repeated and repeated[key]!=sample:repeat_conflicts.add(group)
            repeated[key]=sample
            cohort_cases[cohort].add((record['case_id'],item_id))
    output=[]
    for group,rows in groups.items():
        identity=rows[0][0];offline=by_model[identity['model']];missing=[];failures=[]
        cohort=digest({key:identity[key] for key in ('projection_version','reference_version','behavior_fingerprint')})
        if len(cohort_cases[cohort])<minimum_live:missing.append('200_distinct_live_comparisons')
        if not offline['held_out']['bar_met']:missing.append('held_out_offline_bar')
        if any(not value for value in identity.values()) or identity['projection_version']!=PROJECTION_VERSION:missing.append('exact_identity')
        if conflicts or group in repeat_conflicts:failures.append('conflicting_live_comparisons')
        resolved=[];seen=set()
        for _,record,item,truth,reasons in rows:
            thresholds=item.get('thresholds') or {}
            missing+=reasons
            key=(record['case_id'],item['item_id'])
            if reasons or key in seen:continue
            seen.add(key)
            answers=(item.get('response') or {}).get('answers',{})
            if all(answer(answers[q])[1]>=thresholds[q] for q in REQUIRED):resolved.append((answers,truth))
        metrics={}
        for q in REQUIRED:
            agreement=sum(answer(answers[q])[0]==label(truth[q]) for answers,truth in resolved)/len(resolved) if resolved else None
            metrics[q]={'compared':len(resolved),'agreement':agreement}
            if agreement is not None and agreement+tolerance<offline['baseline_agreement'].get(q,1):failures.append(q+':live_agreement_below_offline')
        positives=[(answers,truth) for answers,truth in resolved if truth['lane']=='needs_reply']
        if not positives:missing.append('live_needs_reply_evidence')
        elif any(answer(answers['lane'])[0]!='needs_reply' for answers,_ in positives):failures.append('live_needs_reply_recall_loss')
        qualified_outputs={q:set() for q in REQUIRED}
        for q in REQUIRED:
            for value in offline['reviewed_outputs'].get(q,[]):
                subset=[truth for answers,truth in resolved if answer(answers[q])[0]==value]
                agreement=sum(value==label(truth[q]) for truth in subset)/len(subset) if subset else None
                if agreement is not None and agreement+tolerance>=offline['output_agreement'][q][value]:
                    qualified_outputs[q].add(value)
        usable=[truth for answers,truth in resolved if all(answer(answers[q])[0] in qualified_outputs[q] for q in REQUIRED)]
        if not usable:missing.append('no_fully_qualified_live_output')
        elif not any(truth['lane']=='needs_reply' for truth in usable):missing.append('qualified_needs_reply_evidence')
        qualifications=[]
        if not missing and not failures:
            evidence_id='channel-classify:'+digest({'offline':offline['evidence_digest'],'review_ref':review_ref,
                'identity':identity,'live':[record for _,record,_,_,_ in rows]})
            for q in REQUIRED:
                for value in sorted(qualified_outputs[q]):
                    qualifications.append(dict(identity,question=q,output=value,
                        evidence_id=evidence_id,human_review_ref=review_ref))
        output.append({'identity':identity,'status':'FAIL' if failures else 'INCOMPLETE' if missing else 'PASS',
            'distinct_cohort_comparisons':len(cohort_cases[cohort]),'distinct_model_comparisons':len(seen),
            'missing':sorted(set(missing)),'failures':sorted(set(failures)),'metrics':metrics,'qualifications':qualifications})
    return {'status':'FAIL' if any(g['status']=='FAIL' for g in output) else 'PASS' if output and all(g['status']=='PASS' for g in output) else 'INCOMPLETE',
        'distinct_live_comparisons':max(map(len,cohort_cases.values()),default=0),
        'rejected':dict(rejected),'groups':output,'gate_changed':False}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gold',type=Path,required=True);parser.add_argument('--baseline',type=Path,required=True)
    parser.add_argument('--offline',type=Path,action='append',required=True)
    parser.add_argument('--records',type=Path,required=True);parser.add_argument('--review-ref',required=True)
    parser.add_argument('--socket',type=Path,required=True,help='Current engine discovery socket; no model calls')
    parser.add_argument('--reference-version',required=True,help='Current host classifier reference version')
    parser.add_argument('--principal',required=True);parser.add_argument('--workspace',required=True)
    parser.add_argument('--root',type=Path,default=Path('~/MagicianNotes').expanduser());parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    try:gold=freeze([json.loads(line) for line in args.gold.read_text().splitlines() if line.strip()])
    except (ValueError,KeyError) as error:parser.error(str(error))
    if len(gold)<150 or not any(case.get('partition')=='held_out' for case in gold):parser.error('qualification requires the reviewed 150-case packet and held-out split')
    for case in gold:
        if case.get('source') and {key:case['source'].get(key) for key in ('principal','workspace')}!={'principal':args.principal,'workspace':args.workspace}:parser.error('gold belongs to another scope')
    baseline_rows=[json.loads(line) for line in args.baseline.read_text().splitlines() if line.strip()]
    baseline={row['id']:row for row in baseline_rows}
    if len(baseline)!=len(baseline_rows):parser.error('duplicate baseline cases')
    evidence=[offline_evidence(path,gold,baseline) for path in args.offline]
    evaluation=evaluation_module('eval-channel-classify-decisions')
    policy=evaluation.call(args.socket,'/v1/operations')
    evaluation_module('eval-memory-human-review').require_current_wire(policy)
    current=next((op for op in policy['operations'] if op['name']=='channel_classify'),None)
    if not current or current['classification']['pack']!='channel_classify' or current['classification']['pack_version'] not in ('1.0.0','1.1.0'):
        parser.error('current classification policy is unbound or incompatible')
    if any(item['pack_version'] != current['classification']['pack_version'] for item in evidence):
        parser.error('offline pack differs from the current route')
    profiles=set(current['route_local'])|set(current['route_cloud'])
    if any(item['profile'] not in profiles for item in evidence):parser.error('offline model is absent from the current route')
    records=[row for row in load_records(args.records) if row.get('operation')=='channel_classify'
        and not row.get('stage') and row.get('behavior_fingerprint')==current['classification']['behavior_fingerprint']
        and row.get('reference_version')==args.reference_version
        and ((row.get('scope') or {}).get('scope') or row.get('export_scope'))=={'principal':args.principal,'workspace':args.workspace}]
    result=candidates(evidence,records,args.review_ref,current_behavior=current['classification']['behavior_fingerprint'],reference_version=args.reference_version)
    destination=private_path(args.output,args.root);destination.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    evaluation_module('eval-channel-classify-decisions').private_json(destination,result)
    print(f"{result['status']}: {result['distinct_live_comparisons']} live comparisons; gate unchanged")
    if result['status']!='PASS':raise SystemExit(1 if result['status']=='FAIL' else 2)


if __name__=='__main__':main()
