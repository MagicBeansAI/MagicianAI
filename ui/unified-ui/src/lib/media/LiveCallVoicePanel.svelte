<script lang="ts">
	import { onMount } from 'svelte';

	import { mediaPreferencesStore, saveMediaPreferences } from './preferences';
	import { mediaProvidersStore } from './providers';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';

	$: profiles = ($mediaProvidersStore.realtime_voice_profiles ?? []).filter(
		(profile) => profile.mode !== 'translation' && (profile.voices?.length ?? 0) > 0
	);
	$: selected = $mediaPreferencesStore.preferences.realtime_voices ?? {};
	$: catalogReady = $mediaProvidersStore.resolved;
	let savingProfile = '';

	onMount(() => {
		void mediaProvidersStore.refresh();
		void mediaPreferencesStore.refresh();
	});

	async function chooseVoice(profileId: string, voice: string): Promise<void> {
		savingProfile = profileId;
		try {
			await saveMediaPreferences({
				realtime_voices: { [profileId]: voice || 'default' }
			});
			showSuccess('Live call voice saved');
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Could not save live call voice');
		} finally {
			savingProfile = '';
		}
	}
</script>

{#if !catalogReady || profiles.length > 0}
	<section class="live-voices" aria-labelledby="live-call-voices-title">
		<div class="header">
			<p class="eyebrow">Live call</p>
			<h2 id="live-call-voices-title">Engine voices</h2>
			<p>
				Choose the spoken voice for GPT Realtime, Gemini Live, and GPT Live 1. This applies to the
				next call; it does not change the engine itself.
			</p>
		</div>
		<div class="rows">
			{#if !catalogReady && profiles.length === 0}
				<p>Loading live engines…</p>
			{/if}
			{#each profiles as profile (profile.profile_id)}
				<label>
					<span>{profile.label}</span>
					<select
						value={selected[profile.profile_id] ?? ''}
						disabled={!profile.available || savingProfile === profile.profile_id}
						on:change={(event) =>
							void chooseVoice(profile.profile_id, (event.currentTarget as HTMLSelectElement).value)}
					>
						<option value="">Provider default{profile.voice ? ` (${profile.voice})` : ''}</option>
						{#each profile.voices ?? [] as voice (voice.id)}
							<option value={voice.id}>{voice.label}</option>
						{/each}
					</select>
				</label>
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
	label {
		display: grid;
		gap: 0.32rem;
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
