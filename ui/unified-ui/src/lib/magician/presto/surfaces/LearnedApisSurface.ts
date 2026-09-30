import type { MuijComponent } from '$lib/stores/muijStore';

// ---------------------------------------------------------------------------
// Types — matching backend API responses
// ---------------------------------------------------------------------------

export interface RegistryIndex {
	version: string;
	origins: Record<string, OriginEntry>;
	last_rebuilt: number;
}

export interface OriginEntry {
	origin_key: string;
	origin_url: string;
	capability_count: number;
	trace_count: number;
	capabilities: CapabilitySummary[];
	updated_at: number;
}

export interface CapabilitySummary {
	id: string;
	name: string;
	method: string;
	url_template: string;
	confidence: string;
	side_effects: string;
	sample_count: number;
	updated_at: number;
	graphql_operation?: string;
	replay_success_count?: number;
	replay_failure_count?: number;
}

export interface AuthStatus {
	has_auth: boolean;
	is_stale: boolean;
	has_cookies: boolean;
	has_headers: boolean;
	has_storage: boolean;
}

export interface ReplayResponse {
	status: number;
	headers: Record<string, string> | null;
	body: string | null;
	elapsed_ms: number;
	auth_was_stale: boolean;
	confidence_after: string;
	error: string | null;
}

// ---------------------------------------------------------------------------
// Input interface
// ---------------------------------------------------------------------------

export interface LearnedApisSurfaceInput {
	registry: RegistryIndex | null;
	authStatuses: Record<string, AuthStatus>;
	loading: boolean;
	error: string | null;
	replayResults: Record<string, ReplayResponse>;
}

// ---------------------------------------------------------------------------
// Action ID constants
// ---------------------------------------------------------------------------

export const LEARNED_APIS_REFRESH_BTN = 'learned-apis-refresh-btn';
export const LEARNED_APIS_GRID_PREFIX = 'learned-apis-grid-';
export const LEARNED_APIS_OPENAPI_PREFIX = 'learned-apis-openapi-';
export const LEARNED_APIS_REFRESH_AUTH_PREFIX = 'learned-apis-refresh-auth-';

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

function methodColor(method: string): 'default' | 'success' | 'warning' | 'error' | 'info' {
	switch (method.toUpperCase()) {
		case 'GET':
			return 'success';
		case 'POST':
			return 'info';
		case 'PUT':
		case 'PATCH':
			return 'warning';
		case 'DELETE':
			return 'error';
		default:
			return 'default';
	}
}

function confidenceColor(confidence: string): 'default' | 'success' | 'warning' | 'error' | 'info' {
	switch (confidence) {
		case 'Trusted':
			return 'success';
		case 'Validated':
			return 'info';
		case 'Candidate':
			return 'warning';
		case 'Observed':
			return 'default';
		default:
			return 'default';
	}
}

function buildAuthBadges(originKey: string, auth: AuthStatus | undefined): MuijComponent[] {
	if (!auth) {
		return [
			{
				id: `learned-apis-auth-unknown-${originKey}`,
				component_type: 'Badge',
				props: { text: 'Auth unknown', color: 'default' }
			}
		];
	}

	const badges: MuijComponent[] = [];

	if (auth.has_auth) {
		badges.push({
			id: `learned-apis-auth-ok-${originKey}`,
			component_type: 'Badge',
			props: {
				text: auth.is_stale ? 'Auth stale' : 'Auth ready',
				color: auth.is_stale ? 'warning' : 'success'
			}
		});
	} else {
		badges.push({
			id: `learned-apis-auth-none-${originKey}`,
			component_type: 'Badge',
			props: { text: 'No auth', color: 'default' }
		});
	}

	if (auth.has_cookies) {
		badges.push({
			id: `learned-apis-auth-cookies-${originKey}`,
			component_type: 'Badge',
			props: { text: 'Cookies', color: 'info' }
		});
	}

	if (auth.has_headers) {
		badges.push({
			id: `learned-apis-auth-headers-${originKey}`,
			component_type: 'Badge',
			props: { text: 'Headers', color: 'info' }
		});
	}

	if (auth.has_storage) {
		badges.push({
			id: `learned-apis-auth-storage-${originKey}`,
			component_type: 'Badge',
			props: { text: 'Storage', color: 'info' }
		});
	}

	return badges;
}

// ---------------------------------------------------------------------------
// Build function
// ---------------------------------------------------------------------------

