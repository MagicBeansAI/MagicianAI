import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	buildAuthoringYamlSnippet,
	emptyAuthoringSelection,
	fetchAuthoringTools,
	partitionAuthoringToolsForApps,
	type AuthoringToolEntry
} from './authoringCatalog';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('fetchAuthoringTools', () => {
	it('accepts the sealed agent and interactive kinds from a degraded catalog', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify({
						status: 'degraded',
						count: 2,
						items: [
							{
								name: 'reviewed-agent',
								kind: 'agent',
								yaml_declaration: '- name: reviewed-agent',
								app_eligible: true,
								lock_review_required: true
							},
							{
								name: 'browser__snapshot',
								kind: 'interactive',
								yaml_declaration: '- name: browser__snapshot',
								app_eligible: true,
								lock_review_required: true
							}
						]
					}),
					{ status: 200, headers: { 'content-type': 'application/json' } }
				)
			)
		);

		const page = await fetchAuthoringTools({});
		expect(page.status).toBe('degraded');
		expect(page.items.map((entry) => entry.kind)).toEqual(['agent', 'interactive']);
	});

	it('keeps an unavailable catalog as a typed empty page', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ status: 'unavailable', count: 0, items: [] }), {
					status: 200,
					headers: { 'content-type': 'application/json' }
				})
			)
		);

		await expect(fetchAuthoringTools({})).resolves.toMatchObject({
			status: 'unavailable',
			count: 0,
			items: []
		});
	});
});

describe('buildAuthoringYamlSnippet', () => {
	it('emits exact declared names without a free-text tool box', () => {
		const snippet = buildAuthoringYamlSnippet(
			{
				tools: ['content_read'],
				agents: ['research-agent'],
				personalities: ['brutal'],
				procedures: ['external-summary']
			},
			{
				tools: [
					{
						name: 'content_read',
						kind: 'compiled',
						version: '1.0.0',
						description: 'Read content.',
						lock_review_required: false,
						app_eligible: true,
						yaml_declaration: '- name: content_read\n  version_requirement: "^1"',
						layer: 'built-in'
					}
				],
				agents: [
					{
						name: 'research-agent',
						description: 'Research',
						default_runner: false,
						yaml_declaration: 'agent: research-agent'
					}
				],
				personalities: [
					{
						name: 'brutal',
						description: 'Direct',
						yaml_declaration: 'personality: brutal'
					}
				],
				procedures: [
					{
						name: 'external-summary',
						version: '1.2.0',
						description: 'Summarize',
						yaml_declaration: '- skill: skill:external-summary\n  version_requirement: "^1"'
					}
				]
			}
		);
		expect(snippet).toContain('name: content_read');
		expect(snippet).toContain('agent: research-agent');
		expect(snippet).toContain('personality: brutal');
		expect(snippet).toContain('skill: skill:external-summary');
		expect(snippet).toContain('uses: [content_read]');
		expect(buildAuthoringYamlSnippet(emptyAuthoringSelection(), {
			tools: [],
			agents: [],
			personalities: [],
			procedures: []
		})).toBe('');
	});
});

describe('partitionAuthoringToolsForApps', () => {
	const tool = (name: string, extra: Partial<AuthoringToolEntry>): AuthoringToolEntry =>
		({
			name,
			kind: 'skill',
			description: '',
			yaml_declaration: '',
			app_eligible: true,
			lock_review_required: false,
			...extra
		}) as AuthoringToolEntry;

	it('lists runnable tools apart and groups the rest by blocker, largest first', () => {
		const jail = 'skillshub/CLI tools need OS-jail contain before they can run in apps';
		const sideEffect = 'bound side-effect binder is not wired; mutations stay refused';
		const { ready, blocked } = partitionAuthoringToolsForApps([
			tool('time_math', { kind: 'compiled', dispatchable: true }),
			tool('youtube-search', { dispatchable: false, dispatch_note: jail }),
			tool('weather', { dispatchable: false, dispatch_note: jail }),
			tool('notes_write', { dispatchable: false, dispatch_note: sideEffect }),
			tool('private', { app_eligible: false, ineligible_reason: 'expose.apps is false' })
		]);
		expect(ready.map((entry) => entry.name)).toEqual(['time_math']);
		expect(blocked.map((group) => [group.reason, group.tools.length])).toEqual([
			[jail, 2],
			[sideEffect, 1],
			['expose.apps is false', 1]
		]);
	});
});
