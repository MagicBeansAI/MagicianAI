<script lang="ts">
	/**
	 * OPS DECK system bar — the deck's masthead and vital signs.
	 *
	 * Every figure is a live measurement: component LEDs from `/health`,
	 * spend/calls/tokens from the Today pulse (real DuckDB SQL), the mode
	 * word from the deck state machine. Offline sources render dashes and
	 * dark LEDs — the bar never pretends.
	 */
	import { fmtUsd, fmtCompact, type DeckMode } from './deck';

	export let mode: DeckMode = 'nominal';
	export let clock = '--:--:--';
	/** null = probe not yet answered; each key true/false once probed. */
	export let health: { magician: boolean; magicutor: boolean; tauri: boolean } | null = null;
	export let spendToday: number | null = null;
	export let spendYesterday: number | null = null;
	export let callsToday: number | null = null;
	export let memoriesToday: number | null = null;
	export let eventsPerSecond = 0;

	/** Operator scope. Lives up here rather than under the chat rail: it is
	 *  ambient context for the whole deck, not a property of one channel. */
	export let scope: { principal: string; workspace: string } | null = null;


	const MODE_LABEL: Record<DeckMode, string> = {
		nominal: 'NOMINAL',
		attention: 'ATTENTION',
		fault: 'FAULT'
	};

	$: spendDelta =
		spendToday != null && spendYesterday != null && spendYesterday > 0
			? Math.round(((spendToday - spendYesterday) / spendYesterday) * 100)
			: null;

	function ledState(ok: boolean | undefined): string {
		if (health === null || ok === undefined) return 'unknown';
		return ok ? 'up' : 'down';
	}
</script>

