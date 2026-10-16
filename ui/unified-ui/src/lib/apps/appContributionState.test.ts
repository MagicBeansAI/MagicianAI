import { describe, expect, it } from 'vitest';
import { parseAppContributionState } from './appContributionState';

const DIGEST = `blake3:${'a'.repeat(64)}`;

function fixture() {
	return {
		memory: {
			destination_generation: 4,
			destination_receipt_digest: DIGEST,
			items: [{
				proposal_id: 'proposal:one', proposal_digest: DIGEST,
				installation_id: 'installation:one', workflow_id: 'workflow:one',
				action_id: 'action:one', contribution_port_id: 'memory',
				source_event_ref: 'source:one', source_event_revision: 2,
				state: 'accepted', reason: 'owner_accepted', state_changed_at_ms: 20,
				expires_at_ms: 100, retained_until_ms: 90,
				claim_or_summary: 'A bounded reviewed claim.', details_compacted: false,
				revoke_review: { opaque: 'ignored by the web UI' }
			}]
		},
		retrieval: {
			destination_generation: 3, destination_receipt_digest: DIGEST,
			items: [{
				proposal_id: 'proposal:two', proposal_digest: `blake3:${'b'.repeat(64)}`,
				installation_id: 'installation:one', source_event_ref: 'source:two',
				source_event_revision: 3, state: 'stale',
				source_invalidation_reason: 'source_updated', state_changed_at_ms: 30,
				expires_at_ms: 120, target_agent_id: 'personal-assistant',
				details_compacted: true
			}]
		},
		retrieval_available: true
	};
}

describe('app contribution state', () => {
	it('parses both canonical destinations and orders state monotonically', () => {
		const state = parseAppContributionState(fixture());
		expect(state.items.map((item) => `${item.destination}:${item.state}`)).toEqual([
			'retrieval:stale', 'memory:accepted'
		]);
		expect(state.items[0]?.reason).toBe('source_updated');
	});

	it('rejects unknown state and retrieval availability substitution', () => {
		const unknown = fixture();
		unknown.memory.items[0]!.state = 'promoted';
		expect(() => parseAppContributionState(unknown)).toThrow(/unknown/i);

		const missing = fixture();
		delete (missing as { retrieval?: unknown }).retrieval;
		expect(() => parseAppContributionState(missing)).toThrow(/substituted/i);
	});

	it('rejects unbounded or unknown nested fields', () => {
		const unknown = fixture();
		(unknown.retrieval.items[0] as Record<string, unknown>).ranking = 1;
		expect(() => parseAppContributionState(unknown)).toThrow(/unknown field/i);

		const oversized = fixture();
		oversized.memory.items[0]!.claim_or_summary = 'x'.repeat(65 * 1024);
		expect(() => parseAppContributionState(oversized)).toThrow(/invalid/i);
	});
});
