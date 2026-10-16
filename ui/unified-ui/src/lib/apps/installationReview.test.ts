import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	defaultGrantedNames,
	toggleCustomSurfaceGrant,
	toggleSecretUseGrant,
	toggleAnyPublicHostGrant,
	secretReach,
	setSecretUseScope,
	appNamedHosts,
	approveAppInstallation,
	fetchAppInstallationReview,
	grantDisplayName,
	inertWorkflowsForGrant,
	appInstallationReviewMatrix,
	type AppInstallationReview
} from './installationReview';

const review: AppInstallationReview = {
	requested_behaviors: [],
	requested_event_behaviors: [],
	installation_id: 'install_1',
	attempt_id: 'attempt:1',
	attempt_kind: 'initial_install',
	package_revision_ref: 'pkg:1',
	package_content_digest: 'blake3:abc',
	name: 'notes',
	version: '0.1.0',
	description: 'A private app',
	requested_tools: ['capability:content_read', 'capability:time_math'],
	requested_agents: ['agent:research-agent'],
	requested_personalities: ['personality:brutal'],
	requested_interactive_capabilities: [],
	tool_dispatch: [
		{
			tool: 'capability:time_math',
			dispatchable: true,
			attested_operations: ['date_range', 'now'],
			reason: 'date_range is a pure transform; now is a trusted local clock read'
		},
		{
			tool: 'capability:content_read',
			dispatchable: false,
			attested_operations: [],
			reason: 'no trusted dispatcher target for this tool'
		}
	],
	workflows: [
		{
			workflow_id: 'summarize',
			uses: ['capability:content_read'],
			agent: 'agent:research-agent',
			personality: 'personality:brutal'
		}
	],
	// One binding per workflow, because that is what the server emits:
	// `resolve_reviewed_workflow_material` builds
	// `Vec::with_capacity(manifest.app.workflows.len())` and pushes once per
	// workflow. The parser enforces the same 1:1 rule, so a fixture with a
	// workflow and no binding is a payload the server cannot produce — it was
	// rejecting every test that did not also blank `workflows`.
	workflow_material_bindings: [
		{
			schema: 'magician.app-reviewed-workflow-material.v1',
			workflow_id: 'summarize',
			agent_ref: 'agent:research-agent',
			agent_definition_revision: 1,
			agent_definition_digest: 'blake3:agent-definition',
			agent_descriptor_digest: 'blake3:agent-descriptor',
			binding_digest: 'blake3:binding'
		}
	],
	workflow_material_digest: 'blake3:review-material',
	inert_workflows: [],
	requested_data_handling_policy: {
		classification_floor: 'personal', model_processing: 'local_only',
		personal_agent_access: 'denied', memory_promotion: 'denied',
		external_egress: 'denied', approved_destinations: []
	},
	requested_background_execution: { mode: 'denied' },
	requested_network_policy: { mode: 'denied' },
	requested_resource_ceiling: {
		max_input_tokens: 8_000, max_output_tokens: 2_000, max_cost_microusd: 0,
		max_paid_tool_invocations: 0, max_active_seconds: 30, max_lifetime_seconds: 60,
		max_browser_network_actions: 0, max_concurrent_foreground_runs: 1,
		max_concurrent_background_runs: 0, max_records: 100, max_payload_bytes: 65_536,
		max_attachment_bytes: 0, max_monthly_tokens: 20_000, max_monthly_cost_microusd: 0
	}
};

afterEach(() => vi.unstubAllGlobals());

describe('grantDisplayName', () => {
	it('strips grant prefixes so owners see the authoring names', () => {
		expect(grantDisplayName('capability:content_read')).toBe('content_read');
		expect(grantDisplayName('agent:research-agent')).toBe('research-agent');
		expect(grantDisplayName('personality:brutal')).toBe('brutal');
		expect(grantDisplayName('content_read')).toBe('content_read');
	});
});

describe('defaultGrantedNames', () => {
	it('starts with every requested tool, agent and personality granted', () => {
		expect(defaultGrantedNames(review)).toEqual({
			review_material_digest: 'blake3:review-material',
			granted_tools: ['capability:content_read', 'capability:time_math'],
			granted_agents: ['agent:research-agent'],
			granted_personalities: ['personality:brutal']
		});
	});
});

