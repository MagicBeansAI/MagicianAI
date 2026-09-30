import type { MuijComponent } from '$lib/stores/muijStore';
import type { Task } from '$lib/stores/taskStore';
import { buildPrestoComponentId } from '$lib/magician/presto/contracts/id';

export type ScrollWidgetSource = 'recent' | 'running' | 'overdue' | 'completed' | 'all';

export interface ScrollWidget {
	id: string;
	title: string;
	source: ScrollWidgetSource;
	limit: number;
	createdAt: number;
	lastRefreshedAt: number;
}

export interface ScrollSurfaceInput {
	widgets: ScrollWidget[];
	tasks: Task[];
	isLoading: boolean;
	pageError: string | null;
	interactionBusy: boolean;
	maxWidgets: number;
	lastUpdatedAt: number | null;
	now?: number;
	schemaVersion?: string;
}

const PRESTO_SCROLL_ROUTE = '/briefing' as const;

export type PublishedScrollSortMode = 'newest' | 'title' | 'task' | 'agent';
export type PublishedScrollGroupMode = 'none' | 'task' | 'agent' | 'layer';

export interface PublishedScrollEntry {
	surfaceId: string;
	title: string;
	summary?: string;
	tags: string[];
	layer?: string;
	publishedAt: string;
	taskId?: string;
	agentId?: string;
	producerStage?: string;
	selected: boolean;
}

export interface PublishedScrollViewportInput {
	entries: PublishedScrollEntry[];
	totalLoaded: number;
	visibleCount: number;
	hiddenCount: number;
	isLoading: boolean;
	isRefreshing: boolean;
	routeError: string | null;
	lastUpdatedAt: number | null;
	connectionStatus?: 'connected' | 'connecting' | 'disconnected';
	searchQuery: string;
	sortMode: PublishedScrollSortMode;
	groupMode: PublishedScrollGroupMode;
	hasMore: boolean;
	requestedSurfaceCount: number;
	scopeTaskId?: string;
	scopeAgentId?: string;
	schemaVersion?: string;
	now?: number;
}

export interface PublishedScrollFocusInput {
	entry: PublishedScrollEntry | null;
	isLoading: boolean;
	error: string | null;
	now?: number;
}

interface ScrollWidgetRow {
	taskId: string;
	title: string;
	status: string;
	updatedAt: string;
	updatedSort: number;
	dueDate: string;
	summary: string;
}

interface FormValues {
	widgetSource: ScrollWidgetSource;
	widgetLimit: number;
}

const DEFAULT_WIDGET_LIMIT = 6;
const MAX_TABLE_ROWS = 20;

function asString(value: unknown): string {
	if (typeof value === 'string') return value;
	if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
		return String(value);
	}
	return '';
}

function asFiniteNumber(value: unknown): number | undefined {
	if (typeof value === 'number' && Number.isFinite(value)) return value;
	if (typeof value === 'string') {
		const trimmed = value.trim();
		if (!trimmed) return undefined;
		const parsed = Number(trimmed);
		return Number.isFinite(parsed) ? parsed : undefined;
	}
	return undefined;
}

function clampNumber(value: number | undefined, fallback: number, min: number, max: number): number {
	if (typeof value !== 'number' || !Number.isFinite(value)) return fallback;
	const rounded = Math.round(value);
	return Math.max(min, Math.min(max, rounded));
}

function safeTimestamp(iso: string): number {
	if (!iso) return 0;
	const next = Date.parse(iso);
	return Number.isFinite(next) ? next : 0;
}

function formatRelativeTime(timestamp: number, now: number): string {
	if (!timestamp) return 'never';
	const delta = now - timestamp;
	const abs = Math.abs(delta);
	const minutes = Math.round(abs / 1000 / 60);
	const hours = Math.round(abs / 1000 / 60 / 60);
	const days = Math.round(abs / 1000 / 60 / 60 / 24);
	if (minutes < 1) return 'now';
	if (minutes < 60) return `${minutes}m ${delta >= 0 ? 'ago' : 'from now'}`;
	if (hours < 36) return `${hours}h ${delta >= 0 ? 'ago' : 'from now'}`;
	return `${days}d ${delta >= 0 ? 'ago' : 'from now'}`;
}

function formatDate(value: string | undefined): string {
	if (!value) return 'n/a';
	try {
		return new Date(value).toLocaleDateString();
	} catch {
		return 'invalid date';
	}
}

