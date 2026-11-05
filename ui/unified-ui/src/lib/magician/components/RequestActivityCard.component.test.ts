import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// The card is a pure renderer over `chatTurnEventsStore` (the store owns the
// SSE + REST fetch). Seed the store directly and make the network entry points
// no-ops, which is exactly how the card receives a turn's events in the app.
const store = vi.hoisted(() => ({
	events: new Map<string, unknown[]>(),
	usage: new Map<string, unknown>(),
	unsubscribe: { calls: 0 }
}));

vi.mock('$lib/stores/chatTurnEventsStore', async (importOriginal) => {
	const { writable } = await import('svelte/store');
	const { eventTimestampMs } =
		await importOriginal<typeof import('$lib/stores/chatTurnEventsStore')>();
	const chatTurnEventsStore = writable(store.events);
	const chatTurnUsageBaselineStore = writable(store.usage);
	return {
		chatTurnEventsStore,
		chatTurnUsageBaselineStore,
		eventTimestampMs,
		eventDedupeKey: (raw: Record<string, any>) =>
			(raw?.data?.event?.payload?.event_id as string | undefined) ?? null,
		subscribeToChatTurn: vi.fn(() => () => {
			store.unsubscribe.calls += 1;
		}),
		fetchEventsPage: vi.fn(async () => {}),
		__seed: (turnId: string, events: unknown[]) => {
			store.events.set(turnId, events);
			chatTurnEventsStore.set(new Map(store.events));
		},
		__seedUsage: (turnId: string, usage: unknown) => {
			if (usage === null) store.usage.delete(turnId);
			else store.usage.set(turnId, usage);
			chatTurnUsageBaselineStore.set(new Map(store.usage));
		}
	};
});

vi.mock('$lib/magician/tasks/taskAttention', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskAttention')>()),
	readTaskRunState: vi.fn(async () => ({ ok: true, state: null }))
}));

import * as eventsStore from '$lib/stores/chatTurnEventsStore';
import { recordTaskLlmCall, recordTaskUsageSnapshot, resetTaskUsage } from '$lib/magician/chat/next/taskUsage';
import type { ExecutionPanelState } from '$lib/types/executionPanel';
import RequestActivityCard from './RequestActivityCard.svelte';
import { chatTurnStateStore, resetChatTurnStates } from '$lib/magician/chat/turnState';
import { get } from 'svelte/store';

const seed = (eventsStore as unknown as { __seed: (id: string, e: unknown[]) => void }).__seed;
const seedUsage = (eventsStore as unknown as { __seedUsage: (id: string, u: unknown) => void })
	.__seedUsage;

it('retains context timings and typed Decision Engine tokens in saved response steps', async () => {
	seed('context-turn',[
		agentEvent('context.retrieval.started',1,{phase_id:'p'}),
		agentEvent('context.retrieval.completed',2,{phase_id:'p',duration_ms:123,status:'completed',summary:'Memory search: 100 ms · Procedures: 20 ms'}),
		agentEvent('context.retrieval.started',3,{phase_id:'p'}),
		{event_type:'LLMResponseReceived',timestamp_ms:4,data:{success:true,model:'jev-test',latency_ms:71,input_tokens:1200,output_tokens:0,cost:0,usage_reported:true,correlation:{llm_call_id:'de-1',usage_availability:{tokens:true,cost:true,cache_read:false}}}}
	]);
	const {container}=render(RequestActivityCard,{sessionId:'s1',chatTurnId:'context-turn',live:true,embedded:true});
	await tick();
	expect(container).toHaveTextContent('Memory search: 100 ms');
	expect(container).toHaveTextContent('1.2k tok');
	expect(container).toHaveTextContent('$0.000');
});

/** An AgentEvent-enveloped frame, as the per-turn projection stores it. */
function agentEvent(eventType: string, ts: number, payload: Record<string, unknown>) {
	return {
		event_type: 'AgentEvent',
		timestamp_ms: ts,
		data: { event: { event_type: eventType, agent_id: 'chat', payload: { timestamp_ms: ts, ...payload } } }
	};
}

// One running web fetch (URL arg) and one finished file read.
const FIXTURE_EVENTS = [
	{
		event_type: 'tool.call.started',
		timestamp_ms: 1,
		data: {
			call_id: 'c1',
			tool_name: 'web_fetch',
			args: { url: 'https://www.rust-lang.org/learn' }
		}
	},
	{
		event_type: 'tool.call.started',
		timestamp_ms: 2,
		data: { call_id: 'c2', tool_name: 'read_file', args: { path: '/tmp/work/notes.md' } }
	},
	{
		event_type: 'tool.call.finished',
		timestamp_ms: 3,
		data: { call_id: 'c2', tool_name: 'read_file', duration_ms: 12 }
	}
];

