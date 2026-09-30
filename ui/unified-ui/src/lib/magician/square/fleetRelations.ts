import { get } from 'svelte/store';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

/**
 * The RELATIONSHIP GRAPH: who summons whom, how often, and how it goes.
 * Aggregated client-side from recent v3 tasks' execution trees (the same
 * substrate the live hand-off beams read, but across ALL recent tasks and
 * with per-edge outcome counts). Best-effort with a 2-minute cache.
 */

export interface RelationEdge {
	from: string;
	to: string;
	/** Total summons observed (child executions under a different agent). */
	count: number;
	/** Child executions that completed. */
	ok: number;
	/** Child executions that failed / were cancelled. */
	fail: number;
}

export interface FleetRelations {
	edges: RelationEdge[];
	/** agent -> summons RECEIVED (how requested the agent is). */
	inbound: Map<string, number>;
	/** agent -> summons ISSUED (how demanding the agent is). */
	outbound: Map<string, number>;
	/** Tasks whose trees were aggregated (coverage honesty). */
	tasksScanned: number;
}

interface TreeNode {
	execution_id: string;
	parent_execution_id?: string | null;
	agent_id?: string;
	status?: string;
}

const TASK_LIMIT = 40;
const TREE_LIMIT = 20;
const CACHE_MS = 120_000;

let cache: { at: number; value: FleetRelations | null } | null = null;
let inflight: Promise<FleetRelations | null> | null = null;

function outcome(status: string | undefined): 'ok' | 'fail' | 'other' {
	const s = (status ?? '').toLowerCase();
	if (s.includes('complet') || s.includes('succe')) return 'ok';
	if (s.includes('fail') || s.includes('cancel') || s.includes('error')) return 'fail';
	return 'other';
}

async function compute(): Promise<FleetRelations | null> {
	const scope = get(scopeIdentityStore);
	if (!scope.isResolved) return null;
	try {
		const params = new URLSearchParams();
		const res = await timedFetch(`/api/magician/v3/tasks?${params.toString()}`);
		if (!res.ok) return null;
		const body = (await res.json()) as { tasks?: { id?: string }[] };
		const ids = (body.tasks ?? [])
			.map((t) => t.id)
			.filter((id): id is string => typeof id === 'string')
			.slice(0, TREE_LIMIT);

		const trees = await Promise.all(
			ids.map(async (id) => {
				const p = new URLSearchParams();
				const r = await timedFetch(
					`/api/magician/v3/tasks/${encodeURIComponent(id)}/execution-tree?${p.toString()}`
				);
				if (!r.ok) return null;
				const b = (await r.json()) as { tree?: { nodes?: TreeNode[] } };
				return b.tree?.nodes ?? null;
			})
		);

		const edgeMap = new Map<string, RelationEdge>();
		const inbound = new Map<string, number>();
		const outbound = new Map<string, number>();
		for (const nodes of trees) {
			if (!nodes) continue;
			const byId = new Map(nodes.map((n) => [n.execution_id, n]));
			for (const node of nodes) {
				const parent = node.parent_execution_id ? byId.get(node.parent_execution_id) : undefined;
				if (!parent?.agent_id || !node.agent_id || parent.agent_id === node.agent_id) continue;
				const key = `${parent.agent_id}->${node.agent_id}`;
				let edge = edgeMap.get(key);
				if (!edge) {
					edge = { from: parent.agent_id, to: node.agent_id, count: 0, ok: 0, fail: 0 };
					edgeMap.set(key, edge);
				}
				edge.count += 1;
				const o = outcome(node.status);
				if (o === 'ok') edge.ok += 1;
				else if (o === 'fail') edge.fail += 1;
				inbound.set(node.agent_id, (inbound.get(node.agent_id) ?? 0) + 1);
				outbound.set(parent.agent_id, (outbound.get(parent.agent_id) ?? 0) + 1);
			}
		}
		const edges = Array.from(edgeMap.values()).sort((a, b) => b.count - a.count);
		return { edges, inbound, outbound, tasksScanned: trees.filter(Boolean).length };
	} catch {
		return null;
	}
}

export async function fetchFleetRelations(): Promise<FleetRelations | null> {
	if (cache && Date.now() - cache.at < CACHE_MS) return cache.value;
	if (inflight) return inflight;
	inflight = compute().then((value) => {
		cache = { at: Date.now(), value };
		inflight = null;
		return value;
	});
	return inflight;
}
