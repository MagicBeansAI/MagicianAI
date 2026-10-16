import { describe, it, expect } from 'vitest';
import { coalesceKey, coalesceCard } from './coalesce';
import type { NotifyCard } from './cardModel';

function actionable(correlationId: string): NotifyCard {
	return {
		id: correlationId,
		kind: 'actionable',
		correlationId,
		source: 'approval',
		inputType: 'confirmation',
		prompt: `Q ${correlationId}`
	};
}

function errorCard(execId: string, message: string): NotifyCard {
	return { id: `exec-fail-${execId}`, kind: 'error', title: 'Execution failed', message, deepLink: execId };
}

describe('coalesceKey', () => {
	it('keys an actionable card on its correlationId (no prefix)', () => {
		expect(coalesceKey(actionable('c1'))).toBe('c1');
	});

	it('keys an info/success/error card as `${kind}:${deepLink ?? id}`', () => {
		expect(coalesceKey(errorCard('e1', 'boom'))).toBe('error:e1');
		expect(
			coalesceKey({ id: 'exec-done-e1', kind: 'success', title: 'Completed', deepLink: 'e1' })
		).toBe('success:e1');
		// Falls back to id when there is no deepLink.
		expect(coalesceKey({ id: 'standalone', kind: 'info', title: 'x' })).toBe('info:standalone');
	});

	it('keeps actionable and info key spaces disjoint even on coincident ids', () => {
		// An actionable correlationId that equals an info card's deepLink does
		// not collide — the info key is kind-prefixed.
		expect(coalesceKey(actionable('e1'))).toBe('e1');
		expect(coalesceKey(errorCard('e1', 'boom'))).toBe('error:e1');
		expect(coalesceKey(actionable('e1'))).not.toBe(coalesceKey(errorCard('e1', 'boom')));
	});
});

describe('coalesceCard', () => {
	it('appends a new card when no key matches', () => {
		const out = coalesceCard([], errorCard('e1', 'first'));
		expect(out.map((c) => c.id)).toEqual(['exec-fail-e1']);
	});

	it('replaces a same-key card in place (latest payload wins, no reorder)', () => {
		let cards: NotifyCard[] = [];
		cards = coalesceCard(cards, errorCard('e1', 'first'));
		cards = coalesceCard(cards, errorCard('e2', 'other'));
		cards = coalesceCard(cards, errorCard('e1', 'updated'));
		expect(cards.map((c) => c.id)).toEqual(['exec-fail-e1', 'exec-fail-e2']);
		const updated = cards.find((c) => c.id === 'exec-fail-e1');
		expect(updated && 'message' in updated ? updated.message : undefined).toBe('updated');
	});

	it('keeps two different executions as two cards', () => {
		let cards: NotifyCard[] = [];
		cards = coalesceCard(cards, errorCard('e1', 'a'));
		cards = coalesceCard(cards, errorCard('e2', 'b'));
		expect(cards).toHaveLength(2);
	});

	it('does not collide an actionable and an info card with coincident ids', () => {
		let cards: NotifyCard[] = [];
		cards = coalesceCard(cards, actionable('e1'));
		cards = coalesceCard(cards, errorCard('e1', 'boom'));
		// Both survive — different coalesce keys (`e1` vs `error:e1`).
		expect(cards).toHaveLength(2);
		expect(cards.map((c) => c.kind).sort()).toEqual(['actionable', 'error']);
	});

	it('keeps success and error for the same execution distinct', () => {
		let cards: NotifyCard[] = [];
		cards = coalesceCard(cards, errorCard('e1', 'boom'));
		cards = coalesceCard(cards, {
			id: 'exec-done-e1',
			kind: 'success',
			title: 'Completed',
			deepLink: 'e1'
		});
		expect(cards).toHaveLength(2);
	});
});
