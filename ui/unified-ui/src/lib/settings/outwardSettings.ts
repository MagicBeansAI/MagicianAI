import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type OutwardMode = 'inherit' | 'capture' | 'live';
export type OutwardContext = 'owner_chat' | 'owner_tasks' | 'opc' | 'public_agents';

export interface OutwardSettings {
	capture_only: boolean;
	force_capture_only: boolean;
	/** `public_agents` is absent on servers older than its split from OPC. */
	contexts: Record<Exclude<OutwardContext, 'public_agents'>, OutwardMode> & { public_agents?: OutwardMode };
	browser_submission_guard: boolean;
	/**
	 * When a send is captured: `true` pauses the agent and asks the owner
	 * (Send, Discard or Always allow); `false` only records it. Absent on
	 * servers older than review-before-sending, and then never sent back.
	 */
	review_captured_sends?: boolean;
}

export interface OutwardEnvelope {
	revision: string;
	settings: OutwardSettings;
	applied: boolean;
	/** true means captured; false permits dispatch if other checks pass. */
	effective: Record<Exclude<OutwardContext, 'public_agents'> | 'unclassified', boolean> & { public_agents?: boolean };
}

export const OUTWARD_CONTEXTS: { key: OutwardContext; label: string; description: string }[] = [
	{ key: 'owner_chat', label: 'Your chats and delegated work', description: 'Private chat and voice requests, including specialists and tasks launched from those requests.' },
	{ key: 'owner_tasks', label: 'Tasks you create in Work', description: 'Tasks created through Work or VibeDev by you or your owner API token, including their child tasks and scheduled runs.' },
	{ key: 'opc', label: 'OPC and autonomous work', description: 'Company programme cycles and autonomous tasks. Manually starting a programme still uses this setting.' },
	{ key: 'public_agents', label: 'Envoy and public agents', description: 'Agents that speak for you to people outside: Envoy, bots answering in public or shared channels, and the ambassador. Kept apart from autonomous work so it can send while these stay captured.' }
];

/** The contexts this server knows; an older one has no `public_agents`. */
export function contextsOf(settings: OutwardSettings): typeof OUTWARD_CONTEXTS {
	return OUTWARD_CONTEXTS.filter(({ key }) => settings.contexts?.[key] !== undefined);
}

export function captures(settings: OutwardSettings, context: OutwardContext): boolean {
	if (settings.force_capture_only) return true;
	const mode = settings.contexts[context] ?? 'inherit';
	return mode === 'inherit' ? settings.capture_only : mode === 'capture';
}

function validEnvelope(value: unknown): value is OutwardEnvelope {
	if (!value || typeof value !== 'object') return false;
	const envelope = value as OutwardEnvelope;
	const settings = envelope.settings;
	return typeof envelope.revision === 'string' && envelope.revision.length > 0
		&& typeof envelope.applied === 'boolean' && !!settings
		&& typeof settings.capture_only === 'boolean'
		&& typeof settings.force_capture_only === 'boolean'
		&& typeof settings.browser_submission_guard === 'boolean'
		&& (settings.review_captured_sends === undefined || typeof settings.review_captured_sends === 'boolean')
		&& OUTWARD_CONTEXTS.filter(({ key }) => key !== 'public_agents' || settings.contexts?.public_agents !== undefined)
			.every(({ key }) => ['inherit', 'capture', 'live'].includes(settings.contexts?.[key] as string))
		&& ['owner_chat', 'owner_tasks', 'opc', 'unclassified'].every((key) => typeof envelope.effective?.[key as keyof OutwardEnvelope['effective']] === 'boolean');
}

async function request(method: 'GET' | 'PUT', body?: unknown): Promise<OutwardEnvelope> {
	const response = await timedFetch('/api/magician/v2/settings/outward-actions', {
		method,
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
		...(body === undefined ? {} : { body: JSON.stringify(body) })
	});
	const result = await response.json().catch(() => null);
	if (!response.ok) {
		throw new Error(result?.message || `Could not ${method === 'GET' ? 'load' : 'save'} outward settings (${response.status}).`);
	}
	if (!validEnvelope(result)) throw new Error('The server returned invalid outward settings. Reload to retry.');
	return result;
}

export const loadOutwardSettings = () => request('GET');
export const saveOutwardSettings = (settings: OutwardSettings, expectedRevision: string) =>
	request('PUT', { settings, expected_revision: expectedRevision });

/** A standing "Always allow" permission: this agent may send to this recipient. */
export interface OutwardGrant {
	id: string;
	agent_id: string;
	capability: string;
	recipient: string;
	granted_at_ms: number;
}

const GRANTS_URL = '/api/magician/v2/settings/outward-actions/grants';

function validGrant(value: unknown): value is OutwardGrant {
	if (!value || typeof value !== 'object') return false;
	const grant = value as OutwardGrant;
	return typeof grant.id === 'string' && grant.id.length > 0
		&& typeof grant.agent_id === 'string'
		&& typeof grant.capability === 'string'
		&& typeof grant.recipient === 'string'
		&& typeof grant.granted_at_ms === 'number';
}

/**
 * The owner's always-allowed agent → recipient pairs, or `null` when the
 * server predates them (404), so the panel hides the list.
 */
export async function loadOutwardGrants(): Promise<OutwardGrant[] | null> {
	const response = await timedFetch(GRANTS_URL, {
		method: 'GET',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' })
	});
	if (response.status === 404) return null;
	const result = await response.json().catch(() => null);
	if (!response.ok) throw new Error(result?.message || `Could not load always-allowed sends (${response.status}).`);
	const grants = (result as { grants?: unknown } | null)?.grants;
	if (!Array.isArray(grants) || !grants.every(validGrant)) {
		throw new Error('The server returned invalid always-allowed sends. Reload to retry.');
	}
	return grants;
}

/** Revoke one standing permission. A grant already gone (404) counts as revoked. */
export async function revokeOutwardGrant(id: string): Promise<void> {
	const response = await timedFetch(`${GRANTS_URL}/${encodeURIComponent(id)}`, {
		method: 'DELETE',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' })
	});
	if (response.ok || response.status === 404) return;
	const result = await response.json().catch(() => null);
	throw new Error(result?.message || `Could not revoke this permission (${response.status}).`);
}
