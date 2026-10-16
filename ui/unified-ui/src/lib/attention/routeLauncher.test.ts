import { describe, expect, it } from 'vitest';

import { parseAttentionRouteIntent } from './routeLauncher';

describe('parseAttentionRouteIntent', () => {
	it('recognizes canonical and legacy Attention routes', () => {
		expect(parseAttentionRouteIntent('/attention')).toEqual({ itemId: null });
		expect(parseAttentionRouteIntent('/approvals/')).toEqual({ itemId: null });
	});

	it('prefers exact item aliases carried by the route', () => {
		expect(parseAttentionRouteIntent('/attention?attention_item=pause%2F7')).toEqual({
			itemId: 'pause/7'
		});
		expect(parseAttentionRouteIntent('/approvals?approval_id=approval-4')).toEqual({
			itemId: 'approval-4'
		});
	});

	it('rejects unrelated routes', () => {
		expect(parseAttentionRouteIntent('/tasks?attention_item=pause-1')).toBeNull();
		expect(parseAttentionRouteIntent('https://example.test/attention?attention_item=pause-1')).toBeNull();
		expect(parseAttentionRouteIntent(null)).toBeNull();
	});
});
