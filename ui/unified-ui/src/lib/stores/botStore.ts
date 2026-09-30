import { browser } from '$app/environment';
import { writable } from 'svelte/store';
import {
	appendCurrentScopeQuery,
	getCurrentScopeIdentity,
	scopeIdentityStore
} from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

export type BotRuntimeState = 'stopped' | 'running' | 'restarting' | 'stop_requested' | 'failed';
export type BotAuthStatus =
	| 'unsupported'
	| 'ok'
	| 'needs_auth'
	| 'account_mismatch'
	| 'error';
export type BotAuthFlowState = 'idle' | 'active' | 'queued';

export interface BotExitSnapshot {
	success: boolean;
	code?: number;
	finished_at: string;
}

export interface BotStatusSnapshot {
	name: string;
	enabled: boolean;
	auto_restart: boolean;
	qr_supported: boolean;
	desired_running: boolean;
	state: BotRuntimeState;
	pid?: number;
	started_at?: string;
	stopped_at?: string;
	uptime_secs?: number;
	restart_count: number;
	restart_backoff_secs?: number;
	last_exit?: BotExitSnapshot;
	last_error?: string;
	command: string;
	args: string[];
	cwd?: string;
}

export interface BotLogLine {
	timestamp: string;
	stream: string;
	line: string;
}

export interface BotProcessConfig {
	enabled: boolean;
	command: string;
	args: string[];
	env: Record<string, string>;
	cwd?: string;
	auto_restart: boolean;
	restart_max_backoff_secs: number;
}

export interface BotAuthSessionSnapshot {
	name: string;
	provider: string;
	profile_label?: string;
	expected_account?: string;
}

export interface BotAuthSnapshot {
	name: string;
	supported: boolean;
	provider?: string;
	status: BotAuthStatus;
	flow_state: BotAuthFlowState;
	profile_label?: string;
	expected_account?: string;
	current_account?: string;
	detail?: string;
}

interface ListBotsResponse {
	bots: BotStatusSnapshot[];
}

interface BotLogsResponse {
	name: string;
	lines: BotLogLine[];
}

interface BotConfigResponse {
	name: string;
	config: BotProcessConfig;
}

interface BotAuthResponse {
	name: string;
	auth: BotAuthSnapshot;
}

interface BotAuthStateResponse {
	auth_state: BotAuthStateSnapshot;
}

export interface BotAuthStateSnapshot {
	active?: BotAuthSessionSnapshot;
	queue: BotAuthSessionSnapshot[];
}

export interface BotAuthStartResponse {
	name: string;
	flow_state: BotAuthFlowState;
	auth_state: BotAuthStateSnapshot;
}

export interface BotStoreState {
	bots: BotStatusSnapshot[];
	logsByName: Record<string, BotLogLine[]>;
	isLoading: boolean;
	error: string | null;
	lastLoadedAt: number | null;
	mutatingByName: Record<string, string | undefined>;
	logsLoadingByName: Record<string, boolean | undefined>;
	authByName: Record<string, BotAuthSnapshot | undefined>;
	authLoadingByName: Record<string, boolean | undefined>;
	authMutatingByName: Record<string, boolean | undefined>;
	authState: BotAuthStateSnapshot;
	qrRefreshToken: number;
}

const defaultState: BotStoreState = {
	bots: [],
	logsByName: {},
	isLoading: false,
	error: null,
	lastLoadedAt: null,
	mutatingByName: {},
	logsLoadingByName: {},
	authByName: {},
	authLoadingByName: {},
	authMutatingByName: {},
	authState: { queue: [] },
	qrRefreshToken: Date.now()
};

export const botStore = writable<BotStoreState>(defaultState);

type BotScopeToken = {
	generation: number;
	scopeKey: string;
};

let botScopeGeneration = 0;

function currentBotScopeKey(): string {
	const scope = getCurrentScopeIdentity();
	return `${scope.principal}:${scope.workspace}`;
}

function nextBotScopeToken(): BotScopeToken {
	return {
		generation: botScopeGeneration,
		scopeKey: currentBotScopeKey()
	};
}

function isStaleBotScopeToken(token: BotScopeToken): boolean {
	return token.generation !== botScopeGeneration || currentBotScopeKey() !== token.scopeKey;
}

function resetBotStoreForScopeChange(): void {
	botStore.set({
		...defaultState,
		qrRefreshToken: Date.now()
	});
}

