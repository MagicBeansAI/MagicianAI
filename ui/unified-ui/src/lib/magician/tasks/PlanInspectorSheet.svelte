<script lang="ts">
	/**
	 * The task's plan inspector in a modal sheet: the plan graph (nodes and
	 * edges), the waterfall/hierarchy view, version history and plan editing,
	 * all owned by `ExecutionPlanInspector`. This only adds the sheet chrome —
	 * backdrop, header, Close, focus trap and Escape — so any surface that has a
	 * task id can open the same inspector.
	 */
	import { createEventDispatcher } from 'svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import { focusTrap } from '$lib/shared/focusTrap';
	import { rootPortal } from '$lib/shared/rootPortal';
	import ExecutionPlanInspector from '../../../routes/(app)/ExecutionPlanInspector.svelte';

	export let taskId: string;
	export let title: string | null = null;
	export let planStatus: string | null = null;

	const dispatch = createEventDispatcher<{ close: void }>();
	const close = () => dispatch('close');

	function onKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') {
			event.stopPropagation();
			close();
		}
	}

	$: statusLabel = planStatus
		? planStatus.charAt(0).toUpperCase() + planStatus.slice(1).replace(/_/g, ' ')
		: null;
</script>

<svelte:window on:keydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="plan-inspector-sheet-backdrop" use:rootPortal on:click={close}>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="plan-inspector-sheet"
		role="dialog"
		aria-modal="true"
		aria-label="Task plan inspector"
		tabindex="-1"
		use:focusTrap
		on:click|stopPropagation
	>
		<header class="plan-inspector-sheet__header">
			<div class="plan-inspector-sheet__copy">
				<span class="plan-inspector-sheet__eyebrow">Plan Inspector</span>
				<h2>{title || 'Task plan'}</h2>
				<p>
					{#if statusLabel}
						{statusLabel} plan · graph, hierarchy, history and editing.
					{:else}
						Graph, hierarchy, history and editing for this task's plan.
					{/if}
				</p>
			</div>
			<Button label="Close" variant="outline" size="sm" on:click={close} />
		</header>
		<div class="plan-inspector-sheet__body">
			<ExecutionPlanInspector {taskId} modal={true} />
		</div>
	</div>
</div>

<style>
	.plan-inspector-sheet-backdrop {
		position: fixed;
		inset: 0;
		z-index: 980;
		background: rgba(33, 37, 41, 0.34);
		backdrop-filter: blur(2px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 1rem;
	}

	.plan-inspector-sheet {
		width: min(1040px, calc(100vw - 1.8rem));
		max-height: calc(100dvh - 2rem);
		display: flex;
		flex-direction: column;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: 1.2rem;
		box-shadow: 0 22px 56px rgba(26, 32, 44, 0.18);
		overflow: hidden;
	}

	.plan-inspector-sheet__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		padding: 1rem 1.05rem 0.85rem;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
		background: linear-gradient(
			180deg,
			color-mix(
					in srgb,
					var(--accent-secondary-soft, rgba(78, 205, 196, 0.12)) 72%,
					var(--bg-card, #ffffff)
				)
				0%,
			var(--bg-card, #ffffff) 100%
		);
	}

	.plan-inspector-sheet__copy {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.plan-inspector-sheet__header :global(.native-button) {
		flex-shrink: 0;
		white-space: nowrap;
	}

	.plan-inspector-sheet__eyebrow {
		display: inline-block;
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #8f9799);
		margin-bottom: 0.25rem;
	}

	.plan-inspector-sheet__copy h2 {
		margin: 0;
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary, #2d3436);
	}

	.plan-inspector-sheet__copy p {
		margin: 0.25rem 0 0;
		font-size: 0.76rem;
		line-height: 1.35;
		color: var(--text-muted, #7f8c8d);
	}

	.plan-inspector-sheet__body {
		flex: 1;
		min-height: 0;
		overflow: auto;
		padding: 0.9rem 1rem 1rem;
		background: var(--bg-base, #fffdf8);
	}

	@media (max-width: 900px) {
		.plan-inspector-sheet-backdrop {
			padding: 0.4rem;
		}

		.plan-inspector-sheet {
			width: 100%;
			max-height: calc(100dvh - 0.8rem);
			border-radius: 1rem;
		}
	}
</style>
