<script lang="ts">
	// "You are <cycling role>." — the hook that makes the visitor supply
	// themselves. The roles hold in turn (roleCycle.ts carries the list and the
	// pure HOLD_MS math); the last one is a small, specific detail sitting in
	// the same list as "a professional", which is the argument the page makes.
	//
	// Every role is a different length, so the sentence has to MORPH between
	// them: the trailing period jumps on every swap otherwise. That needs each
	// role's natural width, and the obvious way to get it — stack all of them
	// in one grid cell and read `clientWidth` — turned out to be the wrong
	// trade. Ten painted nodes in the display layer produced three separate
	// artifacts at once: inactive roles overflowed the animating box unclipped
	// and ghosted during the fade, `justify-self: stretch` sized every role's
	// `background-clip: text` box to the ANIMATING width so the gradient
	// re-scaled every frame and dropped out wherever text ran past its own
	// box, and ten gradient-clipped nodes transitioning together thrashed
	// paint.
	//
	// So measurement and display are separate layers here. A hidden,
	// absolutely-positioned measurer holds every role and never paints;
	// the display layer only ever holds the one role you can see (plus its
	// predecessor for the length of a fade). That keeps the width exact and
	// the paint honest.
	//
	// This span is aria-hidden: a screen reader would otherwise be handed
	// whichever single role happened to be showing at read time. LandingHero's
	// <h1> carries the stable aria-label with the full proposition instead.
	//
	// Reduced motion renders the full static list, no timer, no cycling — the
	// same information the cycling version delivers over a full pass, at once.
	import { onDestroy, onMount } from 'svelte';
	import { motionEnabled } from '$lib/motion';
	import { cycleIndex, ROLES } from './roleCycle';

	$: reduced = !$motionEnabled;

	let elapsed = 0;
	let startedAt = 0;
	let timer: ReturnType<typeof setInterval> | null = null;
	let mounted = false;

	function stop(): void {
		if (timer !== null) {
			clearInterval(timer);
			timer = null;
		}
	}

	function start(): void {
		if (timer !== null) return;
		startedAt = Date.now();
		elapsed = 0;
		// A tenth of HOLD_MS is plenty of resolution for a hold this long
		// without redrawing every frame for a line that isn't moving.
		timer = setInterval(() => {
			elapsed = Date.now() - startedAt;
		}, 100);
	}

	// The width transition must not be armed for the very first width the box
	// ever gets, or the hero animates on mount from whatever the unmeasured
	// box happened to be. Two animation frames is enough for that first width
	// to have painted with no transition active; only role-to-role changes
	// after that ever animate.
	let stageSized = false;
	function armStageTransition(): void {
		requestAnimationFrame(() => {
			requestAnimationFrame(() => {
				stageSized = true;
			});
		});
	}

	// Guarded on `mounted`, not just `reduced`: this reactive statement also
	// runs during SSR (only onMount/onDestroy are browser-only), and an
	// unguarded setInterval there would leak a live timer into the server
	// process. Once mounted, a live reduced-motion toggle stops the cycle
	// immediately and, if motion returns, resumes from a clean start and
	// re-arms `stageSized`, since the reduced branch tears the stage down.
	$: if (mounted) {
		if (reduced) {
			stop();
		} else {
			stageSized = false;
			armStageTransition();
			start();
		}
	}

	$: index = cycleIndex(elapsed, ROLES.length);
	$: role = ROLES[index];

	// Measured off the hidden layer below, never off anything painted. Stays
	// `null` until a role has actually reported a size — kept distinct from
	// `0` so the stage can tell "not measured yet" from "measured as
	// zero-width" and only ever takes an explicit width once a real one
	// exists. `bind:clientWidth` runs on a ResizeObserver, so a role
	// re-reports for free when the webfont swaps in and reflows it.
	let widths: (number | null)[] = ROLES.map(() => null);
	$: stageWidth = widths[index] ?? null;

	onMount(() => {
		mounted = true;
	});

	onDestroy(stop);
</script>

