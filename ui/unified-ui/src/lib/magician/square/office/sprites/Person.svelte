<script lang="ts">
	/**
	 * A seated crew member, drawn as layered SVG parts rather than a picked
	 * sprite: chair, torso, collar, neck, head, hair, face, accessories.
	 *
	 * COMPOSITION IS THE POINT. Casting (palette.ts) draws gender, skin, hair
	 * colour, hair style and outfit independently off the citizen id, so eleven
	 * people are eleven people. A fixed cast of portraits would repeat by the
	 * fifth desk and could not colour itself per program.
	 *
	 * The origin is the SEAT BASELINE — (0,0) sits where the chair meets the
	 * desk — and the figure is drawn upward from there in negative y. Desk.svelte
	 * is painted AFTER this component, so the slab crosses the body at about the
	 * chest and what survives above it is head, shoulders, upper torso and the
	 * arms reaching to the desk. That occlusion is what makes the pose read as
	 * seated; do not "fix" the torso looking short.
	 *
	 * SCALE IS NOT THE DESK'S. The caller draws this at `plan.personScale`, which
	 * is `plan.seatScale * PERSON_RATIO` — a person is drawn SMALLER than the
	 * furniture around them, because this is a top-down plan and a front-facing
	 * figure only reads as a token on a map while it is small (see PERSON_RATIO
	 * in floorPlan.ts). Sizes here are therefore in PERSON sprite units, and the
	 * desk's are not; a coordinate copied from Desk.svelte lands in the wrong
	 * place. Draw the face at full detail regardless: it is authored large and
	 * scaled down, which is why it stays clean at token size.
	 *
	 * Flat fills, no gradients, no filters — sharp at any scale, which is the
	 * whole reason these are drawn rather than imported.
	 */
	import type { Vibe } from '../../engine/types';
	import { STATUS_COLOUR, type PersonLook } from './palette';

	/** Cast once by the caller (personLookOf) so a redraw never re-rolls a face. */
	export let look: PersonLook;
	export let vibe: Vibe = 'idle';
	export let resting = false;
	/** Seated at a desk, or on their feet (walking / signalling / away). */
	export let pose: 'seated' | 'standing' = 'seated';
	export let signalling = false;

	$: resolved = look;
	$: standing = pose === 'standing';

	const HEAD_Y = -45;
	const HEAD_R = 12;
</script>

