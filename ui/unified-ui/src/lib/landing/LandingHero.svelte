<!--
	The root page's declaration.

	The page used to open with a category claim ("Superintelligence + you")
	that any AI company could print. This hero replaces it with a hook that
	makes the visitor supply themselves: a role cycles through the h1, then
	the turn line breaks the pattern the cycle just built. The existing
	scroll film still supplies the proof; this beat now supplies the reason
	to keep reading it.

	Theme fonts are allowed to shape the rest of the page, but this product
	proposition uses the stable brand face so a wide theme cannot make the
	line overflow. Theme colour still owns the quiet light. The page's one
	aurora motif now begins at the turn line instead of the retired `+`
	glyph: it leaves the relationship named here and continues into the
	proof below instead of being rediscovered around every screen.
-->
<script lang="ts">
	import RoleCycle from './RoleCycle.svelte';
	import { ROLES } from './roleCycle';

	// Derived, never hand-written. A screen reader gets the whole proposition
	// at once rather than whichever role happened to be showing at read time,
	// and the list cannot drift out of sync with the cycle the way a
	// duplicated string would — it already had, once.
	const TURN_LINE = 'Your AI has only met one of them.';
	const heroLabel = `You are ${ROLES.join(', ')}. ${TURN_LINE}`;
</script>

<section class="lh" aria-labelledby="landing-hero-title">
	<!-- The first screen was ~70% flat, static paper: one soft motionless
	     blob and a hairline squiggle trailing off into nothing. This layer
	     is the fix — the same drifting aurora technique `Declaration.svelte`
	     uses lower on the page, reused here rather than invented twice, so
	     the page's one motion language starts on screen one instead of
	     three screens down. Pure CSS: no scroll trigger needed (nothing here
	     reveals, it simply lives), so no IntersectionObserver and no new
	     script in a file that currently has none. -->
	<div class="lh-aurora" aria-hidden="true"></div>
	<div class="lh-copy">
		<h1 id="landing-hero-title" aria-label={heroLabel}>
			<span class="lh-title-you" aria-hidden="true">You are <RoleCycle />.</span>
		</h1>
		<p class="lh-turn">
			{TURN_LINE}
			<svg
				class="lh-motif-origin"
				viewBox="-500 0 1000 700"
				preserveAspectRatio="none"
				aria-hidden="true"
			>
				<defs>
					<linearGradient id="lh-motif-paint" x1="0" y1="0" x2="0.58" y2="1">
						<stop offset="0" stop-color="var(--lh-a)" />
						<stop offset="0.58" stop-color="var(--lh-b)" />
						<stop offset="1" stop-color="var(--lh-a)" stop-opacity="0.2" />
					</linearGradient>
				</defs>
				<!-- One loose departure, not an orbit: the film picks up this
				     direction at its top edge and carries it forward. -->
				<path
					class="lh-motif-line"
					pathLength="1"
					d="M 0 0 C 44 72 164 102 142 184 C 116 282 -132 304 -104 424 C -76 542 94 574 74 700"
				/>
				<path
					class="lh-motif-glint"
					pathLength="1"
					d="M 0 0 C 44 72 164 102 142 184 C 116 282 -132 304 -104 424 C -76 542 94 574 74 700"
				/>
			</svg>
		</p>

		<nav class="lh-actions" aria-label="Landing page introduction">
			<a href="#landing-cta">See how <span aria-hidden="true">→</span></a>
		</nav>
	</div>
</section>

