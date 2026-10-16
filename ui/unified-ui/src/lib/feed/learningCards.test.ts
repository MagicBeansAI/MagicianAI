import { describe, expect, it } from 'vitest';

import type { FeedItem } from './types';
import { agentLearningSourceUrl, learningValueLabel } from './learningCards';

function feedItem(metadata: unknown): FeedItem {
	return {
		id: 'learning_candidate:candidate-1',
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'learning_candidate',
		title: 'Review public contact lead',
		summary: 'Fallback summary',
		status: 'needs_action',
		created_at: 1,
		updated_at: 1,
		actions: [],
		metadata
	};
}

describe('learningValueLabel', () => {
	it('prefers memory_value_label over raw structured memory JSON', () => {
		const item = feedItem({
			memory_value_label: 'Identity: Jane Rao · Org: Acme AI',
			memory_value: {
				kind: 'public_contact_identity_research',
				possible_identity: 'Jane Rao',
				org: 'Acme AI'
			}
		});

		expect(learningValueLabel(item)).toBe('Identity: Jane Rao · Org: Acme AI');
	});
});

describe('agentLearningSourceUrl', () => {
	it('reads canonical and legacy research provenance shapes', () => {
		expect(agentLearningSourceUrl({ sources: ['/tasks/canonical'] })).toBe('/tasks/canonical');
		expect(agentLearningSourceUrl({ sources: { summary: '/tasks/scalar' } })).toBe(
			'/tasks/scalar'
		);
		expect(agentLearningSourceUrl({ source_urls: ['/tasks/legacy'] })).toBe('/tasks/legacy');
	});

	it('returns null for absent or non-string provenance', () => {
		expect(agentLearningSourceUrl(null)).toBeNull();
		expect(agentLearningSourceUrl({ sources: [42] })).toBeNull();
	});
});
