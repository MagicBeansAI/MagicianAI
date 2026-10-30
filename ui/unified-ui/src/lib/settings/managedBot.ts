/** Client side of the managed-bot connect flow (Settings → Service accounts). */
export type BotConnectMethod = 'login_url' | 'code' | 'pairing_code' | 'qr';
export type BotPendingKind = 'phone' | 'code' | 'password' | 'pairing_code' | 'login_url' | 'qr';
export type BotConnectStatus = 'connected' | 'needs_sign_in' | 'authenticating' | 'unknown' | 'unavailable';

export type BotSignIn = { mode?: string; ticket?: string; viewer_path?: string; expires_in?: number };
export type BotPending = {
	kind: BotPendingKind;
	label?: string;
	pairing_code?: string;
	qr_data_url?: string;
	sign_in?: BotSignIn;
};
export type BotConnectState = { bot: string; status: BotConnectStatus; detail?: string; pending: BotPending | null };

const METHODS: readonly BotConnectMethod[] = ['login_url', 'code', 'pairing_code', 'qr'];
const PENDING: readonly BotPendingKind[] = ['phone', 'code', 'password', 'pairing_code', 'login_url', 'qr'];
const STATUSES: readonly BotConnectStatus[] = ['connected', 'needs_sign_in', 'authenticating', 'unknown', 'unavailable'];
/** The engine sends PNG QR codes only. */
const QR_DATA_URL = /^data:image\/png;base64,[A-Za-z0-9+/=]+$/;

function text(value: unknown): string | undefined {
	return typeof value === 'string' && value.length > 0 ? value : undefined;
}

/** The row's methods in preference order; a row without them falls back by bot name. */
export function botMethods(row: { bot?: string; methods?: unknown }): BotConnectMethod[] {
	const listed = Array.isArray(row.methods)
		? row.methods.filter((method): method is BotConnectMethod => METHODS.includes(method as BotConnectMethod))
		: [];
	if (listed.length) return [...new Set(listed)];
	const bot = row.bot ?? '';
	if (bot.startsWith('gmail-')) return ['login_url'];
	if (bot.startsWith('telegram')) return ['code', 'qr'];
	if (bot.startsWith('whatsapp')) return ['pairing_code', 'qr'];
	return [];
}

/** Code and pairing-code logins start from the account's phone number. */
export function methodNeedsPhone(method: BotConnectMethod): boolean {
	return method === 'code' || method === 'pairing_code';
}

export function methodLabel(bot: string, method: BotConnectMethod): string {
	const telegram = bot.startsWith('telegram');
	switch (method) {
		case 'login_url': return 'Sign in with Google';
		case 'code': return 'Code';
		case 'pairing_code': return 'Link with phone number';
		case 'qr': return telegram ? 'QR' : 'Scan QR';
	}
}

/** Normalizes a `BotConnectState` body; unknown pending kinds and unsafe QR URLs are dropped. */
export function parseBotState(bot: string, data: unknown): BotConnectState {
	const body = (data && typeof data === 'object' ? data : {}) as Record<string, unknown>;
	const status = STATUSES.includes(body.status as BotConnectStatus) ? (body.status as BotConnectStatus) : 'unknown';
	const raw = body.pending && typeof body.pending === 'object' ? (body.pending as Record<string, unknown>) : null;
	let pending: BotPending | null = null;
	if (raw && PENDING.includes(raw.kind as BotPendingKind)) {
		const qr = text(raw.qr_data_url);
		pending = {
			kind: raw.kind as BotPendingKind,
			label: text(raw.label),
			pairing_code: text(raw.pairing_code),
			qr_data_url: qr && QR_DATA_URL.test(qr) ? qr : undefined,
			sign_in: raw.sign_in && typeof raw.sign_in === 'object' ? (raw.sign_in as BotSignIn) : undefined
		};
	}
	return { bot: text(body.bot) ?? bot, status, detail: text(body.detail), pending };
}

/** `bot/project-access`: the Google account's roles on its OAuth client's Cloud project. */
export type ProjectAccessStatus = 'granted' | 'missing' | 'unknown';
export type ProjectAccessReport = {
	bot: string;
	project_id?: string;
	email?: string;
	status: ProjectAccessStatus;
	missing_roles: string[];
	detail?: string;
	grant_account?: string;
	commands: string[];
	console_url?: string;
	can_grant: boolean;
};

const ACCESS_STATUSES: readonly ProjectAccessStatus[] = ['granted', 'missing', 'unknown'];

function texts(value: unknown): string[] {
	return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item.length > 0) : [];
}

function httpsUrl(value: unknown): string | undefined {
	const raw = text(value);
	if (!raw) return undefined;
	try { return new URL(raw).protocol === 'https:' ? raw : undefined; } catch { return undefined; }
}

/** Normalizes a project-access report; an unknown status reads as `unknown` and only an https console link is kept. */
export function parseProjectAccess(bot: string, data: unknown): ProjectAccessReport {
	const body = (data && typeof data === 'object' ? data : {}) as Record<string, unknown>;
	const status = ACCESS_STATUSES.includes(body.status as ProjectAccessStatus) ? (body.status as ProjectAccessStatus) : 'unknown';
	return {
		bot: text(body.bot) ?? bot,
		project_id: text(body.project_id),
		email: text(body.email),
		status,
		missing_roles: texts(body.missing_roles),
		detail: text(body.detail),
		grant_account: text(body.grant_account),
		commands: texts(body.commands),
		console_url: httpsUrl(body.console_url),
		can_grant: body.can_grant === true && status === 'missing'
	};
}
