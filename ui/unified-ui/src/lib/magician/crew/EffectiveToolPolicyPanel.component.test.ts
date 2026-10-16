import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import EffectiveToolPolicyPanel from './EffectiveToolPolicyPanel.svelte';

function response(surface: 'chat' | 'realtime_voice') {
	const voice = surface === 'realtime_voice';
	return {
		schema_version: 'effective_tool_policy_preview.v1',
		generated_at: '2026-07-19T10:00:00Z',
		snapshot_id: voice ? 'voice-snapshot-123456' : 'chat-snapshot-123456',
		agent_id: 'presto',
		definition_version: 4,
		definition_digest: 'definition-digest',
		trust_level: 'local',
		selected_surface: surface,
		feature_mode: 'none',
		source_kind: 'direct',
		available_surfaces: [
			{ surface: 'chat', feature_mode: 'none', label: 'Chat' },
			{ surface: 'realtime_voice', feature_mode: 'none', label: 'Realtime voice' }
		],
		direct: [
			{
				name: voice ? 'voice_search' : 'search_memory',
				description: 'Search grounded context',
				provider_visible: true,
				dispatchable: true,
				requires_approval: false
			}
		],
		runtime: [],
		structural: [],
		deferred: [],
		internal: [{ name: 'task_state_action', dispatchable: true }],
		delegation_targets: [],
		handover_targets: [],
		denied_tool_names: ['shell'],
		approval_rule_count: 1,
		provider_tool_count: 1,
		dispatch_tool_count: 1
	};
}

function cacheResponse() {
	return {
		schema_version: 'runtime_context_cache_preview.v1',
		generated_at: '2026-07-19T10:00:00Z',
		agent_id: 'presto',
		definition_version: 4,
		scope: { principal: 'anonymous', workspace: 'default' },
		registry_revision: 'registry-revision-123456',
		built_at_ms: 1784474400000,
		pack_count: 42,
		tool_index_count: 96,
		cache: {
			entry_count: 1,
			hits: 3,
			misses: 1,
			builds: 1,
			invalidations: 0,
			revision_failures: 0,
			coalesced_waiters: 0
		},
		surface_plan_status: 'active',
		surface_plan_cache: {
			entry_count: 2,
			hits: 3,
			misses: 1,
			inserts: 2,
			evictions: 0,
			invalidations: 0,
			static_prompt_entry_count: 3,
			static_prompt_hits: 7,
			static_prompt_misses: 1,
			static_prompt_inserts: 3,
			static_prompt_evictions: 0
		},
		working_set_store: {
			entry_count: 1,
			reads: 4,
			creates: 1,
			mutations: 0,
			removals: 0,
			evictions: 0
		}
	};
}

beforeEach(() => {
	installFetchMock([
		{
			match: '/runtime-context-cache',
			handle: () => jsonResponse(cacheResponse())
		},
		{
			match: '/effective-tools',
			handle: (call) =>
				jsonResponse(response(call.url.includes('surface=realtime_voice') ? 'realtime_voice' : 'chat'))
		}
	]);
});

describe('EffectiveToolPolicyPanel', () => {
	it('shows every effective category and re-resolves when the surface changes', async () => {
		const user = userEvent.setup();
		render(EffectiveToolPolicyPanel, { agentId: 'presto' });

		expect(await screen.findByText('search_memory')).toBeInTheDocument();
		expect(screen.getByText('Internal · server-owned')).toBeInTheDocument();
		expect(screen.getByText('Filtered by definition')).toBeInTheDocument();
		expect(screen.getByText('3 prompt prefixes · 7 hits')).toBeInTheDocument();
		expect(screen.getByText('1 approval rule')).toBeInTheDocument();

		await user.selectOptions(screen.getByRole('combobox', { name: 'Invocation surface' }), 'realtime_voice');
		expect(await screen.findByText('voice_search')).toBeInTheDocument();
		expect(screen.queryByText('search_memory')).not.toBeInTheDocument();
	});

	it('uses the mutation endpoint for an explicit runtime cache refresh', async () => {
		const user = userEvent.setup();
		const { calls } = installFetchMock([
			{
				match: '/runtime-context-cache',
				handle: () => jsonResponse(cacheResponse())
			},
			{
				match: '/effective-tools',
				handle: () => jsonResponse(response('chat'))
			}
		]);
		render(EffectiveToolPolicyPanel, { agentId: 'presto' });

		expect(await screen.findByText('search_memory')).toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Refresh effective tools' }));

		await waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true));
		expect(await screen.findByText('Runtime cache refreshed')).toBeInTheDocument();
	});

	it('inherits canonical theme tokens and protects narrow layouts', () => {
		const source = readFileSync(
			join(process.cwd(), 'src/lib/magician/crew/EffectiveToolPolicyPanel.svelte'),
			'utf8'
		);

		for (const token of [
			'--component-card-bg',
			'--component-card-border',
			'--accent-primary',
			'--accent-secondary',
			'--color-warning-soft',
			'--color-error-soft',
			'--font-primary',
			'--font-mono'
		]) {
			expect(source).toContain(`var(${token}`);
		}

		expect(source).not.toMatch(/#[0-9a-f]{3,8}\b/i);
		expect(source).not.toContain('var(--accent,');
		expect(source).not.toContain('var(--border-color,');
		expect(source).toContain('box-sizing: border-box');
		expect(source).toContain('flex-direction: column');
		expect(source).toContain('minmax(min(150px, 100%), 1fr)');
		expect(source).toContain(':focus-visible');
	});
});