function sortModeLabel(value: PublishedScrollSortMode): string {
	switch (value) {
		case 'title':
			return 'title';
		case 'task':
			return 'task';
		case 'agent':
			return 'agent';
		case 'newest':
		default:
			return 'newest';
	}
}

function groupModeLabel(value: PublishedScrollGroupMode): string {
	switch (value) {
		case 'task':
			return 'task';
		case 'agent':
			return 'agent';
		case 'layer':
			return 'layer';
		case 'none':
		default:
			return 'none';
	}
}

function connectionLabel(
	connectionStatus: PublishedScrollViewportInput['connectionStatus'],
	isRefreshing: boolean
): string {
	if (isRefreshing) return 'refreshing';
	switch (connectionStatus) {
		case 'connected':
			return 'live';
		case 'connecting':
			return 'reconnecting';
		case 'disconnected':
			return 'offline';
		default:
			return 'idle';
	}
}

function scopeSummary(taskId: string | undefined, agentId: string | undefined): string {
	const parts: string[] = [];
	if (taskId) parts.push(`task ${taskId}`);
	if (agentId) parts.push(`agent ${agentId}`);
	return parts.length > 0 ? parts.join(' · ') : 'workspace feed';
}

function compactTags(tags: string[]): string {
	if (tags.length === 0) return 'no tags';
	return tags.slice(0, 3).join(', ');
}

function groupEntries(
	entries: PublishedScrollEntry[],
	groupMode: PublishedScrollGroupMode
): Array<{ id: string; title: string; subtitle: string; entries: PublishedScrollEntry[] }> {
	if (groupMode === 'none') {
		return [
			{
				id: 'all',
				title: 'All dashboards',
				subtitle: `${entries.length} loaded in the current route space`,
				entries
			}
		];
	}

	const grouped = new Map<string, PublishedScrollEntry[]>();
	for (const entry of entries) {
		const groupValue =
			groupMode === 'task'
				? entry.taskId || 'unscoped'
				: groupMode === 'agent'
					? entry.agentId || 'unknown-agent'
					: entry.layer || 'default-layer';
		const bucket = grouped.get(groupValue);
		if (bucket) {
			bucket.push(entry);
		} else {
			grouped.set(groupValue, [entry]);
		}
	}

	return Array.from(grouped.entries())
		.sort(([left], [right]) => left.localeCompare(right))
		.map(([value, groupEntries]) => ({
			id: value,
			title:
				groupMode === 'task'
					? `Task ${value}`
					: groupMode === 'agent'
						? `Agent ${value}`
						: `Layer ${value}`,
			subtitle: `${groupEntries.length} dashboard${groupEntries.length === 1 ? '' : 's'}`,
			entries: groupEntries
		}));
}

function publishedScrollEntrySubtitle(entry: PublishedScrollEntry, now: number): string {
	const details: string[] = [`published ${formatRelativeTime(safeTimestamp(entry.publishedAt), now)}`];
	if (entry.taskId) details.push(`task ${entry.taskId}`);
	if (entry.agentId) details.push(`agent ${entry.agentId}`);
	if (entry.layer) details.push(`layer ${entry.layer}`);
	return details.join(' · ');
}

function buildPublishedScrollEntryCards(
	entries: PublishedScrollEntry[],
	now: number
): MuijComponent[] {
	return entries.map((entry) => {
		const focusId = buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-focus', entry.surfaceId);
		const dismissId = buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-dismiss', entry.surfaceId);
		const taskOpenId = entry.taskId
			? buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-open-task', entry.taskId)
			: null;
		const agentOpenId = entry.agentId
			? buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-open-agent', entry.agentId)
			: null;

		const actions: MuijComponent[] = [
			{
				id: focusId,
				component_type: 'Button',
				label: entry.selected ? 'Focused' : 'Focus',
				props: {
					interactive: true,
					variant: entry.selected ? 'primary' : 'secondary',
					size: 'sm'
				}
			},
			{
				id: dismissId,
				component_type: 'Button',
				label: 'Dismiss',
				props: {
					interactive: true,
					variant: 'outline',
					size: 'sm'
				}
			}
		];

		if (taskOpenId) {
			actions.push({
				id: taskOpenId,
				component_type: 'Button',
				label: 'Open task',
				props: {
					interactive: true,
					variant: 'secondary',
					size: 'sm'
				}
			});
		}
		if (agentOpenId) {
			actions.push({
				id: agentOpenId,
				component_type: 'Button',
				label: 'Open agent',
				props: {
					interactive: true,
					variant: 'secondary',
					size: 'sm'
				}
			});
		}

		return {
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-card', entry.surfaceId),
			component_type: 'Card',
			label: entry.title,
			props: {
				title: entry.title,
				subtitle: publishedScrollEntrySubtitle(entry, now),
				body: entry.summary || `Tags: ${compactTags(entry.tags)}`
			},
			children: [
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-meta', entry.surfaceId),
					component_type: 'DataList',
					props: {
						items: [
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-meta-tags', entry.surfaceId),
								key: 'Tags',
								value: compactTags(entry.tags)
							},
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-meta-stage', entry.surfaceId),
								key: 'Stage',
								value: entry.producerStage || 'published'
							},
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-meta-layer', entry.surfaceId),
								key: 'Layer',
								value: entry.layer || 'default'
							},
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-meta-date', entry.surfaceId),
								key: 'Published',
								value: formatDate(entry.publishedAt)
							}
						]
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-actions', entry.surfaceId),
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.5rem',
						wrap: true
					},
					children: actions
				}
			]
		};
	});
}