<style>
	.lh {
		--lh-a: var(--accent-primary, #9e59ff);
		--lh-b: var(--accent-secondary, var(--lh-a));
		position: relative;
		isolation: isolate;
		min-height: 100svh;
		display: grid;
		place-items: center;
		overflow: hidden;
		padding: clamp(6rem, 13vh, 8.5rem) clamp(1.2rem, 5vw, 4rem) clamp(4rem, 10vh, 7rem);
		font-family: var(--lp-font);
		color: var(--text-primary);
		background: var(--landing-bg);
	}
	/* Three blurred blobs on the hero's own accent pair, drifting on an
	   independent clock — never in step with the role cycle, so it reads as
	   the room's own ambient light rather than a cue tied to any one word. */
	.lh-aurora {
		position: absolute;
		inset: -20% -10%;
		z-index: 0;
		pointer-events: none;
		background:
			radial-gradient(
				40% 46% at 24% 30%,
				color-mix(in srgb, var(--lh-a) 26%, transparent),
				transparent 70%
			),
			radial-gradient(
				36% 42% at 78% 66%,
				color-mix(in srgb, var(--lh-b) 22%, transparent),
				transparent 72%
			),
			radial-gradient(
				32% 36% at 50% 96%,
				color-mix(in srgb, var(--lh-a) 14%, transparent),
				transparent 74%
			);
		filter: blur(52px);
		animation: lh-aurora-drift 26s ease-in-out infinite alternate;
	}
	@keyframes lh-aurora-drift {
		0% {
			transform: translate3d(0, 0, 0) scale(1);
		}
		50% {
			transform: translate3d(-1.5%, 2%, 0) scale(1.05);
		}
		100% {
			transform: translate3d(1.5%, -1.5%, 0) scale(1);
		}
	}
	.lh-copy {
		position: relative;
		z-index: 2;
		display: grid;
		justify-items: center;
		text-align: center;
		width: min(64rem, 100%);
	}
	h1 {
		margin: 0;
		display: grid;
		justify-items: center;
		gap: clamp(0.45rem, 1.5vh, 0.9rem);
		width: 100%;
		font-family: 'Geist', ui-sans-serif, system-ui, sans-serif;
		font-weight: 690;
		line-height: 0.9;
	}
	.lh-title-you {
		max-width: 100%;
		/* `white-space: nowrap`, sized so the LONGEST role ("You are someone
		   who still checks the score.", ~43 characters) still fits one line
		   inside `.lh-copy` (capped at 64rem) at ordinary desktop widths, and
		   inside the viewport minus `.lh`'s side padding at phone widths. The
		   vw coefficient and both clamp bounds are chosen with headroom for
		   that longest string specifically — the short roles have room to
		   spare. `line-height` overrides the h1's tight 0.9, which would
		   overlap a wrapped line if this sizing is ever wrong for a phrase
		   this long. */
		font-size: clamp(0.95rem, 3.8vw, 2.9rem);
		line-height: 1.08;
		letter-spacing: -0.045em;
		white-space: nowrap;
		background: var(
			--landing-title-gradient,
			linear-gradient(104deg, var(--text-primary) 12%, var(--lh-a) 62%, var(--lh-b))
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	/* The turn line: same family and gradient treatment as the title above
	   it, one size down, so the two read as one voice rather than a
	   headline handing off to a caption. This is also the motif's new
	   anchor — it used to depart from the `+` glyph inside the old lockup,
	   which no longer exists, so `position: relative` here is load-bearing
	   for the absolutely-positioned SVG below. */
	.lh-turn {
		position: relative;
		max-width: 34rem;
		margin: clamp(1rem, 3vh, 1.8rem) 0 0;
		font-family: 'Geist', ui-sans-serif, system-ui, sans-serif;
		font-size: clamp(1.1rem, 2.6vw, 1.6rem);
		font-weight: 620;
		letter-spacing: -0.02em;
		line-height: 1.35;
		background: var(
			--landing-title-gradient,
			linear-gradient(104deg, var(--text-primary) 12%, var(--lh-a) 62%, var(--lh-b))
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.lh-motif-origin {
		position: absolute;
		z-index: -1;
		left: 50%;
		top: 54%;
		width: 100vw;
		height: max(55svh, 25rem);
		transform: translateX(-50%);
		overflow: visible;
		pointer-events: none;
	}
	.lh-motif-line,
	.lh-motif-glint {
		fill: none;
		stroke: url(#lh-motif-paint);
		stroke-linecap: round;
		vector-effect: non-scaling-stroke;
	}
	/* Was a 1.2px hairline at 0.56 opacity — in a screenshot it read as a
	   stray pencil mark that wandered off the bottom of the page rather than
	   a deliberate signature stroke. Doubled the weight and the presence: it
	   is the one hand-drawn element on an otherwise typeset screen, and it
	   should read as chosen, not accidental. */
	.lh-motif-line {
		stroke-width: 2.4;
		opacity: 0.8;
		filter: drop-shadow(0 0 9px color-mix(in srgb, var(--lh-a) 62%, transparent));
	}
	.lh-motif-glint {
		stroke-width: 3.2;
		stroke-dasharray: 0.11 0.89;
		stroke-dashoffset: 0;
		opacity: 0.92;
		filter: drop-shadow(0 0 13px var(--lh-b));
		animation: lh-motif-travel 7s linear infinite;
	}
	.lh-actions {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		gap: 0.75rem;
		margin-top: clamp(2rem, 4.5vh, 2.8rem);
	}
	.lh-actions a {
		min-height: 2.9rem;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.55rem;
		border-radius: 999px;
		padding: 0.7rem 1.2rem;
		font-size: 0.86rem;
		font-weight: 660;
		text-decoration: none;
		color: var(--text-on-accent, white);
		background: linear-gradient(105deg, var(--lh-a), var(--lh-b));
		box-shadow: 0 12px 34px color-mix(in srgb, var(--lh-a) 20%, transparent);
		transition: transform 650ms ease, box-shadow 650ms ease;
	}
	.lh-actions a:hover {
		transform: translateY(-2px);
		box-shadow: 0 16px 38px color-mix(in srgb, var(--lh-a) 27%, transparent);
	}
	@keyframes lh-motif-travel {
		to { stroke-dashoffset: -1; }
	}
	@media (max-width: 420px) {
		.lh-actions { width: 100%; }
		.lh-actions a { width: min(18rem, 100%); }
	}
	@media (prefers-reduced-motion: reduce) {
		.lh-motif-glint { display: none; }
		.lh-actions a { transition: none; }
		.lh-aurora { animation: none; }
	}
</style>
