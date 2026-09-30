import { browser } from '$app/environment';
import { pushState, replaceState } from '$app/navigation';
import { page } from '$app/stores';
import { get, writable, type Readable } from 'svelte/store';

import { attentionRowAliases, type AttentionDisplayRow } from './model';

export const ATTENTION_CENTER_QUERY_KEY = 'attention';
export const ATTENTION_ITEM_QUERY_KEY = 'attention_item';
export const ATTENTION_HISTORY_STATE_KEY = '__magician_attention_entry';

export interface AttentionHistoryMarker {
	version: 1;
	kind: 'center' | 'item';
	itemParent?: 'center' | 'closed';
}

export type AttentionCloseHistoryPlan =
	| { kind: 'back'; delta: -1 }
	| { kind: 'go'; delta: -2 }
	| { kind: 'replace'; url: URL };

export interface AttentionCenterState {
	/** Center is "active" (mounted/session running) — true when the list is
	 *  requested OR an item is selected. Drives the machinery, not the list UI. */
	open: boolean;
	itemId: string | null;
	/** The LIST was explicitly requested (`attention=1`). Drives whether the
	 *  center's list modal renders. A direct single-item open sets `itemId`
	 *  WITHOUT this, so the item resolves + its prompt opens with no list behind. */
	listRequested: boolean;
}

export interface AttentionUrlUpdate {
	open: boolean;
	itemId?: string | null;
}

export function readAttentionCenterUrl(url: URL): AttentionCenterState {
	const itemId = url.searchParams.get(ATTENTION_ITEM_QUERY_KEY)?.trim() || null;
	const listRequested = url.searchParams.get(ATTENTION_CENTER_QUERY_KEY) === '1';
	return {
		open: listRequested || itemId !== null,
		itemId,
		listRequested
	};
}

/** Return a clone with only Attention-owned query keys changed. */
export function updateAttentionCenterUrl(url: URL, update: AttentionUrlUpdate): URL {
	const next = new URL(url);
	if (update.open) {
		next.searchParams.set(ATTENTION_CENTER_QUERY_KEY, '1');
	} else {
		next.searchParams.delete(ATTENTION_CENTER_QUERY_KEY);
	}

	// Item is independent of the list: a direct single-item open sets the item
	// param WITHOUT `attention=1`, so the item resolves with no list rendered.
	const itemId = update.itemId?.trim();
	if (itemId) {
		next.searchParams.set(ATTENTION_ITEM_QUERY_KEY, itemId);
	} else {
		next.searchParams.delete(ATTENTION_ITEM_QUERY_KEY);
	}
	return next;
}

export function readAttentionHistoryMarker(state: unknown): AttentionHistoryMarker | null {
	if (!state || typeof state !== 'object') return null;
	const marker = (state as Record<string, unknown>)[ATTENTION_HISTORY_STATE_KEY];
	if (!marker || typeof marker !== 'object') return null;
	const candidate = marker as Partial<AttentionHistoryMarker>;
	if (candidate.version !== 1 || (candidate.kind !== 'center' && candidate.kind !== 'item')) {
		return null;
	}
	if (
		candidate.kind === 'item' &&
		candidate.itemParent !== 'center' &&
		candidate.itemParent !== 'closed'
	) {
		return null;
	}
	return candidate as AttentionHistoryMarker;
}

export function planAttentionCenterClose(url: URL, state: unknown): AttentionCloseHistoryPlan {
	const marker = readAttentionHistoryMarker(state);
	if (marker?.kind === 'item' && marker.itemParent === 'center') {
		return { kind: 'go', delta: -2 };
	}
	if (marker) return { kind: 'back', delta: -1 };
	return { kind: 'replace', url: updateAttentionCenterUrl(url, { open: false }) };
}

const attentionCenterWritable = writable<AttentionCenterState>(
	{ open: false, itemId: null, listRequested: false },
	(set) => {
		const unsubscribePage = page.subscribe(($page) => {
			const url = browser ? new URL(window.location.href) : $page.url;
			set(readAttentionCenterUrl(url));
		});
		if (!browser) return unsubscribePage;

		const syncFromLocation = () => {
			set(readAttentionCenterUrl(new URL(window.location.href)));
		};
		window.addEventListener('popstate', syncFromLocation);
		return () => {
			unsubscribePage();
			window.removeEventListener('popstate', syncFromLocation);
		};
	}
);

export const attentionCenterState: Readable<AttentionCenterState> = {
	subscribe: attentionCenterWritable.subscribe
};

/** Publish a shallow URL mutation that `$app/stores.page.url` does not observe. */
export function syncAttentionCenterUrl(url: URL): void {
	attentionCenterWritable.set(readAttentionCenterUrl(url));
}

function currentUrl(): URL | null {
	return browser ? new URL(window.location.href) : null;
}

function currentPageState(): Record<string, unknown> {
	const state = get(page).state;
	return state && typeof state === 'object' ? { ...(state as Record<string, unknown>) } : {};
}

function stateWithAttentionMarker(
	state: Record<string, unknown>,
	marker: AttentionHistoryMarker | null | 'preserve'
): Record<string, unknown> {
	const next = { ...state };
	if (marker === 'preserve') return next;
	if (marker) next[ATTENTION_HISTORY_STATE_KEY] = marker;
	else delete next[ATTENTION_HISTORY_STATE_KEY];
	return next;
}

