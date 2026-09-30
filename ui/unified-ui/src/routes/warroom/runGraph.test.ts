import { describe, expect, it } from 'vitest';

import type { TapeRow } from './deck';
import {
	createRunGraph,
	seedTaskRun,
	hashAngle,
	inCluster,
	ingestRow,
	nodeLife,
	prune,
	ROOT_ID,
	syncTasks
} from './runGraph';

function row(over: Partial<TapeRow> & { id: number }): TapeRow {
	return {
		event_type: 'ToolCallStarted',
		ts: 1_000_000,
		agent_id: null,
		task_id: null,
		severity: 'info',
		...over
	};
}

describe('growth', () => {
	it('a task frame grows a branch: task node + chained event nodes stepping outward', () => {
		const g = createRunGraph();
		ingestRow(g, row({ id: 1, task_id: 'T1' }), 1_000_000);
		ingestRow(g, row({ id: 2, task_id: 'T1' }), 1_001_000);
		ingestRow(g, row({ id: 3, task_id: 'T1' }), 1_002_000);

		const task = g.nodes.get('task:T1')!;
		const e1 = g.nodes.get('ev:1')!;
		const e2 = g.nodes.get('ev:2')!;
		const e3 = g.nodes.get('ev:3')!;
		expect(task.kind).toBe('task');
		// Chain: task → e1 → e2 → e3, each link stepping outward.
		expect(e1.parentId).toBe('task:T1');
		expect(e2.parentId).toBe('ev:1');
		expect(e3.parentId).toBe('ev:2');
		expect(e2.radius).toBeGreaterThan(e1.radius);
		expect(e3.radius).toBeGreaterThan(e2.radius);
		// The whole limb belongs to the task's cluster.
		expect(e3.clusterId).toBe('T1');
	});

	it('branch growth saturates so a chatty task cannot leave the stage', () => {
		const g = createRunGraph();
		for (let i = 0; i < 60; i += 1) ingestRow(g, row({ id: i, task_id: 'T1' }), 1_000_000 + i);
		const radii = [...g.nodes.values()].filter((n) => n.kind === 'event').map((n) => n.radius);
		expect(Math.max(...radii)).toBeLessThanOrEqual(0.92);
	});

	it('agent frames hang off a shared agent node; anonymous frames are ambient sparks', () => {
		const g = createRunGraph();
		ingestRow(g, row({ id: 1, agent_id: 'A' }), 1_000_000);
		ingestRow(g, row({ id: 2, agent_id: 'A' }), 1_000_500);
		ingestRow(g, row({ id: 3 }), 1_001_000);
		expect(g.nodes.get('ev:1')!.parentId).toBe('agent:A');
		expect(g.nodes.get('ev:2')!.parentId).toBe('agent:A');
		const spark = g.nodes.get('ev:3')!;
		expect(spark.parentId).toBe(ROOT_ID);
		// Ambient noise forgets faster than task history.
		expect(spark.ttlMs).toBeLessThan(g.nodes.get('ev:1')!.ttlMs);
	});

	it('layout is deterministic — same ids, same angles, no randomness', () => {
		expect(hashAngle('T1')).toBe(hashAngle('T1'));
		expect(hashAngle('T1')).not.toBe(hashAngle('T2'));
		const a = createRunGraph();
		const b = createRunGraph();
		ingestRow(a, row({ id: 1, task_id: 'T1' }), 1_000_000);
		ingestRow(b, row({ id: 1, task_id: 'T1' }), 1_000_000);
		expect(a.nodes.get('ev:1')!.angle).toBe(b.nodes.get('ev:1')!.angle);
	});

	it('duplicate frame ids are ignored', () => {
		const g = createRunGraph();
		ingestRow(g, row({ id: 1, task_id: 'T1' }), 1_000_000);
		ingestRow(g, row({ id: 1, task_id: 'T1' }), 1_000_001);
		expect([...g.nodes.values()].filter((n) => n.kind === 'event')).toHaveLength(1);
	});
});

describe('task store sync', () => {
	it('a running task gets a labelled cluster before any event arrives', () => {
		const g = createRunGraph();
		syncTasks(g, [{ id: 'T9', title: 'Summarize the report', status: 'running' }], 1_000_000);
		const node = g.nodes.get('task:T9')!;
		expect(node.label).toBe('Summarize the report');
		expect(nodeLife(node, 1_000_000)).toBe(1);
	});

	it('settled tasks are not added', () => {
		const g = createRunGraph();
		syncTasks(g, [{ id: 'T9', status: 'completed' }], 1_000_000);
		expect(g.nodes.has('task:T9')).toBe(false);
	});
});

describe('decay and pruning', () => {
	it('nodes age out and pruning removes them', () => {
		const g = createRunGraph();
		ingestRow(g, row({ id: 1 }), 1_000_000);
		expect(nodeLife(g.nodes.get('ev:1')!, 1_000_000)).toBe(1);
		prune(g, 1_000_000 + 60_000); // past the 30s ambient TTL
		expect(g.nodes.has('ev:1')).toBe(false);
	});

	it('orphans re-root instead of dangling when their parent is pruned', () => {
		const g = createRunGraph();
		// Ambient parent chain: agent events outlive... build task chain then kill middle via cap
		for (let i = 0; i < 200; i += 1) ingestRow(g, row({ id: i, task_id: 'T1' }), 1_000_000 + i);
		prune(g, 1_000_200);
		for (const node of g.nodes.values()) {
			if (node.parentId) expect(g.nodes.has(node.parentId)).toBe(true);
		}
	});

	it('the hard cap holds under a flood', () => {
		const g = createRunGraph();
		for (let i = 0; i < 500; i += 1) ingestRow(g, row({ id: i, task_id: `T${i % 7}` }), 1_000_000 + i);
		prune(g, 1_000_500);
		expect(g.nodes.size).toBeLessThanOrEqual(150);
	});

	it('the root is immortal', () => {
		const g = createRunGraph();
		prune(g, 9_999_999_999);
		expect(g.nodes.has(ROOT_ID)).toBe(true);
	});
});