export function buildLearnedApisSurface(input: LearnedApisSurfaceInput): MuijComponent[] {
	const { registry, authStatuses, loading, error } = input;

	// Loading state
	if (loading) {
		return [
			{
				id: 'learned-apis-loading',
				component_type: 'Spinner',
				props: { label: 'Loading learned APIs...' }
			}
		];
	}

	// Error state
	if (error) {
		return [
			{
				id: 'learned-apis-error',
				component_type: 'Alert',
				props: { type: 'error', message: error }
			}
		];
	}

	// Empty state
	const originKeys = registry ? Object.keys(registry.origins) : [];
	if (!registry || originKeys.length === 0) {
		return [
			{
				id: 'learned-apis-empty',
				component_type: 'EmptyState',
				props: {
					title: 'No APIs discovered yet',
					description: 'Browse a website to start learning.'
				}
			}
		];
	}

	const components: MuijComponent[] = [];

	// --- Stats row ---
	const totalOrigins = originKeys.length;
	const totalCapabilities = originKeys.reduce(
		(sum, key) => sum + (registry.origins[key]?.capability_count ?? 0),
		0
	);

	components.push({
		id: 'learned-apis-stats',
		component_type: 'Container',
		props: {},
		children: [
			{
				id: 'learned-apis-stats-row',
				component_type: 'Stack',
				props: { direction: 'row', gap: '0.75rem', align: 'center' },
				children: [
					{
						id: 'learned-apis-stats-origins',
						component_type: 'Badge',
						props: {
							text: `${totalOrigins} origin${totalOrigins !== 1 ? 's' : ''}`,
							color: 'info'
						}
					},
					{
						id: 'learned-apis-stats-capabilities',
						component_type: 'Badge',
						props: {
							text: `${totalCapabilities} capabilit${totalCapabilities !== 1 ? 'ies' : 'y'}`,
							color: 'info'
						}
					},
					{
						id: LEARNED_APIS_REFRESH_BTN,
						component_type: 'Button',
						label: 'Refresh',
						props: {
							variant: 'secondary',
							size: 'sm',
							interactive: true
						}
					}
				]
			}
		]
	});

	// --- Per-origin sections ---
	for (const originKey of originKeys) {
		const origin = registry.origins[originKey];
		if (!origin) continue;

		const auth = authStatuses[originKey];
		const authBadges = buildAuthBadges(originKey, auth);

		const rows = origin.capabilities.map((cap) => ({
			id: cap.id,
			method: { kind: 'badge', text: cap.method, color: methodColor(cap.method) },
			url_template: cap.url_template,
			confidence: {
				kind: 'badge',
				text: cap.confidence,
				color: confidenceColor(cap.confidence)
			},
			side_effects: cap.side_effects,
			samples: cap.sample_count
		}));

		const panelChildren: MuijComponent[] = [
			{
				id: LEARNED_APIS_GRID_PREFIX + originKey,
				component_type: 'EntityGrid',
				props: {
					expandable: true,
					rowIdKey: 'id',
					actions: [
						{ id: 'replay', label: 'Replay', variant: 'info' },
						{ id: 'edit_replay', label: 'Edit & Replay', variant: 'default' }
					],
					columns: [
						{ key: 'method', label: 'Method', sortable: true },
						{ key: 'url_template', label: 'URL Template', sortable: true },
						{ key: 'confidence', label: 'Confidence', sortable: true },
						{ key: 'side_effects', label: 'Side Effects', sortable: true },
						{ key: 'samples', label: 'Samples', sortable: true }
					],
					rows
				}
			}
		];

		// Replay result panels for capabilities in this origin
		const originResults = origin.capabilities
			.filter((cap) => input.replayResults[cap.id])
			.map((cap) => {
				const result = input.replayResults[cap.id];
				const children: MuijComponent[] = [];

				// Status badge
				children.push({
					id: `replay-status-${cap.id}`,
					component_type: 'Badge',
					props: {
						text: result.error ? 'Error' : `${result.status}`,
						color: result.error
							? 'error'
							: result.status < 300
								? 'success'
								: result.status < 500
									? 'warning'
									: 'error'
					}
				});

				// Timing
				children.push({
					id: `replay-time-${cap.id}`,
					component_type: 'Text',
					props: { children: `${result.elapsed_ms}ms`, variant: 'caption' }
				});

				// Auth stale warning
				if (result.auth_was_stale) {
					children.push({
						id: `replay-auth-warn-${cap.id}`,
						component_type: 'Alert',
						props: {
							type: 'warning',
							message: 'Auth credentials may be stale (401/403 response)'
						}
					});
				}

				// Error
				if (result.error) {
					children.push({
						id: `replay-error-${cap.id}`,
						component_type: 'Alert',
						props: { type: 'error', message: result.error }
					});
				}

				// Response body
				if (result.body) {
					let language = 'text';
					let displayBody = result.body;
					try {
						displayBody = JSON.stringify(JSON.parse(result.body), null, 2);
						language = 'json';
					} catch {
						// not JSON, keep as-is
					}
					children.push({
						id: `replay-body-${cap.id}`,
						component_type: 'CodeBlock',
						props: {
							code: displayBody,
							language,
							maxHeight: 300
						}
					});
				}

				return {
					id: `replay-result-${cap.id}`,
					component_type: 'Panel',
					props: {
						header: `Replay: ${cap.method} ${cap.url_template}`,
						collapsible: true
					},
					children
				} as MuijComponent;
			});

		panelChildren.push(...originResults);

		panelChildren.push({
			id: LEARNED_APIS_OPENAPI_PREFIX + originKey,
			component_type: 'Button',
			label: 'View OpenAPI',
			props: {
				variant: 'default',
				size: 'sm',
				interactive: true
			}
		});

		// If auth is stale, add a refresh-auth button
		if (auth?.is_stale) {
			panelChildren.push({
				id: LEARNED_APIS_REFRESH_AUTH_PREFIX + originKey,
				component_type: 'Button',
				label: 'Refresh Auth',
				props: {
					type: 'warning',
					size: 'sm',
					interactive: true
				}
			});
		}

		components.push({
			id: `learned-apis-origin-${originKey}`,
			component_type: 'Panel',
			props: {
				header: origin.origin_url,
				collapsible: true
			},
			children: [
				{
					id: `learned-apis-origin-badges-${originKey}`,
					component_type: 'Stack',
					props: { direction: 'row', gap: '0.5rem', align: 'center' },
					children: [
						{
							id: `learned-apis-origin-cap-count-${originKey}`,
							component_type: 'Badge',
							props: {
								text: `${origin.capability_count} capabilit${origin.capability_count !== 1 ? 'ies' : 'y'}`,
								color: 'default',
							}
						},
						{
							id: `learned-apis-origin-trace-count-${originKey}`,
							component_type: 'Badge',
							props: {
								text: `${origin.trace_count} trace${origin.trace_count !== 1 ? 's' : ''}`,
								color: 'default',
							}
						},
						...authBadges
					]
				},
				...panelChildren
			]
		});
	}

	return components;
}

