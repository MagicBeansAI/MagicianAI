import { describe, expect, it } from 'vitest';
import {
	applyCodingEvent,
	detectStuck,
	emptySpineState,
	groupCards,
	snapWindowStart,
	spineCardsList,
	stripSummary,
	STRIP_MIN_RUN,
	STRIP_SUMMARY_MAX,
	STUCK_WINDOW_EVENTS,
	STUCK_WINDOW_MS,
	WINDOW_SNAP_MAX_ADVANCE,
	type ActionStrip,
	type CodingEvent,
	type SpineCard,
	type TurnGroup
} from './spineModel';

// ── fixtures ────────────────────────────────────────────────────────────────

let autoSeq = 0;

/** Minimal card fixture — ordering fields auto-increment so a list built in
 *  source order is already in display order (mirrors `spineCardsList`). */
function card(overrides: Partial<SpineCard> & Pick<SpineCard, 'id' | 'kind'>): SpineCard {
	autoSeq += 1;
	return {
		taskId: 'task-1',
		shadowId: 'shadow-1',
		turn: 1,
		sequence: autoSeq,
		order: autoSeq,
		ts: 1_000 + autoSeq,
		status: 'done',
		title: overrides.toolName ?? overrides.kind,
		detail: null,
		...overrides
	};
}

function action(id: string, toolName: string, extra: Partial<SpineCard> = {}): SpineCard {
	return card({ id, kind: 'action', toolName, title: toolName, ...extra });
}

function stripsOf(groups: TurnGroup[]): ActionStrip[] {
	return groups.flatMap((g) =>
		g.items.filter((item): item is ActionStrip => item.kind === 'action-strip')
	);
}

function itemKinds(group: TurnGroup): string[] {
	return group.items.map((item) => (item.kind === 'card' ? item.card.kind : item.kind));
}

// ── strip formation ─────────────────────────────────────────────────────────

describe('groupCards — strip formation', () => {
	it('folds runs of ≥3 consecutive action cards into one strip, in order', () => {
		const cards = [action('a1', 'read'), action('a2', 'read'), action('a3', 'edit')];
		const groups = groupCards(cards);
		expect(groups).toHaveLength(1);
		expect(itemKinds(groups[0])).toEqual(['action-strip']);
		const strip = groups[0].items[0] as ActionStrip;
		expect(strip.count).toBe(3);
		expect(strip.cards.map((c) => c.id)).toEqual(['a1', 'a2', 'a3']);
		expect(STRIP_MIN_RUN).toBe(3);
	});

	it('leaves runs of <3 actions as individual cards', () => {
		const cards = [action('a1', 'read'), action('a2', 'edit')];
		const groups = groupCards(cards);
		expect(itemKinds(groups[0])).toEqual(['action', 'action']);
		expect(stripsOf(groups)).toHaveLength(0);
	});

	it('non-action kinds break strips (test/diff/error/message/plan/reasoning)', () => {
		// 2 actions, a test card, 2 more actions: neither action run reaches 3.
		const brokenByTest = groupCards([
			action('a1', 'read'),
			action('a2', 'read'),
			card({ id: 't1', kind: 'test', toolName: 'vitest' }),
			action('a3', 'read'),
			action('a4', 'read')
		]);
		expect(stripsOf(brokenByTest)).toHaveLength(0);
		expect(itemKinds(brokenByTest[0])).toEqual(['action', 'action', 'test', 'action', 'action']);

		// 3 actions | diff | 3 actions → two separate strips around the diff.
		const aroundDiff = groupCards([
			action('b1', 'read'),
			action('b2', 'read'),
			action('b3', 'read'),
			card({ id: 'd1', kind: 'diff' }),
			action('b4', 'edit'),
			action('b5', 'edit'),
			action('b6', 'edit')
		]);
		expect(itemKinds(aroundDiff[0])).toEqual(['action-strip', 'diff', 'action-strip']);

		// every other non-action kind individually breaks a run
		for (const kind of ['error', 'message', 'plan', 'reasoning', 'completed'] as const) {
			const groups = groupCards([
				action('c1', 'read'),
				action('c2', 'read'),
				card({ id: `k-${kind}`, kind }),
				action('c3', 'read')
			]);
			expect(stripsOf(groups), `kind=${kind} must break the strip`).toHaveLength(0);
		}
	});
});

