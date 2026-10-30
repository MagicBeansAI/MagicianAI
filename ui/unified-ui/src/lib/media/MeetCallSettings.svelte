<script lang="ts">
	import { onMount } from 'svelte';

	import { whatsAppEngineChoices } from '$lib/observe/api';

	import { meetVoiceProfileStore } from './meetVoiceProfile';
	import { mediaProvidersStore } from './providers';

	$: engines = whatsAppEngineChoices(
		$mediaProvidersStore.realtime_voice_profiles,
		[],
		$meetVoiceProfileStore
	);

	function onProfile(event: Event): void {
		const select = event.currentTarget;
		if (!(select instanceof HTMLSelectElement)) return;
		meetVoiceProfileStore.set(select.value);
	}

	onMount(() => {
		void mediaProvidersStore.refresh();
	});
</script>

<section class="meet-call-settings" aria-labelledby="meet-call-settings-title">
	<div class="header">
		<p class="eyebrow">Google Meet</p>
		<h2 id="meet-call-settings-title">Call engine</h2>
		<p>
			The engine that talks when Magician joins a Google Meet. Same models as the composer Call
			menu. The choice applies to the next join.
		</p>
	</div>
	<label>
		<span>Realtime model</span>
		<select
			aria-label="Meet realtime model"
			value={$meetVoiceProfileStore}
			on:change={onProfile}
		>
			{#each engines as engine (engine.id)}
				<option
					value={engine.id}
					disabled={!engine.available}
					title={engine.unavailable_reason ?? ''}
				>
					{engine.label}{engine.available ? '' : ' (unavailable)'}
				</option>
			{/each}
		</select>
	</label>
	<p>The realtime model hears the call and takes the turns. Dictation and hands-free are not used here.</p>
</section>

<style>
	.meet-call-settings {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 0.85rem;
		padding: 1rem;
	}
	.eyebrow {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		margin: 0;
		text-transform: uppercase;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		margin: 0.2rem 0 0.35rem;
	}
	p {
		color: var(--text-secondary);
		font-size: 0.82rem;
		line-height: 1.45;
		margin: 0;
	}
	label {
		display: grid;
		gap: 0.32rem;
		grid-template-columns: minmax(0, 1fr);
	}
	label > span {
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 650;
	}
	select {
		background: var(--input-bg, var(--bg-soft));
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.86rem;
		height: 2.25rem;
		max-width: 28rem;
		padding: 0 0.65rem;
	}
</style>
