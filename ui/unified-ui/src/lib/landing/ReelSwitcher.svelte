<script lang="ts">
	// The film selector — page chrome, beside the theme control.
	//
	// Act 0 has two cuts (landingReel.ts): the photoreal film and the
	// miniature-diorama film shot to the same storyboard. Which one plays
	// used to be an authoring decision with deliberately no visitor-facing
	// switch; the 2026-08-04 owner call reversed that, so the choice now sits
	// in the corner next to the theme, is remembered, and takes effect
	// immediately — MovieTrack flows the new reel to PrologueReel, which
	// re-cuts to the new directory without a reload.
	//
	// A two-position segmented control rather than a one-button toggle: with
	// exactly two films, showing both means the visitor can see which one is
	// playing without hovering, and the pill sits at the theme button's own
	// 32px height so the corner reads as one cluster.
	import { onMount } from 'svelte';

	import { chooseReel, REELS, initReelChoice, selectedReel } from './landingReel';

	const films = Object.values(REELS);

	// The chrome may mount before or after MovieTrack; both settle the same
	// choice through the same idempotent call, so neither has to be first.
	onMount(initReelChoice);
</script>

<div class="rs" role="group" aria-label="Prologue film">
	{#each films as film (film.id)}
		<button
			class="rs-btn"
			class:on={$selectedReel.id === film.id}
			type="button"
			aria-pressed={$selectedReel.id === film.id}
			aria-label="{film.name} film"
			title="{film.name} film"
			on:click={() => chooseReel(film)}
		>
			{#if film.id === 'photoreal'}
				<!-- A lens with its glint: the film that was shot. -->
				<svg
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.8"
					stroke-linecap="round"
					stroke-linejoin="round"
					aria-hidden="true"
				>
					<circle cx="12" cy="12" r="8.5" />
					<circle cx="12" cy="12" r="3.4" />
					<circle cx="7.8" cy="7.8" r="1.3" fill="currentColor" stroke="none" />
				</svg>
			{:else}
				<!-- A block, drawn as a made object: the film that was BUILT.
				     The first sketch was a framed landscape, and at 16px that is
				     indistinguishable from a generic image icon — which beside a
				     lens says "photo" twice instead of saying shot vs made. -->
				<svg
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.8"
					stroke-linecap="round"
					stroke-linejoin="round"
					aria-hidden="true"
				>
					<path d="M12 2.6l8.4 4.7v9.4L12 21.4l-8.4-4.7V7.3z" />
					<path d="M3.6 7.3L12 12l8.4-4.7" />
					<path d="M12 12v9.4" />
				</svg>
			{/if}
		</button>
	{/each}
</div>

<style>
	/* Sized to the theme button, not merely near it: 26px cells inside 2px of
	   padding inside a 1px border is exactly its 32px, with the same 8px
	   radius and the same transparent-until-touched surface. */
	.rs {
		display: inline-flex;
		align-items: center;
		gap: 2px;
		padding: 2px;
		border: 1px solid transparent;
		border-radius: 8px;
	}

	.rs-btn {
		width: 26px;
		height: 26px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		background: transparent;
		border: none;
		border-radius: 6px;
		color: var(--text-secondary, #555);
		cursor: pointer;
		transition:
			background 650ms ease,
			color 650ms ease;
	}

	.rs-btn:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
	}

	/* The house's own "this one is chosen" treatment, borrowed from the theme
	   dropdown's active row: accent ink on an accent-soft bed. Which is also
	   why the unchosen cell is NOT dimmed — it carries the theme button's own
	   ink weight, so the three glyphs read as one row and the selection reads
	   as colour, not as one icon being half switched off. */
	.rs-btn.on {
		background: var(--accent-primary-soft, rgba(158, 89, 255, 0.14));
		color: var(--accent-primary, #9e59ff);
	}

	.rs-btn:focus-visible {
		outline: 2px solid var(--accent-primary, #9e59ff);
		outline-offset: 1px;
	}
</style>
