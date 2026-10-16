import { describe, expect, it } from 'vitest';

import { PrimitiveRegistry } from './primitiveRegistry';
import type { TutorRecipe } from './recipeTypes';

function fakeResponse(body: unknown, ok = true): Response {
	return {
		ok,
		json: async () => body
	} as unknown as Response;
}

const ANGLE: TutorRecipe = {
	type: 'angle_marker',
	aliases: ['arc', 'perpendicular_marker'],
	defaults: {},
	draw: [{ op: 'arc', params: { cx: 'cx', cy: 'cy', r: 'r|size' } }]
};

describe('PrimitiveRegistry — lookup + alias matching', () => {
	it('resolves by primary type and aliases (case-insensitive)', () => {
		const registry = new PrimitiveRegistry([ANGLE]);
		expect(registry.recipe('angle_marker')?.type).toBe('angle_marker');
		expect(registry.recipe('ARC')?.type).toBe('angle_marker');
		expect(registry.recipe('perpendicular_marker')?.type).toBe('angle_marker');
		expect(registry.isSupported('arc')).toBe(true);
		expect(registry.isSupported('unknown_type')).toBe(false);
		expect(registry.recipe('unknown_type')).toBeUndefined();
	});

	it('dedups recipes by primary type', () => {
		const registry = new PrimitiveRegistry([ANGLE]);
		expect(registry.recipes).toHaveLength(1);
		expect(registry.recipes[0].type).toBe('angle_marker');
	});
});

describe('PrimitiveRegistry — refresh', () => {
	it('adopts a bare-array recipe set', async () => {
		const registry = new PrimitiveRegistry();
		const ok = await registry.refresh(async () =>
			fakeResponse([{ type: 'circle', draw: [{ op: 'circle', cx: 'cx', cy: 'cy', r: 'r' }] }])
		);
		expect(ok).toBe(true);
		expect(registry.hasFetched).toBe(true);
		expect(registry.isSupported('circle')).toBe(true);
	});

	it('adopts an { primitives: [...] } envelope', async () => {
		const registry = new PrimitiveRegistry();
		const ok = await registry.refresh(async () =>
			fakeResponse({ primitives: [{ type: 'rect', draw: [] }], etag: 'abc' })
		);
		expect(ok).toBe(true);
		expect(registry.isSupported('rect')).toBe(true);
	});

	it('keeps the current set on a non-2xx response', async () => {
		const registry = new PrimitiveRegistry([ANGLE]);
		const ok = await registry.refresh(async () => fakeResponse([], false));
		expect(ok).toBe(false);
		expect(registry.isSupported('angle_marker')).toBe(true); // not downgraded
	});

	it('keeps the current set on a malformed / empty payload', async () => {
		const registry = new PrimitiveRegistry([ANGLE]);
		expect(await registry.refresh(async () => fakeResponse('not-a-recipe-set'))).toBe(false);
		expect(await registry.refresh(async () => fakeResponse([]))).toBe(false);
		expect(registry.isSupported('angle_marker')).toBe(true);
	});

	it('keeps the current set on a network error', async () => {
		const registry = new PrimitiveRegistry([ANGLE]);
		const ok = await registry.refresh(async () => {
			throw new Error('offline');
		});
		expect(ok).toBe(false);
		expect(registry.isSupported('angle_marker')).toBe(true);
	});
});
