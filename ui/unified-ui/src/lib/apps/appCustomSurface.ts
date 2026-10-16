import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { AppSurfaceClientError } from './appSurfaceRuntime';

const MAX_CUSTOM_SURFACE_RESPONSE_BYTES = 8 * 1024 * 1024;
const CUSTOM_SURFACE_CSP = "default-src 'none'; connect-src 'none'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'; img-src 'none'; media-src 'none'; font-src 'none'; style-src 'unsafe-inline'; script-src 'none'";

export interface AppCustomSurfaceHostEnvelope {
	srcdoc: string;
	sandbox: string;
	csp: string;
	allowed_assets: string[];
	session_ref: string;
	nonce: string;
	installation_id: string;
	package_revision_ref: string;
	surface_revision: number;
	grant_revision: number;
	change_sequence: number;
	envelope_digest: string;
}

export type AppCustomSurfaceRunStatus =
	| 'queued' | 'planning' | 'running' | 'paused' | 'deferred' | 'waiting' | 'blocked'
	| 'cancelling' | 'completed' | 'failed' | 'cancelled' | 'archived' | 'uncertain';

export type AppCustomSurfaceRetryDisposition =
	| 'none' | 'poll_run' | 'retry_identical_input' | 'outcome_uncertain';

export interface AppCustomSurfaceRunError {
	code: string;
	disposition: string;
	message: string;
	details?: Record<string, unknown>;
	retry_after_ms?: number;
}

export interface AppCustomSurfaceCancellationReceipt {
	generation: number;
	idempotency_key: string;
	status: AppCustomSurfaceRunStatus;
	requested_at: string;
}

export interface AppCustomSurfaceRunReply {
	run_ref: string;
	status: AppCustomSurfaceRunStatus;
	terminal: boolean;
	result_withheld: boolean;
	cancellation_generation?: number;
	result?: unknown;
	error?: AppCustomSurfaceRunError;
	receipt?: AppCustomSurfaceCancellationReceipt;
	retry_disposition: AppCustomSurfaceRetryDisposition;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function string(value: unknown): string {
	return typeof value === 'string' ? value : '';
}

function positiveInteger(value: unknown): number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : 0;
}

function nonNegativeInteger(value: unknown): number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : -1;
}

function hasOnlyKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
	const allowed = new Set(keys);
	return Object.keys(value).every((key) => allowed.has(key));
}

function canonicalSurfaceAssetPath(path: string): boolean {
	if (path.length === 0 || path.length > 512 || path.normalize('NFKC') !== path) return false;
	if (!/^[\x20-\x7e]+$/.test(path) || path.includes('\\') || path.includes(':')) return false;
	const segments = path.split('/');
	return segments.length >= 2
		&& segments.length <= 32
		&& segments[0] === 'surfaces'
		&& segments.every((segment) => segment.length > 0
			&& segment.length <= 128
			&& segment !== '.'
			&& segment !== '..'
			&& segment.trim() === segment);
}

const RUN_STATUSES = new Set<AppCustomSurfaceRunStatus>([
	'queued', 'planning', 'running', 'paused', 'deferred', 'waiting', 'blocked',
	'cancelling', 'completed', 'failed', 'cancelled', 'archived', 'uncertain'
]);
const TERMINAL_RUN_STATUSES = new Set<AppCustomSurfaceRunStatus>([
	'completed', 'failed', 'cancelled', 'archived', 'uncertain'
]);
const RETRY_DISPOSITIONS = new Set<AppCustomSurfaceRetryDisposition>([
	'none', 'poll_run', 'retry_identical_input', 'outcome_uncertain'
]);
const ERROR_CODES = new Set([
	'invalid_request', 'not_authorized', 'not_found', 'conflict', 'stale_revision',
	'schema_mismatch', 'policy_denied', 'resource_exhausted', 'rate_limited',
	'unavailable', 'timeout', 'canceled', 'external_outcome_uncertain', 'internal'
]);
const ERROR_DISPOSITIONS = new Set([
	'terminal', 'retry_same_input', 'refresh_and_retry', 'reauthorize',
	'user_action_required', 'outcome_uncertain'
]);

function parseRunError(value: unknown): AppCustomSurfaceRunError | undefined {
	if (value === undefined) return undefined;
	if (!isRecord(value) || !hasOnlyKeys(value, ['code', 'disposition', 'message', 'details', 'retry_after_ms'])) {
		throw new AppSurfaceClientError('The custom-surface run error was refused.', 500, 'invalid_custom_surface_run');
	}
	const code = string(value.code);
	const disposition = string(value.disposition);
	const message = string(value.message);
	const details = value.details;
	const retryAfter = value.retry_after_ms;
	if (!ERROR_CODES.has(code) || !ERROR_DISPOSITIONS.has(disposition) || !message || message.length > 4096
		|| (details !== undefined && !isRecord(details))
		|| (retryAfter !== undefined && (!Number.isSafeInteger(retryAfter) || Number(retryAfter) <= 0))) {
		throw new AppSurfaceClientError('The custom-surface run error was refused.', 500, 'invalid_custom_surface_run');
	}
	return {
		code,
		disposition,
		message,
		...(details === undefined ? {} : { details }),
		...(retryAfter === undefined ? {} : { retry_after_ms: Number(retryAfter) })
	};
}