if (browser) {
	let lastBotScopeKey = currentBotScopeKey();
	scopeIdentityStore.subscribe((scope) => {
		const scopeKey = `${scope.principal}:${scope.workspace}`;
		if (scopeKey === lastBotScopeKey) {
			return;
		}
		lastBotScopeKey = scopeKey;
		botScopeGeneration += 1;
		resetBotStoreForScopeChange();
	});
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(payload: Record<string, unknown>, field: string): string | undefined {
	const value = payload[field];
	return typeof value === 'string' ? value : undefined;
}

function readBoolean(payload: Record<string, unknown>, field: string): boolean | undefined {
	const value = payload[field];
	return typeof value === 'boolean' ? value : undefined;
}

function readNumber(payload: Record<string, unknown>, field: string): number | undefined {
	const value = payload[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function readStringArray(payload: Record<string, unknown>, field: string): string[] {
	const value = payload[field];
	if (!Array.isArray(value)) return [];
	return value.filter((entry): entry is string => typeof entry === 'string');
}

function readStringRecord(payload: Record<string, unknown>, field: string): Record<string, string> {
	const value = payload[field];
	const record = asRecord(value);
	if (!record) return {};

	return Object.fromEntries(
		Object.entries(record).filter(
			(entry): entry is [string, string] => typeof entry[1] === 'string'
		)
	);
}

function parseBotExitSnapshot(raw: unknown): BotExitSnapshot | undefined {
	const payload = asRecord(raw);
	if (!payload) return undefined;

	const success = readBoolean(payload, 'success');
	const finishedAt = readString(payload, 'finished_at');
	if (success === undefined || !finishedAt) return undefined;

	return {
		success,
		finished_at: finishedAt,
		...(readNumber(payload, 'code') !== undefined ? { code: readNumber(payload, 'code') } : {})
	};
}

function parseBotStatusSnapshot(raw: unknown): BotStatusSnapshot | null {
	const payload = asRecord(raw);
	if (!payload) return null;

	const name = readString(payload, 'name');
	const command = readString(payload, 'command');
	const state = readString(payload, 'state') as BotRuntimeState | undefined;
	const enabled = readBoolean(payload, 'enabled');
	const autoRestart = readBoolean(payload, 'auto_restart');
	const qrSupported = readBoolean(payload, 'qr_supported');
	const desiredRunning = readBoolean(payload, 'desired_running');
	const restartCount = readNumber(payload, 'restart_count');
	if (
		!name ||
		!command ||
		!state ||
		enabled === undefined ||
		autoRestart === undefined ||
		qrSupported === undefined ||
		desiredRunning === undefined ||
		restartCount === undefined
	) {
		return null;
	}

	return {
		name,
		enabled,
		auto_restart: autoRestart,
		qr_supported: qrSupported,
		desired_running: desiredRunning,
		state,
		restart_count: restartCount,
		command,
		args: readStringArray(payload, 'args'),
		...(readNumber(payload, 'pid') !== undefined ? { pid: readNumber(payload, 'pid') } : {}),
		...(readString(payload, 'started_at') ? { started_at: readString(payload, 'started_at') } : {}),
		...(readString(payload, 'stopped_at') ? { stopped_at: readString(payload, 'stopped_at') } : {}),
		...(readNumber(payload, 'uptime_secs') !== undefined
			? { uptime_secs: readNumber(payload, 'uptime_secs') }
			: {}),
		...(readNumber(payload, 'restart_backoff_secs') !== undefined
			? { restart_backoff_secs: readNumber(payload, 'restart_backoff_secs') }
			: {}),
		...(parseBotExitSnapshot(payload.last_exit) ? { last_exit: parseBotExitSnapshot(payload.last_exit) } : {}),
		...(readString(payload, 'last_error') ? { last_error: readString(payload, 'last_error') } : {}),
		...(readString(payload, 'cwd') ? { cwd: readString(payload, 'cwd') } : {})
	};
}

function parseBotLogLine(raw: unknown): BotLogLine | null {
	const payload = asRecord(raw);
	if (!payload) return null;

	const timestamp = readString(payload, 'timestamp');
	const stream = readString(payload, 'stream');
	const line = readString(payload, 'line');
	if (!timestamp || !stream || line === undefined) {
		return null;
	}

	return { timestamp, stream, line };
}

function parseBotProcessConfig(raw: unknown): BotProcessConfig | null {
	const payload = asRecord(raw);
	if (!payload) return null;

	const enabled = readBoolean(payload, 'enabled');
	const command = readString(payload, 'command');
	const autoRestart = readBoolean(payload, 'auto_restart');
	const restartMaxBackoffSecs = readNumber(payload, 'restart_max_backoff_secs');
	if (
		enabled === undefined ||
		!command ||
		autoRestart === undefined ||
		restartMaxBackoffSecs === undefined
	) {
		return null;
	}

	return {
		enabled,
		command,
		args: readStringArray(payload, 'args'),
		env: readStringRecord(payload, 'env'),
		auto_restart: autoRestart,
		restart_max_backoff_secs: restartMaxBackoffSecs,
		...(readString(payload, 'cwd') ? { cwd: readString(payload, 'cwd') } : {})
	};
}

function parseBotAuthSessionSnapshot(raw: unknown): BotAuthSessionSnapshot | null {
	const payload = asRecord(raw);
	if (!payload) return null;

	const name = readString(payload, 'name');
	const provider = readString(payload, 'provider');
	if (!name || !provider) return null;

	return {
		name,
		provider,
		...(readString(payload, 'profile_label')
			? { profile_label: readString(payload, 'profile_label') }
			: {}),
		...(readString(payload, 'expected_account')
			? { expected_account: readString(payload, 'expected_account') }
			: {})
	};
}

function parseBotAuthSnapshot(raw: unknown): BotAuthSnapshot | null {
	const payload = asRecord(raw);
	if (!payload) return null;

	const name = readString(payload, 'name');
	const supported = readBoolean(payload, 'supported');
	const status = readString(payload, 'status') as BotAuthStatus | undefined;
	const flowState = readString(payload, 'flow_state') as BotAuthFlowState | undefined;
	if (!name || supported === undefined || !status || !flowState) {
		return null;
	}

	return {
		name,
		supported,
		status,
		flow_state: flowState,
		...(readString(payload, 'provider') ? { provider: readString(payload, 'provider') } : {}),
		...(readString(payload, 'profile_label')
			? { profile_label: readString(payload, 'profile_label') }
			: {}),
		...(readString(payload, 'expected_account')
			? { expected_account: readString(payload, 'expected_account') }
			: {}),
		...(readString(payload, 'current_account')
			? { current_account: readString(payload, 'current_account') }
			: {}),
		...(readString(payload, 'detail') ? { detail: readString(payload, 'detail') } : {})
	};
}

function parseBotAuthStateSnapshot(raw: unknown): BotAuthStateSnapshot {
	const payload = asRecord(raw);
	if (!payload) return { queue: [] };

	const active = parseBotAuthSessionSnapshot(payload.active);
	const queue = Array.isArray(payload.queue)
		? payload.queue
				.map((entry) => parseBotAuthSessionSnapshot(entry))
				.filter((entry): entry is BotAuthSessionSnapshot => entry !== null)
		: [];

	return {
		...(active ? { active } : {}),
		queue
	};
}

async function readApiError(response: Response): Promise<string> {
	try {
		const text = await response.text();
		if (!text) {
			return `Request failed (${response.status})`;
		}

		try {
			const parsed = JSON.parse(text) as unknown;
			const payload = asRecord(parsed);
			if (payload) {
				return readString(payload, 'error') || readString(payload, 'message') || text;
			}
		} catch {
			// Fall through to raw text.
		}

		return text;
	} catch {
		return `Request failed (${response.status})`;
	}
}

function replaceBotSnapshot(snapshot: BotStatusSnapshot): void {
	botStore.update((state) => {
		const existing = state.bots.filter((bot) => bot.name !== snapshot.name);
		return {
			...state,
			bots: [...existing, snapshot].sort((left, right) => left.name.localeCompare(right.name)),
			lastLoadedAt: Date.now(),
			qrRefreshToken: Date.now()
		};
	});
}

export async function loadBots(): Promise<BotStatusSnapshot[]> {
	const scopeToken = nextBotScopeToken();
	botStore.update((state) => ({
		...state,
		isLoading: true,
		error: null
	}));

	try {
		const response = await timedFetch('/api/magician/v2/bots');
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}

		const payload = (await response.json()) as ListBotsResponse;
		const bots = Array.isArray(payload.bots)
			? payload.bots
					.map((entry) => parseBotStatusSnapshot(entry))
					.filter((entry): entry is BotStatusSnapshot => entry !== null)
			: [];
		if (isStaleBotScopeToken(scopeToken)) {
			return bots;
		}

		botStore.update((state) => ({
			...state,
			bots: bots.sort((left, right) => left.name.localeCompare(right.name)),
			isLoading: false,
			error: null,
			lastLoadedAt: Date.now(),
			qrRefreshToken: Date.now()
		}));

		return bots;
	} catch (error) {
		if (isStaleBotScopeToken(scopeToken)) {
			return [];
		}
		const message = error instanceof Error ? error.message : 'Failed to load bots';
		botStore.update((state) => ({
			...state,
			isLoading: false,
			error: message
		}));
		throw error;
	}
}

async function mutateBot(name: string, action: 'start' | 'stop' | 'restart'): Promise<BotStatusSnapshot> {
	const scopeToken = nextBotScopeToken();
	botStore.update((state) => ({
		...state,
		error: null,
		mutatingByName: {
			...state.mutatingByName,
			[name]: action
		}
	}));

	try {
		const response = await timedFetch(`/api/magician/v2/bots/${encodeURIComponent(name)}/${action}`, {
			method: 'POST'
		});
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}

		const payload = (await response.json()) as unknown;
		const snapshot = parseBotStatusSnapshot(payload);
		if (!snapshot) {
			throw new Error(`Malformed bot ${action} response`);
		}
		if (isStaleBotScopeToken(scopeToken)) {
			return snapshot;
		}

		replaceBotSnapshot(snapshot);
		return snapshot;
	} finally {
		if (!isStaleBotScopeToken(scopeToken)) {
			botStore.update((state) => ({
				...state,
				mutatingByName: {
					...state.mutatingByName,
					[name]: undefined
				}
			}));
		}
	}
}

export function startBot(name: string): Promise<BotStatusSnapshot> {
	return mutateBot(name, 'start');
}

export function stopBot(name: string): Promise<BotStatusSnapshot> {
	return mutateBot(name, 'stop');
}

export function restartBot(name: string): Promise<BotStatusSnapshot> {
	return mutateBot(name, 'restart');
}

export async function loadBotAuth(name: string): Promise<BotAuthSnapshot> {
	const scopeToken = nextBotScopeToken();
	botStore.update((state) => ({
		...state,
		authLoadingByName: {
			...state.authLoadingByName,
			[name]: true
		}
	}));

	try {
		const response = await timedFetch(`/api/magician/v2/bots/${encodeURIComponent(name)}/auth`);
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}

		const payload = (await response.json()) as BotAuthResponse;
		const auth = parseBotAuthSnapshot(payload.auth);
		if (!auth) {
			throw new Error(`Malformed bot auth response for ${name}`);
		}
		if (isStaleBotScopeToken(scopeToken)) {
			return auth;
		}

		botStore.update((state) => ({
			...state,
			authByName: {
				...state.authByName,
				[name]: auth
			},
			authLoadingByName: {
				...state.authLoadingByName,
				[name]: false
			}
		}));

		return auth;
	} catch (error) {
		if (isStaleBotScopeToken(scopeToken)) {
			throw error;
		}
		botStore.update((state) => ({
			...state,
			authLoadingByName: {
				...state.authLoadingByName,
				[name]: false
			}
		}));
		throw error;
	}
}

