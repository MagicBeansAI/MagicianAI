<script lang="ts">
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { Task } from '$lib/stores/taskStore';
	import {
		formatRelative,
		planStatusColor,
		planStatusHeadline,
		planStatusSummary,
		taskPendingQuestions,
		titleCase
	} from './planHelpers';

	export let tasks: Task[];
	export let activeReplyTaskId: string | null = null;
	export let activeReplyQuestionId: string | null = null;
	export let disabled = false;
	export let isActionBusy: (taskId: string, action: string) => boolean;
	export let onAction: (
		task: Task,
		action: 'open' | 'approve' | 'reject' | 'replan' | 'execute'
	) => Promise<void>;
	export let onReplyToQuestion: (taskId: string, questionId?: string) => boolean;
	export let onOpenThread: (taskId: string) => void;

	let expanded = false;
	let selectedTaskId: string | null = null;
	let selectedTask: Task | null = null;
	let pendingQuestionCount = 0;
	let draftReviewCount = 0;
	let attentionItemCount = 0;

	$: if (activeReplyTaskId && tasks.some((task) => task.id === activeReplyTaskId)) {
		selectedTaskId = activeReplyTaskId;
	}
	$: if (!selectedTaskId || !tasks.some((task) => task.id === selectedTaskId)) {
		selectedTaskId = tasks[0]?.id ?? null;
	}
	$: selectedTask = tasks.find((task) => task.id === selectedTaskId) ?? tasks[0] ?? null;
	$: pendingQuestionCount = tasks.reduce(
		(total, task) => total + taskPendingQuestions(task).length,
		0
	);
	$: draftReviewCount = tasks.filter((task) => task.planStatus === 'draft').length;
	$: attentionItemCount = pendingQuestionCount + draftReviewCount;

	function replyToQuestion(taskId: string, questionId: string): void {
		if (onReplyToQuestion(taskId, questionId)) {
			expanded = false;
		}
	}
</script>

