import { browser } from '$app/environment';
import { timedFetch } from '$lib/shared/fetch';

const API_BASE = '/api/magician/v2';
const SETUP_TOKEN_STORAGE_KEY = 'magician:vault:setup-token';

export type SameSite = 'strict' | 'lax' | 'none';
export type SecretInjectionKind = 'header' | 'form_fields' | 'cookies';

export interface SecretPolicy {
	allowed_tools: string[];
	allowed_domains: string[];
	max_uses_per_day?: number;
	requires_approval: boolean;
}

export interface SupportedPolicyRoutes {
	http: string[];
	browser: string[];
}

export interface SetupTokenStatus {
	configured: boolean;
	available: boolean;
	header_name: string;
	pending_acknowledgement: boolean;
	created_at?: number;
	acknowledged_at?: number;
	pending_token?: string;
	unavailable_reason?: string;
	supported_policy_routes: SupportedPolicyRoutes;
}

export interface SecretSummary {
	id: string;
	label: string;
	created_at: number;
	field_names: string[];
	injection: SecretInjectionTarget;
	policy: SecretPolicy;
}

export interface SecretDetail extends SecretSummary {
	fields?: Record<string, string>;
}

export interface PendingSecretApproval {
	challenge_id: string;
	secret_id: string;
	secret_label: string;
	tool: string;
	action: string;
	domain?: string;
	/** A domain set, or `["*"]` for all sites (MagicVault scoped requests). */
	domains?: string[];
	expires_at: number;
}

/** What an approval lets the key reach: one domain, a set, or all sites. */
export function approvalDomainLabel(approval: Pick<PendingSecretApproval, 'domain' | 'domains'>): string {
	if (approval.domains?.length === 1 && approval.domains[0] === '*') return 'All sites';
	if (approval.domains?.length) return approval.domains.join(', ');
	return approval.domain || 'n/a';
}

export interface SecretVaultOverview {
	setup: SetupTokenStatus;
	secrets: SecretSummary[];
	approvals: PendingSecretApproval[];
}

export interface SecretFieldRow {
	key: string;
	value: string;
}

export interface SecretMappingRow {
	source: string;
	target: string;
}

export interface SecretCookieRow {
	name: string;
	domain: string;
	path: string;
	secure: boolean;
	http_only: boolean;
	same_site: '' | SameSite;
	expires: string;
}

export interface SecretDraft {
	id: string;
	label: string;
	fields: SecretFieldRow[];
	injectionKind: SecretInjectionKind;
	headerName: string;
	headerPrefix: string;
	formMappings: SecretMappingRow[];
	cookies: SecretCookieRow[];
	allowedToolsText: string;
	allowedDomainsText: string;
	maxUsesPerDay: string;
	requiresApproval: boolean;
}

export class SecretVaultApiError extends Error {
	status: number;
	code?: string;
	details?: Record<string, unknown> | null;

	constructor(message: string, status: number, code?: string, details?: Record<string, unknown> | null) {
		super(message);
		this.name = 'SecretVaultApiError';
		this.status = status;
		this.code = code;
		this.details = details ?? null;
	}
}

