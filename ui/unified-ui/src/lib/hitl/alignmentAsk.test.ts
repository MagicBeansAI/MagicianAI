/**
 * A preference alignment hold on the web: what the shared projection says,
 * what the private read adds, and what each answer posts.
 */
import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/scopeIdentityStore', () => ({
	scopedRequestHeaders: (headers: Record<string, string>) => headers
}));

import {
	alignmentAnswerValue,
	deadlineLine,
	parseAlignmentDetail,
	readAlignmentProjection
} from './alignmentAsk';
import { fetchAlignmentDetail } from './alignmentDetail';
import { hitlRequestFromCanonicalEvent } from './adapters';
import { mapPromptResultToHitlValue, promptFor } from './respondToHitl';

const projection = {
	schema_version: 'alignment_ask.v1',
	kind: 'preference_alignment',
	ask_id: 'ask-1',
	purpose: 'allow_once',
	choices: [
		{ id: 'allow_once', label: 'Allow this once' },
		{ id: 'use_alternative', label: 'Do something else instead' },
		{ id: 'change_preference', label: 'Change my preference' },
		{ id: 'cancel', label: 'Cancel' }
	],
	free_text: true,
	deadline_at_ms: 1_000_000,
	stage: 'decide'
};

function holdEvent() {
	return {
		event_type: 'HitlRequested',
		data: {
			correlation_id: 'exec-1:plan:step~ask-1',
			source: 'agentic',
			input_type: 'confirmation',
			prompt: 'An action needs your review before it runs.',
			principal: 'owner',
			workspace: 'ws',
			execution_id: 'exec-1',
			input_schema: {
				action_type: 'tool_call',
				escalation_trigger: 'preference_alignment',
				confirm_label: 'Allow this once',
				deny_label: 'Cancel',
				alignment: projection
			}
		}
	};
}

describe('the shared projection', () => {
	it('is read off the canonical event and carries no reason', () => {
		const request = hitlRequestFromCanonicalEvent(holdEvent());
		expect(request?.schema.escalation_trigger).toBe('preference_alignment');
		expect(request?.schema.alignment?.askId).toBe('ask-1');
		expect(request?.schema.alignment?.choices.map((choice) => choice.id)).toEqual([
			'allow_once',
			'use_alternative',
			'change_preference',
			'cancel'
		]);
		expect(readAlignmentProjection({ ...projection, kind: 'something_else' })).toBeNull();
		expect(readAlignmentProjection({ ...projection, choices: [] })).toBeNull();
	});

	it('becomes a hold prompt that maps every answer to a typed choice, never by label', () => {
		const request = hitlRequestFromCanonicalEvent(holdEvent())!;
		const prompt = promptFor(request);
		expect(prompt.title).toBe('Review before it runs');
		expect(prompt.alignment?.correlationId).toBe('exec-1:plan:step~ask-1');

		expect(
			mapPromptResultToHitlValue(request, { kind: 'choice', choiceId: 'use_alternative' })
		).toEqual({ type: 'choice', selected_id: 'use_alternative' });
		// The plain pair maps onto the ask's own consent and cancel.
		expect(mapPromptResultToHitlValue(request, { kind: 'choice', choiceId: 'confirm' })).toEqual({
			type: 'choice',
			selected_id: 'allow_once'
		});
		expect(mapPromptResultToHitlValue(request, { kind: 'choice', choiceId: 'deny' })).toEqual({
			type: 'choice',
			selected_id: 'cancel'
		});
		// A typed reply is text: the owner's instruction, not consent.
		expect(
			mapPromptResultToHitlValue(request, {
				kind: 'choice',
				choiceId: 'respond',
				input: '  order the paneer one  '
			})
		).toEqual({ type: 'text', value: 'order the paneer one' });
		// A new wording rides with its choice.
		expect(
			mapPromptResultToHitlValue(request, {
				kind: 'choice',
				choiceId: 'change_preference',
				input: 'vegetarian on weekdays'
			})
		).toEqual({
			type: 'choice',
			selected_id: 'change_preference',
			other_value: 'vegetarian on weekdays'
		});
	});
});

