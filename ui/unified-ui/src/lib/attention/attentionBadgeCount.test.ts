import { describe, expect, it } from 'vitest';

import { resolveAttentionBadgeCount } from './attentionBadgeCount';

describe('resolveAttentionBadgeCount', () => {
	it('deduplicates overlapping HITL and feed needs-action counts', () => {
		expect(resolveAttentionBadgeCount(4, 5, 0)).toBe(5);
		expect(resolveAttentionBadgeCount(7, 5, 2)).toBe(9);
	});

	it('never exposes negative projection values', () => {
		expect(resolveAttentionBadgeCount(-1, -2, -3)).toBe(0);
	});
});
