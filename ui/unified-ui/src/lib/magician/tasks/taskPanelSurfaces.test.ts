/**
 * Every surface that opens a task panel opens **the** task panel.
 *
 * The migration's rule, as an assertion: anything that opened a panel opens the
 * unified one, anything that navigated keeps navigating, and no surface carries
 * a second panel for a special case.
 *
 * **Why a source contract and not more component tests.** The thread route's
 * wiring is proven behaviourally in `ThreadTasksPanel.component.test.ts` — a real
 * render, a real click, a real verdict. Today's cannot be: the page throws on
 * `$todayStore.counts` under any board payload jsdom can supply, which aborts the
 * reactive flush before the panel's own statements run. So the proof is
 * transferred rather than duplicated: this asserts Today computes its model with
 * the *same* expression the tested route does, character for character. Sever
 * either one and this fails; let them drift and this fails too, which is the
 * only thing that keeps the transfer honest.
 *
 * Chat and `/crew/<id>` are on the same footing and for a blunter reason: both
 * are ~4,000-line surfaces whose mount needs a live chat scope or a hydrated
 * agent before any panel exists to assert on. **What they hand the panel is
 * proven behaviourally instead** — `executionPanelModel.test.ts` covers the
 * mapping and `UnifiedTaskPanel.component.test.ts` covers a model with only a
 * run — and what is left for this file is the join: that each surface actually
 * calls the adapter, with the id guards intact. That is the weaker half of the
 * proof, and it is stated here rather than left to be discovered.
 */
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const SRC = join(process.cwd(), 'src');

/** The routes that hold a real store `Task` and therefore reuse `toTaskPanelModel`. */
const MIGRATED_ROUTES = [
	'routes/(app)/t/[name]/tasks/+page.svelte'
] as const;

/**
 * The adapter call, exactly. The two id checks are part of it: an output list or
 * a run state belonging to the task the reader just left would render as this
 * task's, and every value involved is individually valid, so nothing downstream
 * can catch it. The second one now guards **two** slices — the mid-run ask and
 * the Run act's timeline both come out of `panelRunState` — so a surface that
 * dropped it would list another task's calls as well as its ask.
 */
const ADAPTER_CALL = `toTaskPanelModel(
					selectedTask,
					panelOutputsTaskId === selectedTask.id ? panelOutputs : null,
					panelRunStateTaskId === selectedTask.id ? panelRunState : null,`;

/**
 * **The third guard**, added with the run picker: a chosen run id, keyed on the
 * task it was chosen about.
 *
 * Pinned separately from the call above rather than folded into it, because the
 * two say different things and only one of them is about a payload. The first two
 * guards keep another task's *data* from rendering under this one; this one keeps
 * another task's *reader choice* from pointing this panel's Run act at an
 * execution the task has never had — a request the endpoint answers with
 * `execution panel state not found`, so the Run act would simply never load.
 *
 * The comment between them in the source is why this is a separate constant: a
 * single block spanning both would pin prose as well as code.
 */
const ADAPTER_RUN_SELECTION =
	'panelRunSelectionTaskId === selectedTask.id ? panelRunSelectionId : null,';

/**
 * The same call, from the chat surface, where the open task is `taskPanelTask`
 * rather than `selectedTask`. Written out rather than templated over the
 * variable name: what this pins is that the two id checks are *there*, and a
 * regex loose enough to span both would be loose enough to miss one of them
 * going away.
 */
const CHAT_ADAPTER_CALL = `toTaskPanelModel(
					taskPanelTask,
					panelOutputsTaskId === taskPanelTask.id ? panelOutputs : null,
					panelRunStateTaskId === taskPanelTask.id ? panelRunState : null,`;

/** The run picker's guard on the chat surface. See `ADAPTER_RUN_SELECTION`. */
const CHAT_ADAPTER_RUN_SELECTION =
	'panelRunSelectionTaskId === taskPanelTask.id ? panelRunSelectionId : null,';

/**
 * Every surface that opens a panel on something that is **not** a task, and the
 * adapter call that maps it. Both go through `toExecutionPanelModel`; what
 * differs is the file list, which is the only thing that can differ — chat holds
 * a real task id so `/outputs` answers, and a `/crew` cycle id has no outputs
 * endpoint at all, which is `null` rather than `[]`.
 */