interface ParsedApiError {
	message: string;
	status: number;
	code?: string;
	details?: Record<string, unknown> | null;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(record: Record<string, unknown>, field: string): string | undefined {
	const value = record[field];
	return typeof value === 'string' ? value : undefined;
}

function readNumber(record: Record<string, unknown>, field: string): number | undefined {
	const value = record[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function readBoolean(record: Record<string, unknown>, field: string): boolean | undefined {
	const value = record[field];
	return typeof value === 'boolean' ? value : undefined;
}

function normalizeTimestamp(value: unknown): number | undefined {
	if (typeof value === 'number' && Number.isFinite(value)) {
		return value < 1_000_000_000_000 ? value * 1000 : value;
	}
	if (typeof value === 'string' && value.trim().length > 0) {
		const parsed = Date.parse(value);
		return Number.isFinite(parsed) ? parsed : undefined;
	}
	return undefined;
}

function ensurePolicy(raw: unknown): SecretPolicy {
	const record = asRecord(raw);
	const allowedTools = Array.isArray(record?.allowed_tools)
		? record?.allowed_tools.filter((value): value is string => typeof value === 'string')
		: [];
	const allowedDomains = Array.isArray(record?.allowed_domains)
		? record?.allowed_domains.filter((value): value is string => typeof value === 'string')
		: [];
	const maxUses = record ? readNumber(record, 'max_uses_per_day') : undefined;
	return {
		allowed_tools: allowedTools,
		allowed_domains: allowedDomains,
		max_uses_per_day: maxUses,
		requires_approval: record ? readBoolean(record, 'requires_approval') === true : false
	};
}

function parseSupportedPolicyRoutes(raw: unknown): SupportedPolicyRoutes {
	const record = asRecord(raw);
	const http = Array.isArray(record?.http)
		? record.http.filter((value): value is string => typeof value === 'string')
		: [];
	const browserRoutes = Array.isArray(record?.browser)
		? record.browser.filter((value): value is string => typeof value === 'string')
		: [];
	return {
		http,
		browser: browserRoutes
	};
}

function parseInjectionTarget(raw: unknown): SecretInjectionTarget {
	const record = asRecord(raw);
	const kind = readString(record || {}, 'kind');
	if (kind === 'header') {
		return {
			kind: 'header',
			name: readString(record || {}, 'name') || '',
			prefix: readString(record || {}, 'prefix')
		};
	}
	if (kind === 'form_fields') {
		const mappingRecord = asRecord(record?.mapping) || {};
		const mapping: Record<string, string> = {};
		for (const [key, value] of Object.entries(mappingRecord)) {
			if (typeof value === 'string') {
				mapping[key] = value;
			}
		}
		return { kind: 'form_fields', mapping };
	}
	const cookies: SecretCookieSpec[] = [];
	if (Array.isArray(record?.cookies)) {
		for (const entry of record.cookies) {
			const cookie = asRecord(entry);
			if (!cookie) continue;
			const name = readString(cookie, 'name');
			const domain = readString(cookie, 'domain');
			const path = readString(cookie, 'path');
			if (!name || !domain || !path) continue;
			const sameSite = readString(cookie, 'same_site');
			const normalizedSameSite =
				sameSite === 'strict' || sameSite === 'lax' || sameSite === 'none' ? sameSite : undefined;
			cookies.push({
				name,
				domain,
				path,
				secure: readBoolean(cookie, 'secure') === true,
				http_only: readBoolean(cookie, 'http_only') === true,
				same_site: normalizedSameSite,
				expires: readNumber(cookie, 'expires')
			});
		}
	}
	return {
		kind: 'cookies',
		cookies
	};
}

function parseSecretSummary(raw: unknown): SecretSummary | null {
	const record = asRecord(raw);
	if (!record) return null;
	const id = readString(record, 'id');
	const label = readString(record, 'label');
	if (!id || !label) return null;
	return {
		id,
		label,
		created_at: normalizeTimestamp(record.created_at) ?? Date.now(),
		field_names: Array.isArray(record.field_names)
			? record.field_names.filter((value): value is string => typeof value === 'string')
			: [],
		injection: parseInjectionTarget(record.injection),
		policy: ensurePolicy(record.policy)
	};
}

function parseSecretDetail(raw: unknown): SecretDetail | null {
	const record = asRecord(raw);
	const secret = record ? asRecord(record.secret) : null;
	if (!secret) return null;
	const summary = parseSecretSummary(secret);
	if (!summary) return null;
	const fieldsRecord = asRecord(secret.fields);
	const fields: Record<string, string> | undefined = fieldsRecord
		? Object.fromEntries(
				Object.entries(fieldsRecord).filter((entry): entry is [string, string] => typeof entry[1] === 'string')
			)
		: undefined;
	return { ...summary, fields };
}

function parseSetupTokenStatus(raw: unknown): SetupTokenStatus | null {
	const record = asRecord(raw);
	if (!record) return null;
	const headerName = readString(record, 'header_name');
	if (!headerName) return null;
	return {
		configured: readBoolean(record, 'configured') !== false,
		available: readBoolean(record, 'available') !== false,
		header_name: headerName,
		pending_acknowledgement: readBoolean(record, 'pending_acknowledgement') === true,
		created_at: normalizeTimestamp(record.created_at),
		acknowledged_at: normalizeTimestamp(record.acknowledged_at),
		pending_token: readString(record, 'pending_token'),
		unavailable_reason: readString(record, 'unavailable_reason'),
		supported_policy_routes: parseSupportedPolicyRoutes(record.supported_policy_routes)
	};
}

function parseApprovals(raw: unknown): PendingSecretApproval[] {
	const record = asRecord(raw);
	if (!record || !Array.isArray(record.approvals)) return [];
	const approvals: PendingSecretApproval[] = [];
	for (const entry of record.approvals) {
		const approval = asRecord(entry);
		if (!approval) continue;
		const challengeId = readString(approval, 'challenge_id');
		const secretId = readString(approval, 'secret_id');
		const secretLabel = readString(approval, 'secret_label');
		const tool = readString(approval, 'tool');
		const action = readString(approval, 'action');
		if (!challengeId || !secretId || !secretLabel || !tool || !action) continue;
		approvals.push({
			challenge_id: challengeId,
			secret_id: secretId,
			secret_label: secretLabel,
			tool,
			action,
			domain: readString(approval, 'domain'),
			...(Array.isArray(approval.domains)
				? {
						domains: approval.domains.filter(
							(value: unknown): value is string => typeof value === 'string'
						)
					}
				: {}),
			expires_at: normalizeTimestamp(approval.expires_at) ?? Date.now()
		});
	}
	return approvals;
}

async function readApiError(response: Response): Promise<ParsedApiError> {
	let message = `Request failed (${response.status})`;
	let code: string | undefined;
	let details: Record<string, unknown> | null = null;
	try {
		const text = await response.text();
		if (!text) {
			return { message, status: response.status };
		}
		try {
			const parsed = JSON.parse(text) as unknown;
			const record = asRecord(parsed);
			const error = record ? readString(record, 'error') : undefined;
			code = record ? readString(record, 'code') : undefined;
			details = record ? asRecord(record.details) : null;
			message = error || text;
		} catch {
			message = text;
		}
	} catch {
		// Best effort only.
	}
	return { message, status: response.status, code, details };
}

function buildHeaders(token?: string): Headers {
	const headers = new Headers({
		Accept: 'application/json'
	});
	if (token && token.trim().length > 0) {
		headers.set('X-Magician-Setup-Token', token.trim());
	}
	return headers;
}

async function fetchJson(path: string, init?: RequestInit, token?: string): Promise<unknown> {
	const headers = buildHeaders(token);
	if (init?.body) {
		headers.set('Content-Type', 'application/json');
	}
	if (init?.headers) {
		const extraHeaders = new Headers(init.headers);
		extraHeaders.forEach((value, key) => headers.set(key, value));
	}
	const response = await timedFetch(`${API_BASE}${path}`, {
		...init,
		headers
	});
	if (!response.ok) {
		const parsed = await readApiError(response);
		throw new SecretVaultApiError(parsed.message, parsed.status, parsed.code, parsed.details);
	}
	if (response.status === 204) return null;
	return response.json();
}

function splitListInput(value: string): string[] {
	return value
		.split(/[\n,]/)
		.map((entry) => entry.trim())
		.filter((entry) => entry.length > 0);
}

function normalizeFieldRows(rows: SecretFieldRow[]): Record<string, string> {
	const fields: Record<string, string> = {};
	for (const row of rows) {
		const key = row.key.trim();
		const value = row.value;
		if (!key && value.trim().length === 0) continue;
		if (!key || value.trim().length === 0) {
			throw new Error('Secret fields require both a key and a value.');
		}
		fields[key] = value;
	}
	return fields;
}

function normalizeFormMappings(rows: SecretMappingRow[]): Record<string, string> {
	const mapping: Record<string, string> = {};
	for (const row of rows) {
		const source = row.source.trim();
		const target = row.target.trim();
		if (!source && !target) continue;
		if (!source || !target) {
			throw new Error('Form mappings require both a source field and a target field.');
		}
		mapping[source] = target;
	}
	return mapping;
}

function normalizeCookies(rows: SecretCookieRow[]): SecretCookieSpec[] {
	const cookies: SecretCookieSpec[] = [];
	for (const row of rows) {
		const name = row.name.trim();
		const domain = row.domain.trim();
		const path = row.path.trim();
		if (!name && !domain && !path && row.expires.trim().length === 0) continue;
		if (!name || !domain || !path) {
			throw new Error('Cookie rows require name, domain, and path.');
		}
		const expires = row.expires.trim().length > 0 ? Number(row.expires.trim()) : undefined;
		if (expires !== undefined && !Number.isFinite(expires)) {
			throw new Error(`Cookie "${name}" has an invalid expiry timestamp.`);
		}
		cookies.push({
			name,
			domain,
			path,
			secure: row.secure,
			http_only: row.http_only,
			same_site: row.same_site || undefined,
			expires
		});
	}
	return cookies;
}

export type SecretInjectionTarget =
	| {
			kind: 'header';
			name: string;
			prefix?: string;
	  }
	| {
			kind: 'form_fields';
			mapping: Record<string, string>;
	  }
	| {
			kind: 'cookies';
			cookies: SecretCookieSpec[];
	  };

export interface SecretCookieSpec {
	name: string;
	domain: string;
	path: string;
	secure: boolean;
	http_only: boolean;
	same_site?: SameSite;
	expires?: number;
}

export function createEmptySecretDraft(): SecretDraft {
	return {
		id: '',
		label: '',
		fields: [{ key: 'value', value: '' }],
		injectionKind: 'header',
		headerName: '',
		headerPrefix: '',
		formMappings: [{ source: '', target: '' }],
		cookies: [{ name: '', domain: '', path: '/', secure: true, http_only: true, same_site: '', expires: '' }],
		allowedToolsText: '',
		allowedDomainsText: '',
		maxUsesPerDay: '',
		requiresApproval: false
	};
}

export function draftFromSecret(detail: SecretDetail): SecretDraft {
	const base = createEmptySecretDraft();
	const fields = detail.fields
		? Object.entries(detail.fields).map(([key, value]) => ({ key, value }))
		: base.fields;
	const draft: SecretDraft = {
		...base,
		id: detail.id,
		label: detail.label,
		fields,
		allowedToolsText: detail.policy.allowed_tools.join('\n'),
		allowedDomainsText: detail.policy.allowed_domains.join('\n'),
		maxUsesPerDay:
			detail.policy.max_uses_per_day !== undefined ? String(detail.policy.max_uses_per_day) : '',
		requiresApproval: detail.policy.requires_approval
	};
	if (detail.injection.kind === 'header') {
		draft.injectionKind = 'header';
		draft.headerName = detail.injection.name;
		draft.headerPrefix = detail.injection.prefix || '';
	} else if (detail.injection.kind === 'form_fields') {
		draft.injectionKind = 'form_fields';
		draft.formMappings = Object.entries(detail.injection.mapping).map(([source, target]) => ({
			source,
			target
		}));
		if (draft.formMappings.length === 0) {
			draft.formMappings = [{ source: '', target: '' }];
		}
	} else {
		draft.injectionKind = 'cookies';
		draft.cookies = detail.injection.cookies.map((cookie) => ({
			name: cookie.name,
			domain: cookie.domain,
			path: cookie.path,
			secure: cookie.secure,
			http_only: cookie.http_only,
			same_site: cookie.same_site || '',
			expires: cookie.expires !== undefined ? String(cookie.expires) : ''
		}));
		if (draft.cookies.length === 0) {
			draft.cookies = base.cookies;
		}
	}
	return draft;
}

export function buildSecretRequestFromDraft(
	draft: SecretDraft,
	mode: 'create' | 'update'
): Record<string, unknown> {
	const label = draft.label.trim();
	if (!label) {
		throw new Error('Secret label is required.');
	}

	const fields = normalizeFieldRows(draft.fields);
	if (Object.keys(fields).length === 0) {
		throw new Error('At least one secret field is required.');
	}

	let injection: SecretInjectionTarget;
	if (draft.injectionKind === 'header') {
		const name = draft.headerName.trim();
		if (!name) {
			throw new Error('Header injection requires a header name.');
		}
		injection = {
			kind: 'header',
			name,
			prefix: draft.headerPrefix.trim().length > 0 ? draft.headerPrefix : undefined
		};
	} else if (draft.injectionKind === 'form_fields') {
		const mapping = normalizeFormMappings(draft.formMappings);
		if (Object.keys(mapping).length === 0) {
			throw new Error('Form field injection requires at least one mapping.');
		}
		injection = { kind: 'form_fields', mapping };
	} else {
		const cookies = normalizeCookies(draft.cookies);
		if (cookies.length === 0) {
			throw new Error('Cookie injection requires at least one cookie.');
		}
		injection = { kind: 'cookies', cookies };
	}

	const maxUses = draft.maxUsesPerDay.trim();
	const maxUsesValue =
		maxUses.length > 0
			? (() => {
					const parsed = Number(maxUses);
					if (!Number.isInteger(parsed) || parsed <= 0) {
						throw new Error('Max uses per day must be a positive integer.');
					}
					return parsed;
				})()
			: undefined;

	const request: Record<string, unknown> = {
		label,
		fields,
		injection,
		policy: {
			allowed_tools: splitListInput(draft.allowedToolsText),
			allowed_domains: splitListInput(draft.allowedDomainsText),
			max_uses_per_day: maxUsesValue,
			requires_approval: draft.requiresApproval
		}
	};

	if (mode === 'create') {
		const id = draft.id.trim();
		if (!id) {
			throw new Error('Secret id is required.');
		}
		request.id = id;
	}

	return request;
}

export function getStoredSetupToken(): string {
	if (!browser) return '';
	try {
		return sessionStorage.getItem(SETUP_TOKEN_STORAGE_KEY) || '';
	} catch {
		return '';
	}
}

export function storeSetupToken(token: string): void {
	if (!browser) return;
	try {
		if (token.trim().length === 0) {
			sessionStorage.removeItem(SETUP_TOKEN_STORAGE_KEY);
		} else {
			sessionStorage.setItem(SETUP_TOKEN_STORAGE_KEY, token.trim());
		}
	} catch {
		// Best effort only.
	}
}

export async function loadSecretVaultOverview(token?: string): Promise<SecretVaultOverview> {
	const setupRaw = await fetchJson('/secrets/setup-token');

	const setup = parseSetupTokenStatus(setupRaw);
	if (!setup) {
		throw new Error('Secret vault setup state is invalid.');
	}

	if (!setup.available) {
		return {
			setup,
			secrets: [],
			approvals: []
		};
	}

	const [secretsRaw, approvalsRaw] = await Promise.all([
		fetchJson('/secrets'),
		fetchJson('/secrets/approvals')
	]);

	const secretsRecord = asRecord(secretsRaw);
	const secrets = Array.isArray(secretsRecord?.secrets)
		? secretsRecord.secrets
				.map((entry) => parseSecretSummary(entry))
				.filter((entry): entry is SecretSummary => entry !== null)
		: [];

	return {
		setup,
		secrets,
		approvals: parseApprovals(approvalsRaw)
	};
}

export async function loadSecretDetail(
	secretId: string,
	token?: string,
	includeFields: boolean = false
): Promise<SecretDetail> {
	const query = includeFields ? '?include_fields=true' : '';
	const detail = parseSecretDetail(
		await fetchJson(`/secrets/${encodeURIComponent(secretId)}${query}`, undefined, token)
	);
	if (!detail) {
		throw new Error('Secret detail response is invalid.');
	}
	return detail;
}

export async function createSecret(token: string, draft: SecretDraft): Promise<SecretDetail> {
	const detail = parseSecretDetail(
		await fetchJson(
			'/secrets',
			{
				method: 'POST',
				body: JSON.stringify(buildSecretRequestFromDraft(draft, 'create'))
			},
			token
		)
	);
	if (!detail) {
		throw new Error('Create secret response is invalid.');
	}
	return detail;
}

export async function updateSecret(
	token: string,
	secretId: string,
	draft: SecretDraft
): Promise<SecretDetail> {
	const detail = parseSecretDetail(
		await fetchJson(
			`/secrets/${encodeURIComponent(secretId)}`,
			{
				method: 'PUT',
				body: JSON.stringify(buildSecretRequestFromDraft(draft, 'update'))
			},
			token
		)
	);
	if (!detail) {
		throw new Error('Update secret response is invalid.');
	}
	return detail;
}

export async function deleteSecret(token: string, secretId: string): Promise<boolean> {
	const record = asRecord(
		await fetchJson(
			`/secrets/${encodeURIComponent(secretId)}`,
			{
				method: 'DELETE'
			},
			token
		)
	);
	return record ? readBoolean(record, 'deleted') === true : false;
}

export async function approveSecretChallenge(token: string, challengeId: string): Promise<boolean> {
	const record = asRecord(
		await fetchJson(
			'/secrets/approve',
			{
				method: 'POST',
				body: JSON.stringify({ challenge_id: challengeId })
			},
			token
		)
	);
	return record ? readBoolean(record, 'approved') === true : false;
}

export async function acknowledgeSetupToken(token: string): Promise<SetupTokenStatus> {
	const status = parseSetupTokenStatus(
		await fetchJson('/secrets/setup-token/acknowledge', {
			method: 'POST',
			body: JSON.stringify({ token })
		})
	);
	if (!status) {
		throw new Error('Setup token acknowledgement response is invalid.');
	}
	return status;
}

export async function rotateSetupToken(token: string): Promise<SetupTokenStatus> {
	const status = parseSetupTokenStatus(
		await fetchJson(
			'/secrets/setup-token/rotate',
			{
				method: 'POST'
			},
			token
		)
	);
	if (!status) {
		throw new Error('Setup token rotation response is invalid.');
	}
	return status;
}
