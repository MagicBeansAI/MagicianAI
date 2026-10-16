import { describe, expect, it } from 'vitest';
import {
	effectiveToolCount,
	shortPolicyId,
	type EffectiveToolPolicyPreview
} from './effectiveTools';

function preview(): EffectiveToolPolicyPreview {
	return {
		schema_version: 'effective_tool_policy_preview.v1',
		generated_at: '2026-07-19T10:00:00Z',
		snapshot_id: '1234567890abcdef',
		agent_id: 'presto',
		definition_version: 4,
		definition_digest: 'definition-digest',
		trust_level: 'trusted',
		selected_surface: 'chat',
		feature_mode: 'none',
		source_kind: 'direct',
		available_surfaces: [{ surface: 'chat', feature_mode: 'none', label: 'Chat' }],
		direct: [{ name: 'search_memory', provider_visible: true, dispatchable: true, requires_approval: false }],
		runtime: [{ name: 'switch_personality', provider_visible: true, dispatchable: true, requires_approval: false }],
		structural: [{ name: 'delegate_to_agent', provider_visible: true, dispatchable: true, requires_approval: false, allowed_targets: ['researcher'] }],
		deferred: [{ name: 'browser_click', provider_visible: false, dispatchable: false, requires_approval: false }],
		internal: [{ name: 'task_state_action', dispatchable: true }],
		delegation_targets: ['researcher'],
		handover_targets: [],
		denied_tool_names: ['shell'],
		approval_rule_count: 0,
		provider_tool_count: 3,
		dispatch_tool_count: 3
	};
}

describe('effective Crew tool policy helpers', () => {
	it('counts every effective category including internal tools', () => {
		expect(effectiveToolCount(preview())).toBe(5);
	});

	it('keeps snapshot provenance compact without changing short ids', () => {
		expect(shortPolicyId(preview().snapshot_id)).toBe('1234567890ab');
		expect(shortPolicyId('snapshot-1')).toBe('snapshot-1');
	});
});
