import { browser } from '$app/environment';
import { get } from 'svelte/store';
import { getV2EventSequence, v2Events, type V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import type { MuijDocument } from '$lib/stores/muijStore';
import {
	getCurrentScopeBearerToken,
	getCurrentScopeIdentity,
	scopeIdentityStore
} from '$lib/stores/scopeIdentityStore';
import {
	PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
	type PublishedSurfaceRecord,
	type PublishedSurfaceRefreshRealtimeEvent,
	type PublishedSurfaceRenderRecord,
	type SurfaceArtifactMetadata,
	type SurfaceManifestQuery,
	type SurfacePhysicalLocator
} from '$lib/types/surfaces';

import { timedFetch } from '$lib/shared/fetch';
const V3_API_BASE = '/api/magician/v3';
const SURFACE_MANIFEST_NAMESPACE = 'surfaces';

type PublishedSurfaceQuery = Omit<SurfaceManifestQuery, 'domain' | 'artifact_type'>;

export interface PublishedSurfaceScopeSnapshot {
	principal: string;
	workspace: string;
}

interface PublishedSurfaceAuthSnapshot {
	scope: PublishedSurfaceScopeSnapshot;
	bearer: string;
}

interface PublishedSurfaceProjectionPayload {
	surface: {
		surface_id: string;
		principal: string;
		workspace: string;
		surface_kind: string;
		status: string;
		logical_surface_id?: string;
		route: string;
		document_key: string;
		task_id?: string;
		ui_thread_id?: string;
		source_output_id?: string;
		source_execution_id?: string;
		media_type?: string;
		title: string;
		summary?: string;
		placement: {
			placement_kind: string;
			placement_id?: string;
			pinned?: boolean;
		};
		manifest_artifact_uid?: string;
		manifest_name?: string;
		input_artifact_ids: string[];
		published_at: string;
		unpublished_at?: string;
		updated_at: string;
	};
	task_title?: string;
	task_status?: string;
	source_agent_id?: string;
	source_output_media_type?: string;
	source_output_summary?: string;
	render_origin: string;
	render_kind: string;
	presentation_state: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(payload: Record<string, unknown>, field: string): string | undefined {
	const value = payload[field];
	return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
}

function readStringArray(payload: Record<string, unknown>, field: string): string[] {
	const value = payload[field];
	return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}

function parseMuijDocument(raw: unknown): MuijDocument | null {
	const payload = asRecord(raw);
	if (!payload) return null;
	const muij_version = readString(payload, 'muij_version');
	const agent_id = readString(payload, 'agent_id');
	const generated_at = readString(payload, 'generated_at');
	const layout = Array.isArray(payload.layout) ? payload.layout : null;
	if (!muij_version || !agent_id || !generated_at || !layout) {
		return null;
	}
	return payload as unknown as MuijDocument;
}

async function readApiError(response: Response): Promise<string> {
	let message = `Request failed (${response.status})`;
	try {
		const text = await response.text();
		if (!text) return message;
		try {
			const parsed = JSON.parse(text) as unknown;
			const root = asRecord(parsed);
			const rootMessage = root ? readString(root, 'message') : undefined;
			if (rootMessage) {
				return rootMessage;
			}
			const nestedError = root ? root.error : undefined;
			const errorRecord = asRecord(nestedError);
			const nestedMessage = errorRecord ? readString(errorRecord, 'message') : undefined;
			if (nestedMessage) {
				return nestedMessage;
			}
			if (typeof nestedError === 'string' && nestedError.trim().length > 0) {
				return `Request failed (${response.status}): ${nestedError}`;
			}
			return `Request failed (${response.status}): ${text}`;
		} catch {
			return `Request failed (${response.status}): ${text}`;
		}
	} catch {
		return message;
	}
}

async function expectOk(response: Response): Promise<void> {
	if (!response.ok) {
		throw new Error(await readApiError(response));
	}
}

function appendQueryParam(params: URLSearchParams, key: string, value: string | number | undefined): void {
	if (value === undefined) return;
	const normalized = String(value).trim();
	if (!normalized) return;
	params.set(key, normalized);
}

function compareNewestFirst(
	left: { producer: { produced_at: string }; artifact_uid: string },
	right: { producer: { produced_at: string }; artifact_uid: string }
): number {
	const leftTime = Date.parse(left.producer.produced_at);
	const rightTime = Date.parse(right.producer.produced_at);
	const leftStamp = Number.isFinite(leftTime) ? leftTime : 0;
	const rightStamp = Number.isFinite(rightTime) ? rightTime : 0;
	if (leftStamp !== rightStamp) {
		return rightStamp - leftStamp;
	}
	return right.artifact_uid.localeCompare(left.artifact_uid);
}

function parsePublishedSurfaceRefreshRealtimeEventRecord(
	record: Record<string, unknown>,
	producer_agent_id: string
): PublishedSurfaceRefreshRealtimeEvent | null {
	const surface_id = readString(record, 'surface_id');
	const principal = readString(record, 'principal');
	const workspace = readString(record, 'workspace');
	const route = readString(record, 'route');
	const document_key = readString(record, 'document_key');
	const status = readString(record, 'status');
	if (!surface_id || !principal || !workspace || !route || !document_key || !status) {
		return null;
	}

	return {
		event_type: PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
		producer_agent_id,
		surface_id,
		principal,
		workspace,
		route,
		document_key,
		status,
		surface_kind: readString(record, 'surface_kind'),
		logical_surface_id: readString(record, 'logical_surface_id'),
		execution_id: readString(record, 'execution_id'),
		task_id: readString(record, 'task_id'),
		ui_thread_id: readString(record, 'ui_thread_id'),
		source_output_id: readString(record, 'source_output_id'),
		published_at: readString(record, 'published_at'),
		updated_at: readString(record, 'updated_at')
	};
}

export function parsePublishedSurfaceRefreshRealtimeEvent(
	event: V2WebSocketEvent
): PublishedSurfaceRefreshRealtimeEvent | null {
	if (event.event_type !== 'AgentEvent') {
		return null;
	}

	const container = asRecord(event.data);
	const envelope = container ? asRecord(container.event) : null;
	if (!envelope) {
		return null;
	}

	const event_type = readString(envelope, 'event_type');
	const producer_agent_id = readString(envelope, 'agent_id');
	if (event_type !== PUBLISHED_SURFACE_CHANGED_EVENT_TYPE || !producer_agent_id) {
		return null;
	}

	const payload = asRecord(envelope.payload);
	return payload ? parsePublishedSurfaceRefreshRealtimeEventRecord(payload, producer_agent_id) : null;
}

export function matchesPublishedSurfaceRefreshRealtimeEvent(
	event: PublishedSurfaceRefreshRealtimeEvent,
	query: PublishedSurfaceQuery
): boolean {
	const activeScope = get(scopeIdentityStore);
	if (event.principal !== activeScope.principal || event.workspace !== activeScope.workspace) {
		return false;
	}
	if (query.route_target && event.route !== query.route_target) {
		return false;
	}
	if (query.execution_id && event.execution_id !== query.execution_id) {
		return false;
	}
	if (query.task_id && event.task_id !== query.task_id) {
		return false;
	}
	if (query.agent_id && event.producer_agent_id !== query.agent_id) {
		return false;
	}
	if (query.workflow_instance_id || query.run_id || query.cycle_id) {
		return false;
	}
	return true;
}

export function subscribeToPublishedSurfaceRefresh(
	query: PublishedSurfaceQuery,
	onPublished: (event: PublishedSurfaceRefreshRealtimeEvent) => void,
	options: { connect?: boolean } = {}
): () => void {
	let initialized = false;
	let lastProcessedSequence = 0;

	if (browser && options.connect !== false) {
		v2Events.connectGlobal();
	}

	return v2Events.subscribe((events) => {
		const currentHighWater =
			events.length > 0 ? Math.max(...events.map((event) => getV2EventSequence(event))) : lastProcessedSequence;
		if (!initialized) {
			initialized = true;
			lastProcessedSequence = currentHighWater;
			return;
		}

		const newEvents = events.filter((event) => getV2EventSequence(event) > lastProcessedSequence);
		lastProcessedSequence = currentHighWater;
		for (const event of newEvents) {
			const published = parsePublishedSurfaceRefreshRealtimeEvent(event);
			if (!published || !matchesPublishedSurfaceRefreshRealtimeEvent(published, query)) {
				continue;
			}
			onPublished(published);
		}
	});
}

export interface LoadPublishedSurfaceOptions
	extends PublishedSurfaceQuery {
	maxItems?: number;
	scope?: PublishedSurfaceScopeSnapshot;
}

export interface LoadPublishedSurfacePageResult {
	records: PublishedSurfaceRecord[];
	hasMore: boolean;
}

function currentPublishedSurfaceAuth(): PublishedSurfaceAuthSnapshot {
	const scope = getCurrentScopeIdentity();
	return {
		scope: {
			principal: scope.principal,
			workspace: scope.workspace
		},
		bearer: getCurrentScopeBearerToken()
	};
}

function authHeaders(
	auth: PublishedSurfaceAuthSnapshot,
	headers?: HeadersInit
): Headers {
	const next = new Headers(headers);
	if (auth.bearer) next.set('Authorization', `Bearer ${auth.bearer}`);
	return next;
}

function buildScopedV3Query(
	query: PublishedSurfaceQuery,
	limit?: number
): string {
	const params = new URLSearchParams();
	appendQueryParam(params, 'route', query.route_target);
	appendQueryParam(params, 'task_id', query.task_id);
	appendQueryParam(params, 'agent_id', query.agent_id);
	appendQueryParam(params, 'limit', limit);
	return params.toString();
}

function parsePublishedSurfaceProjection(raw: unknown): PublishedSurfaceProjectionPayload | null {
	const payload = asRecord(raw);
	if (!payload) return null;
	const surface = asRecord(payload.surface);
	const placement = surface ? asRecord(surface.placement) : null;
	if (!surface || !placement) return null;

	const surface_id = readString(surface, 'surface_id');
	const principal = readString(surface, 'principal');
	const workspace = readString(surface, 'workspace');
	const surface_kind = readString(surface, 'surface_kind');
	const status = readString(surface, 'status');
	const route = readString(surface, 'route');
	const document_key = readString(surface, 'document_key');
	const title = readString(surface, 'title');
	const placement_kind = readString(placement, 'placement_kind');
	const published_at = readString(surface, 'published_at');
	const updated_at = readString(surface, 'updated_at');
	const render_origin = readString(payload, 'render_origin');
	const render_kind = readString(payload, 'render_kind');
	const presentation_state = readString(payload, 'presentation_state');

	if (
		!surface_id ||
		!principal ||
		!workspace ||
		!surface_kind ||
		!status ||
		!route ||
		!document_key ||
		!title ||
		!placement_kind ||
		!published_at ||
		!updated_at ||
		!render_origin ||
		!render_kind ||
		!presentation_state
	) {
		return null;
	}
	// Defense-in-depth: backend (since v0.6.445) filters retired surfaces
	// out of the projections list by default, but if a stale realtime
	// projection slips through OR an admin tool calls with `status=all`,
	// keep them out of the canvas. Otherwise unpublished records render
	// with degraded muij data ("unknown · <task_id>") and the per-card
	// Remove button errors because the surface is already retired.
	if (status === 'unpublished' || status === 'superseded') {
		return null;
	}

	return {
		surface: {
			surface_id,
			principal,
			workspace,
			surface_kind,
			status,
			logical_surface_id: readString(surface, 'logical_surface_id'),
			route,
			document_key,
			task_id: readString(surface, 'task_id'),
			ui_thread_id: readString(surface, 'ui_thread_id'),
			source_output_id: readString(surface, 'source_output_id'),
			source_execution_id: readString(surface, 'source_execution_id'),
			media_type: readString(surface, 'media_type'),
			title,
			summary: readString(surface, 'summary'),
			placement: {
				placement_kind,
				placement_id: readString(placement, 'placement_id'),
				pinned: placement.pinned === true
			},
			manifest_artifact_uid: readString(surface, 'manifest_artifact_uid'),
			manifest_name: readString(surface, 'manifest_name'),
			input_artifact_ids: readStringArray(surface, 'input_artifact_ids'),
			published_at,
			unpublished_at: readString(surface, 'unpublished_at'),
			updated_at
		},
		task_title: readString(payload, 'task_title'),
		task_status: readString(payload, 'task_status'),
		source_agent_id: readString(payload, 'source_agent_id'),
		source_output_media_type: readString(payload, 'source_output_media_type'),
		source_output_summary: readString(payload, 'source_output_summary'),
		render_origin,
		render_kind,
		presentation_state
	};
}

async function listPublishedSurfaceProjections(
	query: PublishedSurfaceQuery = {},
	limit?: number,
	auth: PublishedSurfaceAuthSnapshot = currentPublishedSurfaceAuth()
): Promise<PublishedSurfaceProjectionPayload[]> {
	const queryString = buildScopedV3Query(query, limit);
	const response = await timedFetch(
		`${V3_API_BASE}/published-surfaces/projections${queryString ? `?${queryString}` : ''}`,
		{ headers: authHeaders(auth, { Accept: 'application/json' }) }
	);
	await expectOk(response);
	const payload = (await response.json()) as unknown;
	const root = asRecord(payload);
	const surfaces = root?.surfaces;
	if (!Array.isArray(surfaces)) {
		throw new Error('Malformed V3 published surface projection response');
	}
	return surfaces
		.map(parsePublishedSurfaceProjection)
		.filter((item): item is PublishedSurfaceProjectionPayload => item !== null)
		.sort((left, right) => compareNewestFirst(
			{ producer: { produced_at: left.surface.published_at }, artifact_uid: left.surface.surface_id },
			{ producer: { produced_at: right.surface.published_at }, artifact_uid: right.surface.surface_id }
		));
}

export async function loadPublishedSurfaceRecords(
	options: LoadPublishedSurfaceOptions
): Promise<PublishedSurfaceRecord[]> {
	const page = await loadPublishedSurfacePage(options);
	return page.records;
}

/**
 * Mark a published surface as unpublished. Reverses `create_dashboard` and
 * the equivalent V3 `publish_surface` API. The surface is retired (removed
 * from active feed projections + the dashboard / briefing canvas) but is
 * not deleted from disk — admins can republish via the V3 republish API.
 *
 * Returns the resolved `surface_id` on success so the caller can update
 * its local store optimistically. Throws on HTTP errors so the calling
 * component can surface the error to the user.
 */
export async function unpublishPublishedSurface(
	surfaceId: string
): Promise<{ surface_id: string }> {
	const normalized = surfaceId.trim();
	if (!normalized) {
		throw new Error('surfaceId is required');
	}
	const auth = currentPublishedSurfaceAuth();
	const response = await timedFetch(
		`${V3_API_BASE}/published-surfaces/${encodeURIComponent(normalized)}/unpublish`,
		{ method: 'POST', headers: authHeaders(auth) }
	);
	await expectOk(response);
	return { surface_id: normalized };
}

/**
 * Republish a surface — re-runs the publish pipeline (incl. fresh
 * materialization) against the existing surface's task + source. Used to
 * refresh existing surfaces after a backend rendering change without the
 * user having to delete-and-recreate. The rendered muij document is
 * regenerated from the current source bytes, which (since v0.6.441)
 * inlines the actual agent deliverable instead of the auto user-summary's
 * download link.
 *
 * Realtime: republish broadcasts `published_surface.changed`, so connected
 * canvases pick up the new render automatically without a page refresh.
 */
export async function republishPublishedSurface(
	surfaceId: string
): Promise<{ surface_id: string }> {
	const normalized = surfaceId.trim();
	if (!normalized) {
		throw new Error('surfaceId is required');
	}
	const auth = currentPublishedSurfaceAuth();
	const response = await timedFetch(
		`${V3_API_BASE}/published-surfaces/${encodeURIComponent(normalized)}/republish`,
		{
			method: 'POST',
			headers: authHeaders(auth, { 'content-type': 'application/json' }),
			body: '{}'
		}
	);
	await expectOk(response);
	return { surface_id: normalized };
}

async function readPublishedSurfaceRenderWithAuth(
	surfaceId: string,
	auth: PublishedSurfaceAuthSnapshot
): Promise<PublishedSurfaceRenderRecord> {
	const normalized = surfaceId.trim();
	if (!normalized) {
		throw new Error('surfaceId is required');
	}
	const response = await timedFetch(
		`${V3_API_BASE}/published-surfaces/${encodeURIComponent(normalized)}/render`,
		{ headers: authHeaders(auth, { Accept: 'application/json' }) }
	);
	await expectOk(response);
	const payload = (await response.json()) as unknown;
	const root = asRecord(payload);
	const render = root?.render;
	if (!render || typeof render !== 'object' || Array.isArray(render)) {
		throw new Error(`Malformed published surface render payload for '${normalized}'`);
	}
	const parsed = parsePublishedSurfaceRender(render);
	if (!parsed) {
		throw new Error(`Malformed published surface render payload for '${normalized}'`);
	}
	return parsed;
}

export async function readPublishedSurfaceRender(
	surfaceId: string
): Promise<PublishedSurfaceRenderRecord> {
	return readPublishedSurfaceRenderWithAuth(surfaceId, currentPublishedSurfaceAuth());
}

function parsePublishedSurfaceRender(raw: unknown): PublishedSurfaceRenderRecord | null {
	const payload = asRecord(raw);
	if (!payload) return null;
	const surface = asRecord(payload.surface);
	if (!surface) return null;

	const surface_id = readString(surface, 'surface_id');
	const title = readString(surface, 'title');
	const route = readString(surface, 'route');
	const document_key = readString(surface, 'document_key');
	const status = readString(surface, 'status');
	const render_origin = readString(payload, 'render_origin');
	const render_kind = readString(payload, 'render_kind');
	const presentation_state = readString(payload, 'presentation_state');

	if (
		!surface_id ||
		!title ||
		!route ||
		!document_key ||
		!status ||
		!render_origin ||
		!render_kind ||
		!presentation_state
	) {
		return null;
	}

	return {
		surface: {
			surface_id,
			task_id: readString(surface, 'task_id'),
			ui_thread_id: readString(surface, 'ui_thread_id'),
			source_output_id: readString(surface, 'source_output_id'),
			source_execution_id: readString(surface, 'source_execution_id'),
			media_type: readString(surface, 'media_type'),
			title,
			summary: readString(surface, 'summary'),
			route,
			document_key,
			status,
			manifest_name: readString(surface, 'manifest_name')
		},
		task_title: readString(payload, 'task_title'),
		task_status: readString(payload, 'task_status'),
		source_agent_id: readString(payload, 'source_agent_id'),
		source_output_id: readString(payload, 'source_output_id'),
		source_execution_id: readString(payload, 'source_execution_id'),
		source_output_relative_path: readString(payload, 'source_output_relative_path'),
		source_output_summary: readString(payload, 'source_output_summary'),
		media_type: readString(payload, 'media_type'),
		render_origin,
		render_kind,
		presentation_state,
		durable_document_key: readString(payload, 'durable_document_key'),
		durable_manifest_name: readString(payload, 'durable_manifest_name'),
		muij_document: parseMuijDocument(payload.muij_document) ?? undefined,
		text_content: readString(payload, 'text_content'),
		json_content: payload.json_content,
		unavailable_reason: readString(payload, 'unavailable_reason')
	};
}

function syntheticMetadataFromProjection(
	projection: PublishedSurfaceProjectionPayload,
	render: PublishedSurfaceRenderRecord
): SurfaceArtifactMetadata {
	const physical_locator: SurfacePhysicalLocator =
		render.durable_manifest_name
			? {
					type: 'durable_store',
					namespace: SURFACE_MANIFEST_NAMESPACE,
					name: render.durable_manifest_name
				}
			: {
					type: 'in_memory',
					key: `published-surface:${projection.surface.surface_id}`
				};
	return {
		artifact_uid: projection.surface.manifest_artifact_uid ?? projection.surface.surface_id,
		domain: 'ui',
		artifact_type: 'v3:published_surface',
		physical_locator,
		route_target: projection.surface.route,
		ownership: {
			execution_id: render.source_execution_id,
			task_id: projection.surface.task_id
		},
		producer: {
			producer_agent_id: projection.source_agent_id ?? 'unknown',
			producer_stage: undefined,
			produced_at: projection.surface.published_at
		},
		lifecycle_state: projection.surface.status === 'active' ? 'active' : 'stale'
	};
}

function syntheticManifestFromProjection(
	projection: PublishedSurfaceProjectionPayload,
	render: PublishedSurfaceRenderRecord
) {
	return {
		surface_version: 'v3',
		surface_id: projection.surface.surface_id,
		route: projection.surface.route,
		title: projection.surface.title,
		summary: projection.surface.summary ?? render.source_output_summary,
		input_artifacts: [],
		tags: [],
		search_text:
			[projection.surface.title, projection.surface.summary ?? render.source_output_summary ?? '']
				.join(' ')
				.trim() || undefined,
		muij_ref: {
			document_key: render.durable_document_key ?? projection.surface.document_key
		},
		published_at: projection.surface.published_at
	};
}

export async function loadPublishedSurfacePage(
	options: LoadPublishedSurfaceOptions
): Promise<LoadPublishedSurfacePageResult> {
	const {
		maxItems,
		scope,
		...query
	} = options;
	const auth = currentPublishedSurfaceAuth();
	if (
		scope &&
		(scope.principal !== auth.scope.principal || scope.workspace !== auth.scope.workspace)
	) {
		throw new Error('Published surface request scope changed before dispatch');
	}
	const normalizedLimit = typeof maxItems === 'number' && maxItems > 0 ? Math.floor(maxItems) : undefined;
	const normalizedOffset = typeof query.offset === 'number' && query.offset > 0 ? Math.floor(query.offset) : 0;
	const requestLimit =
		typeof normalizedLimit === 'number'
			? normalizedOffset + normalizedLimit + 1
			: undefined;
	const projections = await listPublishedSurfaceProjections(
		{
			...query,
			limit: undefined,
			offset: undefined
		},
		requestLimit,
		auth
	);
	const pageWindow =
		typeof normalizedLimit === 'number'
			? projections.slice(normalizedOffset, normalizedOffset + normalizedLimit + 1)
			: projections.slice(normalizedOffset);
	const hasMore = typeof normalizedLimit === 'number' ? pageWindow.length > normalizedLimit : false;
	const limited = typeof normalizedLimit === 'number' ? pageWindow.slice(0, normalizedLimit) : pageWindow;

	const records: Array<PublishedSurfaceRecord | null> = await Promise.all(
		limited.map(async (projection) => {
			try {
				const render = await readPublishedSurfaceRenderWithAuth(projection.surface.surface_id, auth);
				const syntheticManifest = syntheticManifestFromProjection(projection, render);
				return {
					metadata: syntheticMetadataFromProjection(projection, render),
					manifest: syntheticManifest,
					manifest_name: render.durable_manifest_name ?? `${projection.surface.surface_id}.json`,
					task_title: projection.task_title,
					task_status: projection.task_status,
					source_agent_id: projection.source_agent_id,
					source_output_media_type: projection.source_output_media_type,
					source_output_summary: projection.source_output_summary,
					render_origin: projection.render_origin,
					render_kind: projection.render_kind,
					presentation_state: projection.presentation_state,
					render
				} as PublishedSurfaceRecord;
			} catch (error) {
				console.warn(
					'[publishedSurfaces] failed to load V3 published surface',
					projection.surface.surface_id,
					error
				);
				return null;
			}
		})
	);
	const filteredRecords: PublishedSurfaceRecord[] = records.filter(
		(item): item is PublishedSurfaceRecord => item !== null
	);

	return {
		records: filteredRecords,
		hasMore
	};
}