<header class="bar" data-mode={mode}>
	<div class="ident">
		<span class="mark" aria-hidden="true"></span>
		<h1 class="word">DECK</h1>
	</div>
	<span class="notch" aria-hidden="true"></span>
	{#if mode !== 'nominal'}
		<!-- NOMINAL is the default state of the room; a badge that is always
		     on says nothing. When the deck has something to report the word
		     appears here, on the right side of the notch. -->
		<span class="mode" data-mode={mode}>{MODE_LABEL[mode]}</span>
	{/if}

	<div class="leds" role="group" aria-label="Component health">
		{#each [{ key: 'magician', label: 'CORE' }, { key: 'magicutor', label: 'BRIDGE' }, { key: 'tauri', label: 'HOST' }] as c (c.key)}
			<span class="led" data-state={ledState(health?.[c.key as 'magician' | 'magicutor' | 'tauri'])}>
				<i aria-hidden="true"></i>{c.label}
			</span>
		{/each}
	</div>

	<div class="gauges">
		<div class="gauge" title="Spend today (account-wide, live SQL)">
			<span class="g-label">SPEND</span>
			<span class="g-value">{fmtUsd(spendToday)}</span>
			{#if spendDelta !== null}
				<span class="g-delta" data-neg={spendDelta > 0}>{spendDelta > 0 ? '+' : ''}{spendDelta}%</span>
			{/if}
		</div>
		<div class="gauge" title="LLM calls today">
			<span class="g-label">CALLS</span>
			<span class="g-value">{fmtCompact(callsToday)}</span>
		</div>
		<div class="gauge" title="Memory events today">
			<span class="g-label">MEM</span>
			<span class="g-value">{fmtCompact(memoriesToday)}</span>
		</div>
		<div class="gauge" title="Event stream rate (EMA)">
			<span class="g-label">FLOW</span>
			<span class="g-value">{eventsPerSecond.toFixed(1)}<small>/s</small></span>
		</div>
	</div>

	{#if scope}
		<span class="scope" title="principal / workspace">{scope.principal}/{scope.workspace}</span>
	{/if}
	<time class="clock" datetime={clock}>{clock}</time>
</header>

<style>
	.bar {
		grid-area: sysbar;
		/* Above the scanline film and the panels: this row carries the theme
		   control and must never sit under a decorative layer. */
		position: relative;
		z-index: 5;
		display: flex;
		align-items: center;
		gap: 28px;
		/* Right padding reserves the lane for `.deck-theme-corner`, which is
		   a FIXED sibling of the deck rather than a child of this bar. */
		padding: 0 68px 0 18px;
		height: 54px;
		border-bottom: 1px solid var(--deck-line);
		background: linear-gradient(180deg, color-mix(in srgb, var(--deck-glow) 7%, transparent), transparent 80%);
		position: relative;
	}
	/* Diagonal notch — the bar reads as cut metal, not a web navbar.
	   IN FLOW, not absolutely positioned: the old `left: 236px` was aligned
	   to an ident sized for the removed NOMINAL badge, and stayed stranded
	   mid-LEDs when the ident shrank. As a flex child it hugs DECK however
	   the masthead changes, and the mode badge (ATTENTION / FAULT) lands on
	   its right side when it appears. */
	.notch {
		align-self: stretch;
		width: 22px;
		flex: none;
		margin: 0 -8px -1px;
		background: linear-gradient(105deg, transparent calc(50% - 1px), var(--deck-line) 50%, transparent calc(50% + 1px));
		pointer-events: none;
	}

	.ident { display: flex; align-items: center; gap: 12px; }
	.mark {
		width: 10px; height: 10px;
		background: var(--deck-glow);
		clip-path: polygon(50% 0, 100% 50%, 50% 100%, 0 50%);
		box-shadow: 0 0 12px color-mix(in srgb, var(--deck-glow) 70%, transparent);
	}
	.word {
		margin: 0;
		font: 700 17px/1 var(--font-display);
		letter-spacing: 0.24em;
		/* The wordmark wears the theme accent — the deck owns no palette. */
		color: var(--deck-glow);
	}
	.mode {
		font: 600 10px/1 var(--font-data);
		letter-spacing: 0.22em;
		padding: 4px 8px 3px;
		border: 1px solid var(--deck-line);
		color: var(--deck-glow);
	}
	.mode[data-mode='attention'] { animation: mode-blink 1.6s steps(2, jump-none) infinite; }
	.mode[data-mode='fault'] { animation: mode-blink 0.9s steps(2, jump-none) infinite; }
	@keyframes mode-blink { 50% { background: color-mix(in srgb, var(--deck-glow) 18%, transparent); } }

	.leds { display: flex; gap: 16px; }
	.led {
		display: inline-flex; align-items: center; gap: 6px;
		font: 500 10px/1 var(--font-data);
		letter-spacing: 0.18em;
		color: var(--deck-dim);
	}
	.led i {
		width: 7px; height: 7px; border-radius: 50%;
		/* Unprobed: a dimmed foreground, not a fixed dark grey — the latter
		   disappears entirely on a light theme, reading as "no LED at all". */
		background: color-mix(in srgb, var(--deck-text) 22%, transparent);
		transition: background 300ms, box-shadow 300ms;
	}
	.led[data-state='up'] i { background: var(--sev-ok); box-shadow: 0 0 8px color-mix(in srgb, var(--sev-ok) 70%, transparent); }
	.led[data-state='down'] i { background: var(--sev-err); box-shadow: 0 0 8px color-mix(in srgb, var(--sev-err) 70%, transparent); }

	.gauges { display: flex; gap: 26px; margin-left: auto; align-items: baseline; }
	.gauge { display: flex; align-items: baseline; gap: 8px; }
	.g-label { font: 600 9px/1 var(--font-data); letter-spacing: 0.2em; color: var(--deck-dim); }
	.g-value {
		font: 600 16px/1 var(--font-data);
		font-variant-numeric: tabular-nums;
		color: var(--deck-text);
	}
	.g-value small { font-size: 10px; color: var(--deck-dim); }
	.g-delta { font: 500 10px/1 var(--font-data); color: var(--sev-ok); }
	.g-delta[data-neg='true'] { color: var(--sev-warn); }

	.scope {
		font: 400 9px/1 var(--font-data);
		letter-spacing: 0.1em;
		color: var(--deck-dim);
		opacity: 0.75;
		white-space: nowrap;
	}

	.clock {
		font: 500 14px/1 var(--font-data);
		font-variant-numeric: tabular-nums;
		letter-spacing: 0.08em;
		color: var(--deck-dim);
	}

	@media (max-width: 1100px) {
		.gauges { gap: 14px; }
		.leds { display: none; }
	}
</style>
