<script lang="ts">
	/**
	 * LiveToolInspector — lightweight panel that lists in-flight tool
	 * calls for the current scope so the Developer-Mode user can see
	 * what the agent is doing right now (and cancel if needed).
	 *
	 * Phase 5 of Developer Mode. Subscribes to `AgenticActionExecuted`
	 * / step events from the existing event stream and tracks
	 * elapsed time. Cancel button hits the existing
	 * `cancellation_token` endpoint.
	 *
	 * See docs/plans/2026-05-13-developer-mode-workbench.md Phase 5.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { applyExecutionControl } from '$lib/magician/execution/controlClient';
	import { showError } from '$lib/shared/stores/notifications';

	/// null = the unscoped /dev workbench (all sessions, no thread label).
	export let threadId: string | null = null;

	type InFlightTool = {
		executionId: string;
		stepId: string;
		toolName: string;
		startedAt: number;
	};

	let inFlight: InFlightTool[] = [];
	let unsubscribe: (() => void) | null = null;
	let now = Date.now();
	let tickTimer: ReturnType<typeof setInterval> | null = null;

	onMount(() => {
		unsubscribe = v2Events.subscribe((events) => {
			for (const event of events) {
				// event.event_type is a typed union that doesn't enumerate
				// every agentic event — widen to string so we can match
				// against the runtime taxonomy entries.
				const type = event.event_type as string;
				const data = (event as { data?: Record<string, unknown> }).data;
				if (!data) continue;
				// Track agent step starts and completions to maintain a
				// "what's in flight right now" view.
				if (type === 'AgenticStepStarted') {
					const executionId = (data.execution_id as string) ?? '';
					const stepId = (data.step_id as string) ?? '';
					const toolName = (data.tool_name as string) ?? (data.action_type as string) ?? 'tool';
					const startedAt =
						(data.timestamp as number) ?? (data.started_at_ms as number) ?? Date.now();
					if (!executionId || !stepId) continue;
					inFlight = [
						...inFlight.filter((t) => !(t.executionId === executionId && t.stepId === stepId)),
						{ executionId, stepId, toolName, startedAt }
					];
				} else if (
					type === 'AgenticStepCompleted' ||
					type === 'AgenticStepFailed'
				) {
					const executionId = (data.execution_id as string) ?? '';
					const stepId = (data.step_id as string) ?? '';
					inFlight = inFlight.filter(
						(t) => !(t.executionId === executionId && t.stepId === stepId)
					);
				}
			}
		});
		tickTimer = setInterval(() => {
			now = Date.now();
		}, 500);
	});

	onDestroy(() => {
		unsubscribe?.();
		if (tickTimer) clearInterval(tickTimer);
	});

	function formatElapsed(startedAt: number): string {
		const ms = Math.max(0, now - startedAt);
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${Math.round(ms / 100) / 10}s`;
		const sec = Math.floor(ms / 1000);
		return `${Math.floor(sec / 60)}m${sec % 60}s`;
	}

	async function cancel(tool: InFlightTool): Promise<void> {
		try {
			await applyExecutionControl(tool.executionId, 'cancel');
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Could not stop this execution.');
		}
	}
</script>

<section class="tool-inspector" aria-label="Live tool calls">
	<header class="tool-inspector__head">
		<span class="tool-inspector__title">In flight</span>
		<span class="tool-inspector__count">{inFlight.length}</span>
		<span class="tool-inspector__thread">{threadId ? `#${threadId}` : 'all sessions'}</span>
	</header>
	{#if inFlight.length === 0}
		<p class="tool-inspector__empty">No tools currently running.</p>
	{:else}
		<ul class="tool-inspector__list">
			{#each inFlight as tool (tool.executionId + '/' + tool.stepId)}
				<li class="tool-inspector__entry">
					<span class="tool-inspector__name">{tool.toolName}</span>
					<span class="tool-inspector__elapsed">{formatElapsed(tool.startedAt)}</span>
					<button
						type="button"
						class="tool-inspector__cancel"
						title="Cancel this execution"
						on:click={() => void cancel(tool)}
					>cancel</button>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.tool-inspector {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 10px 12px;
		margin-top: 8px;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 6px;
	}

	.tool-inspector__head {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}

	.tool-inspector__title {
		font-family: var(--font-primary);
		font-size: 11px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-primary, #1a1a1a);
	}

	.tool-inspector__count {
		font-family: var(--font-mono);
		font-size: 10.5px;
		padding: 1px 8px;
		border-radius: 999px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-muted, #888);
	}

	.tool-inspector__thread {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-muted, #888);
	}

	.tool-inspector__empty {
		margin: 0;
		font-size: 12px;
		color: var(--text-muted, #888);
	}

	.tool-inspector__list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.tool-inspector__entry {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 4px 6px;
		border-radius: 4px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.tool-inspector__name {
		font-weight: 600;
		flex: 1 1 auto;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.tool-inspector__elapsed {
		font-size: 11px;
		color: var(--text-muted, #888);
	}

	.tool-inspector__cancel {
		font-family: var(--font-primary);
		font-size: 10.5px;
		padding: 1px 8px;
		border: 1px solid color-mix(in srgb, var(--color-error, #c0392b) 40%, transparent);
		background: transparent;
		color: var(--color-error, #c0392b);
		border-radius: 4px;
		cursor: pointer;
	}

	.tool-inspector__cancel:hover {
		background: var(--color-error-soft, color-mix(in srgb, var(--color-error, #c0392b) 12%, transparent));
	}
</style>
