<script lang="ts">
	import { browser } from '$app/environment';
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import type { Unsubscriber } from 'svelte/store';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import {
		getComponentsByAgent,
		requestAgentSnapshotIfNeeded,
		type MuijComponent,
		type MuijInteractionEventDetail
	} from '$lib/stores/muijStore';
	import MuijRenderer from './generative/MuijRenderer.svelte';

	export let agentId = '';
	export let title = 'Live execution view';
	export let showEmpty = true;

	const dispatch = createEventDispatcher<{ interaction: MuijInteractionEventDetail }>();
	const COMPONENT_LABELS: Record<string, string> = {
		Gauge: 'Progress',
		TerminalTransient: 'Agent log',
		LiveSelectors: 'Target elements',
		Tabs: 'Trace tabs',
		Tree: 'Execution tree',
		TreeNode: 'Execution tree',
		Table: 'Table',
		Card: 'Card',
		Markdown: 'Notes',
		CodeBlock: 'Code',
		ActionBus: 'Actions'
	};
	const SUMMARY_OMITTED_TYPES = new Set([
		'Container',
		'Stack',
		'Grid',
		'SplitPanel',
		'Panel',
		'ScrollArea',
		'Divider'
	]);

	let components: MuijComponent[] = [];
	let mounted = false;
	let subscribedAgentId = '';
	let unsubscribe: Unsubscriber | null = null;

	$: normalizedAgentId = agentId.trim();
	$: componentTypes = collectComponentTypes(components);
	$: componentSummary = summarizeComponentTypes(componentTypes);
	$: if (mounted && normalizedAgentId !== subscribedAgentId) {
		subscribeToAgent(normalizedAgentId);
	}

	function unsubscribeCurrent(): void {
		if (unsubscribe) {
			unsubscribe();
			unsubscribe = null;
		}
	}

	function subscribeToAgent(nextAgentId: string): void {
		unsubscribeCurrent();
		components = [];
		subscribedAgentId = nextAgentId;
		if (!nextAgentId || !browser) return;

		unsubscribe = getComponentsByAgent(nextAgentId).subscribe((value) => {
			components = value;
		});
		v2Events.connectGlobal();
		requestAgentSnapshotIfNeeded(nextAgentId, v2Events);
	}

	function collectComponentTypes(values: MuijComponent[]): string[] {
		const seen = new Set<string>();
		const stack = [...values].reverse();
		while (stack.length > 0) {
			const component = stack.pop();
			if (!component) continue;
			seen.add(component.component_type);
			if (Array.isArray(component.children)) {
				for (let i = component.children.length - 1; i >= 0; i -= 1) {
					stack.push(component.children[i]);
				}
			}
		}
		return Array.from(seen);
	}

	function summarizeComponentTypes(types: string[]): string {
		const labels: string[] = [];
		const seen = new Set<string>();
		for (const type of types) {
			if (SUMMARY_OMITTED_TYPES.has(type)) continue;
			const label = COMPONENT_LABELS[type] ?? type;
			if (seen.has(label)) continue;
			seen.add(label);
			labels.push(label);
		}
		return labels.join(' · ');
	}

	function forwardInteraction(event: CustomEvent<MuijInteractionEventDetail>): void {
		dispatch('interaction', event.detail);
	}

	onMount(() => {
		mounted = true;
		subscribeToAgent(normalizedAgentId);
	});

	onDestroy(() => {
		mounted = false;
		unsubscribeCurrent();
	});
</script>

{#if normalizedAgentId}
	<section class="live-agent-surface" aria-label={title}>
		<div class="live-agent-surface__header">
			<div class="live-agent-surface__title-group">
				<p class="live-agent-surface__eyebrow">Runtime instrumentation</p>
				<h2>{title}</h2>
				{#if componentSummary}
					<p class="live-agent-surface__summary">{componentSummary}</p>
				{/if}
			</div>
			<span
				class:live-agent-surface__state--active={components.length > 0}
				class="live-agent-surface__state"
			>
				{components.length > 0 ? 'live snapshot' : 'waiting'}
			</span>
		</div>

		{#if components.length > 0}
			<div class="live-agent-surface__body">
				<MuijRenderer
					{components}
					agentId={normalizedAgentId}
					idNamespace={`live-${normalizedAgentId}`}
					validateRouteContract={false}
					on:interaction={forwardInteraction}
				/>
			</div>
		{:else if showEmpty}
			<div class="live-agent-surface__empty">
				No live runtime snapshot.
			</div>
		{/if}
	</section>
{/if}

<style>
	.live-agent-surface {
		margin: 14px 0 20px;
		width: 100%;
		overflow: hidden;
		border: 1px solid var(--border-soft, #d6dde8);
		border-radius: var(--radius-lg, 10px);
		background: var(--bg-card, #ffffff);
	}

	.live-agent-surface__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
		padding: 11px 14px;
		border-bottom: 1px solid var(--border-soft, #d6dde8);
		background: var(--bg-soft, #f8fafc);
	}

	.live-agent-surface__title-group {
		min-width: 0;
	}

	.live-agent-surface__eyebrow {
		margin: 0 0 3px;
		color: var(--text-muted, #64748b);
		font-size: 0.68rem;
		font-weight: 700;
		letter-spacing: 0.02em;
		text-transform: uppercase;
	}

	.live-agent-surface h2 {
		margin: 0;
		color: var(--text-primary, #0f172a);
		font-size: 0.98rem;
		line-height: 1.25;
	}

	.live-agent-surface__summary {
		margin: 4px 0 0;
		color: var(--text-secondary, #475569);
		font-size: 0.78rem;
		line-height: 1.35;
	}

	.live-agent-surface__state {
		flex: 0 0 auto;
		margin-top: 1px;
		border: 1px solid var(--border-soft, #d6dde8);
		border-radius: 999px;
		padding: 4px 9px;
		color: var(--text-muted, #64748b);
		background: var(--bg-surface, #f8fafc);
		font-size: 0.76rem;
		font-weight: 700;
	}

	.live-agent-surface__state--active {
		color: var(--accent-primary, #2563eb);
		background: color-mix(in srgb, var(--accent-primary, #2563eb) 8%, var(--bg-card, #ffffff));
		border-color: color-mix(in srgb, var(--accent-primary, #2563eb) 30%, var(--border-soft, #d6dde8));
	}

	.live-agent-surface__body {
		display: grid;
		align-items: start;
		gap: 12px;
		max-height: clamp(240px, 38vh, 420px);
		overflow: auto;
		padding: 12px;
		background: var(--bg-card, #ffffff);
	}

	.live-agent-surface__empty {
		padding: 14px;
		color: var(--text-muted, #64748b);
		font-size: 0.88rem;
		line-height: 1.45;
	}

	.live-agent-surface__body :global(.muij-terminal-body) {
		max-height: 140px;
	}

	.live-agent-surface__body :global(.muij-live-selectors) {
		max-height: 220px;
	}

	.live-agent-surface__body :global(.muij-tabs-panel) {
		max-height: 240px;
		overflow: auto;
	}

	@media (max-width: 720px) {
		.live-agent-surface__header {
			flex-direction: column;
		}
	}
</style>