describe('secret uses', () => {
	const withSecrets: AppInstallationReview = {
		...review,
		requested_secret_uses: [
			{
				tool: 'capability:news-search-via-tavily',
				secret_ref: 'TAVILY_API_KEY',
				required: true,
				destination: 'api.tavily.com'
			}
		]
	};

	it('starts with every secret unticked', () => {
		expect(defaultGrantedNames(withSecrets).granted_secret_uses).toEqual([]);
		expect(defaultGrantedNames(review).granted_secret_uses).toBeUndefined();
	});

	it('ticks and unticks only a requested secret', () => {
		const start = defaultGrantedNames(withSecrets);
		const ticked = toggleSecretUseGrant(withSecrets, start, 'capability:news-search-via-tavily', 'TAVILY_API_KEY');
		expect(ticked.granted_secret_uses).toEqual([
			{ tool: 'capability:news-search-via-tavily', secret_ref: 'TAVILY_API_KEY' }
		]);
		expect(
			toggleSecretUseGrant(withSecrets, ticked, 'capability:news-search-via-tavily', 'TAVILY_API_KEY')
				.granted_secret_uses
		).toEqual([]);
		expect(() =>
			toggleSecretUseGrant(withSecrets, start, 'capability:news-search-via-tavily', 'OPENAI_API_KEY')
		).toThrow();
	});
});

describe('in-place tools and network', () => {
	const inPlace: AppInstallationReview = {
		...review,
		requested_secret_uses: [
			{
				tool: 'capability:web-search-via-minimax',
				secret_ref: 'MINIMAX_API_KEY',
				required: true,
				destination: '',
				app_granted_hosts: true,
				delivery: 'config_file'
			}
		],
		tool_runtime: [
			{
				tool: 'capability:web-search-via-minimax',
				in_place_from: 'web-search-via-minimax',
				reaches_granted_hosts: true
			}
		],
		offers_any_public_host: true
	};

	it('starts with "any public host" off and toggles it only when offered', () => {
		const start = defaultGrantedNames(inPlace);
		expect(start.granted_any_public_host).toBe(false);
		expect(defaultGrantedNames(review).granted_any_public_host).toBeUndefined();
		const any = toggleAnyPublicHostGrant(inPlace, start);
		expect(any.granted_any_public_host).toBe(true);
		expect(toggleAnyPublicHostGrant(inPlace, any).granted_any_public_host).toBe(false);
		expect(() => toggleAnyPublicHostGrant(review, start)).toThrow();
	});

	it('scopes host-less keys to picked hosts, or any site only by explicit opt-in', () => {
		const use = inPlace.requested_secret_uses![0];
		expect(secretReach(use)).toBe('pick the hosts');
		expect(secretReach({ ...use, destination: 'api.tavily.com', app_granted_hosts: false }))
			.toBe('api.tavily.com');
		expect(secretReach({ ...use, not_grantable: 'no host' })).toBe('cannot be sent anywhere in this app');

		// A malicious app names two hosts; the owner picks which the key may reach.
		const named = {
			...inPlace,
			requested_network_policy: {
				mode: 'approved_destinations' as const,
				destinations: ['destination:api.minimax.io', 'destination:collector.evil.com']
			}
		};
		expect(appNamedHosts(named)).toEqual(['api.minimax.io', 'collector.evil.com']);
		const start = defaultGrantedNames(named);
		const one = setSecretUseScope(named, start, use.tool, use.secret_ref, { hosts: ['api.minimax.io'], anySite: false });
		expect(one.granted_secret_uses).toEqual([
			{ tool: use.tool, secret_ref: use.secret_ref, hosts: ['api.minimax.io'] }
		]);
		expect(secretReach(use, one.granted_secret_uses![0])).toBe('api.minimax.io');
		const both = setSecretUseScope(named, one, use.tool, use.secret_ref, {
			hosts: ['collector.evil.com', 'api.minimax.io'], anySite: false
		});
		expect(both.granted_secret_uses).toEqual([
			{ tool: use.tool, secret_ref: use.secret_ref, hosts: ['api.minimax.io', 'collector.evil.com'] }
		]);
		expect(setSecretUseScope(named, one, use.tool, use.secret_ref, { hosts: [], anySite: false }).granted_secret_uses).toEqual([]);
		expect(() => setSecretUseScope(named, start, use.tool, use.secret_ref, { hosts: ['elsewhere.example.com'], anySite: false })).toThrow();
		// "Any site" needs an app that asks for any public host.
		expect(() => setSecretUseScope(named, start, use.tool, use.secret_ref, { hosts: [], anySite: true })).toThrow();
		const anyApp = {
			...named,
			requested_data_handling_policy: { ...named.requested_data_handling_policy, external_egress: 'any_public_host' as const }
		};
		expect(setSecretUseScope(anyApp, start, use.tool, use.secret_ref, { hosts: [], anySite: true }).granted_secret_uses)
			.toEqual([{ tool: use.tool, secret_ref: use.secret_ref, any_site: true }]);
		// Host-less keys are never ticked blindly.
		expect(() => toggleSecretUseGrant(named, start, use.tool, use.secret_ref)).toThrow();
	});

	it('parses in-place tools, host-less keys and the any-host offer', async () => {
		const body = structuredClone({ ...inPlace, workflows: [], workflow_material_bindings: [] });
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		const parsed = await fetchAppInstallationReview('install_1');
		expect(parsed.tool_runtime).toEqual(inPlace.tool_runtime);
		expect(parsed.offers_any_public_host).toBe(true);
		expect(parsed.requested_secret_uses?.[0]).toMatchObject({ app_granted_hosts: true, delivery: 'config_file' });

		// A key that says nowhere it can go is not rendered as harmless.
		const nowhere = structuredClone(body);
		delete (nowhere.requested_secret_uses![0] as { app_granted_hosts?: boolean }).app_granted_hosts;
		fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(nowhere), { status: 200 }));
		await expect(fetchAppInstallationReview('install_1')).rejects.toThrow(/payload is invalid/i);
	});
});

