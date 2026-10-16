<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let label: string = '';
	export let ariaLabel: string = '';
	export let checked: boolean = false;
	export let disabled: boolean = false;

	const dispatch = createEventDispatcher<{ change: { checked: boolean } }>();
	let localChecked = checked;
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Toggle';

	$: if (checked !== localChecked) {
		localChecked = checked;
	}

	function toggle(): void {
		if (disabled) return;
		localChecked = !localChecked;
		dispatch('change', { checked: localChecked });
	}
</script>

<button
	type="button"
	class="muij-toggle"
	class:muij-toggle-on={localChecked}
	on:click={toggle}
	disabled={disabled}
	role="switch"
	aria-checked={localChecked}
	aria-label={!label ? resolvedAriaLabel : undefined}
>
	<span class="muij-toggle-thumb"></span>
	<span class="muij-toggle-label">{label}</span>
</button>

<style>
	.muij-toggle {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		border: none;
		background: transparent;
		padding: 0;
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
	}

	.muij-toggle-thumb {
		position: relative;
		width: 32px;
		height: 18px;
		background: var(--border-soft);
		border-radius: 999px;
		transition: background 160ms ease;
	}

	.muij-toggle-thumb::after {
		content: '';
		position: absolute;
		left: 2px;
		top: 2px;
		width: 14px;
		height: 14px;
		background: #fff;
		border-radius: 999px;
		transition: transform 160ms ease;
	}

	.muij-toggle-on .muij-toggle-thumb {
		background: var(--accent-primary);
	}

	.muij-toggle-on .muij-toggle-thumb::after {
		transform: translateX(14px);
	}

	.muij-toggle:disabled {
		opacity: 0.55;
		cursor: default;
	}
</style>
