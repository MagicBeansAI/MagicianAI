<script lang="ts">
	// Bright Sidekick opens the October brand: More You, one star sidekick,
	// then the existing proof, montage, trust, manifesto and ask.
	// BrandReveal, LandingHero, Declaration, Greeting, and MovieTrack stay
	// on disk, unmounted.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { installScopedApiFetch } from '$lib/stores/scopeIdentityStore';
	import LandingAskComposer from '$lib/landing/LandingAskComposer.svelte';

	import LandingChrome from '$lib/landing/LandingChrome.svelte';
	import GitHubLink from '$lib/landing/GitHubLink.svelte';
	import LandingField from '$lib/landing/LandingField.svelte';
	import HeroSplit from '$lib/landing/HeroSplit.svelte';
	import HowTrack from '$lib/landing/HowTrack.svelte';
	import DayTrack from '$lib/landing/DayTrack.svelte';
	import LifeTrack from '$lib/landing/LifeTrack.svelte';
	import TrustReveal from '$lib/landing/TrustReveal.svelte';
	import ManifestoExcerpt from '$lib/landing/ManifestoExcerpt.svelte';
	import HowItWorks from '$lib/landing/HowItWorks.svelte';
	import FilmDialog from '$lib/landing/FilmDialog.svelte';
	import { EXPLAIN_URL } from '$lib/landing/explain';
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
	const staticMarketing = import.meta.env.VITE_MARKETING_BUILD === 'true';
	let backendStatus: 'checking' | 'available' | 'unavailable' = staticMarketing ? 'unavailable' : 'checking';
	let dark = false;
	const setupRequestSubject = 'Magican setup request';
	const setupRequestBody = [
		'Hi,',
		'',
		"I'd like help setting up Magican.",
		'',
		"Here's what I'd like to use Magican for:",
		'[Add your task, project, or side quest.]',
		'',
		'My device(s): [Add your devices.]',
		'',
		'Thanks,',
		'[Your name]'
	].join('\r\n');
	const setupRequestHref = `mailto:reach.magican@gmail.com?subject=${encodeURIComponent(setupRequestSubject)}&body=${encodeURIComponent(setupRequestBody)}`;
	$: actionLabel = backendStatus === 'available' ? 'Try Magican' : 'Request setup';
	$: actionHref = backendStatus === 'available'
		? '/chat'
		: backendStatus === 'unavailable' ? setupRequestHref : '#landing-cta';

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
		if (!staticMarketing) checkBackendStatus();
	});

