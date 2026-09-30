<script lang="ts">
	import { onMount } from 'svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import {
		fetchLocalGeneration,
		switchLocalGeneration,
		type LocalGenerationEnvelope,
		type LocalGenerationModel
	} from '$lib/stores/localGenerationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';

	let envelope = $state<LocalGenerationEnvelope | null>(null);
	let error = $state<string | null>(null);
	let loading = $state(false);
	let switching = $state(false);
	let draftId = $state('');

	const selectedModel = $derived.by(() => {
		const current = envelope;
		if (!current) return null;
		const wanted = draftId || current.selected;
		return current.models.find((model) => model.id === wanted) ?? null;
	});
	const currentId = $derived(envelope?.selected ?? null);
	const dirty = $derived(Boolean(draftId && draftId !== currentId));

	onMount(() => {
		void refresh(false);
	});

	async function refresh(showToast: boolean): Promise<void> {
		loading = true;
		error = null;
		try {
			envelope = await fetchLocalGeneration();
			draftId = envelope.selected ?? '';
			if (showToast) showSuccess('Local generation models loaded');
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to load local generation';
			error = message;
			if (showToast) showError(message);
		} finally {
			loading = false;
		}
	}

	function percent(value: number | null): string {
		return value == null ? '—' : `${Math.round(value)}%`;
	}

	function gb(value: number | null | undefined): string {
		return value == null ? '—' : `${value} GB`;
	}

	function hostLine(data: LocalGenerationEnvelope): string {
		const free = data.host.free_memory_gb != null ? `, ${data.host.free_memory_gb} GB free` : '';
		return `${data.host.memory_gb} GB ${data.host.arch} ${data.host.os}${free}`;
	}

	function recommendedLabel(data: LocalGenerationEnvelope): string {
		const model = data.models.find((item) => item.id === data.recommended);
		if (model) return `${model.label} (${model.id})`;
		if (data.host.memory_gb > 0 && data.host.memory_gb < data.min_memory_gb) {
			return `none — below the ${data.min_memory_gb} GB floor`;
		}
		return 'none';
	}

	function confirmMessage(model: LocalGenerationModel, hostWarnings: string[]): string {
		const lines = [
			...hostWarnings,
			...model.warnings,
			model.installed === false
				? `${model.label} is not installed. Magician will pin it anyway; install with make setup-local-generation MODEL=${model.id}.`
				: null,
			'The switch is allowed. Ollama will reload after the pin is written.'
		].filter((line): line is string => Boolean(line));
		return lines.join('\n\n');
	}

	async function applySwitch(): Promise<void> {
		if (!envelope || !draftId) return;
		const model = envelope.models.find((item) => item.id === draftId);
		if (!model) return;
		const needsConfirm = !model.rule_ok || model.installed === false;
		if (needsConfirm) {
			const confirmed = await requestConfirmation({
				title: model.rule_ok ? `Pin ${model.label}?` : `${model.label} is outside the RAM rule`,
				message: confirmMessage(model, envelope.warnings),
				confirmLabel: 'Switch anyway',
				cancelLabel: 'Cancel'
			});
			if (!confirmed) return;
		}

		switching = true;
		error = null;
		try {
			const result = await switchLocalGeneration(draftId, true);
			envelope = result;
			draftId = result.selected ?? '';
			const parts = [`Pinned ${result.selected ?? draftId}`];
			if (result.ollama_reloaded) parts.push('Ollama reloaded');
			if (result.ollama_error) parts.push(`Ollama reload failed: ${result.ollama_error}`);
			if (result.reload_error) parts.push(`Config reload failed: ${result.reload_error}`);
			if (result.rule_violated) parts.push('RAM-tier rule is still violated');
			if (result.ollama_error || result.reload_error) {
				showError(parts.join('. '));
			} else {
				showSuccess(parts.join('. '));
			}
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to switch local generation';
			error = message;
			showError(message);
		} finally {
			switching = false;
		}
	}

	async function reloadCurrent(): Promise<void> {
		if (!currentId) return;
		draftId = currentId;
		await applySwitch();
	}
</script>

