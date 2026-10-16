<script lang="ts">
	// The cream reading surface behind every long-form document on the
	// marketing side: /manifesto, /privacy, /terms. The palette below used to
	// be copied into the manifesto route "by design (YAGNI: no shared token
	// file)" — that held while there was one document. Three is a token file.
	//
	// `legal` opts a document into the section typography (h2/p/ul) that the
	// policy pages need. The manifesto renders its own blocks and must not
	// inherit those rules, so they are gated behind the class rather than
	// applied to `.doc` outright.
	export let title: string;
	export let date = '';
	export let legal = false;
</script>

<div class="lp-root">
	<article class="doc" class:legal>
		<a class="back" href="/">Magican</a>
		{#if date}
			<p class="date">{date}</p>
		{/if}
		<h1>{title}</h1>
		<slot />
	</article>
</div>

<style>
	.lp-root {
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
		/* app.css heads use --font-display; pin it so the visitor's stored
		   theme cannot leak a different face onto this document. */
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
	}

	.doc {
		max-width: 40rem;
		margin: 0 auto;
		padding: clamp(2.25rem, 7vh, 4.5rem) clamp(1.25rem, 4vw, 2.5rem) clamp(4.5rem, 12vh, 7rem);
		font-family: var(--lp-font);
	}

	.back {
		display: inline-block;
		margin: 0 0 clamp(2.4rem, 6vh, 4rem);
		color: var(--text-primary);
		font-size: 0.95rem;
		font-weight: 450;
		letter-spacing: -0.015em;
		text-decoration: none;
	}

	.back:hover {
		color: var(--lp-primary);
	}

	.date {
		margin: 0 0 0.75rem;
		color: var(--landing-subtitle);
		font-size: 0.8rem;
		letter-spacing: 0.04em;
	}

	.doc h1 {
		margin: 0 0 2rem;
		font-size: clamp(2.5rem, 6.4vw, 4rem);
		font-weight: 400;
		letter-spacing: -0.035em;
		line-height: 1.1;
		text-wrap: balance;
	}

	.doc h1::after {
		content: '';
		display: block;
		width: 2.4rem;
		height: 1px;
		margin-top: 1.35rem;
		background: currentColor;
		opacity: 0.28;
	}

	/* Slotted markup is compiled in the route, so these rules must be global.
	   They are namespaced under .legal so the manifesto's own block styles
	   stay untouched. */
	.legal :global(p),
	.legal :global(li) {
		font-size: clamp(1.02rem, 1.7vw, 1.1rem);
		line-height: 1.66;
	}

	.legal :global(p) {
		margin: 0 0 1.35rem;
	}

	.legal :global(.lede) {
		margin-bottom: 2.4rem;
		color: var(--text-secondary);
		font-size: clamp(1.1rem, 2vw, 1.24rem);
		line-height: 1.55;
	}

	.legal :global(h2) {
		margin: 2.9rem 0 1rem;
		font-size: clamp(1.15rem, 2.2vw, 1.3rem);
		font-weight: 500;
		letter-spacing: -0.02em;
		line-height: 1.25;
	}

	.legal :global(ul) {
		margin: 0 0 1.35rem;
		padding-left: 1.15rem;
		list-style: none;
	}

	.legal :global(li) {
		position: relative;
		margin: 0 0 0.6rem;
	}

	.legal :global(li)::before {
		content: '—';
		position: absolute;
		left: -1.15rem;
		color: var(--landing-subtitle);
	}

	.legal :global(strong) {
		font-weight: 600;
	}

	/* Body links only. The .back link is DocShell's own chrome and keeps the
	   quiet manifesto treatment; a descendant selector on bare `a` would
	   outrank .back's rule and underline it. */
	.legal :global(p a),
	.legal :global(li a) {
		color: var(--lp-primary);
		text-decoration: none;
		border-bottom: 1px solid color-mix(in srgb, var(--lp-primary) 35%, transparent);
	}

	.legal :global(p a:hover),
	.legal :global(li a:hover) {
		border-bottom-color: var(--lp-primary);
	}
</style>
