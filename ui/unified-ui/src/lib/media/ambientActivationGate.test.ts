import { describe, expect, it, vi } from 'vitest';

import { createAmbientActivationGate } from './ambientActivationGate';

describe('Ambient Dictation activation gate', () => {
	it('does not admit background activity without an explicit permit', async () => {
		const gate = createAmbientActivationGate();
		const admitted = vi.fn();
		void gate.wait().then(admitted);
		await Promise.resolve();
		expect(admitted).not.toHaveBeenCalled();

		gate.admit();
		await Promise.resolve();
		expect(admitted).toHaveBeenCalledWith(true);
	});

	it('coalesces repeated wake hits into one turn', async () => {
		const gate = createAmbientActivationGate();
		gate.admit();
		gate.admit();
		expect(await gate.wait()).toBe(true);

		let secondResolved = false;
		void gate.wait().then(() => { secondResolved = true; });
		await Promise.resolve();
		expect(secondResolved).toBe(false);
		gate.cancel();
	});

	it('cancellation releases a pending waiter and is terminal', async () => {
		const gate = createAmbientActivationGate();
		const waiting = gate.wait();
		gate.cancel();
		expect(await waiting).toBe(false);
		gate.admit();
		expect(await gate.wait()).toBe(false);
	});
});
