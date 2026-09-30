/**
 * The Run act's timeline, as a projection of one execution's payload.
 *
 * What these pin is what only this module can be wrong about: which events
 * reach the feed, which are deliberately left to the acts that already own
 * them, and the two numbers an operator reads a feed for — latency and the
 * token bill — being the ones the payload actually reported.
 *
 * **Every fixture value below is distinct from every other**, including the
 * ones that could plausibly substitute for each other: six instants, four token
 * counts, and a latency that matches none of them. A fixture where two of those
 * agree cannot tell a correct projection from one reading the wrong field, and
 * that collision is the first shape of this project's recurring defect.
 */
import { describe, expect, it } from 'vitest';

import type { ExecutionPanelState } from '$lib/types/executionPanel';

import {
	cachedPercent,
	delegationSpan,
	deriveTimeline,
	followsBottom,
	formatTokens,
	formatUsd,
	latencyScale,
	latencyShare,
	runCost,
	runCostRows,
	timelineClock,
	timelineIsolated,
	timelineCost,
	timelineMeta,
	timelineOffset,
	timelineOrigin,
	timelineWindow,
	TIMELINE_STICK_THRESHOLD_PX,
	type TimelineEntry
} from './taskTimeline';

const TASK_ID = 'task-corpus-reindex';
const EXECUTION_ID = 'exec-9f3c';

/** Six instants, ascending, none equal to another. */
const RUN_STARTED_AT = 1_770_000_000_000;
const LLM_AT = 1_770_000_030_000;
const TOOL_AT = 1_770_000_060_000;
const SHELL_AT = 1_770_000_090_000;
const OBSERVED_AT = 1_770_000_120_000;
const RUN_ENDED_AT = 1_770_000_150_000;
/** Later than every event, and nothing may sort by it. */
const UPDATED_AT = 1_770_000_999_000;

/**
 * The four token counts, all different and none a round multiple of another, so
 * a meta line reading the wrong field renders a visibly wrong number. `input`
 * *includes* `cacheRead` in this provider's accounting, which is the fact
 * `cachedPercent` is built on: 9,600 of 12,000 is 80%.
 */
const INPUT_TOKENS = 12_000;
const OUTPUT_TOKENS = 384;
const CACHE_READ_TOKENS = 9_600;
const CACHE_CREATION_TOKENS = 1_150;
/** Unlike every token count and every instant, so it cannot pass as one. */
const LATENCY_MS = 4_200;
/** A backend-priced sub-cent call, distinct from every other fixture number. */
const COST_USD = 0.00610875;

function state(overrides: {
	run?: Partial<ExecutionPanelState['run']>;
	output?: Partial<ExecutionPanelState['output']>;
	debug?: Partial<ExecutionPanelState['debug']>;
} = {}): ExecutionPanelState {
	return {
		default_tab: 'run',
		overview: {
			task_id: TASK_ID,
			execution_id: EXECUTION_ID,
			principal: 'anonymous',
			workspace: 'default',
			ui_thread_id: 'general',
			title: 'Reindex the corpus',
			description: '',
			status: 'running',
			assigned_agent_id: 'personal-assistant',
			has_plan: false,
			created_at: RUN_STARTED_AT,
			updated_at: UPDATED_AT
		},
		run: { pending_questions: [], needs_attention: [], recent_activity: [], ...overrides.run },
		output: { deliveries: [], recent_runs: [], ...overrides.output },
		debug: {
			selected_execution: null,
			timeline: [],
			observations: [],
			shell_entries: [],
			history_count: 0,
			tags: [],
			...overrides.debug
		}
	};
}

/**
 * One activity row. `item_type` is `'task'` because **`FeedItemType` has no
 * `execution` member** — a fixture asserting a shape the backend cannot produce
 * passes vitest, which transpiles without type-checking, and is caught only by
 * `svelte-check`. That exact defect has already been found once in this
 * directory.
 */
function activity(overrides: {
	id: string;
	createdAt: number;
	status?: 'running' | 'done' | 'failed' | 'needs_action' | 'info';
	title?: string;
	summary?: string | null;
	metadata?: Record<string, unknown>;
}): NonNullable<ExecutionPanelState['run']['activity_log']>[number] {
	return {
		id: overrides.id,
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'task',
		task_id: TASK_ID,
		title: overrides.title ?? 'Something happened',
		summary: overrides.summary ?? null,
		status: overrides.status ?? 'done',
		created_at: overrides.createdAt,
		updated_at: overrides.createdAt,
		actions: [],
		metadata: overrides.metadata ?? {}
	};
}

const LLM_METADATA = {
	event_type: 'llm.succeeded',
	capability: 'research',
	model: 'claude-opus-4',
	latency_ms: LATENCY_MS,
	input_tokens: INPUT_TOKENS,
	output_tokens: OUTPUT_TOKENS,
	cache_read_tokens: CACHE_READ_TOKENS,
	cache_creation_tokens: CACHE_CREATION_TOKENS,
	cost_usd: COST_USD
};

/**
 * Every case below is about the one execution the fixtures describe. The id is
 * a parameter of the projection rather than something it re-derives, so the
 * cases that are *about* the id call `deriveTimeline` directly.
 *
 * The `null` throws rather than being coalesced to `[]`: `null` and `[]` are the
 * distinction this module exists to keep, and a helper that quietly turned one
 * into the other would make every case below unable to notice it had been lost.
 */
const timelineOf = (payload: ExecutionPanelState): TimelineEntry[] => {
	const entries = deriveTimeline(payload, EXECUTION_ID);
	if (entries === null) throw new Error(`no timeline derived for ${EXECUTION_ID}`);
	return entries;
};

const byId = (entries: readonly TimelineEntry[], id: string): TimelineEntry => {
	const found = entries.find((entry) => entry.id === id);
	if (!found) throw new Error(`no timeline entry ${id} in [${entries.map((e) => e.id).join(', ')}]`);
	return found;
};

const timelineFixture = (overrides: Partial<TimelineEntry> = {}): TimelineEntry => ({
	id: 'event-0',
	at: LLM_AT,
	kind: 'event',
	status: 'info',
	title: 'Event',
	body: null,
	detail: null,
	latencyMs: null,
	model: null,
	costUsd: null,
	tokens: null,
	screenshot: false,
	executionId: null,
	agentId: null,
	...overrides
});

