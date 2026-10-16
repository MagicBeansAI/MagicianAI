import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import ChatTurnProgress from './ChatTurnProgress.svelte';
import { MockWebSocket } from '../../../test/browser';

// One running LLM call, then a running web read. The web read is the latest
// running row, so the next variant's working chip should show it.
const FIXTURE_EVENTS = [
	{
		event_type: 'llm.requested',
		timestamp_ms: 1,
		data: { event_id: 'e1', trace_id: 't1', model: 'test-model' }
	},
	{
		event_type: 'tool.call.started',
		timestamp_ms: 2,
		data: {
			event_id: 'e2',
			call_id: 'c1',
			tool_name: 'web_fetch',
			args: { url: 'https://www.rust-lang.org/learn' }
		}
	}
];

const fetchUrls: string[] = [];

/** The bubble's history is one `backfill_only` REST request; live events come
 *  from the app's event hub. */
function stubEventStream(events: unknown[]): void {
	vi.stubGlobal(
		'fetch',
		vi.fn(async (input: RequestInfo | URL) => {
			fetchUrls.push(String(input instanceof Request ? input.url : input));
			return new Response(events.map((event) => `${JSON.stringify(event)}\n`).join(''));
		})
	);
}

beforeEach(() => {
	fetchUrls.length = 0;
	vi.spyOn(console, 'debug').mockImplementation(() => {});
	stubEventStream(FIXTURE_EVENTS);
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('ChatTurnProgress', () => {
	it('renders the classic row list by default', async () => {
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });

		await vi.waitFor(() =>
			expect(container.querySelectorAll('.chat-turn-progress__row')).toHaveLength(2)
		);
		expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('Calling web_fetch');
		expect(container.querySelector('[data-testid="turn-stage"]')).toBeNull();
	});

	it('renders TurnStage with variant="next"', async () => {
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1', variant: 'next' });

		await vi.waitFor(() =>
			expect(container.querySelector('[data-testid="turn-stage-chip"]')).toHaveTextContent(
				'Calling web_fetch'
			)
		);
		const chip = container.querySelector('[data-testid="turn-stage-chip"]');
		expect(chip).toHaveTextContent('rust-lang.org');
		expect(chip?.querySelector('[data-glyph="web"]')).not.toBeNull();
		expect(container.querySelector('.chat-turn-progress')).not.toBeNull();
		expect(container.querySelector('.chat-turn-progress__list')).toBeNull();
	});

	it('drops the bubble chrome with bare, keeping the stage', async () => {
		const { container } = render(ChatTurnProgress, {
			executionId: 'exec-1',
			variant: 'next',
			bare: true
		});

		await vi.waitFor(() =>
			expect(container.querySelector('[data-testid="turn-stage-chip"]')).toHaveTextContent(
				'Calling web_fetch'
			)
		);
		expect(container.querySelector('.chat-header')).toBeNull();
		expect(container.querySelector('.chat-bubble')).toBeNull();
		expect(container).not.toHaveTextContent('Assistant');
		expect(container).not.toHaveTextContent('Working…');
	});

	it('keeps a compact "Waiting on you" state with bare', async () => {
		stubEventStream([
			...FIXTURE_EVENTS,
			{ event_type: 'input.requested', timestamp_ms: 3, data: { event_id: 'e3' } }
		]);
		const { container } = render(ChatTurnProgress, {
			executionId: 'exec-1',
			variant: 'next',
			bare: true
		});

		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__pause')).toHaveTextContent(
				'Waiting on you'
			)
		);
		expect(container.querySelector('.chat-header')).toBeNull();
	});

	it('stops waiting on the reader once the ask is resolved', async () => {
		stubEventStream([
			{
				event_type: 'hitl.requested',
				timestamp_ms: 3,
				data: { event_id: 'e3', correlation_id: 'plan_1', prompt: 'Plan ready for review' }
			},
			{
				event_type: 'hitl.resolved',
				timestamp_ms: 4,
				data: { event_id: 'e4', correlation_id: 'plan_1' }
			}
		]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });

		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('Answered')
		);
		expect(container.querySelector('.chat-turn-progress--paused')).toBeNull();
		expect(container).not.toHaveTextContent('Waiting on your input');
	});

	it("shows only its own task's events when scoped to a task", async () => {
		stubEventStream([
			{
				event_type: 'tool.call.started',
				timestamp_ms: 1,
				data: { event_id: 'mine', call_id: 'c1', tool_name: 'web_fetch', task_id: 'task-1' }
			},
			{
				event_type: 'tool.call.started',
				timestamp_ms: 2,
				data: { event_id: 'theirs', call_id: 'c2', tool_name: 'shell_exec', task_id: 'task-2' }
			},
			{
				event_type: 'tool.call.started',
				timestamp_ms: 3,
				data: { event_id: 'unattributed', call_id: 'c3', tool_name: 'file_read' }
			}
		]);
		const { container } = render(ChatTurnProgress, { taskId: 'task-1', executionId: 'exec-1' });

		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('Calling web_fetch')
		);
		expect(container).not.toHaveTextContent('shell_exec');
		expect(container).not.toHaveTextContent('file_read');
	});

	it('keeps the bubble chrome by default', async () => {
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1', variant: 'next' });

		await vi.waitFor(() =>
			expect(container.querySelector('[data-testid="turn-stage-chip"]')).not.toBeNull()
		);
		expect(container.querySelector('.chat-header')).toHaveTextContent('Assistant');
		expect(container).toHaveTextContent('Working…');
	});

	it('reads history with one backfill-only request, never a held stream', async () => {
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });
		await vi.waitFor(() =>
			expect(container.querySelectorAll('.chat-turn-progress__row')).toHaveLength(2)
		);
		const eventCalls = fetchUrls.filter((url) => url.includes('/api/magician/v3/events'));
		expect(eventCalls).toHaveLength(1);
		expect(eventCalls[0]).toContain('backfill_only=true');
		expect(eventCalls[0]).toContain('execution_id=exec-1');
	});

	it('renders live events delivered by the event hub', async () => {
		stubEventStream([]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });
		const socket = MockWebSocket.instances.at(-1);
		expect(socket).toBeDefined();
		socket!.open();
		// Wait for the (empty) backfill to settle so live events are not held.
		await vi.waitFor(() => expect(fetchUrls.length).toBeGreaterThan(0));
		await new Promise((resolve) => setTimeout(resolve, 0));
		socket!.receive(
			JSON.stringify({
				event_type: 'AgentEvent',
				data: {
					event: {
						event_type: 'tool.call.started',
						agent_id: 'personal-assistant',
						payload: { event_id: 'live-1', call_id: 'c9', tool_name: 'web_search' }
					}
				}
			})
		);
		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('web_search')
		);
	});

	describe('delegated agentic steps in a task card', () => {
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
		const rowsOf = (container: HTMLElement) =>
			container.querySelectorAll('.chat-turn-progress__row');

		it('shows a step running at start, then settles the same row by call_id', async () => {
			stubEventStream([started({ call_id: 'act-1', detail: null })]);
			const { container } = render(ChatTurnProgress, { taskId: 'task-d', executionId: 'exec-d' });
			const socket = MockWebSocket.instances.at(-1);
			socket!.open();

			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(rowsOf(container)[0]).toHaveClass('chat-turn-progress__row--running');
			expect(rowsOf(container)[0]).toHaveTextContent('Calling shell');

			await new Promise((resolve) => setTimeout(resolve, 0));
			socket!.receive(JSON.stringify(executed({ call_id: 'act-1' })));
			await vi.waitFor(() =>
				expect(rowsOf(container)[0]).toHaveClass('chat-turn-progress__row--done')
			);
			expect(rowsOf(container)).toHaveLength(1);
			expect(rowsOf(container)[0]).toHaveTextContent('shell returned');
		});

		it('still renders a legacy executed event without call_id', async () => {
			stubEventStream([executed({})]);
			const { container } = render(ChatTurnProgress, { taskId: 'task-d', executionId: 'exec-d' });

			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(rowsOf(container)[0]).toHaveClass('chat-turn-progress__row--done');
			expect(rowsOf(container)[0]).toHaveTextContent('shell returned');
		});

		it('labels a step with its server detail', async () => {
			const detail = 'metabase-mcp · execute_sql — SELECT id FROM orders';
			stubEventStream([started({ call_id: 'act-2', target: 'metabase-mcp__call_tool', detail })]);
			const { container } = render(ChatTurnProgress, { taskId: 'task-d', executionId: 'exec-d' });

			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(rowsOf(container)[0]).toHaveTextContent(`Calling ${detail}`);
			expect(container).not.toHaveTextContent('__call_tool');
		});
	});

	it('names a chat MCP call_tool step by pack and remote tool', async () => {
		const sql = 'SELECT id, total FROM orders WHERE total > 100';
		stubEventStream([
			{
				event_type: 'tool.call.started',
				timestamp_ms: 1,
				data: {
					event_id: 'm1-start',
					call_id: 'm1',
					tool_name: 'metabase-mcp__call_tool',
					args: { name: 'execute_sql', arguments_json: JSON.stringify({ database_id: 2, sql }) }
				}
			}
		]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1', variant: 'next' });

		await vi.waitFor(() =>
			expect(container.querySelector('[data-testid="turn-stage-chip"]')).toHaveTextContent(
				'Calling metabase-mcp · execute_sql'
			)
		);
		const chip = container.querySelector('[data-testid="turn-stage-chip"]');
		expect(chip).toHaveTextContent(sql);
		expect(chip).not.toHaveTextContent('database_id');
	});
});
