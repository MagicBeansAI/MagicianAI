/**
 * Time Loom — pure helpers for the Live Thinking Map replay UI.
 *
 * Everything here is deterministic and side-effect free so it can be
 * unit-tested without a DOM or network (see `timeLoom.test.ts`). The
 * `TimeLoom.svelte` panel owns all fetching/state; this module owns:
 *
 *   - `categorizeEvent`   — which jump categories a `MapEvent` belongs to
 *     (utterance / correction / decision / clarification / promotion);
 *   - `jumpTargetIndex`   — the next/previous event index for a category,
 *     relative to the current scrub position (`null` = "now");
 *   - `diffMaps`          — then/now node-id comparison (added / removed /
 *     changed) between a replayed historical map and the current map.
 */

import type {
	MapEvent,
	ThinkingMap,
	ThinkingNode
} from '$lib/types/thinkingMap';

// ── Event categorization ─────────────────────────────────────────────────────

/** The five jumpable marker categories on the Time Loom timeline. */
export type EventCategory =
	| 'utterance'
	| 'correction'
	| 'decision'
	| 'clarification'
	| 'promotion';

/** Stable display/order list of every category (also the marker z-order). */
export const EVENT_CATEGORIES: readonly EventCategory[] = [
	'utterance',
	'correction',
	'decision',
	'clarification',
	'promotion'
] as const;

/**
 * Categorize one event-log entry. An event can belong to SEVERAL categories
 * (e.g. a spoken correction is both `utterance` and `correction`); the result
 * preserves `EVENT_CATEGORIES` order and never contains duplicates.
 *
 *   - `utterance`     — the envelope carries a non-empty `utterance_id`
 *     (i.e. it was folded in from a spoken/typed utterance);
 *   - `correction`    — any `update_node` or `set_epistemic_state` op
 *     (label/detail/confidence edits, confirm/contradict/supersede…);
 *   - `decision`      — an `add_node` whose node kind is `decision`, or a
 *     `set_node_kind` that turns a node INTO a decision;
 *   - `clarification` — `create_clarification` / `resolve_clarification`;
 *   - `promotion`     — `link_promoted_object` (node → task/memory link).
 *
 * Only top-level envelope operations count: a `propose_restructure`'s NESTED
 * operations are staged, not applied, so they don't mark the timeline (the
 * confirm event's own ops do, when the proposal lands).
 */
export function categorizeEvent(event: MapEvent): EventCategory[] {
	const found = new Set<EventCategory>();
	if ((event.envelope.utterance_id ?? '').trim() !== '') {
		found.add('utterance');
	}
	for (const op of event.envelope.operations) {
		switch (op.op) {
			case 'update_node':
			case 'set_epistemic_state':
				found.add('correction');
				break;
			case 'add_node':
				if (op.node.kind === 'decision') found.add('decision');
				break;
			case 'set_node_kind':
				if (op.kind === 'decision') found.add('decision');
				break;
			case 'create_clarification':
			case 'resolve_clarification':
				found.add('clarification');
				break;
			case 'link_promoted_object':
				found.add('promotion');
				break;
			default:
				break;
		}
	}
	return EVENT_CATEGORIES.filter((c) => found.has(c));
}

// ── Category jumps ───────────────────────────────────────────────────────────

/**
 * Find the index of the nearest event in `category`, scanning strictly
 * before (`direction = -1`) or after (`direction = 1`) the current position.
 *
 * `fromIndex = null` means the scrubber is at "now". Since the map's live
 * state equals the LAST event's replayed state, "now" is treated as index
 * `events.length - 1` — so jumping back from now lands on the most recent
 * matching event that actually shows something older than the present.
 *
 * Returns the target index, or `null` when no matching event exists in that
 * direction (the caller leaves the scrubber where it is).
 */
export function jumpTargetIndex(
	events: readonly MapEvent[],
	fromIndex: number | null,
	category: EventCategory,
	direction: 1 | -1
): number | null {
	if (events.length === 0) return null;
	const from = fromIndex === null ? events.length - 1 : fromIndex;
	if (direction === -1) {
		for (let i = Math.min(from, events.length) - 1; i >= 0; i--) {
			if (categorizeEvent(events[i]).includes(category)) return i;
		}
	} else {
		for (let i = Math.max(from, -1) + 1; i < events.length; i++) {
			if (categorizeEvent(events[i]).includes(category)) return i;
		}
	}
	return null;
}

// ── Then/now comparison ──────────────────────────────────────────────────────

/** Node-id sets describing how `now` differs from the replayed `then`. */
export interface MapDiff {
	/** Live in `now`, absent (or tombstoned) in `then` — added since. */
	added: string[];
	/** Live in `then`, absent (or tombstoned) in `now` — removed since. */
	removed: string[];
	/** Live in both but semantically different — changed since. */
	changed: string[];
}

/** The live (non-tombstoned) nodes of a map, keyed by id. */
function liveNodes(map: ThinkingMap): globalThis.Map<string, ThinkingNode> {
	const out = new globalThis.Map<string, ThinkingNode>();
	for (const node of Object.values(map.nodes)) {
		if (!node.tombstoned) out.set(node.node_id, node);
	}
	return out;
}

/**
 * A node's SEMANTIC signature — the fields whose change means the thought
 * itself changed. Layout (`position`, `position_locked`), bookkeeping
 * timestamps, and speaker display metadata are deliberately excluded so a
 * drag or re-render never counts as a "change".
 */
function nodeSignature(node: ThinkingNode): string {
	return JSON.stringify([
		node.kind,
		node.label,
		node.detail_markdown ?? null,
		node.epistemic_state,
		node.confidence,
		node.parent_id ?? null,
		node.promoted_refs.map((r) => `${r.destination_kind}:${r.object_id}`).sort()
	]);
}

/**
 * Compare a replayed historical map (`then`) against the current map (`now`).
 * Tombstoned nodes count as ABSENT on both sides (matching the canvas, which
 * filters them out), so tombstoning a node between then and now reports it as
 * `removed`, and restoring one reports it as `added`. All three id lists are
 * lexicographically sorted for deterministic rendering/tests.
 */
export function diffMaps(then: ThinkingMap, now: ThinkingMap): MapDiff {
	const thenNodes = liveNodes(then);
	const nowNodes = liveNodes(now);
	const added: string[] = [];
	const removed: string[] = [];
	const changed: string[] = [];
	for (const [id, nowNode] of nowNodes) {
		const thenNode = thenNodes.get(id);
		if (!thenNode) added.push(id);
		else if (nodeSignature(thenNode) !== nodeSignature(nowNode)) changed.push(id);
	}
	for (const id of thenNodes.keys()) {
		if (!nowNodes.has(id)) removed.push(id);
	}
	added.sort();
	removed.sort();
	changed.sort();
	return { added, removed, changed };
}

// ── Shared view types ────────────────────────────────────────────────────────

/**
 * What the Time Loom tells its host page while scrubbing: the sequence and
 * revision being viewed, plus the replayed map once it has been fetched
 * (`map: null` while the debounced replay request is in flight). The panel
 * emits `null` when the user returns to "now".
 */
export interface HistoryView {
	seq: number;
	revision: number;
	map: ThinkingMap | null;
}