describe('deriveTimeline', () => {
	it('retains the bounded recipe lifecycle payload for task-level cues', () => {
		const entries = timelineOf(
			state({
				run: {
					activity_log: [
						activity({
							id: 'recipe-fallback',
							createdAt: TOOL_AT,
							metadata: {
								event_type: 'recipe.replay',
								kind: 'recipe.replay.fallback.handoff',
								recipe_id: 'recipe_1',
								step_id: 'step_3',
								class: 'schema_drift',
								replayed_steps: 2
							}
						})
					]
				}
			})
		);

		expect(entries[0]?.recipe).toEqual({
			kind: 'recipe.replay.fallback.handoff',
			recipe_id: 'recipe_1',
			step_id: 'step_3',
			class: 'schema_drift',
			replayed_steps: 2,
			duration_ms: undefined,
			origin: undefined,
			to: undefined,
			version: undefined,
			decision: undefined
		});
	});

	it('reads an LLM call whole — its model, its bill and how long it took', () => {
		const entries = timelineOf(
			state({ run: { activity_log: [activity({ id: 'log-1', createdAt: LLM_AT, metadata: LLM_METADATA })] } })
		);

		const entry = byId(entries, 'log-1');
		expect(entry.kind).toBe('llm');
		expect(entry.at).toBe(LLM_AT);
		expect(entry.model).toBe('claude-opus-4');
		expect(entry.latencyMs).toBe(LATENCY_MS);
		expect(entry.costUsd).toBe(COST_USD);
		// Each count read from its own field. Four distinct numbers, so a mapping
		// that crossed two of them cannot pass.
		expect(entry.tokens).toEqual({
			input: INPUT_TOKENS,
			output: OUTPUT_TOKENS,
			cacheRead: CACHE_READ_TOKENS,
			cacheCreation: CACHE_CREATION_TOKENS
		});
	});

	it('keeps legacy activity snapshots priced while preferring the unit-bearing field', () => {
		const metadata = { ...LLM_METADATA, cost_usd: 0.0042, cost: 99 };
		const current = timelineOf(
			state({ run: { activity_log: [activity({ id: 'current', createdAt: LLM_AT, metadata })] } })
		);
		expect(current[0]?.costUsd).toBe(0.0042);

		const legacyMetadata = Object.fromEntries(
			Object.entries(metadata).filter(([key]) => key !== 'cost_usd')
		);
		const legacy = timelineOf(
			state({ run: { activity_log: [activity({ id: 'legacy', createdAt: LLM_AT, metadata: legacyMetadata })] } })
		);
		expect(legacy[0]?.costUsd).toBe(99);
	});

	it('titles rows in the chat activity card\'s own vocabulary', () => {
		const entries = timelineOf(
			state({
				run: {
					activity_log: [
						activity({
							id: 'log-llm',
							createdAt: LLM_AT,
							title: 'LLM call succeeded',
							metadata: LLM_METADATA
						}),
						activity({
							id: 'log-tool-start',
							createdAt: TOOL_AT,
							title: 'Tool started',
							metadata: { event_type: 'tool.started', target: 'memory_search' }
						}),
						activity({
							id: 'log-tool-fail',
							createdAt: TOOL_AT + 1,
							status: 'failed',
							title: 'Tool failed',
							metadata: { event_type: 'tool.failed', tool_name: 'shell_run' }
						})
					]
				}
			})
		);

		// The same step must read identically in the card and in the panel it
		// opens, so these are the card's phrasings rather than the backend's
		// humanized titles — which the fixtures deliberately set to something else.
		expect(byId(entries, 'log-llm').title).toBe('Thinking with research');
		expect(byId(entries, 'log-tool-start').title).toBe('Calling memory_search');
		expect(byId(entries, 'log-tool-fail').title).toBe('shell_run failed');
	});

	it('falls back to the humanized title when nothing names the tool', () => {
		// An older payload carries the event type and no tool name. `Calling
		// undefined` would be worse than the generic sentence the backend wrote.
		const entries = timelineOf(
			state({
				run: {
					activity_log: [
						activity({
							id: 'log-1',
							createdAt: TOOL_AT,
							title: 'Tool call started',
							metadata: { event_type: 'tool.started' }
						})
					]
				}
			})
		);
		expect(byId(entries, 'log-1').title).toBe('Tool call started');
		expect(byId(entries, 'log-1').kind).toBe('tool');
	});

	it('reports no bill for a call that reported none, rather than a zeroed one', () => {
		const entries = timelineOf(
			state({
				run: {
					activity_log: [
						activity({ id: 'log-1', createdAt: LLM_AT, metadata: { event_type: 'llm.requested' } })
					]
				}
			})
		);
		// `null`, not `{input: null, …}`: the meta line renders nothing for the
		// first and `– → – tok` for the second, which claims the call was measured.
		expect(byId(entries, 'log-1').tokens).toBeNull();
		expect(byId(entries, 'log-1').latencyMs).toBeNull();
		expect(byId(entries, 'log-1').model).toBeNull();
	});

	it('carries shell stdout as the row\'s own block, keyed so two commands cannot collide', () => {
		const entries = timelineOf(
			state({
				debug: {
					shell_entries: [
						{
							step_id: 'step-3',
							step_index: 3,
							command: 'cargo build --release',
							lines: [
								{ text: 'Compiling magician', stream: 'stdout', timestamp: SHELL_AT },
								{ text: 'Finished in 42s', stream: 'stdout', timestamp: SHELL_AT + 1 }
							],
							exit_code: 0,
							is_complete: true,
							started_at: SHELL_AT,
							execution_id: EXECUTION_ID
						},
						{
							// The **same step index**, which is the collision a positional id
							// would produce: one of the two rows would silently vanish from a
							// keyed `{#each}`.
							step_id: 'step-3',
							step_index: 3,
							command: 'cargo test',
							lines: [],
							exit_code: 101,
							is_complete: true,
							started_at: SHELL_AT + 2,
							execution_id: EXECUTION_ID
						}
					]
				}
			})
		);

		const shell = entries.filter((entry) => entry.kind === 'shell');
		expect(shell).toHaveLength(2);
		expect(new Set(shell.map((entry) => entry.id)).size).toBe(2);
		expect(shell[0].title).toBe('cargo build --release');
		expect(shell[0].detail).toBe('Compiling magician\nFinished in 42s');
		expect(shell[0].status).toBe('done');
		// A non-zero exit is a failure; no output at all is not, so `detail` is
		// absent rather than an empty `<pre>`.
		expect(shell[1].status).toBe('failed');
		expect(shell[1].detail).toBeNull();
	});

	it('calls an unfinished shell command running, and an untimed complete one done', () => {
		const entries = timelineOf(
			state({
				debug: {
					shell_entries: [
						{
							step_id: 'a',
							step_index: 1,
							command: 'tail -f log',
							lines: [],
							exit_code: null,
							is_complete: false,
							started_at: SHELL_AT,
							execution_id: EXECUTION_ID
						},
						{
							step_id: 'b',
							step_index: 2,
							command: 'echo done',
							lines: [],
							// The backend omits the code for commands it did not wait on.
							// Calling that failed would be a verdict invented from a gap.
							exit_code: null,
							is_complete: true,
							started_at: SHELL_AT + 1,
							execution_id: EXECUTION_ID
						}
					]
				}
			})
		);
		expect(entries.map((entry) => entry.status)).toEqual(['running', 'done']);
	});

	it('marks an observation neutral and says whether a screenshot was captured', () => {
		const entries = timelineOf(
			state({
				debug: {
					observations: [
						{
							observation_id: 'obs-1',
							captured_at: OBSERVED_AT,
							url: 'https://example.test/reports',
							has_screenshot: true
						},
						{
							observation_id: 'obs-2',
							captured_at: OBSERVED_AT + 1,
							page_stage: 'after-submit',
							has_screenshot: false
						}
					]
				}
			})
		);
		// A capture is not an outcome — it neither succeeded nor failed, so it
		// takes the neutral mark rather than a tick it did not earn.
		expect(entries.map((entry) => entry.status)).toEqual(['info', 'info']);
		expect(entries[0].title).toBe('https://example.test/reports');
		expect(entries[0].screenshot).toBe(true);
		// No url: the stage is the only name there is, and it beats a placeholder.
		expect(entries[1].title).toBe('after-submit');
		expect(entries[1].screenshot).toBe(false);
	});

	/**
	 * `recent_runs` is the **task's** run history, not this run's, so it is the
	 * one collection here that has to be filtered. A task retried three times
	 * would otherwise render three identical `Run started` rows, and the only
	 * thing that could tell them apart is an execution id — an L3 value in an L2
	 * row, which is the exact defect the design blames for the old panel.
	 */
	it('records both ends of the run it is about, and no attempt but that one', () => {
		const payload = state({
			output: {
				recent_runs: [
					{
						execution_id: EXECUTION_ID,
						started_at: RUN_STARTED_AT,
						ended_at: RUN_ENDED_AT,
						status: 'completed',
						completion_summary: 'Reindexed 4,102 documents.',
						completion_artifact_names: []
					},
					{
						// An earlier attempt at the same task. Its instants sit inside this
						// run's, so a projection that kept it would interleave rather than
						// bracket — visible in the ids below rather than only in a count.
						execution_id: 'exec-earlier',
						started_at: LLM_AT,
						ended_at: TOOL_AT,
						status: 'failed',
						error_message: 'Provider unavailable',
						completion_artifact_names: []
					}
				]
			}
		});

		const entries = timelineOf(payload);
		expect(entries.map((entry) => entry.id)).toEqual([
			`run:${EXECUTION_ID}:start`,
			`run:${EXECUTION_ID}:end`
		]);
		const ended = byId(entries, `run:${EXECUTION_ID}:end`);
		expect(ended.at).toBe(RUN_ENDED_AT);
		expect(ended.status).toBe('done');
		expect(ended.body).toBe('Reindexed 4,102 documents.');
		// The id stays out of the row: the act's provenance already carries it.
		expect(entries.every((entry) => entry.detail === null)).toBe(true);

		// Read as the other run, the other run's rows are the ones that render —
		// so this is a filter on the id it was given rather than on the first row.
		// The payload's `overview.execution_id` still names this run, so this also
		// shows the agreement check below is not what selects the rows.
		expect(
			deriveTimeline({ ...payload, overview: { ...payload.overview, execution_id: 'exec-earlier' } }, 'exec-earlier')?.map(
				(entry) => entry.id
			)
		).toEqual(['run:exec-earlier:start', 'run:exec-earlier:end']);
	});

	/**
	 * **The three absences, and none of them is `[]`.**
	 *
	 * `[]` is a run whose events were read and numbered zero, and the act says so
	 * out loud. Each of these is a run nothing observed, and the act must stay
	 * silent — which on the task surfaces is not hypothetical: the run's id comes
	 * off a store row and its events off a separately fetched payload, so the two
	 * genuinely can name different executions.
	 */
	describe('what it refuses to project', () => {
		it('answers nothing when there is no payload to read', () => {
			expect(deriveTimeline(null, EXECUTION_ID)).toBeNull();
		});

		it('answers nothing when no run has been named, since no act would hold it', () => {
			expect(deriveTimeline(state(), null)).toBeNull();
		});

		/**
		 * The one that matters, and the one a per-collection filter would miss: the
		 * `activity_log`, the shell entries and the observations are all *this*
		 * payload's, so nothing in them carries an execution id to filter on. Read
		 * under another run's name they would render whole and plausible under that
		 * run's heading, with every individual value correct.
		 */
		it('answers nothing when the payload turns out to describe another run', () => {
			const payload = state({
				run: {
					activity_log: [activity({ id: 'log-1', createdAt: LLM_AT, metadata: LLM_METADATA })]
				}
			});

			expect(timelineOf(payload)).toHaveLength(1);
			expect(deriveTimeline(payload, 'exec-some-other-attempt')).toBeNull();
		});

		/**
		 * `selected_execution` is the fallback `executionIdOf` reads when the
		 * overview carries none, so a payload identified only that way still
		 * projects. Pinned because the alternative — treating a missing
		 * `overview.execution_id` as a disagreement — would silence the timeline on
		 * every payload that names its run the other way.
		 */
		it('accepts a payload that names its run only on the selected execution', () => {
			const payload = state({
				run: {
					activity_log: [activity({ id: 'log-1', createdAt: LLM_AT, metadata: LLM_METADATA })]
				},
				debug: {
					selected_execution: {
						execution_id: EXECUTION_ID,
						status: 'running',
						artifact_names: [],
						linked_inputs: [],
						step_statuses: []
					}
				}
			});
			payload.overview.execution_id = null;

			expect(deriveTimeline(payload, EXECUTION_ID)).toHaveLength(1);
		});
	});

	it('keeps only the start of a run that has not ended', () => {
		const entries = timelineOf(
			state({
				output: {
					recent_runs: [
						{
							execution_id: EXECUTION_ID,
							started_at: RUN_STARTED_AT,
							ended_at: null,
							status: 'running',
							completion_artifact_names: []
						}
					]
				}
			})
		);
		expect(entries.map((entry) => entry.id)).toEqual([`run:${EXECUTION_ID}:start`]);
	});

	it('sorts every source into one chronology', () => {
		const entries = timelineOf(
			state({
				// Supplied out of order and from four different collections, because a
				// projection that concatenated without sorting would still pass a
				// fixture whose sources happened to be in order already.
				run: {
					activity_log: [
						activity({ id: 'log-tool', createdAt: TOOL_AT }),
						activity({ id: 'log-llm', createdAt: LLM_AT })
					]
				},
				output: {
					recent_runs: [
						{
							execution_id: EXECUTION_ID,
							started_at: RUN_STARTED_AT,
							ended_at: RUN_ENDED_AT,
							status: 'completed',
							completion_artifact_names: []
						}
					]
				},
				debug: {
					observations: [
						{ observation_id: 'obs-1', captured_at: OBSERVED_AT, has_screenshot: false }
					],
					shell_entries: [
						{
							step_id: 'a',
							step_index: 1,
							command: 'ls',
							lines: [],
							exit_code: 0,
							is_complete: true,
							started_at: SHELL_AT,
							execution_id: EXECUTION_ID
						}
					]
				}
			})
		);

		expect(entries.map((entry) => entry.at)).toEqual([
			RUN_STARTED_AT,
			LLM_AT,
			TOOL_AT,
			SHELL_AT,
			OBSERVED_AT,
			RUN_ENDED_AT
		]);
		// `updated_at` is later than every one of them and must never be a sort key.
		expect(entries.map((entry) => entry.at)).not.toContain(UPDATED_AT);
	});

	it('reads the capped activity view only when the full log is absent', () => {
		const both = timelineOf(
			state({
				run: {
					activity_log: [activity({ id: 'log-1', createdAt: LLM_AT })],
					recent_activity: [activity({ id: 'recent-1', createdAt: TOOL_AT })]
				}
			})
		);
		// The two describe the same run from two projections; rendering both would
		// put one event on screen twice.
		expect(both.map((entry) => entry.id)).toEqual(['log-1']);

		const cappedOnly = timelineOf(
			state({ run: { recent_activity: [activity({ id: 'recent-1', createdAt: TOOL_AT })] } })
		);
		expect(cappedOnly.map((entry) => entry.id)).toEqual(['recent-1']);
	});

	it('reads the debug timeline only when the full log is absent, for the same reason', () => {
		const debugTimeline = [
			{
				id: 'tl-1',
				timestamp: TOOL_AT,
				severity: 'error' as const,
				title: 'Retrying after a provider error',
				message: 'HTTP 529'
			}
		];

		const withLog = timelineOf(
			state({
				run: { activity_log: [activity({ id: 'log-1', createdAt: LLM_AT })] },
				debug: { timeline: debugTimeline }
			})
		);
		expect(withLog.map((entry) => entry.id)).toEqual(['log-1']);

		const withoutLog = timelineOf(state({ debug: { timeline: debugTimeline } }));
		expect(withoutLog[0].kind).toBe('reasoning');
		expect(withoutLog[0].status).toBe('failed');
		expect(withoutLog[0].body).toBe('HTTP 529');
	});

	/**
	 * The load-bearing exclusion, and the whole reason this is a slice of the Run
	 * act rather than a fourth act. Three of the retired feed's eight kinds are
	 * things the acts already render; carrying them here would put the same fact
	 * on screen twice at two resolutions, and the reader would have no way to
	 * know it was one thing.
	 */
	it('leaves the acts what the acts already own', () => {
		const entries = timelineOf(
			state({
				run: {
					pending_questions: [
						{
							id: 'question-1',
							question_text: 'Which corpus revision?',
							status: 'open',
							context_snippets: ['revision 4 was the last green one'],
							related_slots: [],
							options: []
						}
					]
				},
				output: {
					result: { summary: 'Reindexed 4,102 documents.', artifact_names: ['corpus/index'] }
				},
				debug: {
					selected_execution: {
						execution_id: EXECUTION_ID,
						status: 'running',
						artifact_names: [],
						linked_inputs: [],
						step_statuses: [
							{ number: 1, name: 'Fetch the corpus', status: 'completed', progress: '' }
						]
					}
				}
			})
		);
		// The Plan act renders the question, the Run act's step list renders the
		// step, the Output act renders the report and the files.
		expect(entries).toEqual([]);
	});

	it('survives a payload whose empty collections arrived as null', () => {
		// The backend serialises absent collections as `null` even where the type
		// says array. One of them throwing takes the whole panel with it.
		const nulled = {
			...state(),
			run: { ...state().run, activity_log: null, recent_activity: null },
			output: { ...state().output, recent_runs: null },
			debug: { ...state().debug, timeline: null, observations: null, shell_entries: null }
		} as unknown as ExecutionPanelState;
		expect(timelineOf(nulled)).toEqual([]);
	});
});