describe('the private read', () => {
	const privateBody = {
		private: true,
		prompt: 'An action needs your review before it runs.',
		retry_reason: null,
		alignment: {
			...projection,
			stage: 'confirm_wording',
			explanation: 'You asked to keep dinners vegetarian.',
			act_summary: 'swiggy order',
			intentions: [
				{
					id: 'pref-1',
					statement: 'diet: vegetarian dinners',
					assertion_status: 'direct_owner_statement',
					editable: true
				}
			],
			choices: [
				{ id: 'confirm_wording', label: 'Save this wording', lasting: true, consent: false },
				{ id: 'keep_as_is', label: "Don't change it", lasting: false, consent: false },
				{ id: 'cancel', label: 'Cancel', lasting: false, consent: false }
			],
			readback: {
				kind: 'replace',
				wording: 'vegetarian on weekdays',
				superseded: 'diet: vegetarian dinners',
				token: 'tok-123'
			}
		}
	};

	it('parses the reason, the cited preferences and the readback', () => {
		const detail = parseAlignmentDetail(privateBody)!;
		expect(detail.private).toBe(true);
		expect(detail.stage).toBe('confirm_wording');
		expect(detail.explanation).toBe('You asked to keep dinners vegetarian.');
		expect(detail.intentions[0].statement).toBe('diet: vegetarian dinners');
		expect(detail.readback).toEqual({
			kind: 'replace',
			wording: 'vegetarian on weekdays',
			superseded: 'diet: vegetarian dinners',
			token: 'tok-123'
		});
		expect(detail.choices.find((choice) => choice.id === 'confirm_wording')?.lasting).toBe(true);
		// Confirming posts the readback's token, nothing else.
		expect(alignmentAnswerValue('confirm_wording', detail.readback!.token)).toEqual({
			type: 'choice',
			selected_id: 'confirm_wording',
			other_value: 'tok-123'
		});
		expect(alignmentAnswerValue('respond', '   ')).toBeNull();
	});

	it('asks for the web surface and treats a replaced ask as gone', async () => {
		const fetched = vi.fn(async (_url: string) => new Response(JSON.stringify(privateBody), { status: 200 }));
		vi.stubGlobal('fetch', fetched);
		const outcome = await fetchAlignmentDetail('exec-1:plan:step~ask-1');
		expect(outcome.status).toBe('ok');
		expect(String(fetched.mock.calls[0][0])).toBe(
			'/api/magician/v2/hitl/exec-1%3Aplan%3Astep~ask-1/alignment?surface=web'
		);

		vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 409 })));
		expect((await fetchAlignmentDetail('x')).status).toBe('gone');
		vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 503 })));
		expect((await fetchAlignmentDetail('x')).status).toBe('unavailable');
		vi.unstubAllGlobals();
	});

	it('reads its own parsed projection back, so an open target keeps the hold', () => {
		const parsed = readAlignmentProjection({ ...projection, also_covers: ['approval_rule'] })!;
		expect(readAlignmentProjection(parsed)).toEqual(parsed);
		expect(readAlignmentProjection({ askId: 'a', choices: [] })).toBeNull();
	});

	it('reads a combined ask: the gate names publicly, their reasons privately', () => {
		expect(readAlignmentProjection({ ...projection, also_covers: ['approval_rule'] })?.alsoCovers).toEqual([
			'approval_rule'
		]);
		expect(readAlignmentProjection(projection)?.alsoCovers).toEqual([]);
		const detail = parseAlignmentDetail({
			...privateBody,
			alignment: {
				...privateBody.alignment,
				stage: 'decide',
				also_covers: [{ gate: 'approval_rule', reason: 'Action matched approval rule' }, { reason: 'no gate' }]
			}
		})!;
		expect(detail.alsoCovers).toEqual([{ gate: 'approval_rule', reason: 'Action matched approval rule' }]);
	});

	it('says how long the owner has, and that a late answer runs nothing', () => {
		expect(deadlineLine(null)).toBeNull();
		expect(deadlineLine(10 * 60_000, 0)).toBe("Answer within 10 min; after that it won't run.");
		expect(deadlineLine(3 * 3_600_000, 0)).toBe("Answer within 3 h; after that it won't run.");
		expect(deadlineLine(0, 1)).toBe('The answer window has closed; a late answer runs nothing.');
	});
});
