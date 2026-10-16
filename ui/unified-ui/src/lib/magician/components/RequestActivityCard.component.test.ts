import { cleanup, render } from '@testing-library/svelte';
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

vi.mock('$lib/stores/chatTurnEventsStore', async () => {
	const { writable } = await import('svelte/store');
	const chatTurnEventsStore = writable(store.events);
	const chatTurnUsageBaselineStore = writable(store.usage);
	return {
		chatTurnEventsStore,
		chatTurnUsageBaselineStore,
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

import * as eventsStore from '$lib/stores/chatTurnEventsStore';
import RequestActivityCard from './RequestActivityCard.svelte';

const seed = (eventsStore as unknown as { __seed: (id: string, e: unknown[]) => void }).__seed;
const seedUsage = (eventsStore as unknown as { __seedUsage: (id: string, u: unknown) => void })
	.__seedUsage;

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
	store.events.clear();
	store.usage.clear();
	seedUsage('__none__', null);
	store.unsubscribe.calls = 0;
	vi.mocked(eventsStore.subscribeToChatTurn).mockClear();
	vi.mocked(eventsStore.fetchEventsPage).mockClear();
	seed('turn-1', FIXTURE_EVENTS);
});

afterEach(() => cleanup());

describe('RequestActivityCard', () => {
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