// ---------------------------------------------------------------------------
// Action parser
// ---------------------------------------------------------------------------

export type LearnedApisAction =
	| { type: 'refresh' }
	| { type: 'replay'; originKey: string; capabilityId: string }
	| { type: 'edit_replay'; originKey: string; capabilityId: string }
	| { type: 'openapi'; originKey: string }
	| { type: 'refresh_auth'; originKey: string };

export function parseLearnedApisAction(
	componentId: string,
	interaction: string,
	detail: Record<string, unknown>
): LearnedApisAction | null {
	if (componentId === LEARNED_APIS_REFRESH_BTN) return { type: 'refresh' };

	if (componentId.startsWith(LEARNED_APIS_OPENAPI_PREFIX)) {
		return {
			type: 'openapi',
			originKey: componentId.slice(LEARNED_APIS_OPENAPI_PREFIX.length)
		};
	}

	if (componentId.startsWith(LEARNED_APIS_REFRESH_AUTH_PREFIX)) {
		return {
			type: 'refresh_auth',
			originKey: componentId.slice(LEARNED_APIS_REFRESH_AUTH_PREFIX.length)
		};
	}

	// Grid action events carry { rowId, actionId }
	if (componentId.startsWith(LEARNED_APIS_GRID_PREFIX) && interaction === 'action') {
		const originKey = componentId.slice(LEARNED_APIS_GRID_PREFIX.length);
		const rowId = detail.rowId as string;
		const actionId = detail.actionId as string;
		if (actionId === 'replay')
			return { type: 'replay', originKey, capabilityId: rowId };
		if (actionId === 'edit_replay')
			return { type: 'edit_replay', originKey, capabilityId: rowId };
	}

	return null;
}