// ── summary digest ──────────────────────────────────────────────────────────

describe('stripSummary', () => {
	it('coalesces consecutive repeated tool names with ×N, order-preserving', () => {
		const cards = [
			action('s1', 'bash', { args: JSON.stringify({ command: 'npm test' }) }),
			action('s2', 'read'),
			action('s3', 'read'),
			action('s4', 'read'),
			action('s5', 'edit'),
			action('s6', 'edit')
		];
		// single shell call shows its command; repeats coalesce by tool name
		expect(stripSummary(cards)).toBe('npm test, read ×3, edit ×2');
	});

	it('preserves order for non-consecutive repeats (no global merge)', () => {
		const cards = [action('s1', 'read'), action('s2', 'edit'), action('s3', 'read')];
		expect(stripSummary(cards)).toBe('read, edit, read');
	});

	it('caps the digest and appends an ellipsis token', () => {
		const cards = Array.from({ length: 30 }, (_, i) =>
			action(`s${i}`, `some_long_tool_name_${i}`)
		);
		const summary = stripSummary(cards);
		// The trailing '…' token joins AFTER the cap check, so the rendered digest
		// can exceed STRIP_SUMMARY_MAX by at most the join overhead (', ' + '…').
		const DIGEST_JOIN_SLACK = 3;
		expect(summary.length).toBeLessThanOrEqual(STRIP_SUMMARY_MAX + DIGEST_JOIN_SLACK);
		expect(summary.endsWith('…')).toBe(true);
	});

	it('is surfaced on the strip itself', () => {
		const groups = groupCards([
			action('a1', 'read'),
			action('a2', 'read'),
			action('a3', 'edit')
		]);
		const strip = groups[0].items[0] as ActionStrip;
		expect(strip.summary).toBe('read ×2, edit');
	});
});

// ── live tail ───────────────────────────────────────────────────────────────

describe('groupCards — live tail rule', () => {
	it('never folds the last card while live; preceding completed run still folds', () => {
		const cards = [
			action('a1', 'read'),
			action('a2', 'read'),
			action('a3', 'read'),
			action('a4', 'edit', { status: 'running' })
		];
		const live = groupCards(cards, { live: true });
		expect(itemKinds(live[0])).toEqual(['action-strip', 'action']);
		const strip = live[0].items[0] as ActionStrip;
		expect(strip.count).toBe(3);
		expect((live[0].items[1] as { kind: 'card'; card: SpineCard }).card.id).toBe('a4');

		// same list, run finished → the whole run folds
		const done = groupCards(cards, { live: false });
		expect(itemKinds(done[0])).toEqual(['action-strip']);
		expect((done[0].items[0] as ActionStrip).count).toBe(4);
	});

	it('keeps everything individual when excluding the live tail drops the run below 3', () => {
		const cards = [action('a1', 'read'), action('a2', 'read'), action('a3', 'read')];
		const groups = groupCards(cards, { live: true });
		expect(itemKinds(groups[0])).toEqual(['action', 'action', 'action']);
	});

	it('folds normally while live when the last card is not an action', () => {
		const cards = [
			action('a1', 'read'),
			action('a2', 'read'),
			action('a3', 'read'),
			card({ id: 'm1', kind: 'message', status: 'running' })
		];
		const groups = groupCards(cards, { live: true });
		expect(itemKinds(groups[0])).toEqual(['action-strip', 'message']);
	});
});

// ── turn boundaries ─────────────────────────────────────────────────────────

