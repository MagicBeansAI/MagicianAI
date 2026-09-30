<script lang="ts">
	// Magican landing: dusk-town field, centred Magican lockup, a day of
	// proof, montage, trust, manifesto excerpt, what it takes, one ask.
	// BrandReveal, LandingHero, Declaration, Greeting, and MovieTrack stay
	// on disk, unmounted.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { installScopedApiFetch } from '$lib/stores/scopeIdentityStore';
	import LandingAskComposer from '$lib/landing/LandingAskComposer.svelte';

	import LandingChrome from '$lib/landing/LandingChrome.svelte';
	import LandingField from '$lib/landing/LandingField.svelte';
	import HeroSplit from '$lib/landing/HeroSplit.svelte';
	import HowTrack from '$lib/landing/HowTrack.svelte';
	import DayTrack from '$lib/landing/DayTrack.svelte';
	import LifeTrack from '$lib/landing/LifeTrack.svelte';
	import TrustReveal from '$lib/landing/TrustReveal.svelte';
	import ManifestoExcerpt from '$lib/landing/ManifestoExcerpt.svelte';
	import WhatItTakes from '$lib/landing/WhatItTakes.svelte';
	import {
		LANDING_BACKEND_PROBE_TIMEOUT_MS,
		probeLandingBackend
	} from '$lib/landing/backendCapability';

	// The landing route lives outside (app)'s layout, so the scoped-fetch
	// patch (bearer auth on /api/magician/) must be installed
	// here too — and at init, not onMount, so it is in place before any
	// child component's own mount can fire a fetch through the composer
	// path. Browser-guarded and idempotent inside the helper.
	installScopedApiFetch();

	let query = '';
	let taking = false;
	let backendStatus: 'checking' | 'available' | 'unavailable' = 'checking';

	function markBackendUnavailable(): void {
		if (typeof window !== 'undefined') {
			(window as unknown as Record<string, unknown>).__MAGICIAN_MISSING__ = true;
		}
		backendStatus = 'unavailable';
	}

	function markBackendAvailable(): void {
		if (typeof window !== 'undefined') {
			delete (window as unknown as Record<string, unknown>).__MAGICIAN_MISSING__;
		}
		backendStatus = 'available';
	}

	// Capability, not hostname, decides whether this is an app landing or a
	// brochure. A real deployment may proxy Magician on a public hostname; a
	// static marketing host may return its HTML fallback with HTTP 200. The
	// health probe validates Magician's JSON identity, not merely the status or
	// content type, and remains bounded even when dependent services are offline.
	async function checkBackendStatus(): Promise<void> {
		if (typeof window === 'undefined') return;
		const available = await probeLandingBackend(
			fetch,
			AbortSignal.timeout(LANDING_BACKEND_PROBE_TIMEOUT_MS)
		);
		if (available) markBackendAvailable();
		else markBackendUnavailable();
	}

	async function handleSubmit(): Promise<void> {
		const description = query.trim();
		if (!description || taking) return;
		await checkBackendStatus();
		if (backendStatus !== 'available') return;
		// The press-then-go beat: the input carries `view-transition-name:
		// command-ask`, so navigation morphs it into the chat composer
		// (pair in app.css). Reduced motion skips the pause and just goes.
		if (!window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
			taking = true;
			await new Promise((resolve) => setTimeout(resolve, 420));
		}
		goto(`/chat?q=${encodeURIComponent(description)}`);
	}

	onMount(() => {
		checkBackendStatus();
	});