beforeEach(() => {
	resetTaskUsage();
	store.events.clear();
	store.usage.clear();
	seedUsage('__none__', null);
	store.unsubscribe.calls = 0;
	vi.mocked(eventsStore.subscribeToChatTurn).mockClear();
	vi.mocked(eventsStore.fetchEventsPage).mockClear();
	seed('turn-1', FIXTURE_EVENTS);
});

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it('lets the live subscription own its initial REST read', async () => {
	render(RequestActivityCard, { sessionId: 's1', chatTurnId: 'live-turn', live: true });
	await tick();
	expect(eventsStore.subscribeToChatTurn).toHaveBeenCalledTimes(1);
	expect(eventsStore.fetchEventsPage).not.toHaveBeenCalled();
});

it('defers offscreen historical event reads until the card approaches the viewport', async () => {
	let reveal!: (entries: Array<{ isIntersecting: boolean }>) => void;
	vi.stubGlobal('IntersectionObserver', class {
		constructor(callback: typeof reveal) { reveal = callback; }
		observe() {}
		disconnect() {}
	});
	render(RequestActivityCard, { sessionId: 's1', chatTurnId: 'offscreen-turn' });
	await tick();
	expect(eventsStore.fetchEventsPage).not.toHaveBeenCalled();
	reveal([{ isIntersecting: true }]);
	await tick();
	expect(eventsStore.fetchEventsPage).toHaveBeenCalledTimes(1);
	reveal([{ isIntersecting: true }]);
	await tick();
	expect(eventsStore.fetchEventsPage).toHaveBeenCalledTimes(1);
});

it('loads historical steps on explicit expansion even before the visibility callback', async () => {
	vi.stubGlobal('IntersectionObserver', class { observe() {} disconnect() {} });
	const { container } = render(RequestActivityCard, { sessionId: 's1', chatTurnId: 'offscreen-open' });
	await tick();
	expect(eventsStore.fetchEventsPage).not.toHaveBeenCalled();
	await fireEvent.click(container.querySelector('.request-activity__header')!);
	expect(eventsStore.fetchEventsPage).toHaveBeenCalledTimes(1);
});

it('observes an embedded turn even while its empty activity strip is hidden', async () => {
	let reveal!: (entries: Array<{ isIntersecting: boolean }>) => void;
	vi.stubGlobal('IntersectionObserver', class {
		constructor(callback: typeof reveal) { reveal = callback; }
		observe() {}
		disconnect() {}
	});
	const { container } = render(RequestActivityCard, { sessionId: 's1', chatTurnId: 'hidden-turn', embedded: true });
	await tick();
	expect(container.querySelector('.request-activity')).toBeNull();
	expect(container.querySelector('.activity-viewport-marker')).not.toBeNull();
	reveal([{ isIntersecting: true }]);
	await tick();
	expect(eventsStore.fetchEventsPage).toHaveBeenCalledTimes(1);
	seed('hidden-turn', FIXTURE_EVENTS);
	await tick();
	expect(container.querySelector('.request-activity')).not.toBeNull();
});

