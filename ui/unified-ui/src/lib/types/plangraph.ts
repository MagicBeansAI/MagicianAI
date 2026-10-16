//! TypeScript type definitions for PlanGraph structures
//! Matches the Rust backend types in src/magician_v2/strategy/plan.rs

/**
 * Complete planning graph emitted by a strategy
 */
export interface PlanGraph {
	/** Ordered list of steps that constitute the plan */
	steps: PlanStep[];
	/** Directed edges describing dependencies between steps */
	edges: PlanEdge[];
	/** Unresolved inputs that require clarification from the user */
	unresolved_inputs: UnresolvedInput[];
	/** Overall confidence assigned by the strategy */
	confidence: number;
	/** Provenance information for audit/debugging */
	provenance: PlanProvenance;
}

/**
 * Single planned step referencing a tool invocation
 */
export interface PlanStep {
	/** Stable identifier for the step (usually matches exploration node id) */
	id: string;
	/** Natural language description of the task */
	task: string;
	/** Tool the step intends to execute, if known */
	tool: string | null;
	/** Parameters already resolved for the tool */
	parameters: Record<string, string>;
	/** Expected outputs or side effects (best-effort) */
	expected_outputs: string[];
	/** Confidence assigned to this particular step */
	confidence: number;
	/** Additional metadata (strategy specific hints, reasoning, etc.) */
	metadata: Record<string, string>;
	/** Agent that will provide / execute this step (set when delegated to another agent) */
	providing_agent_id?: string;
	/** Content-addressable hash of the step for deduplication / caching */
	step_hash?: string;
}

/**
 * Directed dependency between two steps
 */
export interface PlanEdge {
	/** Source step identifier */
	from: string;
	/** Target step identifier */
	to: string;
	/** Human readable explanation of the dependency */
	reason: string;
}

/**
 * Description of an unresolved input required to execute the plan
 */
export interface InputHole {
	/** Identifier of the step that requires this input */
	step_id: string | null;
	/** Logical name of the missing parameter */
	parameter: string;
	/** Expected datatype or shape of the parameter */
	expected_type: string | null;
	/** Guidance for the elicitation engine when asking the user */
	prompt: string | null;
	/** Optional additional notes */
	notes: string | null;
}

/**
 * Priority levels for unresolved inputs
 */
export type InputPriority = 'critical' | 'pre_execution' | 'just_in_time' | 'optional' | 'inferrable';

/**
 * When to ask for the input
 */
export type AskTiming = 'Upfront' | 'JustInTime';

/**
 * Status of input resolution
 */
export type InputStatus = 'pending' | 'resolved' | 'skipped' | 'auto_filled';

/**
 * Extended unresolved input with UI-specific fields
 * Used by the progressive elicitation UI components
 */
export interface UnresolvedInput extends InputHole {
	/** Unique identifier for this input (e.g., "slot_github_repo_url") */
	id?: string;
	/** Display name for the UI */
	display_name?: string;
	/** Description of the input for display */
	description?: string;
	/** Whether this input is required */
	required?: boolean;
	/** Priority level for resolution ordering */
	priority?: InputPriority;
	/** When to ask for this input */
	ask_timing?: AskTiming;
	/** Whether auto-fill was attempted */
	auto_fill?: boolean;
	/** Source of auto-filled value */
	source?: string;
	/** Confidence of auto-filled value (0-1) */
	auto_fill_confidence?: number;
	/** Current status of the input */
	status?: InputStatus;
	/** Steps that depend on this input */
	linked_steps?: string[];
}

/**
 * Provenance / audit metadata for the generated plan
 */
export interface PlanProvenance {
	/** Strategy that produced the plan (e.g., "MCTS", "AtomicComposition") */
	strategy: string;
	/** Model or algorithm responsible for the reasoning */
	generator: string | null;
	/** Additional notes (prompt hashes, retries, etc.) */
	notes: string | null;
}

/**
 * Helper type guards and utilities
 */

/**
 * Check if a plan graph has unresolved inputs
 */
export function hasUnresolvedInputs(plan: PlanGraph): boolean {
	return plan.unresolved_inputs.length > 0;
}

/**
 * Check if a plan step has a resolved tool
 */
export function hasResolvedTool(step: PlanStep): boolean {
	return step.tool !== null;
}

/**
 * Get required unresolved inputs (based on notes or prompt presence)
 */
export function getRequiredInputHoles(holes: InputHole[]): InputHole[] {
	return holes.filter(
		(hole) =>
			hole.notes?.toLowerCase().includes('required') ||
			(!hole.notes && hole.prompt !== null)
	);
}

/**
 * Get optional unresolved inputs
 */
export function getOptionalInputHoles(holes: InputHole[]): InputHole[] {
	return holes.filter(
		(hole) =>
			hole.notes?.toLowerCase().includes('optional') ||
			(hole.notes && !hole.notes.toLowerCase().includes('required') && hole.prompt !== null)
	);
}

/**
 * Group input holes by step
 */
export function groupInputHolesByStep(holes: InputHole[]): Map<string, InputHole[]> {
	const grouped = new Map<string, InputHole[]>();

	for (const hole of holes) {
		const stepId = hole.step_id || 'unknown';
		if (!grouped.has(stepId)) {
			grouped.set(stepId, []);
		}
		grouped.get(stepId)!.push(hole);
	}

	return grouped;
}

/**
 * Get step by ID from plan
 */
export function getStepById(plan: PlanGraph, stepId: string): PlanStep | null {
	return plan.steps.find((step) => step.id === stepId) || null;
}

/**
 * Get dependencies for a step (incoming edges)
 */
export function getStepDependencies(plan: PlanGraph, stepId: string): PlanEdge[] {
	return plan.edges.filter((edge) => edge.to === stepId);
}

/**
 * Get dependents of a step (outgoing edges)
 */
export function getStepDependents(plan: PlanGraph, stepId: string): PlanEdge[] {
	return plan.edges.filter((edge) => edge.from === stepId);
}

/**
 * Check if plan is a DAG (has no cycles)
 * Simple cycle detection using DFS
 */
export function isPlanAcyclic(plan: PlanGraph): boolean {
	const visited = new Set<string>();
	const recursionStack = new Set<string>();

	function hasCycleDFS(nodeId: string): boolean {
		if (recursionStack.has(nodeId)) return true; // Cycle detected
		if (visited.has(nodeId)) return false; // Already checked this path

		visited.add(nodeId);
		recursionStack.add(nodeId);

		const dependents = getStepDependents(plan, nodeId);
		for (const edge of dependents) {
			if (hasCycleDFS(edge.to)) {
				return true;
			}
		}

		recursionStack.delete(nodeId);
		return false;
	}

	// Check all steps as potential starting points
	for (const step of plan.steps) {
		if (hasCycleDFS(step.id)) {
			return false;
		}
	}

	return true;
}
