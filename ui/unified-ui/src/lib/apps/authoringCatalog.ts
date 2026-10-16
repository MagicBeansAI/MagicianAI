import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type AuthoringToolKind = 'compiled' | 'skill' | 'agent' | 'interactive';
export type AuthoringCatalogStatus = 'ok' | 'degraded' | 'unavailable';

export interface AuthoringToolEntry {
	name: string;
	kind: AuthoringToolKind;
	version: string | null;
	description: string;
	lock_review_required: boolean;
	app_eligible: boolean;
	dispatchable?: boolean;
	io_kind?: string | null;
	dispatch_note?: string | null;
	ineligible_reason?: string | null;
	expose_apps?: boolean | null;
	expose_agents?: boolean | null;
	yaml_declaration: string;
	layer: string;
}

export interface AuthoringToolShow extends AuthoringToolEntry {
	status: string;
	actions: string[];
	yaml_snippet: string;
}

export interface AuthoringAgentEntry {
	name: string;
	display_name?: string | null;
	description: string;
	default_runner: boolean;
	yaml_declaration: string;
}

export interface AuthoringPersonalityEntry {
	name: string;
	description: string;
	yaml_declaration: string;
}

export interface AuthoringProcedureEntry {
	name: string;
	version: string | null;
	description: string;
	yaml_declaration: string;
}

export interface AuthoringCatalogList<T> {
	status: AuthoringCatalogStatus;
	count: number;
	items: T[];
}

export interface AuthoringSelection {
	tools: string[];
	agents: string[];
	personalities: string[];
	procedures: string[];
}

const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;

export function emptyAuthoringSelection(): AuthoringSelection {
	return { tools: [], agents: [], personalities: [], procedures: [] };
}

export async function fetchAuthoringTools(options: {
	appEligible?: boolean;
	kind?: AuthoringToolKind;
	signal?: AbortSignal;
}): Promise<AuthoringCatalogList<AuthoringToolEntry>> {
	const query = new URLSearchParams();
	if (options.appEligible) query.set('app_eligible', 'true');
	if (options.kind) query.set('kind', options.kind);
	const suffix = query.size > 0 ? `?${query}` : '';
	const body = await fetchAuthoringJson(`/api/magician/v2/apps/authoring/tools${suffix}`, options.signal);
	return parseCatalogList(body, parseToolEntry);
}

export async function fetchAuthoringTool(
	name: string,
	signal?: AbortSignal
): Promise<AuthoringToolShow> {
	const body = await fetchAuthoringJson(
		`/api/magician/v2/apps/authoring/tools/${encodeURIComponent(name)}`,
		signal
	);
	const tool = parseToolEntry(body);
	if (!isRecord(body) || typeof body.status !== 'string' || !Array.isArray(body.actions)) {
		throw new Error('The authoring tool detail is invalid.');
	}
	return {
		...tool,
		status: body.status,
		actions: body.actions.filter((item): item is string => typeof item === 'string'),
		yaml_snippet: typeof body.yaml_snippet === 'string' ? body.yaml_snippet : tool.yaml_declaration
	};
}

export async function fetchAuthoringAgents(
	signal?: AbortSignal
): Promise<AuthoringCatalogList<AuthoringAgentEntry>> {
	const body = await fetchAuthoringJson('/api/magician/v2/apps/authoring/agents', signal);
	return parseCatalogList(body, parseAgentEntry);
}

export async function fetchAuthoringPersonalities(
	signal?: AbortSignal
): Promise<AuthoringCatalogList<AuthoringPersonalityEntry>> {
	const body = await fetchAuthoringJson('/api/magician/v2/apps/authoring/personalities', signal);
	return parseCatalogList(body, parsePersonalityEntry);
}

export async function fetchAuthoringProcedures(
	signal?: AbortSignal
): Promise<AuthoringCatalogList<AuthoringProcedureEntry>> {
	const body = await fetchAuthoringJson('/api/magician/v2/apps/authoring/procedures', signal);
	return parseCatalogList(body, parseProcedureEntry);
}

