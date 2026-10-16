import { describe, expect, it } from 'vitest';
import { fixtureEvents } from './fixtures';
import { getCardForKind, hasDedicatedCard, registeredKinds } from './registry';
import type { AgentUpdateKindTag } from '$lib/types/agentUpdate';

// We avoid importing the `.svelte` file directly — vitest here is not
// configured with the Svelte plugin (no test currently imports `.svelte`).
// Every declared kind now has a dedicated card, so to get the UnknownCard
// reference we request a synthetic kind not in the registry.
const UnknownCardRef = getCardForKind('__synthetic_unknown__' as AgentUpdateKindTag);

describe('card registry', () => {
	it('resolves a component for every fixture event', () => {
		for (const event of fixtureEvents) {
			const component = getCardForKind(event.kind);
			expect(component, `no component resolved for ${event.kind}`).toBeTruthy();
		}
	});

	it('returns UnknownCard only for truly unregistered kinds', () => {
		// All declared AgentUpdateKind variants now have dedicated cards.
		// Any other string (e.g. a future variant we haven't mapped yet)
		// should still fall through safely to UnknownCard.
		const syntheticKind = '__synthetic_unknown__' as AgentUpdateKindTag;
		expect(hasDedicatedCard(syntheticKind)).toBe(false);
		expect(getCardForKind(syntheticKind)).toBe(UnknownCardRef);
	});

	it('every declared AgentUpdateKind has a dedicated card', () => {
		const allKinds: AgentUpdateKindTag[] = [
			'cycle_started',
			'cycle_completed',
			'cycle_failed',
			'cycle_paused',
			'approval_requested',
			'approval_resolved',
			'approval_expired',
			'artifact_created',
			'artifact_create_failed',
			'task_created',
			'task_updated',
			'task_completed',
			'task_failed',
			'circuit_opened',
			'circuit_recovered',
			'goal_completed',
			'goal_failed',
			'goal_recovered',
			'delegation_issued',
			'delegation_resolved',
			'agent_created',
			'agent_updated',
			'agent_deleted',
			'agent_paused',
			'agent_resumed',
			'memory_report',
			'feedback_generated',
			'tier_consolidated',
			'feed_stalled',
			'published_surface_changed'
		];
		expect(allKinds.length).toBe(30);
		for (const kind of allKinds) {
			expect(hasDedicatedCard(kind), `${kind} should have a dedicated card`).toBe(true);
			expect(
				getCardForKind(kind),
				`${kind} should not resolve to the UnknownCard fallback`
			).not.toBe(UnknownCardRef);
		}
	});

	it('dedicated cards for the same family share the same component reference', () => {
		// Cycle variants all go to CycleProgressCard; approval variants to
		// ApprovalRequestCard; task variants to TaskCard. This lets the
		// dispatcher render any variant in a family without a family-specific
		// branch. Verify the 1:N mapping.
		expect(getCardForKind('cycle_started')).toBe(getCardForKind('cycle_completed'));
		expect(getCardForKind('cycle_completed')).toBe(getCardForKind('cycle_failed'));
		expect(getCardForKind('cycle_failed')).toBe(getCardForKind('cycle_paused'));
		expect(getCardForKind('approval_requested')).toBe(getCardForKind('approval_resolved'));
		expect(getCardForKind('approval_resolved')).toBe(getCardForKind('approval_expired'));
		expect(getCardForKind('task_created')).toBe(getCardForKind('task_failed'));
		expect(getCardForKind('circuit_opened')).toBe(getCardForKind('circuit_recovered'));
		expect(getCardForKind('goal_completed')).toBe(getCardForKind('goal_recovered'));
		expect(getCardForKind('delegation_issued')).toBe(getCardForKind('delegation_resolved'));
	});

	it('different families resolve to different components', () => {
		expect(getCardForKind('cycle_started')).not.toBe(getCardForKind('approval_requested'));
		expect(getCardForKind('cycle_started')).not.toBe(getCardForKind('task_created'));
		expect(getCardForKind('task_created')).not.toBe(getCardForKind('goal_completed'));
		expect(getCardForKind('goal_completed')).not.toBe(getCardForKind('delegation_issued'));
		expect(getCardForKind('artifact_created')).not.toBe(getCardForKind('circuit_opened'));
	});

	it('registeredKinds() covers all 30 variants', () => {
		expect(registeredKinds().length).toBe(30);
	});

	it('fixtures cover all 30 variant kinds exactly once', () => {
		const seen = new Set<string>();
		for (const event of fixtureEvents) {
			expect(seen.has(event.kind), `duplicate fixture for ${event.kind}`).toBe(false);
			seen.add(event.kind);
		}
		expect(seen.size).toBe(30);
	});
});
