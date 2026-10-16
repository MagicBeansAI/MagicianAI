#!/usr/bin/env python3
"""Evaluate a pinned Decision Engine route against reviewed classify gold.

Use a disposable engine configuration per model; this script never edits a live
route or activates a gate. Raw replies and dated pricing facts remain private.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import time
import uuid

from channel_classify_review import REQUIRED, PROJECTION_VERSION, private_path, freeze, review_counts
from decision_memory_report import usage_totals, reference_physical_calls, queue_wait_samples


def normalized(answer):
    if answer.get('type') == 'choice':
        return answer.get('choice'), answer.get('confidence')
    if answer.get('type') == 'noul':
        p = answer.get('noul')
        if not isinstance(p,(int,float)) or isinstance(p,bool) or not math.isfinite(p) or not 0 <= p <= 1:
            return None, None
        return p >= .5, max(p,1-p)
    return None, None


def fit_threshold(rows, question):
    """Compare each accepted subset with the SAME reviewed LLM baseline subset.

Missing human or baseline labels never count as correct or as a free fallback.
No useful threshold is returned without coverage and the recall comparison.
"""
    comparable=[]
    for row in rows:
        expected=row.get('expected',{})
        baseline=row.get('baseline',{})
        label,confidence=normalized(row.get('answers',{}).get(question,{}))
        if question in expected and question in baseline and label is not None and isinstance(confidence,(int,float)) and not isinstance(confidence,bool) and math.isfinite(confidence) and 0 <= confidence <= 1:
            comparable.append((row,label,confidence))
    for threshold in sorted({confidence for _,_,confidence in comparable}):
        accepted=[(row,label) for row,label,confidence in comparable if confidence >= threshold]
        correct=sum(label==row['expected'][question] for row,label in accepted)
        baseline_correct=sum(row['baseline'][question]==row['expected'][question] for row,_ in accepted)
        positives=[(row,label) for row,label in accepted if row['expected'].get('lane')=='needs_reply']
        # A lane gate with no needs-reply evidence cannot establish recall.
        if question=='lane' and not positives:
            continue
        recall=sum(label=='needs_reply' for _,label in positives) if question=='lane' else 0
        baseline_recall=sum(row['baseline'].get('lane')=='needs_reply' for row,_ in positives) if question=='lane' else 0
        if correct >= baseline_correct and recall >= baseline_recall:
            return {'threshold':threshold,'accepted':len(accepted),'comparable':len(comparable),
                'coverage':len(accepted)/len(rows) if rows else 0,'accuracy':correct/len(accepted),
                'baseline_accuracy':baseline_correct/len(accepted),
                'needs_reply_recall':recall/len(positives) if positives else None,
                'baseline_needs_reply_recall':baseline_recall/len(positives) if positives else None}
    return {'threshold':None,'accepted':0,'comparable':len(comparable),'coverage':0,'reason':'insufficient_or_inferior_evidence'}


def accepted(row, thresholds):
    if len(thresholds) != len(REQUIRED):
        return False
    for q in REQUIRED:
        label, confidence = normalized(row.get('answers', {}).get(q, {}))
        if label is None or not isinstance(confidence, (int,float)) or isinstance(confidence,bool) or not math.isfinite(confidence) or not 0 <= confidence <= 1 or confidence < thresholds[q]:
            return False
    return True


def validate_thresholds(rows, thresholds):
    """Apply frozen thresholds to unseen rows; never fit on this subset."""
    resolved = [row for row in rows if accepted(row, thresholds)]
    questions = {}
    for q in REQUIRED:
        compared = [r for r in resolved if q in r.get('baseline', {}) and q in r.get('expected', {})]
        correct = sum(normalized(r['answers'][q])[0] == r['expected'][q] for r in compared)
        baseline_correct = sum(r['baseline'][q] == r['expected'][q] for r in compared)
        questions[q] = {'compared':len(compared), 'accuracy':correct/len(compared) if compared else None,
                        'baseline_accuracy':baseline_correct/len(compared) if compared else None}
    positives = [r for r in resolved if r['expected'].get('lane') == 'needs_reply' and 'lane' in r.get('baseline',{})]
    recall = sum(normalized(r['answers']['lane'])[0] == 'needs_reply' for r in positives)/len(positives) if positives else None
    base_recall = sum(r['baseline']['lane'] == 'needs_reply' for r in positives)/len(positives) if positives else None
    complete = bool(rows and resolved and positives) and all(v['compared'] == len(resolved) for v in questions.values())
    return {'cases':len(rows), 'resolved':len(resolved), 'coverage':len(resolved)/len(rows) if rows else 0,
            'questions':questions, 'needs_reply_cases':len(positives), 'needs_reply_recall':recall,
            'baseline_needs_reply_recall':base_recall,
            'bar_met':complete and recall >= base_recall and all(v['accuracy'] >= v['baseline_accuracy'] for v in questions.values())}


def private_json(path, value):
    with path.open('x', encoding='utf-8') as stream:
        os.chmod(path,0o600)
        json.dump(value, stream, indent=2, allow_nan=False)
        stream.write('\n')


def call(socket,path,body=None):
    timeout=125 if body else 10
    command=['/usr/bin/curl','--silent','--show-error','--fail','--max-time',str(timeout),'--unix-socket',str(socket),'http://localhost'+path]
    if body is not None:command+=['-H','Content-Type: application/json','--data-binary','@-']
    return json.loads(subprocess.check_output(command,input=json.dumps(body).encode() if body else None))


def input_fingerprint(case):
    return hashlib.sha256(json.dumps(case['input'],sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()


def validate_cases(cases, predictions_only=False, diagnostic=False):
    if not cases or len({case['id'] for case in cases}) != len(cases):
        raise ValueError('cases must have unique, nonempty identities')
    for case in cases:
        if not case['id'] or case.get('projection_version') != PROJECTION_VERSION or not isinstance(case.get('input',{}).get('state'),dict):
            raise ValueError('incomplete or incompatible classify projection')
        if case.get('partition','development') not in ('development','held_out'):
            raise ValueError('unknown dataset partition')
        status=case.get('review',{}).get('status')
        if predictions_only:
            if status not in ('pending','approved'):
                raise ValueError('prediction collection requires a private owner-review packet')
        elif status == 'synthetic' and diagnostic:
            # Validate the synthetic vocabulary without recording human approval.
            freeze([dict(case,review={'status':'approved'})])
        else:
            freeze([case])


def replay_rows(path, cases, policy, helper, model, operation='channel_classify'):
    """Reuse physical receipts only for the exact frozen inputs and wire identity."""
    rows=[json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    indexed={row['id']:row for row in rows}
    if len(indexed)!=len(rows) or set(indexed)!={case['id'] for case in cases}:
        raise ValueError('replay needs exactly one receipt row per frozen case')
    result=[]
    for case in cases:
        row=indexed[case['id']]
        request,reply=row.get('request',{}),row.get('reply',{})
        if request.get('operation')!=operation or request.get('projection_version')!=PROJECTION_VERSION:
            raise ValueError('replay belongs to another operation or projection')
        if request.get('expected_policy_revision')!=policy.get('policy_revision'):
            raise ValueError('replay request used another captured policy')
        if row.get('input_sha256')!=input_fingerprint(case):
            raise ValueError('replay input differs from the approved projection')
        offered=request.get('items',[])
        if len(offered)!=1 or offered[0].get('item_id')!=case['id'] or offered[0].get('state')!=case['input']['state'] or request.get('context')!=case['input'].get('context'):
            raise ValueError('replay request differs from the frozen input')
        issue=helper.reply_issue(request,reply,policy)
        if row.get('issue') or issue:
            raise ValueError('failed or stale replay cannot supply calibration evidence')
        item=next((item for item in reply.get('items',[]) if item.get('item_id')==case['id']),{})
        response=item.get('response') or {}
        if response and response.get('model',{}).get('model')!=model:
            raise ValueError('replay belongs to another physical model')
        binding=next((op for op in policy.get('operations',[]) if op['name']==operation),{}).get('classification',{})
        if response and (response.get('pack_id')!=binding.get('pack') or response.get('pack_version')!=binding.get('pack_version')):
            raise ValueError('replay belongs to another pack version')
        if row.get('partition','development')!=case.get('partition','development'):
            raise ValueError('replay cannot move a held-out case into training')
        result.append(dict(row,expected=case.get('expected',{}),answers=response.get('answers',{})))
    return result


def dated_replay_prices(path, calls):
    """Reusing evidence must not reprice historical provider attempts."""
    original_rows=[json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    original_calls=[call for row in original_rows for call in row['reply'].get('model_calls',[])]
    prices=json.loads((path.parent/'pricing.json').read_text())
    original_ids=[call['call_id'] for call in original_calls]
    ids=[call['call_id'] for call in calls]
    if len(prices)!=len(original_calls) or len(set(original_ids))!=len(original_ids) or len(set(ids))!=len(ids) or set(ids)!=set(original_ids):
        raise ValueError('dated prices do not match the replay physical receipts')
    indexed=dict(zip(original_ids,prices))
    for price in prices:
        cost=price.get('cost_usd')
        if cost is not None and (not isinstance(cost,(int,float)) or isinstance(cost,bool) or not math.isfinite(cost) or cost<0 or not price.get('pricing_version') or price.get('cost_source') not in ('computed','provider','estimated','local') or (price.get('cost_source')=='local' and cost!=0)):
            raise ValueError('invalid dated replay price')
    return [indexed[call_id] for call_id in ids]


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gold',type=Path,required=True);parser.add_argument('--baseline',type=Path)
    parser.add_argument('--diagnostic',action='store_true',help='Allow synthetic smoke tests without a baseline; cannot fit qualifying thresholds')
    parser.add_argument('--predictions-only',action='store_true',help='Collect pending private review cases without fitting thresholds or claiming gold')
    parser.add_argument('--replay',type=Path,help='Reuse exact frozen rows.jsonl receipts after review; makes no model calls')
    parser.add_argument('--socket',type=Path);parser.add_argument('--operation',default='channel_classify')
    parser.add_argument('--profile',required=True,help='Decision Engine model profile key for threshold suggestions')
    parser.add_argument('--model',required=True,help='Exact response model id required for this single-model evaluation')
    parser.add_argument('--pricing-helper',type=Path)
    parser.add_argument('--root',type=Path,default=Path('~/MagicianNotes').expanduser());parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--locality',choices=['local','cloud'],default='cloud')
    args=parser.parse_args()
    gold=[json.loads(line) for line in args.gold.read_text().splitlines() if line.strip()]
    if args.predictions_only and args.diagnostic:parser.error('pending review collection and synthetic diagnostics are separate modes')
    try:validate_cases(gold,args.predictions_only,args.diagnostic)
    except (ValueError,KeyError) as error:parser.error(str(error))
    if not args.baseline and not (args.diagnostic or args.predictions_only):parser.error('a real matched LLM baseline is required outside diagnostic mode')
    if not args.socket and not args.replay:parser.error('--socket is required when collecting model calls')
    if not args.pricing_helper and not args.replay:parser.error('--pricing-helper is required when collecting physical receipts')
    baseline_rows=[json.loads(line) for line in args.baseline.read_text().splitlines() if line.strip()] if args.baseline else []
    baseline={r['id']:r for r in baseline_rows}
    for case in gold:
        fingerprint=input_fingerprint(case)
        if args.baseline and baseline.get(case['id'],{}).get('input_sha256') != fingerprint:
            parser.error('baseline input is missing or differs from the frozen gold projection')
    if len(baseline)!=len(baseline_rows):parser.error('duplicate baseline identities')
    spec=importlib.util.spec_from_file_location('memory_eval',Path(__file__).with_name('eval-memory-human-review.py'))
    helper=importlib.util.module_from_spec(spec);spec.loader.exec_module(helper)
    policy=json.loads((args.replay.parent/'policy.json').read_text()) if args.replay else call(args.socket,'/v1/operations')
    helper.require_current_wire(policy)
    operation=next((o for o in policy['operations'] if o['name']==args.operation),None)
    if operation is None:parser.error('requested operation is unbound')
    output=private_path(args.output,args.root);output.mkdir(parents=True,exist_ok=False,mode=0o700)
    private_json(output/'policy.json',policy)
    rows=[];latencies=[];all_calls=[];issues=[]
    journal=(output/'rows.jsonl').open('x',encoding='utf-8');os.chmod(journal.name,0o600)
    if args.replay:
        try:rows=replay_rows(args.replay,gold,policy,helper,args.model,args.operation)
        except (ValueError,KeyError) as error:parser.error(str(error))
        for row in rows:
            row['baseline']=baseline.get(row['id'],{}).get('labels',{})
            journal.write(json.dumps(row,allow_nan=False)+'\n')
            latencies.append(row['latency_ms']);all_calls+=row['reply'].get('model_calls',[])
        journal.flush();os.fsync(journal.fileno())
    for case in ([] if args.replay else gold):
        # Single-item batches avoid unqualified shared-chunk transforms.
        request={'contract_version':5,'operation':args.operation,'state':'','locality':args.locality,
            'mode':'shadow','request_id':uuid.uuid4().hex,'reference_version':'channel-classify-offline-v1',
            'expected_policy_revision':policy['policy_revision'],'projection_version':PROJECTION_VERSION,
            'execution_budget_ms':operation['classification']['limits']['shadow_budget_ms'],
            'context':case['input']['context'],'items':[{'item_id':case['id'],'state':case['input']['state']}]}
        started=time.monotonic()
        try:
            reply=call(args.socket,'/v1/decide',request)
            issue=helper.reply_issue(request,reply,policy)
            # Wire v5 supplies physical receipts but no top-level accounting
            # flag. Honor an explicit incomplete flag on a future reply.
            if not issue and 'accounting_complete' in reply and reply['accounting_complete'] is not True:
                issue='incomplete_physical_accounting'
        except (subprocess.SubprocessError, ValueError) as error:
            reply={};issue=type(error).__name__
        latency=round((time.monotonic()-started)*1000);latencies.append(latency)
        item=next((r for r in reply.get('items',[]) if r.get('item_id')==case['id']),{})
        response=item.get('response') or {}
        if response and response['model']['model'] != args.model:issue='unexpected_model; use_an_isolated_pinned_route'
        if response and (response.get('pack_id')!=operation['classification']['pack'] or response.get('pack_version')!=operation['classification']['pack_version']):issue='unexpected_pack_version'
        physical=reply.get('model_calls',[]);all_calls+=physical
        if issue:issues.append({'id':case['id'],'issue':issue})
        row={'id':case['id'],'input_sha256':input_fingerprint(case),'expected':{} if args.predictions_only else case['expected'],'baseline':baseline.get(case['id'],{}).get('labels',{}),
            'answers':response.get('answers',{}) if not issue else {},'partition':case.get('partition','development'),
            'latency_ms':latency,'status':item.get('status','unknown'),'issue':issue,'request':request,'reply':reply}
        rows.append(row)
        journal.write(json.dumps(row,allow_nan=False)+'\n');journal.flush();os.fsync(journal.fileno())
        if issue:
            # Preserve attempted-call receipts, then stop an invalid run.
            break
    journal.close()
    pricing=dated_replay_prices(args.replay,all_calls) if args.replay else helper.price_calls(all_calls,args.pricing_helper)
    fits={q:fit_threshold([r for r in rows if r['partition']=='development'],q) for q in REQUIRED} if not args.predictions_only else {}
    costs=[p.get('cost_usd') for p in pricing]
    cost_complete=bool(all_calls) and not issues and all(isinstance(c,(int,float)) and not isinstance(c,bool) and math.isfinite(c) and c>=0 for c in costs)
    thresholds={q:v['threshold'] for q,v in fits.items() if v['threshold'] is not None}
    resolved=sum(accepted(r,thresholds) for r in rows)
    validation=validate_thresholds([r for r in rows if r['partition']=='held_out'],thresholds) if not args.predictions_only else None
    completed_ids={row['id'] for row in rows}
    baseline_calls=[c for r in baseline_rows if r['id'] in completed_ids for c in reference_physical_calls(r.get('reference'))]
    baseline_costs=[c.get('cost_usd') for c in baseline_calls]
    baseline_complete=bool(baseline_calls) and all(isinstance(c,(int,float)) and not isinstance(c,bool) and math.isfinite(c) and c>=0 for c in baseline_costs)
    report={'model':args.model,'cases':len(rows),**review_counts(gold),
        'diagnostic':args.diagnostic,
        'predictions_only':args.predictions_only,'reused_receipts':bool(args.replay),
        'ungated_questions':{q:{'answered':sum(normalized(r['answers'].get(q,{}))[0] is not None for r in rows),
            'correct':sum(normalized(r['answers'].get(q,{}))[0] == r['expected'][q] for r in rows) if not args.predictions_only else None} for q in REQUIRED},
        'questions':fits,'resolved':resolved if not args.predictions_only else None,'coverage':resolved/len(rows) if not args.predictions_only else None,'thresholds_by_model':{args.profile:thresholds} if not args.predictions_only else {},
        'p50_ms':statistics.median(latencies),'p95_ms':sorted(latencies)[math.ceil(.95*len(latencies))-1],
        'cost_usd':sum(costs) if cost_complete else None,'cost_per_thread_usd':sum(costs)/len(rows) if cost_complete else None,
        'physical_calls':len(all_calls),'new_model_calls':0 if args.replay else len(all_calls),'new_cost_usd':0.0 if args.replay else sum(costs) if cost_complete else None,'pricing_complete':cost_complete,'activation':'unchanged',
        'decision_usage':usage_totals(all_calls),'baseline_usage':usage_totals(baseline_calls),
        'queue_wait_ms':queue_wait_samples(all_calls), 'issues':issues,
        'baseline_cost_usd':sum(baseline_costs) if baseline_complete else None,
        'held_out':validation,
        'held_out_cases':sum(r['partition']=='held_out' for r in rows),
        'qualification':'pending_human_review; no_thresholds_fitted' if args.predictions_only else 'offline_only; requires independent live shadow evidence'}
    private_json(output/'pricing.json',pricing)
    private_json(output/'report.json',report)
    print(json.dumps({k:v for k,v in report.items() if k not in ('questions','thresholds_by_model')}))
    if issues:raise SystemExit(1)

if __name__=='__main__':main()