describe('timelineWindow', () => {
	it('keeps the newest bounded rows and reports the omitted count', () => {
		const entries = Array.from({ length: 205 }, (_, index) =>
			timelineFixture({ id: `event-${index}`, at: LLM_AT + index })
		);
		const window = timelineWindow(entries, 200);

		expect(window.hidden).toBe(5);
		expect(window.entries).toHaveLength(200);
		expect(window.entries[0]?.id).toBe('event-5');
		expect(window.entries.at(-1)?.id).toBe('event-204');
	});

	it('does not copy or claim omissions for an already bounded feed', () => {
		const entries = [timelineFixture({ id: 'event-1' }), timelineFixture({ id: 'event-2' })];
		const window = timelineWindow(entries, 200);

		expect(window).toEqual({ entries, hidden: 0 });
		expect(window.entries).toBe(entries);
	});
});

describe('timelineMeta', () => {
	const entry = (overrides: Partial<TimelineEntry> = {}): TimelineEntry => ({
		id: 'entry-1',
		at: LLM_AT,
		kind: 'llm',
		status: 'done',
		title: 'Thinking with research',
		body: null,
		detail: null,
		latencyMs: LATENCY_MS,
		model: 'claude-opus-4',
		costUsd: COST_USD,
		tokens: {
			input: INPUT_TOKENS,
			output: OUTPUT_TOKENS,
			cacheRead: CACHE_READ_TOKENS,
			cacheCreation: CACHE_CREATION_TOKENS
		},
		screenshot: false,
		executionId: null,
		agentId: null,
		...overrides
	});

	it('reads the model, the bill and the cache rate as one line', () => {
		// 9,600 of 12,000 input tokens came from cache: 80%. The three segments are
		// visibly different kinds of thing, so a line that reordered or repeated
		// one cannot pass.
		expect(timelineMeta(entry())).toBe('claude-opus-4 · 12k → 384 tok · 80% cached');
	});

	it('renders nothing at all for an event with no numbers to report', () => {
		// A row with nothing to add renders no element rather than a separator with
		// empty sides — the rule `runSummary` and `fileMeta` both follow.
		expect(timelineMeta(entry({ model: null, tokens: null }))).toBe('');
	});

	it('drops the arrow rather than half of it', () => {
		// `– → 384 tok` reads as a rendering fault, and the arrow means nothing
		// with one side missing.
		const half = timelineMeta(
			entry({ model: null, tokens: { input: null, output: OUTPUT_TOKENS, cacheRead: null, cacheCreation: null } })
		);
		expect(half).toBe('');
	});

	it('omits the cache rate when nothing was read from cache', () => {
		expect(
			timelineMeta(
				entry({ tokens: { input: INPUT_TOKENS, output: OUTPUT_TOKENS, cacheRead: null, cacheCreation: null } })
			)
		).toBe('claude-opus-4 · 12k → 384 tok');
	});

	/**
	 * **The cost half without the model, because the row renders them differently.**
	 *
	 * A model name is an identifier and goes in `<code>`; the token counts are
	 * figures a reader compares down a column and stay tabular text. Joined into one
	 * string they had to share one treatment, and the shared treatment made a
	 * provider's model id read as prose.
	 *
	 * `timelineMeta` is written in terms of this, which is what keeps the plain-text
	 * form from acquiring a second implementation — the cases above measure that.
	 */
	describe('timelineCost', () => {
		it('reports the bill and the cache rate, and never the model', () => {
			expect(timelineCost(entry())).toEqual(['12k → 384 tok', '80% cached']);
		});

		it('reports nothing for an event that carried no usage at all', () => {
			// `null` tokens is "no usage was reported", which is not a zeroed bill.
			expect(timelineCost(entry({ tokens: null }))).toEqual([]);
		});

		it('is unaffected by the model, which is the point of the split', () => {
			expect(timelineCost(entry({ model: null }))).toEqual(timelineCost(entry()));
		});

		it('drops the arrow rather than half of it, exactly as the joined line does', () => {
			expect(
				timelineCost(
					entry({ tokens: { input: null, output: OUTPUT_TOKENS, cacheRead: null, cacheCreation: null } })
				)
			).toEqual([]);
		});

		/**
		 * The joined form is the segments, in order, with the model in front — asserted
		 * rather than assumed, so `timelineMeta` cannot drift into a second answer.
		 */
		it('composes the joined line exactly', () => {
			expect(timelineMeta(entry())).toBe(['claude-opus-4', ...timelineCost(entry())].join(' · '));
		});
	});
});