function normalizeSource(raw: unknown): ScrollWidgetSource {
	const normalized = asString(raw).trim().toLowerCase();
	if (normalized === 'recent' || normalized === 'running' || normalized === 'overdue' || normalized === 'completed' || normalized === 'all') {
		return normalized;
	}
	return 'recent';
}

function sourceLabel(source: ScrollWidgetSource): string {
	switch (source) {
		case 'recent':
			return 'recent';
		case 'running':
			return 'running + planned';
		case 'overdue':
			return 'overdue';
		case 'completed':
			return 'completed';
		case 'all':
			return 'all';
		default:
			return 'recent';
	}
}

function isOverdue(task: Task, today: string): boolean {
	return !!task.dueDate && task.dueDate < today && task.status !== 'completed';
}

function buildWidgetRows(tasks: Task[], source: ScrollWidgetSource, limit: number): ScrollWidgetRow[] {
	const today = new Date().toISOString().split('T')[0] ?? '';
	const normalizedLimit = Math.max(1, Math.min(MAX_TABLE_ROWS, limit || DEFAULT_WIDGET_LIMIT));
	const sourceRows = tasks.filter((task) => {
		if (!task) return false;
		switch (source) {
			case 'running':
				return ['pending', 'planning', 'ready', 'running', 'paused'].includes(task.status);
			case 'completed':
				return task.status === 'completed';
			case 'overdue':
				return isOverdue(task, today);
			case 'all':
				return true;
			case 'recent':
			default:
				return true;
		}
	});

	const sorted = [...sourceRows].sort((left, right) => safeTimestamp(right.updatedAt) - safeTimestamp(left.updatedAt));
	const selected = sorted.slice(0, normalizedLimit);

	return selected.map((task) => ({
		taskId: task.id || 'unknown-task-id',
		title: asString(task.title).trim() || 'Untitled task',
		status: asString(task.status) || 'unknown',
		updatedAt: formatDate(task.updatedAt),
		updatedSort: safeTimestamp(task.updatedAt),
		dueDate: formatDate(task.dueDate),
		summary: asString(task.description).trim() || 'no summary available'
	}));
}

