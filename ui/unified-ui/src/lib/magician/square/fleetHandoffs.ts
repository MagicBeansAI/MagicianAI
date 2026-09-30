import { get } from 'svelte/store';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';
import type { HandoffEdge } from './engine/handoffs';

/**
 * Live delegation edges for the hand-off beams: the same substrate the old
 * town-square coordination lane used — active /v3/tasks with root executions,
 * each task's /execution-tree, an edge wherever a child's agent differs from
 * its parent's and the child is still active. Best-effort.
 */

export interface HandoffTaskListItem {
	id: string;
	status?: string;
	active_root_execution_id?: string | null;
	latest_root_execution_id?: string | null;
	last_completed_root_execution_id?: string | null;
}

export interface HandoffTreeTarget {
	taskId: string;
	rootExecutionId: string;
}

interface TreeNode {
	execution_id: string;
	parent_execution_id?: string | null;
	root_execution_id?: string | null;
	agent_id?: string;
	status?: string;
}

interface ExecutionTree {
	active_root_execution_id?: string | null;
	root_execution_id?: string | null;
	nodes?: TreeNode[];
}

function nonEmpty(value: string | null | undefined): string | null {
	const normalized = value?.trim();
	return normalized ? normalized : null;
}

function rootExecutionIdOf(task: HandoffTaskListItem): string | null {
	return nonEmpty(task.active_root_execution_id);
}

export function selectHandoffTrees(tasks: readonly HandoffTaskListItem[]): HandoffTreeTarget[] {
	const targets: HandoffTreeTarget[] = [];
	for (const task of tasks) {
		const taskId = nonEmpty(task.id);
		const rootExecutionId = rootExecutionIdOf(task);
		if (
			taskId &&
			rootExecutionId &&
			['running', 'planning', 'paused'].includes((task.status ?? '').trim().toLowerCase())
		) {
			targets.push({ taskId, rootExecutionId });
		}
	}
	return targets;
}

function isActiveStatus(status: string | undefined): boolean {
	const s = (status ?? '').trim().toLowerCase();
	if (!s) return false;
	return !['completed', 'failed', 'cancelled', 'canceled'].includes(s);
}

function activeRootNodes(tree: ExecutionTree, requestedRootId: string): TreeNode[] {
	const nodes = tree.nodes ?? [];
	const rootId =
		nonEmpty(tree.active_root_execution_id) ??
		nonEmpty(tree.root_execution_id) ??
		requestedRootId;
	const includedIds = new Set([rootId]);
	let changed = true;
	while (changed) {
		changed = false;
		for (const node of nodes) {
			if (
				!includedIds.has(node.execution_id) &&
				(node.root_execution_id === rootId ||
					(node.parent_execution_id ? includedIds.has(node.parent_execution_id) : false))
			) {
				includedIds.add(node.execution_id);
				changed = true;
			}
		}
	}
	return nodes.filter((node) => includedIds.has(node.execution_id));
}

export async function fetchHandoffEdges(): Promise<HandoffEdge[] | null> {
	const scope = get(scopeIdentityStore);
	if (!scope.isResolved) return null;
	try {
		const params = new URLSearchParams();
		const res = await timedFetch(`/api/magician/v3/tasks?${params.toString()}`);
		if (!res.ok) return null;
		const body = (await res.json()) as { tasks?: HandoffTaskListItem[] };
		const targets = selectHandoffTrees(body.tasks ?? []);
		if (targets.length === 0) return [];

		const trees = await Promise.all(
			targets.map(async (target) => {
				const p = new URLSearchParams();
				const r = await timedFetch(
					`/api/magician/v3/tasks/${encodeURIComponent(target.taskId)}/execution-tree?${p.toString()}`
				);
				if (!r.ok) return null;
				const b = (await r.json()) as { tree?: ExecutionTree };
				return b.tree ? activeRootNodes(b.tree, target.rootExecutionId) : [];
			})
		);

		const byPair = new Map<string, HandoffEdge>();
		for (const nodes of trees) {
			if (!nodes) continue;
			const byId = new Map(nodes.map((n) => [n.execution_id, n]));
			for (const node of nodes) {
				if (!isActiveStatus(node.status)) continue;
				const parent = node.parent_execution_id ? byId.get(node.parent_execution_id) : undefined;
				if (parent?.agent_id && node.agent_id && parent.agent_id !== node.agent_id) {
					const key = `${parent.agent_id}->${node.agent_id}`;
					if (!byPair.has(key)) {
						byPair.set(key, { key, fromAgent: parent.agent_id, toAgent: node.agent_id });
					}
				}
			}
		}
		return Array.from(byPair.values());
	} catch {
		return null;
	}
}