describe('cachedPercent', () => {
	it('divides by the input alone, because the input already includes the cached part', () => {
		// The trap: `cached / (input + cached)` gives 44% here, which is the wrong
		// side of the number an operator is watching for.
		expect(
			cachedPercent({
				input: INPUT_TOKENS,
				output: OUTPUT_TOKENS,
				cacheRead: CACHE_READ_TOKENS,
				cacheCreation: CACHE_CREATION_TOKENS
			})
		).toBe(80);
	});

	it('has no rate to report without both halves', () => {
		expect(cachedPercent(null)).toBeNull();
		expect(
			cachedPercent({ input: INPUT_TOKENS, output: null, cacheRead: null, cacheCreation: null })
		).toBeNull();
		expect(
			cachedPercent({ input: null, output: null, cacheRead: CACHE_READ_TOKENS, cacheCreation: null })
		).toBeNull();
	});
});

describe('formatTokens', () => {
	it('scales to the largest unit that leaves a number worth reading', () => {
		expect(formatTokens(384)).toBe('384');
		expect(formatTokens(1_500)).toBe('1.5k');
		expect(formatTokens(12_000)).toBe('12k');
		expect(formatTokens(2_400_000)).toBe('2.4M');
	});

	it('has nothing to render for a count nothing reported', () => {
		expect(formatTokens(null)).toBeNull();
		// Zero is a real count and renders; a negative one is not a measurement.
		expect(formatTokens(0)).toBe('0');
		expect(formatTokens(-1)).toBeNull();
	});
});

