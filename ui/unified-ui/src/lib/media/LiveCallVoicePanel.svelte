<script lang="ts">
	import { onMount } from 'svelte';
	import VoicePersonaPicker from './VoicePersonaPicker.svelte';

	import { mediaPreferencesStore } from './preferences';
	import { mediaProvidersStore } from './providers';

	$: profiles = ($mediaProvidersStore.realtime_voice_profiles ?? []).filter(
		(profile) => profile.mode !== 'translation' && (profile.voices?.length ?? 0) > 0
	);
	$: catalogReady = $mediaProvidersStore.resolved;
	onMount(() => {
		void mediaProvidersStore.refresh();
		void mediaPreferencesStore.refresh();
	});

</script>

{#if !catalogReady || profiles.length > 0}
	<section class="live-voices" aria-labelledby="live-call-voices-title">
		<div class="header">
			<p class="eyebrow">Live call</p>
			<h2 id="live-call-voices-title">Engine voices</h2>
			<p>
				Choose the spoken voice for each realtime model. This applies to the
				next call; it does not change the engine itself.
			</p>
		</div>
		<div class="rows">
			{#if !catalogReady && profiles.length === 0}
				<p>Loading live engines…</p>
			{/if}
			{#each profiles as profile (profile.profile_id)}
				<VoicePersonaPicker kind="realtime" bindingId={profile.profile_id}
					label={profile.label} defaultVoice={profile.voice} choices={profile.voices ?? []}
					disabled={!profile.available} />
			{/each}
		</div>
	</section>
{/if}

<style>
	.live-voices {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: grid;
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
	.rows {
		display: grid;
		gap: 0.75rem;
	}
</style>
