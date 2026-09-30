import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ExecutionPanelAttentionItem } from '$lib/types/executionPanel';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import { fetchTaskRunState, runAttentionFrom } from './taskAttention';
import { deriveVerdict } from './taskVerdict';

const NOW = 1_000_000;

/**
 * One attention row. The two halves are deliberately given **different**
 * instants and different words, because the reading order between them is the
 * thing under test: a reader of the wrong half gets a plausible sentence and a
 * duration that is wrong by minutes.
 */
function item(overrides: Partial<ExecutionPanelAttentionItem> = {}): ExecutionPanelAttentionItem {
	return {
		id: 'feed-1',
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'approval',
		task_id: 'task-1',
		title: 'Approval requested',
		summary: 'The lane label, not the ask',
		status: 'needs_action',
		created_at: NOW - 30 * 60_000,
		updated_at: NOW - 30 * 60_000,
		actions: [],
		metadata: {},
		hitl_request: {
			id: 'ccp-9f2c',
			source: 'diff_approval',
			input_type: 'diff_approval',
			prompt: 'Apply 3 edits to revenue.py?',
			at: NOW - 12 * 60_000
		},
		...overrides
	};
}

describe('runAttentionFrom', () => {
	it('reads the source, the ask and the instant off the canonical request', () => {
		expect(runAttentionFrom([item()])?.attention).toEqual({
			source: 'diff_approval',
			// The prompt, not the feed row's title or summary: `Approval requested`
			// is a lane label and says less than the question it labels.
			summary: 'Apply 3 edits to revenue.py?',
			// The request's own instant, not the row's — 12m, not 30m. This is the
			// first ask on this panel whose blocked duration is measured rather
			// than omitted, so reading the wrong half is a wrong number on screen.
			raisedAt: NOW - 12 * 60_000
		});
	});

	/**
	 * **The half this function used to throw away.**
	 *
	 * It read `hitl_request`, kept the source, the prompt and the instant, and
	 * discarded the id, the input type, the schema, the identifiers and the scope
	 * — everything needed to answer the ask it had just described. Nothing new is
	 * fetched to get them back; the payload was already parsed.
	 */
	it('keeps the target that answers the ask, off the same row as the verdict', () => {
		const asked = runAttentionFrom([item()]);

		expect(asked?.ask?.id).toBe('ccp-9f2c');
		expect(asked?.ask?.input_type).toBe('diff_approval');
		// The same row, which is the property that matters: a verdict and a
		// control derived separately can describe one ask and answer another,
		// and every value on screen would still be individually correct.
		expect(asked?.ask?.source).toBe(asked?.attention.source);
		expect(asked?.ask?.prompt).toBe(asked?.attention.summary);
	});

	it('describes an ask it cannot answer rather than hiding it', () => {
		// The declared type says `input_type: HitlInputType`; the wire says
		// `string`, and vitest transpiles without type-checking, so this is a
		// payload the backend could really send after a twelfth variant lands.
		const unrenderable = item({
			hitl_request: {
				...item().hitl_request!,
				input_type: 'holographic_gesture' as never
			}
		});

		const asked = runAttentionFrom([unrenderable]);
		// The verdict still says the task is blocked, because it is.
		expect(asked?.attention.source).toBe('diff_approval');
		// And no control, because none of them could express it.
		expect(asked?.ask).toBeNull();
	});

	it('produces a verdict that says how long, which no other ask on this panel can', () => {
		const attention = runAttentionFrom([item()])?.attention ?? null;
		const verdict = deriveVerdict({
			status: 'queued',
			queuedFor: 'capacity',
			attention,
			error: null,
			currentStep: null,
			totalSteps: null,
			currentStepLabel: null,
			elapsedMs: null,
			lastProgressAt: null,
			now: NOW
		});

		expect(verdict.state).toBe('waiting');
		expect(verdict.headline).toBe('Waiting on you · 12m');
	});

	/**
	 * **The filter, and the reason it is `hitl_request` and not a kind list.** The
	 * attention rows a task carries are wider than "a human must answer": a
	 * terminal execution failure is one of them. Ranking that as `waiting` paints
	 * a **failed** task `Waiting on you` and replaces its error message with a
	 * generic sentence — a louder lie than the `Queued` this whole mechanism
	 * exists to fix.
	 */
	it('skips a row that carries no request, so a failure never reads as an ask', () => {
		const failure = item({
			item_type: 'task',
			status: 'failed',
			title: 'Execution failed',
			metadata: { attention_kind: 'execution.failed' },
			hitl_request: null
		});

		expect(runAttentionFrom([failure])).toBeNull();
	});

	it('skips a source it cannot describe rather than rendering an empty sentence', () => {
		// An unmodelled source would reach `ATTENTION_COPY`, whose lookup answers
		// `undefined` — a `Waiting on you` headline over a blank detail line.
		const unknown = item({
			hitl_request: {
				id: 'x',
				source: 'telepathy' as never,
				input_type: 'text',
				prompt: 'Think at me'
			}
		});

		expect(runAttentionFrom([unknown])).toBeNull();
	});

	it('takes the first row that is an ask, not the first row', () => {
		const ask = item({ id: 'feed-2', hitl_request: { ...item().hitl_request!, source: 'approval' } });
		const failure = item({ id: 'feed-1', hitl_request: null });

		expect(runAttentionFrom([failure, ask])?.attention.source).toBe('approval');
	});

	it('falls back to the row summary only when the request carries no prompt', () => {
		const promptless = item({
			hitl_request: { ...item().hitl_request!, prompt: '   ' }
		});

		expect(runAttentionFrom([promptless])?.attention.summary).toBe('The lane label, not the ask');
	});

	it('omits the instant rather than approximating it when neither half carries one', () => {
		const undated = item({
			created_at: 0,
			hitl_request: { ...item().hitl_request!, at: undefined }
		});

		// Design §5: no duration at all beats a duration measured from whatever
		// instant happened to be nearest. `0` is a serde default, not an instant.
		expect(runAttentionFrom([undated])?.attention.raisedAt).toBeNull();
	});

	it('answers nothing for an empty list, which is the ordinary unblocked task', () => {
		expect(runAttentionFrom([])).toBeNull();
	});
});

