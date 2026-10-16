import { describe, expect, it } from 'vitest';

import type { AttentionDisplayRow } from './model';
import {
	ATTENTION_HISTORY_STATE_KEY,
	findAttentionRowByAlias,
	planAttentionCenterClose,
	readAttentionHistoryMarker,
	readAttentionCenterUrl,
	updateAttentionCenterUrl
} from './centerState';

function row(overrides: Partial<AttentionDisplayRow> = {}): AttentionDisplayRow {
	return {
		key: 'pause-1',
		source: 'agentic',
		prompt: 'Need input',
		scope: {},
		at: 1,
		correlation_id: 'correlation-1',
		alias_ids: ['approval-1'],
		origin: 'feed',
		request: null,
		feed_item_id: 'v3:attention:raw-feed-id',
		...overrides
	};
}

describe('Attention center URL ownership', () => {
	it('preserves the current path, unrelated query values, and hash', () => {
		const current = new URL('https://magician.test/square?cycle=7&panel=crew#citizen-4');
		const opened = updateAttentionCenterUrl(current, {
			open: true,
			itemId: 'pause/id with spaces'
		});

		expect(opened.pathname).toBe('/square');
		expect(opened.searchParams.get('cycle')).toBe('7');
		expect(opened.searchParams.get('panel')).toBe('crew');
		expect(opened.searchParams.get('attention')).toBe('1');
		expect(opened.searchParams.get('attention_item')).toBe('pause/id with spaces');
		expect(opened.hash).toBe('#citizen-4');
		expect(readAttentionCenterUrl(opened)).toEqual({
			open: true,
			itemId: 'pause/id with spaces',
			listRequested: true
		});
	});

	it('distinguishes a direct item prompt from a requested list modal', () => {
		const direct = new URL('https://magician.test/tasks?attention_item=pause-1');
		const listed = new URL(
			'https://magician.test/tasks?attention=1&attention_item=pause-1'
		);

		expect(readAttentionCenterUrl(direct)).toEqual({
			open: true,
			itemId: 'pause-1',
			listRequested: false
		});
		expect(readAttentionCenterUrl(listed)).toEqual({
			open: true,
			itemId: 'pause-1',
			listRequested: true
		});
	});

	it('can select an item without opening the list behind it', () => {
		const current = new URL('https://magician.test/square?panel=crew');
		const direct = updateAttentionCenterUrl(current, {
			open: false,
			itemId: ' pause-1 '
		});

		expect(direct.searchParams.has('attention')).toBe(false);
		expect(direct.searchParams.get('attention_item')).toBe('pause-1');
		expect(readAttentionCenterUrl(direct).listRequested).toBe(false);
	});

	it('closing removes only Attention-owned query keys', () => {
		const current = new URL(
			'https://magician.test/tasks?compose=1&attention=1&attention_item=pause-1#queue'
		);
		const closed = updateAttentionCenterUrl(current, { open: false });

		expect(closed.searchParams.get('compose')).toBe('1');
		expect(closed.searchParams.has('attention')).toBe(false);
		expect(closed.searchParams.has('attention_item')).toBe(false);
		expect(closed.hash).toBe('#queue');
	});

	it('backs out of a center entry created by Attention instead of pushing closed state', () => {
		const current = new URL('https://magician.test/tasks?compose=1&attention=1#queue');
		const state = {
			unrelated: 'kept',
			[ATTENTION_HISTORY_STATE_KEY]: { version: 1, kind: 'center' }
		};

		expect(planAttentionCenterClose(current, state)).toEqual({ kind: 'back', delta: -1 });
		expect(readAttentionHistoryMarker(state)).toEqual({ version: 1, kind: 'center' });
	});

	it('unwinds both item and center entries when selection was pushed from the center', () => {
		const current = new URL(
			'https://magician.test/tasks?compose=1&attention=1&attention_item=pause-1#queue'
		);
		const state = {
			[ATTENTION_HISTORY_STATE_KEY]: {
				version: 1,
				kind: 'item',
				itemParent: 'center'
			}
		};

		expect(planAttentionCenterClose(current, state)).toEqual({ kind: 'go', delta: -2 });
	});

	it('backs out once when a direct item was opened over a closed center', () => {
		const current = new URL('https://magician.test/tasks?attention_item=pause-1');
		const state = {
			[ATTENTION_HISTORY_STATE_KEY]: {
				version: 1,
				kind: 'item',
				itemParent: 'closed'
			}
		};

		expect(planAttentionCenterClose(current, state)).toEqual({ kind: 'back', delta: -1 });
	});

	it('rejects malformed history markers instead of applying unsafe navigation', () => {
		for (const marker of [
			{ version: 2, kind: 'center' },
			{ version: 1, kind: 'unknown' },
			{ version: 1, kind: 'item' },
			{ version: 1, kind: 'item', itemParent: 'unknown' }
		]) {
			expect(
				readAttentionHistoryMarker({ [ATTENTION_HISTORY_STATE_KEY]: marker })
			).toBeNull();
		}
	});

	it('replaces an unmarked deep link in place so Back cannot reopen it', () => {
		const current = new URL(
			'https://magician.test/square?cycle=7&attention=1&attention_item=pause-1#citizen-4'
		);
		const plan = planAttentionCenterClose(current, { unrelated: true });

		expect(plan.kind).toBe('replace');
		if (plan.kind !== 'replace') throw new Error('expected replacement plan');
		expect(plan.url.href).toBe('https://magician.test/square?cycle=7#citizen-4');
	});
});

describe('findAttentionRowByAlias', () => {
	it('resolves canonical, declared alias, and raw FeedItem ids to the same row', () => {
		const candidate = row();
		for (const id of [
			'pause-1',
			'correlation-1',
			'approval-1',
			'v3:attention:raw-feed-id'
		]) {
			expect(findAttentionRowByAlias([candidate], id)).toBe(candidate);
		}
	});
});
