import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { AppSurfaceClientError } from './appSurfaceRuntime';

// Kernel-emitted constants (magician-apps `surface_scripted_host` /
// `custom_surface_review`). The host page displays and enforces these; it
// never chooses them, and widening them is a kernel code change.
export const SCRIPTED_SURFACE_SANDBOX = 'allow-scripts';
export const SCRIPTED_SURFACE_FAILED_NOTICE = 'The custom surface failed safely and was closed.';
export const SCRIPTED_SURFACE_UNSUPPORTED_NOTICE =
	'Custom surfaces are not supported on this client.';
export const SCRIPTED_SURFACE_MAX_RELOADS = 3;
export const SCRIPTED_SURFACE_MAX_BRIDGE_MESSAGES = 32;

export type AppScriptedSurfaceMethod =
	| 'query_data'
	| 'mutate_data'
	| 'launch_action'
	| 'get_action_run'
	| 'compose_action_run'
	| 'cancel_action_run'
	| 'read_entity_changes'
	| 'contract_capabilities';

export const SCRIPTED_SURFACE_METHODS: readonly AppScriptedSurfaceMethod[] = [
	'query_data',
	'mutate_data',
	'launch_action',
	'get_action_run',
	'compose_action_run',
	'cancel_action_run',
	'read_entity_changes',
	'contract_capabilities'
];

export interface AppScriptedSurfaceHostPlan {
	sandbox: string;
	csp: string;
	session_ref: string;
	nonce: string;
	installation_id: string;
	package_revision_ref: string;
	surface_revision: number;
	grant_revision: number;
	entry_route: string;
	entry_document: string;
	entry_document_digest: string;
	entry_url: string;
	methods: AppScriptedSurfaceMethod[];
}

export interface AppScriptedSurfaceBridgeRequest {
	schema_version: 1;
	request_id: string;
	sequence: number;
	method: AppScriptedSurfaceMethod;
	origin: 'null';
	session_ref: string;
	nonce: string;
	installation_id: string;
	package_revision_ref: string;
	surface_revision: number;
	grant_revision: number;
	view_or_action?: string;
	payload: Record<string, unknown>;
}

/**
 * One per-frame FIFO for the bridge's HTTP leg. Frame messages are admitted in
 * strict sequence order, but independent fetches may arrive at the server out
 * of order (especially over HTTP/2). Chaining every submission here preserves
 * the admitted order. The tail deliberately absorbs each result so an ordinary
 * operation failure is returned only to that request and cannot poison later
 * submissions.
 */
export class ScriptedSurfaceBridgeSubmissionFifo {
	private tail: Promise<void> = Promise.resolve();