<span class="rc" aria-hidden="true">
	{#if reduced}
		<span class="rc-static">{ROLES.join(', ')}</span>
	{:else}
		<!-- MEASUREMENT LAYER. Every role, so each one's natural width is known
		     before it is ever shown. `visibility: hidden` rather than
		     `opacity: 0` — it removes the subtree from paint entirely instead
		     of drawing ten transparent gradient-clipped sentences over each
		     other, and unlike `display: none` it still lays out, which is the
		     whole point. Absolutely positioned so it contributes nothing to the
		     line's own geometry. -->
		<span class="rc-measure">
			{#each ROLES as roleText, i (roleText)}
				<span bind:clientWidth={widths[i]}>{roleText}</span>
			{/each}
		</span>

		<!-- DISPLAY LAYER. Exactly one role in the DOM at any moment: `{#key}`
		     with no `transition:` directive removes the outgoing node
		     synchronously, so nothing overlaps and nothing can ghost. -->
		<span
			class="rc-stage"
			class:rc-stage--sized={stageSized}
			style={stageWidth !== null ? `width: ${stageWidth}px;` : ''}
		>
			{#key role}
				<span class="rc-role">{role}</span>
			{/key}
		</span>
	{/if}
</span>

<style>
	.rc {
		display: inline;
	}

	/* Never painted, never selectable, never measured against the line. Only
	   `clientWidth` is ever read off it. */
	.rc-measure {
		position: absolute;
		visibility: hidden;
		pointer-events: none;
		user-select: none;
		white-space: nowrap;
		/* Kept off the layout flow entirely; the stage below owns the geometry. */
		top: 0;
		left: 0;
		height: 0;
		overflow: hidden;
	}
	/* `width: max-content` is load-bearing, not tidiness. A plain `display:
	   block` child fills its container, so every role reported the CONTAINER's
	   width — ten identical numbers, all equal to the longest role — and the
	   stage sized itself to the longest one every time while the active role
	   sat left-aligned in it with the difference as dead space to the right.
	   `max-content` shrinks each row to its own text, which is the whole
	   point of measuring them separately. */
	.rc-measure > span {
		display: block;
		width: max-content;
		white-space: nowrap;
	}

	.rc-stage {
		display: inline-grid;
		vertical-align: baseline;
		/* Growing into a longer role, the new text is at full width instantly
		   while the box is still gliding open, so it would spill over the
		   trailing period without this. Clipped instead, and the fade below
		   runs on the same curve and duration — so the box opens and the text
		   arrives together, reading as a reveal rather than as text being cut.
		   Shrinking needs no such care: the shorter text already fits. */
		overflow: hidden;
	}
	.rc-stage--sized {
		transition: width 420ms cubic-bezier(0.33, 1, 0.68, 1);
	}

	.rc-role {
		grid-area: 1 / 1;
		/* NOT `stretch`. A stretched grid item takes the stage's ANIMATING
		   width, which would size the `background-clip: text` box below to a
		   value changing every frame — the gradient would re-scale as it moved
		   and drop out wherever the text ran past its own box. `start` pins
		   each role's paint box to its own text. */
		justify-self: start;
		white-space: nowrap;
		/* Same duration and curve as the stage's width transition, so the
		   reveal and the morph are one movement rather than two. */
		animation: rc-fade-in 420ms cubic-bezier(0.33, 1, 0.68, 1) both;
	}

	.rc-role,
	.rc-static {
		background: var(
			--landing-title-gradient,
			linear-gradient(
				104deg,
				var(--text-primary) 12%,
				var(--lh-a, var(--accent-primary, #9e59ff)) 62%,
				var(--lh-b, var(--accent-secondary, var(--lh-a, #9e59ff)))
			)
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}

	@keyframes rc-fade-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	/* The reduced-motion list is every role at once and is expected to wrap;
	   it overrides the `white-space: nowrap` LandingHero's `.lh-title-you`
	   sets for the cycling case, which would otherwise force this much longer
	   sentence onto one line and off the edge of the viewport. */
	.rc-static {
		white-space: normal;
	}

	/* A stage that is mid-morph while the visitor has asked for less motion
	   should not keep animating; the branch above already avoids rendering it,
	   this is belt-and-braces for a live toggle. */
	@media (prefers-reduced-motion: reduce) {
		.rc-stage--sized {
			transition: none;
		}
		.rc-role {
			animation: none;
		}
	}
</style>
