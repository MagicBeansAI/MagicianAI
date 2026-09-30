import { describe, it, expect } from 'vitest';
import { applyEvent } from './notifyStream';
import type { NotifyCard } from './cardModel';

/**
 * Narrow a `NotifyCard` to its `actionable` variant so tests can read
 * actionable-only fields (`correlationId`, `prompt`) without `svelte-check`
 * flagging the union access. Throws if a non-actionable card slips through,
 * which would itself be a meaningful test failure.
 */
function actionable(card: NotifyCard): Extract<NotifyCard, { kind: 'actionable' }> {
	if (card.kind !== 'actionable') throw new Error(`expected actionable card, got ${card.kind}`);
	return card;
}

function requested(id: string, extra: Record<string, unknown> = {}) {
	return {
		event_type: 'HitlRequested',
		data: { correlation_id: id, source: 'approval', input_type: 'confirmation', prompt: `Q ${id}`, ...extra }
	};
}

function resolved(id: string, outcome = 'responded') {
	return { event_type: 'HitlResolved', data: { correlation_id: id, outcome } };
}

describe('applyEvent — dedup + dismiss', () => {
	it('adds a card on HitlRequested', () => {
		const out = applyEvent([], requested('c1'));
		expect(out.map((c) => actionable(c).correlationId)).toEqual(['c1']);
		expect(out[0].kind).toBe('actionable');
	});

	it('does not duplicate a second identical HitlRequested', () => {
		const once = applyEvent([], requested('c1'));
		const twice = applyEvent(once, requested('c1'));
		expect(twice.map((c) => actionable(c).correlationId)).toEqual(['c1']);
	});

	it('replaces in place (latest payload wins) without reordering', () => {
		let cards: NotifyCard[] = applyEvent([], requested('a'));
		cards = applyEvent(cards, requested('b'));
		cards = applyEvent(cards, requested('a', { prompt: 'updated' }));
		expect(cards.map((c) => actionable(c).correlationId)).toEqual(['a', 'b']);
		const updated = cards.find((c) => c.kind === 'actionable' && c.correlationId === 'a');
		expect(updated ? actionable(updated).prompt : undefined).toBe('updated');
	});

	it('removes the card on HitlResolved (responded)', () => {
		const added = applyEvent([], requested('c1'));
		const removed = applyEvent(added, resolved('c1', 'responded'));
		expect(removed).toEqual([]);
	});

	it('removes the card on HitlResolved with outcome expired', () => {
		const added = applyEvent([], requested('c1'));
		const removed = applyEvent(added, resolved('c1', 'expired'));
		expect(removed).toEqual([]);
	});

	it('dismisses regardless of outcome (cancelled / dismissed)', () => {
		for (const outcome of ['cancelled', 'dismissed']) {
			const added = applyEvent([], requested('c1'));
			expect(applyEvent(added, resolved('c1', outcome))).toEqual([]);
		}
	});

	it('ignores a HitlResolved for an unknown correlation_id', () => {
		const added = applyEvent([], requested('c1'));
		const same = applyEvent(added, resolved('other'));
		expect(same).toBe(added); // unchanged reference — no card matched
	});

	it('ignores unrelated / malformed events', () => {
		const added = applyEvent([], requested('c1'));
		expect(applyEvent(added, { event_type: 'SomethingElse', data: {} })).toBe(added);
		expect(applyEvent(added, { event_type: 'HitlRequested', data: {} })).toBe(added);
	});
});

describe('applyEvent — backfill + reentrancy (T8)', () => {
	it('produces a card from a backfilled HitlRequested delivered first', () => {
		// The V3 /events backfill replays prior events on connect through the
		// same ingest, so the first event a freshly-connected overlay sees can
		// be a still-open HitlRequested. It must materialize a card.
		const out = applyEvent([], requested('backfilled'));
		expect(out.map((c) => actionable(c).correlationId)).toEqual(['backfilled']);
	});

	it('is order-independent: request→resolve and replay converge', () => {
		// Drive the full backfill in order.
		let a: NotifyCard[] = [];
		a = applyEvent(a, requested('x'));
		a = applyEvent(a, requested('y'));
		a = applyEvent(a, resolved('x'));
		expect(a.map((c) => actionable(c).correlationId)).toEqual(['y']);

		// Replaying the SAME backlog from empty (reconnect re-reads backfill)
		// converges on the identical surviving set — ingest is idempotent.
		let b: NotifyCard[] = [];
		for (const ev of [requested('x'), requested('y'), resolved('x')]) {
			b = applyEvent(b, ev);
		}
		expect(b.map((c) => actionable(c).correlationId)).toEqual(
			a.map((c) => actionable(c).correlationId)
		);
	});

	it('is reentrant: replaying an already-resolved pair leaves no card', () => {
		let cards: NotifyCard[] = [];
		for (const ev of [requested('z'), resolved('z'), requested('z'), resolved('z')]) {
			cards = applyEvent(cards, ev);
		}
		expect(cards).toEqual([]);
	});
});

describe('applyEvent — informational routing + coalesce', () => {
	const completed = (executionId: string) => ({
		event_type: 'ExecutionCompleted',
		data: { execution_id: executionId, success: true, steps_total: 2 }
	});
	const failed = (executionId: string, error: string) => ({
		event_type: 'ExecutionFailed',
		data: { execution_id: executionId, error }
	});

	it('produces a success card from an ExecutionCompleted (success:true)', () => {
		const out = applyEvent([], completed('e1'));
		expect(out).toEqual([
			{
				id: 'exec-done-e1',
				kind: 'success',
				title: 'Completed',
				deepLink: 'e1',
				dismissAfterMs: 5000
			}
		]);
	});

	it('coalesces a repeated completion into one card (in place)', () => {
		let cards: NotifyCard[] = applyEvent([], completed('e1'));
		cards = applyEvent(cards, completed('e1'));
		expect(cards).toHaveLength(1);
		expect(cards[0].id).toBe('exec-done-e1');
	});

	it('coalesces repeated failures for the same execution into one error card', () => {
		let cards: NotifyCard[] = applyEvent([], failed('e2', 'first'));
		cards = applyEvent(cards, failed('e2', 'second'));
		expect(cards).toHaveLength(1);
		const card = cards[0];
		expect(card.kind === 'error' ? card.message : undefined).toBe('second');
	});

	it('keeps two different executions as two info cards', () => {
		let cards: NotifyCard[] = applyEvent([], failed('e3', 'a'));
		cards = applyEvent(cards, failed('e4', 'b'));
		expect(cards.map((c) => c.id)).toEqual(['exec-fail-e3', 'exec-fail-e4']);
	});

	it('keeps an actionable HITL and an info card with coincident ids separate', () => {
		// A HitlRequested whose correlation_id equals a failed execution_id must
		// not be coalesced away by the error card (kind-prefixed coalesce keys).
		let cards: NotifyCard[] = applyEvent([], requested('e5'));
		cards = applyEvent(cards, failed('e5', 'boom'));
		expect(cards).toHaveLength(2);
		expect(cards.map((c) => c.kind).sort()).toEqual(['actionable', 'error']);
	});

	it('does not surface a failed ExecutionCompleted (ExecutionFailed owns failures)', () => {
		const out = applyEvent([], {
			event_type: 'ExecutionCompleted',
			data: { execution_id: 'e6', success: false }
		});
		expect(out).toEqual([]);
	});

	it('leaves an info card untouched on an unrelated HitlResolved', () => {
		const cards = applyEvent([], completed('e7'));
		const same = applyEvent(cards, resolved('not-a-card'));
		expect(same).toBe(cards); // same reference — no actionable card matched
	});
});
