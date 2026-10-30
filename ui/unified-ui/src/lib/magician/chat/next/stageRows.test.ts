import { describe, expect, it } from 'vitest';

import {
	agenticActionAsToolEvent,
	clipText,
	describeToolCall,
	latestRows,
	STAGE_LATEST_LIMIT,
	summarizeCallArgs,
	toolRowName,
	toStageRows,
	toStageStatus,
	toolTarget
} from './stageRows';

describe('stageRows', () => {
	it('toolTarget reads a host without www., else a path basename', () => {
		expect(toolTarget({ url: 'https://www.rust-lang.org/learn' })).toBe('rust-lang.org');
		expect(toolTarget({ href: 'https://docs.rs/x' })).toBe('docs.rs');
		expect(toolTarget({ path: '/tmp/work/notes.md' })).toBe('notes.md');
		expect(toolTarget({ file_path: 'C:\\work\\a.txt' })).toBe('a.txt');
		expect(toolTarget({ url: 'not a url' })).toBeUndefined();
		expect(toolTarget(null)).toBeUndefined();
	});

	it('toStageStatus keeps the TurnStage vocabulary and settles the rest', () => {
		expect(toStageStatus('running')).toBe('running');
		expect(toStageStatus('failed')).toBe('failed');
		expect(toStageStatus('waiting')).toBe('waiting');
		expect(toStageStatus('done')).toBe('done');
		expect(toStageStatus('idle')).toBe('done');
	});

	it('toStageRows attaches tool meta and falls back for the target', () => {
		const rows = [
			{ key: 'a', label: 'Calling web_fetch', status: 'running' },
			{ key: 'b', label: 'Artifact', status: 'idle' }
		];
		const meta = new Map([['a', { tool: 'web_fetch', target: 'rust-lang.org' }]]);
		expect(toStageRows(rows, meta, (row) => (row.key === 'b' ? 'out.csv' : undefined))).toEqual([
			{ key: 'a', label: 'Calling web_fetch', status: 'running', tool: 'web_fetch', target: 'rust-lang.org' },
			{ key: 'b', label: 'Artifact', status: 'done', target: 'out.csv' }
		]);
	});

	it('toStageRows carries a start only on a running row, for its live elapsed', () => {
		const rows = [
			{ key: 'a', label: 'Thinking…', status: 'running', startedAt: 1_000 },
			{ key: 'b', label: 'Thinking with x', status: 'done', startedAt: 500 },
			{ key: 'c', label: 'Calling y', status: 'running' }
		];
		expect(toStageRows(rows, new Map())).toEqual([
			{ key: 'a', label: 'Thinking…', status: 'running', startedAt: 1_000 },
			{ key: 'b', label: 'Thinking with x', status: 'done' },
			{ key: 'c', label: 'Calling y', status: 'running' }
		]);
	});

	it('toStageRows keeps the secondary line of model calls and context phases only', () => {
		const rows = [
			{ key: 'context:p1', label: 'Context prepared (partial)', status: 'done', kind: 'step',
				detail: '5769 ms total · Memory search: not completed' },
			{ key: 'llm:1', label: 'Thinking with jev-1.13.0', status: 'done', kind: 'llm',
				detail: '299ms · 7.9k tok · <$0.001' },
			{ key: 'tool:1', label: 'Calling web_fetch', status: 'done', kind: 'tool', detail: 'GET /' },
			{ key: 'file:1', label: 'Artifact', status: 'done', kind: 'llm', detail: 'out.csv' }
		];
		const stage = toStageRows(rows, new Map(), (row) => (row.key === 'file:1' ? 'out.csv' : undefined));
		expect(stage.map((row) => row.detail)).toEqual([
			'5769 ms total · Memory search: not completed',
			'299ms · 7.9k tok · <$0.001',
			undefined,
			undefined
		]);
	});

	it('latestRows keeps the latest rows and counts the rest', () => {
		const rows = Array.from({ length: 8 }, (_, i) => i);
		expect(latestRows(rows)).toEqual({ shown: [3, 4, 5, 6, 7], hidden: 3 });
		expect(STAGE_LATEST_LIMIT).toBe(5);
		expect(latestRows([1, 2], 5)).toEqual({ shown: [1, 2], hidden: 0 });
		expect(latestRows([], 5)).toEqual({ shown: [], hidden: 0 });
	});

	it('describeToolCall prefers a server detail, clipped to one short line', () => {
		const detail = 'metabase-mcp · execute_sql — SELECT id FROM orders';
		expect(describeToolCall('metabase-mcp', undefined, detail)).toEqual({
			tool: 'metabase-mcp',
			name: detail,
			source: 'detail'
		});
		const long = describeToolCall('x', undefined, `a\n${'b'.repeat(300)}`);
		expect(long.name.length).toBeLessThanOrEqual(100);
		expect(long.name.endsWith('…')).toBe(true);
		expect(long.name).not.toContain('\n');
	});

	it('describeToolCall names a <pack>__call_tool call by pack and remote tool', () => {
		const sql = `SELECT o.id, o.total FROM orders o WHERE o.created_at > now() - interval '7 days' ORDER BY o.total DESC`;
		const call = describeToolCall('metabase-mcp__call_tool', {
			name: 'execute_sql',
			arguments_json: JSON.stringify({ database_id: 2, sql })
		});
		expect(call.name).toBe('metabase-mcp · execute_sql');
		expect(call.source).toBe('mcp');
		expect(call.target?.startsWith('SELECT o.id, o.total')).toBe(true);
		expect(call.target!.length).toBeLessThanOrEqual(80);

		// Object-valued arguments and a term field.
		expect(
			describeToolCall('zepto-mcp__call_tool', { name: 'search', arguments_json: { term: 'milk' } })
		).toEqual({ tool: 'zepto-mcp__call_tool', name: 'zepto-mcp · search', target: 'milk', source: 'mcp' });
	});

	it('shows the governed MCP tool_name and the internal CLI operation', () => {
		expect(describeToolCall('zepto-mcp__call_tool', { tool_name: 'search_products', arguments_json: '{"query":"milk"}' }))
			.toMatchObject({ name: 'zepto-mcp · search_products', target: 'milk' });
		expect(describeToolCall('higgsfield__run', { args: ['model', 'list', '--json'] }))
			.toMatchObject({ name: 'higgsfield · model list', source: 'cli' });
		expect(describeToolCall('creative__run', { args: ['generate', 'create', 'model', '--prompt', 'private prompt'] }).name)
			.toBe('creative · generate create');
		expect(describeToolCall('creative__run', { args: ['--token', 'private token'] }).name)
			.toBe('creative__run');
	});

	it('describeToolCall never shows raw JSON longer than ~100 chars', () => {
		const big = JSON.stringify({ rows: Array.from({ length: 50 }, (_, i) => ({ i, v: 'x'.repeat(10) })) });
		const call = describeToolCall('pack__call_tool', { name: 'bulk', arguments_json: big });
		expect(call.target!.length).toBeLessThanOrEqual(80);
		expect(summarizeCallArgs('not json {')).toBe('not json {');
		expect(summarizeCallArgs('{}')).toBeUndefined();
		expect(summarizeCallArgs(undefined)).toBeUndefined();
	});

	it('describeToolCall keeps a plain tool name and its toolTarget', () => {
		expect(describeToolCall('web_fetch', { url: 'https://www.rust-lang.org/learn' })).toEqual({
			tool: 'web_fetch',
			name: 'web_fetch',
			target: 'rust-lang.org',
			source: 'plain'
		});
		// A call_tool without args (e.g. its finish) stays plain.
		expect(describeToolCall('metabase-mcp__call_tool').source).toBe('plain');
	});

	it('clipText and toolRowName', () => {
		expect(clipText('  a   b ', 10)).toBe('a b');
		expect(clipText('abcdef', 4)).toBe('abc…');
		expect(toolRowName('Calling metabase-mcp · execute_sql')).toBe('metabase-mcp · execute_sql');
		expect(toolRowName('[analyst] shell returned')).toBe('shell');
		expect(toolRowName('web_fetch failed')).toBe('web_fetch');
		expect(toolRowName('Decomposing into 3 parallel agents')).toBeUndefined();
		expect(toolRowName(null)).toBeUndefined();
	});

	it('agenticActionAsToolEvent maps start/executed onto one call_id', () => {
		const base = { execution_id: 'e1', step_id: 's1', iteration: 2, action_type: 'pack', target: 'shell' };
		const started = agenticActionAsToolEvent('AgenticActionStarted', { ...base, call_id: 'k1', timestamp: 10 });
		expect(started?.eventType).toBe('tool.call.started');
		expect(started?.payload).toMatchObject({ tool_name: 'shell', call_id: 'k1' });

		const done = agenticActionAsToolEvent('AgenticActionExecuted', {
			...base,
			call_id: 'k1',
			success: true,
			latency_ms: 40,
			timestamp: 50
		});
		expect(done).toMatchObject({
			eventType: 'tool.call.finished',
			payload: { call_id: 'k1', duration_ms: 40, tool_name: 'shell' }
		});

		const failed = agenticActionAsToolEvent('AgenticActionExecuted', { ...base, success: false, timestamp: 7 });
		expect(failed?.eventType).toBe('tool.call.failed');
		// Legacy: no call_id → step-iteration-timestamp.
		expect(failed?.payload.call_id).toBe('s1-2-7');

		expect(agenticActionAsToolEvent('AgenticActionStarted', { action_type: 'pack' })?.payload.tool_name).toBe('pack');
		expect(agenticActionAsToolEvent('LLMRequestSent', base)).toBeNull();
	});
});