export function buildAuthoringYamlSnippet(
	selection: AuthoringSelection,
	catalog: {
		tools: AuthoringToolEntry[];
		agents: AuthoringAgentEntry[];
		personalities: AuthoringPersonalityEntry[];
		procedures: AuthoringProcedureEntry[];
	}
): string {
	const tools = catalog.tools.filter((entry) => selection.tools.includes(entry.name));
	const procedures = catalog.procedures.filter((entry) =>
		selection.procedures.includes(entry.name)
	);
	const agent = catalog.agents.find((entry) => selection.agents.includes(entry.name));
	const personality = catalog.personalities.find((entry) =>
		selection.personalities.includes(entry.name)
	);
	if (!tools.length && !procedures.length && !agent && !personality) return '';

	const lines = [
		'App options — use these exact names in the package YAML:',
		'',
		'dependencies:'
	];
	if (tools.length) {
		lines.push('  tools:');
		for (const tool of tools) {
			for (const line of tool.yaml_declaration.split('\n')) {
				lines.push(`    ${line}`);
			}
		}
	} else {
		lines.push('  tools: []');
	}
	if (procedures.length) {
		lines.push('  procedure_skills:');
		for (const procedure of procedures) {
			for (const line of procedure.yaml_declaration.split('\n')) {
				lines.push(`    ${line}`);
			}
		}
	}
	if (agent || personality || tools.length) {
		lines.push('');
		lines.push('workflows:');
		lines.push('  run:');
		if (agent) lines.push(`    ${agent.yaml_declaration}`);
		if (personality) lines.push(`    ${personality.yaml_declaration}`);
		if (tools.length) {
			lines.push(`    uses: [${tools.map((tool) => tool.name).join(', ')}]`);
		}
	}
	return lines.join('\n');
}

function parseCatalogList<T>(body: unknown, parseItem: (value: unknown) => T): AuthoringCatalogList<T> {
	if (!isRecord(body) || !isAuthoringCatalogStatus(body.status) || !Array.isArray(body.items)) {
		throw new Error('The authoring catalog response is invalid.');
	}
	const items = body.items.map(parseItem);
	return {
		status: body.status,
		count: typeof body.count === 'number' ? body.count : items.length,
		items
	};
}

function parseToolEntry(value: unknown): AuthoringToolEntry {
	if (!isRecord(value) || typeof value.name !== 'string' || typeof value.yaml_declaration !== 'string') {
		throw new Error('The authoring tool entry is invalid.');
	}
	if (
		value.kind !== 'compiled' &&
		value.kind !== 'skill' &&
		value.kind !== 'agent' &&
		value.kind !== 'interactive'
	) {
		throw new Error('The authoring tool kind is invalid.');
	}
	return {
		name: value.name,
		kind: value.kind,
		version: typeof value.version === 'string' ? value.version : null,
		description: typeof value.description === 'string' ? value.description : '',
		lock_review_required: value.lock_review_required === true,
		app_eligible: value.app_eligible === true,
		dispatchable: value.dispatchable === true,
		io_kind: typeof value.io_kind === 'string' ? value.io_kind : null,
		dispatch_note: typeof value.dispatch_note === 'string' ? value.dispatch_note : null,
		ineligible_reason: typeof value.ineligible_reason === 'string' ? value.ineligible_reason : null,
		expose_apps: typeof value.expose_apps === 'boolean' ? value.expose_apps : null,
		expose_agents: typeof value.expose_agents === 'boolean' ? value.expose_agents : null,
		yaml_declaration: value.yaml_declaration,
		layer: typeof value.layer === 'string' ? value.layer : ''
	};
}

function isAuthoringCatalogStatus(value: unknown): value is AuthoringCatalogStatus {
	return value === 'ok' || value === 'degraded' || value === 'unavailable';
}

