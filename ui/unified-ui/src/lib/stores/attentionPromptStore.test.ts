import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import {
	attentionPromptStore,
	requestAttentionInput,
	resolveAttentionPrompt
} from './attentionPromptStore';

const ordinary = { title: 'Which quarter?', kind: 'text' as const };
const credential = {
	title: 'Enter your password',
	kind: 'password' as const,
	sensitive: { oneTime: false, kind: 'password' as const }
};

describe('attentionPromptStore', () => {
	it('lets an ordinary prompt be displaced by the next one', async () => {
		const first = requestAttentionInput(ordinary);
		const second = requestAttentionInput({ ...ordinary, title: 'Which month?' });
		await expect(first).resolves.toBeNull();
		expect(get(attentionPromptStore).request?.title).toBe('Which month?');
		resolveAttentionPrompt(null);
		await expect(second).resolves.toBeNull();
		expect(get(attentionPromptStore).request).toBeNull();
	});

	// Displacing a credential prompt resolved it `null`, which `respondToHitl`
	// reads as a dismissal — so it posted `aborted`, cancelling the ask and
	// retiring its custody because something else wanted the modal, while the
	// owner was halfway through typing the value.
	it('never displaces a credential prompt, and opens the queued one after it', async () => {
		const secret = requestAttentionInput(credential);
		const queued = requestAttentionInput(ordinary);
		expect(get(attentionPromptStore).request?.title).toBe('Enter your password');

		let settled = false;
		void secret.then(() => {
			settled = true;
		});
		await Promise.resolve();
		expect(settled, 'the credential prompt is still open').toBe(false);

		resolveAttentionPrompt({ kind: 'password', value: 'prompt-store-canary' });
		await expect(secret).resolves.toEqual({ kind: 'password', value: 'prompt-store-canary' });
		expect(get(attentionPromptStore).request?.title).toBe('Which quarter?');
		resolveAttentionPrompt(null);
		await expect(queued).resolves.toBeNull();
	});
});