describe('groupCards — turn boundaries', () => {
	it('starts a new group when turn or shadowId changes', () => {
		const cards = [
			card({ id: 'h1', kind: 'run_header', turn: 0 }),
			card({ id: 'r1', kind: 'reasoning', turn: 1 }),
			card({ id: 'm1', kind: 'message', turn: 1 }),
			card({ id: 'r2', kind: 'reasoning', turn: 2 }),
			card({ id: 'h2', kind: 'run_header', turn: 0, shadowId: 'shadow-2' })
		];
		const groups = groupCards(cards);
		expect(groups.map((g) => [g.shadowId, g.turn])).toEqual([
			['shadow-1', 0],
			['shadow-1', 1],
			['shadow-1', 2],
			['shadow-2', 0]
		]);
		// group ids are POSITION-INDEPENDENT — turn::<shadowId>::<turnNumber>,
		// never derived from whichever card happens to lead the rendered slice
		expect(groups.map((g) => g.id)).toEqual([
			'turn::shadow-1::0',
			'turn::shadow-1::1',
			'turn::shadow-1::2',
			'turn::shadow-2::0'
		]);
	});

	it('labels numbered turns only when a run spans multiple turns', () => {
		const multi = groupCards([
			card({ id: 'h1', kind: 'run_header', turn: 0 }),
			card({ id: 'm1', kind: 'message', turn: 1 }),
			card({ id: 'm2', kind: 'message', turn: 2 })
		]);
		expect(multi.map((g) => g.label)).toEqual([null, 'Turn 1', 'Turn 2']);

		// single-turn run: a "Turn 1" header would be noise
		const single = groupCards([
			card({ id: 'h2', kind: 'run_header', turn: 0, shadowId: 's-single' }),
			card({ id: 'm3', kind: 'message', turn: 1, shadowId: 's-single' })
		]);
		expect(single.map((g) => g.label)).toEqual([null, null]);
	});
});

// ── stability across incremental appends ────────────────────────────────────

describe('groupCards — identity stability', () => {
	it('same prefix → same group and strip ids (no keyed-each churn)', () => {
		const base = [
			card({ id: 'h1', kind: 'run_header', turn: 0 }),
			action('a1', 'read'),
			action('a2', 'read'),
			action('a3', 'read'),
			action('a4', 'edit')
		];
		const before = groupCards(base);
		const after = groupCards([...base, action('a5', 'edit'), card({ id: 'm1', kind: 'message' })]);

		// group ids for the shared prefix are unchanged — and position-independent
		// (shadow+turn), so they'd survive a window-start slide too
		expect(after.map((g) => g.id).slice(0, before.length)).toEqual(before.map((g) => g.id));
		expect(before.map((g) => g.id)).toEqual(['turn::shadow-1::0', 'turn::shadow-1::1']);

		// the strip keeps its id (keyed by FIRST member card) while growing
		const stripBefore = stripsOf(before)[0];
		const stripAfter = stripsOf(after)[0];
		expect(stripBefore.id).toBe(stripAfter.id);
		expect(stripBefore.count).toBe(4);
		expect(stripAfter.count).toBe(5);

		// an expansion Set keyed on strip ids survives the re-derivation
		const expandedStrips = new Set([stripBefore.id]);
		expect(expandedStrips.has(stripAfter.id)).toBe(true);
	});

	it('does not mutate the input cards (pure derivation)', () => {
		const cards = [action('a1', 'read'), action('a2', 'read'), action('a3', 'read')];
		const snapshot = JSON.stringify(cards);
		groupCards(cards, { live: true });
		groupCards(cards, { live: false });
		expect(JSON.stringify(cards)).toBe(snapshot);
	});
});

// ── window-start snapping ───────────────────────────────────────────────────

