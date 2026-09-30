import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

const MAX_RESPONSE_BYTES = 128 * 1024;
const MAX_AUDIT_RECEIPTS = 8;
const PROFILES = new Set(['browser_session', 'macos_host', 'android_device']);
const ACTIVITIES = new Set(['observe', 'navigate_or_launch', 'interact', 'capture_pixels', 'transfer_artifact', 'outward_commit']);
const PHASES = new Set(['active', 'stop_requested', 'completed', 'cancelled_before_io', 'outcome_uncertain']);
const TERMINALS = new Set(['completed', 'cancelled_before_io', 'outcome_uncertain']);
const AVAILABILITY_STATES = new Set(['conditional', 'active', 'stopping', 'settled', 'unavailable']);
const AVAILABILITY_REASONS = new Set(['not_declared', 'runtime_owner_unavailable', 'session_expired', 'run_terminal']);
const TARGET_KINDS = new Set(['isolated_browser', 'reviewed_macos_application', 'reviewed_android_application']);
const STOP_PHASES = new Set(['unavailable', 'available', 'requested']);
const STOP_REASONS = new Set(['no_current_session', 'session_terminal', 'runtime_owner_unavailable', 'owner_closing', 'run_terminal']);

export type AppInteractiveProfile = 'browser_session' | 'macos_host' | 'android_device';

export interface AppInteractiveOwnerAvailability {
	profile: AppInteractiveProfile;
	state: 'conditional' | 'active' | 'stopping' | 'settled' | 'unavailable';
	unavailableReason?: 'not_declared' | 'runtime_owner_unavailable' | 'session_expired' | 'run_terminal';
}

export interface AppInteractiveTargetSummary {
	targetRef: string;
	kind: 'isolated_browser' | 'reviewed_macos_application' | 'reviewed_android_application';
}

export interface AppInteractiveResourceClaim {
	evidenceBytes: number;
	evidenceNodes: number;
	pixels: number;
	artifactBytes: number;
	outputBytes: number;
}

export interface AppInteractiveSessionState {
	sessionRef: string;
	profile: AppInteractiveProfile;
	targetRef: string;
	targetSummary: AppInteractiveTargetSummary;
	activity: string;
	phase: 'active' | 'stop_requested' | 'completed' | 'cancelled_before_io' | 'outcome_uncertain';
	startedAt: string;
	expiresAt: string;
	resourceClaim: AppInteractiveResourceClaim;
}

export interface AppInteractiveDeclaredStopState {
	phase: 'unavailable' | 'available' | 'requested';
	sessionRef?: string;
	stopRef?: string;
	requestedAt?: string;
	unavailableReason?: 'no_current_session' | 'session_terminal' | 'runtime_owner_unavailable' | 'owner_closing' | 'run_terminal';
}

export interface AppInteractiveAuditReceipt {
	receiptRef: string;
	sessionRef: string;
	activity: string;
	sequence: number;
	terminal: 'completed' | 'cancelled_before_io' | 'outcome_uncertain';
	resultBytes: number;
	evidenceBytes: number;
}

export interface AppInteractiveRunStateSnapshot {
	runRef: string;
	installationId: string;
	installationGeneration: number;
	ownerAvailability: AppInteractiveOwnerAvailability[];
	currentSession?: AppInteractiveSessionState;
	recentAuditReceipts: AppInteractiveAuditReceipt[];
	declaredStopState: AppInteractiveDeclaredStopState;
	stopAvailable: boolean;
	capturedContentIncluded: false;
}

export interface AppInteractiveStopReceipt {
	runRef: string;
	sessionRef: string;
	stopRef: string;
	requestedAt: string;
	phase: 'stop_requested';
	receiptDigest: string;
}