/**
 * Where the time went.
 *
 * A duration per row is a number the reader compares by hand; a bar drawn to
 * scale answers "what took so long?" without their doing the arithmetic. What
 * these pin is the two ways such a bar can lie: drawing one where nothing was
 * measured, and drawing one where there is nothing to compare against.
 *
 * **The latencies below are deliberately not multiples of each other except
 * where a case is about the ratio**, so a share computed off the wrong
 * denominator — the total instead of the maximum, say — cannot coincide with the
 * right answer.
 */
describe('latencyScale and latencyShare', () => {
	const timed = (...latencies: (number | null)[]): TimelineEntry[] =>
		latencies.map((latencyMs, index) => ({
			id: `entry-${index}`,
			at: LLM_AT + index,
			kind: 'llm',
			status: 'done',
			title: `Call ${index}`,
			body: null,
			detail: null,
			latencyMs,
			model: null,
			costUsd: null,
			tokens: null,
			screenshot: false,
			executionId: null,
			agentId: null,
		}));

	it('has no scale until two rows were timed, because one bar compares to nothing', () => {
		// A comparison needs two things. A single timed row would draw a full-width
		// bar meaning "the longest of the one thing measured", which the eye reads
		// as "this took a long time" — a claim the list does not contain.
		expect(latencyScale([])).toBeNull();
		expect(latencyScale(timed(null, null, null))).toBeNull();
		expect(latencyScale(timed(4_300, null, null))).toBeNull();
		expect(latencyScale(timed(4_300, 900))).toBe(4_300);
	});

	it('does not count a zero or a negative toward the two rows it needs', () => {
		// `0` is not a measurement a reader can act on, and letting one count would
		// switch the whole strip on for a feed holding a single real number.
		expect(latencyScale(timed(4_300, 0))).toBeNull();
		expect(latencyScale(timed(4_300, -1))).toBeNull();
		expect(latencyScale(timed(4_300, 0, 900))).toBe(4_300);
	});

	it('scales against the longest and not the total, so one slow call stands out', () => {
		// 12s among four calls totalling 13.2s. Against the max the slow one fills
		// the track and the rest are slivers — the answer, seen rather than read.
		// Against the total it would be 91%, 5%, 3%, 2% — different numbers, so a
		// denominator swap cannot pass this.
		const scale = latencyScale(timed(12_000, 700, 400, 100));
		expect(scale).toBe(12_000);
		expect(latencyShare(12_000, scale)).toBe(100);
		expect(latencyShare(700, scale)).toBe(6);
		expect(latencyShare(400, scale)).toBe(3);
	});

	it('draws a sliver rather than nothing for a real but tiny measurement', () => {
		// 1ms against 12s rounds to 0%, and a zero-width bar is indistinguishable
		// from the no-bar an *unmeasured* row gets. Those are different facts, so
		// the floor exists — and it is a floor, not a default: an unmeasured row
		// still gets `null`.
		expect(latencyShare(1, 12_000)).toBe(2);
		expect(latencyShare(null, 12_000)).toBeNull();
	});

	it('draws equal bars for equal calls, which is the truthful picture of a flat run', () => {
		// Nothing stood out, so nothing is drawn as standing out. Full-width rather
		// than uniformly tiny: the bars are a *relative* claim, and every call here
		// really was as long as the longest.
		const scale = latencyScale(timed(900, 900, 900));
		expect(scale).toBe(900);
		expect([900, 900, 900].map((ms) => latencyShare(ms, scale))).toEqual([100, 100, 100]);
	});

	it('draws no bar without a scale, however long the row itself was', () => {
		// The row's own number is still shown; only the comparison is withheld.
		expect(latencyShare(12_000, null)).toBeNull();
		expect(latencyShare(12_000, 0)).toBeNull();
	});
});

/**
 * When each row happened, in the two forms the owner asked for.
 *
 * `at` was fetched, sorted by, and never rendered. What these pin is the shape
 * of the two ways rendering it can lie: a wall clock built from the `0` that
 * `deriveTimeline` coalesces a missing timestamp to — which is 1970 wearing a
 * plausible `HH:MM:SS` — and an offset measured from a start nobody recorded.
 */
