import { describe, expect, it } from 'vitest';

import type { FeedItem } from '$lib/feed/types';
import {
	classifyRespondResult,
	parseWebPushPayload,
	planAskActions,
	requestFromFeedItem,
	respondBody,
	safeWebPath,
	unwrapFeedItem,
	urlBase64ToUint8Array
} from './protocol';

function askItem(
	schema: Record<string, unknown>,
	extra: Record<string, unknown> = {}
): FeedItem {
	return {
		id: 'feed-1',
		principal: 'owner',
		workspace: 'default',
		item_type: 'attention' as FeedItem['item_type'],
		task_id: 'task-1',
		title: 'Ship the release?',
		status: 'pending' as FeedItem['status'],
		created_at: 1,
		updated_at: 2,
		actions: [],
		metadata: {
			attention_kind: 'hitl.requested',
			source: 'user_request',
			request_id: 'cid-1',
			execution_id: 'exec-1',
			input_schema: schema,
			...extra
		}
	};
}

const confirmItem = () =>
	askItem({ type: 'confirmation', confirm_label: 'Ship it', deny_label: 'Hold' });
const choiceItem = (options: unknown[], more: Record<string, unknown> = {}) =>
	askItem({ type: 'choice', options, ...more });

describe('parseWebPushPayload', () => {
	it('reads an attention_requested payload and keeps a same-origin web_path', () => {
		expect(
			parseWebPushPayload({
				kind: 'attention_requested',
				correlation_id: 'cid-1',
				source: 'user_request',
				expires_at: '2026-10-10T00:00:00Z',
				web_path: '/attention?attention_item=cid-1',
				answer_mode: 'confirm',
				option_count: 2
			})
		).toEqual({
			kind: 'attention_requested',
			correlation_id: 'cid-1',
			source: 'user_request',
			expires_at: '2026-10-10T00:00:00Z',
			web_path: '/attention?attention_item=cid-1',
			answer_mode: 'confirm',
			option_count: 2
		});
	});

	it('falls back to the Attention deep link for an off-origin web_path', () => {
		const parsed = parseWebPushPayload(
			JSON.stringify({ kind: 'attention_requested', correlation_id: 'a b', web_path: '//evil.test/x' })
		);
		expect(parsed?.kind === 'attention_requested' && parsed.web_path).toBe(
			'/attention?attention_item=a%20b'
		);
		expect(safeWebPath('https://evil.test/', 'x')).toBe('/attention?attention_item=x');
		expect(safeWebPath('/\\evil.test', 'x')).toBe('/attention?attention_item=x');
	});

	it('reads resolution kinds and rejects unknown ones', () => {
		expect(parseWebPushPayload({ kind: 'attention_resolved', correlation_id: 'c' })).toEqual({
			kind: 'attention_resolved',
			correlation_id: 'c'
		});
		expect(parseWebPushPayload({ kind: 'attention_expired', correlation_id: 'c' })?.kind).toBe(
			'attention_expired'
		);
		expect(parseWebPushPayload({ kind: 'other', correlation_id: 'c' })).toBeNull();
		expect(parseWebPushPayload({ kind: 'attention_requested' })).toBeNull();
		expect(parseWebPushPayload('not json')).toBeNull();
	});
});

describe('feed item unwrapping', () => {
	it('accepts a bare item or {item}', () => {
		const item = confirmItem();
		expect(unwrapFeedItem(item)).toBe(item);
		expect(unwrapFeedItem({ item })).toBe(item);
		expect(unwrapFeedItem(null)).toBeNull();
	});

	it('lifts an ask nested under metadata.hitl_request', () => {
		const item = askItem({}, {});
		item.metadata = {
			attention_kind: 'hitl.requested',
			hitl_request: {
				source: 'user_request',
				request_id: 'cid-n',
				input_schema: { type: 'confirmation' }
			}
		};
		const request = requestFromFeedItem(item);
		expect(request?.input_type).toBe('confirmation');
		expect(request?.identifiers.correlation_id).toBe('cid-n');
	});
});

