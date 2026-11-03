import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';

import {
	confirmationStore,
	requestConfirmation,
	requestTextInput,
	resolveConfirmation
} from './confirmationStore';

const prompt = { title: 'New thread', confirmLabel: 'Create', input: { label: 'Thread name' } };

describe('requestTextInput', () => {
	it('resolves the trimmed value on confirm', async () => {
		const pending = requestTextInput(prompt);
		expect(get(confirmationStore).request?.input?.label).toBe('Thread name');
		resolveConfirmation(true, '  Trip planning  ');
		expect(await pending).toBe('Trip planning');
		expect(get(confirmationStore).request).toBeNull();
	});

	it('resolves null on cancel or an empty value', async () => {
		const cancelled = requestTextInput(prompt);
		resolveConfirmation(false, 'ignored');
		expect(await cancelled).toBeNull();

		const empty = requestTextInput(prompt);
		resolveConfirmation(true, '   ');
		expect(await empty).toBeNull();
	});

	it('resolves null when a newer request replaces it', async () => {
		const first = requestTextInput(prompt);
		const second = requestConfirmation({ title: 'Sure?', message: 'Really' });
		expect(await first).toBeNull();
		resolveConfirmation(true);
		expect(await second).toBe(true);
	});
});
