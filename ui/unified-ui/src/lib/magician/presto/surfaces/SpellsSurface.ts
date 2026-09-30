import type { MuijComponent } from '$lib/stores/muijStore';
import type { Task, TaskStatus } from '$lib/stores/taskStore';
import { cronToHumanReadable } from '$lib/utils/cron';
import { sanitizeIdSegment } from '$lib/magician/presto/contracts/id';
import { statusBadgeTone, statusTone } from '$lib/shared/statusTone';
import type {
	AgentPickerOption,
	ParsedTaskAction,
	ParsedTaskCompletionChange
} from '$lib/magician/tasks/types';
export type {
	AgentPickerOption,
	ParsedTaskAction,
	ParsedTaskCompletionChange
} from '$lib/magician/tasks/types';

export type PrestoTaskSurfaceMode = 'home' | 'spells';

/** Task-id class token stamped on a card's ⋯ overflow trigger button so
 *  pages can locate the real DOM node as the TaskCardMenu anchor. Pages
 *  build their `[class~="…"]` selectors from this same helper. */
export function taskMenuTriggerClass(taskId: string): string {
	return `presto-task-menu-trigger--${sanitizeIdSegment(taskId)}`;
}

export interface PrestoTaskSurfaceInput {
	mode: PrestoTaskSurfaceMode;
	tasks: Task[];
	filteredTasks: Task[];
	selectedTaskId: string | null;
	activeTagEditorTaskId?: string | null;
	activeScheduleEditorTaskId?: string | null;
	isLoading: boolean;
	pageError: string | null;
	interactionBusy: boolean;
	lastUpdatedAt: number | null;
	greeting: string;
	formattedDate: string;
	activeFilter: string;
	maxCardsPerSection: number;
	schemaVersion?: string;
	now?: number;
	/** Personal agents available for the task-creation agent picker. */
	personalAgents?: AgentPickerOption[];
	/** Pre-selected agent id for the create-form agent picker — the default/primary
	 *  personal agent. Falls back to the first option when unset. */
	selectedAgentId?: string;
	/** Whether the schedule section in the create form is expanded. */
	scheduleExpanded?: boolean;
	/** Current cron + timezone for the create-form schedule fields, bound so the
	 *  quick-preset buttons (which set page state) reflect into the visible inputs and
	 *  the timezone can default to the device timezone. */
	scheduleCron?: string;
	scheduleTimezone?: string;
	/** Options for the schedule timezone dropdown (device tz first, then common
	 *  zones). When empty the field is omitted from the select. */
	scheduleTimezoneOptions?: Array<{ value: string; label: string }>;
	/** Whether the compose (task creation) panel is expanded. */
	composeExpanded?: boolean;
	/** Available threads for the thread picker in the create form. */
	threadOptions?: Array<{ id: string; name: string }>;
	/** Pre-selected thread ID (locked on thread pages, selectable on /tasks). */
	selectedThreadId?: string;
	/** If true, the thread picker is disabled (used on thread pages). */
	threadLocked?: boolean;
}

interface SurfaceTaskRow {
	taskId: string;
	title: string;
	status: TaskStatus;
	planStatus?: Task['planStatus'];
	hasPlan?: boolean;
	updatedAt: string;
	dueDate: string;
	dueDateRaw: string;
	priority: string;
	tags: string[];
	description: string;
	sourceLabel: string;
	isSelected: boolean;
	agentName: string;
	scheduleCron: string;
	scheduleTimezone: string;
	scheduleRetentionMaxRecords: string;
	scheduleRetentionMaxDays: string;
	createdBy: string;
	completionSummary: string;
	readOnly: boolean;
	uiThreadId: string;
	executionId?: string;
	pendingQuestion?: { id: string; question: string };
	/** Muted one-liner under the meta line — only set for running-tone cards. */
	activityLine: string;
}

const PREVIEW_CARD_COUNT_DEFAULT = 3;
const PREVIEW_CARD_COUNT_MAX = 12;

const ROUTE_IDS = {
	header: 'presto-home-header',
	headerActions: 'presto-home-actions',
	metricsGrid: 'presto-tasks-metrics',
	sectionOverdue: 'presto-home-overdue',
	sectionToday: 'presto-home-today',
	sectionEmpty: 'presto-home-empty',
	taskCreateForm: 'presto-task-create-form',
	taskGrid: 'presto-spells-task-grid',
	taskCardPrefix: 'presto-task-card',
	taskMetaPrefix: 'presto-task-meta',
	taskActionsPrefix: 'presto-task-actions',
	taskSourcePrefix: 'presto-task-source'
};

/** Well-known cron presets for the schedule picker. */
export const SCHEDULE_PRESETS: Array<{ label: string; cron: string }> = [
	{ label: 'Every hour', cron: '0 * * * *' },
	{ label: 'Daily at 9 AM', cron: '0 9 * * *' },
	{ label: 'Weekly (Mon 9 AM)', cron: '0 9 * * 1' },
	{ label: 'Monthly (1st, 9 AM)', cron: '0 9 1 * *' }
];

function asString(value: unknown): string {
	if (typeof value === 'string') return value;
	if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
		return String(value);
	}
	return '';
}

