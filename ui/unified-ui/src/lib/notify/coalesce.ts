/**
 * Notify-overlay COALESCE keys — pure, no I/O.
 *
 * A single key per logical notification so a chatty source (an execution that
 * re-emits completion / failure events, or the V3 backfill replaying prior
 * events on reconnect) shows ONE updating card instead of a duplicate stack.
 * `notifyStream.applyEvent` uses `coalesceKey` for add-or-replace: a new card
 * whose key matches an existing card REPLACES it in place (no reorder, no
 * duplicate, latest payload wins).
 *
 * Key derivation:
 *   - actionable HITL → its `correlationId`. This preserves the existing
 *     dedup-by-correlationId behavior verbatim — the HitlResolved removal in
 *     `applyEvent` still matches on `correlationId` directly.
 *   - info / success / error → `${kind}:${deepLink ?? id}`.
 *
 * The `kind:` prefix on the informational keys is load-bearing: it guarantees
 * an informational card can never collide with an actionable card whose
 * `correlationId` happens to equal an informational card's `deepLink`/`id`
 * (e.g. an execution_id reused as a correlation_id). Actionable keys are the
 * bare correlationId (no prefix), so the two key spaces are disjoint as long
 * as no correlationId starts with `actionable:`/`info:`/`success:`/`error:` —
 * and even then the prefix difference keeps them apart.
 *
 * Note the success and error key spaces are intentionally distinct (`success:`
 * vs `error:`) so a later `ExecutionCompleted{success:true}` does NOT overwrite
 * an earlier `ExecutionFailed` card for the same execution_id, and vice versa —
 * the two carry different `kind`s and should both be expressible. Within a
 * single kind, repeated events for the same `deepLink` collapse (a re-emitted
 * failure updates the one error card).
 */

import type { NotifyCard } from './cardModel';

/**
 * The dedup/replace key for a card. Two cards with the same key are the same
 * logical notification — adding the second replaces the first in place.
 */
export function coalesceKey(card: NotifyCard): string {
	if (card.kind === 'actionable') {
		return card.correlationId;
	}
	return `${card.kind}:${card.deepLink ?? card.id}`;
}

/**
 * Add `card` to `current`, or replace an existing card with the same
 * `coalesceKey` in place. Pure: returns a new array only when the contents
 * change; never reorders survivors.
 *
 * Shared by `notifyStream.applyEvent` so the live stream and the unit tests
 * exercise one add-or-replace path. When the incoming card is byte-identical
 * to the existing one this still returns a fresh array (cheap; the reducer's
 * own no-op guards live upstream where a same-reference result matters).
 */
export function coalesceCard(current: NotifyCard[], card: NotifyCard): NotifyCard[] {
	const key = coalesceKey(card);
	const index = current.findIndex((existing) => coalesceKey(existing) === key);
	if (index === -1) {
		return [...current, card];
	}
	const next = current.slice();
	next[index] = card;
	return next;
}