function buildWidgetCards(widgets: ScrollWidget[], tasks: Task[], now: number, interactionBusy: boolean): MuijComponent[] {
	return widgets.map((widget) => {
		const rows = buildWidgetRows(tasks, widget.source, widget.limit);
		const tableRows = rows.map((row, index) => ({
			task_id: row.taskId,
			title: row.title,
			status: row.status,
			updated: row.updatedAt,
			due: row.dueDate,
			summary: row.summary,
			_updated_sort: row.updatedSort,
			_index: String(index + 1)
		}));

		const sourceSummary = rows.length;
		return {
			id: `scroll-widget-${widget.id}`,
			component_type: 'Card',
			label: widget.title,
			props: {
				title: widget.title,
				subtitle: `${sourceSummary} item${sourceSummary === 1 ? '' : 's'} · source ${sourceLabel(widget.source)}`,
				body: `Last refresh: ${formatRelativeTime(widget.lastRefreshedAt, now)}`
			},
			children: [
				{
					id: `scroll-widget-${widget.id}-summary`,
					component_type: 'DataList',
					props: {
						items: [
							{
								id: `scroll-widget-${widget.id}-summary-source`,
								key: 'Source',
								value: sourceLabel(widget.source)
							},
							{
								id: `scroll-widget-${widget.id}-summary-limit`,
								key: 'Item limit',
								value: String(widget.limit)
							},
							{
								id: `scroll-widget-${widget.id}-summary-created`,
								key: 'Created',
								value: formatRelativeTime(widget.createdAt, now)
							}
					]
				}
				},
				{
					id: `scroll-widget-${widget.id}-table`,
					component_type: 'Table',
					props: {
						columns: [
							{ key: 'title', label: 'Task', sortable: true },
							{ key: 'status', label: 'Status', sortable: true },
							{ key: 'updated', label: 'Updated', sortable: true },
							{ key: 'due', label: 'Due' },
							{ key: 'summary', label: 'Summary' }
						],
						rows: tableRows,
						sortKey: 'updated',
						sortValueByKey: {
							updated: '_updated_sort'
						},
						sortDir: 'desc'
					}
				},
				{
					id: `scroll-widget-${widget.id}-actions`,
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.6rem',
						wrap: true
					},
					children: [
						{
							id: `scroll-widget:${widget.id}:refresh`,
							component_type: 'Button',
							label: interactionBusy ? 'Refreshing...' : 'Refresh widget',
							props: {
								interactive: true,
								variant: 'secondary',
								size: 'sm',
								disabled: interactionBusy
							}
						},
						{
							id: `scroll-widget:${widget.id}:remove`,
							component_type: 'Button',
							label: 'Remove',
							props: {
								interactive: true,
								variant: 'outline',
								size: 'sm',
								disabled: interactionBusy
							}
						}
					]
				}
			]
		};
	});
}

function buildTopMetricCards(
	totalTasks: number,
	activeWidgets: number,
	displayedRows: number,
	lastUpdatedAt: number | null,
	now: number
): MuijComponent[] {
	return [
		{
			id: 'scroll-metric-total-tasks',
			component_type: 'MetricCard',
			label: 'Tasks tracked',
			props: {
				value: String(totalTasks),
				label: 'task records loaded'
			}
		},
		{
			id: 'scroll-metric-active-widgets',
			component_type: 'MetricCard',
			label: 'Active widgets',
			props: {
				value: String(activeWidgets),
				label: 'configured dashboard views'
			}
		},
		{
			id: 'scroll-metric-updated-rows',
			component_type: 'MetricCard',
			label: 'Rows in view',
			props: {
				value: String(displayedRows),
				label: 'based on visible task set'
			}
		},
		{
			id: 'scroll-metric-last-refresh',
			component_type: 'MetricCard',
			label: 'Global refresh',
			props: {
				value: formatRelativeTime(lastUpdatedAt ?? 0, now),
				label: 'briefing data freshness'
			}
		}
	];
}

function readFormField(raw: Record<string, unknown>, keys: string[]): unknown {
	for (const key of keys) {
		if (Object.prototype.hasOwnProperty.call(raw, key)) {
			return raw[key];
		}
	}
	return undefined;
}

function parseAddWidgetValues(raw: Record<string, unknown>): FormValues {
	const source = normalizeSource(readFormField(raw, ['widget_source', 'widgetSource', 'source']));
	const limit = clampNumber(
		asFiniteNumber(readFormField(raw, ['widget_limit', 'widgetLimit', 'limit'])),
		DEFAULT_WIDGET_LIMIT,
		1,
		MAX_TABLE_ROWS
	);
	return { widgetSource: source, widgetLimit: limit };
}