<g class="office-person" class:office-person--offline={vibe === 'offline'}>
	<!-- back hair, behind the head and shoulders -->
	{#if resolved.hairStyle === 'long'}
		<rect x="-13.5" y="-52" width="27" height="36" rx="11" fill={resolved.hair} />
	{:else if resolved.hairStyle === 'bob'}
		<rect x="-13.5" y="-52" width="27" height="26" rx="10" fill={resolved.hair} />
	{:else if resolved.hairStyle === 'ponytail'}
		<ellipse cx="14" cy="-38" rx="5.5" ry="10" fill={resolved.hair} />
		<circle cx="12" cy="-48" r="4" fill={resolved.hair} />
	{:else if resolved.hairStyle === 'bun'}
		<circle cx="0" cy="-59" r="6.5" fill={resolved.hair} />
	{:else if resolved.hairStyle === 'curly'}
		<circle cx="0" cy="-47" r="15" fill={resolved.hair} />
	{/if}

	{#if !standing}
		<!-- office chair, peeking out past the shoulders -->
		<rect x="-21" y="-32" width="42" height="38" rx="7" fill="var(--office-chair)" />
		<rect x="-17" y="-28" width="34" height="30" rx="5" fill="var(--office-chair-dark)" />
	{:else}
		<!-- standing legs. Origin stays the seated baseline, so a walk does not
		     lift the figure off the floor the moment they get up. -->
		<rect x="-9" y="4" width="7" height="16" rx="2.5" fill={resolved.outfit} />
		<rect x="2" y="4" width="7" height="16" rx="2.5" fill={resolved.outfit} />
		<rect x="-9.5" y="18" width="8" height="3.5" rx="1.4" fill="#2a2230" />
		<rect x="1.5" y="18" width="8" height="3.5" rx="1.4" fill="#2a2230" />
	{/if}

	<!-- torso -->
	<path
		d="M-17.5,5 L-17.5,-23 Q-17.5,-33.5 -7,-35.5 L7,-35.5 Q17.5,-33.5 17.5,-23 L17.5,5 Z"
		fill={resolved.outfit}
	/>
	<path d="M6.5,-34.5 Q17.5,-32.5 17.5,-23 L17.5,5 L6.5,5 Z" fill="#000" opacity="0.1" />

	<!-- ARMS. Seated: elbows out and hands on the desk. Standing: arms hang,
	     unless they are signalling — then one arm goes up. -->
	{#if standing}
		{#each [-1, 1] as side (side)}
			{@const raised = signalling && side === 1}
			<path
				d={raised
					? `M${side * 13},-31 Q${side * 20},-48 ${side * 16},-58`
					: `M${side * 13},-31 Q${side * 16},-8 ${side * 12},8`}
				fill="none"
				stroke={resolved.outfit}
				stroke-width="7.5"
				stroke-linecap="round"
			/>
			<circle
				cx={raised ? side * 16 : side * 12}
				cy={raised ? -58 : 8}
				r="3.8"
				fill={resolved.skin}
			/>
		{/each}
	{:else}
		{#each [-1, 1] as side (side)}
			<path
				d="M{side * 13},-31 Q{side * 22},-23 {side * 16.5},-14.5"
				fill="none"
				stroke={resolved.outfit}
				stroke-width="7.5"
				stroke-linecap="round"
			/>
			<path
				d="M{side * 13},-31 Q{side * 22},-23 {side * 16.5},-14.5"
				fill="none"
				stroke="#000"
				opacity="0.13"
				stroke-width="7.5"
				stroke-linecap="round"
			/>
			<circle cx={side * 16} cy="-14.5" r="3.8" fill={resolved.skin} />
		{/each}
	{/if}

	{#if resolved.collar}
		<path d="M-7.5,-35.5 L0,-25 L7.5,-35.5 Z" fill="var(--office-paper)" />
		{#if resolved.badge === 'ceo'}
			<path d="M0,-26 L-2.6,-22 L0,-10 L2.6,-22 Z" fill="#b03a48" />
		{/if}
	{/if}

	<!-- neck and head -->
	<rect x="-4.5" y="-40" width="9" height="9" fill={resolved.skinShade} />
	<circle cx="-12" cy={HEAD_Y + 1} r="3" fill={resolved.skinShade} />
	<circle cx="12" cy={HEAD_Y + 1} r="3" fill={resolved.skinShade} />
	<circle cx="0" cy={HEAD_Y} r={HEAD_R} fill={resolved.skin} />

	<!-- front hair -->
	{#if resolved.hairStyle === 'buzz'}
		<path d="M-12,-45 A 12,12 0 0 1 12,-45 Q 0,-50 -12,-45 Z" fill={resolved.hair} />
	{:else if resolved.hairStyle === 'sidepart'}
		<path d="M-12,-45 A 12,12 0 0 1 12,-45 Q 6,-49 0,-48 Q -6,-47 -12,-45 Z" fill={resolved.hair} />
		<path d="M-11,-48 Q -3,-59 11,-49 Q 1,-53 -11,-48 Z" fill={resolved.hair} />
	{:else}
		<path d="M-12,-45 A 12,12 0 0 1 12,-45 Q 6,-49 0,-48 Q -6,-47 -12,-45 Z" fill={resolved.hair} />
	{/if}
	{#if resolved.hairStyle === 'curly'}
		<circle cx="-8" cy="-53" r="4.6" fill={resolved.hair} />
		<circle cx="0" cy="-56" r="5" fill={resolved.hair} />
		<circle cx="8" cy="-53" r="4.6" fill={resolved.hair} />
	{/if}
	{#if resolved.hairStyle === 'short'}
		<rect x="-12.5" y="-46" width="3" height="7" rx="1.5" fill={resolved.hair} />
		<rect x="9.5" y="-46" width="3" height="7" rx="1.5" fill={resolved.hair} />
	{/if}

	<!-- face -->
	<ellipse cx="-7" cy="-41.5" rx="2.6" ry="1.7" fill="#e07a7a" opacity="0.28" />
	<ellipse cx="7" cy="-41.5" rx="2.6" ry="1.7" fill="#e07a7a" opacity="0.28" />
	<ellipse cx="-4.2" cy="-46" rx="1.5" ry="1.9" fill="#2a2230" />
	<ellipse cx="4.2" cy="-46" rx="1.5" ry="1.9" fill="#2a2230" />
	<path
		d="M-3,-40.6 Q0,-38.2 3,-40.6"
		fill="none"
		stroke="#2a2230"
		stroke-width="1.2"
		stroke-linecap="round"
	/>
	{#if resolved.glasses}
		<g fill="none" stroke="#2a2230" stroke-width="1" opacity="0.85">
			<circle cx="-4.4" cy="-46" r="4.2" />
			<circle cx="4.4" cy="-46" r="4.2" />
			<path d="M-0.2,-46 L0.2,-46" stroke-width="1.2" />
		</g>
	{/if}

	<!-- Role badges: the same three the HUD singles out. They sit ON the figure,
	     not floating above it — a badge hovering a head's height clear reaches
	     into the desk of the row behind in a two-row room. -->
	{#if resolved.badge === 'primary'}
		<path
			d="M-13,-58 L-10.8,-53.4 L-5.8,-52.8 L-9.4,-49.3 L-8.5,-44.4 L-13,-46.8 L-17.5,-44.4 L-16.6,-49.3 L-20.2,-52.8 L-15.2,-53.4 Z"
			fill="#e8b437"
			stroke="#a97c14"
			stroke-width="0.6"
		/>
	{:else if resolved.badge === 'ceo'}
		<path
			d="M-8,-56 L-8,-63 L-3.5,-59.5 L0,-65 L3.5,-59.5 L8,-63 L8,-56 Z"
			fill="#e8b437"
			stroke="#a97c14"
			stroke-width="0.6"
		/>
	{:else if resolved.badge === 'envoy'}
		<path
			d="M-13,-46 A 13,13 0 0 1 13,-46"
			fill="none"
			stroke="var(--office-metal-dark)"
			stroke-width="2"
		/>
		<rect x="-16" y="-48" width="4" height="7" rx="2" fill="var(--office-metal-dark)" />
	{/if}

	<!-- status: a needs-you bubble is the only one loud enough to draw in world
	     space; the rest are carried by the desk chip and the HTML name plate -->
	{#if vibe === 'needs' || signalling}
		<g class:office-person__signal={signalling}>
			<path d="M14,-52 L20,-56 L20,-48 Z" fill={STATUS_COLOUR.needs} />
			<rect x="17" y="-68" width="16" height="16" rx="3" fill={STATUS_COLOUR.needs} />
			<rect x="24" y="-65" width="2" height="6" fill="#fff" />
			<rect x="24" y="-57.5" width="2" height="2" fill="#fff" />
		</g>
	{:else if resting && !standing}
		<text
			x="15"
			y="-56"
			font-size="11"
			font-family="var(--game-font-display, monospace)"
			fill="var(--office-ink-soft)">z</text
		>
	{/if}
</g>

<style>
	.office-person--offline {
		opacity: 0.45;
	}
	.office-person__signal {
		transform-origin: 25px -60px;
		animation: office-signal 1.1s ease-in-out infinite;
	}
	@media (prefers-reduced-motion: reduce) {
		.office-person__signal {
			animation: none;
		}
	}
	@keyframes office-signal {
		0%,
		100% {
			transform: translateY(0);
		}
		50% {
			transform: translateY(-3px);
		}
	}
</style>
