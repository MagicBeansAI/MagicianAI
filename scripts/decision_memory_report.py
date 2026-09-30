#!/usr/bin/env python3
"""Content-free qualification report. Never enables a gate or invents missing cost."""
from __future__ import annotations
import argparse
import hashlib
import json
import math
from collections import Counter, defaultdict
from pathlib import Path

DESTRUCTIVE = {
    ('memory_utility_review', 'utility', 'harmful'),
    ('memory_conflict_review', 'resolution', 'replace_existing'),
    ('memory_conflict_review', 'resolution', 'keep_existing'),
    ('memory_lifecycle_relation', 'relation', 'supersede'),
    ('procedure_feedback', 'deprecate', 'true'),
    ('procedure_feedback', 'deprecation_recommended', 'true'),
    ('procedure_feedback', 'verdict', 'harmful'),
}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False)


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def label(value):
    if isinstance(value, bool):
        return 'true' if value else 'false'
    if isinstance(value, str):
        return value
    if finite(value):
        return str(int(value)) if float(value).is_integer() else str(value)
    return None


def answer(value, projection=None):
    kind = value.get('type')
    if kind == 'noul' and finite(value.get('noul')) and 0 <= value['noul'] <= 1:
        p = value['noul']
        return ('true' if p >= .5 else 'false'), max(p, 1-p)
    if kind == 'choice' and isinstance(value.get('choice'), str) and finite(value.get('confidence')) and 0 <= value['confidence'] <= 1:
        return value['choice'], value['confidence']
    if kind == 'score' and finite(value.get('score')) and finite(value.get('confidence')) and 0 <= value['confidence'] <= 1:
        if projection == 'evidence_promote_v1_importance_quarters' and 0 <= value['score'] <= 4:
            return label(math.floor(value['score'] + .5) / 4), value['confidence']
        if projection == 'episode_quality_v1_score_round_0_6' and 0 <= value['score'] <= 6:
            return str(math.floor(value['score'] + .5)), value['confidence']
        return None, None
    return None, None


def wilson(success, count):
    if not count:
        return None
    z = 1.959963984540054
    p = success/count
    denominator = 1 + z*z/count
    midpoint = (p+z*z/(2*count))/denominator
    margin = z*math.sqrt(p*(1-p)/count + z*z/(4*count*count))/denominator
    return [max(0, midpoint-margin), min(1, midpoint+margin)]


def percentile(values, quantile=.95):
    return sorted(values)[max(0, math.ceil(len(values)*quantile)-1)] if values else None


def queue_wait_samples(calls):
    """One wait per model admission; retries repeat the same wait metadata."""
    waits = {}
    for call in calls:
        value = call.get('queue_wait_ms')
        if not finite(value) or value < 0:
            continue
        key = (call.get('provider'), call.get('model'),
               call.get('retry_group_id') or call.get('call_id'))
        waits[key] = max(waits.get(key, 0), value)
    return list(waits.values())


def load_records(path):
    """Accept exported event JSONL or a fixture replay's JSON array."""
    text = Path(path).read_text()
    rows = json.loads(text) if text.lstrip().startswith('[') else [json.loads(line) for line in text.splitlines() if line.strip()]
    for row in rows:
        envelope = row
        row = row.get('record', row)
        if 'record' in envelope:
            row = dict(row, export_scope={'principal': envelope.get('principal'), 'workspace': envelope.get('workspace')})
        if row.get('schema_version') == 1 and 'comparison_id' in row:
            yield row


