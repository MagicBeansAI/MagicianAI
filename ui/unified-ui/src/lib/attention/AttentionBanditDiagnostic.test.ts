import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import AttentionBanditDiagnostic from './AttentionBanditDiagnostic.svelte';
import type { AttentionBanditDecision } from './attentionBandit';

const shadow: AttentionBanditDecision = {
	schema_version: 1,
	mode: 'shadow',
	policy_snapshot_id: 'policy-1',
	policy_model_version: 'contextual-ts-v1',
	posterior_version: 12,
	posterior_uncertainty: 0.31,
	proposed_position: 3,
	served_position: 1,
	served_propensity: 1,
	posterior_draw_count: 16,
	seed_identity: 'seed-1',
	support: true,
	exploration: false,
	applied: false,
	degradation_reason: null
};

describe('AttentionBanditDiagnostic', () => {
	it('labels shadow as preview-only and identifies server-served order', () => {
		const { body } = render(AttentionBanditDiagnostic, { props: { decision: shadow } });
		expect(body).toContain('Personal ranking preview');
		expect(body).toContain('Served position 1');
		expect(body).toContain('Server-served order');
		expect(body).not.toContain('Personal canary active');
	});

	it('calls canary active only for an explicitly server-applied decision', () => {
		const retained = { ...shadow, mode: 'canary' as const };
		const applied = {
			...retained,
			served_propensity: 0.24,
			exploration: true,
			applied: true
		};
		expect(render(AttentionBanditDiagnostic, { props: { decision: retained } }).body)
			.toContain('Canary baseline retained');
		expect(render(AttentionBanditDiagnostic, { props: { decision: applied } }).body)
			.toContain('Personal canary active');
	});
});