describe('focus', () => {
	it('cluster membership dims everything outside the chosen task', () => {
		const g = createRunGraph();
		ingestRow(g, row({ id: 1, task_id: 'T1' }), 1_000_000);
		ingestRow(g, row({ id: 2, task_id: 'T2' }), 1_000_000);
		const inT1 = g.nodes.get('ev:1')!;
		const inT2 = g.nodes.get('ev:2')!;
		expect(inCluster(inT1, 'T1')).toBe(true);
		expect(inCluster(inT2, 'T1')).toBe(false);
		expect(inCluster(g.nodes.get(ROOT_ID)!, 'T1')).toBe(true);
		// No focus → everything shows.
		expect(inCluster(inT2, null)).toBe(true);
	});
});

describe('seedTaskRun — inspect ANY task', () => {
	const STEPS = [
		{ id: 's1', description: 'fetch the page', status: 'completed' },
		{ id: 's2', description: 'summarize', status: 'completed', depends_on: ['s1'] },
		{ id: 's3', description: 'post result', status: 'failed', depends_on: ['s2'] }
	];

	it('a settled task shows its outcome on the task node', () => {
		const g = createRunGraph();
		seedTaskRun(g, { id: 'T1', label: 'Run', status: 'completed' }, [], 1_000_000);
		expect(g.nodes.get('task:T1')!.severity).toBe('success');
		seedTaskRun(g, { id: 'T2', label: 'Run', status: 'failed' }, [], 1_000_000);
		expect(g.nodes.get('task:T2')!.severity).toBe('error');
		seedTaskRun(g, { id: 'T3', label: 'Run', status: 'cancelled' }, [], 1_000_000);
		expect(g.nodes.get('task:T3')!.severity).toBe('warn');
	});

	it('multi-step runs grow a labelled, status-coloured branch', () => {
		const g = createRunGraph();
		seedTaskRun(g, { id: 'T1', label: 'Run', status: 'failed' }, STEPS, 1_000_000);
		const s1 = g.nodes.get('step:T1:s1')!;
		const s2 = g.nodes.get('step:T1:s2')!;
		const s3 = g.nodes.get('step:T1:s3')!;
		expect(s1.parentId).toBe('task:T1');
		expect(s2.parentId).toBe('step:T1:s1');
		expect(s3.parentId).toBe('step:T1:s2');
		expect(s2.radius).toBeGreaterThan(s1.radius);
		expect(s1.severity).toBe('success');
		expect(s3.severity).toBe('error');
		expect(s1.label).toBe('fetch the page');
	});

	it('a declared dependency forks the limb instead of chaining plan order', () => {
		const g = createRunGraph();
		seedTaskRun(
			g,
			{ id: 'T1', status: 'completed' },
			[
				{ id: 'a', status: 'completed' },
				{ id: 'b', status: 'completed', depends_on: ['a'] },
				{ id: 'c', status: 'completed', depends_on: ['a'] } // forks off a, not b
			],
			1_000_000
		);
		expect(g.nodes.get('step:T1:c')!.parentId).toBe('step:T1:a');
	});

	it('tasks are their own roots — no edge to the orb', () => {
		const g = createRunGraph();
		seedTaskRun(g, { id: 'T1', status: 'completed' }, [], 1_000_000);
		expect(g.nodes.get('task:T1')!.parentId).toBeNull();
		ingestRow(g, row({ id: 1, task_id: 'T2' }), 1_000_000);
		expect(g.nodes.get('task:T2')!.parentId).toBeNull();
	});

	it('a step with a tool grows an ACTION node naming what it ran and who ran it', () => {
		const g = createRunGraph();
		seedTaskRun(
			g,
			{ id: 'T1', status: 'completed' },
			[{ id: 's1', description: 'search', status: 'completed', tool_name: 'web_search', providing_agent_id: 'researcher' }],
			1_000_000
		);
		const act = g.nodes.get('act:T1:s1')!;
		expect(act.parentId).toBe('step:T1:s1');
		expect(act.label).toBe('web_search · researcher');
		expect(act.radius).toBeGreaterThan(g.nodes.get('step:T1:s1')!.radius);
	});

	it('re-seeding updates statuses in place without duplicating nodes', () => {
		const g = createRunGraph();
		seedTaskRun(g, { id: 'T1', status: 'running' }, [{ id: 's1', status: 'in_progress' }], 1_000_000);
		expect(g.nodes.get('step:T1:s1')!.severity).toBe('info');
		seedTaskRun(g, { id: 'T1', status: 'completed' }, [{ id: 's1', status: 'completed' }], 1_001_000);
		expect(g.nodes.get('step:T1:s1')!.severity).toBe('success');
		expect(g.nodes.get('task:T1')!.severity).toBe('success');
		expect([...g.nodes.keys()].filter((k) => k.startsWith('step:T1:'))).toHaveLength(1);
	});

	it('live events arriving after a seed continue the limb past the last step', () => {
		const g = createRunGraph();
		seedTaskRun(g, { id: 'T1', status: 'running' }, STEPS, 1_000_000);
		ingestRow(g, row({ id: 99, task_id: 'T1' }), 1_001_000);
		expect(g.nodes.get('ev:99')!.parentId).toBe('step:T1:s3');
	});
});
