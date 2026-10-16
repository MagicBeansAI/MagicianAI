#!/usr/bin/env python3
"""Replay frozen human labels through the decision API, without memory mutations.

Raw receipts and the frozen dataset are retained. This is classification evaluation,
not owner-workflow or full production qualification. Direct API calls bypass the
Magician call ledger; costs use its shared Rust pricing helper and dated registry.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import time
import uuid

WIRE_VERSION = 5


def require_current_wire(policy):
    if policy.get('contract_version') != WIRE_VERSION:
        raise ValueError(f"Decision contract mismatch: expected {WIRE_VERSION}, got {policy.get('contract_version')}")


def normalized(answer, question):
    kind = answer.get('type')
    if kind == 'noul':
        return answer['noul'] >= 0.5
    if kind == 'choice':
        return answer['choice']
    if kind == 'score':
        rounded = math.floor(answer['score'] + 0.5)
        return rounded / 4 if question == 'importance' else rounded
    raise ValueError(f'Unsupported answer: {answer}')


def grade(case, reply):
    item = next((v for v in reply.get('items', []) if v['item_id'] == case['id']), {})
    response = item.get('response') or {}
    answers = response.get('answers', {})
    expected = case['review']['expected_labels']
    actual = {q: normalized(v, q) for q, v in answers.items()}
    eligible = set(item.get('eligible_answers', {}))
    wrong = [q for q, v in expected.items() if q in actual and actual[q] != v]
    missing = sorted(set(expected) - set(actual))
    deferred = sorted(set(expected) - eligible)
    return {'id': case['id'], 'operation': case['operation'], 'actual': actual,
            'expected': expected, 'wrong': wrong, 'missing': missing,
            'deferred': deferred, 'accepted_wrong': sorted(set(wrong) & eligible),
            'correct': not wrong and not missing, 'fully_eligible': not deferred,
            'status': item.get('status', reply.get('status'))}


def price_calls(calls, helper):
    ids = [c['call_id'] for c in calls]
    if len(set(ids)) != len(ids):
        raise ValueError('Duplicate physical receipt')
    result = subprocess.run([str(helper)], input=json.dumps(calls), text=True,
                            capture_output=True, check=True, timeout=30)
    rows = json.loads(result.stdout)
    by_id = {r['call_id']: r['pricing'] for r in rows}
    if len(by_id) != len(rows) or set(by_id) != set(ids):
        raise ValueError('Pricing helper receipt membership mismatch')
    for fact in by_id.values():
        cost = fact.get('cost_usd')
        if cost is not None and (isinstance(cost, bool) or not isinstance(cost, (int, float)) or
                                 not math.isfinite(cost) or cost < 0 or
                                 not fact.get('pricing_version') or
                                 (fact.get('cost_source') == 'local' and cost != 0) or
                                 fact.get('cost_source') not in ('computed', 'provider', 'estimated', 'local')):
            raise ValueError('Invalid pricing fact')
    return [by_id[call_id] for call_id in ids]


def grouped_cases(cases, batch_size):
    """Keep item order and only share a request when operation and context match."""
    if batch_size < 1:
        raise ValueError('batch_size must be positive')
    groups = []
    for case in cases:
        if (groups and len(groups[-1]) < batch_size and
                groups[-1][0]['operation'] == case['operation'] and
                groups[-1][0]['input']['context'] == case['input']['context']):
            groups[-1].append(case)
        else:
            groups.append([case])
    return groups


def select_cases(cases, partition):
    """Require an explicit split for a partitioned review dataset."""
    partitions = {case.get('partition') for case in cases}
    if partitions == {None}:
        if partition:
            raise ValueError('Unpartitioned dataset cannot select a partition')
        return cases
    if None in partitions or partitions != {'development', 'held_out'}:
        raise ValueError(f'Invalid review partitions: {partitions}')
    if partition is None:
        raise ValueError('Partitioned dataset requires --partition development or held_out')
    selected = [case for case in cases if case['partition'] == partition]
    if not selected:
        raise ValueError(f'Empty review partition: {partition}')
    return selected


def unique_request_rows(rows):
    seen = set()
    unique = []
    for row in rows:
        request_id = row['request']['request_id']
        if request_id not in seen:
            seen.add(request_id)
            unique.append(row)
    return unique


def require_local_calls(reply, allowed_models):
    """A strict-local replay must not silently use a hosted route."""
    for call in reply.get('model_calls', []):
        if call.get('local') is not True or call.get('model') not in allowed_models:
            raise ValueError(f"Nonlocal model call in strict-local replay: {call.get('call_id')}")
    for item in reply.get('items', []):
        response = item.get('response') or {}
        if response and response['model']['model'] not in allowed_models:
            raise ValueError(f"Nonlocal answer in strict-local replay: {item['item_id']}")


def reply_issue(request, reply, policy):
    """Reject incomplete or stale replies before calling a paired run valid."""
    if not isinstance(reply, dict):
        return 'invalid_reply_shape'
    if reply.get('status') == 'transport_error':
        return 'transport_error'
    for key, expected in [('contract_version', WIRE_VERSION),
                          ('request_id', request['request_id']),
                          ('engine_instance', policy['engine_instance']),
                          ('policy_revision', policy['policy_revision'])]:
        if reply.get(key) != expected:
            return f'{key}_mismatch'
    if reply.get('error'):
        return 'request_error'
    items = reply.get('items')
    if not isinstance(items, list):
        return 'missing_items'
    offered = [item['item_id'] for item in request['items']]
    returned = [item.get('item_id') for item in items if isinstance(item, dict)]
    if (len(returned) != len(offered) or any(not isinstance(item_id, str) for item_id in returned)
            or len(set(returned)) != len(returned) or set(returned) != set(offered)):
        return 'item_membership_mismatch'
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dataset', type=Path, default=Path('data/magician_v2/evals/memory_decisions/human-review-v1.json'))
    parser.add_argument('--socket', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--pricing-helper', type=Path, required=True,
                        help='decision_model_pricing example built by the Make target')
    parser.add_argument('--local-model', action='append', default=[], help='Allowed local fallback identity with independent thresholds')
    parser.add_argument('--locality', choices=['cloud', 'local'], default='cloud',
                        help='Decision request locality; defaults to cloud')
    parser.add_argument('--require-local-calls', action='store_true',
                        help='Fail if any physical call or answer uses a model outside --local-model')
    parser.add_argument('--local-threshold', type=float, default=0.75)
    parser.add_argument('--batch-size', type=int, default=1,
                        help='Items with identical operation and context per request')
    parser.add_argument('--partition', choices=['development', 'held_out'],
                        help='Required for a partitioned dataset; never combine held-out and development replay')
    parser.add_argument('--expected-strategy', choices=['per_item', 'shared_chunk'],
                        help='Require this discovered classification strategy')
    args = parser.parse_args()
    args.pricing_helper = args.pricing_helper.resolve(strict=True)
    if not args.pricing_helper.is_file() or not os.access(args.pricing_helper, os.X_OK):
        parser.error('--pricing-helper must be an executable file')
    assert 0 <= args.local_threshold <= 1
    if args.batch_size < 1:
        parser.error('--batch-size must be positive')
    if args.require_local_calls and (args.locality != 'local' or not args.local_model):
        parser.error('--require-local-calls needs --locality local and --local-model')
    raw = args.dataset.read_bytes()
    dataset = json.loads(raw)
    assert dataset['status'] == 'human_approved_frozen'
    assert all(c['review']['status'] == 'approved' and c['review']['expected_labels'] for c in dataset['cases'])
    selected_cases = select_cases(dataset['cases'], args.partition)
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / 'dataset.json').write_bytes(raw)

    def call(path, body=None):
        timeout = max(12, (body or {}).get('execution_budget_ms', 0) / 1000 + 2)
        cmd = ['/usr/bin/curl', '-fsS', '--max-time', str(timeout), '--unix-socket', args.socket, 'http://localhost' + path]
        if body is not None:
            cmd += ['-H', 'Content-Type: application/json', '--data-binary', '@-']
        result = json.loads(subprocess.check_output(cmd, input=None if body is None else json.dumps(body).encode()))
        if not isinstance(result, dict):
            raise ValueError('Decision API returned a non-object JSON reply')
        return result

    policy = call('/v1/operations')
    require_current_wire(policy)
    (args.output / 'policy.json').write_text(json.dumps(policy, indent=2))
    ops = {v['name']: v for v in policy['operations']}
    snapshot = dataset['evaluation_config_snapshot']
    for name, expected in snapshot['operations'].items():
        actual = ops[name]['classification']
        assert (actual['pack'], actual['pack_version']) == (expected['pack'], expected['pack_version'])
        if args.expected_strategy:
            assert actual['batch_strategy'] == args.expected_strategy, f'{name} strategy drift'
        pack = Path('data/magician_v2/decision_packs') / expected['pack'] / (expected['pack_version'] + '.json')
        assert hashlib.sha256(pack.read_bytes()).hexdigest() == snapshot['pack_sha256'][name]
    rows = []
    started = time.monotonic()
    for group in grouped_cases(selected_cases, args.batch_size):
        case = group[0]
        op = case['operation']
        request = {'contract_version': WIRE_VERSION, 'operation': op, 'state': '', 'locality': args.locality, 'mode': 'gate',
                   'request_id': uuid.uuid4().hex, 'reference_version': dataset['dataset_id'],
                   'expected_policy_revision': policy['policy_revision'], 'projection_version': 'human-review-api-v1',
                   'execution_budget_ms': (ops[op]['classification']['limits']['decision_budget_ms']
                                           + ops[op]['classification']['limits'].get('queue_budget_ms', 0)),
                   'context': case['input']['context'],
                   'items': [{'item_id': item['id'], 'state': item['input']['state']} for item in group]}
        t = time.monotonic()
        try:
            reply = call('/v1/decide', request)
        except (subprocess.CalledProcessError, ValueError) as error:
            reply = {'status': 'transport_error', 'error': str(error)}
        elapsed = round((time.monotonic() - t) * 1000)
        issue = reply_issue(request, reply, policy)
        if args.require_local_calls and issue is None:
            try:
                require_local_calls(reply, set(args.local_model))
            except ValueError as error:
                issue = str(error)
        for item in reply.get('items', []):
            if item.get('response'):
                identity = item['response']['model']
                expected_thresholds = snapshot['operations'][op]['thresholds']
                if identity['model'] in args.local_model:
                    assert identity['adapter'] in ('kev-mlx', 'kev-onnx', 'laya-mlx', 'laya-onnx'), 'Remote fallback'
                    expected_thresholds = {q: args.local_threshold for q in expected_thresholds}
                else:
                    assert identity['model'] == snapshot['model'], 'Model drift'
                assert item['thresholds'] == expected_thresholds, 'Threshold drift'
        for item in group:
            row = {'case_id': item['id'], 'elapsed_ms': elapsed, 'request': request,
                   'reply': reply, 'reply_validation_error': issue, 'grade': grade(item, reply)}
            rows.append(row)
            print(item['id'], json.dumps(row['grade']), flush=True)
        (args.output / 'results.json').write_text(json.dumps(rows, indent=2) + '\n')
        if issue:
            break  # Keep the failed request, but never mix invalid replies into a comparison.
    request_rows = unique_request_rows(rows)
    calls = [c for r in request_rows for c in r['reply'].get('model_calls', [])]
    replay_seconds = round(time.monotonic() - started, 3)
    pricing = price_calls(calls, args.pricing_helper)
    (args.output / 'pricing.json').write_text(json.dumps(
        [{'call_id': c['call_id'], 'pricing': p} for c, p in zip(calls, pricing)], indent=2) + '\n')
    costs = [p.get('cost_usd') for p in pricing]
    timings = sorted(r['elapsed_ms'] for r in request_rows)
    summary = {'dataset_sha256': hashlib.sha256(raw).hexdigest(), 'partition': args.partition,
               'cases': len(rows), 'planned_cases': len(selected_cases),
               'requests': len(request_rows), 'batch_size_limit': args.batch_size,
               'strategy': args.expected_strategy or 'not_asserted',
               'valid_comparison': len(rows) == len(selected_cases) and all(r['reply_validation_error'] is None for r in rows),
               'invalid_replies': sum(r['reply_validation_error'] is not None for r in request_rows),
               'correct_cases': sum(r['grade']['correct'] for r in rows),
               'fully_eligible_cases': sum(r['grade']['fully_eligible'] for r in rows),
               'accepted_wrong_heads': sum(len(r['grade']['accepted_wrong']) for r in rows),
               'correct_heads': sum(len(r['grade']['expected']) - len(r['grade']['wrong']) - len(r['grade']['missing']) for r in rows),
               'total_heads': sum(len(r['grade']['expected']) for r in rows),
               'wall_seconds': replay_seconds, 'p50_ms': statistics.median(timings),
               'p95_ms': timings[math.ceil(len(timings) * .95) - 1], 'model_calls': len(calls),
               'known_cost_usd': sum(v for v in costs if v is not None) if any(v is not None for v in costs) else None,
               'unpriced_calls': sum(v is None for v in costs),
               'input_tokens': sum(c.get('input_tokens') or 0 for c in calls),
               'output_tokens': sum(c.get('output_tokens') or 0 for c in calls),
               'cache_read_tokens_known_calls': sum(c.get('cache_read_tokens') is not None for c in calls),
               'cache_write_tokens_known_calls': sum(c.get('cache_write_tokens') is not None for c in calls),
               'pricing_source': 'Shared magician::magician_v2::analytics::decision_model_telemetry::pricing; dated builtin magicllm registry; exact versions in pricing.json',
               'allowed_local_models': args.local_model,
               'locality': args.locality, 'strict_local_calls': args.require_local_calls,
               'scope': 'Direct classification API; no owner mutations, Magician ledger writes or host cache; full qualification not claimed'}
    (args.output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