export async function loadBotAuthState(): Promise<BotAuthStateSnapshot> {
	const scopeToken = nextBotScopeToken();
	const response = await timedFetch('/api/magician/v2/bots/auth/state');
	if (!response.ok) {
		throw new Error(await readApiError(response));
	}

	const payload = (await response.json()) as BotAuthStateResponse;
	const authState = parseBotAuthStateSnapshot(payload.auth_state);
	if (isStaleBotScopeToken(scopeToken)) {
		return authState;
	}
	botStore.update((state) => ({
		...state,
		authState
	}));
	return authState;
}

export async function startBotAuth(name: string): Promise<BotAuthStartResponse> {
	const scopeToken = nextBotScopeToken();
	botStore.update((state) => ({
		...state,
		error: null,
		authMutatingByName: {
			...state.authMutatingByName,
			[name]: true
		}
	}));

	try {
		const response = await timedFetch(`/api/magician/v2/bots/${encodeURIComponent(name)}/auth/start`, {
			method: 'POST'
		});
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}

		const payload = (await response.json()) as BotAuthStartResponse;
		const authState = parseBotAuthStateSnapshot(payload.auth_state);
		if (isStaleBotScopeToken(scopeToken)) {
			return {
				name: payload.name,
				flow_state: payload.flow_state,
				auth_state: authState
			};
		}
		botStore.update((state) => ({
			...state,
			authState
		}));
		return {
			name: payload.name,
			flow_state: payload.flow_state,
			auth_state: authState
		};
	} finally {
		if (!isStaleBotScopeToken(scopeToken)) {
			botStore.update((state) => ({
				...state,
				authMutatingByName: {
					...state.authMutatingByName,
					[name]: false
				}
			}));
		}
	}
}

