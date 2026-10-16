#!/usr/bin/env python3
"""Paired live rail qualification. Requires an isolated running stack and real credentials.

Only the typed gate changes between arms; host decision mode stays all_engines.
Artifacts can contain tool output: keep --output outside git. No token is logged.
"""
from __future__ import annotations

import argparse
import http.client
import http.server
import html
import threading
import json
import math
import os
from pathlib import Path
import re
import socket
import time
import urllib.parse
import urllib.request
import uuid

ENGINES = ['magician', 'pi', 'claude_code', 'codex', 'codex_app_server', 'grok', 'agy']
CONTRACT_VERSION = 4
MODELS = {'grok': 'grok-4.7-build-fast', 'agy': 'gemini-3.7-flash-high'}
TERMINAL = {'completed', 'failed', 'cancelled', 'canceled', 'paused', 'paused_by_user',
            'waiting_for_user', 'waiting_for_input', 'waiting_for_confirmation', 'max_iterations_reached'}


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')
    path.chmod(0o600)


def number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value >= 0 else None


def usage_summary(events):
    """Count terminal logical calls once; never add streamed cumulative snapshots."""
    calls = {}
    for index, event in enumerate(events):
        if event.get('event_type') not in ('llm.succeeded', 'llm.failed'):
            continue
        p = event.get('payload', {})
        calls[p.get('llm_call_id') or p.get('provider_attempt_id') or event.get('event_id') or f'uncorrelated-{index}'] = (event['event_type'], p)
    rows = []
    for kind, p in calls.values():
        available = p.get('usage_availability')
        legacy_harness = available is None and str(p.get('provider', '')).startswith('harness')
        known_tokens = available.get('tokens', False) if available is not None else kind == 'llm.succeeded'
        row = {}
        for key, availability in [('input_tokens', 'tokens'), ('output_tokens', 'tokens'), ('cache_read_tokens', 'cache_read'), ('cache_creation_tokens', 'cache_write'), ('cost_usd', 'cost')]:
            known = available.get(availability, False) if available is not None else known_tokens
            if legacy_harness and availability != 'tokens':
                known = False
            raw = p.get('cost_usd', p.get('cost')) if key == 'cost_usd' else p.get(key)
            row[key] = number(raw) if known else None
        rows.append(row)
    result = {'calls': len(rows), 'models': sorted({str(p.get('model')) for _, p in calls.values()})}
    for field in ['input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_creation_tokens', 'cost_usd']:
        known = [r[field] for r in rows if r[field] is not None]
        result[field] = {'reported_sum': sum(known) if known else None, 'reported_calls': len(known), 'total_calls': len(rows), 'complete': bool(rows) and len(known) == len(rows)}
    paired = [r for r in rows if r['input_tokens'] is not None and r['cache_read_tokens'] is not None]
    denominator = sum(r['input_tokens'] for r in paired)
    result['cache_hit_rate'] = sum(r['cache_read_tokens'] for r in paired) / denominator if paired and len(paired) == len(rows) and denominator else None
    result['cost_note'] = 'Execution-event costs or CLI estimates; not proof of subscription billing. Query the shared llm_calls ledger for Decision Model receipts and combined cost totals.'
    return result


def grade_file(record, expected):
    state = record.get('execution_result', {}).get('state', {})
    reads = [e for e in record.get('events', []) if e.get('event_type') == 'tool.succeeded' and (e.get('payload', {}).get('target', '').split('(', 1)[0] == 'read_file' or e.get('payload', {}).get('tool_name') == 'read_file')]
    paths = record.get('fixture_paths', [])
    paths_read = len(paths) == len(expected) and all(any(path in json.dumps(e.get('payload', {})) for e in reads) for path in paths)
    return state.get('status') == 'completed' and state.get('completion_kind') == 'full' and len(reads) >= len(expected) and paths_read and all(x in (record.get('final_output') or '') for x in expected)