<section class="card" aria-labelledby="local-generation-title">
	<div class="header">
		<div>
			<p class="overline">Local models</p>
			<h2 id="local-generation-title">On-device generation</h2>
			<p>
				Pick the kitty model every local Ollama profile uses. Switching rewrites
				<code>runtime.ollama.local_generation.selected</code> and reloads Ollama. RAM-tier
				mismatches warn, then still allow the pin. JSON workers stay <code>think:false</code>.
			</p>
		</div>
		<div class="actions">
			<button type="button" disabled={loading || switching} onclick={() => void refresh(true)}>
				{loading ? 'Refreshing…' : 'Refresh'}
			</button>
			<button
				class="secondary"
				type="button"
				disabled={loading || switching || !currentId}
				onclick={() => void reloadCurrent()}
			>
				{switching && !dirty ? 'Reloading…' : 'Reload Ollama'}
			</button>
			<button
				class="primary"
				type="button"
				disabled={loading || switching || !dirty}
				onclick={() => void applySwitch()}
			>
				{switching && dirty
					? 'Switching…'
					: selectedModel && !selectedModel.rule_ok
						? 'Switch anyway and reload'
						: 'Switch and reload Ollama'}
			</button>
		</div>
	</div>

	{#if error}
		<p class="alert error" role="alert">{error}</p>
	{/if}

	{#if envelope}
		<dl class="meta">
			<div>
				<dt>This machine</dt>
				<dd>{hostLine(envelope)}</dd>
			</div>
			<div>
				<dt>Auto-setup would pick</dt>
				<dd>{recommendedLabel(envelope)}</dd>
			</div>
			<div>
				<dt>Current pin</dt>
				<dd>{envelope.selected ?? 'none'}</dd>
			</div>
			<div>
				<dt>Processing</dt>
				<dd>{envelope.processing_mode}</dd>
			</div>
		</dl>

		{#if envelope.warnings.length}
			<div class="alert warning" role="status">
				{#each envelope.warnings as warning (warning)}
					<p>{warning}</p>
				{/each}
			</div>
		{/if}

		<div class="models" role="radiogroup" aria-label="Local generation model">
			{#each envelope.models as model (model.id)}
				<label class="model" class:selected={draftId === model.id} class:active={model.selected} class:violates={!model.rule_ok}>
					<input
						type="radio"
						name="local-generation-model"
						value={model.id}
						bind:group={draftId}
						disabled={loading || switching}
						aria-label={model.label}
					/>
					<div class="model-copy">
						<div class="model-title">
							<strong>{model.label}</strong>
							<code>{model.id}</code>
						</div>
						<div class="chips">
							{#if model.selected}<span class="chip">Current</span>{/if}
							{#if model.recommended}<span class="chip recommended">Recommended</span>{/if}
							{#if !model.rule_ok}<span class="chip warn">Outside RAM rule</span>{/if}
							{#if model.installed === false}<span class="chip warn">Not installed</span>{/if}
							{#if model.installed === true}<span class="chip">Installed</span>{/if}
						</div>
						<p class="stats">
							{gb(model.resident_gb)} resident · {model.min_memory_gb} GB+ auto-pick · classify
							{percent(model.classify_agree_pct)} · distill {percent(model.distill_recall_pct)} · browser
							{model.browser_effect ?? '—'}
						</p>
						{#if model.warnings.length && draftId === model.id}
							<ul class="model-warnings">
								{#each model.warnings as warning (warning)}
									<li>{warning}</li>
								{/each}
							</ul>
						{/if}
					</div>
				</label>
			{/each}
		</div>
	{:else if loading}
		<p class="empty">Loading local generation models…</p>
	{:else}
		<p class="empty">Refresh to load the local generation kitty.</p>
	{/if}
</section>

<style>
	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1rem;
	}
	.header {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}
	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0;
		margin: 0;
		text-transform: uppercase;
	}
	h2,
	p {
		letter-spacing: 0;
		margin: 0;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
	}
	.header p,
	.empty {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
	}
	code {
		font-family: var(--font-mono);
		font-size: 0.84em;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.65rem;
	}
	button {
		align-items: center;
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.86rem;
		font-weight: 600;
		justify-content: center;
		line-height: 1.2;
		min-height: 2.25rem;
		padding: 0.55rem 0.8rem;
	}
	button.primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--accent-on-primary, #fff);
	}
	button.secondary {
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}
	.meta {
		display: grid;
		gap: 0.75rem 1rem;
		grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
		margin: 0;
	}
	.meta dt {
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}
	.meta dd {
		color: var(--text-primary);
		font-size: 0.88rem;
		line-height: 1.4;
		margin: 0.15rem 0 0;
		overflow-wrap: anywhere;
	}
	.alert {
		border-radius: 8px;
		font-size: 0.88rem;
		line-height: 1.45;
		padding: 0.75rem 0.85rem;
	}
	.alert p {
		color: inherit;
		font-size: inherit;
	}
	.alert p + p {
		margin-top: 0.4rem;
	}
	.alert.error {
		background: color-mix(in srgb, var(--danger, #c2410c) 10%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--danger, #c2410c) 30%, var(--border-soft));
		color: var(--text-primary);
	}
	.alert.warning {
		background: color-mix(in srgb, var(--warning, #b7791f) 12%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--warning, #b7791f) 34%, var(--border-soft));
		color: var(--text-primary);
	}
	.models {
		display: grid;
		gap: 0.75rem;
	}
	.model {
		align-items: flex-start;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		cursor: pointer;
		display: flex;
		gap: 0.75rem;
		padding: 0.85rem;
	}
	.model.selected {
		border-color: color-mix(in srgb, var(--accent-primary) 55%, var(--border-soft));
		box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent-primary) 35%, transparent);
	}
	.model.violates.selected {
		border-color: color-mix(in srgb, var(--warning, #b7791f) 55%, var(--border-soft));
	}
	.model input {
		accent-color: var(--accent-primary);
		margin-top: 0.2rem;
	}
	.model-copy {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: 0.35rem;
		min-width: 0;
	}
	.model-title {
		align-items: baseline;
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
	}
	.model-title strong {
		color: var(--text-primary);
		font-size: 0.98rem;
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
	}
	.chip {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-soft));
		border-radius: 999px;
		color: var(--text-primary);
		font-size: 0.72rem;
		font-weight: 700;
		padding: 0.15rem 0.5rem;
	}
	.chip.recommended {
		background: color-mix(in srgb, var(--accent-primary) 22%, var(--bg-soft));
	}
	.chip.warn {
		background: color-mix(in srgb, var(--warning, #b7791f) 22%, var(--bg-soft));
	}
	.stats,
	.model-warnings {
		color: var(--text-secondary);
		font-size: 0.8rem;
		line-height: 1.45;
		margin: 0;
	}
	.model-warnings {
		padding-left: 1.1rem;
	}
	@media (min-width: 960px) {
		.header {
			align-items: flex-start;
			flex-direction: row;
			justify-content: space-between;
		}
		.actions {
			flex-shrink: 0;
			justify-content: flex-end;
			max-width: 28rem;
		}
	}
</style>