{#if selectedTask}
	<section class="planner-dock" class:planner-dock--expanded={expanded} aria-label="Planner attention">
		<header class="planner-dock__header">
			<button
				type="button"
				class="planner-dock__toggle"
				on:click={() => (expanded = !expanded)}
				aria-expanded={expanded}
			>
				<span class="planner-dock__identity">
					<Icon name="file-text" size={16} />
					<strong>Planner needs you</strong>
					<time>{formatRelative(selectedTask.updatedAt)}</time>
				</span>
				<span class="planner-dock__counts">
					{#if tasks.length === 1}
						<span class="planner-dock__collapsed-title" title={selectedTask.title}>{selectedTask.title}</span>
					{/if}
					<span>{attentionItemCount} {attentionItemCount === 1 ? 'item' : 'items'}</span>
					{#if tasks.length > 1}
						<span class="planner-dock__across">across {tasks.length} tasks</span>
					{/if}
					<Icon name={expanded ? 'chevron-up' : 'chevron-down'} size={16} />
				</span>
			</button>

			{#if expanded}
				<div class="planner-dock__selection">
					{#if tasks.length > 1}
						<label>
							<span class="sr-only">Planner task needing attention</span>
							<select bind:value={selectedTaskId} aria-label="Planner task needing attention">
								{#each tasks as task (task.id)}
									<option value={task.id}>{task.title}</option>
								{/each}
							</select>
						</label>
					{:else}
						<strong title={selectedTask.title}>{selectedTask.title}</strong>
					{/if}
					<Badge
						text={titleCase(selectedTask.planStatus || 'planning')}
						color={planStatusColor(selectedTask.planStatus)}
					/>
				</div>
			{/if}
		</header>

		{#if expanded}
		<div class="planner-dock__body">
			<div class="planner-dock__summary">
				<strong>{planStatusHeadline(selectedTask)}</strong>
				<span>{planStatusSummary(selectedTask)}</span>
			</div>

			{#if selectedTask.planStatus === 'eliciting' && taskPendingQuestions(selectedTask).length > 0}
				<div class="planner-dock__questions">
					{#each taskPendingQuestions(selectedTask) as question, index (question.id)}
						<div
							class="planner-dock__question"
							class:planner-dock__question--active={activeReplyTaskId === selectedTask.id && activeReplyQuestionId === question.id}
						>
							<div class="planner-dock__question-copy">
								<span>Question {index + 1} of {taskPendingQuestions(selectedTask).length}</span>
								<p>{question.question}</p>
							</div>
							<Button
								label={activeReplyTaskId === selectedTask.id && activeReplyQuestionId === question.id ? 'Replying' : 'Reply'}
								size="sm"
								variant={activeReplyTaskId === selectedTask.id && activeReplyQuestionId === question.id ? 'primary' : 'outline'}
								disabled={disabled}
								on:click={() => replyToQuestion(selectedTask!.id, question.id)}
							/>
						</div>
					{/each}
				</div>
			{/if}

			<div class="planner-dock__actions">
				<Button
					label={selectedTask.planStatus === 'draft' ? 'Review plan' : 'Open plan'}
					variant="outline"
					size="sm"
					disabled={disabled || isActionBusy(selectedTask.id, 'open')}
					on:click={() => void onAction(selectedTask!, 'open')}
				/>
				<Button
					label="Open thread"
					variant="outline"
					size="sm"
					disabled={disabled}
					on:click={() => onOpenThread(selectedTask!.id)}
				/>
				{#if selectedTask.planStatus === 'draft'}
					<Button
						label="Approve"
						size="sm"
						disabled={disabled || isActionBusy(selectedTask.id, 'approve')}
						on:click={() => void onAction(selectedTask!, 'approve')}
					/>
					<Button
						label="Replan"
						variant="outline"
						size="sm"
						disabled={disabled || isActionBusy(selectedTask.id, 'replan')}
						on:click={() => void onAction(selectedTask!, 'replan')}
					/>
					<Button
						label="Reject"
						variant="outline"
						size="sm"
						disabled={disabled || isActionBusy(selectedTask.id, 'reject')}
						on:click={() => void onAction(selectedTask!, 'reject')}
					/>
				{/if}
			</div>
		</div>
		{/if}
	</section>
{/if}

<style>
	.planner-dock {
		flex: 0 0 auto;
		width: min(var(--chat-col, 760px), calc(100% - 48px));
		margin: 0 auto 0.45rem;
		border: 1px solid var(--border-soft, #ddd);
		border-radius: 8px;
		background: var(--bg-elevated, #fff);
		box-shadow: var(--shadow-sm, 0 3px 12px rgba(0, 0, 0, 0.08));
		overflow: hidden;
	}

	.planner-dock__selection,
	.planner-dock__identity,
	.planner-dock__counts,
	.planner-dock__actions,
	.planner-dock__question {
		display: flex;
		align-items: center;
	}

	.planner-dock__header {
		display: grid;
		background: color-mix(in srgb, var(--bg-soft, #f6f1e8) 68%, var(--bg-elevated, #fff));
	}

	.planner-dock__toggle {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		width: 100%;
		min-width: 0;
		padding: 0.55rem 0.7rem;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
	}

	.planner-dock__toggle:hover {
		background: color-mix(in srgb, var(--text-primary, #2d3436) 5%, transparent);
	}

	.planner-dock__toggle:focus-visible {
		outline: 2px solid var(--color-info, #4d9de0);
		outline-offset: -2px;
	}

	.planner-dock__identity {
		min-width: 0;
		gap: 0.35rem;
		font-size: var(--text-xs);
		color: var(--text-primary, #2d3436);
	}

	.planner-dock__identity strong {
		white-space: nowrap;
	}

	.planner-dock__identity time {
		font-weight: 400;
		color: var(--text-muted, #7f8c8d);
	}

	.planner-dock__counts {
		justify-content: flex-end;
		gap: 0.35rem;
		min-width: 0;
		font-size: var(--text-xs);
		color: var(--text-secondary, #5f6668);
		white-space: nowrap;
	}

	.planner-dock__collapsed-title {
		max-width: min(18rem, 36vw);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-primary, #2d3436);
	}

	.planner-dock__selection {
		justify-content: flex-end;
		gap: 0.5rem;
		min-width: 0;
		padding: 0.5rem 0.7rem;
		border-top: 1px solid var(--border-soft, #ddd);
	}

	.planner-dock__selection label {
		min-width: 0;
	}

	.planner-dock__selection select {
		max-width: min(22rem, 42vw);
		min-width: 10rem;
		padding: 0.25rem 1.7rem 0.25rem 0.45rem;
		border: 1px solid var(--border-default, #ccc);
		border-radius: 6px;
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #2d3436);
		font: inherit;
		font-size: var(--text-xs);
	}

	.planner-dock__selection strong {
		max-width: min(24rem, 42vw);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-sm);
	}

	.planner-dock__body {
		display: grid;
		gap: 0.55rem;
		padding: 0.65rem 0.7rem;
		border-top: 1px solid var(--border-soft, #ddd);
	}

	.planner-dock__summary {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
		min-width: 0;
		font-size: var(--text-xs);
	}

	.planner-dock__summary strong {
		flex: 0 0 auto;
		color: var(--text-primary, #2d3436);
	}

	.planner-dock__summary span {
		min-width: 0;
		color: var(--text-secondary, #5f6668);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.planner-dock__questions {
		display: grid;
		border-top: 1px solid var(--border-soft, #ddd);
	}

	.planner-dock__question {
		justify-content: space-between;
		gap: 0.75rem;
		min-width: 0;
		padding: 0.55rem 0;
		border-bottom: 1px solid var(--border-soft, #ddd);
	}

	.planner-dock__question--active {
		color: var(--color-info, #347aa5);
	}

	.planner-dock__question-copy {
		min-width: 0;
		display: grid;
		gap: 0.15rem;
	}

	.planner-dock__question-copy span {
		font-size: var(--text-2xs);
		font-weight: 700;
		color: var(--text-muted, #7f8c8d);
		text-transform: uppercase;
	}

	.planner-dock__question-copy p {
		margin: 0;
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-primary, #2d3436);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.planner-dock__actions {
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.planner-dock__question :global(.muij-button),
	.planner-dock__actions :global(.muij-button) {
		flex: 0 0 auto;
		white-space: nowrap;
		overflow-wrap: normal;
		word-break: normal;
	}

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	@media (max-width: 640px) {
		.planner-dock {
			width: calc(100% - 16px);
			margin-bottom: 0.35rem;
		}

		.planner-dock__summary {
			align-items: flex-start;
			flex-direction: column;
		}

		.planner-dock__selection {
			width: 100%;
			justify-content: space-between;
		}

		.planner-dock__toggle {
			align-items: flex-start;
		}

		.planner-dock__identity {
			flex-wrap: wrap;
		}

		.planner-dock__across {
			display: none;
		}

		.planner-dock__collapsed-title {
			max-width: 38vw;
		}

		.planner-dock__selection label,
		.planner-dock__selection select {
			width: 100%;
			max-width: none;
		}

		.planner-dock__summary span {
			white-space: normal;
		}
	}
</style>