describe('snapWindowStart', () => {
	it('yields stable strip + group ids as the window start slides through an action run', () => {
		const cards = [
			card({ id: 'h1', kind: 'run_header', turn: 0 }),
			...Array.from({ length: 10 }, (_, i) => action(`a${i + 1}`, 'read')),
			card({ id: 'm1', kind: 'message' })
		];
		const stripIds = new Set<string>();
		const groupIds = new Set<string>();
		for (let start = 0; start < cards.length; start += 1) {
			const snapped = snapWindowStart(cards, start);
			const groups = groupCards(cards.slice(snapped));
			for (const g of groups) groupIds.add(g.id);
			for (const s of stripsOf(groups)) stripIds.add(s.id);
		}
		// Every possible window start renders either the run's TRUE first-member
		// strip id or no strip at all — never a mid-run id like strip::a4 — and
		// group ids stay position-independent throughout the slide.
		expect([...stripIds]).toEqual(['strip::a1']);
		expect([...groupIds].sort()).toEqual(['turn::shadow-1::0', 'turn::shadow-1::1']);
	});

	it('advances a bisecting start to the first card past the action run', () => {
		const cards = [
			card({ id: 'h1', kind: 'run_header', turn: 0 }),
			...Array.from({ length: 10 }, (_, i) => action(`a${i + 1}`, 'read')),
			card({ id: 'm1', kind: 'message' })
		];
		// start bisects the run (a1..a10 occupy indices 1..10) → snap to m1
		expect(snapWindowStart(cards, 5)).toBe(11);
		// hidden-count truth: the caller derives "N earlier" from the SNAPPED start
	});

	it('mega-run guard: returns the original start when the remainder exceeds the cap', () => {
		const mega = [
			card({ id: 'h1', kind: 'run_header', turn: 0 }),
			...Array.from({ length: WINDOW_SNAP_MAX_ADVANCE + 20 }, (_, i) => action(`x${i}`, 'read')),
			card({ id: 'm1', kind: 'message' })
		];
		// bisecting early: > cap actions remain ahead → the snap declines (hiding
		// that much history would cost more than the id-churn edge it prevents)
		expect(snapWindowStart(mega, 5)).toBe(5);
		// bisecting near the run's end: remainder fits within the cap → snaps past
		const nearEnd = mega.length - 1 - 10;
		expect(snapWindowStart(mega, nearEnd)).toBe(mega.length - 1);
	});

	it('does not advance when the start is not a bisection', () => {
		// the run STARTS at the boundary (predecessor is a same-turn message):
		// the run is intact, skipping it would hide it for nothing
		const intact = [
			card({ id: 'm0', kind: 'message' }),
			action('a1', 'read'),
			action('a2', 'read'),
			action('a3', 'read')
		];
		expect(snapWindowStart(intact, 1)).toBe(1);
		// start ≤ 0 / non-action starts are untouched
		expect(snapWindowStart(intact, 0)).toBe(0);

		// a turn boundary between predecessor and start means a NEW run begins
		// at the window start — also not a bisection
		const turnSplit = [
			action('b1', 'read', { turn: 1 }),
			action('b2', 'read', { turn: 2 }),
			action('b3', 'read', { turn: 2 }),
			action('b4', 'read', { turn: 2 })
		];
		expect(snapWindowStart(turnSplit, 1)).toBe(1);
	});
});

// ── stuck detection: windowing + check-runner exemption ────────────────────

