import { describe, expect, it } from 'vitest';

import type { HitlRequest } from '$lib/hitl/types';
import { isNoticeRequest, isPendingDiffRow, noticeParts, splitNoticeRows, type VibeRow } from './vibeHitlStore';
import { parseLiveRun } from './vibeDevLiveRunsStore';

function request(partial: Partial<HitlRequest>): HitlRequest {
	return {
		id: 'r1',
		source: 'user_request',
		input_type: 'choice',
		schema: {},
		prompt: '',
		scope: {},
		identifiers: {},
		...partial
	};
}

function row(req: HitlRequest): VibeRow {
	return { key: req.id, entry: { correlation_id: req.id, at: 0 }, request: req };
}

const briefing = request({
	id: 'brief',
	schema: {
		request_type: 'harness.notify_owner.briefing',
		options: [{ id: 'acknowledge', label: 'Acknowledge' }]
	},
	prompt: 'Owner briefing: Autopilot run finished: Pi\n\nAutopilot run finished: Pi. Open it to review the result.'
});

describe('VibeDev waits by kind', () => {
	it('reads an acknowledge-only request as a notice and a choice as a decision', () => {
		expect(isNoticeRequest(briefing)).toBe(true);
		expect(
			isNoticeRequest(
				request({ schema: { options: [{ id: 'dismiss', label: 'Dismiss' }] } })
			)
		).toBe(true);
		expect(
			isNoticeRequest(
				request({
					schema: {
						options: [
							{ id: 'approve', label: 'Approve' },
							{ id: 'deny', label: 'Deny' }
						]
					}
				})
			)
		).toBe(false);
		// One option that needs typed input is a question, not a notice.
		expect(
			isNoticeRequest(
				request({ schema: { options: [{ id: 'acknowledge', label: 'Ok', requires_input: true }] } })
			)
		).toBe(false);
		expect(isNoticeRequest(request({ input_type: 'diff_approval', schema: { options: [{ id: 'acknowledge', label: 'Ok' }] } }))).toBe(false);
	});

	it('splits rows and the briefing prompt', () => {
		const ask = request({ id: 'ask', schema: { options: [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }] } });
		const { notices, decisions } = splitNoticeRows([row(briefing), row(ask)]);
		expect(notices.map((r) => r.key)).toEqual(['brief']);
		expect(decisions.map((r) => r.key)).toEqual(['ask']);
		expect(noticeParts(briefing)).toEqual({
			title: 'Autopilot run finished: Pi',
			message: 'Autopilot run finished: Pi. Open it to review the result.'
		});
		expect(noticeParts(request({ prompt: 'Plain text' }))).toEqual({ title: null, message: 'Plain text' });
	});

	it('treats a hydrated, already-resolved proposal as not pending', () => {
		const live = row(request({ id: 'ccp-1', input_type: 'diff_approval', source: 'diff_approval' }));
		const done = row(
			request({ id: 'ccp-2', input_type: 'diff_approval', source: 'diff_approval', raw: { historical: true, status: 'applied' } })
		);
		expect(isPendingDiffRow(live)).toBe(true);
		expect(isPendingDiffRow(done)).toBe(false);
	});

	it('parses the live-run waiting kind and notice, leniently', () => {
		const run = parseLiveRun({
			run_id: 'task_report',
			state: 'done',
			waiting_kind: 'notice',
			notice: {
				request_id: 'vibedev-run-ended:task_report:exec_1',
				option_id: 'acknowledge',
				title: 'Autopilot run finished: Pi',
				message: 'Autopilot run finished: Pi. Open it.',
				severity: 'info'
			}
		});
		expect(run?.state).toBe('done');
		expect(run?.waitingKind).toBe('notice');
		expect(run?.notice).toEqual({
			requestId: 'vibedev-run-ended:task_report:exec_1',
			optionId: 'acknowledge',
			title: 'Autopilot run finished: Pi',
			message: 'Autopilot run finished: Pi. Open it.',
			severity: 'info',
			respond: null
		});
		// Older servers and unknown kinds read as nothing waiting.
		const old = parseLiveRun({ run_id: 'r', state: 'needs_you', waiting_kind: 'something_new' });
		expect(old?.waitingKind).toBeNull();
		expect(old?.notice).toBeNull();
	});
});