def grade_browser(record, receipt):
    state = record.get('execution_result', {}).get('state', {})
    click_succeeded = any(e.get('event_type') == 'tool.succeeded' and
        'browser__click' in (e.get('payload', {}).get('target', '') or e.get('payload', {}).get('tool_name', ''))
        for e in record.get('events', []))
    return bool(receipt and state.get('status') == 'completed' and state.get('completion_kind') == 'full'
                and click_succeeded and receipt in (record.get('final_output') or ''))


class BrowserFixture:
    """A private loopback form: its receipt exists only after the real POST."""
    def __init__(self, output):
        self.receipts = {}
        self.beacons = []
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def page(self, body):
                raw = ('<!doctype html><html><head><title>Decision qualification</title></head><body>' + body + '</body></html>').encode()
                self.send_response(200)
                self.send_header('Content-Type', 'text/html; charset=utf-8')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                self.page('<h1>Local browser qualification</h1><form method="post"><label>Test marker <input name="marker" required></label><button>Continue</button></form>')

            def do_POST(self):
                length = int(self.headers.get('Content-Length', '0'))
                if length > 4096:
                    self.send_error(413)
                    return
                marker = urllib.parse.parse_qs(self.rfile.read(length).decode()).get('marker', [''])[0]
                receipt = 'BROWSER_PROOF_' + uuid.uuid4().hex
                fixture.receipts[marker] = receipt
                fixture.beacons.append({'time': time.time(), 'marker': marker, 'receipt': receipt})
                save(output / 'beacons.json', fixture.beacons)
                self.page('<h1>Complete</h1><p>Receipt: ' + html.escape(receipt) + '</p>')

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def browser_pairs(client, original, summary):
    args = client.args
    fixture = BrowserFixture(args.output)
    try:
        for repetition in range(args.repeats):
            for index, engine in enumerate(args.engines):
                pair = f'browser-{repetition}-{engine}-{uuid.uuid4().hex[:8]}'
                for arm in pair_order(repetition, index):
                    client.gate(original, arm)
                    case = pair + '-' + arm
                    prompt = f'Browser interaction test on our isolated local fixture. Discover browser__open, browser__snapshot, browser__fill, browser__click, and browser__get using exact tool_search select names. For every browser call set connection_mode=headless and engine=bundled_chrome; keep the session assigned by the host, with no session flags. Open {fixture.url}, snapshot, fill Test marker with {case}, click Continue, then read and report the exact BROWSER_PROOF receipt shown by the page. Complete only after observing that receipt. No scripts, shell, batch, fetch, other websites, or delegation.'
                    print('START', case, flush=True)
                    record = client.task(engine, case, prompt, 'web-researcher')
                    record['pair'] = pair
                    record['expected'] = fixture.receipts.get(case)
                    record['passed'] = grade_browser(record, record['expected'])
                    record['browser_post_verified'] = case in fixture.receipts
                    row = client.finish(record, arm)
                    summary['rows'].append(row)
                    save(args.output / 'summary.json', summary)
                    print('DONE', engine, arm, row['passed'], row['elapsed_s'], row['selection_counts'], flush=True)
    finally:
        fixture.close()


def pair_order(repetition, engine_index):
    return ['on', 'off'] if (repetition + engine_index) % 2 == 0 else ['off', 'on']


