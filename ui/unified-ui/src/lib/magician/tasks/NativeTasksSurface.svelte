<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { Task, TaskStatus } from '$lib/stores/taskStore';
	import { cronToHumanReadable } from '$lib/utils/cron';
	import { statusTone } from '$lib/shared/statusTone';
	import ExecutionControls from '$lib/magician/components/execution/ExecutionControls.svelte';
	import type { ParsedTaskAction, ParsedTaskCompletionChange } from './types';

	export let tasks: Task[] = [];
	export let selectedTaskId: string | null = null;
	export let isLoading = false;
	export let interactionBusy = false;
	export let pageError: string | null = null;
	export let activeFilter = 'all';
	export let activeTagEditorTaskId: string | null = null;
	export let activeScheduleEditorTaskId: string | null = null;

	type ScheduleValues = {
		schedule_cron: string;
		schedule_timezone: string;
		schedule_retention_max_records: string;
		schedule_retention_max_days: string;
	};

	const dispatch = createEventDispatcher<{
		action: ParsedTaskAction;
		complete: ParsedTaskCompletionChange;
		tagAdd: { taskId: string; tagName: string };
		tagRemove: { taskId: string; tagName: string };
		toggleTagEditor: { taskId: string };
		toggleScheduleEditor: { taskId: string };
		scheduleSubmit: { taskId: string; values: ScheduleValues };
		openMenu: { taskId: string; anchor: HTMLElement };
		executionChanged: { taskId: string };
		compose: void;
	}>();

	type NativeTaskAction = {
		action: ParsedTaskAction['action'];
		label: string;
		primary?: boolean;
	};

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		return '';
	}

	function parseDueDate(value: string | undefined): Date | null {
		const raw = asString(value).trim();
		if (!raw) return null;
		const isoMatch = /^(\d{4})-(\d{2})-(\d{2})/.exec(raw);
		if (isoMatch) {
			const parsed = new Date(Number(isoMatch[1]), Number(isoMatch[2]) - 1, Number(isoMatch[3]));
			if (!Number.isNaN(parsed.getTime())) return parsed;
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
		if (!value) return '';
		const parsed = parseDueDate(value);
		const delta = dueDateDeltaDays(value);
		if (!parsed || delta === null) return 'invalid date';
		if (delta === 0) return 'Today';
		if (delta === -1) return 'Yesterday';
		if (delta === 1) return 'Tomorrow';
		if (delta === -2) return '2 days ago';
		if (delta === 2) return 'In 2 days';
		return parsed.toLocaleDateString('en-US', {
			weekday: 'short',
			month: 'short',
			day: 'numeric'
		});
	}

	function statusLabel(status: TaskStatus | string | undefined, task?: Task): string {
		if (status === 'pending' || status === 'ready') return 'Ready';
		if (status === 'planning') return 'Planning';
		if (status === 'running') return 'Running';
		if (status === 'paused') return task?.pendingQuestion ? 'Needs Input' : 'Paused';
		if (status === 'completed') return 'Completed';
		if (status === 'failed') return 'Failed';
		if (status === 'cancelled') return 'Cancelled';
		if (status === 'archived') return 'Archived';
		return 'Unknown';
	}

	function formatRelativeTime(timestamp: string | undefined): string {
		const parsed = Date.parse(asString(timestamp));
		if (!Number.isFinite(parsed)) return 'never';
		const delta = Date.now() - parsed;
		const abs = Math.abs(delta);
		const minutes = Math.round(abs / 60000);
		const hours = Math.round(abs / 3600000);
		const days = Math.round(abs / 86400000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ${delta >= 0 ? 'ago' : 'from now'}`;
		if (hours < 36) return `${hours}h ${delta >= 0 ? 'ago' : 'from now'}`;
		return `${days}d ${delta >= 0 ? 'ago' : 'from now'}`;
	}

	function formatElapsed(timestamp: string | undefined): string {
		const parsed = Date.parse(asString(timestamp));
		if (!Number.isFinite(parsed)) return '';
		const minutes = Math.round(Math.max(0, Date.now() - parsed) / 60000);
		if (minutes < 1) return 'under a minute';
		if (minutes < 60) return `${minutes}m`;
		const hours = Math.round(minutes / 60);
		if (hours < 36) return `${hours}h`;
		return `${Math.round(hours / 24)}d`;
	}

	function activityLine(task: Task): string {
		if (statusTone(task.status).tone !== 'running') return '';
		const stepTitle = asString(task.currentSubstepTitle).trim() || asString(task.currentStepTitle).trim();
		if (stepTitle) return stepTitle;
		const inProgressStep = Array.isArray(task.planSteps)
			? task.planSteps.find((step) => step.status === 'in_progress')
			: undefined;
		const stepDescription = asString(inProgressStep?.description).trim();
		if (stepDescription) return stepDescription;
		const elapsed = formatElapsed(task.updatedAt);
		if (!elapsed) return '';
		return task.status === 'planning' ? `Planning ${elapsed}` : `Active ${elapsed}`;
	}

	function preferredTaskTitle(task: Task, fallbackIndex: number): string {
		const title = asString(task.title).trim();
		if (title && !/^[a-z0-9_-]{20,}$/i.test(title)) return title;
		const description = asString(task.description).trim();
		if (description) return description.length > 72 ? `${description.slice(0, 69)}...` : description;
		return `Task ${fallbackIndex + 1}`;
	}

	function tagNames(task: Task): string[] {
		return task.tags.map((tag) => asString(typeof tag === 'string' ? tag : tag.name).trim()).filter(Boolean);
	}

	function metaParts(task: Task): string[] {
		return [
			asString(task.agentName).trim() || asString(task.agentId).trim(),
			task.uiThreadId ? `#${task.uiThreadId}` : '',
			`updated ${formatRelativeTime(task.updatedAt)}`
		].filter(Boolean);
	}

	function completionDisabled(task: Task): boolean {
		return interactionBusy || task.status === 'running' || task.status === 'paused';
	}

	function completionSummary(task: Task): string {
		if (task.status === 'failed' || task.status === 'cancelled') {
			return (
				asString(task.errorMessage).trim() ||
				asString(task.completionOutcome).trim() ||
				asString(task.completionSummary).trim()
			);
		}
		return asString(task.completionSummary).trim();
	}

	function showCompletionSummary(task: Task): boolean {
		return !!completionSummary(task) && !['pending', 'running', 'planning'].includes(task.status);
	}

	function isRecurring(task: Task): boolean {
		if (Boolean(task.schedule?.cron)) return true;
		if (task.tags?.some((tag) => {
			const name = typeof tag === 'string' ? tag : tag.name;
			return name?.toLowerCase() === 'recurring';
		})) return true;
		return false;
	}

	function hasFinalResult(task: Task): boolean {
		if (task.completionArtifactNames && task.completionArtifactNames.length > 0) {
			return true;
		}
		if (typeof task.completionSummary === 'string' && task.completionSummary.trim().length > 0) {
			return true;
		}
		if (task.status === 'completed' && typeof task.completionOutcome === 'string') {
			const outcome = task.completionOutcome.trim().toLowerCase();
			if (outcome && !['failed', 'cancelled', 'canceled', 'stopped', 'error'].some((word) => outcome.includes(word))) {
				return true;
			}
		}
		return false;
	}

	function taskActions(task: Task): NativeTaskAction[] {
		if (task.planStatus === 'planning') return [{ action: 'open', label: 'View Plan', primary: true }];
		if (task.planStatus === 'eliciting') {
			return [{ action: 'open', label: task.pendingQuestion ? 'Answer Question' : 'View Plan', primary: true }];
		}
		if (task.planStatus === 'draft') return [{ action: 'open', label: 'Review Plan', primary: true }];
		if (task.planStatus === 'approved' && task.status === 'ready') {
			return [{ action: 'doit', label: 'Run Plan', primary: true }];
		}
		if (task.status === 'pending') {
			return [
				{ action: 'doit', label: 'PrePlan', primary: true },
				{ action: 'doit_direct', label: 'Run Now' }
			];
		}
		if (task.status === 'ready') return [{ action: 'doit', label: 'Run Now', primary: true }];
		if (task.status === 'paused') {
			return [
				{ action: 'open', label: task.pendingQuestion ? 'View Question' : 'View Execution', primary: true },
				{ action: 'reset', label: 'Reset to Ready' }
			];
		}
		if (task.status === 'running' || task.status === 'planning') {
			return [{ action: 'abort', label: 'Stop', primary: true }];
		}
		if (task.status === 'failed' || task.status === 'cancelled') {
			const actions: NativeTaskAction[] = [];
			if (task.executionId) actions.push({ action: 'reset', label: 'Reset to Ready', primary: true });
			else if (task.planStatus) actions.push({ action: 'open', label: 'Review Plan', primary: true });
			actions.push({ action: 'publish_notes', label: 'Publish to Notes', primary: actions.length === 0 });
			return actions;
		}
		if (task.status === 'completed') {
			const actions: NativeTaskAction[] = [];
			if (hasFinalResult(task)) {
				actions.push({ action: 'view_result', label: 'Result', primary: true });
			}
			actions.push({ action: 'publish_notes', label: 'Publish to Notes', primary: actions.length === 0 });
			return actions;
		}
		return [];
	}

	function primaryAction(task: Task): NativeTaskAction | null {
		return taskActions(task).find((action) => action.primary) ?? taskActions(task)[0] ?? null;
	}

	function secondaryActions(task: Task): NativeTaskAction[] {
		const primary = primaryAction(task);
		return taskActions(task).filter((action) => {
			if (action.action === 'view_result') return false;
			if (primary && action.action === primary.action) return false;
			return true;
		});
	}

	function showExecutionControls(task: Task): boolean {
		return Boolean(
			task.activeExecutionId && ['running', 'planning', 'paused'].includes(task.status)
		);
	}

	function dueClass(task: Task): string {
		const delta = dueDateDeltaDays(task.dueDate || undefined);
		if (delta === null) return 'native-task-chip';
		if (delta < 0) return 'native-task-chip native-task-chip--error';
		if (delta === 0) return 'native-task-chip native-task-chip--warning';
		return 'native-task-chip native-task-chip--success';
	}

	function priorityClass(task: Task): string {
		const priority = asString(task.priority).trim().toUpperCase();
		if (priority === 'P1') return 'native-task-chip native-task-chip--error';
		if (priority === 'P2') return 'native-task-chip native-task-chip--warning';
		if (priority === 'P3') return 'native-task-chip native-task-chip--info';
		return 'native-task-chip';
	}

	function describeCron(cron: string): string {
		return cronToHumanReadable(cron) || cron.trim() || 'none';
	}

	function emptyCopy(filter: string): { title: string; description: string; canCreate: boolean } {
		if (filter === 'completed') {
			return { title: 'No completed tasks yet.', description: 'Tasks land here once they finish.', canCreate: false };
		}
		if (filter === 'running') return { title: 'Nothing is running right now.', description: '', canCreate: false };
		if (filter === 'overdue') return { title: 'Nothing is overdue.', description: '', canCreate: false };
		if (filter.startsWith('tag:')) {
			const tag = filter.slice(4).trim();
			return { title: `Nothing matches ${tag ? `#${tag}` : 'Tagged'}.`, description: '', canCreate: false };
		}
		return {
			title: 'Your task queue is clear',
			description: "Create a task to get started. We'll help you plan and run it.",
			canCreate: filter === 'all' || filter === 'inbox' || filter === 'today'
		};
	}

	function submitSchedule(event: SubmitEvent, taskId: string): void {
		const form = event.currentTarget;
		if (!(form instanceof HTMLFormElement)) return;
		const data = new FormData(form);
		dispatch('scheduleSubmit', {
			taskId,
			values: {
				schedule_cron: asString(data.get('schedule_cron')).trim(),
				schedule_timezone: asString(data.get('schedule_timezone')).trim(),
				schedule_retention_max_records: asString(data.get('schedule_retention_max_records')).trim(),
				schedule_retention_max_days: asString(data.get('schedule_retention_max_days')).trim()
			}
		});
	}

	function submitTag(event: SubmitEvent, taskId: string): void {
		const form = event.currentTarget;
		if (!(form instanceof HTMLFormElement)) return;
		const data = new FormData(form);
		dispatch('tagAdd', { taskId, tagName: asString(data.get('tag_name')) });
		form.reset();
	}

	function openMenu(event: MouseEvent, taskId: string): void {
		const anchor = event.currentTarget;
		if (anchor instanceof HTMLElement) {
			dispatch('openMenu', { taskId, anchor });
		}
	}
</script>

<div class="native-task-list">
	{#if pageError}
		<div class="native-task-alert" role="alert">{pageError}</div>
	{/if}

	{#if tasks.length === 0 && isLoading}
		<div class="native-task-loading" aria-label="Loading tasks">
			{#each [1, 2, 3, 4] as row}
				<div class="native-task-skeleton" aria-hidden="true" data-row={row}></div>
			{/each}
		</div>
	{:else if tasks.length === 0}
		{@const empty = emptyCopy(activeFilter)}
		<div class="native-task-empty">
			<div class="native-task-empty__icon" aria-hidden="true">*</div>
			<h2>{empty.title}</h2>
			{#if empty.description}
				<p>{empty.description}</p>
			{/if}
			{#if empty.canCreate}
				<button class="native-task-button native-task-button--primary" type="button" on:click={() => dispatch('compose')}>+ Create Task</button>
			{/if}
		</div>
	{:else}
		<div class="native-task-grid">
			{#each tasks as task, index (task.id)}
				{@const title = preferredTaskTitle(task, index)}
				{@const tone = statusTone(task.status).tone}
				{@const primary = primaryAction(task)}
				{@const dueLabel = safeDateString(task.dueDate || undefined)}
				{@const summary = completionSummary(task)}
				<article class:selected={task.id === selectedTaskId} class="native-task-card presto-task-row-card">
					<div class="native-task-main-row presto-task-rest-row">
						<input
							class="native-task-checkbox"
							type="checkbox"
							checked={task.status === 'completed'}
							disabled={completionDisabled(task)}
							aria-label={`Mark ${title} complete`}
							on:change={(event) => dispatch('complete', { taskId: task.id, checked: event.currentTarget.checked })}
						/>
						<span
							class={`presto-task-status-dot presto-task-status-dot--${tone}${tone === 'running' ? ' presto-task-status-dot--pulse' : ''}`}
							title={statusLabel(task.status, task)}
							aria-hidden="true"
						></span>
						<button
							class="native-task-title-button presto-task-title"
							type="button"
							title={title}
							on:click={() => dispatch('action', { taskId: task.id, action: 'open' })}
						>
							{title}
						</button>
						<!--
							**In the always-visible row, not down beside the status chip.**
							The status chip lives in `.presto-task-reveal-row`, which is
							`max-height: 0; opacity: 0` until the card is hovered or focused —
							fine for a label that only refines a state the dot already shows,
							and wrong for the one thing on this row that is a claim on the
							reader's time. A "someone must look at this" cue that appears only
							once you have already looked is not a cue.

							It sits next to the title rather than replacing the status dot
							because it does not contradict the status: the task really is
							`paused`, and this says what the pause is for.
						-->
						{#if isRecurring(task)}
							<span
								class="native-task-chip native-task-chip--recurring"
								title={task.schedule?.cron ? `Recurring: ${describeCron(task.schedule.cron)} (${task.schedule.cron})` : 'Recurring task'}
							>
								↻ Recurring
							</span>
						{/if}
						{#if task.awaitingDiffApproval}
							<span
								class="native-task-chip native-task-chip--attention native-task-chip--awaiting-diff"
								title="Review the file changes before they are applied"
							>
								Review changes
							</span>
						{/if}
						{#if showExecutionControls(task)}
							<ExecutionControls
								executionId={task.activeExecutionId}
								label={title}
								variant="compact"
								showCancel={false}
								refreshKey={`${task.status}:${task.updatedAt}`}
								on:changed={() => dispatch('executionChanged', { taskId: task.id })}
							/>
						{/if}
						{#if hasFinalResult(task) && primary?.action !== 'view_result'}
							<button
								class="native-task-button native-task-button--compact native-task-button--result"
								type="button"
								disabled={interactionBusy}
								title="View final result in task panel"
								on:click={() => dispatch('action', { taskId: task.id, action: 'view_result' })}
							>
								Result
							</button>
						{/if}
						{#if primary}
							<button
								class="native-task-button native-task-button--compact presto-task-primary-action"
								type="button"
								disabled={interactionBusy}
								on:click={() => dispatch('action', { taskId: task.id, action: primary.action })}
							>
								{primary.label}
							</button>
						{/if}
					</div>

					<div class="presto-task-meta-line" title={metaParts(task).join(' · ')}>
						{metaParts(task).join(' · ')}
					</div>

					{#if activityLine(task)}
						<div class="presto-task-activity-line" title={activityLine(task)}>{activityLine(task)}</div>
					{/if}

					{#if showCompletionSummary(task)}
						<div class="presto-task-result-preview" title={summary}>{summary}</div>
					{/if}

					<div class="native-task-tools presto-task-reveal-row">
						{#if task.status !== 'completed'}
							<span class={`native-task-chip native-task-chip--status native-task-chip--${tone}`}>
								{statusLabel(task.status, task)}
							</span>
						{/if}
						{#if dueLabel}
							<span class={dueClass(task)}>{dueLabel}</span>
						{/if}
						{#if task.priority}
							<span class={priorityClass(task)}>{asString(task.priority).trim().toUpperCase()}</span>
						{/if}
						{#each tagNames(task).slice(0, 4) as tagName (tagName)}
							<button class="native-task-chip native-task-chip--tag" type="button" disabled={interactionBusy} on:click={() => dispatch('tagRemove', { taskId: task.id, tagName })}>
								{tagName}
								<span aria-hidden="true">x</span>
							</button>
						{/each}
						{#if activeTagEditorTaskId === task.id}
							<form class="native-task-inline-form" on:submit|preventDefault={(event) => submitTag(event, task.id)}>
								<input name="tag_name" placeholder="Tag" disabled={interactionBusy} required />
								<button type="submit" disabled={interactionBusy}>Add</button>
							</form>
						{:else}
							<button class="native-task-button native-task-button--compact" type="button" disabled={interactionBusy} on:click={() => dispatch('toggleTagEditor', { taskId: task.id })}>+Tag</button>
						{/if}

						{#each secondaryActions(task) as action (action.action)}
							<button class="native-task-button native-task-button--compact" type="button" disabled={interactionBusy} on:click={() => dispatch('action', { taskId: task.id, action: action.action })}>
								{action.label}
							</button>
						{/each}

						{#if activeScheduleEditorTaskId === task.id}
							<form class="native-task-schedule-form" on:submit|preventDefault={(event) => submitSchedule(event, task.id)}>
								<input name="schedule_cron" placeholder="0 9 * * *" value={task.schedule?.cron || ''} disabled={interactionBusy} />
								<input name="schedule_timezone" placeholder="UTC" value={task.schedule?.timezone || ''} disabled={interactionBusy} />
								<input name="schedule_retention_max_records" placeholder="Max runs" value={task.schedule?.execution_history_retention?.max_records?.toString() || ''} disabled={interactionBusy} />
								<input name="schedule_retention_max_days" placeholder="Max days" value={task.schedule?.execution_history_retention?.max_age_days?.toString() || ''} disabled={interactionBusy} />
								<button type="submit" disabled={interactionBusy}>Set</button>
							</form>
						{:else if task.schedule?.cron}
							<button class="native-task-button native-task-button--compact" type="button" disabled={interactionBusy} title={`${describeCron(task.schedule.cron)} (${task.schedule.cron})`} on:click={() => dispatch('toggleScheduleEditor', { taskId: task.id })}>
								{describeCron(task.schedule.cron)}
							</button>
						{:else}
							<button class="native-task-button native-task-button--compact" type="button" disabled={interactionBusy} on:click={() => dispatch('toggleScheduleEditor', { taskId: task.id })}>+Sched</button>
						{/if}

						{#if task.readOnly !== true}
							<button class="native-task-button native-task-button--compact native-task-menu-button" type="button" disabled={interactionBusy} title="More actions" on:click={(event) => openMenu(event, task.id)}>
								...
							</button>
						{/if}
					</div>
				</article>
			{/each}
		</div>
	{/if}
</div>

<style>
	.native-task-list {
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
	}

	.native-task-grid,
	.native-task-loading {
		display: grid;
		gap: 0.65rem;
	}

	.native-task-card {
		border: 1px solid var(--border-soft, #e5e7eb);
		border-radius: 8px;
		background: var(--bg-card, #fff);
		padding: 0.72rem 0.8rem;
		box-shadow: var(--shadow-sm, 0 1px 2px rgb(0 0 0 / 0.06));
	}

	.native-task-card.selected {
		border-color: color-mix(in srgb, var(--accent-primary, #c2502a) 50%, var(--border-soft, #e5e7eb));
		background: color-mix(in srgb, var(--accent-primary, #c2502a) 5%, var(--bg-card, #fff));
	}

	.native-task-main-row,
	.native-task-tools {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		min-width: 0;
	}

	.native-task-tools {
		align-items: center;
		flex-wrap: wrap;
		gap: 0.35rem;
		margin-top: 0.2rem;
	}

	.native-task-checkbox {
		appearance: none;
		display: inline-grid;
		place-content: center;
		flex: 0 0 auto;
		width: 1rem;
		height: 1rem;
		border: 1.5px solid var(--border-default, color-mix(in srgb, currentColor 32%, transparent));
		border-radius: 5px;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 96%, transparent),
				color-mix(in srgb, var(--bg-soft, #f8fafc) 92%, transparent)
			);
		color: var(--text-on-accent, #fff);
		cursor: pointer;
		margin-top: 0.2rem;
		box-shadow:
			inset 0 1px 0 color-mix(in srgb, #fff 40%, transparent),
			0 1px 2px color-mix(in srgb, #000 10%, transparent);
		transition:
			background 120ms ease,
			border-color 120ms ease,
			box-shadow 120ms ease,
			transform 120ms ease;
	}

	.native-task-checkbox::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 120ms ease;
	}

	.native-task-checkbox:hover:not(:disabled) {
		border-color: var(--accent-primary, currentColor);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, currentColor) 14%, transparent),
			inset 0 1px 0 color-mix(in srgb, #fff 36%, transparent);
	}

	.native-task-checkbox:checked {
		border-color: var(--accent-primary, #c2502a);
		background: linear-gradient(
			135deg,
			var(--accent-primary, #c2502a),
			color-mix(in srgb, var(--accent-primary, #c2502a) 72%, var(--accent-secondary, #f5a35c))
		);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, #c2502a) 18%, transparent),
			0 2px 8px color-mix(in srgb, var(--accent-primary, #c2502a) 24%, transparent);
	}

	.native-task-checkbox:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.native-task-checkbox:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.native-task-checkbox:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.native-task-title-button {
		appearance: none;
		border: 0;
		background: transparent;
		color: var(--text-primary, #111827);
		cursor: pointer;
		font: inherit;
		font-weight: 750;
		padding: 0;
		text-align: left;
	}

	.native-task-title-button:hover {
		color: var(--accent-primary, #c2502a);
	}

	.native-task-button,
	.native-task-chip,
	.native-task-inline-form button,
	.native-task-schedule-form button {
		border: 1px solid var(--border-soft, #d8d2c8);
		border-radius: 6px;
		background: color-mix(in srgb, var(--bg-card, #fff) 92%, var(--bg-soft, #f8f6f2));
		color: var(--text-primary, #111827);
		cursor: pointer;
		font: inherit;
		font-size: 0.74rem;
		font-weight: 780;
		line-height: 1;
		padding: 0.35rem 0.55rem;
		text-decoration: none;
		white-space: nowrap;
	}

	.native-task-button--primary {
		border-color: var(--accent-primary, #c2502a);
		background: var(--accent-primary, #c2502a);
		color: var(--button-primary-color, #fff);
	}

	.native-task-button--compact {
		font-size: var(--text-2xs, 0.72rem);
		padding: 0.18rem 0.55rem;
	}

	.native-task-button:disabled,
	.native-task-chip:disabled,
	.native-task-inline-form button:disabled,
	.native-task-schedule-form button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.native-task-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.28rem;
		cursor: default;
	}

	.native-task-chip--tag {
		cursor: pointer;
	}

	/* The one chip that renders in the main row, where the title is the flex
	   child that grows. Pinned to its content width so a long title cannot
	   squeeze it into an ellipsis, and nudged down to sit on the title's line
	   (the row aligns to `flex-start` for the checkbox's sake). */
	.native-task-chip--awaiting-diff {
		flex: 0 0 auto;
		margin-top: 0.05rem;
		font-size: var(--text-2xs, 0.72rem);
		padding: 0.18rem 0.55rem;
	}

	.native-task-chip--running { border-color: var(--status-running); color: var(--status-running); }
	.native-task-chip--paused { border-color: var(--status-paused); color: var(--status-paused); }
	.native-task-chip--failed,
	.native-task-chip--error { border-color: var(--status-failed); color: var(--status-failed); }
	.native-task-chip--attention,
	.native-task-chip--warning { border-color: var(--status-attention); color: var(--status-attention); }
	.native-task-chip--completed,
	.native-task-chip--success { border-color: var(--status-completed); color: var(--status-completed); }
	.native-task-chip--info { border-color: var(--accent-primary, #c2502a); color: var(--accent-primary, #c2502a); }

	.native-task-inline-form,
	.native-task-schedule-form {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		flex-wrap: wrap;
	}

	.native-task-inline-form input,
	.native-task-schedule-form input {
		min-height: 1.55rem;
		max-width: 9.5rem;
		border: 1px solid var(--input-border, var(--border-soft, #d8d2c8));
		border-radius: 6px;
		background: var(--input-bg, var(--bg-card, #fff));
		color: var(--text-primary, #111827);
		font: inherit;
		font-size: 0.74rem;
		padding: 0.2rem 0.45rem;
	}

	.native-task-alert,
	.native-task-empty {
		border: 1px solid var(--border-soft, #e5e7eb);
		border-radius: 8px;
		background: var(--bg-card, #fff);
		padding: 1rem;
	}

	.native-task-alert {
		border-color: color-mix(in srgb, var(--color-error, #d23a3a) 42%, var(--border-soft, #e5e7eb));
		color: var(--color-error, #d23a3a);
	}

	.native-task-empty {
		display: grid;
		justify-items: start;
		gap: 0.45rem;
		color: var(--text-secondary, #6b7280);
	}

	.native-task-empty h2,
	.native-task-empty p {
		margin: 0;
	}

	.native-task-empty h2 {
		color: var(--text-primary, #111827);
		font-size: 1rem;
	}

	.native-task-empty__icon {
		color: var(--accent-primary, #c2502a);
		font-weight: 900;
	}

	.native-task-skeleton {
		height: 72px;
		border-radius: 8px;
		background: linear-gradient(
			90deg,
			color-mix(in srgb, var(--bg-card, #fff) 84%, var(--border-soft, #e5e7eb)),
			color-mix(in srgb, var(--bg-card, #fff) 96%, var(--border-soft, #e5e7eb)),
			color-mix(in srgb, var(--bg-card, #fff) 84%, var(--border-soft, #e5e7eb))
		);
		background-size: 200% 100%;
		animation: native-task-pulse 1.2s ease-in-out infinite;
	}

	@keyframes native-task-pulse {
		from { background-position: 200% 0; }
		to { background-position: -200% 0; }
	}

	.native-task-button--result {
		color: var(--accent-color, var(--primary, #0284c7));
		border-color: color-mix(in srgb, var(--accent-color, #0284c7) 40%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-color, #0284c7) 8%, transparent);
	}

	.native-task-button--result:hover {
		background: color-mix(in srgb, var(--accent-color, #0284c7) 16%, transparent);
	}

	.native-task-chip--recurring {
		background: color-mix(in srgb, #6366f1 14%, transparent);
		color: color-mix(in srgb, #4f46e5 80%, currentColor);
		border-color: color-mix(in srgb, #6366f1 35%, transparent);
		font-weight: 500;
	}
</style>