	enqueue<T>(submit: () => Promise<T>): Promise<T> {
		const result = this.tail.then(submit);
		this.tail = result.then(
			() => undefined,
			() => undefined
		);
		return result;
	}
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

function isAppReference(value: string): boolean {
	return value.length <= 192 && /^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/.test(value);
}

/**
 * Fail-closed parse of a scripted-surface host plan. The sandbox must be
 * exactly the kernel `allow-scripts` constant (never
 * `allow-same-origin`, never the general iframe set), the CSP must be a
 * deny-egress policy, and the entry URL must be an installation-scoped,
 * digest-keyed same-site path carrying exactly the minted session path
 * segment — the asset route's required credential, embedded by the kernel
 * because the frame's opaque origin can present no headers. The segment is
 * deliberately in the path: relative script/style URLs do not inherit a base
 * URL query. A plan whose entry URL lacks the segment, or carries any other
 * session than the plan binding, is refused.
 */
export function parseScriptedSurfaceHostPlan(
	value: unknown,
	installationId: string
): AppScriptedSurfaceHostPlan {
	if (!isRecord(value)) {
		throw new AppSurfaceClientError('The scripted surface response is invalid.', 500, 'invalid_custom_surface');
	}
	const sandbox = string(value.sandbox);
	const csp = string(value.csp);
	const sessionRef = string(value.session_ref);
	const nonce = string(value.nonce);
	const installation = string(value.installation_id);
	const packageRevisionRef = string(value.package_revision_ref);
	const entryRoute = string(value.entry_route);
	const entryDocument = string(value.entry_document);
	const entryDigest = string(value.entry_document_digest);
	const entryUrl = string(value.entry_url);
	const surfaceRevision = positiveInteger(value.surface_revision);
	const grantRevision = positiveInteger(value.grant_revision);
	const methods = Array.isArray(value.methods) ? value.methods.map((method) => string(method)) : null;
	const assetPrefix = `/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/custom-surface-v1/assets/`;
	const expectedEntryPath = `${assetPrefix}${sessionRef}/${entryDigest}/${entryDocument}`;
	if (
		sandbox !== SCRIPTED_SURFACE_SANDBOX
		|| sandbox.includes('allow-same-origin')
		|| !csp.includes("default-src 'none'")
		|| !csp.includes("connect-src 'none'")
		|| !csp.includes("script-src 'self'")
		|| !csp.includes('frame-ancestors')
		|| !sessionRef
		|| !isAppReference(sessionRef)
		|| !nonce
		|| installation !== installationId
		|| !packageRevisionRef
		|| surfaceRevision === 0
		|| grantRevision === 0
		|| !entryRoute.startsWith('/')
		|| !entryDocument.startsWith('surfaces/')
		|| !entryDocument.endsWith('.html')
		|| !entryDigest.startsWith('blake3:')
		|| entryUrl !== expectedEntryPath
		|| methods === null
		|| methods.length !== SCRIPTED_SURFACE_METHODS.length
		|| methods.some((method, index) => method !== SCRIPTED_SURFACE_METHODS[index])
	) {
		throw new AppSurfaceClientError('The scripted surface host was refused.', 500, 'invalid_custom_surface');
	}
	return {
		sandbox,
		csp,
		session_ref: sessionRef,
		nonce,
		installation_id: installation,
		package_revision_ref: packageRevisionRef,
		surface_revision: surfaceRevision,
		grant_revision: grantRevision,
		entry_route: entryRoute,
		entry_document: entryDocument,
		entry_document_digest: entryDigest,
		entry_url: entryUrl,
		methods: methods as AppScriptedSurfaceMethod[]
	};
}

async function responseJson(response: Response): Promise<unknown> {
	const body = await response.text();
	if (!body) return {};
	try {
		return JSON.parse(body) as unknown;
	} catch {
		throw new AppSurfaceClientError('The app service returned an invalid response.', response.status, 'invalid_response');
	}
}

async function errorFromResponse(
	response: Response,
	fallbackMessage: string,
	fallbackCode: string
): Promise<never> {
	const body = await responseJson(response).catch(() => null);
	const nested = isRecord(body) ? (isRecord(body.error) ? body.error : body) : {};
	throw new AppSurfaceClientError(
		string(nested.message) || fallbackMessage,
		response.status,
		string(nested.code) || string(nested.error) || fallbackCode
	);
}

export async function fetchScriptedSurfaceHost(
	installationId: string,
	options: { route?: string; signal?: AbortSignal } = {}
): Promise<AppScriptedSurfaceHostPlan> {
	const query = new URLSearchParams();
	if (options.route) query.set('route', options.route);
	const serialized = query.toString();
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/custom-surface-v1/host${serialized ? `?${serialized}` : ''}`,
		{
			headers: scopedRequestHeaders({ Accept: 'application/json' }),
			signal: options.signal
		}
	);
	if (!response.ok) {
		await errorFromResponse(response, 'The scripted surface could not be loaded.', 'app_custom_surface_failed');
	}
	return parseScriptedSurfaceHostPlan(await responseJson(response), installationId);
}

/**
 * Relay one frame bridge message through the authenticated host endpoint.
 * The frame itself holds no credentials; only this host page attaches
 * authentication. Returns the raw reply body for re-posting to the frame.
 */
export async function postScriptedSurfaceBridge(
	installationId: string,
	message: AppScriptedSurfaceBridgeRequest,
	signal?: AbortSignal
): Promise<unknown> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/custom-surface-v1/bridge`,
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
	if (!response.ok) {
		await errorFromResponse(response, 'The scripted-surface bridge refused the request.', 'app_custom_surface_denied');
	}
	return responseJson(response);
}

/**
 * The reload-note route for one minted session: the kernel counts every
 * frame reload and renderer crash against the reload/crash budget it
 * enforces server-side.
 */
export function scriptedSurfaceReloadNoteRoute(installationId: string, sessionRef: string): string {
	// The API accepts the kernel's canonical `bridge-scripted:...` reference
	// literally and rejects every percent-encoded path before scope resolution.
	// Preserve only its colon separators; path-changing bytes such as `/` stay
	// encoded and therefore fail closed at that early server guard.
	const sessionSegment = encodeURIComponent(sessionRef).replace(/%3A/g, ':');
	return `/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/custom-surface-v1/sessions/${sessionSegment}/reload-note`;
}

/**
 * Record one frame reload or renderer crash against the kernel's
 * reload/crash budget, under the host page's own authentication (the
 * frame holds no credentials; the same scoped headers the bridge helper
 * attaches ride this POST). A 409 answer — budget quarantined
 * (`app_custom_surface_denied`), or session gone
 * (`app_custom_surface_unavailable`); the server only ever answers 409
 * on this route — throws the same `AppSurfaceClientError` shape the
 * bridge refusal throws, so the host runs its existing closed path (any
 * 410 handling below is defensive client parity).
 */
export async function postScriptedSurfaceReloadNote(
	installationId: string,
	sessionRef: string,
	signal?: AbortSignal
): Promise<void> {
	const response = await fetch(scriptedSurfaceReloadNoteRoute(installationId, sessionRef), {
		method: 'POST',
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		signal
	});
	if (!response.ok) {
		await errorFromResponse(response, 'The scripted-surface reload was refused.', 'app_custom_surface_denied');
	}
}

/**
 * Per-frame bridge admission (design T3/T4): on web, two sandboxed
 * surfaces share `origin "null"`, so the exact frame object the host
 * created is the discriminator. Messages from any other source, any other
 * origin, any other session, any non-bridge channel, or any method
 * outside the closed set are dropped.
 */
export function bridgeEventSourceIsAdmitted(
	event: MessageEvent,
	expectedFrame: HTMLIFrameElement | null,
	plan: AppScriptedSurfaceHostPlan | null
): boolean {
	if (!expectedFrame || !plan) return false;
	if (event.source !== expectedFrame.contentWindow) return false;
	// A sandboxed frame without allow-same-origin has an opaque origin
	// serialized as the string "null". Anything else is not our frame.
	if (event.origin !== 'null') return false;
	if (!isRecord(event.data) || event.data.channel !== 'magician-surface-bridge') return false;
	return true;
}

/**
 * Validate one bridge request from the frame against the plan binding.
 * The frame cannot select its installation, session, nonce, revisions, or
 * method set; those come from the minted plan. Sequence must advance.
 */
export function frameBridgeRequestFromEvent(
	event: MessageEvent,
	plan: AppScriptedSurfaceHostPlan,
	lastSequence: number
): AppScriptedSurfaceBridgeRequest | null {
	if (!isRecord(event.data)) return null;
	const request = event.data.request;
	if (!isRecord(request)) return null;
	const method = string(request.method) as AppScriptedSurfaceMethod;
	const sequence = typeof request.sequence === 'number' && Number.isSafeInteger(request.sequence)
		? request.sequence
		: 0;
	const requestId = string(request.request_id);
	const payload = isRecord(request.payload) ? request.payload : null;
	if (
		!SCRIPTED_SURFACE_METHODS.includes(method)
		|| sequence !== lastSequence + 1
		|| !requestId
		|| !isAppReference(requestId)
		|| payload === null
	) {
		return null;
	}
	return {
		schema_version: 1,
		request_id: requestId,
		sequence,
		method,
		origin: 'null',
		session_ref: plan.session_ref,
		nonce: plan.nonce,
		installation_id: plan.installation_id,
		package_revision_ref: plan.package_revision_ref,
		surface_revision: plan.surface_revision,
		grant_revision: plan.grant_revision,
		...(
			typeof request.view_or_action === 'string' && request.view_or_action
				? { view_or_action: request.view_or_action }
				: {}
		),
		payload
	};
}

/**
 * Reload/crash budget mirroring the kernel: at most three navigations,
 * reloads, or frame errors per session; the fourth tears the surface down
 * and replaces it with the closed failure notice.
 */
export class ScriptedSurfaceReloadBudget {
	private reloads = 0;

