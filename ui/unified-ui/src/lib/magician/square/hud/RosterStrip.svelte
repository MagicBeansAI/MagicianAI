<script lang="ts">
	/**
	 * RosterStrip — MMO party frames for the fleet: a left-edge column of
	 * citizen mini-frames (initial disc in the guild hue, status dot, health
	 * sliver, [!] badge). Click = select into the command dock; double-click
	 * flies the campus camera. Hotkeys 1–9 select the first nine.
	 */
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { CitizenVM } from '../engine/types';
	import { healthBand } from '$lib/magician/crew/health';
	import { rosterOrder, guildHueOf } from '../derive';

	export let citizens: CitizenVM[] = [];
	export let selectedId: string | null = null;

	const dispatch = createEventDispatcher<{ select: string; fly: string }>();

	$: frames = rosterOrder(citizens);

	function initials(name: string): string {
		return name.trim().charAt(0).toUpperCase() || '?';
	}

	/** Multiline tooltip: identity, board rank, and the leaderboard stats. */
	function frameTitle(c: CitizenVM, i: number): string {
		const lines = [`${c.name} · ${c.title}`, `#${i + 1} on the Crew Board`];
		const stats: string[] = [];
		if (c.health != null) stats.push(`Overall ${c.health}`);
		if (c.healthCoverage != null) stats.push(`Coverage ${Math.round(c.healthCoverage * 100)}%`);
		if (c.healthAverage7d != null) {
			const delta = c.healthDelta7d == null ? '' : ` (${c.healthDelta7d > 0 ? '+' : ''}${c.healthDelta7d})`;
			stats.push(`7d health ${c.healthAverage7d.toFixed(1)}${delta}`);
		}
		if (c.successRate7d != null) stats.push(`Reliability ${Math.round(c.successRate7d * 100)}%`);
		if (c.spendUsd7d != null) stats.push(`Spend $${c.spendUsd7d.toFixed(2)}`);
		if (c.calls7d != null) stats.push(`${c.calls7d} model calls`);
		if (stats.length > 0) lines.push(stats.join(' · '));
		if (i < 9) lines.push(`hotkey [${i + 1}]`);
		return lines.join('\n');
	}
</script>

<div class="rs" role="toolbar" aria-label="Crew roster" data-game-skin="pixel">
	{#each frames as c, i (c.id)}
		<button
			class="rs__frame"
			class:rs__frame--selected={selectedId === c.id}
			style={`--rs-hue:${guildHueOf(c.guildId)}`}
			title={frameTitle(c, i)}
			on:click={() => dispatch('select', c.id)}
			on:dblclick={() => dispatch('fly', c.id)}
		>
			<span class="rs__disc">
				{initials(c.name)}
				{#if c.isPrimary}<span class="rs__star" aria-hidden="true"><Icon name="sparkle" size={9} /></span>{/if}
			</span>
			<span class="rs__dot" data-vibe={c.vibe}></span>
			{#if c.vibe === 'needs'}<span class="rs__bang" aria-hidden="true">!</span>{/if}
			{#if c.health != null}
				<span class="rs__hp">
					<span
						class="rs__hp-fill"
						data-band={healthBand(c.health)}
						style={`width:${Math.max(6, c.health)}%`}
					></span>
				</span>
			{/if}
		</button>
	{/each}
</div>

<style>
	.rs {
		pointer-events: auto;
		position: absolute;
		top: 3.4rem;
		left: 0.75rem;
		z-index: 5;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		max-height: calc(100% - 8rem);
		overflow-y: auto;
		scrollbar-width: none;
		padding: 0.15rem;
	}
	.rs::-webkit-scrollbar {
		display: none;
	}
	.rs__frame {
		position: relative;
		width: 2.15rem;
		padding: 0;
		border: none;
		background: transparent;
		cursor: pointer;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.12rem;
	}
	/* Party frames as hard tiles: square, 2px guild-hue rule, opaque plate, the
	 * initial set in the display face. Opaque matters — these are the one
	 * crew surface that sits directly on the live canvas, so a translucent
	 * plate would put the initial on an unmeasurable backdrop. */
	.rs__disc {
		position: relative;
		width: 1.9rem;
		height: 1.9rem;
		display: grid;
		place-items: center;
		border: 2px solid hsl(var(--rs-hue) 55% 42%);
		background: var(--game-material-panel, var(--bg-card, #fff));
		color: var(--game-text, var(--text-primary, #222));
		font-family: var(--game-font-display);
		font-size: var(--game-display-lg);
		font-weight: 400;
		font-synthesis: none;
		line-height: 1;
	}
	/* Selected inverts the plate. Contrast is symmetric, so the initial keeps
	 * exactly the ratio it had the other way round, in every app theme. */
	.rs__frame--selected .rs__disc {
		border-color: var(--game-text, #222);
		background: var(--game-text, #222);
		color: var(--game-material-panel, #fff);
	}
	.rs__frame:focus-visible .rs__disc {
		outline: 2px solid var(--game-focus-color, var(--accent-primary, #4aa3c0));
		outline-offset: 2px;
	}
	.rs__star {
		position: absolute;
		top: -0.42rem;
		right: -0.42rem;
		font-size: 0.6rem;
	}
	.rs__dot {
		position: absolute;
		bottom: 0.42rem;
		right: -0.05rem;
		width: 0.5rem;
		height: 0.5rem;
		border: 1.5px solid var(--game-material-panel, var(--bg-card, #fff));
		background: var(--fleet-idle, #0078a4);
	}
	.rs__dot[data-vibe='working'] { background: var(--fleet-working, #20773d); }
	.rs__dot[data-vibe='needs'] { background: var(--fleet-needs, #ae6500); }
	.rs__dot[data-vibe='paused'] { background: var(--fleet-paused, #8b5cf6); }
	.rs__dot[data-vibe='offline'] { background: var(--fleet-offline, #4f5860); }
	/* The needs-you badge keeps its amber fill and white glyph — that pair was
	 * already measured and is unchanged; only the corner radius and the shadow
	 * go, to match the flat vocabulary. */
	.rs__bang {
		position: absolute;
		top: -0.3rem;
		left: -0.3rem;
		width: 0.95rem;
		height: 0.95rem;
		display: grid;
		place-items: center;
		background: var(--fleet-needs, #ae6500);
		color: #fff;
		font-size: 0.62rem;
		font-weight: 900;
		line-height: 1;
	}
	.rs__hp {
		width: 1.8rem;
		height: 3px;
		background: rgba(10, 14, 20, 0.35);
		overflow: hidden;
	}
	.rs__hp-fill {
		display: block;
		height: 100%;
		background: var(--fleet-working, #20773d);
	}
	.rs__hp-fill[data-band='amber'] { background: var(--fleet-needs, #ae6500); }
	.rs__hp-fill[data-band='red'] { background: var(--color-error, #e5484d); }
</style>