export function parseCustomSurfaceRunReply(
	value: unknown,
	expectedRunRef: string
): AppCustomSurfaceRunReply {
	if (!isRecord(value) || !hasOnlyKeys(value, [
		'run_ref', 'status', 'terminal', 'result_withheld', 'cancellation_generation',
		'result', 'error', 'receipt', 'retry_disposition'
	])) {
		throw new AppSurfaceClientError('The custom-surface run response was refused.', 500, 'invalid_custom_surface_run');
	}
	const runRef = string(value.run_ref);
	const status = string(value.status) as AppCustomSurfaceRunStatus;
	const retryDisposition = string(value.retry_disposition) as AppCustomSurfaceRetryDisposition;
	const terminal = value.terminal;
	const resultWithheld = value.result_withheld;
	const cancellationGeneration = value.cancellation_generation;
	const hasResult = Object.prototype.hasOwnProperty.call(value, 'result');
	const error = parseRunError(value.error);
	if (runRef !== expectedRunRef || !runRef.startsWith('run:app-action:')
		|| !RUN_STATUSES.has(status) || !RETRY_DISPOSITIONS.has(retryDisposition)
		|| typeof terminal !== 'boolean' || terminal !== TERMINAL_RUN_STATUSES.has(status)
		|| typeof resultWithheld !== 'boolean'
		|| (cancellationGeneration !== undefined && positiveInteger(cancellationGeneration) === 0)
		|| (resultWithheld && (hasResult || error !== undefined))
		|| (!terminal && (hasResult || error !== undefined))) {
		throw new AppSurfaceClientError('The custom-surface run response was refused.', 500, 'invalid_custom_surface_run');
	}
	let receipt: AppCustomSurfaceCancellationReceipt | undefined;
	if (value.receipt !== undefined) {
		if (!isRecord(value.receipt) || !hasOnlyKeys(value.receipt, [
			'generation', 'idempotency_key', 'status', 'requested_at'
		])) {
			throw new AppSurfaceClientError('The custom-surface cancellation receipt was refused.', 500, 'invalid_custom_surface_run');
		}
		const generation = positiveInteger(value.receipt.generation);
		const idempotencyKey = string(value.receipt.idempotency_key);
		const receiptStatus = string(value.receipt.status) as AppCustomSurfaceRunStatus;
		const requestedAt = string(value.receipt.requested_at);
		if (!generation || generation !== cancellationGeneration || !idempotencyKey
			|| receiptStatus !== status || !requestedAt || Number.isNaN(Date.parse(requestedAt))) {
			throw new AppSurfaceClientError('The custom-surface cancellation receipt was refused.', 500, 'invalid_custom_surface_run');
		}
		receipt = { generation, idempotency_key: idempotencyKey, status: receiptStatus, requested_at: requestedAt };
	}
	return {
		run_ref: runRef,
		status,
		terminal,
		result_withheld: resultWithheld,
		...(cancellationGeneration === undefined ? {} : { cancellation_generation: Number(cancellationGeneration) }),
		...(hasResult ? { result: value.result } : {}),
		...(error === undefined ? {} : { error }),
		...(receipt === undefined ? {} : { receipt }),
		retry_disposition: retryDisposition
	};
}