describe('RequestActivityCard', () => {
	it('uses the subscribed task history even when the chat replay contains only a tiny tail', async () => {
		const taskId = 'task_12345678901234567890123456789012';
		const state = {
			overview: { task_id: taskId, execution_id: 'exec-1', updated_at: 1 },
			run: { activity_log: ['call-a', 'call-b'].flatMap((callId) => ['requested', 'succeeded'].map((phase) => ({
				id: `${callId}-${phase}`, status: phase === 'requested' ? 'running' : 'done', created_at: 1,
				metadata: { event_type: `llm.${phase}`, llm_call_id: callId, execution_id: 'exec-1',
					...(phase === 'succeeded' ? { input_tokens: 1000, output_tokens: 200 } : {}) }
			}))) }, output: {}, debug: {}
		} as unknown as ExecutionPanelState;
		recordTaskUsageSnapshot(taskId, state);
		seed('task-tail', [{ event_type: 'LLMResponseReceived', data: {
			input_tokens: 0, output_tokens: 0, cost: 0, timestamp: 2,
			correlation: { llm_call_id: 'judge', usage_availability: { tokens: false, cost: false } }
		} }]);
		const props = { sessionId: 's1', chatTurnId: 'task-tail', live: true, controlTaskId: taskId, controlExecutionId: 'exec-1' };
		const first = render(RequestActivityCard, props);
		expect(first.container.querySelector('.request-activity__cache-detail')).toHaveTextContent('2 calls · 2.4k tok');
		expect(first.container.querySelector('.request-activity__count')).toHaveTextContent('2');
		await first.unmount();
		// A remount gets an even shorter replay; the recorded history remains.
		seed('task-tail', []);
		const second = render(RequestActivityCard, props);
		expect(second.container.querySelector('.request-activity__count')).toHaveTextContent('2');
		recordTaskLlmCall(taskId, 'call-c', 500, null, 'exec-1');
		await tick();
		expect(second.container.querySelector('.request-activity__cache-detail')).toHaveTextContent('3 calls · 2.9k tok');
	});

	it('labels a priced call with unreported tokens as unavailable', () => {
		recordTaskLlmCall('task-priced', 'call-a', null, 0.01);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1', chatTurnId: 'turn-priced', controlTaskId: 'task-priced', live: true
		});
		expect(container.querySelector('.request-activity__cache-detail')).toHaveTextContent('1 call · tokens unavailable');
		expect(container.querySelector('.request-activity__cache-detail')).not.toHaveTextContent('0 tok');
	});

	it('renders the classic row list by default', () => {
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-1',
			live: true,
			embedded: true
		});

		expect(container.querySelectorAll('.request-activity__row')).toHaveLength(2);
		expect(container.querySelector('.request-activity__tail')).toHaveTextContent('Calling web_fetch');
		expect(container.querySelector('[data-testid="turn-stage"]')).toBeNull();
	});

	it('renders TurnStage for a live turn with variant="next"', () => {
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-1',
			live: true,
			embedded: true,
			variant: 'next'
		});

		const chip = container.querySelector('[data-testid="turn-stage-chip"]');
		expect(chip).toHaveTextContent('Calling web_fetch');
		expect(chip).toHaveTextContent('rust-lang.org');
		expect(chip).not.toHaveTextContent('www.');
		expect(chip?.querySelector('[data-glyph="web"]')).not.toBeNull();
		const dots = container.querySelectorAll('.cn-stage__trail .cn-stage__dot');
		expect(dots).toHaveLength(1);
		expect(dots[0]).toHaveAttribute('data-status', 'done');
		// Header (count, controls) stays; only the row list is swapped.
		expect(container.querySelector('.request-activity__header')).not.toBeNull();
		expect(container.querySelector('.request-activity__tail')).toBeNull();
	});

	it('keeps the classic list for a historical turn even with variant="next"', () => {
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-1',
			live: false,
			embedded: true,
			variant: 'next'
		});

		expect(container.querySelector('[data-testid="turn-stage"]')).toBeNull();
		// Historical cards start collapsed: header only, classic behaviour.
		expect(container.querySelector('.request-activity__header')).not.toBeNull();
	});

	it('renders no TurnStage before the first activity row lands', () => {
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-empty',
			live: true,
			embedded: true,
			variant: 'next'
		});

		expect(container.querySelector('[data-testid="turn-stage"]')).toBeNull();
	});

	it('keeps a known tool name when a later tool.* event carries none', () => {
		seed('turn-projected', [
			...FIXTURE_EVENTS,
			{
				event_type: 'tool.result.projected',
				timestamp_ms: 4,
				data: { call_id: 'c1' }
			}
		]);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-projected',
			live: true,
			embedded: true,
			variant: 'next'
		});

		const chip = container.querySelector('[data-testid="turn-stage-chip"]');
		expect(chip).toHaveTextContent('Calling web_fetch');
		expect(chip).toHaveTextContent('rust-lang.org');
		// The glyph is picked from the tool meta; the literal 'tool' fallback
		// would lose the web glyph.
		expect(chip?.querySelector('[data-glyph="web"]')).not.toBeNull();
	});

	it('styles "Inspect run →" with the shared action chip', () => {
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-1',
			live: true,
			embedded: true,
			controlTaskId: 'task-1',
			controlExecutionId: 'exec-1'
		});

		const inspect = container.querySelector('.request-activity__inspect');
		expect(inspect).toHaveTextContent('Inspect run →');
		expect(inspect).toHaveClass('mg-chip-action');
	});
	describe('delegated agentic steps', () => {
		const action = {
			execution_id: 'exec-d',
			plan_id: 'plan-1',
			step_id: 'step-1',
			iteration: 1,
			action_type: 'pack',
			target: 'shell'
		};
		const started = (data: Record<string, unknown>) => ({
			event_type: 'AgenticActionStarted',
			data: { ...action, timestamp: 10, ...data }
		});
		const executed = (data: Record<string, unknown>) => ({
			event_type: 'AgenticActionExecuted',
			data: { ...action, success: true, latency_ms: 25, timestamp: 40, ...data }
		});
		const renderTurn = (turn: string) =>
			render(RequestActivityCard, { sessionId: 's1', chatTurnId: turn, live: true, embedded: true });

		const nothingToCancel = () =>
			vi.fn(async (_input: RequestInfo | URL) =>
				Response.json(
					{ code: 'invalid_request', error: 'No active execution to cancel', details: { cancelled: false } },
					{ status: 400 }
				)
			);

		it('Stop on a run with nothing left to cancel settles the turn only the client showed as live', async () => {
			resetChatTurnStates();
			const fetchMock = nothingToCancel();
			vi.stubGlobal('fetch', fetchMock);
			try {
				seed('turn-stop', [started({ call_id: 'act-1', detail: null })]);
				const { container } = render(RequestActivityCard, {
					sessionId: 's1',
					chatTurnId: 'turn-stop',
					live: true,
					embedded: true,
					controlTaskId: 'task-1',
					controlExecutionId: 'exec-d',
					stopsTurnWhenRunEnded: true
				});
				await tick();
				const stop = container.querySelector('.request-activity__stop');
				expect(stop).not.toBeNull();
				await fireEvent.click(stop as Element);
				await vi.waitFor(() =>
					expect(get(chatTurnStateStore).get('turn-stop')).toMatchObject({
						status: 'stopped',
						chat_session_id: 's1',
						client_settled: true
					})
				);
				// Only the run's cancel was asked; the chat turn itself was not.
				expect(fetchMock).toHaveBeenCalledTimes(1);
				expect(String(fetchMock.mock.calls[0]?.[0])).toContain('/executions/exec-d/cancel');
				await tick();
				expect(container.querySelector('.request-activity__stop')).toBeNull();
			} finally {
				vi.unstubAllGlobals();
				resetChatTurnStates();
			}
		});

		it('Stop leaves the turn alone unless the panel says only the client keeps it live', async () => {
			resetChatTurnStates();
			const fetchMock = nothingToCancel();
			vi.stubGlobal('fetch', fetchMock);
			try {
				seed('turn-other', [started({ call_id: 'act-1', detail: null })]);
				const { container } = render(RequestActivityCard, {
					sessionId: 's1',
					chatTurnId: 'turn-other',
					live: true,
					embedded: true,
					controlTaskId: 'task-1',
					controlExecutionId: 'exec-d'
				});
				await tick();
				await fireEvent.click(container.querySelector('.request-activity__stop') as Element);
				await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
				// Let the cancel handler finish reading the answer.
				await new Promise((resolve) => setTimeout(resolve, 0));
				await tick();
				expect(get(chatTurnStateStore).has('turn-other')).toBe(false);
				expect(container.querySelector('.request-activity__stop')).not.toBeNull();
			} finally {
				vi.unstubAllGlobals();
			}
		});

		it('opens a running row at start and settles the same row by call_id', async () => {
			seed('turn-agentic', [started({ call_id: 'act-1', detail: null })]);
			const { container } = renderTurn('turn-agentic');
			await tick();

			let rows = container.querySelectorAll('.request-activity__row');
			expect(rows).toHaveLength(1);
			expect(rows[0]).toHaveClass('request-activity__row--running');
			expect(rows[0]).toHaveTextContent('Calling shell');

			seed('turn-agentic', [started({ call_id: 'act-1', detail: null }), executed({ call_id: 'act-1' })]);
			await tick();
			rows = container.querySelectorAll('.request-activity__row');
			expect(rows).toHaveLength(1);
			expect(rows[0]).toHaveClass('request-activity__row--done');
			expect(rows[0]).toHaveTextContent('shell returned');
		});

		it('still renders a legacy executed event without call_id', async () => {
			seed('turn-legacy', [executed({})]);
			const { container } = renderTurn('turn-legacy');
			await tick();

			const rows = container.querySelectorAll('.request-activity__row');
			expect(rows).toHaveLength(1);
			expect(rows[0]).toHaveClass('request-activity__row--done');
			expect(rows[0]).toHaveTextContent('shell returned');
		});

		it('labels a step with its server detail', async () => {
			const detail = 'metabase-mcp · execute_sql — SELECT id FROM orders';
			seed('turn-detail', [started({ call_id: 'act-2', target: 'metabase-mcp__call_tool', detail })]);
			const { container } = renderTurn('turn-detail');
			await tick();

			const row = container.querySelector('.request-activity__row');
			expect(row).toHaveTextContent(`Calling ${detail}`);
			expect(row).not.toHaveTextContent('__call_tool');
		});
	});

	it('names a chat MCP call_tool step by pack and remote tool', async () => {
		const sql = 'SELECT id, total FROM orders WHERE total > 100';
		seed('turn-mcp', [
			{
				event_type: 'tool.call.started',
				timestamp_ms: 1,
				data: {
					call_id: 'm1',
					tool_name: 'metabase-mcp__call_tool',
					args: { name: 'execute_sql', arguments_json: JSON.stringify({ database_id: 2, sql }) }
				}
			}
		]);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-mcp',
			live: true,
			embedded: true,
			variant: 'next'
		});
		await tick();

		const chip = container.querySelector('[data-testid="turn-stage-chip"]');
		expect(chip).toHaveTextContent('Calling metabase-mcp · execute_sql');
		expect(chip).toHaveTextContent(sql);
		expect(chip).not.toHaveTextContent('database_id');

		// The finish carries no args: the row keeps the derived name.
		seed('turn-mcp', [
			...(store.events.get('turn-mcp') ?? []),
			{
				event_type: 'tool.call.finished',
				timestamp_ms: 2,
				data: { call_id: 'm1', tool_name: 'metabase-mcp__call_tool', duration_ms: 9 }
			}
		]);
		await tick();
		const dot = container.querySelector('.cn-stage__trail .cn-stage__dot');
		expect(dot).toHaveAttribute('data-status', 'done');
		expect(dot?.closest('[title]')).toHaveAttribute(
			'title',
			'metabase-mcp · execute_sql returned (done)'
		);
	});

	describe('after the turn ends', () => {
		// A step that lost its terminal event: an agentic start with no
		// finish, an LLM request with no response.
		const ORPHANS = [
			{
				event_type: 'AgenticActionStarted',
				data: {
					execution_id: 'exec-d',
					plan_id: 'p',
					step_id: 's',
					iteration: 1,
					action_type: 'pack',
					target: 'shell',
					call_id: 'orphan-1',
					detail: null,
					timestamp: 10
				}
			},
			{
				event_type: 'tool.call.started',
				timestamp_ms: 11,
				data: { call_id: 'orphan-2', tool_name: 'web_fetch' }
			}
		];

		it('shows no spinner once the card is no longer live', async () => {
			seed('turn-ended', ORPHANS);
			const { container, rerender } = render(RequestActivityCard, {
				sessionId: 's1',
				chatTurnId: 'turn-ended',
				live: true,
				embedded: true
			});
			await tick();
			expect(container.querySelectorAll('.request-activity__row--running')).toHaveLength(2);
			expect(container.querySelector('.request-activity__spinner')).not.toBeNull();

			// The reply landed / the task finished: the host drops `live`.
			await rerender({ live: false });
			await tick();
			expect(container.querySelector('.request-activity__spinner')).toBeNull();
			// A non-live card collapses to its header; open it to see the rows.
			(container.querySelector('.request-activity__header') as HTMLButtonElement).click();
			await tick();
			expect(container.querySelectorAll('.request-activity__row--running')).toHaveLength(0);
			expect(container.querySelectorAll('.request-activity__row--done')).toHaveLength(2);
			expect(container.querySelector('.request-activity')).toHaveClass('request-activity--complete');
		});
	});

	describe('typed LLM calls, paired by llm_call_id', () => {
		const base = { execution_id: 'exec-d', plan_id: 'plan-1', step_id: 'step-1', capability: 'decision' };
		const correlation = (id: string) => ({ trace_id: 'trace-run', llm_call_id: id });
		const sent = (id: string | null, ts: number) => ({
			event_type: 'LLMRequestSent',
			data: {
				...base,
				request_summary: 'decide',
				budget_remaining: 1,
				timestamp: ts,
				...(id ? { correlation: correlation(id) } : {})
			}
		});
		const routed = (id: string, ts: number, model: string) => ({
			event_type: 'LLMRouted',
			data: { ...base, correlation: correlation(id), provider: 'openai', model, profile: 'fast', timestamp: ts }
		});
		const received = (id: string | null, ts: number, extra: Record<string, unknown> = {}) => ({
			event_type: 'LLMResponseReceived',
			data: {
				...base,
				success: true,
				decision_summary: 'call shell',
				cost: 0,
				latency_ms: 1200,
				timestamp: ts,
				...(id ? { correlation: correlation(id) } : {}),
				...extra
			}
		});
		const renderTurn = (turn: string) =>
			render(RequestActivityCard, { sessionId: 's1', chatTurnId: turn, live: true, embedded: true });
		const rowsOf = (container: HTMLElement) => container.querySelectorAll('.request-activity__row');

		it('opens a running row on the request, names the routed model, and settles the same row', async () => {
			seed('turn-typed', [sent('call-1', 10)]);
			const { container } = renderTurn('turn-typed');
			await tick();
			expect(rowsOf(container)).toHaveLength(1);
			expect(rowsOf(container)[0]).toHaveClass('request-activity__row--running');
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking…');

			seed('turn-typed', [sent('call-1', 10), routed('call-1', 11, 'gpt-6.1-sol')]);
			await tick();
			expect(rowsOf(container)).toHaveLength(1);
			expect(rowsOf(container)[0]).toHaveClass('request-activity__row--running');
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking with gpt-6.1-sol');

			seed('turn-typed', [sent('call-1', 10), routed('call-1', 11, 'gpt-6.1-sol'), received('call-1', 20)]);
			await tick();
			expect(rowsOf(container)).toHaveLength(1);
			expect(rowsOf(container)[0]).toHaveClass('request-activity__row--done');
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking with gpt-6.1-sol');
			expect(rowsOf(container)[0]).toHaveTextContent('1200ms');
		});

		it('follows a fallback re-route to the new model', async () => {
			seed('turn-reroute', [
				sent('call-1', 10),
				routed('call-1', 11, 'gpt-6.1-sol'),
				routed('call-1', 12, 'claude-opus-5-5')
			]);
			const { container } = renderTurn('turn-reroute');
			await tick();
			expect(rowsOf(container)).toHaveLength(1);
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking with claude-opus-5-5');
		});

		it('keeps one row per response on a server without llm_call_id', async () => {
			seed('turn-old', [sent(null, 10), received(null, 20), sent(null, 30), received(null, 40)]);
			const { container } = renderTurn('turn-old');
			await tick();
			const rows = rowsOf(container);
			expect(rows).toHaveLength(2);
			for (const row of rows) expect(row).toHaveClass('request-activity__row--done');
		});

		it('folds the chat-side and typed copies of one call into one row, in either order', async () => {
			const chatRequested = agentEvent('llm.requested', 10, { event_id: 'q', iteration: 1, model: 'chat-profile' });
			const chatSucceeded = agentEvent('llm.succeeded', 13, {
				event_id: 's',
				iteration: 1,
				llm_call_id: 'call-own',
				model: 'claude-opus-5-5',
				duration_ms: 900
			});
			seed('turn-copies-a', [chatRequested, received('call-own', 12), chatSucceeded]);
			seed('turn-copies-b', [chatRequested, chatSucceeded, received('call-own', 14)]);
			for (const turn of ['turn-copies-a', 'turn-copies-b']) {
				const { container, unmount } = renderTurn(turn);
				await tick();
				const rows = rowsOf(container);
				expect(rows).toHaveLength(1);
				expect(rows[0]).toHaveClass('request-activity__row--done');
				expect(rows[0]).toHaveTextContent('Thinking with claude-opus-5-5');
				unmount();
			}
		});

		it('ticks a running row once a second and stops when it settles', async () => {
			vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
			vi.setSystemTime(100_000);
			try {
				seed('turn-tick', [sent('call-1', 95_000)]);
				const { container } = renderTurn('turn-tick');
				await tick();
				const elapsed = () => container.querySelector('.request-activity__row-elapsed');
				expect(elapsed()).toHaveTextContent('0:05');

				vi.advanceTimersByTime(1_000);
				await tick();
				expect(elapsed()).toHaveTextContent('0:06');

				seed('turn-tick', [sent('call-1', 95_000), received('call-1', 101_000)]);
				await tick();
				expect(elapsed()).toBeNull();
				expect(rowsOf(container)[0]).toHaveTextContent('1200ms');
			} finally {
				vi.useRealTimers();
			}
		});
	});

	it('relabels a thinking row with the model that answered', () => {
		seed('turn-llm', [
			agentEvent('llm.requested', 10, { trace_id: 'tr-1', model: 'native-profile' }),
			agentEvent('llm.succeeded', 20, {
				trace_id: 'tr-1',
				model: 'claude-opus-5-5',
				provider: 'anthropic',
				duration_ms: 900
			})
		]);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-llm',
			live: true,
			embedded: true
		});
		const tail = container.querySelector('.request-activity__tail');
		expect(tail).toHaveTextContent('Thinking with claude-opus-5-5');
		expect(tail).not.toHaveTextContent('native-profile');
	});

	it('keeps the running label when the settled event names no model', () => {
		seed('turn-llm-bare', [
			agentEvent('llm.requested', 10, { trace_id: 'tr-1', model: 'Claude Code · opus' }),
			agentEvent('llm.succeeded', 20, { trace_id: 'tr-1', duration_ms: 900 })
		]);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-llm-bare',
			live: true,
			embedded: true
		});
		expect(container.querySelector('.request-activity__tail')).toHaveTextContent(
			'Thinking with Claude Code · opus'
		);
	});

	describe('a task row whose final answer is being written', () => {
		const taskEvent = (ts: number, payload: Record<string, unknown>) =>
			agentEvent('task.status_changed', ts, {
				task_id: 'task_synth',
				display_label: 'Researcher',
				...payload
			});
		const renderTask = (turnId: string) =>
			render(RequestActivityCard, {
				sessionId: 's1',
				chatTurnId: turnId,
				live: true,
				embedded: true
			});
		const rowText = (container: HTMLElement) =>
			container.querySelector('.request-activity__row')?.textContent ?? '';

		it('restarts the row clock when the synthesis flag flips, so the timer counts synthesis time', async () => {
			vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
			vi.setSystemTime(100_000);
			try {
				seed('turn-synth', [
					taskEvent(10_000, { status: 'running' }),
					taskEvent(95_000, { status: 'running', synthesis_pending: true })
				]);
				const { container } = renderTask('turn-synth');
				await tick();

				expect(rowText(container)).toContain('Researcher preparing final result');
				const elapsed = () => container.querySelector('.request-activity__row-elapsed');
				// 5 s since the flag, not 90 s since the row first appeared.
				expect(elapsed()).toHaveTextContent('0:05');

				vi.advanceTimersByTime(1_000);
				await tick();
				expect(elapsed()).toHaveTextContent('0:06');
			} finally {
				vi.useRealTimers();
			}
		});

		it('keeps the restarted clock on a repeated flagged event', async () => {
			vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
			vi.setSystemTime(100_000);
			try {
				seed('turn-synth-repeat', [
					taskEvent(10_000, { status: 'running' }),
					taskEvent(90_000, { status: 'running', synthesis_pending: true }),
					taskEvent(98_000, { status: 'running', synthesis_pending: true })
				]);
				const { container } = renderTask('turn-synth-repeat');
				await tick();

				expect(container.querySelector('.request-activity__row-elapsed')).toHaveTextContent('0:10');
			} finally {
				vi.useRealTimers();
			}
		});

		it('settles on the terminal event once the flag clears', async () => {
			vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
			vi.setSystemTime(100_000);
			try {
				seed('turn-synth-done', [
					taskEvent(10_000, { status: 'running' }),
					taskEvent(95_000, { status: 'running', synthesis_pending: true })
				]);
				const { container } = renderTask('turn-synth-done');
				await tick();
				expect(container.querySelector('.request-activity__row-elapsed')).not.toBeNull();

				seed('turn-synth-done', [
					taskEvent(10_000, { status: 'running' }),
					taskEvent(95_000, { status: 'running', synthesis_pending: true }),
					taskEvent(101_000, { status: 'completed', synthesis_pending: false })
				]);
				await tick();
				expect(container.querySelector('.request-activity__row-elapsed')).toBeNull();
				expect(rowText(container)).toContain('Researcher completed');
			} finally {
				vi.useRealTimers();
			}
		});
	});

	it('settles a running task row from a terminal transcript card', () => {
		seed('turn-task', [
			agentEvent('chat.delegate.status_changed', 10, {
				task_id: 'task_1',
				status: 'running',
				display_label: 'Researcher'
			})
		]);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-task',
			live: false,
			embedded: true,
			transcriptTaskStatuses: new Map([['task_1', 'failed']])
		});
		expect(container.querySelector('.request-activity__spinner')).toBeNull();
	});

	it('keeps tailing and re-reads the turn once when live flips off with a task still running', async () => {
		seed('turn-tail', [
			agentEvent('chat.delegate.status_changed', 10, {
				task_id: 'task_2',
				status: 'running',
				display_label: 'Coder'
			})
		]);
		const { container, rerender } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-tail',
			live: true,
			embedded: true
		});
		expect(eventsStore.subscribeToChatTurn).toHaveBeenCalledTimes(1);
		const fetchesBefore = vi.mocked(eventsStore.fetchEventsPage).mock.calls.length;

		await rerender({ sessionId: 's1', chatTurnId: 'turn-tail', live: false, embedded: true });
		// Still subscribed: the delegated task has not settled.
		expect(store.unsubscribe.calls).toBe(0);
		expect(vi.mocked(eventsStore.fetchEventsPage).mock.calls.length).toBe(fetchesBefore + 1);
		expect(container.querySelector('.request-activity__spinner')).not.toBeNull();

		// The terminal event lands on the kept tail: the row settles, the tail drops.
		seed('turn-tail', [
			agentEvent('chat.delegate.status_changed', 10, {
				task_id: 'task_2',
				status: 'running',
				display_label: 'Coder'
			}),
			agentEvent('chat.delegate.status_changed', 20, {
				task_id: 'task_2',
				status: 'failed',
				display_label: 'Coder'
			})
		]);
		await Promise.resolve();
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(store.unsubscribe.calls).toBe(1);
		expect(container.querySelector('.request-activity__spinner')).toBeNull();
	});

	it('shows the server whole-turn usage instead of summing a truncated page', async () => {
		seed('turn-usage', [
			agentEvent('llm.requested', 10, { trace_id: 'a', event_id: 'req-a', model: 'm' }),
			agentEvent('llm.succeeded', 11, {
				trace_id: 'a',
				event_id: 'ok-a',
				input_tokens: 100,
				output_tokens: 10,
				cost_usd: 0.01
			})
		]);
		seedUsage('turn-usage', {
			calls: 25,
			inputTokens: 50_000,
			outputTokens: 4_000,
			costUsd: 1.25,
			truncated: true,
			coveredKeys: new Set(['req-a', 'ok-a'])
		});
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-usage',
			live: false,
			embedded: true
		});
		expect(container.querySelector('.request-activity__cache-detail')).toHaveTextContent('25 calls');

		// A live call after the read adds on top; the covered one is not re-counted.
		seed('turn-usage', [
			...(store.events.get('turn-usage') as unknown[]),
			agentEvent('llm.succeeded', 30, {
				trace_id: 'b',
				event_id: 'ok-b',
				input_tokens: 10,
				output_tokens: 1
			})
		]);
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(container.querySelector('.request-activity__cache-detail')).toHaveTextContent('26 calls');
	});
	it('counts only the turn own calls once, leaving delegated calls to the task store', () => {
		seed('turn-mixed', [
			agentEvent('llm.succeeded', 10, {
				event_id: 'own-a',
				llm_call_id: 'call-own',
				execution_id: 'turn-mixed',
				input_tokens: 1000,
				output_tokens: 100,
				cost_usd: 2
			}),
			{
				event_type: 'LLMResponseReceived',
				timestamp_ms: 11,
				data: {
					execution_id: 'turn-mixed',
					correlation: { llm_call_id: 'call-own' },
					input_tokens: 1000,
					output_tokens: 100,
					cost: 2,
					timestamp_ms: 11
				}
			},
			{
				event_type: 'LLMResponseReceived',
				timestamp_ms: 12,
				data: {
					execution_id: 'exec_task',
					correlation: { llm_call_id: 'task-call-1' },
					input_tokens: 500,
					output_tokens: 50,
					cost: 3,
					timestamp_ms: 12
				}
			}
		]);
		const { container } = render(RequestActivityCard, {
			sessionId: 's1',
			chatTurnId: 'turn-mixed',
			live: false,
			embedded: true
		});
		const detail = container.querySelector('.request-activity__cache-detail');
		expect(detail).toHaveTextContent('1 call ·');
		expect(detail).toHaveTextContent('$2.00');
	});
});