	private tornNotice = '';

	get exceeded(): boolean {
		return this.tornNotice !== '';
	}

	get notice(): string {
		return this.tornNotice;
	}

	noteReload(): { exceeded: boolean; notice: string } {
		this.reloads += 1;
		if (this.reloads > SCRIPTED_SURFACE_MAX_RELOADS && !this.tornNotice) {
			this.tornNotice = SCRIPTED_SURFACE_FAILED_NOTICE;
		}
		return { exceeded: this.exceeded, notice: this.notice };
	}
}

/**
 * Desktop (Tauri) constraint: the surface iframe must never load from the
 * Tauri app's own origin or the dev-server origins that host the unified
 * UI bundle — those are host-page sources, never asset sources. The frame
 * loads only kernel-issued, digest-keyed API paths.
 */
const FORBIDDEN_SURFACE_URL_PREFIXES = [
	'tauri://',
	'https://tauri.localhost',
	'http://tauri.localhost',
	'http://localhost:5173/',
	'http://localhost:3002/'
];

export function surfaceFrameSourceIsAdmitted(url: string): boolean {
	// Absolute host-page sources are refused by name first — the Tauri
	// origin and the dev-server origins are host-page sources, never asset
	// sources — and then only relative, non-escaping, non-percent-encoded
	// paths are admitted.
	const lowered = url.toLowerCase();
	if (FORBIDDEN_SURFACE_URL_PREFIXES.some((prefix) => lowered.startsWith(prefix))) return false;
	if (!url.startsWith('/')) return false;
	// A protocol-relative source (`//evil.example/x`) resolves against the
	// host page's own scheme to a cross-origin absolute URL, so it must
	// never pass the relative-path requirement that a bare `/` prefix
	// alone would satisfy (mirrors the Rust invariant in
	// `desktop/src-tauri/src/app_surface_origin_policy.rs`).
	if (url.startsWith('//')) return false;
	if (url.includes('..') || url.includes('\\') || url.includes('%')) return false;
	return true;
}
