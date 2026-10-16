import { openAttentionCenter, openAttentionItem } from './centerState';

const ATTENTION_ROUTE_ITEM_KEYS = [
	'attention_item',
	'approval_id',
	'correlation_id'
] as const;

export interface AttentionRouteIntent {
	itemId: string | null;
}

export function parseAttentionRouteIntent(route: string | null | undefined): AttentionRouteIntent | null {
	const normalized = route?.trim();
	if (!normalized) return null;
	if (!normalized.startsWith('/')) return null;
	try {
		const url = new URL(normalized, 'http://magician.local');
		const pathname = url.pathname.replace(/\/+$/, '');
		if (pathname !== '/attention' && pathname !== '/approvals') return null;
		for (const key of ATTENTION_ROUTE_ITEM_KEYS) {
			const itemId = url.searchParams.get(key)?.trim();
			if (itemId) return { itemId };
		}
		return { itemId: null };
	} catch {
		return null;
	}
}

/** Open an Attention route as a global overlay without leaving the current page. */
export function openAttentionRoute(
	route: string | null | undefined,
	fallbackItemId?: string | null
): boolean {
	const intent = parseAttentionRouteIntent(route);
	if (!intent) return false;
	const itemId = intent.itemId ?? fallbackItemId?.trim() ?? null;
	if (itemId) openAttentionItem(itemId);
	else openAttentionCenter();
	return true;
}

/**
 * Open an item a feed already resolved, without leaving the current page.
 *
 * An Attention URL wins. Otherwise the item id opens that item. Only an item
 * with neither falls back to the Attention list. Morning Edition's lead uses
 * this so feed-id resolution stays in the launcher the open-path contract owns.
 */
export function openResolvedAttention(
	route: string | null | undefined,
	itemId: string | null | undefined
): void {
	if (openAttentionRoute(route, itemId)) return;
	if (itemId) openAttentionItem(itemId);
	else openAttentionCenter();
}
