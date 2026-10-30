import { describe, expect, it, vi } from 'vitest';
import type { FeedAttentionResponse, FeedItem } from '$lib/feed/types';
import { completePendingFeed } from './pendingFeed';

function item(id: string): FeedItem {
	return { id, principal: 'owner', workspace: 'default', item_type: 'escalation', title: id,
		status: 'needs_action', created_at: 1, updated_at: 1, actions: [], metadata: {} };
}
function page(overrides: Partial<FeedAttentionResponse> = {}): FeedAttentionResponse {
	const end = { total: 1, limit: 1, next_cursor: null, has_more: false };
	return { counts: { requests: 1, approvals: 0, escalations: 0, needs_action: 1, failed: 0, running: 0 },
		requests: [], approvals: [], escalations: [], failed: [], running: [],
		pages: { requests: { ...end }, approvals: { ...end }, escalations: { ...end }, failed: { ...end }, running: { ...end } },
		...overrides };
}

describe('completePendingFeed', () => {
	it('includes older questions and advances only unfinished actionable lanes', async () => {
		const head = page({ requests: [item('first')], failed: [item('failed-history')] });
		head.pages!.requests.next_cursor = 'requests-2';
		head.pages!.approvals.next_cursor = 'approvals-2';
		head.pages!.failed.next_cursor = 'failed-2';
		const middle = page({ requests: [item('older')], approvals: [item('approval')] });
		middle.pages!.approvals.next_cursor = 'approvals-3';
		const last = page({ requests: [item('do-not-restart-this-lane')], approvals: [item('oldest-approval')] });
		const read = vi.fn().mockResolvedValueOnce(middle).mockResolvedValueOnce(last);
		const result = await completePendingFeed(head, read);
		expect(result.requests.map((row) => row.id)).toEqual(['first', 'older']);
		expect(result.approvals.map((row) => row.id)).toEqual(['approval', 'oldest-approval']);
		expect(result.failed.map((row) => row.id)).toEqual(['failed-history']);
		expect(result.pages!.failed.next_cursor).toBe('failed-2');
		expect(read.mock.calls[1][0]).toEqual({ requests: null, approvals: 'approvals-3', escalations: null });
	});
	it('refuses a repeated cursor instead of polling it forever', async () => {
		const head = page(); head.pages!.requests.next_cursor = 'same';
		const read = vi.fn().mockResolvedValue(head);
		await expect(completePendingFeed(head, read)).rejects.toThrow(/repeated/);
		expect(read).toHaveBeenCalledTimes(1);
	});
	it('does not paginate servers without cursor metadata', async () => {
		const head = page(); delete head.pages;
		const read = vi.fn(); expect(await completePendingFeed(head, read)).toBe(head);
		expect(read).not.toHaveBeenCalled();
	});
});
