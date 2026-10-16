<script lang="ts">
	import { onMount } from 'svelte';

	import { showError } from '$lib/shared/stores/notifications';
	import { mediaPreferencesStore, saveMediaPreferences } from './preferences';
	import SurfaceAudioProfileControl from './SurfaceAudioProfileControl.svelte';
	import type { AudioSurface } from './audioSettings';

	const SURFACES: AudioSurface[] = ['dictation', 'meeting', 'listening', 'hands_free'];

	let savingSpeak = $state(false);

	onMount(() => {
		void mediaPreferencesStore.refresh().catch(() => null);
	});

	async function setAutoSpeak(event: Event): Promise<void> {
		const checked = (event.target as HTMLInputElement).checked;
		savingSpeak = true;
		try {
			await saveMediaPreferences({ auto_speak: checked });
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Failed to save auto-speak');
		} finally {
			savingSpeak = false;
		}
	}
</script>

<section class="shared-voice" aria-labelledby="shared-voice-title">
	<div>
		<p class="eyebrow">Shared with every surface</p>
		<h2 id="shared-voice-title">Voice preferences</h2>
		<p>
			Chat, Observe, and the Orb read these choices from the backend. Engine availability and the
			configured default chain stay in Surface profiles above. A change here saves immediately.
		</p>
	</div>

	<label class="speak">
		<input
			type="checkbox"
			role="switch"
			checked={$mediaPreferencesStore.preferences.auto_speak}
			disabled={savingSpeak || !$mediaPreferencesStore.resolved}
			aria-label="Auto-speak assistant replies"
			onchange={setAutoSpeak}
		/>
		<span>Auto-speak assistant replies</span>
	</label>

	<div class="surfaces">
		{#each SURFACES as surface (surface)}
			<SurfaceAudioProfileControl {surface} showStages={true} />
		{/each}
	</div>
</section>

<style>
	.shared-voice {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: grid;
		gap: 0.95rem;
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
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
		margin: 0.2rem 0 0.35rem;
	}

	p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.45;
		margin: 0;
	}

	.speak {
		align-items: center;
		display: flex;
		gap: 0.6rem;
		font-size: 0.92rem;
	}

	.surfaces {
		display: grid;
		gap: 1rem;
		grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
	}
</style>