describe('timelineClock', () => {
	/**
	 * Built from local components rather than an epoch literal, so the assertion
	 * is timezone-independent: the function renders the reader's own clock, and a
	 * fixed epoch would only pass in whichever zone the fixture was written in.
	 */
	const localInstant = (h: number, m: number, s: number): number =>
		new Date(2026, 6, 30, h, m, s).getTime();

	it('renders the wall clock to the second, zero-padded on every field', () => {
		expect(timelineClock(localInstant(14, 5, 9))).toBe('14:05:09');
		expect(timelineClock(localInstant(0, 0, 0))).toBe('00:00:00');
		expect(timelineClock(localInstant(23, 59, 59))).toBe('23:59:59');
	});

	it('renders nothing for an instant nothing recorded', () => {
		// `0` is what `deriveTimeline` writes when the wire carried no timestamp,
		// so it is the routine case rather than the theoretical one — and the one
		// that would otherwise render a 1970 clock indistinguishable from a real
		// reading.
		expect(timelineClock(0)).toBeNull();
		expect(timelineClock(null)).toBeNull();
		expect(timelineClock(-1)).toBeNull();
		expect(timelineClock(Number.NaN)).toBeNull();
	});
});

describe('delegationSpan', () => {
	const localInstant = (h: number, m: number, s: number): number =>
		new Date(2026, 6, 30, h, m, s).getTime();

	const entryAt = (timeMs: number): TimelineEntry => ({
		id: `entry-${timeMs}`,
		at: timeMs,
		kind: 'llm',
		status: 'done',
		title: 'Thinking',
		body: null,
		detail: null,
		latencyMs: null,
		model: null,
		costUsd: null,
		tokens: null,
		screenshot: false,
		executionId: 'exec-child-1',
		agentId: 'research-agent'
	});

	it('computes startClock, endClock, duration, and summary from entries', () => {
		const t1 = localInstant(10, 14, 2);
		const t2 = localInstant(10, 14, 45);
		const entries = [entryAt(t1), entryAt(t1 + 10_000), entryAt(t2)];
		const span = delegationSpan(entries, {
			execution_id: 'exec-child-1',
			agent_id: 'research-agent',
			status: 'completed',
			entry_count: 3,
			started_at: new Date(t1).toISOString(),
			completed_at: new Date(t2).toISOString()
		});

		expect(span.startClock).toBe('10:14:02');
		expect(span.endClock).toBe('10:14:45');
		expect(span.duration).toBe('43s');
		expect(span.summary).toBe('10:14:02 – 10:14:45 (43s)');
	});

	it('formats running status with start time when ongoing', () => {
		const t1 = localInstant(10, 14, 2);
		const entries = [entryAt(t1)];
		const span = delegationSpan(entries, {
			execution_id: 'exec-child-1',
			agent_id: 'research-agent',
			status: 'running',
			entry_count: 1,
			started_at: new Date(t1).toISOString(),
			completed_at: null
		});

		expect(span.startClock).toBe('10:14:02');
		expect(span.endClock).toBeNull();
		expect(span.summary).toBe('started 10:14:02');
	});

	it('handles single entry with equal start and end', () => {
		const t1 = localInstant(10, 14, 2);
		const entries = [entryAt(t1)];
		const span = delegationSpan(entries, {
			execution_id: 'exec-child-1',
			agent_id: 'research-agent',
			status: 'completed',
			entry_count: 1,
			started_at: new Date(t1).toISOString(),
			completed_at: new Date(t1).toISOString()
		});

		expect(span.startClock).toBe('10:14:02');
		expect(span.endClock).toBe('10:14:02');
		expect(span.summary).toBe('10:14:02');
	});

	it('returns nulls when neither entries nor group provide recorded times', () => {
		const span = delegationSpan([], null);
		expect(span.startClock).toBeNull();
		expect(span.endClock).toBeNull();
		expect(span.duration).toBeNull();
		expect(span.summary).toBeNull();
	});
});

describe('timelineOrigin', () => {
	const at = (...instants: number[]): TimelineEntry[] =>
		instants.map((instant, index) => ({
			id: `entry-${index}`,
			at: instant,
			kind: 'llm',
			status: 'done',
			title: `Call ${index}`,
			body: null,
			detail: null,
			latencyMs: null,
			model: null,
			costUsd: null,
			tokens: null,
			screenshot: false,
			executionId: null,
			agentId: null,
		}));

	it('is the earliest recorded instant, whatever order the entries arrive in', () => {
		expect(timelineOrigin(at(TOOL_AT, RUN_STARTED_AT, SHELL_AT))).toBe(RUN_STARTED_AT);
		expect(timelineOrigin(at(RUN_STARTED_AT))).toBe(RUN_STARTED_AT);
	});

	it('is the run-start row when the payload carried one, because a run precedes its events', () => {
		// Not asserted through a constant but through the projection itself: the
		// lifecycle row `deriveTimeline` emits at `started_at` is the earliest thing
		// in the feed, so the origin every offset is measured from is the run's own
		// start rather than the first call it happened to make.
		const entries = timelineOf(
			state({
				run: { activity_log: [activity({ id: 'log-1', createdAt: LLM_AT, metadata: LLM_METADATA })] },
				output: {
					recent_runs: [
						{
							execution_id: EXECUTION_ID,
							started_at: RUN_STARTED_AT,
							ended_at: RUN_ENDED_AT,
							status: 'completed',
							completion_summary: 'Reindexed 4,102 documents.',
							completion_artifact_names: []
						}
					]
				}
			})
		);

		expect(entries[0].id).toBe(`run:${EXECUTION_ID}:start`);
		expect(timelineOrigin(entries)).toBe(RUN_STARTED_AT);
		// And the first *call* is 30s in rather than at zero, which is the fact the
		// offset column exists to show.
		expect(timelineOffset(byId(entries, 'log-1').at, timelineOrigin(entries))).toBe('+30s');
	});

	it('skips rows nothing timed rather than letting one drag the origin to the epoch', () => {
		// A single untimed row would otherwise make every offset in the feed a
		// fifty-six-year number, each one individually consistent with the last.
		expect(timelineOrigin(at(0, TOOL_AT, SHELL_AT))).toBe(TOOL_AT);
	});

	it('has no origin when nothing in the feed carried an instant', () => {
		expect(timelineOrigin([])).toBeNull();
		expect(timelineOrigin(at(0, 0))).toBeNull();
	});
});

describe('timelineOffset', () => {
	it('measures from the origin and says so with a sign', () => {
		// 30s, 60s and 90s past the run's start, in `durationIfKnown`'s units so a
		// row's offset, a step's duration and the verdict's age all read alike.
		expect(timelineOffset(LLM_AT, RUN_STARTED_AT)).toBe('+30s');
		expect(timelineOffset(TOOL_AT, RUN_STARTED_AT)).toBe('+1m');
		expect(timelineOffset(SHELL_AT, RUN_STARTED_AT)).toBe('+1m 30s');
	});

	it('renders the origin row as a measurement rather than as a placeholder', () => {
		// `+0s` on the row the run began at is a fact. It is only a fabrication when
		// the start it is measured from was invented, which is the case below.
		expect(timelineOffset(RUN_STARTED_AT, RUN_STARTED_AT)).toBe('+0s');
	});

	it('omits the offset when the start is unknown, rather than showing +0s against a guess', () => {
		expect(timelineOffset(LLM_AT, null)).toBeNull();
		expect(timelineOffset(LLM_AT, 0)).toBeNull();
	});

	it('omits the offset for a row nothing timed, even against a known start', () => {
		expect(timelineOffset(0, RUN_STARTED_AT)).toBeNull();
		expect(timelineOffset(null, RUN_STARTED_AT)).toBeNull();
	});

	it('renders nothing rather than a negative offset wearing a plus sign', () => {
		// Unreachable while the origin is the feed's own minimum, and pinned anyway:
		// the two are separate functions, and a caller passing a start of its own is
		// the first thing that would break the invariant.
		expect(timelineOffset(RUN_STARTED_AT, LLM_AT)).toBeNull();
	});
});

