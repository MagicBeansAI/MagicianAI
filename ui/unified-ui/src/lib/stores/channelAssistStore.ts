/**
 * Channel-toggle consent surface for mail & chat — the client half of the
 * `/channel-assist/channels` API. Lists discovered accounts across channels
 * (gmail, the user's WhatsApp, Presto's Kapso WhatsApp) merged with their
 * state in the unified `channel_observe` config (unified observe+assist U4 —
 * the standalone `channel_assist` registry is retired), and writes the FULL
 * set back so replacing the message channels can never silently drop an
 * account. Enabling an account here = both observed (work evidence) AND
 * assisted (follow-ups, drafts): one pipeline, one toggle.
 */

export type ChannelLane = 'user_assist' | 'envoy';

export interface ChannelCapabilities {
	pull_ingest?: boolean;
	content_fetch?: boolean;
	deep_link?: boolean;
	connection_status?: boolean;
	realtime_events?: boolean;
	outbound_send?: boolean;
	draft_create?: boolean;
	attachments?: boolean;
	reactions?: boolean;
	edits_and_deletes?: boolean;
}

export interface ChannelAssistChannel {
	/** 'gmail' | 'whatsapp' | 'whatsapp_kapso'. */
	provider: string;
	/** Backend-provided provider display label. */
	provider_display?: string;
	/** User-facing channel name, e.g. email or whatsapp. */
	channel?: string;
	/** Generic channel kind label, e.g. email, chat, or message. */
	channel_label?: string;
	/** Backend-declared adapter capabilities. */
	capabilities?: ChannelCapabilities;
	account_alias: string;
	/** Human label (email for gmail, "self"/"presto" otherwise). */
	display: string;
	lane: ChannelLane;
	/** Credentials/profile/db present for this account. */
	connected: boolean;
	/** Enabled in the registry (syncs on the next pass). */
	enabled: boolean;
	thread_count: number;
	message_count: number;
	/** Purposes the owner granted beyond observation (`verification_codes`). */
	purposes?: string[];
}

/** The purpose that lets automatic verification-code retrieval read an account. */
export const VERIFICATION_CODES_PURPOSE = 'verification_codes';

export function hasVerificationCodesPurpose(channel: ChannelAssistChannel): boolean {
	return Array.isArray(channel.purposes) && channel.purposes.includes(VERIFICATION_CODES_PURPOSE);
}

/** Whether an account's channel is one the resolver can read at all. */
export function supportsVerificationCodes(channel: ChannelAssistChannel): boolean {
	return channel.provider === 'gmail' || channel.provider === 'agentmail' || channel.provider === 'imessage';
}

/**
 * Grant or withdraw the verification-code purpose on one configured account
 * (secure HITL P6). Separate from enablement on purpose: observing an inbox
 * never silently becomes authority to read login codes from it.
 */
export async function setChannelVerificationCodes(
	provider: string,
	accountAlias: string,
	granted: boolean
): Promise<{ ok: true } | { ok: false; error: string }> {
	try {
		const res = await fetch(`${CHANNEL_ASSIST_CHANNELS_ENDPOINT}/purpose`, {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({
				provider,
				account_alias: accountAlias,
				purpose: VERIFICATION_CODES_PURPOSE,
				granted
			})
		});
		if (!res.ok) {
			const body = await res.json().catch(() => ({}));
			return { ok: false, error: body?.message || body?.error || `HTTP ${res.status}` };
		}
		return { ok: true };
	} catch (e) {
		return { ok: false, error: e instanceof Error ? e.message : String(e) };
	}
}

export interface ChannelUpdateRow {
	provider: string;
	account_alias: string;
	lane: ChannelLane;
	enabled: boolean;
}

export interface ChannelAssistChannelsView {
	channels: ChannelAssistChannel[];
	history_lookback_days: number;
	history_lookback_options: number[];
	ok: boolean;
	error?: string;
}

const CHANNEL_ASSIST_CHANNELS_ENDPOINT = '/api/magician/v2/channel-assist/channels';

/** Discovered accounts + registry state. Errors carry `ok=false`. */
export async function fetchChannelAssistChannels(): Promise<ChannelAssistChannelsView> {
	const fallback = (error: string): ChannelAssistChannelsView => ({
		channels: [],
		history_lookback_days: 7,
		history_lookback_options: [1, 5, 7, 14, 30],
		ok: false,
		error
	});
	try {
		const res = await fetch(CHANNEL_ASSIST_CHANNELS_ENDPOINT);
		if (!res.ok) {
			const body = await res.json().catch(() => ({}));
			return fallback(body?.error || `HTTP ${res.status}`);
		}
		const body = await res.json();
		return {
			channels: Array.isArray(body?.channels) ? (body.channels as ChannelAssistChannel[]) : [],
			history_lookback_days:
				typeof body?.history_lookback_days === 'number' ? body.history_lookback_days : 7,
			history_lookback_options: Array.isArray(body?.history_lookback_options)
				? body.history_lookback_options.filter((v: unknown): v is number => typeof v === 'number')
				: [1, 5, 7, 14, 30],
			ok: true
		};
	} catch (e) {
		return fallback(e instanceof Error ? e.message : String(e));
	}
}

/** Persist the full channel registry. Returns the refreshed view or an error. */
export async function saveChannelAssistChannels(
	accounts: ChannelUpdateRow[],
	historyLookbackDays: number
): Promise<{ ok: true; view: ChannelAssistChannelsView } | { ok: false; error: string }> {
	try {
		const res = await fetch(CHANNEL_ASSIST_CHANNELS_ENDPOINT, {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ accounts, history_lookback_days: historyLookbackDays })
		});
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		return {
			ok: true,
			view: {
				channels: Array.isArray(body?.channels) ? body.channels : [],
				history_lookback_days:
					typeof body?.history_lookback_days === 'number'
						? body.history_lookback_days
						: historyLookbackDays,
				history_lookback_options: Array.isArray(body?.history_lookback_options)
					? body.history_lookback_options.filter((v: unknown): v is number => typeof v === 'number')
					: [1, 5, 7, 14, 30],
				ok: true
			}
		};
	} catch (e) {
		return { ok: false, error: e instanceof Error ? e.message : String(e) };
	}
}

/** Display name for a provider key, preferring backend adapter metadata. */
export function channelProviderLabel(
	provider: string,
	channel?: Pick<ChannelAssistChannel, 'provider_display'>
): string {
	if (channel?.provider_display?.trim()) return channel.provider_display.trim();
	switch (provider) {
		case 'gmail':
			return 'Gmail';
		case 'agentmail':
			return 'AgentMail (Presto)';
		case 'whatsapp':
			return 'WhatsApp (yours)';
		case 'whatsapp_kapso':
			return "WhatsApp (Presto)";
		default:
			return provider;
	}
}

/** Short lane label for the row badge. */
export function laneLabel(lane: ChannelLane): string {
	return lane === 'envoy' ? 'Magican' : 'You';
}
