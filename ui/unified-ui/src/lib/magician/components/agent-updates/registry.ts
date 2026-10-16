// Card registry — maps AgentUpdateKind discriminators to Svelte components.
//
// Extending: add a new entry when a new AgentUpdateKind variant lands a
// dedicated card. Variants without an entry fall back to UnknownCard, which
// renders the raw payload for debugging. That is by design — it keeps the
// feed rendering correct-by-construction even when vocabulary outpaces card
// coverage.

import type { ComponentType } from 'svelte';
import type { AgentUpdateKindTag } from '$lib/types/agentUpdate';
import CycleProgressCard from './CycleProgressCard.svelte';
import ApprovalRequestCard from './ApprovalRequestCard.svelte';
import ArtifactReadyCard from './ArtifactReadyCard.svelte';
import TaskCard from './TaskCard.svelte';
import CircuitCard from './CircuitCard.svelte';
import GoalCard from './GoalCard.svelte';
import DelegationCard from './DelegationCard.svelte';
import AgentLifecycleCard from './AgentLifecycleCard.svelte';
import MemoryReportCard from './MemoryReportCard.svelte';
import FeedbackCard from './FeedbackCard.svelte';
import TierConsolidationCard from './TierConsolidationCard.svelte';
import FeedStalledCard from './FeedStalledCard.svelte';
import PublishedSurfaceCard from './PublishedSurfaceCard.svelte';
import UnknownCard from './UnknownCard.svelte';

export type CardComponent = ComponentType;

const registry: Partial<Record<AgentUpdateKindTag, CardComponent>> = {
	// Cycle — one card handles all four lifecycle variants
	cycle_started: CycleProgressCard as unknown as CardComponent,
	cycle_completed: CycleProgressCard as unknown as CardComponent,
	cycle_failed: CycleProgressCard as unknown as CardComponent,
	cycle_paused: CycleProgressCard as unknown as CardComponent,
	// Approval — one card for request/resolve/expire
	approval_requested: ApprovalRequestCard as unknown as CardComponent,
	approval_resolved: ApprovalRequestCard as unknown as CardComponent,
	approval_expired: ApprovalRequestCard as unknown as CardComponent,
	// Artifact
	artifact_created: ArtifactReadyCard as unknown as CardComponent,
	artifact_create_failed: ArtifactReadyCard as unknown as CardComponent,
	// Task
	task_created: TaskCard as unknown as CardComponent,
	task_updated: TaskCard as unknown as CardComponent,
	task_completed: TaskCard as unknown as CardComponent,
	task_failed: TaskCard as unknown as CardComponent,
	// Circuit breaker
	circuit_opened: CircuitCard as unknown as CardComponent,
	circuit_recovered: CircuitCard as unknown as CardComponent,
	// Goal
	goal_completed: GoalCard as unknown as CardComponent,
	goal_failed: GoalCard as unknown as CardComponent,
	goal_recovered: GoalCard as unknown as CardComponent,
	// Delegation
	delegation_issued: DelegationCard as unknown as CardComponent,
	delegation_resolved: DelegationCard as unknown as CardComponent,
	// Agent lifecycle (5 variants, 1 card)
	agent_created: AgentLifecycleCard as unknown as CardComponent,
	agent_updated: AgentLifecycleCard as unknown as CardComponent,
	agent_deleted: AgentLifecycleCard as unknown as CardComponent,
	agent_paused: AgentLifecycleCard as unknown as CardComponent,
	agent_resumed: AgentLifecycleCard as unknown as CardComponent,
	// Memory / feedback / tier / health / publication
	memory_report: MemoryReportCard as unknown as CardComponent,
	feedback_generated: FeedbackCard as unknown as CardComponent,
	tier_consolidated: TierConsolidationCard as unknown as CardComponent,
	feed_stalled: FeedStalledCard as unknown as CardComponent,
	published_surface_changed: PublishedSurfaceCard as unknown as CardComponent,
};

/** Resolve the card component for a given event kind. */
export function getCardForKind(kind: AgentUpdateKindTag): CardComponent {
	return registry[kind] ?? (UnknownCard as unknown as CardComponent);
}

/** The set of kinds with a dedicated (non-UnknownCard) renderer. */
export function registeredKinds(): AgentUpdateKindTag[] {
	return Object.keys(registry) as AgentUpdateKindTag[];
}

/** True when `kind` has a dedicated renderer (not UnknownCard). */
export function hasDedicatedCard(kind: AgentUpdateKindTag): boolean {
	return kind in registry;
}