describe('detectStuck — windowing + check-runner exemption', () => {
	const BASE_TS = 10_000_000;

	it('healthy red→green loop: the same test command ×3 is NOT stuck', () => {
		const args = JSON.stringify({ command: 'npm test' });
		const cards = [
			action('t1', 'bash', { args, ts: BASE_TS }),
			card({ id: 'm1', kind: 'message', ts: BASE_TS + 1_000 }),
			action('t2', 'bash', { args, ts: BASE_TS + 2_000 }),
			card({ id: 'm2', kind: 'message', ts: BASE_TS + 3_000 }),
			action('t3', 'bash', { args, ts: BASE_TS + 4_000 })
		];
		expect(detectStuck(cards)).toBeNull();
	});

	it('exempts check/build/lint shell commands and test-kind cards', () => {
		for (const command of ['npm run lint', 'cargo build', 'npx svelte-check', 'cargo check']) {
			const args = JSON.stringify({ command });
			const cards = [
				action('c1', 'bash', { args, ts: BASE_TS }),
				action('c2', 'bash', { args, ts: BASE_TS + 1_000 }),
				action('c3', 'bash', { args, ts: BASE_TS + 2_000 })
			];
			expect(detectStuck(cards), `command=${command}`).toBeNull();
		}
		const testKind = [
			card({ id: 'k1', kind: 'test', toolName: 'vitest', args: '{}', ts: BASE_TS }),
			card({ id: 'k2', kind: 'test', toolName: 'vitest', args: '{}', ts: BASE_TS + 1_000 }),
			card({ id: 'k3', kind: 'test', toolName: 'vitest', args: '{}', ts: BASE_TS + 2_000 })
		];
		expect(detectStuck(testKind)).toBeNull();
	});

	it('flags the same non-check tool call ×3 within the window', () => {
		const args = JSON.stringify({ path: 'src/app.ts' });
		const cards = [
			action('r1', 'read', { args, ts: BASE_TS }),
			action('r2', 'read', { args, ts: BASE_TS + 30_000 }),
			action('r3', 'read', { args, ts: BASE_TS + 60_000 })
		];
		expect(detectStuck(cards)).toEqual({ toolName: 'read', args });
	});

	it('does NOT flag ×3 spread over more than the time window', () => {
		const args = JSON.stringify({ path: 'src/app.ts' });
		// r1 sits >STUCK_WINDOW_MS behind the newest card → falls out of the time
		// window; only r2+r3 count (2 < threshold).
		const cards = [
			action('r1', 'read', { args, ts: BASE_TS }),
			action('r2', 'read', { args, ts: BASE_TS + STUCK_WINDOW_MS / 2 + 20_000 }),
			action('r3', 'read', { args, ts: BASE_TS + STUCK_WINDOW_MS + 20_000 })
		];
		expect(detectStuck(cards)).toBeNull();
	});

	it('does NOT flag ×3 separated by more than the event window', () => {
		const args = JSON.stringify({ path: 'src/app.ts' });
		const half = Math.floor(STUCK_WINDOW_EVENTS / 2);
		// Repeats interleaved with enough other events that the last
		// STUCK_WINDOW_EVENTS-card slice never holds all three.
		const cards = [
			action('r1', 'read', { args, ts: BASE_TS }),
			...Array.from({ length: half }, (_, i) =>
				card({ id: `fa${i}`, kind: 'message', ts: BASE_TS + 1_000 + i })
			),
			action('r2', 'read', { args, ts: BASE_TS + 2_000 }),
			...Array.from({ length: half }, (_, i) =>
				card({ id: `fb${i}`, kind: 'message', ts: BASE_TS + 3_000 + i })
			),
			action('r3', 'read', { args, ts: BASE_TS + 4_000 })
		];
		expect(detectStuck(cards)).toBeNull();
	});

	it('command-head scoping: a file path mentioning tests is NOT exempt', () => {
		// `cat src/tests/x.ts` is a read, not a test run — the check-runner
		// exemption must key off the command HEAD, not the full args JSON
		// (where `\btests?\b` matches inside the path).
		const args = JSON.stringify({ command: 'cat src/tests/x.ts' });
		const cards = [
			action('h1', 'bash', { args, ts: BASE_TS }),
			action('h2', 'bash', { args, ts: BASE_TS + 1_000 }),
			action('h3', 'bash', { args, ts: BASE_TS + 2_000 })
		];
		expect(detectStuck(cards)).toEqual({ toolName: 'bash', args });
	});

	it('command-head scoping: `npm run test:unit` IS exempt', () => {
		const args = JSON.stringify({ command: 'npm run test:unit' });
		const cards = [
			action('u1', 'bash', { args, ts: BASE_TS }),
			action('u2', 'bash', { args, ts: BASE_TS + 1_000 }),
			action('u3', 'bash', { args, ts: BASE_TS + 2_000 })
		];
		expect(detectStuck(cards)).toBeNull();
	});

	it('empty input and args-less repeats are never stuck', () => {
		expect(detectStuck([])).toBeNull();
		const noArgs = [
			action('n1', 'read', { ts: BASE_TS }),
			action('n2', 'read', { ts: BASE_TS + 1_000 }),
			action('n3', 'read', { ts: BASE_TS + 2_000 })
		];
		expect(detectStuck(noArgs)).toBeNull();
	});
});

