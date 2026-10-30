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
	it('shows one context phase with its search timing after replay', async () => {
		const event = (type:string, timestamp:number, payload:Record<string,unknown>) => ({event_type:'AgentEvent', timestamp_ms:timestamp, data:{event:{event_type:type,agent_id:'chat',payload:{chat_session_id:'s1',...payload}}}});
		stubEventStream([
			event('context.retrieval.started',1,{phase_id:'p'}),
			event('context.retrieval.completed',2,{phase_id:'p',duration_ms:123,status:'partial',summary:'Memory search: 100 ms · Procedures: 20 ms'}),
			event('context.retrieval.started',3,{phase_id:'p'}),
			event('context.retrieval.completed',4,{phase_id:'another',chat_session_id:'another-chat',duration_ms:50})
		]);
		const {container}=render(ChatTurnProgress,{sessionId:'s1', executionId:'exec-1'});
		await vi.waitFor(()=>expect(container.querySelectorAll('.chat-turn-progress__row')).toHaveLength(1));
		expect(container).toHaveTextContent('Context prepared (partial)');
		expect(container).toHaveTextContent('Memory search: 100 ms');
	});
	it('shows typed Decision Engine usage alongside elapsed time', async () => {
		stubEventStream([{event_type:'LLMResponseReceived',data:{execution_id:'exec-1',success:true,model:'jev-test',latency_ms:71,input_tokens:1200,output_tokens:0,cost:0,usage_reported:true,correlation:{llm_call_id:'de-1',usage_availability:{tokens:true,cost:true,cache_read:false}}}}]);
		const {container}=render(ChatTurnProgress,{executionId:'exec-1'});
		await vi.waitFor(()=>expect(container).toHaveTextContent('1.2k tok'));
		expect(container).toHaveTextContent('$0.000');
	});

	it('keeps reported usage when a typed model call fails', async () => {
		stubEventStream([{
			event_type: 'LLMResponseReceived', data: {
				execution_id: 'exec-1', success: false, error: 'Invalid model response',
				input_tokens: 1200, output_tokens: 10, cost: 0.025, usage_reported: true,
				correlation: { llm_call_id: 'failed-decision' }
			}
		}]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });
		await vi.waitFor(() => expect(container).toHaveTextContent('Invalid model response'));
		expect(container).toHaveTextContent('1.2k tok');
		expect(container).toHaveTextContent('$0.025');
	});
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

	it('stops waiting when the answer names the ask with an attempt suffix', async () => {
		stubEventStream([
			{
				event_type: 'input.requested',
				timestamp_ms: 3,
				data: { event_id: 'e3', pause_state_id: 'exec-1:direct-step-1', prompt: 'Prove you are human' }
			},
			{
				event_type: 'hitl.requested',
				timestamp_ms: 4,
				data: { event_id: 'e4', correlation_id: 'exec-1:direct-step-1~48b8', prompt: 'Prove you are human' }
			},
			{
				event_type: 'hitl.resolved',
				timestamp_ms: 5,
				data: { event_id: 'e5', correlation_id: 'exec-1:direct-step-1~48b8' }
			}
		]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });

		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('Answered')
		);
		expect(container.querySelector('.chat-turn-progress--paused')).toBeNull();
		expect(container).not.toHaveTextContent('Waiting on your input');
	});

	it('stops waiting once its execution resumes from a pause', async () => {
		stubEventStream([
			{
				event_type: 'input.requested',
				timestamp_ms: 3,
				data: { event_id: 'e3', pause_state_id: 'unmatched-ask', prompt: 'Check the page' }
			},
			{
				// The live hub frame: a typed transport event, fields under `data`.
				event_type: 'ExecutionStatusChanged',
				timestamp_ms: 4,
				data: {
					event_id: 'e4',
					execution_id: 'exec-1',
					previous_status: 'WaitingUser',
					new_status: 'Executing'
				}
			}
		]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });

		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('Answered')
		);
		expect(container.querySelector('.chat-turn-progress--paused')).toBeNull();
	});

	it('stops waiting when an answered ask resumes the execution into planning', async () => {
		stubEventStream([
			{
				event_type: 'input.requested',
				timestamp_ms: 3,
				data: { event_id: 'e3', pause_state_id: 'other-ask', prompt: 'Confirm the account' }
			},
			{
				event_type: 'ExecutionStatusChanged',
				timestamp_ms: 4,
				data: {
					event_id: 'e4',
					execution_id: 'exec-1',
					previous_status: 'WaitingUser',
					new_status: 'Planning'
				}
			}
		]);
		const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });

		await vi.waitFor(() =>
			expect(container.querySelector('.chat-turn-progress__list')).toHaveTextContent('Answered')
		);
		expect(container.querySelector('.chat-turn-progress--paused')).toBeNull();
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

	describe('typed LLM calls, paired by llm_call_id', () => {
		const base = { execution_id: 'exec-d', plan_id: 'plan-1', step_id: 'step-1', capability: 'decision' };
		const correlation = (id: string) => ({ trace_id: 'trace-run', llm_call_id: id });
		const sent = (id: string, ts: number, extra: Record<string, unknown> = {}) => ({
			event_type: 'LLMRequestSent',
			data: { ...base, request_summary: 'decide', budget_remaining: 1, timestamp: ts, correlation: correlation(id), ...extra }
		});
		const routed = (id: string, ts: number, model: string) => ({
			event_type: 'LLMRouted',
			data: { ...base, correlation: correlation(id), provider: 'openai', model, profile: 'fast', timestamp: ts }
		});
		const received = (id: string, ts: number) => ({
			event_type: 'LLMResponseReceived',
			data: {
				...base,
				correlation: correlation(id),
				success: true,
				decision_summary: 'call shell',
				cost: 0,
				latency_ms: 1200,
				timestamp: ts
			}
		});
		const rowsOf = (container: HTMLElement) => container.querySelectorAll('.chat-turn-progress__row');

		it('runs from the request, names the routed model, and settles the same row', async () => {
			stubEventStream([sent('call-1', 10), routed('call-1', 11, 'gpt-6.1-sol')]);
			const { container } = render(ChatTurnProgress, { taskId: 'task-d', executionId: 'exec-d' });
			const socket = MockWebSocket.instances.at(-1);
			socket!.open();

			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(rowsOf(container)[0]).toHaveClass('chat-turn-progress__row--running');
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking with gpt-6.1-sol');

			await new Promise((resolve) => setTimeout(resolve, 0));
			socket!.receive(JSON.stringify(received('call-1', 20)));
			await vi.waitFor(() =>
				expect(rowsOf(container)[0]).toHaveClass('chat-turn-progress__row--done')
			);
			expect(rowsOf(container)).toHaveLength(1);
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking with gpt-6.1-sol');
			expect(rowsOf(container)[0]).toHaveTextContent('1.2s');
		});

		it('says "Thinking…" until the call is routed', async () => {
			stubEventStream([sent('call-1', 10)]);
			const { container } = render(ChatTurnProgress, { taskId: 'task-d', executionId: 'exec-d' });
			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking…');
		});

		it("keeps an unscoped bubble to its own execution's calls", async () => {
			stubEventStream([
				sent('call-other', 10, { execution_id: 'background-worker' }),
				sent('call-own', 11, { execution_id: 'exec-1' })
			]);
			const { container } = render(ChatTurnProgress, { executionId: 'exec-1' });
			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(rowsOf(container)[0]).toHaveTextContent('Thinking…');
		});

		it('ticks a running row once a second', async () => {
			// Only the interval and the clock are faked, so the backfill fetch and
			// `waitFor` keep their real timers.
			vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
			vi.setSystemTime(100_000);
			try {
				stubEventStream([sent('call-1', 58_000)]);
				const { container } = render(ChatTurnProgress, { taskId: 'task-d', executionId: 'exec-d' });
				const elapsed = () => container.querySelector('.chat-turn-progress__elapsed');
				await vi.waitFor(() => expect(elapsed()).toHaveTextContent('0:42'));

				vi.advanceTimersByTime(1_000);
				await vi.waitFor(() => expect(elapsed()).toHaveTextContent('0:43'));
			} finally {
				vi.useRealTimers();
			}
		});
	});

	describe('while the final answer is being written', () => {
		const rowsOf = (container: HTMLElement) =>
			Array.from(container.querySelectorAll<HTMLElement>('.chat-turn-progress__row'));
		const synthRow = (container: HTMLElement) =>
			rowsOf(container).find((row) => row.textContent?.includes('Writing the final answer…'));
		const toolStarted = {
			event_type: 'tool.call.started',
			timestamp_ms: 1,
			data: { event_id: 'e-tool', call_id: 'c1', tool_name: 'web_fetch', task_id: 'task-s' }
		};
		const flagged = (timestamp: number, id = 'e-synth') => ({
			event_type: 'task.status_changed',
			timestamp_ms: timestamp,
			data: {
				event_id: id,
				task_id: 'task-s',
				status: 'running',
				synthesis_pending: true
			}
		});
		const executionCompleted = {
			event_type: 'execution.completed',
			timestamp_ms: 3,
			data: { event_id: 'e-exec', task_id: 'task-s' }
		};
		const terminal = (status: string, extra: Record<string, unknown> = {}) => ({
			event_type: 'task.status_changed',
			timestamp_ms: 9,
			data: { event_id: `e-${status}`, task_id: 'task-s', status, synthesis_pending: false, ...extra }
		});
		const render_ = () => render(ChatTurnProgress, { taskId: 'task-s', executionId: 'exec-s' });

		it('opens a running "Writing the final answer…" row for a flagged task.status_changed', async () => {
			stubEventStream([toolStarted, flagged(2)]);
			const { container } = render_();

			await vi.waitFor(() => expect(synthRow(container)).toBeDefined());
			expect(synthRow(container)).toHaveClass('chat-turn-progress__row--running');
		});

		it('keeps it running through execution.completed while the other rows settle', async () => {
			stubEventStream([toolStarted, flagged(2), executionCompleted]);
			const { container } = render_();

			await vi.waitFor(() => expect(synthRow(container)).toBeDefined());
			const tool = rowsOf(container).find((row) => row.textContent?.includes('Calling web_fetch'));
			expect(tool).toHaveClass('chat-turn-progress__row--done');
			expect(synthRow(container)).toHaveClass('chat-turn-progress__row--running');
		});

		it('keeps it running through agentic.execution_completed too', async () => {
			stubEventStream([
				flagged(2),
				{
					event_type: 'agentic.execution_completed',
					timestamp_ms: 3,
					data: { event_id: 'e-agentic', task_id: 'task-s' }
				}
			]);
			const { container } = render_();

			await vi.waitFor(() => expect(synthRow(container)).toBeDefined());
			expect(synthRow(container)).toHaveClass('chat-turn-progress__row--running');
		});

		it('closes it when the task completes with the flag cleared', async () => {
			stubEventStream([toolStarted, flagged(2), executionCompleted, terminal('completed')]);
			const { container } = render_();

			await vi.waitFor(() => expect(synthRow(container)).toHaveClass('chat-turn-progress__row--done'));
		});

		it('closes it as failed when synthesis fails the task', async () => {
			stubEventStream([flagged(2), executionCompleted, terminal('failed')]);
			const { container } = render_();

			await vi.waitFor(() =>
				expect(synthRow(container)).toHaveClass('chat-turn-progress__row--failed')
			);
		});

		it('settles it with a failed or cancelled execution, which has no answer coming', async () => {
			stubEventStream([
				flagged(2),
				{
					event_type: 'execution.cancelled',
					timestamp_ms: 3,
					data: { event_id: 'e-cancel', task_id: 'task-s' }
				}
			]);
			const { container } = render_();

			await vi.waitFor(() =>
				expect(synthRow(container)).toHaveClass('chat-turn-progress__row--failed')
			);
		});

		it('does not open a row for a running event without the flag', async () => {
			stubEventStream([
				toolStarted,
				{
					event_type: 'task.status_changed',
					timestamp_ms: 2,
					data: { event_id: 'e-plain', task_id: 'task-s', status: 'running' }
				}
			]);
			const { container } = render_();

			await vi.waitFor(() => expect(rowsOf(container)).toHaveLength(1));
			expect(synthRow(container)).toBeUndefined();
		});

		it('does not reopen a settled row on a later unflagged running event', async () => {
			stubEventStream([
				flagged(2),
				terminal('completed'),
				{
					event_type: 'task.status_changed',
					timestamp_ms: 10,
					data: { event_id: 'e-late', task_id: 'task-s', status: 'running' }
				}
			]);
			const { container } = render_();

			await vi.waitFor(() => expect(synthRow(container)).toHaveClass('chat-turn-progress__row--done'));
		});

		it('keeps a repeated flagged event on the clock the first one started', async () => {
			stubEventStream([flagged(2, 'e-synth-1'), flagged(50_000, 'e-synth-2')]);
			const { container } = render_();

			await vi.waitFor(() => expect(synthRow(container)).toBeDefined());
			expect(rowsOf(container).filter((row) => row.textContent?.includes('final answer'))).toHaveLength(1);
		});

		it('replays a started flag with no terminal as still running, counted from the event time', async () => {
			vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
			vi.setSystemTime(100_000);
			try {
				stubEventStream([flagged(58_000)]);
				const { container } = render_();
				const elapsed = () => container.querySelector('.chat-turn-progress__elapsed');
				await vi.waitFor(() => expect(elapsed()).toHaveTextContent('0:42'));
				expect(synthRow(container)).toHaveClass('chat-turn-progress__row--running');

				vi.advanceTimersByTime(1_000);
				await vi.waitFor(() => expect(elapsed()).toHaveTextContent('0:43'));
			} finally {
				vi.useRealTimers();
			}
		});

		it('is the working chip of the next variant, with its own elapsed time', async () => {
			stubEventStream([toolStarted, flagged(2), executionCompleted]);
			const { container } = render(ChatTurnProgress, {
				taskId: 'task-s',
				executionId: 'exec-s',
				variant: 'next'
			});

			await vi.waitFor(() =>
				expect(container.querySelector('[data-testid="turn-stage-chip"]')).toHaveTextContent(
					'Writing the final answer…'
				)
			);
			expect(container.querySelector('[data-testid="turn-stage-chip"]')).not.toHaveTextContent(
				'Finishing…'
			);
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
