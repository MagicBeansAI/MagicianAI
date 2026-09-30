import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import AttentionActionabilityBadge from './AttentionActionabilityBadge.svelte';

describe('AttentionActionabilityBadge', () => {
	it('labels shadow scores as previews and retains deterministic audit metadata', () => {
		const { body } = render(AttentionActionabilityBadge, {
			props: {
				metadata: {
					probability: 0.84,
					explanation: { code: 'direct_owner_request', label: 'Direct request to you' },
					model_version: 'actionability-gbt-v1',
					snapshot_id: 'snapshot-42',
					semantic_feature_status: 'succeeded',
					score_status: 'scored',
					mode: 'shadow'
				}
			}
		});

		expect(body).toContain('Actionability preview 84%');
		expect(body).toContain('Direct request to you');
		expect(body).toContain('Explanation direct_owner_request');
		expect(body).not.toContain('attention-actionability--active');
	});

	it('marks enforced scores active and explains deterministic fallback', () => {
		const active = render(AttentionActionabilityBadge, {
			props: {
				metadata: {
					probability: 0.91,
					explanation: null,
					model_version: 'actionability-gbt-v1',
					snapshot_id: 'snapshot-42',
					semantic_feature_status: 'succeeded',
					score_status: 'scored',
					mode: 'enforced'
				}
			}
		}).body;
		expect(active).toContain('Actionability 91%');
		expect(active).toContain('attention-actionability--active');

		const fallback = render(AttentionActionabilityBadge, {
			props: {
				metadata: {
					probability: null,
					explanation: null,
					model_version: null,
					snapshot_id: null,
					semantic_feature_status: 'invalid',
					score_status: 'fallback',
					mode: 'shadow'
				}
			}
		}).body;
		expect(fallback).toContain('invalid semantic features');
		expect(fallback).toContain('Slice 1 fallback');
	});
});
