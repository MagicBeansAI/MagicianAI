/**
 * RUN GRAPH — the deck's living execution tree.
 *
 * A radial graph grown from REAL telemetry: the core at the centre, task
 * clusters branching outward, every event a node chained to the previous
 * event of its task — so a running task literally grows a limb in front of
 * you, and an idle system is a bare ring. Nothing is synthesised: a node
 * exists because a frame arrived on the uplink or a task exists in the
 * task store.
 *
 * This module is the pure model + layout: deterministic, framework-free,
 * canvas-free, so vitest covers the growth rules without a browser. The
 * renderer (`StageGraph.svelte`) only interpolates toward the positions
 * computed here and paints.
 *
 * Design constraints:
 * - DETERMINISTIC layout: a node's home angle comes from a hash of its id,
 *   never from Math.random(), so re-renders and tests reproduce exactly.
 * - BOUNDED: hard node cap with oldest-dead-first pruning — the graph can
 *   run for days without growing without bound.
 * - DECAYING: event nodes age out (TTL by kind); the graph forgets, the
 *   way the tape's blip forgets. History lives in /events, not here.
 */

import type { TapeRow } from './deck';

export type GraphNodeKind = 'root' | 'task' | 'agent' | 'event';

export interface GraphNode {
	id: string;
	kind: GraphNodeKind;
	parentId: string | null;
	/** Task cluster this node belongs to (task/its events); null for ambient. */
	clusterId: string | null;
	label: string | null;
	severity: TapeRow['severity'] | null;
	/** Polar home, relative to the core. The renderer eases toward this. */
	angle: number;
	radius: number;
	bornMs: number;
	lastSeenMs: number;
	/** Time-to-live from lastSeen; Infinity = pinned while its task lives. */
	ttlMs: number;
}

export interface RunGraphState {
	nodes: Map<string, GraphNode>;
	/** Newest event node per task, so the next event chains to it. */
	chainTips: Map<string, string>;
	/** Per-task grown branch length (events so far), drives branch radius. */
	branchLen: Map<string, number>;
}

export const ROOT_ID = '__core';

const MAX_NODES = 150;
const EVENT_TTL_MS = 100_000;
const AMBIENT_TTL_MS = 30_000;
const TASK_TTL_MS = 150_000;
const AGENT_TTL_MS = 140_000;

// Radial geometry, in unit space (renderer multiplies by stage size).
const TASK_RING = 0.42;
const AGENT_RING = 0.34;
const AMBIENT_RING = 0.3;
const BRANCH_STEP = 0.055;
const BRANCH_MAX = 0.5;

/** FNV-1a — tiny, deterministic, good-enough angular spread. */
export function hashAngle(id: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < id.length; i += 1) {
		h ^= id.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return ((h >>> 0) / 0xffffffff) * Math.PI * 2;
}

export function createRunGraph(): RunGraphState {
	const nodes = new Map<string, GraphNode>();
	nodes.set(ROOT_ID, {
		id: ROOT_ID,
		kind: 'root',
		parentId: null,
		clusterId: null,
		label: null,
		severity: null,
		angle: 0,
		radius: 0,
		bornMs: 0,
		lastSeenMs: 0,
		ttlMs: Infinity
	});
	return { nodes, chainTips: new Map(), branchLen: new Map() };
}

function ensureTaskNode(g: RunGraphState, taskId: string, nowMs: number, label?: string | null): GraphNode {
	const id = `task:${taskId}`;
	let node = g.nodes.get(id);
	if (!node) {
		node = {
			id,
			kind: 'task',
			// A task is its own root. The orb is the deck's instrument, not a
			// data node — drawing task→core edges implied a relationship that
			// does not exist.
			parentId: null,
			clusterId: taskId,
			label: label ?? taskId.slice(0, 18),
			severity: null,
			angle: hashAngle(taskId),
			radius: TASK_RING,
			bornMs: nowMs,
			lastSeenMs: nowMs,
			ttlMs: TASK_TTL_MS
		};
		g.nodes.set(id, node);
	} else {
		node.lastSeenMs = nowMs;
		if (label) node.label = label;
	}
	return node;
}

function ensureAgentNode(g: RunGraphState, agentId: string, nowMs: number): GraphNode {
	const id = `agent:${agentId}`;
	let node = g.nodes.get(id);
	if (!node) {
		node = {
			id,
			kind: 'agent',
			parentId: ROOT_ID,
			clusterId: null,
			label: agentId.slice(0, 16),
			severity: null,
			angle: hashAngle(agentId),
			radius: AGENT_RING,
			bornMs: nowMs,
			lastSeenMs: nowMs,
			ttlMs: AGENT_TTL_MS
		};
		g.nodes.set(id, node);
	} else {
		node.lastSeenMs = nowMs;
	}
	return node;
}