// ── end-to-end: real event fold → grouped ───────────────────────────────────

describe('groupCards over applyCodingEvent output', () => {
	function ev(eventType: string, payload: Record<string, unknown>, ts: number): CodingEvent {
		return {
			eventType,
			payload: { shadow_workspace_id: 's1', task_id: 't1', ...payload },
			ts
		};
	}

	it('run header anchors turn 0; the first turn folds its tool run', () => {
		const state = emptySpineState();
		const events: CodingEvent[] = [
			ev('coding.started', { prompt_preview: 'Build it', sequence: 1 }, 1),
			ev('coding.turn.started', { sequence: 2 }, 2),
			ev('coding.tool.started', { tool_name: 'read', tool_call_id: 'c1', sequence: 3 }, 3),
			ev('coding.tool.finished', { tool_name: 'read', tool_call_id: 'c1', sequence: 4 }, 4),
			ev('coding.tool.started', { tool_name: 'read', tool_call_id: 'c2', sequence: 5 }, 5),
			ev('coding.tool.finished', { tool_name: 'read', tool_call_id: 'c2', sequence: 6 }, 6),
			ev('coding.tool.started', { tool_name: 'edit', tool_call_id: 'c3', sequence: 7 }, 7),
			ev('coding.tool.finished', { tool_name: 'edit', tool_call_id: 'c3', sequence: 8 }, 8),
			ev('coding.message', { delta: 'Done.', sequence: 9 }, 9)
		];
		for (const event of events) applyCodingEvent(state, event);

		const groups = groupCards(spineCardsList(state));
		expect(groups).toHaveLength(2);
		expect(itemKinds(groups[0])).toEqual(['run_header']);
		expect(itemKinds(groups[1])).toEqual(['action-strip', 'message']);
		const strip = groups[1].items[0] as ActionStrip;
		expect(strip.count).toBe(3);
		expect(strip.summary).toBe('read ×2, edit');
	});

	it('classifies shell tool kind by command head: path args are NOT test cards', () => {
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev(
				'coding.tool.started',
				{ tool_name: 'bash', tool_call_id: 'x1', args: { command: 'cat src/tests/x.ts' }, sequence: 1 },
				1
			)
		);
		applyCodingEvent(
			state,
			ev(
				'coding.tool.started',
				{ tool_name: 'bash', tool_call_id: 'x2', args: { command: 'npm run test:unit' }, sequence: 2 },
				2
			)
		);
		const cards = spineCardsList(state);
		expect(cards.find((c) => c.toolCallId === 'x1')?.kind).toBe('action');
		expect(cards.find((c) => c.toolCallId === 'x2')?.kind).toBe('test');
	});
});