/**
 * Which rows get a container of their own.
 *
 * The predicate is the testable half of the fix: jsdom computes no layout, so
 * the border this drives cannot be observed in a render, while the decision can.
 */
describe('timelineIsolated', () => {
	const row = (overrides: Partial<TimelineEntry> = {}): TimelineEntry => ({
		id: 'entry',
		at: LLM_AT,
		kind: 'llm',
		status: 'done',
		title: 'Thinking with memory',
		body: null,
		detail: null,
		latencyMs: LATENCY_MS,
		model: 'claude-opus-4',
		costUsd: COST_USD,
		tokens: { input: INPUT_TOKENS, output: OUTPUT_TOKENS, cacheRead: null, cacheCreation: null },
		screenshot: false,
		executionId: null,
		agentId: null,
		...overrides
	});

	it('isolates a row that carries prose or a stdout block', () => {
		expect(timelineIsolated(row({ body: 'It searched three stores.' }))).toBe(true);
		expect(timelineIsolated(row({ detail: 'total 24\ndrwxr-xr-x' }))).toBe(true);
		expect(timelineIsolated(row({ body: 'Ran it.', detail: 'ok' }))).toBe(true);
	});

	it('leaves a single-line row flat, however much else it is carrying', () => {
		// The row above has a model, a token bill, a cache rate and a latency, and
		// every one of them renders. None of them makes it tall, so none of them
		// earns a frame — a feed in which every row is boxed is the card grid this
		// panel replaced, one level down.
		expect(timelineIsolated(row())).toBe(false);
		expect(timelineIsolated(row({ screenshot: true }))).toBe(false);
	});

	it('treats a blank body or detail as no body or detail', () => {
		// `deriveTimeline` cannot produce these — `text()` returns null for a blank
		// string — but a caller building an entry by hand can, and an empty frame
		// says a row has more to it than it does.
		expect(timelineIsolated(row({ body: '' }))).toBe(false);
		expect(timelineIsolated(row({ detail: '' }))).toBe(false);
	});
});

describe('followsBottom', () => {
	it('follows while the reader is at the end, and stops once they scroll away', () => {
		const clientHeight = 400;
		const scrollHeight = 2_000;
		// Exactly at the bottom, and one pixel inside the threshold.
		expect(followsBottom({ scrollTop: 1_600, scrollHeight, clientHeight })).toBe(true);
		expect(
			followsBottom({
				scrollTop: 1_600 - (TIMELINE_STICK_THRESHOLD_PX - 1),
				scrollHeight,
				clientHeight
			})
		).toBe(true);
		// A whole row above it is a reader who has chosen to look at something.
		expect(
			followsBottom({ scrollTop: 1_600 - TIMELINE_STICK_THRESHOLD_PX, scrollHeight, clientHeight })
		).toBe(false);
		expect(followsBottom({ scrollTop: 0, scrollHeight, clientHeight })).toBe(false);
	});

	it('follows when the metrics say nothing, which is what an unlaid-out box reports', () => {
		// jsdom reports zeroes and a freshly opened act has not painted yet. The
		// default has to be "keep following" — a feed that stopped following
		// because it could not measure itself would never start.
		expect(followsBottom({ scrollTop: 0, scrollHeight: 0, clientHeight: 0 })).toBe(true);
		expect(followsBottom({ scrollTop: 0, scrollHeight: Number.NaN, clientHeight: 0 })).toBe(true);
	});
});

/**
 * What the whole run cost — the question the feed stated seventy times and
 * summed nowhere.
 *
 * **The three ways a total can lie are what these pin**, and only the first is
 * arithmetic:
 *
 * - it can be wrong (a sum over the wrong subset, a rate over the wrong
 *   denominator);
 * - it can be **invented** — `0 tok`, `0% cached`, `0s` about a run nothing
 *   measured, which reads as a measurement and is the failure this panel exists
 *   to remove;
 * - it can describe a different set of events than the rows beneath it, which
 *   nothing on screen could contradict.
 *
 * The figures are the module fixture's, so a row's own cost line and the total
 * above it are checkable against each other by hand: three calls of
 * 12,000 → 384 with 9,600 cached is 36k → 1.2k at 80%.
 */