/**
 * Feed one real uplink frame into the graph.
 *
 * Growth rules:
 * - a frame with `task_id` grows that task's BRANCH: the new event node
 *   chains to the task's previous event (or the task node itself), each
 *   link stepping outward — a running task visibly extends;
 * - a frame with only `agent_id` pulses that agent's node and hangs the
 *   event off it;
 * - anonymous frames are ambient sparks on the inner ring, short-lived.
 */
export function ingestRow(g: RunGraphState, row: TapeRow, nowMs: number): void {
	const eventId = `ev:${row.id}`;
	if (g.nodes.has(eventId)) return;

	let parent: GraphNode;
	let clusterId: string | null = null;
	let angle: number;
	let radius: number;
	let ttl = EVENT_TTL_MS;

	if (row.task_id) {
		const task = ensureTaskNode(g, row.task_id, nowMs);
		clusterId = row.task_id;
		const tip = g.chainTips.get(row.task_id);
		parent = (tip && g.nodes.get(tip)) || task;
		const len = (g.branchLen.get(row.task_id) ?? 0) + 1;
		g.branchLen.set(row.task_id, len);
		// The branch steps outward per event and bends deterministically —
		// a limb, not a straight spoke. Growth saturates at BRANCH_MAX so a
		// chatty task cannot leave the stage.
		radius = Math.min(BRANCH_MAX + TASK_RING, task.radius + Math.min(len * BRANCH_STEP, BRANCH_MAX));
		angle = task.angle + Math.sin(len * 1.7 + hashAngle(row.event_type)) * 0.22;
		g.chainTips.set(row.task_id, eventId);
		// Activity keeps the whole cluster alive.
		task.lastSeenMs = nowMs;
	} else if (row.agent_id) {
		const agent = ensureAgentNode(g, row.agent_id, nowMs);
		parent = agent;
		angle = agent.angle + Math.sin(hashAngle(row.event_type + row.id)) * 0.35;
		radius = agent.radius + BRANCH_STEP * 1.4;
	} else {
		parent = g.nodes.get(ROOT_ID)!;
		angle = hashAngle(String(row.id) + row.event_type);
		radius = AMBIENT_RING;
		ttl = AMBIENT_TTL_MS;
	}

	g.nodes.set(eventId, {
		id: eventId,
		kind: 'event',
		parentId: parent.id,
		clusterId,
		label: null,
		severity: row.severity,
		angle,
		radius,
		bornMs: nowMs,
		lastSeenMs: row.ts > 0 ? row.ts : nowMs,
		ttlMs: ttl
	});
}

/**
 * Mirror live tasks from the task store, so a running task has a cluster
 * even before (or without) events, pinned alive while it runs.
 */
export function syncTasks(
	g: RunGraphState,
	tasks: Array<{ id: string; title?: string | null; description?: string | null; status: string }>,
	nowMs: number
): void {
	for (const t of tasks) {
		if (t.status !== 'running' && t.status !== 'paused') continue;
		const label = (t.title || t.description || t.id).slice(0, 22);
		const node = ensureTaskNode(g, t.id, nowMs, label);
		// Pinned while live; prune() restores mortality when it stops syncing.
		node.ttlMs = TASK_TTL_MS;
		node.lastSeenMs = nowMs;
	}
}

/** Age → [0,1] life remaining. 0 = expired. */
export function nodeLife(node: GraphNode, nowMs: number): number {
	if (!Number.isFinite(node.ttlMs)) return 1;
	const age = nowMs - node.lastSeenMs;
	if (age <= 0) return 1;
	return Math.max(0, 1 - age / node.ttlMs);
}

/**
 * Forget: drop expired nodes (children of an expired parent re-root to the
 * task/agent/core so the graph never dangles), clear dead chain tips, and
 * enforce the hard cap oldest-first.
 */
export function prune(g: RunGraphState, nowMs: number): void {
	const dead: string[] = [];
	for (const node of g.nodes.values()) {
		if (node.kind === 'root') continue;
		if (nodeLife(node, nowMs) === 0) dead.push(node.id);
	}
	for (const id of dead) g.nodes.delete(id);

	if (g.nodes.size > MAX_NODES) {
		const mortals = [...g.nodes.values()]
			.filter((n) => n.kind === 'event')
			.sort((a, b) => a.lastSeenMs - b.lastSeenMs);
		for (const n of mortals.slice(0, g.nodes.size - MAX_NODES)) g.nodes.delete(n.id);
	}

	// Heal parent links + chain tips that pointed at pruned nodes.
	for (const node of g.nodes.values()) {
		if (node.parentId && !g.nodes.has(node.parentId)) {
			node.parentId = node.clusterId && g.nodes.has(`task:${node.clusterId}`) ? `task:${node.clusterId}` : ROOT_ID;
		}
	}
	for (const [taskId, tip] of g.chainTips) {
		if (!g.nodes.has(tip)) g.chainTips.delete(taskId);
		if (!g.nodes.has(`task:${taskId}`)) {
			g.chainTips.delete(taskId);
			g.branchLen.delete(taskId);
		}
	}
}