function asRecord(value: unknown): Record<string, unknown> {
	return typeof value === 'object' && value != null && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function parseDueDate(value: string | undefined): Date | null {
	const raw = asString(value).trim();
	if (!raw) return null;
	const isoMatch = /^(\d{4})-(\d{2})-(\d{2})/.exec(raw);
	if (isoMatch) {
		const year = Number(isoMatch[1]);
		const month = Number(isoMatch[2]);
		const day = Number(isoMatch[3]);
		if (Number.isFinite(year) && Number.isFinite(month) && Number.isFinite(day)) {
			const parsed = new Date(year, month - 1, day);
			if (!Number.isNaN(parsed.getTime())) {
				return parsed;
			}
		}
	}
	const fallback = new Date(raw);
	if (Number.isNaN(fallback.getTime())) return null;
	return new Date(fallback.getFullYear(), fallback.getMonth(), fallback.getDate());
}

function dueDateDeltaDays(value: string | undefined): number | null {
	const parsed = parseDueDate(value);
	if (!parsed) return null;
	const today = new Date();
	const todayStart = new Date(today.getFullYear(), today.getMonth(), today.getDate());
	return Math.round((parsed.getTime() - todayStart.getTime()) / (24 * 60 * 60 * 1000));
}

function safeDateString(value: string | undefined): string {
	if (!value) return 'n/a';
	const parsedDate = parseDueDate(value);
	if (!parsedDate) {
		return 'invalid date';
	}

	const dayDiff = dueDateDeltaDays(value);
	if (dayDiff === null) return 'invalid date';

	if (dayDiff === 0) return 'Today';
	if (dayDiff === -1) return 'Yesterday';
	if (dayDiff === 1) return 'Tomorrow';
	if (dayDiff === -2) return '2 days ago';
	if (dayDiff === 2) return 'In 2 days';

	return parsedDate.toLocaleDateString('en-US', {
		weekday: 'short',
		month: 'short',
		day: 'numeric'
	});
}

function normalizeTaskStatus(value: string | undefined): TaskStatus {
	const allowed: ReadonlySet<string> = new Set([
		'pending',
		'planning',
		'running',
		'ready',
		'paused',
		'deferred',
		'completed',
		'failed',
		'cancelled'
	]);
	if (value && allowed.has(value)) {
		return value as TaskStatus;
	}
	return 'pending';
}

function looksLikeOpaqueTaskId(value: string): boolean {
	const trimmed = value.trim();
	if (!trimmed) return false;
	if (/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(trimmed)) {
		return true;
	}
	return /^[a-z0-9_-]{20,}$/i.test(trimmed);
}

function preferredTaskTitle(task: Task, fallbackIndex: number): string {
	const title = asString(task.title).trim();
	if (title && !looksLikeOpaqueTaskId(title)) {
		return title;
	}
	const description = asString(task.description).trim();
	if (description) {
		return description.length > 72 ? `${description.slice(0, 69)}...` : description;
	}
	return `Task ${fallbackIndex + 1}`;
}

function normalizeTaskId(task: Task, fallbackIndex: number): string {
	const id = asString(task.id).trim();
	if (id) return id;
	return `presto-task-${fallbackIndex + 1}`;
}

function statusLabel(status: TaskStatus): string {
	switch (status) {
		case 'pending':
			return 'Ready';
		case 'planning':
			return 'Planning';
		case 'ready':
			return 'Ready';
		case 'running':
			return 'Running';
		case 'paused':
			return 'Needs Attention';
		case 'completed':
			return 'Completed';
		case 'failed':
			return 'Failed';
		case 'cancelled':
			return 'Cancelled';
		default:
			return 'Unknown';
	}
}

function filterDisplayLabel(filter: string): string {
	if (filter.startsWith('tag:')) {
		const tag = filter.slice(4).trim();
		return tag ? `#${tag}` : 'Tagged';
	}
	switch (filter) {
		case 'inbox':
			return 'Inbox';
		case 'today':
			return 'Today';
		case 'overdue':
			return 'Overdue';
		case 'running':
			return 'Running';
		case 'completed':
			return 'Completed';
		default:
			return 'All Tasks';
	}
}

function statusBadgeLabel(task: SurfaceTaskRow): string {
	return `${statusLabel(task.status)}${task.isSelected ? ' · selected' : ''}`;
}

function formatRelativeTime(timestamp: number | null, now: number): string {
	if (!timestamp || !Number.isFinite(timestamp)) return 'never';
	const delta = now - timestamp;
	const abs = Math.abs(delta);
	const minutes = Math.round(abs / 1000 / 60);
	const hours = Math.round(abs / 1000 / 60 / 60);
	const days = Math.round(abs / 1000 / 60 / 60 / 24);
	if (minutes < 1) return 'just now';
	if (minutes < 60) return `${minutes}m ${delta >= 0 ? 'ago' : 'from now'}`;
	if (hours < 36) return `${hours}h ${delta >= 0 ? 'ago' : 'from now'}`;
	return `${days}d ${delta >= 0 ? 'ago' : 'from now'}`;
}

/** Compact elapsed duration ("3m" / "2h" / "4d") for the activity fallback. */
function formatElapsedDuration(timestamp: number | null, now: number): string {
	if (!timestamp || !Number.isFinite(timestamp)) return '';
	const elapsed = Math.max(0, now - timestamp);
	const minutes = Math.round(elapsed / 1000 / 60);
	if (minutes < 1) return 'under a minute';
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.round(minutes / 60);
	if (hours < 36) return `${hours}h`;
	return `${Math.round(hours / 24)}d`;
}

/**
 * Latest-activity one-liner for running-tone cards, from fields that already
 * flow on the Task record (zero new pollers/subscriptions): live step titles
 * when the store carries them, else the in-progress plan step (present when a
 * detail view enriched the task), else an elapsed-time line derived from
 * `updatedAt` — time since the last state write, which resets on any update
 * (NOT run start), hence the deliberately vague "Active/Planning Xm" copy.
 */
function taskActivityLine(task: Task, status: TaskStatus, now: number): string {
	if (statusTone(status).tone !== 'running') return '';
	const stepTitle =
		asString(task.currentSubstepTitle).trim() || asString(task.currentStepTitle).trim();
	if (stepTitle) return stepTitle;
	const inProgressStep = Array.isArray(task.planSteps)
		? task.planSteps.find((step) => step.status === 'in_progress')
		: undefined;
	const stepDescription = asString(inProgressStep?.description).trim();
	if (stepDescription) return stepDescription;
	const elapsed = formatElapsedDuration(Date.parse(task.updatedAt), now);
	if (!elapsed) return '';
	return status === 'planning' ? `Planning ${elapsed}` : `Active ${elapsed}`;
}

function clampCardCount(raw: unknown): number {
	const asNumber = typeof raw === 'number' && Number.isFinite(raw) ? raw : Number.parseInt(asString(raw), 10);
	if (!Number.isFinite(asNumber)) return PREVIEW_CARD_COUNT_DEFAULT;
	return Math.max(1, Math.min(PREVIEW_CARD_COUNT_MAX, Math.round(asNumber)));
}

function componentActionId(mode: PrestoTaskSurfaceMode, taskId: string, action: string): string {
	return `presto-${mode}:task:${taskId}:${action}`;
}

/** Human-friendly description of a cron expression (delegates to shared util). */
function describeCron(cron: string): string {
	return cronToHumanReadable(cron) || cron.trim() || 'none';
}

function asTaskRows(
	tasks: Task[],
	mode: PrestoTaskSurfaceMode,
	selectedTaskId: string | null,
	now: number
): SurfaceTaskRow[] {
	return tasks.map((task, index) => {
		const taskId = normalizeTaskId(task, index);
		const rawDueDate = asString(task.dueDate).trim();
		const status = normalizeTaskStatus(task.status);
		return {
			taskId,
			title: preferredTaskTitle(task, index),
			status,
			planStatus: task.planStatus,
			hasPlan: task.hasPlan === true,
			updatedAt: formatRelativeTime(Date.parse(task.updatedAt), now),
			dueDate: safeDateString(rawDueDate || undefined),
			dueDateRaw: rawDueDate,
			priority: asString(task.priority).trim().toUpperCase(),
			tags: Array.isArray(task.tags)
				? task.tags
						.map((tag) => asString(tag.name).trim())
						.filter(Boolean)
				: [],
			description: asString(task.description).trim() || 'No description',
			sourceLabel: task.source === 'task' ? 'manual' : 'execution',
			isSelected: taskId === selectedTaskId,
			agentName: asString(task.agentName).trim() || asString(task.agentId).trim() || '',
			scheduleCron: task.schedule?.cron || '',
			scheduleTimezone: task.schedule?.timezone || '',
			scheduleRetentionMaxRecords: task.schedule?.execution_history_retention?.max_records?.toString() || '',
			scheduleRetentionMaxDays: task.schedule?.execution_history_retention?.max_age_days?.toString() || '',
			createdBy: asString(task.createdBy).trim() || 'user',
			completionSummary:
				task.status === 'failed' || task.status === 'cancelled'
					? (
						asString(task.errorMessage).trim()
						|| asString(task.completionOutcome).trim()
						|| asString(task.completionSummary).trim()
					)
					: asString(task.completionSummary).trim(),
			readOnly: task.readOnly === true,
			uiThreadId: task.uiThreadId || '',
			executionId: task.executionId,
			pendingQuestion: task.pendingQuestion,
			activityLine: taskActivityLine(task, status, now)
		};
	});
}

function dueDateTagColor(task: SurfaceTaskRow): 'default' | 'success' | 'warning' | 'error' | 'info' {
	if (!task.dueDateRaw) return 'default';
	const delta = dueDateDeltaDays(task.dueDateRaw);
	if (delta === null) return 'default';
	if (delta < 0) return 'error';
	if (delta === 0) return 'warning';
	return 'success';
}

function dueDateTagClass(task: SurfaceTaskRow): string {
	const delta = dueDateDeltaDays(task.dueDateRaw);
	if (delta === null) return 'presto-task-due-date';
	if (delta < 0) return 'presto-task-due-date presto-task-due-date-past';
	if (delta === 0) return 'presto-task-due-date presto-task-due-date-today';
	return 'presto-task-due-date presto-task-due-date-future';
}

function priorityTagColor(priority: string): 'default' | 'success' | 'warning' | 'error' | 'info' {
	if (priority === 'P1') return 'error';
	if (priority === 'P2') return 'warning';
	if (priority === 'P3') return 'info';
	if (priority === 'P4') return 'default';
	return 'default';
}

function taskStatusDisplay(status: TaskStatus, task?: { pendingQuestion?: unknown }): string {
	if (status === 'ready') return 'Ready';
	if (status === 'paused') return task?.pendingQuestion ? 'Needs Input' : 'Paused';
	return statusLabel(status);
}

function buildTaskCard(
	task: SurfaceTaskRow,
	mode: PrestoTaskSurfaceMode,
	interactionBusy: boolean,
	activeTagEditorTaskId: string | null,
	activeScheduleEditorTaskId: string | null = null
): MuijComponent {
	const cardId = `${ROUTE_IDS.taskCardPrefix}-${mode}-${task.taskId}`;
	const useTaskRowLayout = mode === 'spells' || mode === 'home';
	const showDescription = !useTaskRowLayout && task.description !== 'No description';
	const completionDisabled = interactionBusy || task.status === 'running' || task.status === 'paused';

	const children: MuijComponent[] = [];
	const metadataBadges: MuijComponent[] = [
		{
			id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-updated`,
			component_type: 'Tag',
			props: {
				text: `Updated ${task.updatedAt}`,
				color: 'default'
			}
		}
	];
	if (task.dueDate !== 'n/a') {
		metadataBadges.splice(1, 0, {
			id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-due`,
			component_type: 'Tag',
			props: {
				text: task.dueDate,
				color: dueDateTagColor(task),
				className: dueDateTagClass(task)
			}
		});
	}
	if (task.priority) {
		metadataBadges.splice(1, 0, {
			id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-priority`,
			component_type: 'Tag',
			props: {
				text: task.priority,
				color: priorityTagColor(task.priority)
			}
		});
	}
	// Status chip = Badge in `status` mode: statusBadgeTone is THE single
	// status→tone mapping (neutral statuses → null → default gray badge).
	metadataBadges.unshift({
		id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-status`,
		component_type: 'Badge',
		props: {
			text: useTaskRowLayout ? taskStatusDisplay(task.status, task) : statusBadgeLabel(task),
			status: statusBadgeTone(task.status)
		}
	});
	if (!useTaskRowLayout) {
		metadataBadges.push({
			id: `${ROUTE_IDS.taskSourcePrefix}-${mode}-${task.taskId}`,
			component_type: 'Tag',
			props: {
				text: task.sourceLabel,
				color: task.sourceLabel === 'manual' ? 'success' : 'info'
			}
		});
	}

	if (useTaskRowLayout) {
		// Rest state = the calm row: completion checkbox, an ~8px status dot
		// (statusTone tone class → `--status-*` color var; the tooltip carries
		// the status label), the 2-line-clamped title, and ONE contextual
		// primary action. Everything else lives in the hover/focus-within
		// reveal row below plus the page-level ⋯ overflow menu (`menu` action).
		const actionButtons = buildTaskActionButtons(task, mode, interactionBusy);
		const primaryAction =
			actionButtons.find((button) => button.id.endsWith(':doit')) ?? actionButtons[0] ?? null;
		const dotTone = statusTone(task.status).tone;
		const restChildren: MuijComponent[] = [
			{
				id: componentActionId(mode, task.taskId, 'complete'),
				component_type: 'Checkbox',
				label: '',
				props: {
					checked: task.status === 'completed',
					disabled: completionDisabled
				}
			},
			{
				id: `${cardId}-status-dot`,
				component_type: 'Text',
				props: {
					children: '',
					variant: 'caption',
					// Running-tone dots breathe (reduced-motion-guarded `live-pulse`
					// keyframe in app.css).
					className: `presto-task-status-dot presto-task-status-dot--${dotTone}${
						dotTone === 'running' ? ' presto-task-status-dot--pulse' : ''
					}`,
					title: taskStatusDisplay(task.status, task)
				}
			},
			{
				id: `${cardId}-title`,
				component_type: 'Text',
				props: {
					children: task.title,
					className: 'presto-task-title',
					title: task.title
				}
			}
		];
		if (primaryAction) {
			restChildren.push({
				...primaryAction,
				props: { ...primaryAction.props, className: 'presto-task-primary-action' }
			});
		}
		children.push({
			id: `${cardId}-rest-row`,
			component_type: 'Stack',
			props: {
				className: 'presto-task-rest-row',
				direction: 'row',
				align: 'flex-start',
				gap: '0.5rem'
			},
			children: restChildren
		});

		// Meta line: agent · #thread · relative time as one muted text line
		// (context, not state — the chips it replaces were visual noise).
		const metaParts = [
			task.agentName,
			task.uiThreadId ? `#${task.uiThreadId}` : '',
			task.updatedAt !== 'never' ? `updated ${task.updatedAt}` : ''
		].filter(Boolean);
		if (metaParts.length > 0) {
			children.push({
				id: `${cardId}-meta-line`,
				component_type: 'Text',
				props: {
					children: metaParts.join(' · '),
					variant: 'caption',
					className: 'presto-task-meta-line',
					title: metaParts.join(' · ')
				}
			});
		}

		// Latest-activity one-liner: running/planning cards only, sourced from
		// already-flowing Task fields (step titles / in-progress plan step /
		// elapsed-time fallback — see taskActivityLine).
		if (task.activityLine) {
			children.push({
				id: `${cardId}-activity-line`,
				component_type: 'Text',
				props: {
					children: task.activityLine,
					variant: 'caption',
					className: 'presto-task-activity-line',
					title: task.activityLine
				}
			});
		}

		// Result preview (shown whenever a completion summary exists, even after unchecking)
		if (task.completionSummary && !['pending', 'running', 'planning'].includes(task.status)) {
			children.push({
				id: `${cardId}-result-preview`,
				component_type: 'Text',
				props: {
					children: task.completionSummary,
					className: 'presto-task-result-preview',
					title: task.completionSummary
				}
			});
		}

		// Reveal-row content (hover/focus-within): state chips (due date /
		// priority), user tags + tag editor, schedule chip/editor, secondary
		// actions, and the ⋯ overflow trigger for the page-level TaskCardMenu.
		const metaChildren: MuijComponent[] = [];
		// Due Date chip
		if (task.dueDate !== 'n/a') {
			metaChildren.push({
				id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-due-inline`,
				component_type: 'Tag',
				props: {
					text: task.dueDate,
					color: dueDateTagColor(task),
					className: dueDateTagClass(task)
				}
			});
		}
		// 3. Priority
		if (task.priority) {
			metaChildren.push({
				id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-priority-inline`,
				component_type: 'Tag',
				props: {
					text: task.priority,
					color: priorityTagColor(task.priority)
				}
			});
		}
		// 4. User Tags (up to 4, dismissible)
		for (let index = 0; index < Math.min(task.tags.length, 4); index += 1) {
			const tagName = task.tags[index];
			metaChildren.push({
				id: componentActionId(mode, task.taskId, `tag_remove_${sanitizeIdSegment(tagName)}`),
				component_type: 'Tag',
				props: {
					interactive: true,
					text: tagName,
					color: 'success',
					className: 'presto-task-user-tag-chip',
					dismissible: true
				}
			});
		}
		// 5. +Tag button / inline form
		if (mode === 'spells' || mode === 'home') {
			if (activeTagEditorTaskId === task.taskId) {
				metaChildren.push({
					id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-tag-add-inline`,
					component_type: 'Stack',
					props: {
						className: 'presto-task-inline-tag-editor',
						direction: 'row',
						align: 'center',
						gap: '0.2rem'
					},
					children: [
						{
							id: componentActionId(mode, task.taskId, 'tag_add_form'),
							component_type: 'Form',
							props: {
								title: '',
								showSubmit: true,
								submitLabel: 'Add',
								disabled: interactionBusy,
								idBase: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-tag-inline-form`,
								fields: [
									{
										id: 'tag_name',
										label: '',
										type: 'text',
										required: true,
										value: '',
										placeholder: 'Tag',
										rows: 1
									}
								]
							}
						}
					]
				});
			} else {
				metaChildren.push({
					id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-tag-add`,
					component_type: 'Button',
					label: '+Tag',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: 'presto-task-tag-add-btn',
						disabled: interactionBusy
					}
				});
			}
		}
		// Secondary action buttons (doit_direct, reset, …) — the primary
		// action lives on the rest row above.
		for (const button of actionButtons) {
			if (button === primaryAction) continue;
			metaChildren.push(button);
		}
		if (mode === 'spells' || mode === 'home') {
			const toolsRowChildren: MuijComponent[] = [...metaChildren];

			// Schedule Cron — clickable chip or inline editor
			if (activeScheduleEditorTaskId === task.taskId) {
				toolsRowChildren.push({
					id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-schedule-edit-inline`,
					component_type: 'Stack',
					props: {
						className: 'presto-task-inline-schedule-editor',
						direction: 'row',
						align: 'center',
						gap: '0.25rem'
					},
					children: [
						{
							id: componentActionId(mode, task.taskId, 'schedule_edit_form'),
							component_type: 'Form',
							props: {
								title: '',
								showSubmit: true,
								submitLabel: 'Set',
								disabled: interactionBusy,
								className: 'presto-task-inline-schedule-form',
								idBase: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}-schedule-edit-form`,
								fields: [
									{
										id: 'schedule_cron',
										label: '',
										type: 'text',
										required: false,
										value: task.scheduleCron || '',
										placeholder: '0 9 * * *',
										maxLength: 50
									},
									{
										id: 'schedule_timezone',
										label: '',
										type: 'text',
										required: false,
										value: '',
										placeholder: 'UTC',
										maxLength: 64
									},
									{
										id: 'schedule_retention_max_records',
										label: '',
										type: 'text',
										required: false,
										value: task.scheduleRetentionMaxRecords || '',
										placeholder: 'Max runs (e.g. 50)',
										maxLength: 6
									},
									{
										id: 'schedule_retention_max_days',
										label: '',
										type: 'text',
										required: false,
										value: task.scheduleRetentionMaxDays || '',
										placeholder: 'Max days (e.g. 3)',
										maxLength: 6
									}
								]
							}
						}
					]
				});
			} else if (task.scheduleCron) {
				toolsRowChildren.push({
					id: componentActionId(mode, task.taskId, 'schedule_edit'),
					component_type: 'Button',
					label: describeCron(task.scheduleCron),
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: 'presto-task-schedule-chip presto-task-schedule-chip-btn',
						title: `${describeCron(task.scheduleCron)} (${task.scheduleCron})`,
						disabled: interactionBusy
					}
				});
			} else {
				toolsRowChildren.push({
					id: componentActionId(mode, task.taskId, 'schedule_edit'),
					component_type: 'Button',
					label: '+Sched',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: 'presto-task-schedule-add-btn',
						disabled: interactionBusy
					}
				});
			}

			// ⋯ overflow trigger: opens the page-level TaskCardMenu (home of
			// all old 📅/⚑/⋮ popup actions). The task-id class token lets the
			// page locate this button as the menu anchor.
			if (!task.readOnly) {
				toolsRowChildren.push({
					id: componentActionId(mode, task.taskId, 'menu'),
					component_type: 'Button',
					label: '',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						icon: 'dots-horizontal',
						title: 'More actions',
						className: `presto-task-menu-trigger ${taskMenuTriggerClass(task.taskId)}`,
						disabled: interactionBusy
					}
				});
			}

			if (toolsRowChildren.length > 0) {
				children.push({
					id: `${cardId}-reveal-row`,
					component_type: 'Stack',
					props: {
						className: 'presto-task-reveal-row',
						direction: 'row',
						align: 'center',
						justify: 'flex-start',
						gap: '0.35rem',
						wrap: true
					},
					children: toolsRowChildren
				});
			}
		}
	} else {
		children.push({
			id: `${ROUTE_IDS.taskMetaPrefix}-${mode}-${task.taskId}`,
			component_type: 'Stack',
			props: {
				direction: 'row',
				gap: '0.45rem',
				wrap: true
			},
			children: metadataBadges
		});

		if (showDescription) {
			children.push({
				id: `${cardId}-description`,
				component_type: 'Text',
				props: {
					children: task.description
				}
			});
		}

		children.push({
			id: `${ROUTE_IDS.taskActionsPrefix}-${mode}-${task.taskId}`,
			component_type: 'Stack',
			props: {
				direction: 'row',
				gap: '0.5rem',
				wrap: true
			},
			children: buildTaskActionButtons(task, mode, interactionBusy)
		});
	}

	return {
		id: cardId,
		component_type: 'Card',
		label: useTaskRowLayout ? '' : `presto-task-${task.taskId}`,
		props: useTaskRowLayout
			? {
				interactive: true,
				className: 'presto-task-row-card'
			}
			: {
				title: task.title,
				subtitle: statusLabel(task.status),
				body: task.description,
				tooltip: task.title
			},
		children
	};
}

function buildTaskActionButtons(task: SurfaceTaskRow, mode: PrestoTaskSurfaceMode, interactionBusy: boolean): MuijComponent[] {
	if (mode === 'spells' || mode === 'home') {
		if (task.planStatus === 'planning') {
			return [
				{
					id: componentActionId(mode, task.taskId, 'open'),
					component_type: 'Button',
					label: 'View Plan',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				}
			];
		}
		if (task.planStatus === 'eliciting') {
			return [
				{
					id: componentActionId(mode, task.taskId, 'open'),
					component_type: 'Button',
					label: task.pendingQuestion ? 'Answer Question' : 'View Plan',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				}
			];
		}
		if (task.planStatus === 'draft') {
			return [
				{
					id: componentActionId(mode, task.taskId, 'open'),
					component_type: 'Button',
					label: 'Review Plan',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				}
			];
		}
		if (task.planStatus === 'approved' && task.status === 'ready') {
			return [
				{
					id: componentActionId(mode, task.taskId, 'doit'),
					component_type: 'Button',
					label: 'Run Plan',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				}
			];
		}
		if (task.status === 'pending' || task.status === 'ready') {
			const buttons: MuijComponent[] = [];
			buttons.push({
				id: componentActionId(mode, task.taskId, 'doit'),
				component_type: 'Button',
				label: task.status === 'pending' ? 'PrePlan' : 'Run Now',
				props: {
					interactive: true,
					variant: 'outline',
					size: 'sm',
					disabled: interactionBusy
				}
			});
			if (task.status === 'pending') {
				buttons.push({
					id: componentActionId(mode, task.taskId, 'doit_direct'),
					component_type: 'Button',
					label: 'Run Now',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				});
			}
			return buttons;
		}
		if (task.status === 'paused') {
			const pauseButtons: MuijComponent[] = [
				{
					id: componentActionId(mode, task.taskId, 'open'),
					component_type: 'Button',
					label: task.pendingQuestion ? 'View Question' : 'View Execution',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				},
				{
					id: componentActionId(mode, task.taskId, 'reset'),
					component_type: 'Button',
					label: 'Reset to Ready',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				}
			];
			return pauseButtons;
		}
		if (task.status === 'running' || task.status === 'planning') {
			return [
				{
					id: componentActionId(mode, task.taskId, 'abort'),
					component_type: 'Button',
					label: 'Stop',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						disabled: interactionBusy
					}
				}
			];
		}
		if (task.status === 'failed' || task.status === 'cancelled') {
			if (task.executionId) {
				return [
					{
						id: componentActionId(mode, task.taskId, 'reset'),
						component_type: 'Button',
						label: 'Reset to Ready',
						props: {
							interactive: true,
							variant: 'outline',
							size: 'sm',
							disabled: interactionBusy
						}
					}
				];
			}
			if (task.planStatus) {
				return [
					{
						id: componentActionId(mode, task.taskId, 'open'),
						component_type: 'Button',
						label: 'Review Plan',
						props: {
							interactive: true,
							variant: 'outline',
							size: 'sm',
							disabled: interactionBusy
						}
					}
				];
			}
			return [];
		}
		return [];
	}

	const openLabel = task.status === 'paused'
		? (task.pendingQuestion ? 'View Question' : 'View Execution')
		: 'Open';
	const buttons: MuijComponent[] = [
		{
			id: componentActionId(mode, task.taskId, 'open'),
			component_type: 'Button',
			label: openLabel,
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				disabled: interactionBusy
			}
		}
	];

	if (task.status === 'pending' || task.status === 'ready') {
		buttons.push({
			id: componentActionId(mode, task.taskId, 'doit'),
			component_type: 'Button',
			label: task.status === 'pending' ? 'PrePlan' : 'PrePlan and Run',
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				disabled: interactionBusy
			}
		});
		buttons.push({
			id: componentActionId(mode, task.taskId, 'doit_direct'),
			component_type: 'Button',
			label: 'Run Now',
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				disabled: interactionBusy
			}
		});
	}

	if (!task.readOnly && (task.status === 'running' || task.status === 'paused' || task.status === 'planning')) {
		buttons.push({
			id: componentActionId(mode, task.taskId, 'abort'),
			component_type: 'Button',
			label: 'Stop',
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				disabled: interactionBusy
			}
		});
	}

	if (!task.readOnly && task.executionId && (task.status === 'failed' || task.status === 'cancelled')) {
		buttons.push({
			id: componentActionId(mode, task.taskId, 'reset'),
			component_type: 'Button',
			label: 'Reset to Ready',
			props: {
				interactive: true,
				variant: 'secondary',
				size: 'sm',
				disabled: interactionBusy
			}
		});
	}

	if (task.sourceLabel === 'manual' && !task.readOnly) {
		buttons.push({
			id: componentActionId(mode, task.taskId, 'delete'),
			component_type: 'Button',
			label: 'Delete',
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				disabled: interactionBusy
			}
		});
	}

	return buttons;
}

function buildMetricCards(tasks: Task[]): MuijComponent[] {
	const running = tasks.filter((task) => task.status === 'running' || task.status === 'paused').length;
	const ready = tasks.filter((task) => task.status === 'ready').length;
	const pending = tasks.filter((task) => task.status === 'pending').length;
	const completed = tasks.filter((task) => task.status === 'completed').length;
	const failed = tasks.filter((task) => task.status === 'failed' || task.status === 'cancelled').length;

	return [
		{
			id: 'presto-metric-all',
			component_type: 'MetricCard',
			label: 'Total',
			props: { value: String(tasks.length), label: 'tasks' }
		},
		{
			id: 'presto-metric-ready',
			component_type: 'MetricCard',
			label: 'Ready',
			props: { value: String(ready), label: 'awaiting execution' }
		},
		{
			id: 'presto-metric-running',
			component_type: 'MetricCard',
			label: 'Running',
			props: { value: String(running), label: 'currently running' }
		},
		{
			id: 'presto-metric-pending',
			component_type: 'MetricCard',
			label: 'Ready',
			props: { value: String(pending), label: 'awaiting planning' }
		},
		{
			id: 'presto-metric-completed',
			component_type: 'MetricCard',
			label: 'Completed',
			props: { value: String(completed), label: 'finished' }
		},
		{
			id: 'presto-metric-failed',
			component_type: 'MetricCard',
			label: 'Failed',
			props: { value: String(failed), label: 'need review' }
		}
	];
}

function normalizeDueDateValue(raw: string | undefined): string | null {
	const value = asString(raw).trim();
	if (!value) return null;
	const isoDateMatch = /(\d{4}-\d{2}-\d{2})/.exec(value);
	if (isoDateMatch && isoDateMatch[1]) {
		return isoDateMatch[1];
	}
	const mdYMatch = /^(\d{1,2})[/-](\d{1,2})[/-](\d{4})$/.exec(value);
	if (mdYMatch) {
		const month = String(Number(mdYMatch[1])).padStart(2, '0');
		const day = String(Number(mdYMatch[2])).padStart(2, '0');
		const year = mdYMatch[3];
		return `${year}-${month}-${day}`;
	}
	const parsed = new Date(value);
	if (Number.isNaN(parsed.getTime())) return null;
	const year = parsed.getFullYear();
	const month = String(parsed.getMonth() + 1).padStart(2, '0');
	const day = String(parsed.getDate()).padStart(2, '0');
	return `${year}-${month}-${day}`;
}

function splitUrgentTasks(tasks: Task[]): { overdue: Task[]; today: Task[] } {
	const todayDate = new Date();
	todayDate.setHours(0, 0, 0, 0);
	const year = todayDate.getFullYear();
	const month = String(todayDate.getMonth() + 1).padStart(2, '0');
	const day = String(todayDate.getDate()).padStart(2, '0');
	const todayLocalStr = `${year}-${month}-${day}`;
	const todayUtcStr = todayDate.toISOString().split('T')[0];

	const overdue = tasks.filter((task) => {
		if (task.status === 'completed' || task.status === 'cancelled') return false;
		const dueDate = normalizeDueDateValue(task.dueDate);
		return !!dueDate && dueDate < todayLocalStr;
	});

	const today = tasks.filter((task) => {
		if (task.status === 'completed' || task.status === 'cancelled') return false;
		const dueDate = normalizeDueDateValue(task.dueDate);
		return dueDate === todayLocalStr || dueDate === todayUtcStr;
	});

	return {
		overdue,
		today
	};
}

function buildHomeComponents(input: PrestoTaskSurfaceInput, safeTasks: Task[], now: number): MuijComponent[] {
	const maxCardsPerSection = clampCardCount(input.maxCardsPerSection);
	const activeTagEditorTaskId = asString(input.activeTagEditorTaskId).trim() || null;
	const activeScheduleEditorTaskId = asString(input.activeScheduleEditorTaskId).trim() || null;
	const { overdue, today } = splitUrgentTasks(safeTasks);
	const overdueRows = asTaskRows(overdue, 'home', input.selectedTaskId, now).slice(0, maxCardsPerSection);
	const todayRows = asTaskRows(today, 'home', input.selectedTaskId, now).slice(0, maxCardsPerSection);
	const overdueCards = overdueRows.map((row) =>
		buildTaskCard(row, 'home', input.interactionBusy, activeTagEditorTaskId, activeScheduleEditorTaskId)
	);
	const todayCards = todayRows.map((row) =>
		buildTaskCard(row, 'home', input.interactionBusy, activeTagEditorTaskId, activeScheduleEditorTaskId)
	);
	const urgentChildren: MuijComponent[] = [];
	if (overdueCards.length > 0) {
		urgentChildren.push({
			id: ROUTE_IDS.sectionOverdue,
			component_type: 'Card',
			label: '',
			props: {
				title: `Overdue (${overdue.length})`,
				className: 'presto-home-urgent-subsection'
			},
			children: overdueCards
		});
	}
	if (todayCards.length > 0) {
		urgentChildren.push({
			id: ROUTE_IDS.sectionToday,
			component_type: 'Card',
			label: '',
			props: {
				title: `Due for Today (${today.length})`,
				className: 'presto-home-urgent-subsection'
			},
			children: todayCards
		});
	}
	if (urgentChildren.length === 0 && input.isLoading) {
		urgentChildren.push({
			id: 'presto-home-urgent-loading',
			component_type: 'Spinner',
			label: 'Loading tasks...',
			props: { size: 'md', centered: true }
		});
	} else if (urgentChildren.length === 0) {
		urgentChildren.push({
			id: 'presto-home-urgent-empty',
			component_type: 'EmptyState',
			label: '',
			props: {
				icon: '✦',
				title: 'Your task queue is clear',
				description: "Create a task to get started. We'll help you plan and run it.",
				actionLabel: '+  Create Task',
				className: 'presto-queue-empty-state'
			}
		});
	}

	const components: MuijComponent[] = [
		{
			id: ROUTE_IDS.header,
			component_type: 'Card',
			label: 'presto-home-header',
			props: {
				title: input.greeting,
				subtitle: input.formattedDate,
				body: 'Prioritize urgent tasks, then continue execution.'
			},
			children: [
				{
					id: ROUTE_IDS.headerActions,
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.5rem',
						wrap: true
					},
					children: [
						{
							id: 'presto-home-action-scroll',
							component_type: 'Button',
							label: 'Daily briefing',
							props: { interactive: true, variant: 'secondary', size: 'sm', disabled: input.interactionBusy }
						},
						{
							id: 'presto-home-action-spells',
							component_type: 'Button',
							label: 'All tasks',
							props: { interactive: true, variant: 'primary', size: 'sm', disabled: input.interactionBusy }
						},
						{
							id: 'presto-home-action-today',
							component_type: 'Button',
							label: '+ more today',
							props: {
								interactive: true,
								variant: 'outline',
								size: 'sm',
								disabled: today.length <= maxCardsPerSection || input.interactionBusy
							}
						},
						{
							id: 'presto-home-action-overdue',
							component_type: 'Button',
							label: '+ more overdue',
							props: {
								interactive: true,
								variant: 'outline',
								size: 'sm',
								disabled: overdue.length <= maxCardsPerSection || input.interactionBusy
							}
						}
					]
				},
				{
					id: 'presto-home-stream',
					component_type: 'DataList',
					props: {
						items: [
							{
								id: 'presto-home-stream-loading',
								key: 'Task stream',
								value: input.isLoading ? 'updating...' : 'online'
							},
							{
								id: 'presto-home-stream-refresh',
								key: 'Last refresh',
								value: formatRelativeTime(input.lastUpdatedAt ?? 0, now)
							}
						]
					}
				}
			]
		},
		{
			id: 'presto-home-urgent-container',
			component_type: 'Card',
			label: '',
			props: {
				title: '✦ Needs Approval',
				className: 'presto-home-urgent-container'
			},
			children: urgentChildren
		}
	];

	if (input.pageError) {
		components.unshift({
			id: 'presto-home-error',
			component_type: 'Alert',
			label: 'Task surface warning',
			props: {
				type: 'error',
				message: input.pageError,
				closable: true
			}
		});
	}

	return components;
}

/**
 * Build the agent picker select field and schedule picker for the task creation form.
 */
function buildCreateFormAgentAndScheduleFields(
	input: PrestoTaskSurfaceInput
): MuijComponent[] {
	const personalAgents = input.personalAgents ?? [];
	const scheduleExpanded = input.scheduleExpanded === true;
	const isBusy = input.interactionBusy;

	const children: MuijComponent[] = [];

	// -- Agent picker dropdown (Personal agents only) --
	const agentOptions = personalAgents.map((agent) => ({
		value: agent.agent_id,
		label: agent.name || agent.agent_id
	}));

	children.push({
		id: 'presto-spells-agent-picker',
		component_type: 'Form',
		label: '',
		props: {
			title: '',
			showSubmit: false,
			disabled: isBusy,
			idBase: 'presto-spells-agent-picker',
			fields: [
				{
					id: 'agent_id',
					label: 'Assign crew member',
					type: 'select',
					required: true,
					// Default to the page-selected agent (the primary/default personal
					// agent), falling back to the first option.
					value:
						(input.selectedAgentId &&
						agentOptions.some((option) => option.value === input.selectedAgentId)
							? input.selectedAgentId
							: agentOptions.length > 0
								? agentOptions[0].value
								: '') as string,
					placeholder: personalAgents.length > 0 ? 'Choose a crew member...' : 'No crew members available',
					options: agentOptions
				}
			]
		}
	});

	// -- Schedule picker (collapsible) --
	if (!scheduleExpanded) {
		children.push({
			id: 'presto-spells-schedule-toggle',
			component_type: 'Button',
			label: '+ Add Schedule',
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				className: 'presto-spells-schedule-toggle-btn',
				disabled: isBusy
			}
		});
	} else {
		// Preset buttons
		const presetButtons: MuijComponent[] = SCHEDULE_PRESETS.map((preset, index) => ({
			id: `presto-spells-schedule-preset-${index}`,
			component_type: 'Button',
			label: preset.label,
			props: {
				interactive: true,
				variant: 'outline',
				size: 'sm',
				className: 'presto-spells-schedule-preset-btn',
				disabled: isBusy,
				'data-cron': preset.cron
			}
		}));

		children.push({
			id: 'presto-spells-schedule-section',
			component_type: 'Card',
			label: '',
			props: {
				title: 'Schedule (optional)',
				className: 'presto-spells-schedule-card'
			},
			children: [
				{
					id: 'presto-spells-schedule-presets',
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.4rem',
						wrap: true,
						className: 'presto-spells-schedule-presets-row'
					},
					children: presetButtons
				},
				{
					id: 'presto-spells-schedule-custom-form',
					component_type: 'Form',
					label: '',
					props: {
						title: '',
						showSubmit: false,
						disabled: isBusy,
						idBase: 'presto-spells-schedule-custom',
						fields: [
							{
								id: 'schedule_cron',
								label: 'Custom cron',
								type: 'text',
								required: false,
								// Bound to page state so the quick-preset buttons fill it.
								value: asString(input.scheduleCron),
								placeholder: '0 9 * * *',
								rows: 1,
								maxLength: 50
							},
							{
								id: 'schedule_timezone',
								label: 'Timezone',
								type: 'select',
								required: false,
								// Bound to page state — defaults to the device timezone (which is
								// the first option, so it shows selected).
								value: asString(input.scheduleTimezone),
								placeholder: 'Select timezone',
								options: input.scheduleTimezoneOptions ?? []
							}
						]
					}
				},
				{
					id: 'presto-spells-schedule-collapse',
					component_type: 'Button',
					label: 'Remove Schedule',
					props: {
						interactive: true,
						variant: 'outline',
						size: 'sm',
						className: 'presto-spells-schedule-collapse-btn',
						disabled: isBusy
					}
				}
			]
		});
	}

	return children;
}

function buildSpellsComponents(
	input: PrestoTaskSurfaceInput,
	safeFilteredTasks: Task[],
	now: number
): MuijComponent[] {
	const activeFilter = asString(input.activeFilter).trim() || 'all';
	const activeTagEditorTaskId = asString(input.activeTagEditorTaskId).trim() || null;
	const activeScheduleEditorTaskId = asString(input.activeScheduleEditorTaskId).trim() || null;
	const taskCards = asTaskRows(safeFilteredTasks, 'spells', input.selectedTaskId, now).map((row) =>
		buildTaskCard(row, 'spells', input.interactionBusy, activeTagEditorTaskId, activeScheduleEditorTaskId)
	);

	// Build the inline compose row children: form + agent/schedule
	const composeRowChildren: MuijComponent[] = [
		{
			id: ROUTE_IDS.taskCreateForm,
			component_type: 'Form',
			label: '',
			props: {
				title: '',
				showSubmit: false, // Using custom buttons in children
				submitLabel: 'Create Task',
				disabled: input.interactionBusy,
				idBase: ROUTE_IDS.taskCreateForm,
				fields: [
					{
						id: 'task_title',
						label: '',
						type: 'text',
						required: true,
						value: '',
						placeholder: 'Title — e.g. "Book Tokyo flights"',
						rows: 1
					},
					// NOTE: the task description is NOT a form field here — it's rendered as
					// the native @-mention `MentionTextarea` in tasks/+page.svelte (so it can
					// reference completed tasks as chips). Keeping a textarea here too would
					// duplicate the description input.
					{
						id: 'task_output_mode',
						label: 'Run output mode',
						type: 'select',
						required: false,
						value: 'accumulate',
						options: [
							{
								value: 'accumulate',
								label: 'Accumulate outputs'
							},
							{
								value: 'overwrite',
								label: 'Overwrite latest outputs'
							}
						]
					},
					...((input.threadOptions && input.threadOptions.length > 0) ? [{
						id: 'task_thread',
						label: 'Thread',
						type: 'select' as const,
						required: false,
						value: input.selectedThreadId || 'general',
						disabled: input.threadLocked === true,
						options: input.threadOptions.map((t: { id: string; name: string }) => ({ value: t.id, label: `#${t.name}` }))
					}] : [])
				]
			},
			children: [
				{
					id: 'presto-spells-inline-actions',
					component_type: 'Stack',
					props: {
						direction: 'row',
						justify: 'flex-end',
						align: 'center',
						gap: '0.5rem'
					},
					children: [
						{
							id: 'presto-spells-inline-cancel',
							component_type: 'Button',
							label: 'Clear',
							props: {
								interactive: true,
								variant: 'outline',
								size: 'sm',
								className: 'presto-inline-compose-cancel',
								disabled: input.interactionBusy
							}
						},
						{
							id: 'presto-spells-inline-submit',
							component_type: 'Button',
							label: 'Create Task',
							props: {
								interactive: true,
								variant: 'primary',
								size: 'sm',
								type: 'submit',
								disabled: input.interactionBusy
							}
						}
					]
				}
			]
		}
	];

	// Agent picker + schedule picker below the compose row
	const agentScheduleComponents = buildCreateFormAgentAndScheduleFields(input);

	const composeExpanded = input.composeExpanded !== false; // default expanded for backwards compat

	// Build compose panel children: either full form or collapsed toggle
	const composePanelChildren: MuijComponent[] = composeExpanded
		? [
				{
					id: 'presto-spells-inline-create',
					component_type: 'Stack',
					label: '',
					props: {
						className: 'presto-spells-inline-compose',
						direction: 'row',
						align: 'center',
						gap: '0.55rem'
					},
					children: composeRowChildren
				},
				// Agent picker and schedule picker rendered below the compose row
				{
					id: 'presto-spells-agent-schedule-section',
					component_type: 'Stack',
					label: '',
					props: {
						className: 'presto-spells-agent-schedule-section',
						direction: 'column',
						gap: '0.45rem'
					},
					children: agentScheduleComponents
				}
			]
		: [];

	// No summary/status card here anymore: the per-status counts live in the
	// host page's TaskFilterToolbar (its display-only status ledger), so the
	// surface starts straight at the compose panel.
	const components: MuijComponent[] = [
		{
			id: 'presto-spells-compose-panel',
			component_type: 'Card',
			label: '',
			props: {
				title: '',
				className: `presto-spells-compose-card${composeExpanded ? '' : ' presto-spells-compose-collapsed'}`
			},
			children: [
				// Toggle button — always visible
				{
					id: 'presto-spells-compose-toggle',
					component_type: 'Button',
					label: composeExpanded ? 'Hide' : '+ New Task',
					props: {
						interactive: true,
						variant: composeExpanded ? 'ghost' : 'primary',
						size: 'sm',
						className: 'presto-spells-compose-toggle-btn'
					}
				},
				...composePanelChildren
			]
		},
		{
			id: ROUTE_IDS.taskGrid,
			component_type: 'Grid',
			label: 'presto-spells-grid',
			props: {
				columns: 1,
				autoFit: false,
				gap: '0.65rem'
			},
			children: taskCards
		}
	];

	if (safeFilteredTasks.length === 0 && input.isLoading) {
		// Skeleton card rows sketch the incoming task list instead of a lone
		// centered spinner — same loading treatment as ExecutionPanel's card
		// list (generative Skeleton, hosted as a MUIJ component).
		components.push({
			id: 'presto-spells-loading',
			component_type: 'Stack',
			label: 'Loading tasks',
			props: {
				direction: 'column',
				gap: '0.65rem',
				className: 'presto-spells-loading-skeletons'
			},
			children: [1, 2, 3, 4].map((row) => ({
				id: `presto-spells-loading-skeleton-${row}`,
				component_type: 'Skeleton',
				props: { variant: 'rect', height: '72px' }
			}))
		});
	} else if (safeFilteredTasks.length === 0) {
		// Filter-aware empty state: name what's absent for scoped views instead
		// of the universal "queue is clear" line. The Create-Task CTA only shows
		// where creating is the sensible next step (All/Inbox/Today) — it would
		// be a non-sequitur on Completed/Overdue/Running/tag views.
		const canCreateHere = activeFilter === 'all' || activeFilter === 'inbox' || activeFilter === 'today';
		let emptyTitle = 'Your task queue is clear';
		let emptyDescription = "Create a task to get started. We'll help you plan and run it.";
		if (activeFilter === 'completed') {
			emptyTitle = 'No completed tasks yet.';
			emptyDescription = 'Tasks land here once they finish.';
		} else if (activeFilter === 'running') {
			emptyTitle = 'Nothing is running right now.';
			emptyDescription = '';
		} else if (activeFilter === 'overdue') {
			emptyTitle = 'Nothing is overdue.';
			emptyDescription = '';
		} else if (activeFilter.startsWith('tag:')) {
			emptyTitle = `Nothing matches ${filterDisplayLabel(activeFilter)}.`;
			emptyDescription = '';
		}
		components.push({
			id: 'presto-spells-empty',
			component_type: 'EmptyState',
			label: '',
			props: {
				icon: '✦',
				title: emptyTitle,
				description: emptyDescription,
				actionLabel: canCreateHere ? '+  Create Task' : '',
				className: 'presto-queue-empty-state'
			}
		});
	}

	if (input.pageError) {
		components.unshift({
			id: 'presto-spells-error',
			component_type: 'Alert',
			label: 'Task surface warning',
			props: {
				type: 'error',
				message: input.pageError,
				closable: true
			}
		});
	}

	return components;
}