describe('runCost and runCostRows', () => {
	const row = (overrides: Partial<TimelineEntry> = {}): TimelineEntry => ({
		id: 'entry-1',
		at: LLM_AT,
		kind: 'llm',
		status: 'done',
		title: 'Thinking with research',
		body: null,
		detail: null,
		latencyMs: LATENCY_MS,
		model: 'claude-opus-4',
		costUsd: COST_USD,
		tokens: {
			input: INPUT_TOKENS,
			output: OUTPUT_TOKENS,
			cacheRead: CACHE_READ_TOKENS,
			cacheCreation: CACHE_CREATION_TOKENS
		},
		screenshot: false,
		executionId: null,
		agentId: null,
		...overrides
	});

	/** Three billed calls, thirty seconds apart, all answered by one model. */
	const threeCalls = (): TimelineEntry[] => [
		row({ id: 'call-1', at: LLM_AT }),
		row({ id: 'call-2', at: TOOL_AT }),
		row({ id: 'call-3', at: SHELL_AT })
	];

	it('has nothing to sum for a run nothing observed, and nothing for a run with no events', () => {
		// `null` is "no feed" and `[]` is "a feed with no rows". Neither is a run
		// that cost nothing, so neither produces a row.
		expect(runCost(null)).toBeNull();
		expect(runCost([])).toBeNull();
		expect(runCostRows(null)).toEqual([]);
		expect(runCostRows([])).toEqual([]);
	});

	it('renders no cost row at all for a run whose events carried none', () => {
		// The case that matters most: five real events, no usage, no latency, no
		// model. `0 tok · 0% cached · 0s` would be four fabrications in one
		// disclosure, and a reader would take every one of them for a measurement.
		const unbilled = [
			row({ id: 'a', at: LLM_AT, kind: 'tool', tokens: null, latencyMs: null, model: null, costUsd: null }),
			row({ id: 'b', at: TOOL_AT, kind: 'shell', tokens: null, latencyMs: null, model: null, costUsd: null }),
			row({
				id: 'c',
				at: SHELL_AT,
				kind: 'observation',
				status: 'info',
				tokens: null,
				latencyMs: null,
				model: null,
				costUsd: null
			})
		];
		expect(runCostRows(unbilled)).toEqual([]);

		const cost = runCost(unbilled);
		expect(cost?.usage).toBeNull();
		expect(cost?.modelTimeMs).toBeNull();
		expect(cost?.models).toEqual([]);
	});

	it('sums a single-model run into five rows a reader can check by hand', () => {
		expect(runCostRows(threeCalls())).toEqual([
			// Three backend-recorded prices. This is never reconstructed from model ids.
			['Cost', '$0.01832625'],
			// 12,000 x 3 in, 384 x 3 out, over the three calls that reported a bill.
			['Tokens', '36k → 1.2k tok · 3 calls'],
			// 28,800 of 36,000 is 80% — the rate over the run, off `input` alone.
			['Prompt cache', '80% cached · 29k of 36k tok'],
			// 4,200ms x 3 = 12.6s inside models, across a feed spanning 60s: 21%.
			['Model time', '13s of 1m observed · 21%'],
			// Singular label, no call count: one model answered all three, and the
			// token row already said how many that was.
			['Model', 'claude-opus-4']
		]);
	});

	it('formats an explicit zero and sub-cent prices without inventing a missing amount', () => {
		expect(formatUsd(null)).toBeNull();
		expect(formatUsd(Number.NaN)).toBeNull();
		expect(formatUsd(-0.01)).toBeNull();
		expect(formatUsd(0)).toBe('$0.00');
		expect(formatUsd(COST_USD)).toBe('$0.00610875');
	});

	it('does not estimate dollars from tokens and model when price metadata is absent', () => {
		const rows = runCostRows(threeCalls().map((entry) => ({ ...entry, costUsd: null })));
		expect(rows.map(([label]) => label)).not.toContain('Cost');
		expect(rows).toContainEqual(['Tokens', '36k → 1.2k tok · 3 calls']);
	});

	it('names every model that answered, most-used first, when more than one did', () => {
		const rows = runCostRows([
			// Introduced first and answered least, so a list in feed order would put
			// it in front.
			row({ id: 'call-1', at: LLM_AT, model: 'gpt-5.5' }),
			row({ id: 'call-2', at: TOOL_AT, model: 'claude-opus-4' }),
			row({ id: 'call-3', at: SHELL_AT, model: 'claude-opus-4' })
		]);
		expect(rows).toContainEqual(['Models', 'claude-opus-4 (2) · gpt-5.5 (1)']);
		expect(rows.map(([label]) => label)).not.toContain('Model');
	});

	it('counts failed attempts and not the run\'s own outcome', () => {
		const rows = runCostRows([
			row({ id: 'call-1', at: LLM_AT, status: 'failed' }),
			row({ id: 'tool-1', at: TOOL_AT, kind: 'tool', status: 'failed', costUsd: null }),
			// The run's verdict, not a fourth thing that was tried. Counting it would
			// make every failed run's tally wrong by exactly one.
			row({
				id: 'run-end',
				at: RUN_ENDED_AT,
				kind: 'lifecycle',
				status: 'failed',
				title: 'Run failed',
				tokens: null,
				latencyMs: null,
				model: null,
				costUsd: null
			})
		]);
		expect(rows).toContainEqual(['Failed calls', '2']);
		expect(runCost(threeCalls())?.failures).toBe(0);
	});

	it('says nothing about failures rather than claiming there were none', () => {
		// The same rule `runSummary` follows for a step's retries: a zero segment is
		// dropped, because "no failures" and "failures not observable" are one row
		// apart and this list cannot tell them apart.
		expect(runCostRows(threeCalls()).map(([label]) => label)).not.toContain('Failed calls');
	});

	it('counts only the events that reported a bill, however many rows the feed has', () => {
		const rows = runCostRows([
			row({ id: 'call-1', at: LLM_AT }),
			row({ id: 'call-2', at: TOOL_AT }),
			row({ id: 'tool-1', at: SHELL_AT, kind: 'tool', tokens: null, model: null, costUsd: null }),
			row({ id: 'obs-1', at: OBSERVED_AT, kind: 'observation', tokens: null, model: null, costUsd: null }),
			row({
				id: 'run-1',
				at: RUN_STARTED_AT,
				kind: 'lifecycle',
				status: 'info',
				tokens: null,
				latencyMs: null,
				model: null,
				costUsd: null
			})
		]);
		// Two calls, not five events: `5 calls` would inflate the denominator of
		// every figure beside it.
		expect(rows).toContainEqual(['Tokens', '24k → 768 tok · 2 calls']);
		// Four rows were timed at 4,200ms; the lifecycle row was not.
		expect(runCost([])).toBeNull();
	});

	it('reports the token bill without a cache rate for a run that used no cache', () => {
		const cold = threeCalls().map((entry) => ({
			...entry,
			tokens: { input: INPUT_TOKENS, output: OUTPUT_TOKENS, cacheRead: null, cacheCreation: null }
		}));
		const rows = runCostRows(cold);
		expect(rows).toContainEqual(['Tokens', '36k → 1.2k tok · 3 calls']);
		// Not `0% cached`. Nothing was consulted, so nothing missed.
		expect(rows.map(([label]) => label)).not.toContain('Prompt cache');
	});

	it('drops the arrow rather than half of it, exactly as a single row does', () => {
		const halfBilled = threeCalls().map((entry) => ({
			...entry,
			tokens: { input: INPUT_TOKENS, output: null, cacheRead: null, cacheCreation: null }
		}));
		expect(runCostRows(halfBilled).map(([label]) => label)).not.toContain('Tokens');
	});

	it('compares model time to the feed span only when there is a span to compare to', () => {
		// One event: the span is zero, which is not a duration a reader can act on,
		// and `4s of 0s · Infinity%` is what a naive comparison renders.
		expect(runCostRows([row()])).toContainEqual(['Model time', '4s']);
		expect(runCost([row()])?.observedMs).toBeNull();
	});

	it('reports a share above 100% rather than clamping away the fact that calls overlapped', () => {
		// Three 4.2s calls inside a feed spanning one second: 12.6s of model time in
		// 1s of wall clock. Clamping to 100% would tell the reader the run was
		// saturated; 1260% tells them it ran three models at once.
		const concurrent = [
			row({ id: 'call-1', at: LLM_AT }),
			row({ id: 'call-2', at: LLM_AT + 500 }),
			row({ id: 'call-3', at: LLM_AT + 1_000 })
		];
		expect(runCostRows(concurrent)).toContainEqual(['Model time', '13s of 1s observed · 1260%']);
	});

	it('ignores an unrecorded instant when measuring the span, rather than reaching 1970', () => {
		// `deriveTimeline` coalesces a missing instant to `0`. Counted, it would make
		// the span fifty-six years and the share a rounding error.
		const withUntimed = [...threeCalls(), row({ id: 'call-4', at: 0 })];
		expect(runCost(withUntimed)?.observedMs).toBe(SHELL_AT - LLM_AT);
	});
});
