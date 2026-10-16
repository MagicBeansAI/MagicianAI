import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import {
	requestAttentionInput,
	resolveAttentionPrompt
} from '$lib/stores/attentionPromptStore';
import AttentionPromptModal from './AttentionPromptModal.svelte';

afterEach(() => {
	resolveAttentionPrompt(null);
	cleanup();
});

describe('AttentionPromptModal decision safety', () => {
	it('requires an explicit choice and ignores Enter outside the active modal', async () => {
		render(AttentionPromptModal);
		let settled = false;
		const result = requestAttentionInput({
			title: 'Approve deployment',
			body: 'Deploy production changes?',
			kind: 'choice',
			choices: [
				{ id: 'confirm', label: 'Approve' },
				{ id: 'deny', label: 'Reject' }
			]
		}).then((value) => {
			settled = true;
			return value;
		});

		const approve = await screen.findByRole('radio', { name: 'Approve' });
		expect(approve).not.toBeChecked();
		expect(screen.getByRole('radio', { name: 'Reject' })).not.toBeChecked();
		expect(screen.getByRole('button', { name: 'Submit' })).toBeDisabled();

		await fireEvent.keyDown(window, { key: 'Enter' });
		await Promise.resolve();
		expect(settled).toBe(false);

		await fireEvent.click(approve);
		await fireEvent.keyDown(approve, { key: 'Enter' });
		expect(await result).toEqual({ kind: 'choice', choiceId: 'confirm', input: undefined });
	});

	it('does not focus or keyboard-apply a diff approval by default', async () => {
		render(AttentionPromptModal);
		let settled = false;
		const result = requestAttentionInput({
			title: 'Review staged changes',
			body: 'Inspect the patch before applying it.',
			kind: 'diff_approval',
			diffApproval: {
				transactionId: 'txn-7',
				files: [
					{
						path: 'src/app.ts',
						status: 'M',
						additions: 1,
						deletions: 0,
						unified_diff: '@@ -1 +1 @@'
					}
				]
			}
		}).then((value) => {
			settled = true;
			return value;
		});

		const apply = await screen.findByRole('button', { name: 'Apply' });
		expect(apply).not.toHaveFocus();
		await fireEvent.keyDown(document.activeElement ?? window, { key: 'Enter' });
		await Promise.resolve();
		expect(settled).toBe(false);

		await fireEvent.click(apply);
		expect(await result).toEqual({
			kind: 'choice',
			choiceId: 'apply',
			selectedPaths: undefined
		});
	});
});