</script>
<svelte:head>
	<title>Magican — Superpowers for Work, Play and all your side quests</title>
	<meta
		name="description"
		content="Magican is superpowers for work, play, and all your side quests — one intelligence that belongs to you."
	/>
	<!-- The hero face is Outfit now (picked from a live six-way comparison at
	     /font-preview, after Bricolage Grotesque — the first repaint, same
	     day as the film cut — was itself reported "not good enough"; see
	     .lp-root's own comment for the full history) — served from the
	     shared Google Fonts stylesheet app.html already links with a
	     preconnect, not a self-hosted file, so there is nothing static to
	     preload here. `GeistVariable.woff2`'s preload was removed when
	     Bricolage first replaced it as --lp-font and stays removed: nothing
	     mounted on this route asks for non-mono Geist. Geist Mono is still
	     self-hosted and still used for --lp-mono — see the @font-face below
	     — it just never needed a preload of its own (small mono labels,
	     none of them above the fold at first paint). -->
</svelte:head>

<div class="lp-root">
	<!-- Magican top-left after the hero is left; Manifesto top-right from the start. -->
	<LandingChrome />

	<LandingField>
		<HeroSplit />
		<HowTrack />
	</LandingField>

	<DayTrack />

	<!-- THE CLOSING MONTAGE, closing: devices working while people live, only
	     once the day has actually earned it. It used to sit second, right
	     after the hero, with nothing behind it yet — a closing argument
	     opening the film.

	     THE PAYOFF NOW BRACKETS IT (2026-08-17). `Greeting`'s standalone
	     swap ("Love what you do." <-> "Do what you love.") used to sit
	     after TrustReveal, paying off only once trust had been earned.
	     It is retired from that position — `LifeTrack.svelte` now asks
	     "Love what you do?" itself, once, right as the montage opens, lets
	     it fade before the footage takes over, and answers with "Go. Do
	     what you love." once the montage has run long enough to have
	     earned it, lively for as long as the reel keeps playing under it.
	     See LifeTrack.svelte's own header comment. `Greeting.svelte` stays
	     on disk, unmounted — deletion is a separate pass. -->
	<LifeTrack />


	<!-- The hook raises exactly one objection — "it knows all that?" — and this
	     is where it gets answered. The three promises define the proposition,
	     then the concrete trust mechanisms pay for it. The payoff has already
	     landed, inside the montage above — trust is what's left to earn. -->
	<TrustReveal />

	<ManifestoExcerpt />

	<!-- What it takes comes BEFORE the ask now. The earlier order followed the
	     usual rule — do not put friction in front of the call to action — but
	     that rule is written for a purchase. This ask is "write to a human for
	     access", and these are three requirements framed as honesty rather
	     than obstacles. Read first, they qualify the invitation: this is what
	     it takes, and if that is you, write. Read after, they are small print
	     under a decision already made, and the page ends on a list of
	     conditions instead of on the ask. -->
	<WhatItTakes />

	<!-- The founder section (a headline, a paragraph, and originally a quote)
	     stood here between TrustReveal and the ask. Removed whole,
	     2026-08-17: the ask now follows Trust directly. (The payoff itself
	     moved earlier the same day -- it now lives inside LifeTrack,
	     bracketing the montage, well before Trust even opens.) -->
	<section class="lp-cta" id="landing-cta" aria-label="Ask Magican">
		<h2>Put it to work.</h2>
		<!-- The strongest interaction from the retired landing is deliberately
		     back, without reviving that page's copy or visual system: a recorded
		     run types inside the real input, performs beneath it, and yields the
		     instant the visitor writes. It mounts only after the bounded capability
		     probe proves Magician is reachable; brochure deployments receive the
		     static private-beta CTA instead. -->
		{#if backendStatus === 'available'}
			<LandingAskComposer
				bind:value={query}
				{taking}
				onSubmit={handleSubmit}
			/>
		{:else if backendStatus === 'unavailable'}
			<div class="lp-marketing-cta" role="status">
				<span>Private beta</span>
				<p>This was built around one life. I’m working out how it fits around yours.</p>
				<a href="mailto:reach.magican@gmail.com">Write to me →</a>
			</div>
		{/if}
	</section>


	<footer class="lp-footer">
		<span class="lp-footer-tag">Superpowers for work, play, and all your side quests.</span>
		<nav class="lp-footer-nav" aria-label="Footer">
			{#if backendStatus === 'available'}
				<a href="/today">Today</a>
				<a href="/chat">Chat</a>
				<a href="/tasks">Tasks</a>
				<a href="/crew">Crew</a>
			{/if}
			<a href="/manifesto">Manifesto</a>
			<a href="/privacy">Privacy</a>
			<a href="/terms">Terms</a>
		</nav>
		<span class="lp-footer-mark">magican</span>
	</footer>
</div>

<style>
	/* Geist Mono, self-hosted (static/fonts/geist, OFL beside it) — the
	   landing's one remaining self-hosted face, used only where --lp-mono
	   renders (eyebrow labels, footer nav, the composer's file/stage text).
	   `--lp-font`, the display+body voice, is Outfit (see .lp-root's own
	   comment for why it replaced Bricolage Grotesque, which itself had
	   replaced Geist and Newsreader) — loaded from the shared Google Fonts
	   stylesheet app.html already links, not self-hosted, so it has no
	   @font-face of its own here. Non-mono 'Geist' — the original single
	   display face — is retired from this route entirely; its @font-face
	   and preload went with it. `font-display: swap` keeps first paint on
	   system-ui. */
	@font-face {
		font-family: 'Geist Mono';
		src: url('/fonts/geist/GeistMonoVariable.woff2') format('woff2');
		font-weight: 100 900;
		font-style: normal;
		font-display: swap;
	}

	.lp-root {
		/* Sunday afternoon (phase 3, 2026-08-17): the landing pins ONE
		   opinionated palette regardless of the visitor's stored app theme,
		   without writing `data-theme` anywhere. `themeStore.ts` installs a
		   MutationObserver on `document.documentElement`'s `data-theme`
		   attribute that persists any change it sees to localStorage AND the
		   backend (see `syncTheme`/the observer in
		   `src/lib/shared/stores/themeStore.ts`); setting the attribute here
		   would silently overwrite a real visitor's app theme preference the
		   moment they hit the marketing page. Custom properties inherit
		   instead: every value below is a literal, scoped to `.lp-root`, so
		   every descendant (BrandReveal, ProofSwitcher, LifeTrack, TrustReveal,
		   WhatItTakes, PathFork, …) resolves the pinned palette no
		   matter what theme is active on <html>. No global state is touched.
		   Single source of truth for these values: warmPalette.ts — that file
		   does not generate this block, so keep the two in sync by hand;
		   warmPalette.test.ts is what catches drift. Only the vars the landing
		   subtree actually reads are covered; where a var isn't one of these,
		   its existing var(--x, fallback) chain is untouched.

		   REPAINTED (second pass, same day): this block used to be a literal
		   copy of `[data-theme="longhand"]` in src/app.css — same burnt-rust
		   `--accent-primary` and sage `--accent-secondary`, same Newsreader
		   body serif under a self-hosted Geist headline face. Reported live
		   as "not good at all" — the rust+sage pair read as earthy/artisanal
		   rather than clean, and the Newsreader/Geist split meant
		   `LandingAskComposer` (which reads `--font-primary` directly, not
		   `--lp-font`) rendered in a literary serif while every headline on
		   the page rendered in a technical grotesk, a clash nothing asked
		   for. Both are now the landing's OWN choices, no longer required to
		   match `longhand` (that app theme is untouched — it is still what a
		   signed-in visitor can pick from ThemeSwitcher; this is only ever
		   what the marketing route pins for itself). Ink-blue and deep teal
		   replace rust and sage — a cool, confident pair on the same warm
		   cream ground, chosen so a light `--text-on-accent` still clears
		   4.5:1 sitting on either one (warmPalette.test.ts gates this).

		   TYPE, THIRD PASS (same day): Bricolage Grotesque replaced Geist and
		   Newsreader here first, then was itself reported "not good enough."
		   Six real candidates were compared live at /font-preview (a
		   throwaway route, the page's own copy on the page's own cream
		   ground, one candidate per card) rather than described in prose —
		   Outfit was picked from that comparison. Added to app.html's shared
		   Google Fonts link (`family=Outfit:wght@100..900`) alongside the
		   other theme faces; still a self-contained variable weight axis, so
		   large display text can run genuinely light without faking it on a
		   face not drawn for that weight. */
		--accent-primary: #28437a;
		--accent-primary-soft: rgba(40, 67, 122, 0.1);
		--accent-secondary: #12665a;
		--bg-base: #f3ead6;
		--bg-card: #faf3e0;
		--bg-elevated: #faf3e0;
		--bg-soft: #e3d3ac;
		--border-default: rgba(26, 22, 18, 0.26);
		--border-soft: rgba(26, 22, 18, 0.14);
		--button-primary-bg: #28437a;
		--font-mono: 'Geist Mono', 'JetBrains Mono', 'Fira Code', monospace;
		--font-primary: 'Outfit', system-ui, -apple-system, sans-serif;
		/* app.css has a global `h1,h2,h3,h4,h5,h6 { font-family: var(--font-display) }`
		   rule so every (app) route's headings match its theme without
		   hardcoding a face per page. That rule reaches into the landing
		   subtree too (WhatItTakes' <h2>, any other real heading tag), so
		   --font-display has to be pinned here as well — otherwise a real
		   heading element would keep leaking the VISITOR's stored theme's
		   display face on this one property, even though every other token
		   in this block is pinned specifically to prevent that. Matches
		   --font-primary above; the landing no longer splits body from
		   heading typefaces. (This exact bug — an unpinned --font-display
		   silently overriding a per-element font choice — is also what made
		   every headline in the /font-preview comparison look identical
		   before that page's own <h2> got `font-family: inherit`; same
		   mechanism, two different places it had to be closed.) */
		--font-display: 'Outfit', system-ui, -apple-system, sans-serif;
		--input-focus-shadow: 0 0 0 4px rgba(40, 67, 122, 0.14);
		--landing-bg: #f3ead6;
		--landing-blob-1: radial-gradient(circle, rgba(40, 67, 122, 0.16), transparent 70%);
		--landing-blob-2: none;
		--landing-blob-3: none;
		--landing-blob-blend: multiply;
		--landing-chip-bg: #faf3e0;
		--landing-chip-border: rgba(26, 22, 18, 0.18);
		--landing-form-border: rgba(26, 22, 18, 0.18);
		--landing-form-surface: rgba(250, 243, 224, 0.88);
		--landing-header-border: rgba(26, 22, 18, 0.1);
		--landing-input-surface: #faf3e0;
		--landing-subtitle: rgba(58, 47, 36, 0.78);
		--landing-task-card-shadow: none;
		--landing-task-line: transparent;
		--landing-title-gradient: linear-gradient(
			90deg,
			#1a1612 0%,
			#1a1612 38%,
			#28437a 50%,
			#1a1612 62%,
			#1a1612 100%
		);
		--radius-full: 9999px;
		--radius-lg: 12px;
		--radius-md: 8px;
		--shadow-lg: 0 3px 0 rgba(26, 22, 18, 0.07), 0 14px 32px -10px rgba(26, 22, 18, 0.22);
		--shadow-md: 0 2px 0 rgba(26, 22, 18, 0.06), 0 6px 16px -4px rgba(26, 22, 18, 0.14);
		--shadow-sm: 0 1px 0 rgba(26, 22, 18, 0.06), 0 2px 4px rgba(26, 22, 18, 0.05);
		/* longhand ships `#97836b`, which is 3.04:1 on this ground — under the
		   4.5:1 AA bar, and every use of it in the landing subtree is small
		   text (the composer's placeholder and its mono labels). Darkened to
		   the lightest tone on the same hue ramp that clears AA, so it stays
		   visibly fainter than `--text-muted` (5.62:1) rather than collapsing
		   into it. Scoped here deliberately: the app-wide `longhand` token is
		   left alone, since retuning a shared theme is the owner's call and
		   this is the only surface that has been measured. */
		--text-faint: #695948; /* 5.62:1 on base, 6.07 on elevated, 4.54 on soft */
		--text-ink: #1a1612;
		--text-muted: #6a5944;
		--text-on-accent: #faf3e0;
		--text-primary: #1a1612;
		--text-secondary: #3a2f24;
		--lp-font: 'Outfit', var(--font-primary, system-ui), sans-serif;
		--lp-mono: 'Geist Mono', ui-monospace, 'SF Mono', monospace;
		--lp-primary: var(--accent-primary, #9e59ff);
		--lp-secondary: var(--accent-secondary, var(--lp-primary));
		--lp-accent-wash: color-mix(in srgb, var(--lp-primary) 10%, transparent);
		position: relative;
		background: var(--landing-bg);
		color: var(--text-primary);
		min-height: 100svh;
		overflow-x: clip;
		transition:
			background-color 220ms ease,
			color 220ms ease;
	}

	.lp-cta {
		max-width: 40rem;
		margin: 0 auto;
		padding: clamp(2rem, 6vh, 4rem) clamp(1rem, 4vw, 2rem) clamp(4rem, 10vh, 6rem);
		text-align: center;
		font-family: var(--lp-font);
	}
	.lp-cta h2 {
		font-size: clamp(1.7rem, 3.8vw, 2.4rem);
		font-weight: 420;
		letter-spacing: -0.02em;
		margin: 0 0 1.4rem;
		background: var(--landing-title-gradient, linear-gradient(100deg, var(--text-primary), var(--lp-primary)));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.lp-marketing-cta {
		display: grid;
		justify-items: center;
		gap: 0.55rem;
		padding: clamp(1.25rem, 4vw, 1.8rem);
		border: 1px solid var(--border-default, var(--landing-header-border));
		border-radius: var(--radius-lg, 16px);
		background: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 84%, transparent);
		box-shadow: var(--shadow-md, var(--landing-task-card-shadow));
	}
	.lp-marketing-cta span {
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		font-weight: 650;
		letter-spacing: 0.13em;
		text-transform: uppercase;
		color: var(--lp-primary);
	}
	.lp-marketing-cta p {
		margin: 0;
		color: var(--text-primary);
		font-size: 1rem;
	}
	.lp-marketing-cta a {
		color: var(--lp-primary);
		font-family: var(--lp-mono);
		font-size: 0.76rem;
		font-weight: 600;
		text-decoration: none;
	}
	.lp-marketing-cta a:hover {
		color: var(--lp-secondary);
	}

	.lp-footer {
		display: flex;
		align-items: center;
		gap: 1.2rem;
		flex-wrap: wrap;
		justify-content: space-between;
		max-width: 64rem;
		margin: 0 auto;
		padding: 1.6rem clamp(1rem, 4vw, 2rem) 2.2rem;
		border-top: 1px solid var(--landing-header-border);
		font-family: var(--lp-mono);
		font-size: 0.72rem;
		color: var(--landing-subtitle);
	}
	.lp-footer-nav {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem 1rem;
	}
	.lp-footer-nav a {
		color: var(--landing-subtitle);
		text-decoration: none;
	}
	.lp-footer-nav a:hover {
		color: var(--accent-primary);
	}
	.lp-footer-mark {
		letter-spacing: 0.3em;
		color: var(--lp-primary);
	}

</style>
