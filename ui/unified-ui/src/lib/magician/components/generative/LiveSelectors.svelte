<script lang="ts">
	import { onMount } from 'svelte';

	/**
	 * LiveSelectors Component — GD-F01
	 *
	 * Displays a list of CSS selectors targeted by the agent during execution.
	 * Shows real-time feedback on which elements the agent is interacting with.
	 */

	type Outcome = 'success' | 'failed' | 'pending';

	interface SelectorEntry {
		selector: string;
		action_type: string;
		timestamp: number;
		outcome: string;
		dom_changes?: {
			total: number;
			added: number;
			removed: number;
		};
	}

	export let selectors: unknown = [];
	export let maxEntries: unknown = 10;

	$: safeSelectors = normalizeSelectors(selectors);
	$: safeMaxEntries = Math.max(1, Math.floor(toFiniteNumber(maxEntries, 10)));
	$: visibleSelectors = safeSelectors.slice(-safeMaxEntries);
	$: entryCount = safeSelectors.length;

	let nowMs = Date.now();
	onMount(() => {
		const interval = setInterval(() => {
			nowMs = Date.now();
		}, 1000);
		return () => clearInterval(interval);
	});

	function isRecord(value: unknown): value is Record<string, unknown> {
		return value !== null && typeof value === 'object' && !Array.isArray(value);
	}

	function toFiniteNumber(value: unknown, fallback: number): number {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string') {
			const parsed = Number(value.trim());
			if (Number.isFinite(parsed)) return parsed;
		}
		return fallback;
	}

	function normalizeSelectorEntry(entry: unknown): SelectorEntry | null {
		if (!isRecord(entry)) return null;
		const selector = typeof entry.selector === 'string' ? entry.selector.trim() : '';
		const actionType = typeof entry.action_type === 'string' ? entry.action_type.trim() : '';
		if (!selector || !actionType) return null;
		const outcome = typeof entry.outcome === 'string' ? entry.outcome : 'pending';
		const timestamp = toFiniteNumber(entry.timestamp, Date.now());
		const domChanges = isRecord(entry.dom_changes)
			? {
				total: Math.max(0, Math.floor(toFiniteNumber(entry.dom_changes.total, 0))),
				added: Math.max(0, Math.floor(toFiniteNumber(entry.dom_changes.added, 0))),
				removed: Math.max(0, Math.floor(toFiniteNumber(entry.dom_changes.removed, 0)))
			}
			: undefined;
		return {
			selector,
			action_type: actionType,
			timestamp,
			outcome,
			...(domChanges ? { dom_changes: domChanges } : {})
		};
	}

	function normalizeSelectors(value: unknown): SelectorEntry[] {
		if (!Array.isArray(value)) return [];
		const normalized: SelectorEntry[] = [];
		for (const entry of value) {
			const parsed = normalizeSelectorEntry(entry);
			if (parsed) {
				normalized.push(parsed);
			}
		}
		return normalized;
	}

	function normalizeOutcome(value: string): Outcome {
		if (value === 'success' || value === 'failed' || value === 'pending') {
			return value;
		}
		return 'pending';
	}

	function formatRelativeTime(timestamp: number, now: number): string {
		const diffMs = now - timestamp;
		const diffSec = Math.max(0, Math.floor(diffMs / 1000));
		if (diffSec < 60) return `${diffSec}s ago`;
		const diffMin = Math.floor(diffSec / 60);
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		return `${diffHr}h ago`;
	}

	function actionIcon(actionType: string): string {
		switch (actionType) {
			case 'Click':
			case 'ClickCoordinates':
				return '🖱️';
			case 'Type':
			case 'TypeAtCoordinates':
				return '⌨️';
			case 'Hover':
			case 'HoverAtCoordinates':
				return '👆';
			case 'Scroll':
				return '📜';
			case 'SelectOption':
				return '📋';
			case 'ToggleCheckbox':
				return '☑️';
			case 'DragAndDrop':
				return '🎯';
			case 'UploadFile':
				return '📎';
			case 'PressAndHold':
			case 'PressAndHoldAtCoordinates':
				return '🕒';
			default:
				return '▶️';
		}
	}

	function truncateSelector(selector: string, maxLength: number = 40): string {
		if (selector.length <= maxLength) return selector;
		return selector.slice(0, maxLength - 3) + '...';
	}
</script>

