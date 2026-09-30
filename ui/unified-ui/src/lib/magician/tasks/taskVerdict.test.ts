import { describe, it, expect } from 'vitest';
import { deriveVerdict, STALL_AFTER_MS, type VerdictInput } from './taskVerdict';

const base: VerdictInput = {
	status: 'running',
	attention: null,
	queuedFor: null,
	error: null,
	currentStep: 4,
	totalSteps: 7,
	currentStepLabel: 'Searching memory for "quarterly plan"',
	elapsedMs: null,
	lastProgressAt: null,
	now: 0
};

describe('deriveVerdict', () => {
	it('reports a running task with its step and what it is doing', () => {
		const v = deriveVerdict(base);
		expect(v.state).toBe('running');
		expect(v.headline).toBe('Running · step 4 of 7');
		expect(v.detail).toBe('Searching memory for "quarterly plan"');
	});

	it('drops the denominator when the run was never planned', () => {
		const v = deriveVerdict({ ...base, totalSteps: null });
		expect(v.headline).toBe('Running · step 4');
	});

	it('ranks waiting-on-you above finished', () => {
		const v = deriveVerdict({
			...base,
			status: 'finished',
			attention: { source: 'plan_approval', summary: null, raisedAt: null }
		});
		expect(v.state).toBe('waiting');
		expect(v.detail).toBe('Approve the plan before it can run');
	});

	it('has copy for every HITL source', () => {
		// This array is not the exhaustiveness guard — adding a ninth `HitlSource`
		// would not fail here, it would fail to compile `ATTENTION_COPY`, which is
		// a `Record<HitlSource, string>`. What this covers is the rendering rule:
		// every source produces copy, and none of them leaks the enum.
		const sources = [
			'agentic',
			'user_request',
			'approval',
			'plan_approval',
			'clarification',
			'escalation',
			'diff_approval',
			'bot_auth'
		] as const;
		for (const source of sources) {
			const v = deriveVerdict({ ...base, attention: { source, summary: null, raisedAt: null } });
			expect(v.state).toBe('waiting');
			expect(v.detail.length).toBeGreaterThan(0);
			expect(v.detail).not.toContain(source);
		}
	});

	it('prefers the attention summary over the generic per-source copy', () => {
		const v = deriveVerdict({
			...base,
			attention: { source: 'clarification', summary: 'Which quarter?', raisedAt: null }
		});
		expect(v.detail).toBe('Which quarter?');
	});

	it('treats an empty attention summary as absent rather than rendering a blank explanation', () => {
		// A nullish test alone falls back only on `null`, so a backend sending `''`
		// — or a summary that came from a trimmed text field — puts a waiting row on
		// screen with nothing at all on its second line. Blank is the one detail
		// design §3 reserves for `finished`, and a waiting row that explains nothing
		// is the §6 rule broken at the scale of one line.
		for (const summary of ['', '   ']) {
			const v = deriveVerdict({
				...base,
				attention: { source: 'plan_approval', summary, raisedAt: null }
			});
			expect(v.state).toBe('waiting');
			expect(v.detail).toBe('Approve the plan before it can run');
		}
	});

	it('says how long you have been blocked, because 4m and 3h mean different things', () => {
		const v = deriveVerdict({
			...base,
			attention: { source: 'plan_approval', summary: null, raisedAt: 0 },
			now: 4 * 60_000
		});
		expect(v.headline).toBe('Waiting on you · 4m');
	});

	it('omits the duration rather than inventing one when the ask has no timestamp', () => {
		const v = deriveVerdict({
			...base,
			attention: { source: 'plan_approval', summary: null, raisedAt: null }
		});
		expect(v.headline).toBe('Waiting on you');
	});

	it('omits the duration rather than rendering a negative one when the clocks disagree', () => {
		// `raisedAt` is the server's clock and `now` is the browser's, so skew is
		// routine. Every negative value passes the `< 60s` test, so an unguarded
		// version prints `-5s` and `-5400s` — a number worse than no number. This
		// is the same answer as a missing timestamp: we do not know.
		const skewed = (raisedAtMs: number) =>
			deriveVerdict({
				...base,
				attention: { source: 'plan_approval', summary: null, raisedAt: raisedAtMs },
				now: 0
			}).headline;

		expect(skewed(5_000)).toBe('Waiting on you');
		expect(skewed(90 * 60_000)).toBe('Waiting on you');
		// The boundary the guard must not swallow: no skew at all is 0s, a real
		// reading, not an unknown one.
		expect(skewed(0)).toBe('Waiting on you · 0s');
	});

	it('drops a negative elapsed time on finished and cancelled too, for one rule not two', () => {
		expect(deriveVerdict({ ...base, status: 'finished', elapsedMs: -1_000 }).headline).toBe(
			'Finished'
		);
		expect(deriveVerdict({ ...base, status: 'cancelled', elapsedMs: -1_000 }).headline).toBe(
			'Cancelled'
		);
	});

	it('treats a non-finite elapsed time as unknown rather than rendering NaN at the reader', () => {
		// These timestamps arrive as JSON through an adapter, so a subtraction
		// against a missing field reaches this function as `NaN` rather than as
		// `null`. `NaN < 0` is false, so an unguarded version falls through every
		// tier to the hour branch and prints `Finished · NaNh NaNm`.
		for (const elapsedMs of [Number.NaN, Number.POSITIVE_INFINITY]) {
			expect(deriveVerdict({ ...base, status: 'finished', elapsedMs }).headline).toBe('Finished');
			expect(deriveVerdict({ ...base, status: 'cancelled', elapsedMs }).headline).toBe(
				'Cancelled'
			);
		}

		const blocked = (raisedAt: number) =>
			deriveVerdict({
				...base,
				attention: { source: 'plan_approval', summary: null, raisedAt },
				now: 0
			}).headline;

		expect(blocked(Number.NaN)).toBe('Waiting on you');
		// `0 - -Infinity` is `+Infinity`, so this one clears the negative guard and
		// needs the finite check of its own.
		expect(blocked(Number.NEGATIVE_INFINITY)).toBe('Waiting on you');
	});

	it('scales the duration unit so a long block reads as abandoned rather than as 180m', () => {
		const blocked = (ms: number) =>
			deriveVerdict({
				...base,
				attention: { source: 'plan_approval', summary: null, raisedAt: 0 },
				now: ms
			}).headline;

		expect(blocked(45_000)).toBe('Waiting on you · 45s');
		expect(blocked(59_000)).toBe('Waiting on you · 59s');
		// Each bucket boundary, so a separator or ordering slip cannot hide.
		expect(blocked(60_000)).toBe('Waiting on you · 1m');
		expect(blocked(61_000)).toBe('Waiting on you · 1m 1s');
		expect(blocked(192_000)).toBe('Waiting on you · 3m 12s');
		expect(blocked(59 * 60_000 + 59_000)).toBe('Waiting on you · 59m 59s');
		expect(blocked(60 * 60_000)).toBe('Waiting on you · 1h');
		expect(blocked(61 * 60_000)).toBe('Waiting on you · 1h 1m');
		expect(blocked(80 * 60_000)).toBe('Waiting on you · 1h 20m');
		expect(blocked(3 * 60 * 60_000)).toBe('Waiting on you · 3h');
		// Every other hour-scale case above divides evenly into minutes, so all of
		// them pass against an implementation that appends a non-zero seconds
		// component. This one does not: the hour bucket drops seconds always, and
		// no duration is ever three units.
		expect(blocked(80 * 60_000 + 5_000)).toBe('Waiting on you · 1h 20m');
	});

	it('ranks waiting-on-you above failed, so the person blocking it is not hidden', () => {
		const v = deriveVerdict({
			...base,
			status: 'failed',
			error: 'boom',
			attention: { source: 'escalation', summary: null, raisedAt: null }
		});
		expect(v.state).toBe('waiting');
		expect(v.detail).toBe('The run got stuck and needs a decision');
	});

	it('ranks waiting-on-you above stalled', () => {
		const v = deriveVerdict({
			...base,
			attention: { source: 'clarification', summary: null, raisedAt: null },
			lastProgressAt: 0,
			now: 6 * 60_000
		});
		expect(v.state).toBe('waiting');
		expect(v.detail).toBe('Answer a question so planning can finish');
	});

	it('ranks waiting-on-you above cancelled', () => {
		const v = deriveVerdict({
			...base,
			status: 'cancelled',
			elapsedMs: 100_000,
			attention: { source: 'approval', summary: null, raisedAt: null }
		});
		expect(v.state).toBe('waiting');
		// Both lines, not just the state: an inversion that returned `waiting` while
		// rendering someone else's copy would pass a state-only assertion.
		expect(v.headline).toBe('Waiting on you');
		expect(v.detail).toBe('Approve this before it can continue');
	});

	it('ranks waiting-on-you above queued, which is the most common waiting shape there is', () => {
		// A task sitting unstarted because nobody approved its plan is `queued` *and*
		// asking, and it is the ordinary case rather than the exotic one. Demoted, it
		// reads `Queued · Waiting for a free slot` — the right register and the wrong
		// reason, and the user never learns they are the blocker.
		const v = deriveVerdict({
			...base,
			status: 'queued',
			attention: { source: 'plan_approval', summary: null, raisedAt: 0 },
			now: 4 * 60_000
		});
		expect(v.state).toBe('waiting');
		expect(v.headline).toBe('Waiting on you · 4m');
		expect(v.detail).toBe('Approve the plan before it can run');
	});

	it('reports stalled when progress has stopped but status is still running', () => {
		const v = deriveVerdict({ ...base, lastProgressAt: 0, now: 6 * 60_000 });
		expect(v.state).toBe('stalled');
		expect(v.headline).toBe('Stalled · no progress for 6m');
		// No denominator: `step 4 of 7` frames the step as progress, which is the
		// opposite of what the headline just said.
		expect(v.detail).toBe('Still on step 4: Searching memory for "quarterly plan"');
	});

	it('names the stalled step it cannot number rather than printing a null one', () => {
		// A live step title with no index is a real shape — an unplanned run has
		// one — and the two fields are independently nullable. Interpolating the
		// raw index here renders the literal token `null` at the reader.
		const v = deriveVerdict({
			...base,
			currentStep: null,
			totalSteps: null,
			lastProgressAt: 0,
			now: 6 * 60_000
		});
		expect(v.state).toBe('stalled');
		expect(v.detail).toBe('Still on this step: Searching memory for "quarterly plan"');
		expect(v.detail).not.toContain('null');
	});

	it('falls back to saying nothing advanced when there is no step label at all', () => {
		const v = deriveVerdict({
			...base,
			currentStepLabel: null,
			lastProgressAt: 0,
			now: 6 * 60_000
		});
		expect(v.detail).toBe('The run has not advanced');
	});

	it('reports a stall exactly at the threshold, so copy and threshold cannot drift', () => {
		const v = deriveVerdict({ ...base, lastProgressAt: 0, now: STALL_AFTER_MS });
		expect(v.state).toBe('stalled');
		expect(v.headline).toBe('Stalled · no progress for 5m');
	});

	it('reports running rather than stalled while progress is recent', () => {
		// Every other running case here passes `lastProgressAt: null`, so none of them
		// reaches the stall comparison at all and the threshold is pinned from above
		// only. That was harmless while the field did not exist; `last_progress_at`
		// now ships real timestamps, so `null` has stopped being the shape a live run
		// has, and a run that advanced a minute ago must not be called wedged.
		const v = deriveVerdict({ ...base, lastProgressAt: 0, now: 60_000 });
		expect(v.state).toBe('running');
		expect(v.headline).toBe('Running · step 4 of 7');
		expect(v.detail).toBe('Searching memory for "quarterly plan"');

		// One millisecond under, which pins the boundary itself together with the
		// at-threshold case above — neither `>` nor `>=` can drift unnoticed.
		expect(deriveVerdict({ ...base, lastProgressAt: 0, now: STALL_AFTER_MS - 1 }).state).toBe(
			'running'
		);
	});

	it('carries the error text on failure rather than pointing elsewhere', () => {
		const v = deriveVerdict({
			...base,
			status: 'failed',
			error: "Couldn't read revenue.csv — file not found"
		});
		expect(v.headline).toBe('Failed · at step 4 of 7');
		expect(v.detail).toBe("Couldn't read revenue.csv — file not found");
	});

	it('ranks failure above stalled', () => {
		const v = deriveVerdict({
			...base,
			status: 'failed',
			error: 'boom',
			lastProgressAt: 0,
			now: 6 * 60_000
		});
		expect(v.state).toBe('failed');
	});

	it('distinguishes an instant finish from one with no timing recorded', () => {
		expect(deriveVerdict({ ...base, status: 'finished', elapsedMs: 0 }).headline).toBe(
			'Finished · 0s'
		);
		expect(deriveVerdict({ ...base, status: 'finished', elapsedMs: null }).headline).toBe('Finished');
	});

	it('reports a cancelled run with how long it ran and where you stopped it', () => {
		const v = deriveVerdict({ ...base, status: 'cancelled', elapsedMs: 100_000 });
		expect(v.state).toBe('cancelled');
		expect(v.headline).toBe('Cancelled · after 1m 40s');
		expect(v.detail).toBe('You stopped this at step 4 of 7');

		expect(deriveVerdict({ ...base, status: 'cancelled', elapsedMs: null }).headline).toBe(
			'Cancelled'
		);
	});

	it('reports a queued run as waiting for capacity rather than as started', () => {
		const v = deriveVerdict({ ...base, status: 'queued', queuedFor: 'capacity' });
		expect(v.state).toBe('queued');
		expect(v.headline).toBe('Queued');
		expect(v.detail).toBe('Waiting for a free slot');
	});

	/**
	 * **The four wire statuses that share the `queued` state do not share a
	 * reason**, and the second line said they did. A task whose planner is running
	 * this second, and a monitor asleep until its next fire, both read
	 * `Waiting for a free slot` — not imprecise about either one, false about both.
	 *
	 * One state, four statuses, three sentences (`pending` and `ready` really are
	 * both waiting for a runner). Design §9's candidate 2.
	 */
	it('gives each reason a queued task has not started its own second line', () => {
		const detail = (queuedFor: VerdictInput['queuedFor']): string =>
			deriveVerdict({ ...base, status: 'queued', queuedFor }).detail;

		expect(detail('capacity')).toBe('Waiting for a free slot');
		expect(detail('plan')).toBe('Working out a plan before it starts');
		expect(detail('schedule')).toBe('Scheduled to start later');

		// Distinct, not merely present: a map that answered the same sentence for
		// two reasons would pass three `toBeTruthy` assertions and change nothing.
		const lines = new Set(['capacity', 'plan', 'schedule'].map((reason) => detail(reason as never)));
		expect(lines.size).toBe(3);

		// Every one of them still says the task has not started, and none of them
		// claims progress, finality or an ask.
		for (const line of lines) expect(line.length).toBeGreaterThan(0);
	});

	it('says less rather than something false when nothing named the reason', () => {
		// The state is still `queued` and the row still has a second line — blank is
		// the one detail §3 reserves for `finished`. What it must not do is fall
		// back to the capacity sentence, which is exactly the lie the reasons above
		// exist to remove, restored the first time a caller forgets the field.
		const v = deriveVerdict({ ...base, status: 'queued', queuedFor: null });
		expect(v.state).toBe('queued');
		expect(v.detail).toBe('Waiting to start');
		expect(v.detail).not.toBe('Waiting for a free slot');
	});

	it('keeps a deliberate pause distinct from queueing or a runtime ask', () => {
		const v = deriveVerdict({ ...base, status: 'paused' });
		expect(v).toEqual({
			state: 'paused',
			headline: 'Paused',
			detail: 'Ready to resume when you are'
		});
	});

	it('reports archived work as inactive history rather than queued or finished', () => {
		const v = deriveVerdict({ ...base, status: 'archived' });
		expect(v).toEqual({
			state: 'archived',
			headline: 'Archived',
			detail: 'No longer active'
		});
	});

	it('leaves the finished second line empty for the output summary to fill', () => {
		const v = deriveVerdict({ ...base, status: 'finished', elapsedMs: 192_000 });
		expect(v.state).toBe('finished');
		expect(v.headline).toBe('Finished · 3m 12s');
		// Deferred to Task 3, which fills this with the output summary. Pinned in
		// both directions so filling it in has to be a deliberate test change.
		expect(v.detail).toBe('');
	});

	it('drops the step clause entirely rather than trailing a separator with nothing after it', () => {
		// Every other headline case here carries a step, so the bare forms — the ones
		// an unplanned run actually renders — went unasserted and could have been
		// rewritten or lost without a failure.
		const noStep = { ...base, currentStep: null, totalSteps: null };
		expect(deriveVerdict({ ...noStep, status: 'running' }).headline).toBe('Running');
		expect(deriveVerdict({ ...noStep, status: 'failed', error: 'boom' }).headline).toBe('Failed');
	});

	it('has words for every second line it can be missing the words for', () => {
		// Three fallbacks nobody would notice being deleted: each one only renders
		// when the field it would have quoted is absent, which no other case here
		// arranges. (`The run has not advanced` is pinned by the stalled case above.)
		expect(deriveVerdict({ ...base, currentStepLabel: null }).detail).toBe('Working');
		expect(deriveVerdict({ ...base, status: 'failed', error: null }).detail).toBe(
			'No error message was recorded'
		);
		expect(
			deriveVerdict({ ...base, status: 'cancelled', currentStep: null, totalSteps: null }).detail
		).toBe('You stopped this');
	});

	it('falls through to finished for a status it does not model', () => {
		// Every other case here names a status the chain checks, so all of them
		// would still pass if the last branch grew an `if` and a throwing default
		// — silently deleting a documented decision the corpus eval depends on.
		const v = deriveVerdict({ ...base, status: 'future_status', elapsedMs: 192_000 });
		expect(v.state).toBe('finished');
		expect(v.headline).toBe('Finished · 3m 12s');
	});
});