def reconcile_ledger(records, ledger, principal, workspace):
    """Join scoped canonical llm_calls/provider_attempts exports, without repricing.

    A final logical call price cannot prove the price of failed prior attempts.
    Multi-attempt calls require every terminal attempt and its dated price.
    Missing/contradictory rows remain unknown; payload/content columns are ignored.
    """
    import copy
    records = copy.deepcopy(list(records))
    wanted = {'principal': principal, 'workspace': workspace}
    def scoped(row):
        return {k: row.get(k) for k in wanted} == wanted
    def index(rows, key):
        result, conflicts = {}, set()
        for row in rows:
            if not scoped(row) or not row.get(key):
                continue
            identity = row[key]
            if identity in result and canonical(result[identity]) != canonical(row):
                conflicts.add(identity)
            result[identity] = row
        return result, conflicts
    calls, conflicts = index(ledger.get('calls', []), 'llm_call_id')
    attempts, attempt_conflicts = index(ledger.get('attempts', []), 'provider_attempt_id')
    by_call = defaultdict(list)
    for attempt in attempts.values():
        if attempt['provider_attempt_id'] in attempt_conflicts:
            conflicts.add(attempt.get('llm_call_id'))
        by_call[attempt.get('llm_call_id')].append(attempt)
    def facts(call_id, expected):
        call = calls.get(call_id)
        if not call or call_id in conflicts:
            return None
        if any(expected.get(k) and call.get(k) != expected[k] for k in ('model', 'provider')):
            return None
        count = call.get('provider_attempt_count')
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            return None
        expected_count = (expected.get('receipt') or {}).get('provider_attempt_count')
        if expected_count is not None and expected_count != count:
            return None
        if call.get('response_reused'):
            return dict(cost_usd=0.0, pricing_version=call.get('pricing_version'), attempts_complete=True)
        rows = [call]
        if count > 1:
            rows = by_call.get(call_id, [])
            indices = {r.get('provider_attempt_index') for r in rows}
            if len(rows) != count or len(indices) != count or not all(isinstance(i, int) for i in indices) or min(indices, default=-1) not in (0, 1) or sorted(indices) != list(range(min(indices, default=0), min(indices, default=0)+count)):
                return None
        if count < 1 or not all(r.get('pricing_version') and r.get('cost_source') in ('provider', 'computed', 'estimated', 'local')
            and finite(r.get('cost_usd')) and r['cost_usd'] >= 0
            and (r.get('completed_at_ms') is not None or r.get('success') is not None or r.get('transport_success') is not None) for r in rows):
            return None
        result = dict(cost_usd=sum(r['cost_usd'] for r in rows),
            pricing_version=','.join(sorted({r['pricing_version'] for r in rows})), attempts_complete=True)
        for out, source in [('input_tokens','input_tokens'), ('output_tokens','output_tokens'),
                            ('cache_read_tokens','cache_read_tokens'), ('cache_write_tokens','cache_creation_tokens')]:
            result[out] = sum(r[source] for r in rows) if all(finite(r.get(source)) and r[source] >= 0 for r in rows) else None
        return result
    for record in records:
        record['ledger_reconciled'] = True
        for decision in record.get('calls', []):
            f = facts(decision['call_id'], decision)
            if f is None:
                record['accounting_complete'] = False
                record['ledger_reconciled'] = False
            record.setdefault('pricing', {})[decision['call_id']] = f or {'cost_usd': None}
        references = []
        if (record.get('reference') or {}).get('call'):
            references.append(record['reference']['call'])
        if record.get('stage') == 'completion' and record.get('text_call'):
            references.append(record['text_call'])
        for reference in references:
            receipt = reference.get('receipt') or {}
            call_id = receipt.get('context', {}).get('llm_call_id')
            f = facts(call_id, reference)
            reference.update(f or {'cost_usd':None, 'attempts_complete':False})
            if f is None:
                record['ledger_reconciled'] = False
    return records


def usage_totals(rows):
    result = {'receipts': len(rows), 'physical_calls': sum((r.get('receipt') or {}).get('provider_attempt_count', 1) for r in rows)}
    for field in ('input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_write_tokens'):
        known = [r[field] for r in rows if finite(r.get(field)) and r[field] >= 0]
        result[field] = {'reported_sum': sum(known) if known else None,
            'reported_calls':len(known), 'complete':len(known)==len(rows) and all(r.get('attempts_complete', True) for r in rows)}
    input_total = result['input_tokens']; cached = result['cache_read_tokens']
    result['cache_read_fraction'] = cached['reported_sum']/input_total['reported_sum'] if input_total['complete'] and cached['complete'] and input_total['reported_sum'] else None
    return result


def reference_usage(records, text=False):
    unique = {}
    for record in records:
        call = (record.get('completion') or {}).get('text_call') if text else (record.get('reference') or {}).get('call')
        if not call:
            continue
        receipt = call.get('receipt') or {}
        if receipt.get('response_reused'):
            continue
        call_id = receipt.get('context', {}).get('llm_call_id')
        # Unknown identities cannot silently coalesce into one physical call.
        unique[call_id or record['comparison_id']] = call
    return usage_totals(list(unique.values()))