describe('budget stops read differently from coding failures', () => {
	function ev(eventType: string, payload: Record<string, unknown>, ts: number): CodingEvent {
		return {
			eventType,
			payload: { shadow_workspace_id: 's1', task_id: 't1', ...payload },
			ts
		};
	}

	function cardTitles(state: ReturnType<typeof emptySpineState>): string[] {
		return spineCardsList(state).map((card) => card.title);
	}

	it('a genuine failure still reads as a failure', () => {
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev('coding.failed', { stage: 'run_turn', error: 'Pi RPC failed', sequence: 1 }, 1)
		);
		const card = spineCardsList(state)[0];
		expect(card.title).toBe('Failed (run_turn)');
		expect(card.isError).toBe(true);
	});

	it('a no-progress stop names the bound that fired, not a failure', () => {
		// "Ran out of clock mid-thought" and "the model was wrong" were
		// indistinguishable before the typed cause existed.
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev(
				'coding.failed',
				{
					stage: 'run_turn',
					error: 'no progress in the tool phase for 1500s',
					budget_stop: true,
					termination: { cause: 'no_progress', phase: 'tool' },
					sequence: 1
				},
				1
			)
		);
		const card = spineCardsList(state)[0];
		expect(card.title).toBe('Stopped — no progress');
		expect(card.isError).toBe(false);
	});

	it('a turn timeout and a task-budget stop are named separately', () => {
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev(
				'coding.failed',
				{ stage: 'run_turn', budget_stop: true, termination: { cause: 'turn_timeout' }, sequence: 1 },
				1
			)
		);
		applyCodingEvent(
			state,
			ev(
				'coding.failed',
				{ stage: 'run_turn', budget_stop: true, termination: { cause: 'task_budget' }, sequence: 2 },
				2
			)
		);
		expect(cardTitles(state)).toEqual([
			'Stopped — turn ran out of time',
			'Stopped — task budget spent'
		]);
	});

	it('an unrecognised cause degrades to a plain stop rather than a failure', () => {
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev('coding.failed', { stage: 'run_turn', budget_stop: true, sequence: 1 }, 1)
		);
		expect(spineCardsList(state)[0].title).toBe('Stopped');
	});

	it('a preflight budget exhaustion gets its own card', () => {
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev(
				'coding.budget_exhausted',
				{
					termination: { cause: 'task_budget' },
					budget: { task_active_spent_secs: 7300, task_active_remaining_secs: 0 },
					sequence: 1
				},
				1
			)
		);
		const card = spineCardsList(state)[0];
		expect(card.title).toBe('Out of task budget');
		expect(card.isError).toBe(false);
	});
});

describe('budget telemetry folds onto the run', () => {
	function ev(eventType: string, payload: Record<string, unknown>, ts: number): CodingEvent {
		return {
			eventType,
			payload: { shadow_workspace_id: 's1', task_id: 't1', ...payload },
			ts
		};
	}

	it('a completed turn carries spend and headroom', () => {
		// Without this, forty silent minutes and a hang look identical.
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev(
				'coding.completed',
				{
					budget: { task_active_spent_secs: 1800, task_active_remaining_secs: 4200 },
					sequence: 1
				},
				1
			)
		);
		expect(state.meta.get('s1')?.budget).toEqual({ spentSecs: 1800, remainingSecs: 4200 });
	});

	it('a payload without budget telemetry leaves the run unannotated', () => {
		// Durable events recorded before the field existed must not be read as
		// "zero budget remaining".
		const state = emptySpineState();
		applyCodingEvent(state, ev('coding.completed', { sequence: 1 }, 1));
		expect(state.meta.get('s1')?.budget).toBeNull();
	});

	it('an unbounded run reports spend with no remaining, not zero remaining', () => {
		// No whole-task ceiling is the default. Coercing the absent remainder to
		// zero would render a healthy long run as out of budget.
		const state = emptySpineState();
		applyCodingEvent(
			state,
			ev(
				'coding.completed',
				{
					budget: {
						task_active_spent_secs: 14_400,
						task_active_max_secs: null,
						task_active_remaining_secs: null
					},
					sequence: 1
				},
				1
			)
		);
		expect(state.meta.get('s1')?.budget).toEqual({ spentSecs: 14_400, remainingSecs: null });
	});
});