export function parseCustomSurfaceHostEnvelope(
	value: unknown,
	installationId: string
): AppCustomSurfaceHostEnvelope {
	if (!isRecord(value)) {
		throw new AppSurfaceClientError('The custom surface response is invalid.', 500, 'invalid_custom_surface');
	}
	if (!hasOnlyKeys(value, [
		'srcdoc', 'sandbox', 'csp', 'allowed_assets', 'session_ref', 'nonce',
		'installation_id', 'package_revision_ref', 'surface_revision', 'grant_revision',
		'change_sequence', 'envelope_digest', 'worker_pid', 'worker_entry', 'last_render'
	])) {
		throw new AppSurfaceClientError('The custom surface host was refused.', 500, 'invalid_custom_surface');
	}
	const srcdoc = string(value.srcdoc);
	const sandbox = string(value.sandbox);
	const csp = string(value.csp);
	const sessionRef = string(value.session_ref);
	const nonce = string(value.nonce);
	const installation = string(value.installation_id);
	const packageRevisionRef = string(value.package_revision_ref);
	const envelopeDigest = string(value.envelope_digest);
	const surfaceRevision = positiveInteger(value.surface_revision);
	const grantRevision = positiveInteger(value.grant_revision);
	const changeSequence = nonNegativeInteger(value.change_sequence);
	const allowedAssets = Array.isArray(value.allowed_assets)
		? value.allowed_assets.map((path) => string(path)).filter(Boolean)
		: null;
	if (
		!srcdoc
		|| sandbox !== ''
		|| csp !== CUSTOM_SURFACE_CSP
		|| !sessionRef
		|| !nonce
		|| installation !== installationId
		|| !packageRevisionRef
		|| surfaceRevision === 0
		|| grantRevision === 0
		|| changeSequence < 0
		|| !envelopeDigest
		|| allowedAssets === null
		|| allowedAssets.length > 256
		|| allowedAssets.some((path) => !canonicalSurfaceAssetPath(path))
	) {
		throw new AppSurfaceClientError('The custom surface host was refused.', 500, 'invalid_custom_surface');
	}
	return {
		srcdoc,
		sandbox,
		csp,
		allowed_assets: allowedAssets,
		session_ref: sessionRef,
		nonce,
		installation_id: installation,
		package_revision_ref: packageRevisionRef,
		surface_revision: surfaceRevision,
		grant_revision: grantRevision,
		change_sequence: changeSequence,
		envelope_digest: envelopeDigest
	};
}

async function responseJson(response: Response): Promise<unknown> {
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const declaredLength = Number(declared);
		if (!Number.isSafeInteger(declaredLength) || declaredLength < 0) {
			throw new AppSurfaceClientError('The app service returned an invalid response.', response.status, 'invalid_response');
		}
		if (declaredLength > MAX_CUSTOM_SURFACE_RESPONSE_BYTES) {
			throw new AppSurfaceClientError('The app service response exceeded its size limit.', response.status, 'app_response_too_large');
		}
	}
	if (!response.body) return {};
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_CUSTOM_SURFACE_RESPONSE_BYTES) {
			try {
				await reader.cancel();
			} catch {
				// Bounded rejection stays authoritative if cancel is refused.
			}
			throw new AppSurfaceClientError('The app service response exceeded its size limit.', response.status, 'app_response_too_large');
		}
		chunks.push(value);
	}
	const bytes = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	if (!bytes.byteLength) return {};
	try {
		return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)) as unknown;
	} catch {
		throw new AppSurfaceClientError('The app service returned an invalid response.', response.status, 'invalid_response');
	}
}

export async function fetchCustomSurfaceHost(
	installationId: string,
	options: { document?: string; replaceSession?: string; signal?: AbortSignal } = {}
): Promise<AppCustomSurfaceHostEnvelope> {
	const query = new URLSearchParams();
	if (options.document) query.set('document', options.document);
	if (options.replaceSession) query.set('replace_session', options.replaceSession);
	const serializedQuery = query.toString();
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/custom-surface${serializedQuery ? `?${serializedQuery}` : ''}`,
		{
			headers: scopedRequestHeaders({ Accept: 'application/json' }),
			signal: options.signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) {
		const nested = isRecord(body) ? (isRecord(body.error) ? body.error : body) : {};
		throw new AppSurfaceClientError(
			string(nested.message) || 'The custom surface could not be loaded.',
			response.status,
			string(nested.code) || 'app_custom_surface_failed'
		);
	}
	return parseCustomSurfaceHostEnvelope(body, installationId);
}

export async function postCustomSurfaceBridge(
	installationId: string,
	message: Record<string, unknown>,
	signal?: AbortSignal
): Promise<unknown> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/custom-surface/bridge`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'Content-Type': 'application/json'
			}),
			body: JSON.stringify(message),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) {
		const nested = isRecord(body) ? (isRecord(body.error) ? body.error : body) : {};
		throw new AppSurfaceClientError(
			string(nested.message) || 'The custom-surface bridge refused the request.',
			response.status,
			string(nested.code) || 'app_custom_surface_denied'
		);
	}
	return body;
}

/**
 * Closed run-control bridge adapter. Aborting this request only aborts the
 * transport; canonical cancellation requires a separate `cancel_run` message
 * carrying the retained generation and idempotency identity.
 */
export async function postCustomSurfaceRunBridge(
	installationId: string,
	message: Record<string, unknown>,
	expectedRunRef: string,
	signal?: AbortSignal
): Promise<AppCustomSurfaceRunReply> {
	const body = await postCustomSurfaceBridge(installationId, message, signal);
	return parseCustomSurfaceRunReply(body, expectedRunRef);
}
