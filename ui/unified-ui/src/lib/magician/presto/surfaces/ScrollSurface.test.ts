import { describe, expect, it } from 'vitest';

import { validatePrestoRouteComponents } from '$lib/magician/presto/validation';

import {
	buildPublishedScrollFocusSurface,
	buildPublishedScrollViewportSurface
} from './ScrollSurface';

describe('ScrollSurface published viewport builders', () => {
	it('produces contract-valid chrome for /briefing', () => {
		const components = buildPublishedScrollViewportSurface({
			entries: [
				{
					surfaceId: 'surf-1',
					title: 'Campaign Briefing',
					summary: 'Revenue, conversions, and budget pacing.',
					tags: ['campaign', 'marketing'],
					layer: 'briefings',
					publishedAt: '2026-03-14T11:00:00Z',
					taskId: 'task-1',
					agentId: 'agent-a',
					producerStage: 'analyze',
					selected: true
				}
			],
			totalLoaded: 1,
			visibleCount: 1,
			hiddenCount: 0,
			isLoading: false,
			isRefreshing: false,
			routeError: null,
			lastUpdatedAt: Date.parse('2026-03-14T11:05:00Z'),
			connectionStatus: 'connected',
			searchQuery: '',
			sortMode: 'newest',
			groupMode: 'none',
			hasMore: true,
			requestedSurfaceCount: 10,
			schemaVersion: 'scroll-surface-v1',
			now: Date.parse('2026-03-14T11:10:00Z')
		});

		const validation = validatePrestoRouteComponents('/briefing', components);
		expect(validation.ok).toBe(true);
		expect(validation.errors).toEqual([]);
	});

	it('returns an empty-state focus shell when nothing is selected', () => {
		const components = buildPublishedScrollFocusSurface({
			entry: null,
			isLoading: false,
			error: null
		});

		expect(components).toHaveLength(1);
		expect(components[0]?.component_type).toBe('EmptyState');
	});

	it('groups entries when task grouping is enabled', () => {
		const components = buildPublishedScrollViewportSurface({
			entries: [
				{
					surfaceId: 'surf-1',
					title: 'Campaign Briefing',
					summary: 'Revenue, conversions, and budget pacing.',
					tags: ['campaign', 'marketing'],
					layer: 'briefings',
					publishedAt: '2026-03-14T11:00:00Z',
					taskId: 'task-1',
					agentId: 'agent-a',
					producerStage: 'analyze',
					selected: true
				},
				{
					surfaceId: 'surf-2',
					title: 'Channel Review',
					summary: 'Channel pacing and spend.',
					tags: ['channels'],
					layer: 'briefings',
					publishedAt: '2026-03-14T10:30:00Z',
					taskId: 'task-1',
					agentId: 'agent-b',
					producerStage: 'summarize',
					selected: false
				}
			],
			totalLoaded: 2,
			visibleCount: 2,
			hiddenCount: 0,
			isLoading: false,
			isRefreshing: false,
			routeError: null,
			lastUpdatedAt: Date.parse('2026-03-14T11:05:00Z'),
			connectionStatus: 'connected',
			searchQuery: '',
			sortMode: 'newest',
			groupMode: 'task',
			hasMore: false,
			requestedSurfaceCount: 10,
			schemaVersion: 'scroll-surface-v1',
			now: Date.parse('2026-03-14T11:10:00Z')
		});

		expect(
			components.some(
				(component) => component.id === 'presto.briefing.surface-group.task-1'
			)
		).toBe(true);
	});
});