export function buildSpellsSurface(input: PrestoTaskSurfaceInput): MuijComponent[] {
	const safeNow = input.now ?? Date.now();
	const safeTasks = Array.isArray(input.tasks) ? input.tasks : [];
	const safeFilteredTasks = Array.isArray(input.filteredTasks) ? input.filteredTasks : [];
	const safeMode = input.mode === 'home' || input.mode === 'spells' ? input.mode : 'home';

	if (safeMode === 'home') {
		return buildHomeComponents(input, safeTasks, safeNow);
	}

	return buildSpellsComponents(input, safeFilteredTasks, safeNow);
}

export function parseTaskAction(value: unknown): ParsedTaskAction | null {
	const detail = asRecord(value);
	const target = asString(detail.componentId);
	const routeMatch = /^presto-(home|spells):task:(.+):([a-z0-9_]+)$/.exec(target);
	if (!routeMatch) return null;
	const action = routeMatch[3];
	if (
		![
			'open',
			'doit',
			'abort',
			'cancel',
			'delete',
			'reset',
			'schedule_today',
			'schedule_tomorrow',
			'schedule_next_week',
			'schedule_clear',
			'priority_clear',
			'priority_p1',
			'priority_p2',
			'priority_p3',
			'priority_p4',
			'menu',
			'menu_delete',
			'edit_description',
			'schedule_edit',
			'doit_direct',
			'view_result'
		].includes(action)
	) return null;
	return {
		taskId: routeMatch[2],
		action: action as ParsedTaskAction['action']
	};
}

export function parseTaskCompletionChange(value: unknown): ParsedTaskCompletionChange | null {
	const detail = asRecord(value);
	const target = asString(detail.componentId);
	const routeMatch = /^presto-(home|spells):task:(.+):complete$/.exec(target);
	if (!routeMatch) return null;
	const payload = asRecord(detail.detail);
	if (typeof payload.checked !== 'boolean') return null;
	return {
		taskId: routeMatch[2],
		checked: payload.checked
	};
}