describe('inertWorkflowsForGrant', () => {
	it('lists workflows that a subset grant cannot run', () => {
		expect(inertWorkflowsForGrant(review, defaultGrantedNames(review))).toEqual([]);
		expect(
			inertWorkflowsForGrant(review, {
				review_material_digest: 'blake3:review-material',
				granted_tools: ['capability:time_math'],
				granted_agents: ['agent:research-agent'],
				granted_personalities: ['personality:brutal']
			})
		).toEqual([
			{
				workflow_id: 'summarize',
				reasons: ['missing tool content_read']
			}
		]);
	});
});

describe('appInstallationReviewMatrix', () => {
	it('renders policy and resource ceilings without rendering captured content', () => {
		const matrix = appInstallationReviewMatrix(review);
		expect(matrix).toContainEqual({
			category: 'Personal-agent data', request: 'denied', posture: 'denied'
		});
		expect(matrix).toContainEqual({
			category: 'Background execution', request: 'denied', posture: 'denied'
		});
		expect(matrix.find((row) => row.category === 'Storage')?.request).toContain('100 records');
		expect(JSON.stringify(matrix)).not.toContain('screenshot');
	});

	it('accepts the omitted empty destination list and rejects a nested policy substitution', async () => {
		const body = structuredClone({
			...review,
			workflows: [],
			workflow_material_bindings: [],
			requested_data_handling_policy: {
				classification_floor: 'personal', model_processing: 'local_only',
				personal_agent_access: 'denied', memory_promotion: 'denied', external_egress: 'denied'
			}
		});
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(fetchAppInstallationReview('install_1')).resolves.toMatchObject({
			requested_data_handling_policy: { external_egress: 'denied', approved_destinations: [] }
		});

		(body.requested_data_handling_policy as Record<string, unknown>).captured_content = 'forbidden';
		fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(body), { status: 200 }));
		await expect(fetchAppInstallationReview('install_1')).rejects.toThrow(/payload is invalid/i);
	});

	it('parses a package that declares behaviours and a custom surface', async () => {
		// The server skips these keys when empty, so they appear only for
		// packages that use the features. Every shipped system package declares
		// behaviours, and an allow-list that omitted them rejected the entire
		// payload — which made those packages unreviewable in the UI.
		const body = structuredClone({
			...review,
			requested_behaviors: [
				{
					behavior_id: 'sync_queue',
					purpose: 'Refresh the review queue on a schedule.',
					action: 'sync_queue',
					operations: ['list_learning_candidates']
				}
			],
			requested_event_behaviors: [],
			requested_custom_surface: {
				entry_points: [{ route: '/console', document: 'surfaces/console.html', document_digest: 'blake3:document' }],
				executable_members: [{ path: 'surfaces/console.js', content_digest: 'blake3:script', byte_len: 4096 }],
				executable_bytes: 4096,
				request_digest: 'blake3:reviewed-pages',
				scan_findings: [],
				sandbox: 'allow-scripts',
				csp: "default-src 'none'"
			}
		});
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 })));
		const parsed = await fetchAppInstallationReview('install_1');
		// Behaviours must survive parsing: omitting `granted_behaviors` at
		// approval grants every requested behaviour, so a dropped entry is
		// authority the owner never saw.
		expect(parsed.requested_behaviors).toHaveLength(1);
		expect(parsed.requested_behaviors[0].behavior_id).toBe('sync_queue');
		expect(parsed.requested_custom_surface?.executable_member_count).toBe(1);
		expect(parsed.requested_custom_surface?.entry_points).toEqual(body.requested_custom_surface.entry_points);
		const initial = defaultGrantedNames(parsed);
		expect(initial.granted_custom_surface_entry_points).toEqual([]);
		const selected = toggleCustomSurfaceGrant(parsed, initial, '/console');
		expect(selected.granted_custom_surface_entry_points).toEqual([{
			route: '/console', document: 'surfaces/console.html', reviewed_request_digest: 'blake3:reviewed-pages'
		}]);
		expect(toggleCustomSurfaceGrant(parsed, selected, '/console').granted_custom_surface_entry_points).toEqual([]);
		expect(() => toggleCustomSurfaceGrant(parsed, initial, '/unknown')).toThrow(/not requested/);

		const entry = { installation_id: 'install_1', installation_generation: 1, package_revision_ref: 'pkg:1', status: 'ready_for_review' as const };
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			installation_id: 'install_1', generation: 2, status: 'enabled', grant_revision: 1,
			schema_revision: 1, surface_revision: 1, approval_id: 'approval:1', attempt_id: 'attempt:1',
			outcome: 'enabled', inert_workflows: [], granted_custom_surface_entry_points: ['/console']
		}), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(approveAppInstallation(entry, parsed, selected)).resolves.toMatchObject({ granted_custom_surface_entry_points: ['/console'] });
		expect(JSON.parse(fetchMock.mock.calls[0][1].body).granted_custom_surface_entry_points).toEqual(selected.granted_custom_surface_entry_points);
		fetchMock.mockClear();
		for (const substitution of [
			{ route: '/other' }, { document: 'surfaces/other.html' }, { reviewed_request_digest: 'blake3:stale' }
		]) {
			await expect(approveAppInstallation(entry, parsed, { ...selected,
				granted_custom_surface_entry_points: [{ ...selected.granted_custom_surface_entry_points![0], ...substitution }]
			})).rejects.toThrow(/interactive pages no longer match/);
		}
		expect(fetchMock).not.toHaveBeenCalled();

		for (const change of [
			{ entry_points: [{ route: '/console', document: 'surfaces/console.html' }] },
			{ request_digest: undefined },
			{ executable_bytes: 8192 },
			{ executable_members: [{ path: 'surfaces/console.js' }] },
			{ scan_findings: [{ path: 'surfaces/console.js' }] },
			{ sandbox: 'allow-scripts allow-same-origin' }
		]) {
			fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ ...body,
				requested_custom_surface: { ...body.requested_custom_surface, ...change }
			}), { status: 200 }));
			await expect(fetchAppInstallationReview('install_1')).rejects.toThrow(/payload is invalid/);
		}
	});

	it('rejects a behaviour entry it cannot render rather than dropping it', async () => {
		const body = structuredClone({
			...review,
			requested_behaviors: [{ behavior_id: 'sync_queue', purpose: 'x', action: 'sync_queue' }]
		});
		(body.requested_behaviors as Array<Record<string, unknown>>)[0].operations = [123];
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 })));
		await expect(fetchAppInstallationReview('install_1')).rejects.toThrow(/payload is invalid/i);
	});

	it('rejects renderable review collections above the local owner bound', async () => {
		const body = structuredClone({
			...review,
			workflows: Array.from({ length: 257 }, (_, index) => ({
				workflow_id: `workflow_${index}`,
				uses: [],
				agent: `agent:${index}`
			})),
			workflow_material_bindings: []
		});
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 200 })));
		await expect(fetchAppInstallationReview('install_1')).rejects.toThrow(/payload is invalid/i);
	});
});
