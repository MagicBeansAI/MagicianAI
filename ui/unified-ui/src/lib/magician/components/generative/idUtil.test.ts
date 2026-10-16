import { describe, expect, it } from 'vitest';
import { buildStableDomId, normalizeIdPart } from './idUtil';

describe('idUtil', () => {
	it('caps normalized slug length', () => {
		const normalized = normalizeIdPart('X'.repeat(400));
		expect(normalized.length).toBeLessThanOrEqual(64);
		expect(normalized).toBe('x'.repeat(64));
	});

	it('is deterministic for the same input', () => {
		const a = buildStableDomId('muij', 'Component:Alpha');
		const b = buildStableDomId('muij', 'Component:Alpha');
		expect(a).toBe(b);
	});

	it('avoids deterministic collisions for long IDs with same prefix', () => {
		const left = `${'a'.repeat(64)}:`;
		const right = `${'a'.repeat(64)}/`;
		const leftId = buildStableDomId('muij', left);
		const rightId = buildStableDomId('muij', right);
		expect(leftId).not.toBe(rightId);
	});

	it('keeps generated IDs bounded for very long bases', () => {
		const generated = buildStableDomId('muij', 'component-'.repeat(1000));
		expect(generated.length).toBeLessThanOrEqual(220);
	});
});
