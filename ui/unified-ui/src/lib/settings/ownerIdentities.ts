// Owner identities (`envoy.owner_identities` + `owner_identity_envs`) and
// one-time `/claim` codes: owner-only endpoints.
import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export interface OwnerIdentity {
	index: number;
	/** Last two characters only; the full address never reaches the browser. */
	masked: string;
	/** `config`: magician-config.yaml; `env`: the channel's identity variable. */
	source: 'config' | 'env';
	removable: boolean;
}

export interface OwnerIdentityChannel {
	channel: string;
	identities: OwnerIdentity[];
	/** A `/claim` code can be redeemed from this channel. */
	claimable: boolean;
}

export interface OwnerIdentitiesState {
	channels: OwnerIdentityChannel[];
	claimable_channels: string[];
	code_ttl_secs: number;
}

export interface ClaimCode {
	code: string;
	expires_at_ms: number;
	channel: string | null;
	instructions: string;
}

const ENDPOINT = '/api/magician/v2/owner-identities';

async function call(path: string, init: RequestInit): Promise<unknown> {
	const response = await timedFetch(path, {
		...init,
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		cache: 'no-store'
	});
	const text = await response.text();
	let body: unknown = null;
	try {
		body = text ? JSON.parse(text) : null;
	} catch {
		body = null;
	}
	if (!response.ok) {
		const record = (body ?? {}) as Record<string, unknown>;
		const message = typeof record.message === 'string' ? record.message : text || `Request failed (${response.status})`;
		throw new Error(message);
	}
	return body;
}

export function parseOwnerIdentities(body: unknown): OwnerIdentitiesState {
	const raw = (body ?? {}) as Record<string, unknown>;
	if (!Array.isArray(raw.channels)) throw new Error('Malformed owner identities response');
	const channels = raw.channels.map((entry): OwnerIdentityChannel => {
		const channel = (entry ?? {}) as Record<string, unknown>;
		const identities = Array.isArray(channel.identities) ? channel.identities : [];
		return {
			channel: String(channel.channel ?? ''),
			claimable: channel.claimable === true,
			identities: identities.map((item): OwnerIdentity => {
				const identity = (item ?? {}) as Record<string, unknown>;
				return {
					index: typeof identity.index === 'number' ? identity.index : 0,
					masked: String(identity.masked ?? ''),
					source: identity.source === 'config' ? 'config' : 'env',
					removable: identity.removable === true
				};
			})
		};
	});
	return {
		channels,
		claimable_channels: Array.isArray(raw.claimable_channels) ? raw.claimable_channels.map(String) : [],
		code_ttl_secs: typeof raw.code_ttl_secs === 'number' ? raw.code_ttl_secs : 600
	};
}

export async function fetchOwnerIdentities(): Promise<OwnerIdentitiesState> {
	return parseOwnerIdentities(await call(ENDPOINT, { method: 'GET' }));
}

export async function mintClaimCode(channel?: string): Promise<ClaimCode> {
	const body = (await call(`${ENDPOINT}/claim-code`, {
		method: 'POST',
		body: JSON.stringify(channel ? { channel } : {})
	})) as Record<string, unknown> | null;
	if (!body || typeof body.code !== 'string') throw new Error('Malformed claim code response');
	return {
		code: body.code,
		expires_at_ms: typeof body.expires_at_ms === 'number' ? body.expires_at_ms : Date.now() + 600_000,
		channel: typeof body.channel === 'string' ? body.channel : null,
		instructions: typeof body.instructions === 'string' ? body.instructions : `Send /claim ${body.code} to the Magician bot.`
	};
}

export async function removeOwnerIdentity(channel: string, index: number): Promise<OwnerIdentitiesState> {
	return parseOwnerIdentities(
		await call(`${ENDPOINT}/${encodeURIComponent(channel)}/${index}`, { method: 'DELETE' })
	);
}

/** "kapso" → "WhatsApp (Kapso)": what the owner recognises. */
export function channelDisplayName(channel: string): string {
	switch (channel) {
		case 'kapso':
			return 'WhatsApp (Kapso)';
		case 'whatsapp':
			return 'WhatsApp';
		case 'telegram':
			return 'Telegram';
		case 'agentmail':
			return 'Email (AgentMail)';
		case 'imessage':
			return 'iMessage';
		case 'sms':
			return 'SMS';
		default:
			return channel;
	}
}