describe('planAskActions', () => {
	it('offers the confirm/deny labels for a confirmation', () => {
		const plan = planAskActions(requestFromFeedItem(confirmItem())!, 2, 'confirm');
		expect(plan?.mode).toBe('confirm');
		expect(plan?.actions).toEqual([
			{ action: 'confirm', title: 'Ship it' },
			{ action: 'deny', title: 'Hold' }
		]);
		expect(plan?.answers.confirm).toEqual({ type: 'confirmation', confirmed: true });
		expect(plan?.answers.deny).toEqual({ type: 'confirmation', confirmed: false });
	});

	it('defaults to Approve/Reject', () => {
		const plan = planAskActions(requestFromFeedItem(askItem({ type: 'confirmation' }))!, 4, 'confirm');
		expect(plan?.actions.map((action) => action.title)).toEqual(['Approve', 'Reject']);
	});

	it('offers both options of a two-way single choice', () => {
		const plan = planAskActions(
			requestFromFeedItem(
				choiceItem([
					{ id: 'a', label: 'Staging' },
					{ id: 'b', label: 'Production' }
				])
			)!,
			2,
			'choice'
		);
		expect(plan?.mode).toBe('choice');
		expect(plan?.actions.map((action) => action.title)).toEqual(['Staging', 'Production']);
		expect(plan?.answers[plan.actions[1].action]).toEqual({ type: 'choice', selected_id: 'b' });
	});

	it('refuses wider, multi-select or typed choices', () => {
		const three = choiceItem([
			{ id: 'a', label: 'A' },
			{ id: 'b', label: 'B' },
			{ id: 'c', label: 'C' }
		]);
		const typed = choiceItem([
			{ id: 'a', label: 'A' },
			{ id: 'o', label: 'Other', requires_input: true }
		]);
		const multiple = choiceItem(
			[
				{ id: 'a', label: 'A' },
				{ id: 'b', label: 'B' }
			],
			{ multiple: true }
		);
		for (const item of [three, typed, multiple]) {
			expect(planAskActions(requestFromFeedItem(item)!, 2, 'choice')).toBeNull();
		}
	});

	it('never offers buttons for sensitive, destructive, form or text asks', () => {
		const sensitive = askItem({ type: 'confirmation', sensitive: { kind: 'otp' } });
		const destructive = askItem({ type: 'confirmation', destructive: true });
		const form = askItem({
			type: 'form',
			questions: [{ id: 'q', prompt: 'Name?', input_type: 'text' }]
		});
		const text = askItem({ type: 'text' });
		for (const item of [sensitive, destructive, form, text]) {
			const request = requestFromFeedItem(item);
			expect(request ? planAskActions(request, 2, 'confirm') : null).toBeNull();
			expect(request ? planAskActions(request, 2, 'text') : null).toBeNull();
		}
	});

	it('respects Notification.maxActions and the server answer_mode allow-list', () => {
		const request = requestFromFeedItem(confirmItem())!;
		expect(planAskActions(request, 0, 'confirm')).toBeNull();
		expect(planAskActions(request, 1, 'confirm')).toBeNull();
		// No answer_mode: the server says this ask must be opened in the app.
		expect(planAskActions(request, 2, null)).toBeNull();
		expect(planAskActions(request, 2, 'text')).toBeNull();
		expect(planAskActions(request, 2, 'choice')).toBeNull();
		expect(planAskActions(request, 2, 'confirm')).not.toBeNull();
	});

	it('builds a web_push respond body with execution id', () => {
		const plan = planAskActions(requestFromFeedItem(confirmItem())!, 2, 'confirm')!;
		expect(plan.respond.url).toBe('/api/magician/v2/hitl/cid-1/respond');
		expect(respondBody(plan.respond, plan.answers.confirm)).toEqual({
			source: 'user_request',
			input_type: 'confirmation',
			value: { type: 'confirmation', confirmed: true },
			channel: 'web_push',
			execution_id: 'exec-1'
		});
	});
});

describe('classifyRespondResult', () => {
	it('maps statuses', () => {
		expect(classifyRespondResult(200, { accepted: true })).toBe('sent');
		expect(classifyRespondResult(404, null)).toBe('already');
		expect(classifyRespondResult(409, { reason: 'already_resolved' })).toBe('already');
		expect(classifyRespondResult(410, null)).toBe('already');
		expect(classifyRespondResult(500, null)).toBe('failed');
		expect(classifyRespondResult(401, null)).toBe('failed');
		expect(classifyRespondResult(200, { status: 'reask_required' })).toBe('failed');
		expect(classifyRespondResult(200, { accepted: false })).toBe('failed');
	});
});

describe('urlBase64ToUint8Array', () => {
	it('decodes base64url without padding', () => {
		expect(Array.from(urlBase64ToUint8Array('AQID_-8'))).toEqual([1, 2, 3, 255, 239]);
	});
});
