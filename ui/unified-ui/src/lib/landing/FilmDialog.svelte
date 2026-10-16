<script lang="ts">
	// The intro film in a full-screen player. It opens whenever the address is
	// #film: the hero's "Watch the film" link, a shared magican.ai/#film link,
	// or Back/Forward onto it. Nothing from YouTube loads until then, and the
	// player is removed on close so playback stops. On desktop and Android the
	// player also asks for true full screen; iOS Safari does not allow that
	// for an embedded frame, so there the video takes the screen when it plays
	// (playsinline=0) and the overlay already fills the viewport.
	import { onMount } from 'svelte';
	import { replaceState } from '$app/navigation';
	import { FILM_HASH, FILM_TITLE, filmEmbedUrl } from './film';

	let dialog: HTMLDialogElement;
	let open = false;

	function show(): void {
		if (open) return;
		open = true;
		dialog.showModal();
		// still inside the click that changed the hash, which browsers require
		dialog.requestFullscreen?.().catch(() => {});
	}

	function hide(): void {
		if (!open) return;
		open = false;
		if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
		if (dialog.open) dialog.close();
		if (location.hash === FILM_HASH) replaceState(location.pathname + location.search, {});
	}

	const sync = (): void => (location.hash === FILM_HASH ? show() : hide());

	onMount(() => {
		sync();
		window.addEventListener('hashchange', sync);
		return () => window.removeEventListener('hashchange', sync);
	});
</script>

<dialog
	bind:this={dialog}
	class="film"
	aria-label={FILM_TITLE}
	on:close={hide}
	on:click={(event) => event.target === dialog && hide()}
>
	<button type="button" class="film-close" aria-label="Close the film" on:click={hide}>✕</button>
	{#if open}
		<div class="film-frame">
			<iframe
				src={filmEmbedUrl()}
				title={FILM_TITLE}
				allow="autoplay; encrypted-media; picture-in-picture; fullscreen"
				allowfullscreen
				referrerpolicy="strict-origin-when-cross-origin"
			></iframe>
		</div>
	{/if}
</dialog>

<style>
	.film {
		width: 100vw;
		height: 100dvh;
		max-width: none;
		max-height: none;
		margin: 0;
		padding: clamp(0.75rem, 3vw, 2.5rem);
		border: 0;
		background: #000;
		color: #fff;
		display: none;
		place-items: center;
	}

	.film[open] {
		display: grid;
	}

	.film::backdrop {
		background: rgb(0 0 0 / 0.92);
	}

	.film-frame {
		width: min(100%, calc((100dvh - 5rem) * 16 / 9));
		aspect-ratio: 16 / 9;
	}

	.film-frame iframe {
		width: 100%;
		height: 100%;
		border: 0;
		border-radius: 0.6rem;
		background: #000;
	}

	.film-close {
		position: absolute;
		top: max(0.75rem, env(safe-area-inset-top));
		right: max(0.75rem, env(safe-area-inset-right));
		width: 2.75rem;
		height: 2.75rem;
		border: 1px solid rgb(255 255 255 / 0.25);
		border-radius: 999px;
		background: rgb(255 255 255 / 0.08);
		color: #fff;
		font-size: 1rem;
		cursor: pointer;
	}

	.film-close:hover {
		background: rgb(255 255 255 / 0.18);
	}
</style>