/**
 * **The cast is the reason this is checked at all.** The function's return type
 * says `ExecutionPanelState`, and its body is a `payload as` — so without a
 * check that the body *is* one, the type is a claim nothing establishes and
 * every consumer downstream reads fields out of whatever arrived. That is not
 * hypothetical: a fixture in this repo answered this endpoint with
 * `{ run: { needs_attention: [...] } }` and passed for as long as the client
 * happened to read only that branch.
 */
describe('fetchTaskRunState', () => {
	afterEach(() => vi.unstubAllGlobals());

	const panelBody = {
		overview: { task_id: 'task-1', execution_id: 'ex_1', status: 'running' },
		run: { needs_attention: [], recent_activity: [], pending_questions: [], activity_log: [] }
	};

	it('returns the payload whole, so both the ask and the log survive one request', async () => {
		installFetchMock([{ match: '/execution-panel', handle: () => jsonResponse(panelBody) }]);

		const state = await fetchTaskRunState('task-1', 'anonymous', 'default');
		expect(state?.overview.execution_id).toBe('ex_1');
		expect(state?.run.activity_log).toEqual([]);
	});

	it('scopes the request, since a panel state is per principal and workspace', async () => {
		const { calls } = installFetchMock([
			{ match: '/execution-panel', handle: () => jsonResponse(panelBody) }
		]);

		await fetchTaskRunState('task 1/2', 'someone', 'work');
		// The id is encoded rather than interpolated: a task id with a slash would
		// otherwise address a different route entirely.
		expect(calls[0].url).toBe(
			'/api/magician/v3/tasks/task%201%2F2/execution-panel'
		);
	});

	/**
	 * The run picker's whole backend story: the task-scoped route with an execution
	 * pinned. It answers about **one run** without forgetting which task it belongs
	 * to, which is what a panel showing a task-level verdict over an
	 * execution-scoped act has to have — and it needed no backend work, because
	 * `TaskExecutionPanelQuery` has always carried the parameter.
	 */
	it('pins the chosen run with `execution_id`, on the task-scoped route', async () => {
		const { calls } = installFetchMock([
			{ match: '/execution-panel', handle: () => jsonResponse(panelBody) }
		]);

		await fetchTaskRunState('task-1', 'someone', 'work', 'ex_9/a');
		expect(calls[0].url).toBe(
			'/api/magician/v3/tasks/task-1/execution-panel?execution_id=ex_9%2Fa'
		);
		// Never the execution-scoped route: it is not populated for task-backed
		// delegate runs, and its failure is a 404 rather than a thinner payload.
		expect(calls[0].url).not.toContain('/executions/');
	});

	it('omits the parameter entirely when no run was chosen, so the default request is unchanged', async () => {
		const { calls } = installFetchMock([
			{ match: '/execution-panel', handle: () => jsonResponse(panelBody) }
		]);

		await fetchTaskRunState('task-1', 'someone', 'work');
		await fetchTaskRunState('task-1', 'someone', 'work', null);
		await fetchTaskRunState('task-1', 'someone', 'work', '   ');
		// A blank is absence, not an empty selection: `execution_id=` would make the
		// handler's `Some("")` miss every run and answer `not found`.
		for (const call of calls) expect(call.url).not.toContain('execution_id');
	});

	it('answers nothing for a 200 that is not a panel state', async () => {
		installFetchMock([
			{ match: '/execution-panel', handle: () => jsonResponse({ run: { needs_attention: [] } }) }
		]);

		expect(await fetchTaskRunState('task-1', 'anonymous', 'default')).toBeNull();
	});

	it('answers nothing when the request fails, which asserts nothing about the run', async () => {
		installFetchMock([
			{ match: '/execution-panel', handle: () => jsonResponse({ error: 'no' }, { status: 500 }) }
		]);

		expect(await fetchTaskRunState('task-1', 'anonymous', 'default')).toBeNull();
	});
});