export async function loadBotLogs(
	name: string,
	limit = 100,
	options: { captureError?: boolean; markLoading?: boolean } = {}
): Promise<BotLogLine[]> {
	const scopeToken = nextBotScopeToken();
	const { captureError = true, markLoading = true } = options;

	if (markLoading) {
		botStore.update((state) => ({
			...state,
			logsLoadingByName: {
				...state.logsLoadingByName,
				[name]: true
			}
		}));
	}

	try {
		const params = new URLSearchParams({ limit: String(limit) });
		const response = await timedFetch(
			`/api/magician/v2/bots/${encodeURIComponent(name)}/logs?${params.toString()}`
		);
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}

		const payload = (await response.json()) as BotLogsResponse;
		const lines = Array.isArray(payload.lines)
			? payload.lines.map((entry) => parseBotLogLine(entry)).filter((entry): entry is BotLogLine => entry !== null)
			: [];
		if (isStaleBotScopeToken(scopeToken)) {
			return lines;
		}

		botStore.update((state) => ({
			...state,
			logsByName: {
				...state.logsByName,
				[name]: lines
			},
			logsLoadingByName: markLoading
				? {
						...state.logsLoadingByName,
						[name]: false
					}
				: state.logsLoadingByName
		}));

		return lines;
	} catch (error) {
		if (isStaleBotScopeToken(scopeToken)) {
			throw error;
		}
		const message = error instanceof Error ? error.message : `Failed to load logs for ${name}`;
		botStore.update((state) => ({
			...state,
			error: captureError ? message : state.error,
			logsLoadingByName: markLoading
				? {
						...state.logsLoadingByName,
						[name]: false
					}
				: state.logsLoadingByName
		}));
		throw error;
	}
}

