/**
 * The structured approval card, rendered through `AttentionPromptModal` so the
 * joint with the confirmation controls is covered: the card replaces the raw
 * call, agent text is rendered as text, warnings are prominent, and the two
 * decision controls still answer.
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import {
	requestAttentionInput,
	resolveAttentionPrompt,
	type AttentionPromptRequest,
	type AttentionPromptResult
} from '$lib/stores/attentionPromptStore';
import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';
import type { HitlApprovalPreview } from './types';

afterEach(() => {
	resolveAttentionPrompt(null);
	cleanup();
});

function open(
	request: Omit<AttentionPromptRequest, 'id'>
): Promise<AttentionPromptResult | null> {
	render(AttentionPromptModal);
	return requestAttentionInput(request);
}

const PREVIEW: HitlApprovalPreview = {
	title: 'Mira wants to change `facts.md`',
	why_label: "Why (agent's words)",
	why: 'A fact changed <img src=x onerror=alert(1)>',
	diff: {
		path: 'facts.md',
		added: 3,
		removed: 1,
		hunks: [],
		truncated: true,
		omitted_changed_lines: 12,
		unified_diff: '--- a/facts.md\n+++ b/facts.md\n@@ -1,1 +1,1 @@\n-Founded 2023.\n+Founded 2024.\n'
	},
	evidence: ['task t-1', 'https://example.com/source'],
	warnings: [
		{
			code: 'stale_base',
			message:
				'This document changed after the agent read it — approving will be refused; reject and let the agent re-propose.'
		}
	],
	notes: [],
	raw: 'pack:propose_program_revision(content="# Facts…")'
};

describe('approval preview card', () => {
	it('shows who, why, the change size, evidence and warnings instead of the raw call', async () => {
		const answered = open({
			title: 'Confirm action',
			kind: 'confirmation',
			confirmation: { confirmLabel: 'Confirm', denyLabel: 'Deny', destructive: false },
			approvalPreview: PREVIEW
		});

		expect(await screen.findByText('Mira wants to change `facts.md`')).toBeInTheDocument();
		expect(screen.getByText("Why (agent's words)")).toBeInTheDocument();
		// Agent text is text: the markup is shown literally, never parsed.
		expect(
			screen.getByText('A fact changed <img src=x onerror=alert(1)>')
		).toBeInTheDocument();
		expect(document.querySelector('.approval-preview img')).toBeNull();
		expect(screen.getByTestId('approval-preview-stats')).toHaveTextContent('+3 lines, −1 lines');
		expect(screen.getByText('… 12 more changed lines')).toBeInTheDocument();
		const warnings = document.querySelector('.approval-preview__warnings');
		expect(warnings).toHaveAttribute('role', 'alert');
		expect(warnings).toHaveTextContent('changed after the agent read it');
		expect(screen.getByText('https://example.com/source')).toBeInTheDocument();
		// The raw call is kept, collapsed.
		const raw = screen.getByText('Raw call').closest('details');
		expect(raw).not.toBeNull();
		expect(raw!.open).toBe(false);
		expect(raw).toHaveTextContent('pack:propose_program_revision');

		await fireEvent.click(screen.getByRole('button', { name: 'Deny' }));
		expect(await answered).toEqual({ kind: 'choice', choiceId: 'deny' });
	});

	it('renders nothing extra for a plain confirmation', async () => {
		open({
			title: 'Confirm action',
			body: 'Delete the stale index?',
			kind: 'confirmation',
			confirmation: { confirmLabel: 'Yes', denyLabel: 'No', destructive: false }
		});
		expect(await screen.findByText('Delete the stale index?')).toBeInTheDocument();
		expect(screen.queryByTestId('approval-preview')).toBeNull();
	});
});
