<script lang="ts">
	// The processing-locality switch (`privacy.processing.mode`) for Settings →
	// Local models: on-device models or their cloud counterparts for the
	// operations that have both.
	import { onMount } from 'svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		fetchProcessingSettings,
		setProcessingMode,
		type ProcessingMode,
		type ProcessingSettings
	} from './privacyProcessing';

	let settings = $state<ProcessingSettings | null>(null);
	let error = $state<string | null>(null);
	let saving = $state(false);

	const onDevice = $derived(settings ? settings.routing.filter((row) => !row.switched).length : 0);
	const inCloud = $derived(settings ? settings.routing.filter((row) => row.switched).length : 0);

	async function load(): Promise<void> {
		try {
			settings = await fetchProcessingSettings();
			error = null;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Processing settings are unavailable.';
		}
	}

	async function choose(mode: ProcessingMode): Promise<void> {
		if (!settings || saving || settings.mode === mode) return;
		if (mode === 'cloud') {
			const confirmed = await requestConfirmation({
				title: 'Process in the cloud?',
				message:
					'Operations that can run on this machine will send their content to cloud models instead. Embeddings stay on device.',
				confirmLabel: 'Use cloud',
				cancelLabel: 'Keep on device'
			});
			if (!confirmed) return;
		}
		saving = true;
		try {
			settings = await setProcessingMode(mode);
			error = null;
			showSuccess(mode === 'local' ? 'Processing on device.' : 'Processing in the cloud.');
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'The processing mode could not be saved.';
			error = message;
			showError(message);
		} finally {
			saving = false;
		}
	}

	onMount(() => {
		void load();
	});
</script>

<div class="processing" data-testid="processing-mode">
	<div class="processing-text">
		<strong>Processing</strong>
		<span>
			On device keeps local-eligible work on this machine's models; Cloud sends it to their cloud
			counterparts. Embeddings always stay on device. Applies at once, no restart.
		</span>
		{#if settings && settings.routing.length > 0}
			<span class="counts" aria-live="polite">
				{onDevice} on device · {inCloud} in the cloud
			</span>
		{/if}
		{#if error}<span class="error" role="alert">{error}</span>{/if}
		{#each settings?.warnings ?? [] as warning (warning)}<span class="warning">{warning}</span>{/each}
	</div>
	<div class="mode-switch" role="radiogroup" aria-label="Processing">
		<button
			type="button"
			role="radio"
			aria-checked={settings?.mode === 'local'}
			class:active={settings?.mode === 'local'}
			disabled={!settings || saving}
			onclick={() => void choose('local')}
		>
			On device
		</button>
		<button
			type="button"
			role="radio"
			aria-checked={settings?.mode === 'cloud'}
			class:active={settings?.mode === 'cloud'}
			disabled={!settings || saving}
			onclick={() => void choose('cloud')}
		>
			Cloud
		</button>
	</div>
	{#if settings && settings.routing.length > 0}
		<details class="routing">
			<summary>What this changes</summary>
			<ul>
				{#each settings.routing as row (row.operation)}
					<li>
						<span class="operation">{row.operation.replaceAll('_', ' ')}</span>
						<span class="where" class:cloud={row.switched}>{row.switched ? 'Cloud' : 'On device'}</span>
						<code>{row.provider ? `${row.provider} · ` : ''}{row.profile}</code>
					</li>
				{/each}
			</ul>
		</details>
	{/if}
</div>

<style>
	.processing {
		align-items: start;
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: grid;
		gap: 0.5rem 1rem;
		grid-template-columns: minmax(0, 1fr) auto;
		padding: 0.75rem 0.85rem;
	}

	.processing-text {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		min-width: 0;
	}

	.processing-text strong {
		color: var(--text-primary);
		font-size: 0.88rem;
		font-weight: 600;
	}

	.processing-text span {
		color: var(--text-secondary);
		font-size: 0.8rem;
		line-height: 1.45;
	}

	.processing-text .counts {
		color: var(--text-primary);
		font-weight: 600;
	}

	.processing-text .error {
		color: var(--danger, #c2410c);
	}

	.processing-text .warning {
		color: var(--warning, #b7791f);
	}

	/* A two-option segmented switch in the theme's accent. */
	.mode-switch {
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		display: inline-flex;
		overflow: hidden;
	}

	.mode-switch button {
		background: transparent;
		border: 0;
		color: var(--text-secondary);
		cursor: pointer;
		font: inherit;
		font-size: 0.8125rem;
		font-weight: 600;
		min-height: 2rem;
		padding: 0.35rem 0.85rem;
	}

	.mode-switch button + button {
		border-left: 1px solid var(--border-soft);
	}

	.mode-switch button.active {
		background: var(--accent-fill);
		color: var(--accent-on-primary, #fff);
	}

	.mode-switch button:disabled {
		cursor: not-allowed;
	}

	.mode-switch button:disabled:not(.active) {
		opacity: 0.58;
	}

	.mode-switch button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 70%, transparent);
		outline-offset: -2px;
	}

	.routing {
		grid-column: 1 / -1;
	}

	.routing summary {
		color: var(--text-primary);
		cursor: pointer;
		font-size: 0.8rem;
		font-weight: 600;
	}

	.routing ul {
		display: grid;
		gap: 0.3rem;
		list-style: none;
		margin: 0.5rem 0 0;
		padding: 0;
	}

	.routing li {
		align-items: baseline;
		display: grid;
		font-size: 0.8rem;
		gap: 0.6rem;
		grid-template-columns: minmax(8rem, 14rem) 5.5rem minmax(0, 1fr);
	}

	.routing .operation {
		color: var(--text-primary);
		text-transform: capitalize;
	}

	.routing .where {
		color: var(--text-secondary);
		font-weight: 600;
	}

	.routing .where.cloud {
		color: var(--accent-primary);
	}

	.routing code {
		color: var(--text-secondary);
		font-size: 0.75rem;
		overflow-wrap: anywhere;
	}

	@media (max-width: 640px) {
		.processing {
			grid-template-columns: minmax(0, 1fr);
		}

		.routing li {
			grid-template-columns: minmax(0, 1fr) auto;
		}

		.routing code {
			grid-column: 1 / -1;
		}
	}
</style>