def report(records, reviews=None, paired=None):
    reviews, paired = reviews or {}, paired or {}
    invocations = [r for r in records if r.get('stage') == 'invocation']
    records = [r for r in records if r.get('stage') != 'invocation']
    # Completion is emitted after required prose and can arrive before the
    # primary row in a paged export. Join before de-duplicating comparisons.
    primary, completions, reference_stages, conflicts = [], {}, {}, set()
    for record in records:
        if record.get('stage') == 'completion':
            old = completions.get(record.get('comparison_id'))
            if old is not None and old != record:
                conflicts.add(record.get('comparison_id'))
            completions[record.get('comparison_id')] = record
        elif record.get('stage') == 'reference':
            key = record.get('comparison_id')
            old = reference_stages.get(key)
            # Queueing and the first inference timeout are provisional. The
            # physical call may finish later with a receipt but no usable
            # comparison labels. Only distinct terminal stages conflict.
            provisional = ('queued', 'inference_expired')
            if old is not None and old.get('status') not in provisional and record.get('status') not in provisional and old != record:
                conflicts.add(key)
            if old is None or (old.get('status') in provisional and record.get('status') not in provisional) or (old.get('status') == 'queued' and record.get('status') == 'inference_expired'):
                reference_stages[key] = record
        else:
            primary.append(record)
    records = []
    for record in primary:
        reference_stage = reference_stages.get(record.get('comparison_id'))
        row = dict(record, completion=completions.get(record.get('comparison_id')),
                   reference_stage=reference_stage)
        if reference_stage:
            row['reference_selected'] = True
            row['reference_status'] = reference_stage.get('status')
            row['reference_attempted'] = reference_stage.get('reference_attempted', False)
            row['reference'] = reference_stage.get('reference')
        records.append(row)
    groups = {}
    cohorts = defaultdict(dict)
    seen_comparisons = {}
    skipped_cache = 0
    incomplete_records = 0
    for record in records:
        if record.get('application_cache_hit'):
            skipped_cache += 1
            continue
        comparison = record.get('comparison_id')
        if not comparison:
            continue
        if comparison in seen_comparisons:
            if seen_comparisons[comparison] != record:
                conflicts.add(comparison)
            continue
        seen_comparisons[comparison] = record
        # Observation-only policy changes (sampling rate/limits) have their
        # own revision and cannot revoke or fragment decision qualifications.
        cohort = digest({key: record.get(key) for key in ('operation', 'projection_version', 'reference_version', 'behavior_fingerprint')})
        offered_ids = record.get('offered_item_ids') or [i.get('item_id') for i in record.get('items', [])]
        if not offered_ids:
            offered_ids = [f'unanswered-{i}' for i in range(record.get('offered', 0))]
        for item_id in offered_ids:
            cohorts[cohort].setdefault((record.get('case_id'), item_id), None)
        calls = record.get('calls', [])
        reference = record.get('reference') or {}
        reference_labels = reference.get('labels', {})
        if not record.get('items'):
            incomplete_records += 1
        for item in record.get('items', []):
            response = item.get('response') or {}
            matching = [c for c in calls if item.get('item_id') in c.get('item_ids', []) and c.get('model') == response.get('model', {}).get('model') and c.get('status') == 'succeeded']
            provider = matching[-1]['provider'] if matching else None
            identity = {
                'operation': record.get('operation'), 'projection_version': record.get('projection_version'),
                'reference_version': record.get('reference_version'), 'behavior_fingerprint': record.get('behavior_fingerprint'),
                'adapter': response.get('model', {}).get('adapter'), 'model': response.get('model', {}).get('model'),
                'provider': provider, 'pack': response.get('pack_id'), 'pack_version': response.get('pack_version'),
                'threshold_fingerprint': (record.get('threshold_fingerprints') or {}).get(item.get('item_id')),
                'reference_model': (reference.get('call') or {}).get('model'),
                'reference_provider': (reference.get('call') or {}).get('provider'),
                'required_questions': record.get('required_questions') or [],
            }
            group_id = digest(identity)
            group = groups.setdefault(group_id, {'identity': identity, 'cohort': cohort, 'cases': {}, 'records': {}, 'calls': {}, 'prices': {}})
            group['records'][comparison] = record
            for call in calls:
                old = group['calls'].get(call['call_id'])
                if old is not None and old != call:
                    group['receipt_conflict'] = True
                group['calls'][call['call_id']] = call
            for call_id, price in record.get('pricing', {}).items():
                if call_id in group['prices'] and group['prices'][call_id] != price:
                    group['receipt_conflict'] = True
                group['prices'][call_id] = price
            key = (record.get('case_id'), item.get('item_id'))
            # Replays/repeated cache misses never increase held-out sample size.
            group['cases'].setdefault(key, (item, reference_labels.get(item.get('item_id'))))
    output = []
    for group_id, group in sorted(groups.items()):
        errors, missing = [], []
        identity = group['identity']
        if any(v in (None, '') for v in identity.values()):
            missing.append('exact_identity')
        if group.get('receipt_conflict'):
            errors.append('conflicting_receipts')
        if conflicts.intersection(group['records']):
            errors.append('conflicting_comparison_records')
        cases = group['cases']
        reviewed = reviews.get(group_id, {})
        held_out = set(reviewed.get('held_out_cases', []))
        human_cases = reviewed.get('cases', {})
        def human_label(case, question):
            if ':'.join(str(x) for x in case) not in held_out:
                return None
            human = human_cases.get(':'.join(str(x) for x in (*case, question)))
            if not isinstance(human, dict) or human.get('held_out') is not True:
                return None
            return label(human.get('expected')) or None
        offered_count = len(cohorts[group['cohort']])
        questions = defaultdict(lambda: {'offered': offered_count, 'answered': 0, 'eligible': 0, 'authorized': 0, 'authorization_known': 0, 'reference': 0, 'compared': 0, 'agree': 0, 'labels': Counter(), 'matrix': Counter(), 'disagreements': []})
        held_out_matrices = defaultdict(Counter)
        eligible_cases = defaultdict(set)
        for case, (item, references) in cases.items():
            response = item.get('response') or {}
            for question, value in response.get('answers', {}).items():
                if identity['required_questions'] and question not in identity['required_questions']:
                    continue
                q = questions[question]
                predicted, confidence = answer(value, identity['projection_version'])
                if value.get('type') == 'score' and predicted is None:
                    missing.append(f'{question}:valid_reviewed_score_mapping')
                if predicted is None:
                    continue
                q['answered'] += 1
                threshold = (item.get('thresholds') or {}).get(question)
                eligible = finite(threshold) and confidence >= threshold
                q['eligible'] += int(eligible)
                if isinstance(item.get('eligible_answers'), dict):
                    q['authorization_known'] += 1
                    q['authorized'] += int(question in item['eligible_answers'])
                truth = label((references or {}).get(question))
                q['reference'] += int(truth is not None)
                if truth is None or not eligible:
                    continue
                q['compared'] += 1
                q['labels'][truth] += 1
                q['matrix'][(truth, predicted)] += 1
                q['agree'] += int(truth == predicted)
                if ':'.join(str(x) for x in case) in held_out:
                    held_out_matrices[question][(truth, predicted)] += 1
                    eligible_cases[question].add(case)
                if truth != predicted:
                    q['disagreements'].append(case)
        if not reviewed.get('review_ref') or not reviewed.get('reviewer'):
            missing.append('human_review')
        questions_out = {}
        for name, q in sorted(questions.items()):
            held_matrix = held_out_matrices[name]
            held_labels = Counter()
            for (truth, _), count in held_matrix.items():
                held_labels[truth] += count
            held_compared = sum(held_labels.values())
            held_agree = sum(n for (truth, prediction), n in held_matrix.items() if truth == prediction)
            if not reviewed.get('dataset_id') or sum(':'.join(str(x) for x in case) in held_out for case in cases) < 200:
                missing.append(f'{name}:held_out_dataset_manifest')
            if held_compared < 200:
                missing.append(f'{name}:200_distinct_eligible_held_out_cases')
            if len(held_labels) < 2 or any(n < 50 for n in held_labels.values()):
                missing.append(f'{name}:balanced_reference_labels')
            agreement = q['agree']/q['compared'] if q['compared'] else None
            if agreement is not None and agreement < .9:
                errors.append(f'{name}:agreement_below_90_percent')
            held_agreement = held_agree/held_compared if held_compared else None
            if held_agreement is not None and held_agreement < .9:
                errors.append(f'{name}:held_out_agreement_below_90_percent')
            disagreements = [case for case in q['disagreements'] if case in eligible_cases[name]]
            disagreement_reviewed = sum(human_label(case, name) is not None for case in disagreements)
            if disagreement_reviewed < min(20, len(disagreements)):
                missing.append(f'{name}:disagreement_adjudication')
            reviewed_labels = {human_label(case, name) for case in eligible_cases[name]}
            if not set(held_labels).issubset(reviewed_labels):
                missing.append(f'{name}:human_positive_negative_samples')
            per_label = {}
            for outcome in sorted(set(q['labels']) | {p for _, p in q['matrix']}):
                tp = q['matrix'][(outcome, outcome)]
                predicted = sum(n for (_, p), n in q['matrix'].items() if p == outcome)
                positives = q['labels'][outcome]
                per_label[outcome] = {'reference': positives, 'predicted': predicted,
                    'precision': tp/predicted if predicted else None, 'recall': tp/positives if positives else None,
                    'precision_95ci': wilson(tp, predicted), 'recall_95ci': wilson(tp, positives)}
                # Aggregate agreement can hide a systematically missed minority.
                if positives and (tp/positives < .9 or (predicted and tp/predicted < .9)):
                    errors.append(f'{name}/{outcome}:precision_or_recall_below_90_percent')
                held_tp = held_matrix[(outcome, outcome)]
                held_predicted = sum(n for (_, p), n in held_matrix.items() if p == outcome)
                held_positives = held_labels[outcome]
                per_label[outcome]['held_out'] = {
                    'reference': held_positives, 'predicted': held_predicted,
                    'precision': held_tp/held_predicted if held_predicted else None,
                    'recall': held_tp/held_positives if held_positives else None,
                    'precision_95ci': wilson(held_tp, held_predicted),
                    'recall_95ci': wilson(held_tp, held_positives)}
                if (held_positives and held_tp/held_positives < .9) or (held_predicted and held_tp/held_predicted < .9):
                    errors.append(f'{name}/{outcome}:held_out_precision_or_recall_below_90_percent')
                if (identity['operation'], name, outcome) in DESTRUCTIVE:
                    checked = []
                    for case, (item, _) in cases.items():
                        pred, _ = answer((item.get('response') or {}).get('answers', {}).get(name, {}), identity['projection_version'])
                        expected = human_label(case, name)
                        if pred == outcome and case in eligible_cases[name] and expected is not None:
                            checked.append(expected == outcome)
                    if len(checked) < 100:
                        missing.append(f'{name}/{outcome}:100_human_checked_destructive_proposals')
                    if checked and not all(checked):
                        errors.append(f'{name}/{outcome}:incorrect_destructive_proposal')
                    if positives and tp/positives < .95 or predicted and tp/predicted < .95:
                        errors.append(f'{name}/{outcome}:label_agreement_below_95_percent')
                    if (held_positives and held_tp/held_positives < .95) or (held_predicted and held_tp/held_predicted < .95):
                        errors.append(f'{name}/{outcome}:held_out_label_agreement_below_95_percent')
            questions_out[name] = {**{k:v for k,v in q.items() if k not in ('matrix','labels','disagreements')},
                'agreement': agreement, 'agreement_95ci': wilson(q['agree'], q['compared']),
                'held_out_compared': held_compared, 'held_out_agreement': held_agreement,
                'held_out_agreement_95ci': wilson(held_agree, held_compared),
                'coverage': q['eligible']/offered_count if offered_count else 0,
                'unanswered': offered_count-q['answered'], 'missing_reference': offered_count-q['reference'], 'per_label': per_label,
                'confusion': [{'reference': a, 'predicted': b, 'count': n} for (a,b),n in sorted(q['matrix'].items())]}
        if not questions:
            missing.append('answered_questions')
        for required in identity['required_questions']:
            if required not in questions:
                missing.append(f'{required}:answered_question')
        receipts_complete = all(r.get('accounting_complete') for r in group['records'].values())
        prices = [group['prices'].get(call_id, {}).get('cost_usd') for call_id in group['calls']]
        cost_complete = receipts_complete and all(finite(p) and p >= 0 for p in prices)
        if not cost_complete:
            missing.append('complete_decision_cost')
        reference_calls = {}
        text_calls = {}
        reference_complete = True
        text_complete = True
        sample_by_case = {}
        attempted_by_case = {}
        unknown_attempt_by_case = {}
        for record in group['records'].values():
            sample_key = (record.get('case_id') or record.get('comparison_id'),
                          record.get('reference_version'), record.get('behavior_fingerprint'))
            sample_status = record.get('reference_status') or ('unselected' if record.get('mode') == 'gate' else 'shadow')
            previous_status = sample_by_case.get(sample_key)
            if previous_status is None or (previous_status in ('unselected', 'queued') and sample_status not in ('unselected', 'queued')):
                sample_by_case[sample_key] = sample_status
            # Historical shadow records omitted the flag but contain an
            # incumbent call. New gate samples state it explicitly.
            attempt_state = record.get('reference_attempted', bool(record.get('reference')))
            attempted_by_case[sample_key] = attempted_by_case.get(sample_key, False) or attempt_state is True
            unknown_attempt_by_case[sample_key] = unknown_attempt_by_case.get(sample_key, False) or attempt_state is None
            if record.get('reference_selected') and record.get('reference_status') in ('queued', 'inference_expired'):
                reference_complete = False
            if attempt_state is None and record.get('reference_selected'):
                reference_complete = False
            if attempt_state is True:
                call = (record.get('reference') or {}).get('call') or {}
                receipt = call.get('receipt') or {}
                call_id = receipt.get('context', {}).get('llm_call_id')
                cost = call.get('cost_usd')
                if not call_id or not call.get('attempts_complete') or not finite(cost) or cost < 0:
                    reference_complete = False
                else:
                    reference_calls[call_id] = 0.0 if receipt.get('response_reused') else cost
            completion = record.get('completion')
            if record.get('requires_completion') and completion is None:
                text_complete = False
            if record.get('requires_completion') and completion is not None:
                if completion.get('result_validated') is False:
                    errors.append('required_text_validation_failed')
                elif completion.get('result_validated') is not True:
                    missing.append('required_text_validation')
            if completion and completion.get('text_attempted'):
                call = completion.get('text_call') or {}
                receipt = call.get('receipt') or {}
                call_id = receipt.get('context', {}).get('llm_call_id')
                cost = call.get('cost_usd')
                if not call_id or not call.get('attempts_complete') or not finite(cost) or cost < 0:
                    text_complete = False
                else:
                    text_calls[call_id] = 0.0 if receipt.get('response_reused') else cost
        sampling = Counter(sample_by_case.values())
        if not text_complete:
            missing.append('complete_text_stage_receipts_and_cost')
        if not reference_complete:
            missing.append('complete_incumbent_receipts_and_cost')

        pair = paired.get(group_id, {})
        paired_ids = {case.get('case_id') for case in pair.get('cases', []) if case.get('case_id')}
        if not pair.get('evidence_id') or len(paired_ids) < 10:
            missing.append('paired_live_evidence')
        required_invariants = ('rollback', 'cache_invalidation', 'provider_outage_recovery', 'receipts_reconciled', 'mutation_invariants')
        if not all(pair.get('invariants', {}).get(k) is True for k in required_invariants):
            missing.append('live_invariants')
        costs, baselines, latencies = [], [], []
        for case in pair.get('cases', []):
            if not case.get('expected_outcome_passed'):
                errors.append('downstream_outcome')
            a, b, latency = case.get('gated_total_usd'), case.get('incumbent_total_usd'), case.get('foreground_ms')
            if not (case.get('receipts_complete') and all(finite(x) and x >= 0 for x in (a,b,latency))):
                missing.append('complete_paired_path_cost_latency')
                continue
            costs.append(a); baselines.append(b); latencies.append(latency)
        if costs and sum(costs) >= sum(baselines):
            errors.append('gated_cost_not_below_incumbent')
        p95 = percentile(latencies)
        budget = pair.get('budget_ms')
        if not finite(budget) or budget <= 0:
            missing.append('foreground_budget')
        elif p95 is not None and p95 > budget:
            errors.append('foreground_p95_exceeds_budget')
        all_costs = dict(zip(group['calls'], prices)) if cost_complete else {}
        for call_id, cost in {**reference_calls, **text_calls}.items():
            if call_id in all_costs and all_costs[call_id] != cost:
                errors.append('conflicting_physical_call_cost')
            all_costs[call_id] = cost
        output.append({'qualification_id':group_id, 'identity': identity, 'status': 'FAIL' if errors else 'INCOMPLETE' if missing else 'PASS',
            'failures': sorted(set(errors)), 'missing':sorted(set(missing)), 'distinct_cases':len(cases), 'offered_cohort_cases':offered_count, 'questions':questions_out,
             'decision_usage': usage_totals(list(group['calls'].values())),
            'reference_sampling':dict(sampling),
            'reference_attempted_cases':sum(attempted_by_case.values()),
            'reference_attempt_unknown_cases':sum(unknown_attempt_by_case.values()),
            'incumbent_usage': reference_usage(group['records'].values()),
            'text_usage': reference_usage(group['records'].values(), text=True),
            'timing_ms': {key: {'p50':percentile(values,.5), 'p95':percentile(values), 'count':len(values)} for key, values in {
                'discovery':[r['discovery_latency_ms'] for r in group['records'].values() if finite(r.get('discovery_latency_ms')) and r['discovery_latency_ms'] >= 0],
                'model_queue_wait':queue_wait_samples(group['calls'].values()),
                'model_attempt':[c['latency_ms'] for c in group['calls'].values() if finite(c.get('latency_ms')) and c['latency_ms'] >= 0],
                'engine':[r['engine_latency_ms'] for r in group['records'].values() if finite(r.get('engine_latency_ms'))],
                'foreground':[(r.get('completion') or {}).get('foreground_ms',r.get('foreground_latency_ms',r.get('wall_latency_ms') if r.get('mode') == 'gate' else None)) for r in group['records'].values() if finite((r.get('completion') or {}).get('foreground_ms',r.get('foreground_latency_ms',r.get('wall_latency_ms') if r.get('mode') == 'gate' else None)))],
                'evidence_completion':[r['wall_latency_ms'] for r in group['records'].values() if finite(r.get('wall_latency_ms'))],
                'incumbent':[r['reference']['latency_ms'] for r in group['records'].values() if finite((r.get('reference') or {}).get('latency_ms'))]}.items()},
            'decision_calls':len(group['calls']), 'decision_cost_usd':sum(prices) if cost_complete else None,
            'reference_calls':len(reference_calls), 'reference_cost_usd':sum(reference_calls.values()) if reference_complete else None,
            'text_calls':len(text_calls), 'text_cost_usd':sum(text_calls.values()) if text_complete else None,
            'observed_total_cost_usd':sum(all_costs.values()) if cost_complete and reference_complete and text_complete else None,
            'shadow_total_cost_usd':sum(prices)+sum(reference_calls.values()) if cost_complete and reference_complete and all(r.get('mode', 'shadow') == 'shadow' for r in group['records'].values()) else None,
            'paired_foreground_p95_ms':p95, 'paired_gate_cost_usd':sum(costs) if costs else None,
            'paired_incumbent_cost_usd':sum(baselines) if baselines else None})
    orphan_references = sum(key not in seen_comparisons for key in reference_stages)
    return {'schema_version':1, 'status':'FAIL' if any(g['status']=='FAIL' for g in output) else 'PASS' if output and not orphan_references and all(g['status']=='PASS' for g in output) else 'INCOMPLETE',
        'groups':output, 'skipped_cache_records':skipped_cache, 'unanswered_records':incomplete_records,
        'invocation_outcomes':dict(Counter((r.get('status') or 'unknown') for r in invocations)),
        'gate_changed':False,
        'orphan_reference_stages':orphan_references}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ledger', help='Scoped canonical calls/attempts JSON export; joins dated prices without recalculation'); parser.add_argument('records'); parser.add_argument('--principal', required=True); parser.add_argument('--workspace', required=True); parser.add_argument('--reviews'); parser.add_argument('--paired'); parser.add_argument('--output',required=True)
    args=parser.parse_args()
    records = [row for row in load_records(args.records) if (row.get('scope', {}).get('scope') or row.get('export_scope')) == {'principal': args.principal, 'workspace': args.workspace}]
    if args.ledger:
        records = reconcile_ledger(records, json.loads(Path(args.ledger).read_text()), args.principal, args.workspace)
    result=report(records, json.loads(Path(args.reviews).read_text()) if args.reviews else None,
        json.loads(Path(args.paired).read_text()) if args.paired else None)
    Path(args.output).parent.mkdir(parents=True,exist_ok=True)
    Path(args.output).write_text(json.dumps(result,indent=2,allow_nan=False)+'\n')
    print(f"{result['status']}: {len(result['groups'])} qualification groups; gate configuration unchanged")

if __name__=='__main__': main()