function object(value: unknown, label: string): Record<string, unknown> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${label} is not an object.`);
	return value as Record<string, unknown>;
}

function exact(value: Record<string, unknown>, keys: readonly string[], label: string): void {
	if (Object.keys(value).some((key) => !keys.includes(key))) throw new Error(`${label} contains an unknown field.`);
}

function text(value: unknown, label: string, maximum = 512): string {
	if (typeof value !== 'string' || value.length === 0 || new TextEncoder().encode(value).length > maximum) throw new Error(`${label} is invalid.`);
	return value;
}

function natural(value: unknown, label: string): number {
	if (!Number.isSafeInteger(value) || Number(value) < 0) throw new Error(`${label} is invalid.`);
	return Number(value);
}

function reference(value: unknown, namespace: string, label: string): string {
	const parsed = text(value, label);
	if (!parsed.startsWith(`${namespace}:`) || parsed.length > 256) throw new Error(`${label} is invalid.`);
	return parsed;
}

function digestReference(value: unknown, namespace: string, label: string): string {
	const parsed = reference(value, namespace, label);
	const suffix = parsed.slice(namespace.length + 1);
	if (!/^[0-9a-f]{64}$/.test(suffix)) throw new Error(`${label} is invalid.`);
	return parsed;
}

function digest(value: unknown, label: string): string {
	const parsed = text(value, label, 71);
	if (!/^blake3:[0-9a-f]{64}$/.test(parsed)) throw new Error(`${label} is invalid.`);
	return parsed;
}

function timestamp(value: unknown, label: string): string {
	const parsed = text(value, label, 64);
	if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/.test(parsed) || !Number.isFinite(Date.parse(parsed))) throw new Error(`${label} is invalid.`);
	return parsed;
}

function member(value: unknown, values: Set<string>, label: string): string {
	const parsed = text(value, label, 64);
	if (!values.has(parsed)) throw new Error(`${label} is unknown.`);
	return parsed;
}

function parseClaim(value: unknown): AppInteractiveResourceClaim {
	const row = object(value, 'Interactive resource claim');
	exact(row, ['evidence_bytes', 'evidence_nodes', 'pixels', 'artifact_bytes', 'output_bytes'], 'Interactive resource claim');
	return {
		evidenceBytes: natural(row.evidence_bytes, 'Evidence bytes'),
		evidenceNodes: natural(row.evidence_nodes, 'Evidence nodes'),
		pixels: natural(row.pixels, 'Pixels'),
		artifactBytes: natural(row.artifact_bytes, 'Artifact bytes'),
		outputBytes: natural(row.output_bytes, 'Output bytes')
	};
}

function parseAvailability(value: unknown): AppInteractiveOwnerAvailability {
	const row = object(value, 'Interactive owner availability');
	exact(row, ['profile', 'state', 'unavailable_reason'], 'Interactive owner availability');
	const state = member(row.state, AVAILABILITY_STATES, 'Interactive availability') as AppInteractiveOwnerAvailability['state'];
	const unavailableReason = row.unavailable_reason === undefined
		? undefined
		: member(row.unavailable_reason, AVAILABILITY_REASONS, 'Interactive unavailable reason') as NonNullable<AppInteractiveOwnerAvailability['unavailableReason']>;
	if ((state === 'unavailable') !== (unavailableReason !== undefined)) {
		throw new Error('Interactive owner availability is inconsistent.');
	}
	return {
		profile: member(row.profile, PROFILES, 'Interactive profile') as AppInteractiveProfile,
		state,
		...(unavailableReason ? { unavailableReason } : {})
	};
}

function parseTargetSummary(value: unknown): AppInteractiveTargetSummary {
	const row = object(value, 'Interactive target summary');
	exact(row, ['target_ref', 'kind'], 'Interactive target summary');
	return {
		targetRef: digestReference(row.target_ref, 'interactive-target', 'Target reference'),
		kind: member(row.kind, TARGET_KINDS, 'Interactive target kind') as AppInteractiveTargetSummary['kind']
	};
}

function parseSession(value: unknown): AppInteractiveSessionState {
	const row = object(value, 'Interactive session');
	exact(row, ['session_ref', 'profile', 'target_ref', 'target_summary', 'activity', 'phase', 'started_at', 'expires_at', 'resource_claim'], 'Interactive session');
	const startedAt = timestamp(row.started_at, 'Session start');
	const expiresAt = timestamp(row.expires_at, 'Session expiry');
	if (Date.parse(expiresAt) <= Date.parse(startedAt)) throw new Error('Interactive session expiry is invalid.');
	const profile = member(row.profile, PROFILES, 'Interactive profile') as AppInteractiveProfile;
	const targetRef = digestReference(row.target_ref, 'interactive-target', 'Target reference');
	const targetSummary = parseTargetSummary(row.target_summary);
	const expectedTargetKind: AppInteractiveTargetSummary['kind'] = profile === 'browser_session'
		? 'isolated_browser'
		: profile === 'macos_host' ? 'reviewed_macos_application' : 'reviewed_android_application';
	if (targetSummary.targetRef !== targetRef || targetSummary.kind !== expectedTargetKind) {
		throw new Error('Interactive target summary correlation changed.');
	}
	return {
		sessionRef: digestReference(row.session_ref, 'interactive-session', 'Session reference'),
		profile,
		targetRef,
		targetSummary,
		activity: member(row.activity, ACTIVITIES, 'Interactive activity'),
		phase: member(row.phase, PHASES, 'Interactive phase') as AppInteractiveSessionState['phase'],
		startedAt, expiresAt, resourceClaim: parseClaim(row.resource_claim)
	};
}

function parseDeclaredStopState(value: unknown): AppInteractiveDeclaredStopState {
	const row = object(value, 'Interactive declared stop state');
	exact(row, ['phase', 'session_ref', 'stop_ref', 'requested_at', 'unavailable_reason'], 'Interactive declared stop state');
	const phase = member(row.phase, STOP_PHASES, 'Interactive stop phase') as AppInteractiveDeclaredStopState['phase'];
	const sessionRef = row.session_ref === undefined ? undefined : digestReference(row.session_ref, 'interactive-session', 'Stop session reference');
	const stopRef = row.stop_ref === undefined ? undefined : digestReference(row.stop_ref, 'interactive-stop', 'Stop reference');
	const requestedAt = row.requested_at === undefined ? undefined : timestamp(row.requested_at, 'Stop timestamp');
	const unavailableReason = row.unavailable_reason === undefined
		? undefined
		: member(row.unavailable_reason, STOP_REASONS, 'Stop unavailable reason') as NonNullable<AppInteractiveDeclaredStopState['unavailableReason']>;
	if (phase === 'requested') {
		if (!sessionRef || !stopRef || !requestedAt || unavailableReason) throw new Error('Interactive declared stop state is inconsistent.');
	} else if (sessionRef || stopRef || requestedAt || (phase === 'unavailable') !== (unavailableReason !== undefined)) {
		throw new Error('Interactive declared stop state is inconsistent.');
	}
	return {
		phase,
		...(sessionRef ? { sessionRef } : {}),
		...(stopRef ? { stopRef } : {}),
		...(requestedAt ? { requestedAt } : {}),
		...(unavailableReason ? { unavailableReason } : {})
	};
}

function parseAudit(value: unknown): AppInteractiveAuditReceipt {
	const row = object(value, 'Interactive audit receipt');
	exact(row, ['receipt_ref', 'session_ref', 'activity', 'sequence', 'terminal', 'result_bytes', 'evidence_bytes'], 'Interactive audit receipt');
	const sequence = natural(row.sequence, 'Interactive sequence');
	if (sequence === 0) throw new Error('Interactive sequence is invalid.');
	return {
		receiptRef: digestReference(row.receipt_ref, 'interactive-audit', 'Audit receipt reference'),
		sessionRef: digestReference(row.session_ref, 'interactive-session', 'Audit session reference'),
		activity: member(row.activity, ACTIVITIES, 'Audit activity'), sequence,
		terminal: member(row.terminal, TERMINALS, 'Audit terminal') as AppInteractiveAuditReceipt['terminal'],
		resultBytes: natural(row.result_bytes, 'Audit result bytes'),
		evidenceBytes: natural(row.evidence_bytes, 'Audit evidence bytes')
	};
}

export function parseAppInteractiveRunState(value: unknown, expectedRunRef: string): AppInteractiveRunStateSnapshot {
	const root = object(value, 'Interactive run state');
	exact(root, ['run_ref', 'installation_id', 'installation_generation', 'owner_availability', 'current_session', 'recent_audit_receipts', 'declared_stop_state', 'stop_available', 'captured_content_included'], 'Interactive run state');
	const runRef = reference(root.run_ref, 'run:app-action', 'Run reference');
	if (runRef !== expectedRunRef) throw new Error('Interactive run correlation changed.');
	if (!Array.isArray(root.owner_availability) || root.owner_availability.length !== PROFILES.size) throw new Error('Interactive owner availability is invalid.');
	if (!Array.isArray(root.recent_audit_receipts) || root.recent_audit_receipts.length > MAX_AUDIT_RECEIPTS) throw new Error('Interactive audit receipt page is invalid.');
	if (typeof root.stop_available !== 'boolean' || root.captured_content_included !== false) throw new Error('Interactive disclosure flags are invalid.');
	const ownerAvailability = root.owner_availability.map(parseAvailability);
	if (new Set(ownerAvailability.map((availability) => availability.profile)).size !== PROFILES.size ||
		[...PROFILES].some((profile) => !ownerAvailability.some((availability) => availability.profile === profile))) {
		throw new Error('Interactive owner availability is invalid.');
	}
	const currentSession = root.current_session === undefined ? undefined : parseSession(root.current_session);
	const declaredStopState = parseDeclaredStopState(root.declared_stop_state);
	if (root.stop_available !== (declaredStopState.phase === 'available')) throw new Error('Interactive stop availability is inconsistent.');
	if ((declaredStopState.phase === 'available' && currentSession?.phase !== 'active') ||
		(declaredStopState.phase === 'requested' && currentSession === undefined) ||
		(declaredStopState.phase === 'requested' && declaredStopState.sessionRef !== currentSession?.sessionRef) ||
		(currentSession?.phase === 'stop_requested' && declaredStopState.phase !== 'requested')) {
		throw new Error('Interactive declared stop state is inconsistent.');
	}
	if (currentSession) {
		const currentAvailability = ownerAvailability.find((availability) => availability.profile === currentSession.profile);
		if (!currentAvailability || currentAvailability.state === 'conditional' || currentAvailability.unavailableReason === 'not_declared') {
			throw new Error('Interactive current owner availability is inconsistent.');
		}
	}
	const recentAuditReceipts = root.recent_audit_receipts.map(parseAudit);
	if (currentSession) {
		const currentReceipt = recentAuditReceipts.find((receipt) => receipt.sessionRef === currentSession.sessionRef);
		const receiptPhase = currentReceipt?.terminal === 'completed'
			? 'completed'
			: currentReceipt?.terminal === 'cancelled_before_io'
				? 'cancelled_before_io'
				: currentReceipt?.terminal === 'outcome_uncertain' ? 'outcome_uncertain' : undefined;
		if (receiptPhase !== undefined && currentSession.phase !== receiptPhase) {
			throw new Error('The current session contradicts its terminal receipt.');
		}
	}
	return {
		runRef,
		installationId: text(root.installation_id, 'Installation id'),
		installationGeneration: natural(root.installation_generation, 'Installation generation'),
		ownerAvailability,
		...(currentSession ? { currentSession } : {}),
		recentAuditReceipts,
		declaredStopState,
		stopAvailable: root.stop_available,
		capturedContentIncluded: false
	};
}

async function boundedJson(response: Response): Promise<unknown> {
	const declared = Number(response.headers.get('content-length') ?? '0');
	if (Number.isFinite(declared) && declared > MAX_RESPONSE_BYTES) throw new Error('Interactive state response is oversized.');
	const reader = response.body?.getReader();
	if (!reader) throw new Error('Interactive state response has no body.');
	const chunks: Uint8Array[] = [];
	let length = 0;
	while (true) {
		const { value, done } = await reader.read();
		if (done) break;
		if (!value) continue;
		length += value.byteLength;
		if (length > MAX_RESPONSE_BYTES) { await reader.cancel(); throw new Error('Interactive state response is oversized.'); }
		chunks.push(value);
	}
	const bytes = new Uint8Array(length);
	let offset = 0;
	for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
	return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
}

function serverError(value: unknown, fallback: string): Error {
	const row = object(value, 'Server error');
	return new Error(typeof row.message === 'string' ? row.message : fallback);
}

export async function fetchAppInteractiveRunState(runRef: string, signal?: AbortSignal): Promise<AppInteractiveRunStateSnapshot> {
	const response = await fetch(`/api/magician/v2/apps/action-runs/${encodeURIComponent(runRef)}/interactive-state`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedJson(response);
	if (!response.ok) throw serverError(body, 'Interactive state is unavailable.');
	return parseAppInteractiveRunState(body, runRef);
}

export async function requestAppInteractiveStop(runRef: string, sessionRef: string, idempotencyKey: string, signal?: AbortSignal): Promise<AppInteractiveStopReceipt> {
	const response = await fetch(`/api/magician/v2/apps/action-runs/${encodeURIComponent(runRef)}/interactive-stop`, {
		method: 'POST', headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		body: JSON.stringify({ expected_session_ref: sessionRef, idempotency_key: idempotencyKey }), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedJson(response);
	if (!response.ok) throw serverError(body, 'Interactive stop could not be requested.');
	const row = object(body, 'Interactive stop receipt');
	exact(row, ['schema', 'run_ref', 'session_ref', 'stop_ref', 'idempotency_key', 'requested_at', 'phase', 'receipt_digest'], 'Interactive stop receipt');
	if (text(row.schema, 'Stop schema') !== 'magician.app-interactive-stop.v1' || text(row.phase, 'Stop phase') !== 'stop_requested') throw new Error('Interactive stop receipt is invalid.');
	const receipt: AppInteractiveStopReceipt = {
		runRef: reference(row.run_ref, 'run:app-action', 'Stop run reference'),
		sessionRef: digestReference(row.session_ref, 'interactive-session', 'Stop session reference'),
		stopRef: digestReference(row.stop_ref, 'interactive-stop', 'Stop reference'),
		requestedAt: timestamp(row.requested_at, 'Stop timestamp'), phase: 'stop_requested',
		receiptDigest: digest(row.receipt_digest, 'Stop receipt digest')
	};
	if (receipt.runRef !== runRef || receipt.sessionRef !== sessionRef || text(row.idempotency_key, 'Stop idempotency key') !== idempotencyKey) throw new Error('Interactive stop receipt correlation changed.');
	return receipt;
}