</script>
<div class="lp-root" data-appearance={dark ? 'night' : 'day'}>
	<LandingChrome {actionLabel} {actionHref} {dark} on:appearance={() => (dark = !dark)} />
	<HeroSplit {actionLabel} {actionHref} {dark} />
	<FilmDialog />

	<!-- Power this personal must answer to you arrives right after the first
	     viewport: before the day begins, establishing vault secrets, chosen
	     models and human control as the foundational prerequisite. -->
	<TrustReveal />

	<DayTrack />

	<!-- Explore Magican: seven deep-dive feature stations over the dusk-town
	     backdrop. -->
	<LandingField>
		<HowTrack />
	</LandingField>

	<!-- Go do what you love: the montage animation with people living their
	     lives while devices work untouched, bracketed by "Love what you do?"
	     and "Go. Do what you love." -->
	<LifeTrack />

	<!-- Trust asks the reader to believe the claims; this shows the machine
	     behind them, opened up, and hands the curious a live model of it. -->
	<HowItWorks {dark} />

	<ManifestoExcerpt />

	<!-- What it takes comes BEFORE the ask now. The earlier order followed the
	     usual rule — do not put friction in front of the call to action — but
	     that rule is written for a purchase. This ask is "write to a human for
	     access", and these are two requirements framed as honesty rather
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
		     static setup-request CTA instead. -->
		{#if backendStatus === 'available'}
			<LandingAskComposer
				bind:value={query}
				{taking}
				onSubmit={handleSubmit}
			/>
		{:else if backendStatus === 'unavailable'}
			<div class="lp-marketing-cta" role="status">
				<span>Setup help</span>
				<p>Tell me what you’d like to use Magican for and which devices you use.</p>
				<a href={setupRequestHref}>Request setup →</a>
			</div>
		{/if}
	</section>


	<footer class="lp-footer">
		<span class="lp-footer-tag">More You</span>
		<nav class="lp-footer-nav" aria-label="Footer">
			{#if backendStatus === 'available'}
				<a href="/today">Today</a>
				<a href="/chat">Chat</a>
				<a href="/work">Tasks</a>
				<a href="/work?view=crew">Crew</a>
			{/if}
			<GitHubLink />
			<a href={EXPLAIN_URL}>How it works</a>
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

	/* October brand, scoped to this page. Appearance never writes <html>'s
	   data-theme or the visitor's saved application preferences.
	   Keep the literal palette in sync with warmPalette.ts. */
	.lp-root {
		--accent-primary: #1a1612;
		--accent-primary-soft: rgba(26, 22, 18, 0.08);
		--accent-secondary: #61506f;
		--bg-base: #faf8f5;
		--bg-card: #ffffff;
		--bg-elevated: #ffffff;
		--bg-soft: #f0ebf6;
		--border-default: rgba(26, 22, 18, 0.18);
		--border-soft: rgba(26, 22, 18, 0.1);
		--border-subtle: var(--border-soft);
		--button-primary-bg: #1a1612;
		--button-primary-color: var(--text-on-accent);
		--font-mono: 'Geist Mono', 'JetBrains Mono', 'Fira Code', monospace;
		--font-primary: 'Outfit', system-ui, -apple-system, sans-serif;
		--font-display: 'Outfit', system-ui, -apple-system, sans-serif;
		--input-focus-shadow: 0 0 0 4px rgba(26, 22, 18, 0.12);
		--landing-bg: #faf8f5;
		--landing-backdrop-filter: grayscale(35%) saturate(0.6) brightness(1.55);
		--landing-backdrop-opacity: 0.24;
		--landing-blob-1: radial-gradient(circle, rgba(255, 201, 60, 0.16), transparent 70%);
		--landing-blob-2: radial-gradient(circle, rgba(189, 161, 240, 0.15), transparent 70%);
		--landing-blob-3: radial-gradient(circle, rgba(255, 198, 176, 0.2), transparent 70%);
		--landing-blob-blend: normal;
		--landing-chip-bg: #ffffff;
		--landing-chip-border: rgba(26, 22, 18, 0.18);
		--landing-form-border: rgba(26, 22, 18, 0.18);
		--landing-form-surface: rgba(255, 255, 255, 0.92);
		--landing-header-border: rgba(26, 22, 18, 0.1);
		--landing-input-surface: #ffffff;
		--landing-subtitle: #49444f;
		--landing-task-card-shadow: none;
		--landing-task-line: transparent;
		--landing-title-gradient: linear-gradient(90deg, #1a1612, #1a1612);
		--landing-nav-height: 4.5rem;
		--radius-full: 9999px;
		--radius-lg: 16px;
		--radius-md: 10px;
		--shadow-lg: 0 16px 40px -12px rgba(26, 22, 18, 0.18);
		--shadow-md: 0 8px 24px -8px rgba(26, 22, 18, 0.14);
		--shadow-sm: 0 2px 6px rgba(26, 22, 18, 0.05);
		--text-faint: #66606d;
		--text-ink: #1a1612;
		--text-muted: #66606d;
		--text-on-accent: #faf8f5;
		--text-primary: #1a1612;
		--text-secondary: #49444f;
		--brand-gold: #ffc93c;
		--brand-gold-ink: #1a1612;
		--brand-spark-lilac: #bda1f0;
		--brand-signal-primary: #8a6100;
		--brand-signal-secondary: #715092;
		--brand-lilac: #e7e5fc;
		--brand-peach: #ffe4d3;
		--brand-mascot-wash: rgba(255, 228, 211, 0.75);
		--brand-mascot-filter: none;
		--lp-font: 'Outfit', var(--font-primary, system-ui), sans-serif;
		--lp-mono: 'Geist Mono', ui-monospace, 'SF Mono', monospace;
		--lp-primary: var(--accent-primary);
		--lp-secondary: var(--accent-secondary);
		--lp-accent-wash: color-mix(in srgb, var(--lp-primary) 10%, transparent);
		position: relative;
		background: var(--landing-bg);
		color: var(--text-primary);
		min-height: 100svh;
		overflow-x: clip;
		color-scheme: light;
	}
	.lp-root[data-appearance='night'] {
		--accent-primary: #ffc93c;
		--accent-primary-soft: rgba(255, 201, 60, 0.12);
		--accent-secondary: #eadcff;
		--bg-base: #191b22;
		--bg-card: #242731;
		--bg-elevated: #242731;
		--bg-soft: #30313c;
		--border-default: rgba(250, 248, 245, 0.22);
		--border-soft: rgba(250, 248, 245, 0.12);
		--button-primary-bg: #ffc93c;
		--input-focus-shadow: 0 0 0 4px rgba(255, 201, 60, 0.2);
		--landing-bg: #191b22;
		--landing-backdrop-filter: grayscale(20%) saturate(0.65) brightness(0.7);
		--landing-backdrop-opacity: 0.28;
		--landing-blob-1: radial-gradient(circle, rgba(255, 201, 60, 0.08), transparent 70%);
		--landing-blob-2: radial-gradient(circle, rgba(189, 161, 240, 0.1), transparent 70%);
		--landing-blob-3: radial-gradient(circle, rgba(255, 198, 176, 0.07), transparent 70%);
		--landing-chip-bg: #242731;
		--landing-chip-border: rgba(250, 248, 245, 0.2);
		--landing-form-border: rgba(250, 248, 245, 0.2);
		--landing-form-surface: rgba(36, 39, 49, 0.92);
		--landing-header-border: rgba(250, 248, 245, 0.12);
		--landing-input-surface: #242731;
		--landing-subtitle: #dedbe5;
		--landing-title-gradient: linear-gradient(90deg, #faf8f5, #faf8f5);
		--text-faint: #c0b8c8;
		--text-ink: #faf8f5;
		--text-muted: #c0b8c8;
		--text-on-accent: #1a1612;
		--text-primary: #faf8f5;
		--text-secondary: #dedbe5;
		--brand-lilac: #33374f;
		--brand-peach: #594627;
		--brand-spark-lilac: #d3b9ff;
		--brand-signal-primary: #ffc93c;
		--brand-signal-secondary: #d3b9ff;
		--brand-mascot-wash: rgba(255, 201, 60, 0.1);
		--brand-mascot-filter: drop-shadow(0 0 1px rgba(255, 228, 211, 0.4));
		color-scheme: dark;
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
	.lp-footer-tag {
		font-family: var(--lp-font);
		font-weight: 500;
		font-style: normal;
	}
	.lp-footer-nav {
		display: flex;
		align-items: center;
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