export function buildScrollSurface(input: ScrollSurfaceInput): MuijComponent[] {
	const safeNow = input.now ?? Date.now();
	const safeTasks = Array.isArray(input.tasks) ? input.tasks : [];
	const safeWidgets = Array.isArray(input.widgets) ? input.widgets : [];
	const widgetCount = safeWidgets.length;
	const visibleRows = safeTasks.length;

	const hasSchema = asString(input.schemaVersion);
	const refreshActionLabel = input.interactionBusy ? 'Refreshing...' : 'Refresh all';

	const base: MuijComponent[] = [
		{
			id: 'scroll-surface-header',
			component_type: 'Card',
			label: 'briefing-overview',
			props: {
				title: 'Briefing',
				subtitle: 'Dynamic GAUI view from active task + task-history state',
				body: hasSchema ? `surface schema ${hasSchema}` : 'GAUI-only briefing surface'
			}
		},
		{
			id: 'scroll-metric-grid',
			component_type: 'Grid',
			props: {
				autoFit: true,
				minColumnWidth: '190px',
				gap: '0.75rem'
			},
			children: buildTopMetricCards(safeTasks.length, widgetCount, visibleRows, input.lastUpdatedAt, safeNow)
		}
	];

	if (input.pageError) {
		base.push({
			id: 'scroll-surface-error',
			component_type: 'Alert',
			label: 'Briefing data warning',
			props: {
				type: 'error',
				message: input.pageError,
				closable: true
			}
		});
	}

	base.push({
		id: 'scroll-controls',
		component_type: 'Card',
		label: 'briefing-controls',
		props: {
			title: 'Controls',
			subtitle: `${widgetCount}/${input.maxWidgets} widgets configured`,
			body: input.interactionBusy
				? 'Applying requested widget update...'
				: 'Add, refresh, or remove widgets to shape this surface.'
		},
		children: [
			{
				id: 'scroll-controls-summary',
				component_type: 'DataList',
				props: {
					items: [
						{
							id: 'scroll-controls-summary-loading',
							key: 'Task stream',
							value: input.isLoading ? 'updating...' : 'online'
						},
						{
							id: 'scroll-controls-summary-timestamp',
							key: 'Last refresh',
							value: formatRelativeTime(input.lastUpdatedAt ?? 0, safeNow)
						}
					]
				}
			},
			{
				id: 'scroll-controls-actions',
				component_type: 'Stack',
				props: {
					direction: 'row',
					gap: '0.6rem',
					wrap: true
				},
				children: [
					{
						id: 'scroll-refresh-all',
						component_type: 'Button',
						label: refreshActionLabel,
						props: {
							interactive: true,
							variant: 'primary',
							size: 'sm',
							disabled: input.interactionBusy
						}
					}
				]
			},
			{
				id: 'scroll-add-widget-form',
				component_type: 'Form',
				label: 'Add widget',
				props: {
					title: 'Add widget',
					showSubmit: true,
					submitLabel: 'Add widget',
					disabled: input.interactionBusy || widgetCount >= input.maxWidgets,
					fields: [
						{
							id: 'widget_title',
							label: 'Widget title',
							type: 'text',
							required: true,
							value: '',
							placeholder: 'e.g., Completed work',
							rows: 3
						},
						{
							id: 'widget_source',
							label: 'Source',
							type: 'select',
							required: true,
							value: 'recent',
							options: [
								{ value: 'recent', label: 'Recent' },
								{ value: 'running', label: 'Running + planned' },
								{ value: 'overdue', label: 'Overdue' },
								{ value: 'completed', label: 'Completed' },
								{ value: 'all', label: 'All tasks' }
							],
							placeholder: 'recent'
						},
						{
							id: 'widget_limit',
							label: 'Maximum rows',
							type: 'number',
							required: true,
							value: DEFAULT_WIDGET_LIMIT,
							min: 1,
							max: MAX_TABLE_ROWS,
							step: 1,
							placeholder: '6'
						}
					],
					idBase: `scroll-add-widget-form`
				}
				},
				{
					id: `scroll-controls-form-hint`,
					component_type: 'Alert',
					label: 'Widget form',
					props: {
						type: 'info',
						message: input.interactionBusy || widgetCount >= input.maxWidgets
							? 'Form disabled while action is processing or limit reached.'
							: `You can add up to ${input.maxWidgets} widgets.`
					}
				}
			]
		});

	base.push({
		id: 'scroll-widget-grid',
		component_type: 'Grid',
		label: 'Active widget cards',
		props: {
			autoFit: true,
			minColumnWidth: '320px',
			gap: '0.85rem'
		},
		children: buildWidgetCards(safeWidgets, safeTasks, safeNow, input.interactionBusy)
	});

	return base;
}

export function normalizeScrollWidget(input: Partial<ScrollWidget> & { id: unknown; title: unknown }): ScrollWidget | null {
	const id = asString(input.id).trim();
	if (!id) return null;
	const title = asString(input.title).trim();
	if (!title) return null;
	return {
		id,
		title,
		source: normalizeSource(input.source),
		limit: clampNumber(asFiniteNumber(input.limit), DEFAULT_WIDGET_LIMIT, 1, MAX_TABLE_ROWS),
		createdAt: asFiniteNumber(input.createdAt) ?? Date.now(),
		lastRefreshedAt: asFiniteNumber(input.lastRefreshedAt) ?? Date.now()
	};
}

