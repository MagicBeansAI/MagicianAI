/**
 * A confirmation's structured approval card (`input_schema.approval_preview`)
 * on its way from the canonical `hitl.requested` event to the prompt a
 * surface renders — and the shape it falls back to when absent.
 */
import { describe, expect, it } from 'vitest';

import { hitlRequestFromCanonicalEvent } from './adapters';
import { mapPromptResultToHitlValue, promptFor } from './respondToHitl';

function event(inputSchema: Record<string, unknown>): Record<string, unknown> {
	return {
		event_type: 'HitlRequested',
		correlation_id: 'pause-1~0123456789ab',
		source: 'agentic',
		input_type: 'confirmation',
		prompt: 'Mira wants to change `facts.md` (+1 lines, −1 lines)',
		execution_id: 'exec-1',
		principal: 'alice',
		workspace: 'work',
		input_schema: inputSchema
	};
}

const PREVIEW = {
	title: 'Mira wants to change `facts.md`',
	why_label: "Why (agent's words)",
	why: 'A fact changed <b>today</b>',
	diff: {
		path: 'facts.md',
		added: 1,
		removed: 1,
		hunks: [
			{
				old_start: 1,
				old_lines: 2,
				new_start: 1,
				new_lines: 2,
				lines: [
					{ kind: 'context', text: '  indented context' },
					{ kind: 'remove', text: 'Founded 2023.' },
					{ kind: 'add', text: 'Founded 2024.' },
					{ kind: 'bogus', text: 'dropped' }
				]
			}
		],
		truncated: true,
		omitted_changed_lines: 7,
		unified_diff: '--- a/facts.md\n+++ b/facts.md\n@@ -1,2 +1,2 @@\n   indented context\n-Founded 2023.\n+Founded 2024.\n'
	},
	evidence: ['task t-1', '  '],
	warnings: [{ code: 'stale_base', message: 'This document changed after the agent read it.' }]
};

describe('approval preview — adapter and prompt mapping', () => {
	it('reads the preview off the schema and replaces the raw body with it', () => {
		const request = hitlRequestFromCanonicalEvent(
			event({
				action_type: 'pack',
				action_summary: 'pack:propose_program_revision(content="…")',
				reason: 'Plane approval gate ended the harness turn.',
				approval_preview: PREVIEW
			})
		);
		expect(request).not.toBeNull();
		const preview = request!.schema.approval_preview!;
		expect(preview.title).toBe('Mira wants to change `facts.md`');
		expect(preview.why).toBe('A fact changed <b>today</b>');
		expect(preview.evidence).toEqual(['task t-1']);
		expect(preview.warnings).toEqual([
			{ code: 'stale_base', message: 'This document changed after the agent read it.' }
		]);
		// Unknown line kinds are dropped; line text keeps its indentation.
		expect(preview.diff!.hunks[0].lines.map((line) => line.kind)).toEqual([
			'context',
			'remove',
			'add'
		]);
		expect(preview.diff!.hunks[0].lines[0].text).toBe('  indented context');
		expect(preview.diff!.omitted_changed_lines).toBe(7);
		expect(preview.raw).toBe(
			'pack:propose_program_revision(content="…")\n\nPlane approval gate ended the harness turn.'
		);

		const prompt = promptFor(request!);
		expect(prompt.kind).toBe('confirmation');
		expect(prompt.body).toBeUndefined();
		expect(prompt.approvalPreview?.title).toBe('Mira wants to change `facts.md`');
		expect(prompt.confirmation).toBeDefined();
	});

	it('keeps the raw prompt as the body when there is no preview', () => {
		const request = hitlRequestFromCanonicalEvent(
			event({ action_type: 'pack', action_summary: 'Execute action pack' })
		);
		expect(request!.schema.approval_preview).toBeUndefined();
		const prompt = promptFor(request!);
		expect(prompt.approvalPreview).toBeUndefined();
		expect(prompt.body).toBe('Mira wants to change `facts.md` (+1 lines, −1 lines)');
	});

	it('ignores a preview without a title', () => {
		const request = hitlRequestFromCanonicalEvent(
			event({ approval_preview: { why: 'no title' } })
		);
		expect(request!.schema.approval_preview).toBeUndefined();
	});

	it('falls back to the prompt for the raw view when the schema has no raw call', () => {
		const request = hitlRequestFromCanonicalEvent(event({ approval_preview: { title: 'T' } }));
		const prompt = promptFor(request!);
		expect(prompt.approvalPreview?.raw).toBe('Mira wants to change `facts.md` (+1 lines, −1 lines)');
		expect(prompt.approvalPreview?.evidence).toEqual([]);
		expect(prompt.approvalPreview?.warnings).toEqual([]);
	});
});

