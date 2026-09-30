<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	interface NotificationAction {
		id: string;
		label: string;
		disabled?: boolean;
	}

	interface NormalizedNotificationAction {
		key: string;
		id: string;
		label: string;
		disabled: boolean;
	}

	export let title: string = '';
	export let body: string = '';
	export let actions: NotificationAction[] = [];

	const dispatch = createEventDispatcher<{ action: { id: string } }>();
	$: safeActions = normalizeActions(actions);

	function asTrimmedString(value: unknown): string {
		if (typeof value === 'string') return value.trim();
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value).trim();
		}
		return '';
	}

	function normalizeActions(input: NotificationAction[]): NormalizedNotificationAction[] {
		if (!Array.isArray(input)) return [];
		const seen = new Set<string>();
		const normalized: NormalizedNotificationAction[] = [];
		for (let index = 0; index < input.length; index++) {
			const candidate = input[index] as unknown;
			if (candidate == null || typeof candidate !== 'object' || Array.isArray(candidate)) continue;
			const rec = candidate as Record<string, unknown>;
			const id = asTrimmedString(rec.id);
			const label = asTrimmedString(rec.label ?? id);
			if (!label) continue;
			const effectiveId = id || `action-${index + 1}`;
			if (seen.has(effectiveId)) continue;
			seen.add(effectiveId);
			normalized.push({
				key: `${effectiveId}:${index}`,
				id: effectiveId,
				label,
				disabled: rec.disabled === true
			});
		}
		return normalized;
	}

	function triggerAction(actionId: string): void {
		dispatch('action', { id: actionId });
	}
</script>

<div class="muij-notification" role="status" aria-live="polite">
	{#if title.trim().length > 0}
		<div class="muij-notification-title">{title}</div>
	{/if}
	{#if body.trim().length > 0}
		<div class="muij-notification-body">{body}</div>
	{/if}
	{#if safeActions.length > 0}
		<div class="muij-notification-actions">
			{#each safeActions as action (action.key)}
				<button
					type="button"
					class="muij-notification-action"
					on:click={() => triggerAction(action.id)}
					disabled={!!action.disabled}
				>
					{action.label}
				</button>
			{/each}
		</div>
	{/if}
</div>

<style>
	.muij-notification {
		display: grid;
		gap: 6px;
		padding: 10px;
		border-radius: var(--radius-md);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
	}

	.muij-notification-title {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.muij-notification-body {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
		overflow-wrap: anywhere;
	}

	.muij-notification-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.muij-notification-action {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-xs);
		background: var(--bg-soft);
		color: var(--text-body);
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		padding: 4px 8px;
		cursor: pointer;
	}

	.muij-notification-action:disabled {
		opacity: 0.55;
		cursor: default;
	}
</style>