<div class="muij-live-selectors" role="region" aria-label="Target Elements">
	{#if visibleSelectors.length > 0}
		<div class="muij-live-selectors-header">
			<span class="muij-live-selectors-title">🎯 Target Elements</span>
			<span class="muij-live-selectors-count">{entryCount} action{entryCount !== 1 ? 's' : ''}</span>
		</div>
	<ul class="muij-live-selectors-list" role="log" aria-live="polite">
		{#each visibleSelectors as entry (entry.selector + ':' + entry.timestamp + ':' + entry.action_type)}
			{@const normalizedOutcome = normalizeOutcome(entry.outcome)}
		<li
				class="muij-live-selector-entry"
				class:muij-live-selector-success={normalizedOutcome === 'success'}
				class:muij-live-selector-failed={normalizedOutcome === 'failed'}
				class:muij-live-selector-pending={normalizedOutcome === 'pending'}
			>
				<div class="muij-selector-main">
					<span class="muij-selector-icon" aria-hidden="true">
						{actionIcon(entry.action_type)}
					</span>
					<span class="muij-selector-type">{entry.action_type}</span>
					<code class="muij-selector-target" title={entry.selector}>
						{truncateSelector(entry.selector)}
					</code>
					<span
						class="muij-selector-outcome"
						class:muij-outcome-success={normalizedOutcome === 'success'}
						class:muij-outcome-failed={normalizedOutcome === 'failed'}
						aria-label={normalizedOutcome === 'success' ? 'success' : normalizedOutcome === 'failed' ? 'failed' : 'pending'}
					>
						{#if normalizedOutcome === 'success'}<span aria-hidden="true">✓</span>{:else if normalizedOutcome === 'failed'}<span aria-hidden="true">✗</span>{:else}<span aria-hidden="true">⏳</span>{/if}
					</span>
				</div>
				<div class="muij-selector-meta">
					{#if entry.dom_changes}
						<span class="muij-selector-dom">
							{#if entry.dom_changes.added > 0}+{entry.dom_changes.added}{/if}
							{#if entry.dom_changes.removed > 0}-{entry.dom_changes.removed}{/if}
						</span>
					{/if}
						<span class="muij-selector-time">{formatRelativeTime(entry.timestamp, nowMs)}</span>
					</div>
				</li>
			{/each}
	</ul>
	{:else}
		<div class="muij-live-selectors-empty">
			<span class="muij-live-selectors-empty-icon">🎯</span>
			<span class="muij-live-selectors-empty-text">No selectors targeted yet</span>
		</div>
	{/if}
</div>

<style>
	.muij-live-selectors {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg);
		overflow: hidden;
		font-family: var(--font-primary);
		max-height: 400px;
		display: flex;
		flex-direction: column;
	}

	.muij-live-selectors-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--space-sm) var(--space-md);
		background: var(--bg-soft);
		border-bottom: 1px solid var(--border-soft);
		flex-shrink: 0;
	}

	.muij-live-selectors-title {
		font-weight: 600;
		font-size: 0.875rem;
		color: var(--text-primary);
	}

	.muij-live-selectors-count {
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.muij-live-selectors-list {
		list-style: none;
		margin: 0;
		padding: 0;
		overflow-y: auto;
		min-height: 0;
		scrollbar-width: thin;
		scrollbar-color: var(--border-soft) transparent;
	}

	.muij-live-selectors-list::-webkit-scrollbar {
		width: 6px;
	}

	.muij-live-selectors-list::-webkit-scrollbar-track {
		background: transparent;
	}

	.muij-live-selectors-list::-webkit-scrollbar-thumb {
		background: var(--border-soft);
		border-radius: 3px;
	}

	.muij-live-selector-entry {
		padding: var(--space-xs) var(--space-md);
		border-bottom: 1px solid var(--border-soft);
		transition: background 0.15s ease;
	}

	.muij-live-selector-entry:last-child {
		border-bottom: none;
	}

	.muij-live-selector-entry:hover {
		background: var(--bg-soft);
	}

	.muij-selector-main {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		flex-wrap: wrap;
	}

	.muij-selector-icon {
		font-size: 0.875rem;
		flex-shrink: 0;
	}

	.muij-selector-type {
		font-size: 0.75rem;
		font-weight: 500;
		color: var(--text-secondary);
		flex-shrink: 0;
	}

	.muij-selector-target {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		background: var(--bg-soft);
		padding: 2px 6px;
		border-radius: var(--radius-sm);
		color: var(--text-body);
		max-width: 200px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-selector-outcome {
		margin-left: auto;
		font-size: 0.75rem;
		flex-shrink: 0;
	}

	.muij-outcome-success {
		color: var(--color-success);
	}

	.muij-outcome-failed {
		color: var(--color-error);
	}

	.muij-selector-meta {
		display: flex;
		gap: var(--space-sm);
		margin-top: 2px;
		padding-left: calc(1rem + var(--space-xs));
		font-size: 0.625rem;
		color: var(--text-muted);
	}

	.muij-selector-dom {
		font-family: var(--font-mono);
	}

	.muij-selector-time {
		opacity: 0.7;
	}

	.muij-live-selectors-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		padding: var(--space-lg);
		color: var(--text-muted);
	}

	.muij-live-selectors-empty-icon {
		font-size: 1.5rem;
		opacity: 0.5;
		margin-bottom: var(--space-xs);
	}

	.muij-live-selectors-empty-text {
		font-size: 0.75rem;
	}
</style>