function parseAgentEntry(value: unknown): AuthoringAgentEntry {
	if (!isRecord(value) || typeof value.name !== 'string' || typeof value.yaml_declaration !== 'string') {
		throw new Error('The authoring agent entry is invalid.');
	}
	return {
		name: value.name,
		display_name: typeof value.display_name === 'string' ? value.display_name : null,
		description: typeof value.description === 'string' ? value.description : '',
		default_runner: value.default_runner === true,
		yaml_declaration: value.yaml_declaration
	};
}

function parsePersonalityEntry(value: unknown): AuthoringPersonalityEntry {
	if (!isRecord(value) || typeof value.name !== 'string' || typeof value.yaml_declaration !== 'string') {
		throw new Error('The authoring personality entry is invalid.');
	}
	return {
		name: value.name,
		description: typeof value.description === 'string' ? value.description : '',
		yaml_declaration: value.yaml_declaration
	};
}

function parseProcedureEntry(value: unknown): AuthoringProcedureEntry {
	if (!isRecord(value) || typeof value.name !== 'string' || typeof value.yaml_declaration !== 'string') {
		throw new Error('The authoring procedure entry is invalid.');
	}
	return {
		name: value.name,
		version: typeof value.version === 'string' ? value.version : null,
		description: typeof value.description === 'string' ? value.description : '',
		yaml_declaration: value.yaml_declaration
	};
}

async function fetchAuthoringJson(path: string, signal?: AbortSignal): Promise<unknown> {
	const response = await fetch(path, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		signal
	});
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const declaredLength = Number(declared);
		if (!Number.isSafeInteger(declaredLength) || declaredLength < 0 || declaredLength > MAX_RESPONSE_BYTES) {
			throw new Error('The authoring catalog response is invalid.');
		}
	}
	const text = await response.text();
	if (new TextEncoder().encode(text).byteLength > MAX_RESPONSE_BYTES) {
		throw new Error('The authoring catalog response exceeded its size limit.');
	}
	let body: unknown = null;
	if (text) {
		try {
			body = JSON.parse(text);
		} catch {
			throw new Error('The authoring catalog returned invalid JSON.');
		}
	}
	if (!response.ok) {
		const message =
			isRecord(body) && typeof body.message === 'string'
				? body.message
				: 'The authoring catalog could not be loaded.';
		throw new Error(message);
	}
	return body;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export interface AuthoringToolBlockGroup {
	/** The shared reason these tools cannot run in apps yet. */
	reason: string;
	tools: AuthoringToolEntry[];
}

export interface AuthoringToolPartition {
	/** Eligible and dispatchable: runnable in an app today. */
	ready: AuthoringToolEntry[];
	/** Everything else, grouped by why, largest group first. */
	blocked: AuthoringToolBlockGroup[];
}

/**
 * Split the tool catalog into what an app can run today and what it cannot
 * yet, grouped by blocker. Listing every blocked tool with its own reason
 * buried the handful of runnable tools under ~150 near-identical notes.
 */
export function partitionAuthoringToolsForApps(
	entries: AuthoringToolEntry[]
): AuthoringToolPartition {
	const ready: AuthoringToolEntry[] = [];
	const groups = new Map<string, AuthoringToolEntry[]>();
	for (const entry of entries) {
		if (entry.app_eligible && entry.dispatchable !== false) {
			ready.push(entry);
			continue;
		}
		const reason =
			(!entry.app_eligible ? entry.ineligible_reason : entry.dispatch_note)?.trim() ||
			(entry.app_eligible ? 'Not dispatchable in apps yet' : 'Not eligible for apps');
		const rows = groups.get(reason) ?? [];
		rows.push(entry);
		groups.set(reason, rows);
	}
	const blocked = [...groups]
		.map(([reason, tools]) => ({ reason, tools }))
		.sort(
			(left, right) =>
				right.tools.length - left.tools.length || left.reason.localeCompare(right.reason)
		);
	return { ready, blocked };
}