/** A plan/execution step, as the execution-panel endpoint reports it. */
export interface SeedStep {
	id: string;
	description?: string | null;
	status: string;
	depends_on?: string[];
	/** Capability/tool the step ran — rendered as an action node. */
	tool_name?: string | null;
	/** Delegate agent that carried the step, when delegated. */
	providing_agent_id?: string | null;
}

function stepSeverity(status: string): TapeRow['severity'] | null {
	switch (status) {
		case 'completed':
			return 'success';
		case 'failed':
			return 'error';
		case 'in_progress':
			return 'info';
		default:
			return null; // pending / skipped / cancelled render dim
	}
}

function taskOutcomeSeverity(status: string): TapeRow['severity'] | null {
	switch (status) {
		case 'completed':
			return 'success';
		case 'failed':
			return 'error';
		case 'cancelled':
			return 'warn';
		default:
			return null;
	}
}

/**
 * Materialise a task's RUN — outcome + plan steps — into the graph, so ANY
 * task can be inspected, not just ones whose events happened to flow while
 * the deck was open.
 *
 * - The task node takes its OUTCOME severity (completed → green ring,
 *   failed → red, cancelled → amber; live → neutral glow).
 * - Steps become a chained branch stepping outward in plan order; a step
 *   with `depends_on` hangs off its dependency when that step exists, so
 *   forked plans render as forked limbs.
 * - Idempotent: re-seeding updates status colours in place, never
 *   duplicates. Chain bookkeeping is updated so LIVE events arriving after
 *   a seed continue the branch outward.
 */
export function seedTaskRun(
	g: RunGraphState,
	task: { id: string; label?: string | null; status: string },
	steps: SeedStep[],
	nowMs: number
): void {
	const node = ensureTaskNode(g, task.id, nowMs, task.label ?? null);
	node.severity = taskOutcomeSeverity(task.status);
	node.lastSeenMs = nowMs;

	let prevId = `task:${task.id}`;
	steps.forEach((step, i) => {
		const id = `step:${task.id}:${step.id}`;
		const existing = g.nodes.get(id);
		if (existing) {
			existing.severity = stepSeverity(step.status);
			existing.lastSeenMs = nowMs;
			if (step.description) existing.label = step.description.slice(0, 24);
			prevId = id;
			return;
		}
		// A declared dependency wins over plan order — forks render as forks.
		const dep = (step.depends_on ?? [])
			.map((d) => `step:${task.id}:${d}`)
			.find((d) => g.nodes.has(d));
		const parentId = dep ?? prevId;
		const angle = node.angle + Math.sin(i * 1.7 + hashAngle(step.id)) * 0.26;
		const radius = Math.min(
			TASK_RING + BRANCH_MAX,
			node.radius + Math.min((i + 1) * BRANCH_STEP, BRANCH_MAX)
		);
		g.nodes.set(id, {
			id,
			kind: 'event',
			parentId,
			clusterId: task.id,
			label: (step.description ?? step.id).slice(0, 24),
			severity: stepSeverity(step.status),
			angle,
			radius,
			bornMs: nowMs,
			lastSeenMs: nowMs,
			ttlMs: TASK_TTL_MS
		});
		// The step's ACTION — the capability/tool it ran (and who ran it) —
		// hangs off the step as its own node, so a run reads as
		// step → what it actually did.
		const action = step.tool_name?.trim();
		if (action) {
			const who = step.providing_agent_id?.trim();
			g.nodes.set(`act:${task.id}:${step.id}`, {
				id: `act:${task.id}:${step.id}`,
				kind: 'event',
				parentId: id,
				clusterId: task.id,
				label: (who ? `${action} · ${who}` : action).slice(0, 26),
				severity: stepSeverity(step.status),
				angle: angle + 0.16,
				radius: radius + BRANCH_STEP * 0.8,
				bornMs: nowMs,
				lastSeenMs: nowMs,
				ttlMs: TASK_TTL_MS
			});
		}
		prevId = id;
	});

	if (steps.length > 0) {
		// Live events that arrive later continue the limb past the last step.
		g.chainTips.set(task.id, prevId);
		g.branchLen.set(task.id, Math.max(g.branchLen.get(task.id) ?? 0, steps.length));
	}
}

/** Cluster membership test used by focus dimming. */
export function inCluster(node: GraphNode, taskId: string | null): boolean {
	if (!taskId) return true;
	return node.clusterId === taskId || node.id === `task:${taskId}` || node.kind === 'root';
}
