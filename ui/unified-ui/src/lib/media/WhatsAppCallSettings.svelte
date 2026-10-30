<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import {
		decideWhatsAppRing,
		emptyWhatsAppRing,
		fetchWhatsAppRing,
		whatsAppEngineChoices,
		type WhatsAppRingView
	} from '$lib/observe/api';

	import { mediaProvidersStore } from './providers';

	let ring: WhatsAppRingView = emptyWhatsAppRing();
	let missing = false;
	let busy = false;
	let note: string | null = null;
	let timer: ReturnType<typeof setInterval> | null = null;

	$: engines = whatsAppEngineChoices(
		$mediaProvidersStore.realtime_voice_profiles,
		ring.profiles,
		ring.profile
	);
	$: locked =
		missing ||
		busy ||
		ring.phase === 'approved' ||
		ring.phase === 'listening' ||
		ring.phase === 'leaving';
	$: hint = missing
		? 'This Magician process does not handle WhatsApp calls yet.'
		: ring.phase === 'approved' || ring.phase === 'listening' || ring.phase === 'leaving'
			? 'Magician is on a call. This engine applies to the next one. Leave the call on Observe.'
			: 'Chrome microphone stays BlackHole 16ch. Speakers stay the Mac speakers. Join, Listen, and Leave stay on Observe.';

	async function load(): Promise<void> {
		try {
			const view = await fetchWhatsAppRing();
			missing = view == null;
			if (view && !busy) {
				ring = view;
				note = view.error;
			}
		} catch (error) {
			note = error instanceof Error ? error.message : 'Could not load WhatsApp call settings';
		}
	}

	async function onProfile(event: Event): Promise<void> {
		const select = event.currentTarget;
		if (!(select instanceof HTMLSelectElement) || locked) return;
		busy = true;
		note = null;
		try {
			ring = await decideWhatsAppRing({ decision: 'profile', profile: select.value });
			missing = false;
			note = ring.error;
		} catch (error) {
			note = error instanceof Error ? error.message : 'Could not save the WhatsApp engine';
		} finally {
			busy = false;
		}
	}

	onMount(() => {
		void mediaProvidersStore.refresh();
		void load();
		timer = setInterval(() => void load(), 2000);
	});

	onDestroy(() => {
		if (timer) clearInterval(timer);
	});
</script>

<section class="whatsapp-call-settings" aria-labelledby="whatsapp-call-settings-title">
	<div class="header">
		<p class="eyebrow">WhatsApp</p>
		<h2 id="whatsapp-call-settings-title">Call engine</h2>
		<p>
			The engine that talks on a WhatsApp call in this Chrome. Same models as the composer Call
			menu. The choice applies to the next call.
		</p>
	</div>
	<label>
		<span>Realtime model</span>
		<select
			aria-label="WhatsApp realtime model"
			value={ring.profile}
			disabled={locked}
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
	<p>{hint}</p>
	{#if note}<p class="warn">{note}</p>{/if}
</section>

<style>
	.whatsapp-call-settings {
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
	.warn {
		color: var(--color-error);
	}
</style>
