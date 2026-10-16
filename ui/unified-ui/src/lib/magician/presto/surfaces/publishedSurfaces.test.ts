import { afterEach, describe, expect, it, vi } from 'vitest';

import type { V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { PUBLISHED_SURFACE_CHANGED_EVENT_TYPE } from '$lib/types/surfaces';

import {
	loadPublishedSurfacePage,
	matchesPublishedSurfaceRefreshRealtimeEvent,
	parsePublishedSurfaceRefreshRealtimeEvent
} from './publishedSurfaces';

function makePublishedSurfaceRefreshEvent(
	payload: Record<string, unknown> = {}
): V2WebSocketEvent {
	return {
		event_type: 'AgentEvent',
		data: {
			event: {
				event_type: PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
				agent_id: 'planner',
				timestamp: Date.now(),
				payload: {
					surface_id: 'surf-123',
					// Match the default scope from `scopeIdentityStore`
					// (`anonymous/default`) so the matcher's scope check
					// passes in the unit-test environment.
					principal: 'anonymous',
					workspace: 'default',
					status: 'active',
					route: '/briefing',
					document_key: 'surface-surf-123',
					execution_id: 'execution-1',
					task_id: 'task-1',
					published_at: '2026-03-14T10:00:00Z',
					updated_at: '2026-03-14T10:00:00Z',
					...payload
				}
			}
		}
	};
}

function makeProjectionSurface(
	surface_id: string,
	title: string,
	published_at = '2026-03-14T10:00:00Z'
) {
	return {
		surface: {
			surface_id,
			principal: 'default',
			workspace: 'default',
			surface_kind: 'briefing',
			status: 'active',
			route: '/briefing',
			document_key: `surface-${surface_id}`,
			task_id: 'task-1',
			ui_thread_id: 'general',
			source_output_id: 'output-1',
			source_execution_id: 'execution-1',
			media_type: 'text/markdown',
			title,
			summary: `${title} summary`,
			placement: {
				placement_kind: 'workspace',
				pinned: true
			},
			manifest_artifact_uid: `artifact-${surface_id}`,
			manifest_name: `${surface_id}.json`,
			input_artifact_ids: [],
			published_at,
			updated_at: published_at
		},
		task_title: 'Desk task',
		task_status: 'completed',
		source_agent_id: 'planner',
		source_output_media_type: 'text/markdown',
		source_output_summary: `${title} summary`,
		render_origin: 'durable_surface',
		render_kind: 'muij_surface',
		presentation_state: 'durable_surface_ready'
	};
}

function makeRenderPayload(surface_id: string) {
	return {
		render: {
			surface: {
				surface_id,
				task_id: 'task-1',
				ui_thread_id: 'general',
				source_output_id: 'output-1',
				source_execution_id: 'execution-1',
				media_type: 'text/markdown',
				title: 'Campaign Briefing',
				summary: 'Campaign Briefing summary',
				route: '/briefing',
				document_key: `surface-${surface_id}`,
				status: 'active',
				manifest_name: `${surface_id}.json`
			},
			task_title: 'Desk task',
			task_status: 'completed',
			source_agent_id: 'planner',
			source_output_id: 'output-1',
			source_execution_id: 'execution-1',
			source_output_relative_path: 'outputs/task_user.md',
			source_output_summary: 'Campaign Briefing summary',
			media_type: 'text/markdown',
			render_origin: 'durable_surface',
			render_kind: 'muij_surface',
			presentation_state: 'durable_surface_ready',
			durable_document_key: `surface-${surface_id}`,
			durable_manifest_name: `${surface_id}.json`
		}
	};
}

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('published surface realtime helpers', () => {
	it('parses published_surface.changed events from the generic agent envelope', () => {
		const event = parsePublishedSurfaceRefreshRealtimeEvent(makePublishedSurfaceRefreshEvent());
		expect(event).not.toBeNull();
		expect(event?.producer_agent_id).toBe('planner');
		expect(event?.route).toBe('/briefing');
		expect(event?.status).toBe('active');
		expect(event?.principal).toBe('anonymous');
	});

	it('ignores unrelated realtime events', () => {
		const event: V2WebSocketEvent = {
			event_type: 'AgentCycleCompleted',
			data: {
				agent_id: 'planner',
				goal_id: 'goal-1',
				cycle_id: 'cycle-1',
				outcome: 'success',
				iterations_used: 1,
				timestamp: Date.now()
			}
		};

		expect(parsePublishedSurfaceRefreshRealtimeEvent(event)).toBeNull();
	});

	it('filters parsed events by route and ownership scope', () => {
		const parsed = parsePublishedSurfaceRefreshRealtimeEvent(makePublishedSurfaceRefreshEvent());
		expect(parsed).not.toBeNull();
		if (!parsed) {
			throw new Error('expected parsed event');
		}

		expect(
			matchesPublishedSurfaceRefreshRealtimeEvent(parsed, {
				route_target: '/briefing',
				execution_id: 'execution-1',
				task_id: 'task-1',
				agent_id: 'planner'
			})
		).toBe(true);

		expect(
			matchesPublishedSurfaceRefreshRealtimeEvent(parsed, {
				route_target: '/tasks'
			})
		).toBe(false);

		expect(
			matchesPublishedSurfaceRefreshRealtimeEvent(parsed, {
				execution_id: 'execution-2'
			})
		).toBe(false);

		expect(
			matchesPublishedSurfaceRefreshRealtimeEvent(parsed, {
				agent_id: 'other-agent'
			})
		).toBe(false);
	});

	it('loads a paged route-scoped surface window and reports hasMore', async () => {
		const fetchMock = vi.fn(async (input: string | URL | Request) => {
			const url = String(input);
			if (url.includes('/api/magician/v3/published-surfaces/projections')) {
				expect(url).toContain('route=%2Fbriefing');
				expect(url).toContain('limit=2');
				return new Response(
					JSON.stringify({
						surfaces: [
							makeProjectionSurface('surf-1', 'Earlier Briefing', '2026-03-14T10:00:00Z'),
							makeProjectionSurface('surf-2', 'Campaign Briefing', '2026-03-14T10:05:00Z')
						]
					}),
					{ status: 200 }
				);
			}
			if (url.includes('/api/magician/v3/published-surfaces/surf-2/render')) {
				return new Response(JSON.stringify(makeRenderPayload('surf-2')), { status: 200 });
			}
			throw new Error(`unexpected fetch: ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);

		const result = await loadPublishedSurfacePage({
			route_target: '/briefing',
			maxItems: 1
		});

		expect(result.hasMore).toBe(true);
		expect(result.records).toHaveLength(1);
		expect(result.records[0]?.manifest.title).toBe('Campaign Briefing');
		expect(result.records[0]?.metadata.artifact_uid).toBe('artifact-surf-2');
		expect(result.records[0]?.render?.render_kind).toBe('muij_surface');
		expect(fetchMock).toHaveBeenCalledTimes(2);
	});
});