describe('outward review — a captured send held for the owner', () => {
	const OPTIONS = [
		{ id: 'send', label: 'Send' },
		{ id: 'discard', label: 'Discard' },
		{ id: 'always_allow_recipient', label: 'Always allow ambassador to send to a@b.com' }
	];

	function outwardEvent(extra: Record<string, unknown> = {}): Record<string, unknown> {
		return event({
			action_type: 'gmail',
			action_summary: 'gmail:send(to="a@b.com")',
			reason: 'Captured outward send',
			approval_preview: { title: 'ambassador wants to email a@b.com', evidence: [], warnings: [], notes: [] },
			escalation_trigger: 'outward_review',
			options: OPTIONS,
			outward_review: {
				capability: 'gmail',
				action: 'send',
				class: 'message',
				recipients: ['a@b.com'],
				agent_id: 'ambassador',
				deadline_at_ms: 1_800_000_000_000
			},
			...extra
		});
	}

	it('reads the review record and offers the server options, preview unchanged', () => {
		const request = hitlRequestFromCanonicalEvent(outwardEvent());
		expect(request!.schema.outward_review).toEqual({
			capability: 'gmail',
			action: 'send',
			class: 'message',
			recipients: ['a@b.com'],
			agent_id: 'ambassador',
			deadline_at_ms: 1_800_000_000_000
		});
		const prompt = promptFor(request!);
		expect(prompt.kind).toBe('confirmation');
		expect(prompt.approvalPreview?.title).toBe('ambassador wants to email a@b.com');
		expect(prompt.outwardReview?.options.map((option) => [option.id, option.label])).toEqual(
			OPTIONS.map((option) => [option.id, option.label])
		);
		expect(prompt.outwardReview?.deadlineAtMs).toBe(1_800_000_000_000);
	});

	it.each(['send', 'discard', 'always_allow_recipient'])(
		'answers %s as a choice, the shape tool authorization posts',
		(choiceId) => {
			const request = hitlRequestFromCanonicalEvent(outwardEvent());
			expect(mapPromptResultToHitlValue(request!, { kind: 'choice', choiceId })).toEqual({
				type: 'choice',
				selected_id: choiceId,
				other_value: null
			});
		}
	);

	it('still posts a plain confirmation for a bare confirm/deny pair', () => {
		const request = hitlRequestFromCanonicalEvent(outwardEvent());
		expect(mapPromptResultToHitlValue(request!, { kind: 'choice', choiceId: 'confirm' })).toEqual({
			type: 'confirmation',
			confirmed: true
		});
		expect(mapPromptResultToHitlValue(request!, { kind: 'choice', choiceId: 'deny' })).toEqual({
			type: 'confirmation',
			confirmed: false
		});
	});

	it('falls back to Send and Discard when the payload carries no options', () => {
		const request = hitlRequestFromCanonicalEvent(outwardEvent({ options: undefined }));
		expect(promptFor(request!).outwardReview?.options.map((option) => option.id)).toEqual([
			'send',
			'discard'
		]);
	});

	it('leaves an ordinary confirmation without a review', () => {
		const request = hitlRequestFromCanonicalEvent(event({ approval_preview: PREVIEW }));
		const prompt = promptFor(request!);
		expect(prompt.outwardReview).toBeUndefined();
		expect(mapPromptResultToHitlValue(request!, { kind: 'choice', choiceId: 'confirm' })).toEqual({
			type: 'confirmation',
			confirmed: true
		});
	});
});