const EXECUTION_SURFACES = [
	[
		'lib/magician/chat/ChatPanel.svelte',
		`toExecutionPanelModel(
					inspectState,
					inspectOutputsKey === inspectionKey(inspectPanelTarget) ? inspectOutputs : null
				)`
	],
	['routes/(app)/crew/[id]/+page.svelte', 'toExecutionPanelModel(executionPanelState, null)']
] as const;

function source(relativePath: string): string {
	return readFileSync(join(SRC, relativePath), 'utf8');
}

describe('task-panel surfaces', () => {
	it.each(MIGRATED_ROUTES)('%s feeds the shared panel through the shared adapter', (route) => {
		const text = source(route);
		expect(text).toContain(ADAPTER_CALL);
		expect(text).toContain(ADAPTER_RUN_SELECTION);
		expect(text).toContain('<TaskPanelDrawer');
		expect(text).toContain('task={panelModel}');
	});

	/**
	 * **The other half of the transfer, and it was missing.** Pinning the adapter
	 * call proves only that a value named `panelRunState` reaches the panel — a
	 * surface that declared it and never filled it would satisfy every assertion
	 * above while rendering no ask and no timeline at all, because both degrade to
	 * absence by design and absence is what an unfetched payload looks like. So
	 * the live read is pinned too. Every embedded surface now uses the same poll
	 * contract as `/tasks`: one target containing task + selected run, one
	 * last-good snapshot, and one explicit staleness path.
	 */
	it.each([
		'lib/magician/tasks/TasksWorkspace.svelte',
		...MIGRATED_ROUTES,
		'lib/magician/chat/ChatPanel.svelte'
	])(
		'%s follows the selected run through the shared last-good poll',
		(route) => {
			const text = source(route);
			expect(text).toContain('createTaskPanelPoll');
			expect(text).toContain('panelPollCadence(');
			expect(text).toContain('readTaskRunState(');
			expect(text).toContain('target.executionId');
			expect(text).toContain('panelRunState = state;');
			expect(text).toContain('panelRunStateTaskId = target.taskId;');
			expect(text).toContain('onFailure:');
			expect(text).toMatch(/(?:panelPoll|taskPanelPoll)\.stop\(\);/);
		}
	);

	it.each([...MIGRATED_ROUTES, 'lib/magician/chat/ChatPanel.svelte'])(
		'%s keeps output rows stable across ordinary task refreshes',
		(route) => {
			const text = source(route);
			expect(text).toContain('panelOutputsGuard');
			expect(text).toContain("`${taskId}:${");
			expect(text).toContain('if (panelOutputsTaskId !== taskId)');
		}
	);

	it.each([
		'lib/magician/tasks/TasksWorkspace.svelte',
		...MIGRATED_ROUTES,
		'lib/magician/chat/ChatPanel.svelte'
	])('%s keeps planning and direct execution distinct for pending tasks', (route) => {
		const text = source(route);
		expect(text).toContain("label: 'Plan'");
		expect(text).toContain("label: 'Run now'");
		expect(text).toContain('executeTaskDirect');
	});

	/**
	 * **`/tasks` says the same thing in a different place, because it is the one
	 * surface with a clock.** The three routes above re-read only when the store
	 * replaces the task object, so their guard lives in the loader that statement
	 * calls. `TasksWorkspace` polls (`taskPanelPoll.ts`), and a poll's reply is
	 * applied rather than requested — so the clear moved to where a task *change*
	 * is observable, and the reply's own target is what keeps it from landing under
	 * the wrong task. Both halves are pinned: without the first the Run act would
	 * carry the previous task's rows, and without the second it would blank four
	 * times a minute.
	 */
	it('lib/magician/tasks/TasksWorkspace.svelte clears the run on a task change, not on a poll', () => {
		const text = source('lib/magician/tasks/TasksWorkspace.svelte');
		expect(text).toContain(
			`$: if (panelRunStateTaskId !== null && panelRunStateTaskId !== (selectedTask?.id ?? null)) {
		clearPanelRunState(!isPanelOpen);
	}`
		);
		expect(text).toContain(`if (target.taskId !== (selectedTask?.id ?? null)) return;
			panelRunState = state;`);
	});

	/**
	 * **Both tabs of `/tasks` follow a live task the same way, through one module.**
	 *
	 * This is design §2's acceptance test applied to the poll: the two workspaces
	 * share the cadence rule and the aiming, and neither asks what kind of task it
	 * is holding. What differs is only what a read *is* — a run payload on one
	 * route, a details payload on the other — which is the difference between the
	 * two backends rather than between the two panels.
	 *
	 * A surface that grew its own `setInterval` beside this would pass every other
	 * assertion in this file while polling on a second clock nothing else knew
	 * about, so the shared call is what gets pinned.
	 */
	it.each([
		'lib/magician/tasks/TasksWorkspace.svelte',
		'lib/internalTasks/InternalTasksWorkspace.svelte'
	])('%s follows a live task through the shared poll', (route) => {
		const text = source(route);
		expect(text).toContain('createTaskPanelPoll');
		expect(text).toContain('panelPollCadence(');
		expect(text).toContain('panelPoll.aim(');
		expect(text).toContain('panelPoll.stop();');
		// The panel's clock is the only interval either workspace owns. A second one
		// here would be a poll nothing else in this file could see.
		expect(text.match(/setInterval\(/g) ?? []).toHaveLength(1);
	});

	/**
	 * Chat's task gestures — a task-status card's *Inspect run*, a task id in LLM
	 * prose, the PlannerDock's execute — all hold a real store `Task`, so they
	 * reuse `toTaskPanelModel` unchanged, exactly as `/today` and the thread
	 * workspace do.
	 */
	it('chat feeds its task drawer through the shared adapter', () => {
		const text = source('lib/magician/chat/ChatPanel.svelte');
		expect(text).toContain(CHAT_ADAPTER_CALL);
		expect(text).toContain(CHAT_ADAPTER_RUN_SELECTION);
		expect(text).toContain('task={taskPanelModel}');
	});

	/**
	 * An execution record and an agent cycle are not tasks, and neither is
	 * fabricated into one any more. They reach the same panel through the
	 * execution adapter — which is why no second panel has to exist for them.
	 */
	it.each(EXECUTION_SURFACES)('%s maps its run through the execution adapter', (route, call) => {
		const text = source(route);
		expect(text).toContain(call);
		expect(text).toContain('<TaskPanelDrawer');
	});

	/**
	 * The run drawer follows its run. The panel is a pure render of a prop, so
	 * the subscription cannot live in it — it belongs beside the fetch, in the
	 * surface, and that is a join no component test reaches for the reason this
	 * file exists.
	 *
	 * All three lines are asserted because each one alone is satisfiable while
	 * the feature is broken: a subscription that is never started, one that is
	 * started and never torn down (two runs writing one drawer), or one torn down
	 * only when the drawer closes rather than when the component goes.
	 */
	it('chat follows the inspected run live, and lets it go', () => {
		const text = source('lib/magician/chat/ChatPanel.svelte');
		expect(text).toContain('streamExecutionPanelState');
		expect(text).toContain(
			'$: if (browser) syncInspectionStream(inspectPanelOpen ? inspectPanelTarget : null);'
		);
		// In `onDestroy`, not only in the close handler: the subscription outlives
		// the drawer's markup and would otherwise keep writing into a destroyed
		// component's variables.
		expect(text).toMatch(/onDestroy\(\(\) => \{[\s\S]*?inspectStreamStop\?\.\(\)/);
	});

	/**
	 * **`/crew/<id>` follows its cycle too, and through the same module.** The
	 * delta stream reaches an agent cycle for a reason worth writing down: the
	 * synthetic `agent-cycle:` id never leaves the page, `panelDeltaState` tests
	 * scope and `executionIdOf(state)` and nothing else, and the id the target
	 * carries is the agent's real `current_execution_id`. Server-side the panel
	 * projector builds the pushed state with the same builder the fetch's endpoint
	 * uses, so a cycle this page can load is a cycle it can follow.
	 *
	 * The three lines chat's test pins are pinned here for the same three ways the
	 * feature is satisfiable while broken. The fourth assertion is the one this
	 * surface needs and chat does not: **one key decides the subject changed**, and
	 * the fetch and the subscription both hang off it. Ungated, `executionPanelTarget`
	 * is rebuilt on every agent re-hydrate — a new object with the same values —
	 * and the loader would blank the Run act and refill it a round trip later,
	 * while the subscription was torn down and re-established under a live run.
	 */
	it('crew follows the open cycle live, and lets it go', () => {
		const text = source('routes/(app)/crew/[id]/+page.svelte');
		expect(text).toContain('streamExecutionPanelState');
		expect(text).toContain(
			'$: if (browser) syncExecutionPanel(executionPanelOpen ? executionPanelTarget : null);'
		);
		expect(text).toMatch(/onDestroy\(\(\) => \{[\s\S]*?executionPanelStreamStop\?\.\(\)/);
		// The gate, and it guards both halves: below it the loader runs (which
		// clears) and the subscription is replaced. An unchanged cycle reaches
		// neither.
		expect(text).toContain(`const key = target === null ? null : cycleKey(target);
		if (key === executionPanelSubjectKey) return;
		executionPanelSubjectKey = key;`);
	});

	it.each([...MIGRATED_ROUTES, 'lib/magician/chat/ChatPanel.svelte'])(
		'%s owns no drawer chrome of its own',
		(route) => {
			const text = source(route);
			// The shell owns the scrim, the dialog role, focus capture and restore,
			// Escape and the close control. A surface that re-declared any of them
			// would be the seventh copy `TaskPanelDrawer` was extracted to prevent.
			expect(text).not.toContain('aria-label="Close task panel"');
			expect(text).not.toContain('class="task-panel-backdrop"');
		}
	);

	it.each([
		...MIGRATED_ROUTES,
		'routes/(app)/square/+page.svelte',
		'lib/magician/chat/ChatPanel.svelte',
		'routes/(app)/crew/[id]/+page.svelte'
	])('%s carries no second task panel', (route) => {
		const text = source(route);
		expect(text).not.toContain('ExecutionPanel.svelte');
		expect(text).not.toContain('DeepWorkPanel.svelte');
	});

	/**
	 * The retirement, as an assertion rather than a claim in a commit message.
	 * `ExecutionPanel.svelte` was the last panel with live mounts; it reached
	 * zero when chat and `/crew/<id>` migrated, and the file is gone. A surface
	 * that reintroduced it would pass every test above.
	 */
	it('has no ExecutionPanel left to mount', () => {
		expect(existsSync(join(SRC, 'routes/(app)/ExecutionPanel.svelte'))).toBe(false);
		expect(existsSync(join(SRC, 'test/fixtures/ExecutionPanelHarness.svelte'))).toBe(false);
	});

	/**
	 * The other retirement. `DeepWorkPanel` reached zero mounts when chat
	 * migrated, and its modules were kept only because `deriveFeedEntries` was
	 * the sole implementation of a capability worth restoring. That capability is
	 * now `taskTimeline.ts` on the Run act, so the directory is gone — and this
	 * asserts it rather than leaving the deletion to be undone by a copy nobody
	 * noticed was still there.
	 */
	it('has no DeepWorkPanel left, and one home for the timeline', () => {
		expect(existsSync(join(SRC, 'lib/magician/deepwork'))).toBe(false);
		expect(existsSync(join(SRC, 'test/fixtures/DeepWorkPanelHarness.svelte'))).toBe(false);
		expect(existsSync(join(SRC, 'lib/magician/tasks/taskTimeline.ts'))).toBe(true);
	});

	/**
	 * `/square`'s page-level panel was unreachable — every caller of its
	 * `openTaskPanel` sat inside a handler bound only to that panel's own events,
	 * and the one remaining entry was a `?selected=` parameter no code in the repo
	 * produces. Its real task panel is the Fleet HUD's Quest Journal, which mounts
	 * `TasksWorkspace` and therefore the unified panel.
	 */
	it('leaves /square with exactly one task panel, in the Fleet HUD', () => {
		expect(source('routes/(app)/square/+page.svelte')).not.toContain('openTaskPanel(');
		expect(source('lib/magician/square/hud/QuestJournal.svelte')).toContain('<TasksWorkspace');
	});

	/**
	 * The other half of the rule. These surfaces navigate today and must keep
	 * navigating: turning one into a panel is as much a regression as leaving a
	 * legacy panel mounted.
	 */
	it.each([
		['lib/shell/CommandPalette.svelte', 'Open task'],
		['lib/magician/components/execution/ExecutionControls.svelte', 'Inspect run'],
		['lib/monitors/MonitorDetailPanel.svelte', 'the monitor detail task link'],
		['lib/magician/components/PublishedScrollCanvas.svelte', 'a published surface task'],
		['lib/shell/vibe/VibeLegacyStudio.svelte', 'a Vibe run row']
	])('%s still navigates to /tasks (%s)', (route) => {
		expect(source(route)).toContain('/tasks?selected=');
	});
});
