<script lang="ts">
	// A still of the anatomy explorer (showcase/anatomy, published at
	// explain.magican.ai) that opens the live model in a new tab. The model is
	// a full-screen WebGL page with its own gestures and narration, so the
	// landing shows a picture of it rather than embedding it.
	import { EXPLAIN_URL } from './explain';
	export let dark = false;
</script>

<section class="hw" id="landing-how-it-works" aria-labelledby="how-it-works-title">
	<div class="hw-copy">
		<p class="hw-kicker">How it works</p>
		<h2 id="how-it-works-title">The whole thing, opened up.</h2>
		<p>
			The Magician server, every app and device that talks to it, and the wiring between them, built
			from the code itself. Break any box open down to a single type, or follow a request through a
			narrated tour.
		</p>
		<p class="hw-actions">
			<a class="hw-primary" href="{EXPLAIN_URL}/?tour" target="_blank" rel="noreferrer">Take the tour <span aria-hidden="true">↗</span></a>
			<a class="hw-secondary" href={EXPLAIN_URL} target="_blank" rel="noreferrer">Explore it yourself</a>
		</p>
	</div>
	<a class="hw-shot" href={EXPLAIN_URL} target="_blank" rel="noreferrer" aria-label="Open the interactive model of Magican in a new tab">
		<img
			src={dark ? '/landing/explain-teaser-night.webp' : '/landing/explain-teaser-sorbet.webp'}
			width="1360"
			height="820"
			alt="Magican as an opened case: the Magician server in the middle, its processes and crates inside, and the desktop, web, phone, ESP32 and Kindle clients around it, wired to the server."
			loading="lazy"
			decoding="async"
		/>
	</a>
</section>

<style>
	.hw {
		max-width: 74rem;
		margin: 0 auto;
		padding: clamp(3rem, 8vh, 5.5rem) clamp(1rem, 4vw, 2.5rem);
		display: grid;
		grid-template-columns: minmax(16rem, 1fr) minmax(0, 1.7fr);
		gap: clamp(1.5rem, 4vw, 3.5rem);
		align-items: center;
		font-family: var(--lp-font);
		color: var(--text-primary);
	}

	.hw-kicker {
		margin: 0 0 0.7rem;
		color: var(--landing-subtitle, var(--text-secondary));
		font-size: 0.78rem;
		letter-spacing: 0.04em;
	}

	.hw h2 {
		margin: 0 0 1.1rem;
		font-size: clamp(1.9rem, 4.2vw, 2.8rem);
		font-weight: 400;
		letter-spacing: -0.03em;
		line-height: 1.12;
		text-wrap: balance;
	}

	.hw-copy > p:not(.hw-kicker):not(.hw-actions) {
		margin: 0;
		color: var(--text-secondary);
		line-height: 1.6;
		text-wrap: pretty;
	}

	.hw-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem 1.2rem;
		align-items: center;
		margin: 1.6rem 0 0;
	}

	.hw-primary {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.7rem 1.1rem;
		border-radius: 999px;
		background: var(--lp-primary, var(--accent-primary));
		color: var(--text-on-accent, #fff);
		text-decoration: none;
		font-weight: 500;
	}

	.hw-secondary {
		color: var(--lp-primary, var(--accent-primary));
		text-decoration: none;
	}

	.hw-primary:hover,
	.hw-secondary:hover {
		text-decoration: underline;
	}

	.hw-shot {
		display: block;
		border-radius: 1rem;
		overflow: hidden;
		border: 1px solid var(--border-soft);
		box-shadow: 0 24px 60px -36px rgb(0 0 0 / 0.45);
		transition: transform 0.25s ease, box-shadow 0.25s ease;
	}

	.hw-shot:hover {
		transform: translateY(-2px);
		box-shadow: 0 30px 70px -36px rgb(0 0 0 / 0.5);
	}

	.hw-shot img {
		display: block;
		width: 100%;
		height: auto;
	}

	@media (max-width: 820px) {
		.hw {
			grid-template-columns: 1fr;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.hw-shot,
		.hw-shot:hover {
			transition: none;
			transform: none;
		}
	}
</style>