export function readFormDefaults(values: Record<string, unknown> | undefined): FormValues {
	if (!values || typeof values !== 'object') {
		return { widgetSource: 'recent', widgetLimit: DEFAULT_WIDGET_LIMIT };
	}
	return parseAddWidgetValues(values);
}

export function buildPublishedScrollViewportSurface(
	input: PublishedScrollViewportInput
): MuijComponent[] {
	const safeNow = input.now ?? Date.now();
	const safeEntries = Array.isArray(input.entries) ? input.entries : [];
	const refreshLabel = input.isRefreshing ? 'Refreshing...' : 'Refresh feed';
	const scopeLabel = scopeSummary(input.scopeTaskId, input.scopeAgentId);
	const groupedEntries = groupEntries(safeEntries, input.groupMode);

	const components: MuijComponent[] = [
		{
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'viewport-header'),
			component_type: 'Card',
			label: 'briefing-published-header',
			props: {
				title: 'Briefing',
				subtitle: `Published dashboards · ${scopeLabel}`,
				body: input.schemaVersion
					? `Viewport schema ${input.schemaVersion}`
					: 'Artifact-driven dashboard viewport'
			}
		},
		{
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'viewport-metrics'),
			component_type: 'Grid',
			props: {
				autoFit: true,
				minColumnWidth: '180px',
				gap: '0.75rem'
			},
			children: [
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'metric-loaded'),
					component_type: 'MetricCard',
					label: 'Loaded dashboards',
					props: {
						value: String(input.totalLoaded),
						label: 'latest route publications'
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'metric-visible'),
					component_type: 'MetricCard',
					label: 'Visible dashboards',
					props: {
						value: String(input.visibleCount),
						label: 'after local search + dismiss'
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'metric-hidden'),
					component_type: 'MetricCard',
					label: 'Hidden dashboards',
					props: {
						value: String(input.hiddenCount),
						label: 'dismissed in this viewport'
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'metric-stream'),
					component_type: 'MetricCard',
					label: 'Event stream',
					props: {
						value: connectionLabel(input.connectionStatus, input.isRefreshing),
						label: input.lastUpdatedAt
							? `last sync ${formatRelativeTime(input.lastUpdatedAt, safeNow)}`
							: 'waiting for first sync'
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'metric-space'),
					component_type: 'MetricCard',
					label: 'Surface space',
					props: {
						value:
							input.groupMode === 'none'
								? input.hasMore
									? `${input.requestedSurfaceCount}+`
									: String(input.totalLoaded)
								: String(groupedEntries.length),
						label:
							input.groupMode === 'none'
								? 'loaded route-scoped search window'
								: `${groupModeLabel(input.groupMode)} groups in the current window`
					}
				}
			]
		},
		{
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'filter-form'),
			component_type: 'Form',
			label: 'Filter dashboards',
			props: {
				title: 'Filter dashboards',
				showSubmit: true,
				submitLabel: 'Apply',
				disabled: input.isLoading || input.isRefreshing,
				fields: [
					{
						id: 'search_query',
						label: 'Search',
						type: 'search',
						required: false,
						value: input.searchQuery,
						placeholder: 'title / tags / search text'
					},
					{
						id: 'sort_mode',
						label: 'Sort',
						type: 'select',
						required: true,
						value: input.sortMode,
						options: [
							{ value: 'newest', label: 'Newest' },
							{ value: 'title', label: 'Title' },
							{ value: 'task', label: 'Task' },
							{ value: 'agent', label: 'Agent' }
						]
					},
					{
						id: 'group_mode',
						label: 'Group',
						type: 'select',
						required: true,
						value: input.groupMode,
						options: [
							{ value: 'none', label: 'None' },
							{ value: 'task', label: 'Task' },
							{ value: 'agent', label: 'Agent' },
							{ value: 'layer', label: 'Layer' }
						]
					}
				],
				idBase: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'filter-form')
			}
		},
		{
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'toolbar'),
			component_type: 'Stack',
			props: {
				direction: 'row',
				gap: '0.6rem',
				wrap: true
			},
			children: [
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'refresh-all'),
					component_type: 'Button',
					label: refreshLabel,
					props: {
						interactive: true,
						variant: 'primary',
						size: 'sm',
						disabled: input.isLoading || input.isRefreshing
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'clear-filters'),
					component_type: 'Button',
					label: 'Clear filters',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled:
							input.searchQuery.trim().length === 0 &&
							input.sortMode === 'newest' &&
							input.groupMode === 'none'
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'reset-hidden'),
					component_type: 'Button',
					label: 'Reset hidden',
					props: {
						interactive: true,
						variant: 'secondary',
						size: 'sm',
						disabled: input.hiddenCount === 0
					}
				},
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'load-more'),
					component_type: 'Button',
					label: input.hasMore ? 'Load more' : 'Surface window complete',
					props: {
						interactive: true,
						variant: 'secondary',
						size: 'sm',
						disabled: input.isLoading || input.isRefreshing || !input.hasMore
					}
				}
			]
		}
	];

	if (input.routeError) {
		components.push({
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'route-error'),
			component_type: 'Alert',
			label: 'Briefing route error',
			props: {
				type: 'error',
				message: input.routeError,
				closable: true
			}
		});
	}

	if (input.isLoading && safeEntries.length === 0) {
		components.push({
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'loading-card'),
			component_type: 'Card',
			label: 'Loading dashboards',
			props: {
				title: 'Loading published dashboards',
				subtitle: scopeLabel,
				body: 'Reading the latest route-bound surface manifests and preparing the focused viewport.'
			}
		});
		return components;
	}

	if (safeEntries.length === 0) {
		components.push({
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'empty-state'),
			component_type: 'EmptyState',
			label: 'No dashboards available',
			props: {
				title: 'No published dashboards yet',
				description:
					'Publish a task output to /briefing, clear narrow filters, or expand the loaded surface window.'
			}
		});
		return components;
	}

	if (input.groupMode === 'none') {
		components.push(...buildPublishedScrollEntryCards(safeEntries, safeNow));
		return components;
	}

	components.push(
		...groupedEntries.map((group) => ({
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'surface-group', group.id),
			component_type: 'Card',
			label: group.title,
			props: {
				title: group.title,
				subtitle: group.subtitle,
				body: `Grouped by ${groupModeLabel(input.groupMode)}`
			},
			children: buildPublishedScrollEntryCards(group.entries, safeNow)
		}))
	);
	return components;
}