class Client:
    def __init__(self, args):
        self.args = args
        self.token = json.loads(args.auth.read_text())['token']

    def api(self, method, path, body=None, lines=False):
        if not path.startswith('/api/'):
            path = '/api/magician/v2' + path
        request = urllib.request.Request(self.args.base + path, method=method,
            data=json.dumps(body).encode() if body is not None else None,
            headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + self.token})
        with urllib.request.urlopen(request, timeout=self.args.timeout) as response:
            raw = response.read().decode()
        return [json.loads(line) for line in raw.splitlines() if line.strip()] if lines else json.loads(raw)

    def decision(self, path, body=None):
        connection = http.client.HTTPConnection('localhost', timeout=30)
        connection.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.sock.settimeout(30)
        connection.sock.connect(str(self.args.socket))
        try:
            connection.request('POST' if body is not None else 'GET', path,
                json.dumps(body) if body is not None else None, {'Content-Type': 'application/json'})
            response = connection.getresponse()
            value = json.loads(response.read())
            if response.status != 200:
                raise RuntimeError(f'Decision HTTP {response.status}')
            return value
        finally:
            connection.close()

    def gate(self, original, arm):
        binding = re.search(r'(?ms)^  tool_action_judge:\n.*?(?=^  \S|\Z)', original)
        if binding is None:
            raise ValueError('Missing tool_action_judge binding')
        block, count = re.subn(r'(    gate:\n      enabled: )(?:true|false)', r'\g<1>' + str(arm == 'on').lower(), binding.group())
        if count != 1 or not re.search(r'    shadow:\n      enabled: false', block):
            raise ValueError('Expected one gate and shadow disabled in isolated decision config')
        text = original[:binding.start()] + block + original[binding.end():]
        self.args.config.write_text(text)
        probe = {'contract_version': CONTRACT_VERSION, 'snapshot': 'qualification-reload', 'locality': 'cloud',
                 'context': {'goal': 'Read a local qualification fixture'}, 'tools': []}
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            response = self.decision('/v1/action', probe)
            expected = 'no_grounded_candidates' if arm == 'on' else 'structured_gate_disabled'
            if response.get('reason') == expected:
                return
            time.sleep(.25)
        raise RuntimeError(f'Gate {arm} did not become active: {response.get("reason")}')

    def events(self, record):
        query = urllib.parse.urlencode({'task_id': record['task'], 'execution_id': record['execution'],
            'since': int(record['started'] * 1000), 'limit': 10000, 'backfill_only': 'true'})
        return [e for e in self.api('GET', '/api/magician/v3/events?' + query, lines=True)
            if e.get('execution_id') == record['execution'] or e.get('payload', {}).get('execution_id') == record['execution']]

    def task(self, engine, case, prompt, agent='personal-assistant'):
        self.api('PUT', '/plane/engine', {'harness_engine': engine, 'harness_model': MODELS.get(engine, 'default'), 'pi_profile': self.args.pi_profile})
        task = self.api('POST', '/api/magician/v3/tasks', {'title': case, 'description': prompt,
            'agent_id': agent, 'approved': True, 'created_by': 'user'})
        tid = task['task']['manifest']['task_id']
        record = {'engine': engine, 'case': case, 'task': tid, 'started': time.time(), 'prompt': prompt}
        start = time.monotonic()
        try:
            body = {'llm_routing_overrides': {'operations': {'agentic_decision': {'profile': self.args.native_profile}}}} if engine == 'magician' else {}
            run = self.api('POST', f'/api/magician/v3/tasks/{tid}/execute', body)
            eid = record['execution'] = run['execution']['state']['execution_id']
            while time.monotonic() - start < self.args.timeout:
                result = record['execution_result'] = self.api('GET', f'/api/magician/v3/tasks/{tid}/executions/{eid}')
                if result.get('state', {}).get('status') in TERMINAL:
                    break
                time.sleep(.5)
            record['elapsed_s'] = round(time.monotonic() - start, 3)
            status = record.get('execution_result', {}).get('state', {}).get('status')
            if status not in {'completed', 'failed', 'cancelled', 'canceled'}:
                self.api('POST', f'/api/magician/v3/executions/{eid}/cancel', {})
            output = self.args.runtime / f'scopes/anonymous/default/tasks/{tid}/executions/{eid}/outputs/out_exec_{eid}.md'
            deadline = time.monotonic() + 45
            while status == 'completed' and not output.exists() and time.monotonic() < deadline:
                time.sleep(.25)
            record['final_output'] = output.read_text() if output.exists() else None
            record['artifact_elapsed_s'] = round(time.monotonic() - start, 3)
            record['events'] = self.events(record)
        except Exception as error:
            record['error'] = str(error)
            record.setdefault('elapsed_s', round(time.monotonic() - start, 3))
            if record.get('execution'):
                try:
                    self.api('POST', f'/api/magician/v3/executions/{record["execution"]}/cancel', {})
                except Exception:
                    pass
        return record

    def finish(self, record, arm):
        record['arm'] = arm
        record['usage'] = usage_summary(record.get('events', []))
        record['decisions'] = []
        if self.args.decision_log.exists():
            for line in self.args.decision_log.read_text().splitlines():
                decision = json.loads(line)
                if decision.get('time', 0) >= record['started'] and (decision.get('snapshot') or '').startswith(record.get('execution', 'missing') + ':'):
                    record['decisions'].append(decision)
        selects = [d for d in record['decisions'] if d.get('phase') == 'select']
        gate_checks = [d for d in selects if d.get('reason') not in ['consecutive_step_cap', 'prior_action_failed', 'task_instructions_changed']]
        record['arm_verified'] = bool(gate_checks) and all((d.get('reason') == 'structured_gate_disabled') == (arm == 'off') for d in gate_checks)
        record['decision_usage'] = {k: sum((d.get('usage') or {}).get(k, 0) for d in record['decisions']) for k in ['input_tokens', 'output_tokens']}
        record['selection_counts'] = {origin: sum(d.get('origin') == origin for d in record['decisions']) for origin in ['structured', 'planner']}
        record['passed'] = bool(record.get('passed') and record['arm_verified'])
        save(self.args.output / (record['case'] + '.json'), record)
        return {k: v for k, v in record.items() if k not in ['events', 'decisions', 'prompt', 'execution_result', 'final_output']}