function commitAttentionUrl(
	update: AttentionUrlUpdate,
	history: 'push' | 'replace' = 'push',
	marker: AttentionHistoryMarker | null | 'preserve' = 'preserve'
): void {
	const url = currentUrl();
	if (!url) return;
	const next = updateAttentionCenterUrl(url, update);
	if (next.href === url.href) return;
	const state = stateWithAttentionMarker(currentPageState(), marker);
	if (history === 'replace') {
		replaceState(next, state);
	} else {
		pushState(next, state);
	}
	syncAttentionCenterUrl(next);
}

export function openAttentionCenter(): void {
	commitAttentionUrl(
		{ open: true },
		'push',
		{ version: 1, kind: 'center' }
	);
}

export function closeAttentionCenter(): void {
	const url = currentUrl();
	if (!url) return;
	const plan = planAttentionCenterClose(url, currentPageState());
	if (plan.kind === 'back') {
		window.history.back();
	} else if (plan.kind === 'go') {
		window.history.go(plan.delta);
	} else {
		const state = stateWithAttentionMarker(currentPageState(), null);
		replaceState(plan.url, state);
		syncAttentionCenterUrl(plan.url);
	}
}

export function toggleAttentionCenter(): void {
	const url = currentUrl();
	if (!url) return;
	const state = readAttentionCenterUrl(url);
	if (state.open) closeAttentionCenter();
	else openAttentionCenter();
}

/**
 * Clear any Attention selection (list AND item) via a history REPLACE, without
 * navigating. Recovery path for a DIRECT single-item open that could not resolve
 * (item not found / load error): the center grabbed overlay focus + froze the
 * body on open, but with no `attention=1` it renders no visible list, so a failed
 * resolve would otherwise strand the user behind an invisible, click-blocking
 * frozen layer. Clearing the params flips `open` false → the center releases its
 * overlay and unfreezes the body. Uses replace (not history.back) so it recovers
 * even when the URL was loaded directly with no prior history entry to pop.
 */
export function clearAttentionSelection(): void {
	const url = currentUrl();
	if (!url) return;
	const next = updateAttentionCenterUrl(url, { open: false, itemId: null });
	if (next.href === url.href) return;
	const state = stateWithAttentionMarker(currentPageState(), null);
	replaceState(next, state);
	syncAttentionCenterUrl(next);
}

/** Public launcher for any canonical or alias Attention item id. */
export function openAttentionItem(itemId: string): void {
	const normalized = itemId.trim();
	if (!normalized) return;
	const url = currentUrl();
	if (!url) return;
	const center = readAttentionCenterUrl(url);
	const currentMarker = readAttentionHistoryMarker(currentPageState());
	const itemParent = center.open && !center.itemId ? 'center' : 'closed';
	const marker: AttentionHistoryMarker = {
		version: 1,
		kind: 'item',
		itemParent:
			center.itemId && currentMarker?.kind === 'item'
				? currentMarker.itemParent
				: itemParent
	};
	commitAttentionUrl(
		// Only open the LIST when the item is opened from within the center
		// ('center'). A direct open ('closed') sets the item alone, so no list
		// renders behind it and closing it returns to the launching page.
		{ open: marker.itemParent === 'center', itemId: normalized },
		center.itemId ? 'replace' : 'push',
		marker
	);
}

/**
 * Open Attention for a set of item ids: when exactly ONE valid id is present,
 * open that single item directly; otherwise fall back to the center list.
 * Use this for any "N item(s) waiting" affordance so a single item lands the
 * user on the item, not the list.
 */
export function openAttentionForItems(ids: Array<string | null | undefined>): void {
	const valid = ids
		.map((id) => (typeof id === 'string' ? id.trim() : ''))
		.filter((id) => id.length > 0);
	if (valid.length === 1) openAttentionItem(valid[0]);
	else openAttentionCenter();
}

/** Keep the center open while removing a handled/stale item selection. */
export function returnToAttentionCenter(
	history: 'auto' | 'push' | 'replace' = 'auto'
): void {
	const marker = readAttentionHistoryMarker(currentPageState());
	// Both a from-list item ('center') and a direct single-item open ('closed')
	// pushed a history entry, so going back returns the user to where they were —
	// the list, or (for a direct open) the page they launched from — instead of
	// re-opening the list on top of a handled direct item.
	if (
		history === 'auto' &&
		marker?.kind === 'item' &&
		(marker.itemParent === 'center' || marker.itemParent === 'closed')
	) {
		window.history.back();
		return;
	}
	const nextMarker = history === 'auto' ? null : 'preserve';
	commitAttentionUrl({ open: true }, history === 'push' ? 'push' : 'replace', nextMarker);
}

export function findAttentionRowByAlias(
	rows: AttentionDisplayRow[],
	itemId: string | null | undefined
): AttentionDisplayRow | null {
	const normalized = itemId?.trim();
	if (!normalized) return null;
	return rows.find((row) => attentionRowAliases(row).has(normalized)) ?? null;
}