export function buildPublishedScrollFocusSurface(
	input: PublishedScrollFocusInput
): MuijComponent[] {
	const safeNow = input.now ?? Date.now();
	const entry = input.entry;

	if (!entry) {
		return [
			{
				id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-empty'),
				component_type: 'EmptyState',
				label: 'No focused dashboard',
				props: {
					title: 'No dashboard selected',
					description: 'Choose a dashboard from the feed to focus it in the main viewport.'
				}
			}
		];
	}

	const components: MuijComponent[] = [
		{
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-header', entry.surfaceId),
			component_type: 'Card',
			label: entry.title,
			props: {
				title: entry.title,
				subtitle: publishedScrollEntrySubtitle(entry, safeNow),
				body: entry.summary || `Tags: ${compactTags(entry.tags)}`
			},
			children: [
				{
					id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-meta', entry.surfaceId),
					component_type: 'DataList',
					props: {
						items: [
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-meta-tags', entry.surfaceId),
								key: 'Tags',
								value: compactTags(entry.tags)
							},
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-meta-task', entry.surfaceId),
								key: 'Task',
								value: entry.taskId || 'n/a'
							},
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-meta-agent', entry.surfaceId),
								key: 'Agent',
								value: entry.agentId || 'n/a'
							},
							{
								id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-meta-layer', entry.surfaceId),
								key: 'Layer',
								value: entry.layer || 'default'
							}
						]
					}
				}
			]
		}
	];

	if (input.error) {
		components.push({
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-error', entry.surfaceId),
			component_type: 'Alert',
			label: 'Dashboard load error',
			props: {
				type: 'error',
				message: input.error
			}
		});
		return components;
	}

	if (input.isLoading) {
		components.push({
			id: buildPrestoComponentId(PRESTO_SCROLL_ROUTE, 'focus-loading', entry.surfaceId),
			component_type: 'Card',
			label: 'Loading focused dashboard',
			props: {
				title: 'Loading focused dashboard',
				subtitle: entry.title,
				body: 'Reading the published GAUI document for this dashboard.'
			}
		});
	}

	return components;
}