export function clearBotError(): void {
	botStore.update((state) => ({
		...state,
		error: null
	}));
}

export function botQrUrl(name: string, token: number): string {
	const params = appendCurrentScopeQuery(new URLSearchParams({ ts: String(token) }));
	return `/api/magician/v2/bots/${encodeURIComponent(name)}/qr?${params.toString()}`;
}

export async function loadBotConfig(name: string): Promise<BotProcessConfig> {
	const response = await timedFetch(`/api/magician/v2/bots/${encodeURIComponent(name)}/config`);
	if (!response.ok) {
		throw new Error(await readApiError(response));
	}

	const payload = (await response.json()) as BotConfigResponse;
	const config = parseBotProcessConfig(payload.config);
	if (!config) {
		throw new Error(`Malformed bot config response for ${name}`);
	}

	return config;
}

export async function saveBotConfig(
	name: string,
	config: BotProcessConfig
): Promise<BotProcessConfig> {
	const response = await timedFetch(`/api/magician/v2/bots/${encodeURIComponent(name)}/config`, {
		method: 'PUT',
		headers: {
			'content-type': 'application/json'
		},
		body: JSON.stringify(config)
	});
	if (!response.ok) {
		throw new Error(await readApiError(response));
	}

	const payload = (await response.json()) as BotConfigResponse;
	const parsed = parseBotProcessConfig(payload.config);
	if (!parsed) {
		throw new Error(`Malformed bot config save response for ${name}`);
	}

	return parsed;
}

export async function deleteBotConfig(name: string): Promise<void> {
	const response = await timedFetch(`/api/magician/v2/bots/${encodeURIComponent(name)}/config`, {
		method: 'DELETE'
	});
	if (!response.ok) {
		throw new Error(await readApiError(response));
	}
}
