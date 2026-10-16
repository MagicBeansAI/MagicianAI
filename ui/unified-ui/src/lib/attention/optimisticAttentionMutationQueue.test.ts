import { describe, expect, it } from 'vitest';

import {
	countScopedOptimisticMutations,
	followUpAttentionMutationKey,
	optimisticAttentionMutationQueue,
	worthAttentionMutationKey
} from './optimisticAttentionMutationQueue';

const SCOPE = { principal: 'anonymous', workspace: 'default' };

describe('countScopedOptimisticMutations', () => {
	it('counts only the matching lane and scope', () => {
		optimisticAttentionMutationQueue.reset();
		void optimisticAttentionMutationQueue.enqueue(
			followUpAttentionMutationKey('ann-1', SCOPE),
			async () => ({ ok: true })
		);
		void optimisticAttentionMutationQueue.enqueue(
			followUpAttentionMutationKey('ann-2', SCOPE),
			async () => ({ ok: true })
		);
		void optimisticAttentionMutationQueue.enqueue(
			worthAttentionMutationKey('worth-1', SCOPE),
			async () => ({ ok: true })
		);
		void optimisticAttentionMutationQueue.enqueue(
			followUpAttentionMutationKey('ann-other', { principal: 'other', workspace: 'default' }),
			async () => ({ ok: true })
		);

		const { statusByKey } = {
			statusByKey: new Map(
				[
					followUpAttentionMutationKey('ann-1', SCOPE),
					followUpAttentionMutationKey('ann-2', SCOPE),
					worthAttentionMutationKey('worth-1', SCOPE),
					followUpAttentionMutationKey('ann-other', {
						principal: 'other',
						workspace: 'default'
					})
				].map((key) => [key, 'pending' as const])
			)
		};

		expect(countScopedOptimisticMutations(statusByKey, 'follow_up', SCOPE)).toBe(2);
		expect(countScopedOptimisticMutations(statusByKey, 'worth_a_look', SCOPE)).toBe(1);
		optimisticAttentionMutationQueue.reset();
	});
});
