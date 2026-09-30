<script lang="ts">
	export let type: 'info' | 'warning' | 'error' | 'success' = 'info';
	export let message: string = '';
	export let closable: boolean = false;
	export let updateToken: unknown = undefined;

	let closed = false;
	let alertSignature = '';
	let lastUpdateToken: unknown = undefined;
	$: alertType = ['warning', 'error', 'success'].includes(type) ? type : 'info';
	$: {
		const nextSignature = `${alertType}:${closable ? '1' : '0'}:${message}`;
		const tokenChanged = updateToken !== lastUpdateToken;
		if (tokenChanged || nextSignature !== alertSignature) {
			alertSignature = nextSignature;
			lastUpdateToken = updateToken;
			closed = false;
		}
	}

	function closeAlert(): void {
		closed = true;
	}
</script>

{#if !closed}
	<div class={`muij-alert muij-alert-${alertType}`} role="status" aria-live="polite">
		<span class="muij-alert-message">{message}</span>
		{#if closable}
			<button type="button" class="muij-alert-close" on:click={closeAlert} aria-label="Dismiss alert">×</button>
		{/if}
	</div>
{/if}

<style>
	.muij-alert {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 10px;
		padding: 8px 10px;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		font-family: var(--font-primary);
		font-size: 0.75rem;
	}

	.muij-alert-info {
		background: color-mix(in srgb, var(--accent-primary) 10%, white);
		color: #1e3a8a;
	}

	.muij-alert-success {
		background: #ecfdf5;
		color: #065f46;
	}

	.muij-alert-warning {
		background: #fffbeb;
		color: #92400e;
	}

	.muij-alert-error {
		background: #fef2f2;
		color: #991b1b;
	}

	.muij-alert-message {
		overflow-wrap: anywhere;
	}

	.muij-alert-close {
		border: none;
		background: transparent;
		color: inherit;
		font-size: 1rem;
		line-height: 1;
		cursor: pointer;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-alert {
		border-radius: 0;
		font-family: var(--font-mono);
		background: var(--bg-base);
		color: var(--text-primary);
		border: 2px solid var(--text-primary);
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-alert::before {
		content: '[!]';
		margin-right: 8px;
		font-weight: bold;
	}
</style>
