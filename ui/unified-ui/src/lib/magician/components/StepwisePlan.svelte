<script lang="ts" context="module">
	export interface StepwisePlanStep {
		id: string;
		label: string;
		status: 'completed' | 'ongoing' | 'pending' | 'failed';
		isSubtask: boolean;
		logStepId?: string;
	}

	export interface StepwisePlanLog {
		title: string;
		message: string;
	}
</script>

<script lang="ts">
	/**
	 * <StepwisePlan /> — single rendering surface for the live working plan.
	 *
	 * Inputs are deliberately generic so the same component can serve the
	 * Plan tab (markdown-parsed taskplan steps) and any future surface that
	 * has steps + logs to render. The Run tab's Plan Steps card renders
	 * `ExecutionPanelStepStatus` directly — that has a different shape
	 * (`number`/`name`/`step_id`/`capability`) and isn't being unified yet;
	 * a normalized adapter is filed as a future polish.
	 *
	 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` Phase 2.
	 */
	export let steps: StepwisePlanStep[] = [];
	/** Resolve per-step logs. Returns empty array if no logs exist. */
	export let getStepLogs: (step: StepwisePlanStep) => StepwisePlanLog[] = () => [];
	/** When true, parent rows are sticky during scroll. Pass for plans
	 *  with > 5 steps; the host owns the threshold. */
	export let stickyEnabled: boolean = false;
	/** Strip `<!-- step_id: … -->` comments from labels (markdown source). */
	export let stripInlineCommentTag: boolean = true;
	/** Empty-state copy when `steps` is empty. Optional; renders nothing
	 *  when omitted so the host can supply its own fallback (e.g. a single
	 *  task-summary row). */
	export let emptyMessage: string = '';

	function cleanLabel(label: string): string {
		if (!stripInlineCommentTag) return label;
		return label.replace(/<!--\s*step_id:\s*\S+\s*-->/, '').trim();
	}
</script>

{#if steps.length > 0}
	<div class="parsed-steps-list" class:sticky-enabled={stickyEnabled}>
		{#each steps as step (step.id)}
			{@const stepLogs = getStepLogs(step)}
			<div class="parsed-step-row {step.status}" class:subtask={step.isSubtask}>
				<div class="step-indicator">
					{#if step.status === 'completed'}
						<div class="tick-mark">✓</div>
					{:else if step.status === 'failed'}
						<div class="failed-mark">×</div>
					{:else if step.status === 'ongoing'}
						<div class="ongoing-spinner"></div>
					{:else}
						<div class="pending-circle"></div>
					{/if}
				</div>
				<div class="step-content">
					<div class="step-label" title={cleanLabel(step.label)}>
						{cleanLabel(step.label)}
					</div>
					{#if stepLogs.length > 0}
						<div class="step-logs">
							{#each stepLogs as log}
								<div class="step-log-entry">
									<span class="log-title">{log.title}</span>
									<span class="log-msg" title={log.message}>{log.message}</span>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			</div>
		{/each}
	</div>
{:else if emptyMessage}
	<p class="stepwise-empty">{emptyMessage}</p>
{/if}

<style>
	.parsed-steps-list {
		display: flex;
		flex-direction: column;
		position: relative;
	}

	.parsed-step-row {
		display: flex;
		align-items: flex-start;
		gap: 0.75rem;
		padding: 0.75rem;
		background: var(--bg-card, #fff);
		transition: all 0.2s ease;
		border-left: 2px solid transparent;
	}

	.sticky-enabled .parsed-step-row:not(.subtask) {
		position: sticky;
		top: 2.8rem;
		z-index: 5;
		font-weight: 700;
		font-size: 0.92rem;
		border-bottom: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 30%, transparent);
		box-shadow: var(--shadow-sm);
	}

	.parsed-step-row:not(.subtask) {
		background: var(--bg-card, #fff);
		font-size: 0.92rem;
		font-weight: 700;
	}

	.parsed-step-row.subtask {
		margin-left: 1.5rem;
		font-size: 0.85rem;
		font-weight: 500;
		padding: 0.45rem 0.75rem;
		border-left: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 50%, transparent);
	}

	.parsed-step-row.completed {
		background: var(--bg-card, #fff);
	}

	.parsed-step-row.completed .step-label {
		text-decoration: line-through;
		color: var(--text-muted, #7c746a);
	}

	.parsed-step-row.completed .step-log-entry {
		opacity: 0.7;
	}

	.parsed-step-row.completed .tick-mark {
		opacity: 0.6;
	}

	.parsed-step-row.failed {
		border-left-color: color-mix(in srgb, var(--status-error, #d64545) 70%, transparent);
		background: color-mix(in srgb, var(--status-error, #d64545) 5%, var(--bg-card, #fff));
	}

	.parsed-step-row.failed .step-label {
		color: var(--status-error, #d64545);
	}

	.parsed-step-row.ongoing {
		border-left-color: var(--accent-primary, #bf6f45);
		background: color-mix(in srgb, var(--accent-primary, #bf6f45) 3%, var(--bg-card, #fff));
	}

	.step-indicator {
		flex-shrink: 0;
		width: 1.25rem;
		height: 1.25rem;
		display: flex;
		align-items: center;
		justify-content: center;
		margin-top: 0.1rem;
	}

	.tick-mark {
		color: var(--color-success);
		font-weight: bold;
		font-size: 1.1rem;
	}

	.failed-mark {
		color: var(--color-error);
		font-weight: 700;
		font-size: 1.1rem;
		line-height: 1;
	}

	.pending-circle {
		width: 0.75rem;
		height: 0.75rem;
		border: 2px solid var(--border-soft, #d8d0c5);
		border-radius: 50%;
		opacity: 0.6;
	}

	.ongoing-spinner {
		width: 0.9rem;
		height: 0.9rem;
		border: 2px solid var(--accent-primary, #bf6f45);
		border-top-color: transparent;
		border-radius: 50%;
		animation: stepwise-spin 1s linear infinite;
	}

	@keyframes stepwise-spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}

	.step-content {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}

	.step-logs {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		margin-top: 0.1rem;
	}

	.step-log-entry {
		display: flex;
		flex-direction: column;
		padding: 0.35rem 0.6rem;
		background: color-mix(in srgb, var(--bg-soft, #f6f1e8) 50%, transparent);
		border-radius: 0.4rem;
		font-size: 0.75rem;
		line-height: 1.35;
		border-left: 2px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 40%, transparent);
	}

	.step-log-entry .log-title {
		font-weight: 600;
		color: var(--text-secondary, #5d5850);
		font-size: 0.78rem;
	}

	.step-log-entry .log-msg {
		color: var(--text-muted, #7c746a);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.step-label {
		font-size: inherit;
		color: var(--text-primary, #2d2a26);
		line-height: 1.45;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.stepwise-empty {
		margin: 0;
		color: var(--text-muted, #7c746a);
	}
</style>