def model_actions(client, original):
    """Bounded decision API + real reads, explicitly separate from host workflow coverage."""
    args = client.args
    fixture = args.output / 'model-fixture.txt'
    fixture.write_text('MODEL_RAIL_' + uuid.uuid4().hex)
    records = []
    try:
        for model in ['jev', 'laya', 'kev-0.8b', 'kev-4b']:
            text, count = re.subn(r'(  tool_action_judge:\n    model: )[^\n]+', r'\g<1>' + model, original)
            if count != 1:
                raise ValueError('Expected explicitly bound tool_action_judge model')
            # JSON strings are also valid YAML quoted scalars.
            text, count = re.subn(r'(?m)^#?\s?models_dir:.*$', 'models_dir: ' + json.dumps(str(args.models_dir)), text)
            if not count:
                text += '\nmodels_dir: ' + json.dumps(str(args.models_dir)) + '\n'
            args.config.write_text(text)
            start = time.monotonic()
            while time.monotonic() - start < 120:
                operations = client.decision('/v1/operations')['operations']
                if any(model in o['route_cloud' if model == 'jev' else 'route_local'] for o in operations):
                    break
                time.sleep(.25)
            else:
                raise RuntimeError('Model not bound: ' + model)
            row = {'model': model, 'load_reload_s': round(time.monotonic() - start, 3), 'actions': []}
            for name, description, path, reader in [
                ('read_file', 'Read the text of a file.', fixture, fixture.read_text),
                ('list_directory', 'List filenames in a directory.', args.output, lambda: sorted(p.name for p in args.output.iterdir())),
                ('file_metadata', 'Read file metadata without opening contents.', fixture, lambda: fixture.stat().st_size),
            ]:
                for repetition in range(args.repeats):
                    arguments = {'path': str(path)}
                    request = {'contract_version': CONTRACT_VERSION, 'snapshot': uuid.uuid4().hex, 'locality': 'cloud' if model == 'jev' else 'local',
                        'context': {'goal': description + ' The exact requested path is ' + str(path), 'success_criteria': 'The requested read operation returned successfully.', 'instructions': 'User authorized this read-only local operation.', 'observation': {'exists': True, 'path': str(path)}},
                        'tools': [{'name': name, 'description': description, 'parameters': {'type': 'object', 'properties': {'path': {'type': 'string', 'const': str(path)}}, 'required': ['path'], 'additionalProperties': False}}]}
                    before = time.monotonic()
                    response = client.decision('/v1/action', request)
                    verdict = response.get('verdict', {})
                    selected = verdict.get('outcome') == 'execute' and verdict.get('origin') == 'structured' and verdict.get('call') == {'tool': name, 'arguments': arguments}
                    result = reader() if selected else None
                    row['actions'].append({'tool': name, 'repeat': repetition, 'elapsed_s': round(time.monotonic() - before, 3), 'response': response, 'selected_exact_authorized_call': selected, 'real_read_executed': selected, 'result_verified': result == reader() if selected else False})
                    print(model, name, repetition, response.get('reason'), selected, flush=True)
            records.append(row)
            save(args.output / 'model-actions.json', records)
    finally:
        args.config.write_text(original)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='http://127.0.0.1:3102')
    for flag in ['auth', 'config', 'runtime', 'socket', 'decision-log', 'output']:
        parser.add_argument('--' + flag, required=True, type=Path)
    parser.add_argument('--workflow', choices=['files', 'browser'], default='files')
    parser.add_argument('--engines', nargs='+', choices=ENGINES, default=ENGINES)
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--timeout', type=int, default=360)
    parser.add_argument('--native-profile', default='chat-gpt61sol-responses-vision-toolsauto-fast')
    parser.add_argument('--pi-profile', default='chat-openai-adaptive-instant')
    parser.add_argument('--models-dir', type=Path, help='Run bounded model-action qualification instead of paired host workflows')
    parser.add_argument('--fixtures-root', type=Path, default=Path('/tmp'), help='Directory allowed by the test host read_file policy')
    args = parser.parse_args()
    if args.runtime.resolve() == (Path.home() / 'MagicianNotes').resolve() or args.config.parent.resolve() != args.runtime.resolve():
        parser.error('Use an isolated runtime with its own decision config')
    args.output.mkdir(parents=True, exist_ok=True, mode=0o700)
    client = Client(args)
    original = args.config.read_text()
    if args.models_dir:
        model_actions(client, original)
        return
    settings = client.api('GET', '/plane/engines')
    original_mode = {'mode': settings['decision_mode']}
    original_engine = {'harness_engine': settings['current'], 'harness_model': settings['run_model'], 'pi_profile': settings['run_pi_profile']}
    summary = {'kind': 'typed_gate_on_off', 'workflow': args.workflow, 'repeats': args.repeats, 'profiles': {'magician': args.native_profile, 'pi': args.pi_profile}, 'rows': []}
    save(args.output / 'original-settings.json', {'decision_config': original, 'mode': original_mode, 'engine': original_engine})
    try:
        client.api('PUT', '/plane/decision-mode', {'mode': 'all_engines'})
        if args.workflow == 'browser':
            browser_pairs(client, original, summary)
            return
        for repetition in range(args.repeats):
            for index, engine in enumerate(args.engines):
                pair = f'pair-{repetition}-{engine}-{uuid.uuid4().hex[:8]}'
                codes = ['RAIL_' + uuid.uuid4().hex for _ in range(2)]
                files = [args.fixtures_root / ('magician-' + pair + f'-{i}.txt') for i in range(2)]
                for path, code in zip(files, codes):
                    path.write_text('Independent fixture code: ' + code + '\n')
                prompt = f'Read these two local files using read_file, then report both exact independent fixture codes: {files[0]} and {files[1]}. Do not modify anything, run commands, browse, delegate or use other work tools. Complete after both files have been read.'
                for arm in pair_order(repetition, index):
                    client.gate(original, arm)
                    print('START', pair, arm, flush=True)
                    record = client.task(engine, pair + '-' + arm, prompt)
                    record['pair'] = pair
                    record['expected'] = codes
                    record['fixture_paths'] = [str(path) for path in files]
                    record['passed'] = grade_file(record, codes)
                    row = client.finish(record, arm)
                    summary['rows'].append(row)
                    save(args.output / 'summary.json', summary)
                    print('DONE', engine, arm, row['passed'], row['elapsed_s'], row['selection_counts'], flush=True)
    finally:
        args.config.write_text(original)
        client.api('PUT', '/plane/decision-mode', {'mode': original_mode['mode']})
        client.api('PUT', '/plane/engine', {k: original_engine[k] for k in ['harness_engine', 'harness_model', 'pi_profile'] if k in original_engine})


if __name__ == '__main__':
    main()
