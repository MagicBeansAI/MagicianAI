<script lang="ts">
	// A ROAD, OPENED. The fork's three continuations used to unfold inline, in
	// the page's own scroll, which cost them three things:
	//
	//   · a pinned, scroll-scrubbed track inside document flow fights the
	//     page's scroll and the fork's own sticky switcher. Its own
	//     full-viewport scroller has neither to fight, which is what lets the
	//     day (and, in time, the other two) run as real scrubbed film here.
	//   · closing puts the visitor back exactly where they were. The whole
	//     "every road must end by handing the ask back or it strands anyone
	//     who scrolled past the CTA" contract existed ONLY because the roads
	//     were inline; a modal cannot strand anyone.
	//   · the film's closing marker can open a road directly instead of
	//     scrolling somewhere else first.
	//
	// `data-scrub-root` is the contract with `scrub`: any track mounted inside
	// this element measures against IT rather than the window. The geometry is
	// unchanged — a scroller's client box is the viewport for its contents, and
	// sticky sticks to it — but scroll events do not bubble from elements, so
	// the scrubber has to listen to the thing that actually moves.
	import { onDestroy } from 'svelte';

	export let open = false;
	export let label = '';
	export let onClose: () => void = () => {};

	let panelEl: HTMLElement | null = null;
	let scrollEl: HTMLElement | null = null;
	/** Whatever had focus when the road opened, so closing can hand it back. */
	let opener: HTMLElement | null = null;
	let lockedY = 0;

	function lock(): void {
		if (typeof document === 'undefined') return;
		lockedY = window.scrollY;
		// position:fixed rather than overflow:hidden — the latter leaves iOS
		// scrolling the page behind the dialog, which is the whole failure
		// mode a scroll lock exists to prevent.
		document.body.style.position = 'fixed';
		document.body.style.top = `-${lockedY}px`;
		document.body.style.left = '0';
		document.body.style.right = '0';
	}

	function unlock(): void {
		if (typeof document === 'undefined') return;
		document.body.style.position = '';
		document.body.style.top = '';
		document.body.style.left = '';
		document.body.style.right = '';
		if (typeof window.scrollTo === 'function') window.scrollTo(0, lockedY);
	}

	// TRANSITIONS ONLY. This block re-runs whenever ANY of its reactive
	// dependencies change, not just `open` — and re-entering the open branch
	// while already open re-ran `lock()`, which captured `window.scrollY`
	// AFTER the body was already `position: fixed`. That reads 0, so the
	// remembered position was thrown away and closing dumped the visitor at
	// the top of the page. Latch it.
	let wasOpen = false;
	$: if (typeof document !== 'undefined' && open !== wasOpen) {
		wasOpen = open;
		if (open) {
			opener = (document.activeElement as HTMLElement) ?? null;
			lock();
			// The road always opens at its beginning, even when it is being
			// re-opened: a film that resumes halfway is a film nobody chose.
			queueMicrotask(() => {
				// Feature-detected rather than assumed: `scrollTo` is not on
				// every Element implementation (jsdom has none), and an
				// unguarded call throws out of a microtask where nothing is
				// listening for it.
				if (scrollEl) {
					if (typeof scrollEl.scrollTo === 'function') scrollEl.scrollTo(0, 0);
					else scrollEl.scrollTop = 0;
				}
				panelEl?.focus?.();
			});
		} else {
			unlock();
			opener?.focus?.();
			opener = null;
		}
	}

	function onKey(e: KeyboardEvent): void {
		if (e.key === 'Escape') {
			e.stopPropagation();
			onClose();
		}
	}

	onDestroy(() => {
		if (open) unlock();
	});
</script>

<svelte:window on:keydown={open ? onKey : undefined} />

{#if open}
	<!-- The road is mounted only while it is open, so an unopened one costs
	     nothing — no track, no canvases, no rAF. -->
	<div class="rm" role="presentation">
		<button class="rm-scrim" type="button" tabindex="-1" aria-hidden="true" on:click={onClose}
		></button>
		<!-- A <div>, not a <section>: a section carries its own implicit role
		     and cannot take `dialog`. -->
		<div
			class="rm-panel"
			role="dialog"
			aria-modal="true"
			aria-label={label}
			tabindex="-1"
			bind:this={panelEl}
		>
			<button class="rm-close" type="button" on:click={onClose}>
				<span aria-hidden="true">←</span> close
			</button>
			<div class="rm-scroll" data-scrub-root bind:this={scrollEl}>
				<slot />
			</div>
		</div>
	</div>
{/if}

<style>
	.rm {
		position: fixed;
		inset: 0;
		z-index: 90;
		display: flex;
	}
	.rm-scrim {
		position: absolute;
		inset: 0;
		border: 0;
		padding: 0;
		background: color-mix(in srgb, var(--text-primary, #101014) 46%, transparent);
		backdrop-filter: blur(3px);
		cursor: pointer;
	}
	/* INSET, so it reads as a window OVER the page rather than as a new page.
	   Full-bleed it was indistinguishable from a navigation — the fork
	   vanished, the road filled the frame, and nothing on screen said this
	   was a thing you had opened and could close. Leaving the scrim and a
	   margin of the page visible on all four sides is the whole difference. */
	.rm-panel {
		position: relative;
		margin: auto;
		width: min(1280px, 94vw);
		height: min(1000px, 92svh);
		border-radius: clamp(12px, 1.4vw, 20px);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		background: var(--landing-bg, var(--bg-base, #fff));
		box-shadow: 0 40px 90px -30px rgba(10, 12, 20, 0.5);
		outline: none;
		overflow: hidden;
	}
	/* THE SCROLL ROOT. A road's track sizes itself in svh and pins its stage;
	   both resolve against this box, not the window, which is the entire
	   reason a scrubbed film can run in here at all. */
	.rm-scroll {
		height: 100%;
		overflow-y: auto;
		overflow-x: hidden;
		overscroll-behavior: contain;
		border-radius: inherit;
	}
	.rm-close {
		position: absolute;
		top: clamp(0.7rem, 2vh, 1.2rem);
		left: clamp(0.7rem, 2vw, 1.4rem);
		z-index: 2;
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.42rem 0.85rem;
		border-radius: 999px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		background: color-mix(in srgb, var(--landing-bg, #fff) 82%, transparent);
		backdrop-filter: blur(8px);
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.72rem;
		letter-spacing: 0.12em;
		color: var(--text-primary);
		cursor: pointer;
	}
	.rm-close:hover {
		background: var(--landing-bg, #fff);
	}
	@media (prefers-reduced-motion: reduce) {
		.rm-scrim {
			backdrop-filter: none;
		}
	}
</style>
