// Fixture events covering every AgentUpdateKind variant.
//
// Used by the registry test and any future preview route. Order is stable —
// tests can rely on index-based access.

import type { AgentUpdate, AgentUpdateKind } from '$lib/types/agentUpdate';

const BASE_TS = 1_745_432_100_123;

function env(body: AgentUpdateKind, idx: number): AgentUpdate {
	return {
		id: `01HXTEST${idx.toString().padStart(18, '0')}`,
		ts: BASE_TS + idx,
		workspace_id: 'ws_a',
		agent_id: 'cfo',
		thread_id: idx % 3 === 0 ? 'th_1' : undefined,
		cycle_id: idx % 2 === 0 ? 'cyc_42' : undefined,
		...body
	};
}

export const fixtureEvents: AgentUpdate[] = [
	env({ kind: 'agent_created', name: 'CFO', agent_kind: 'Personal' }, 0),
	env({ kind: 'agent_updated', changed_fields: ['persona'] }, 1),
	env({ kind: 'agent_deleted' }, 2),
	env({ kind: 'agent_paused', reason: 'manual' }, 3),
	env({ kind: 'agent_resumed' }, 4),
	env({ kind: 'cycle_started', focus_area: 'daily_review', trigger: 'cron' }, 5),
	env({ kind: 'cycle_completed', outcome: 'succeeded', duration_ms: 42_300 }, 6),
	env({ kind: 'cycle_failed', error: 'tool timeout', duration_ms: 8_500 }, 7),
	env({ kind: 'cycle_paused', reason: 'budget exhausted' }, 8),
	env({ kind: 'goal_completed', goal_id: 'g1', artifacts: [] }, 9),
	env({ kind: 'goal_failed', goal_id: 'g1', error: 'unreachable' }, 10),
	env({ kind: 'goal_recovered', goal_id: 'g1', recovery_strategy: 'retry' }, 11),
	env(
		{
			kind: 'approval_requested',
			approval_id: 'ap_1',
			tool: 'fs.write',
			action: 'create',
			params: { path: '/tmp/x' },
			pending_action_count: 1
		},
		12
	),
	env(
		{
			kind: 'approval_resolved',
			approval_id: 'ap_1',
			decision: 'approve',
			resolver: 'user:alice'
		},
		13
	),
	env({ kind: 'approval_expired', approval_id: 'ap_1' }, 14),
	env(
		{
			kind: 'artifact_created',
			artifact: {
				artifact_id: 'art_1',
				kind: 'persistent',
				surface_url: '/surface/123',
				label: 'Q1 Report'
			}
		},
		15
	),
	env({ kind: 'artifact_create_failed', attempted_kind: 'persistent', error: 'disk full' }, 16),
	env(
		{
			kind: 'circuit_opened',
			scope: 'tool.fs.write',
			reason: 'too_many_failures',
			next_retry_at: BASE_TS + 60_000
		},
		17
	),
	env({ kind: 'circuit_recovered', scope: 'tool.fs.write', recovered_at: BASE_TS + 90_000 }, 18),
	env({ kind: 'task_created', task_id: 't_1', title: 'Draft report' }, 19),
	env({ kind: 'task_updated', task_id: 't_1', changed_fields: ['status'] }, 20),
	env({ kind: 'task_completed', task_id: 't_1', artifacts: [] }, 21),
	env({ kind: 'task_failed', task_id: 't_1', error: 'timeout' }, 22),
	env(
		{
			kind: 'memory_report',
			summary: '10 episodes processed',
			episodes_processed: 10,
			corrections_applied: 2
		},
		23
	),
	env({ kind: 'feedback_generated', corrections: 3, success_patterns: 7 }, 24),
	env(
		{
			kind: 'tier_consolidated',
			source: 'episodes',
			target: 'knowledge',
			records_written: 5,
			rule: 'weekly_summary'
		},
		25
	),
	env(
		{ kind: 'delegation_issued', from_agent: 'cfo', to_agent: 'analyst', task_id: 't_1' },
		26
	),
	env(
		{
			kind: 'delegation_resolved',
			from_agent: 'cfo',
			to_agent: 'analyst',
			task_id: 't_1',
			outcome: 'completed'
		},
		27
	),
	env({ kind: 'feed_stalled', silent_for_ms: 600_000 }, 28),
	env({ kind: 'published_surface_changed', surface_id: 'surf_42', url: '/surface/42' }, 29)
];
