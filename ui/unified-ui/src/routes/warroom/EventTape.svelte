<script lang="ts">
	/**
	 * EVENT STRIP — ambience, not a log.
	 *
	 * The full 12-row tape was information nobody read: the operator wanted
	 * the FEELING of traffic, not the ledger. So each arriving event surfaces
	 * once as a blip — appears, holds a beat, fades — and the numbers that
	 * actually matter stay put: link state, last-event age, total, peak rate.
	 * The full ledger lives at /events; the title is the door to it.
	 *
	 * Still honest: a blip only fires for a REAL frame (keyed on the row id),
	 * and when the uplink is down the strip says so in words instead of
	 * animating silence.
	 */
	import type { TapeRow } from './deck';
	import { fmtAge } from './deck';

	export let rows: TapeRow[] = [];
	/** Real per-second arrival counts, most recent LAST (60 buckets). */
	export let histogram: number[] = [];
	export let connection: 'connecting' | 'open' | 'closed' | 'error' = 'connecting';
	export let totalEvents = 0;
	export let nowMs = Date.now();

	$: histMax = Math.max(1, ...histogram);
	$: newest = rows[0] ?? null;

	const SEV_GLYPH: Record<TapeRow['severity'], string> = {
		info: '·',
		success: '▲',
		warn: '◆',
		error: '✕',
		hitl: '⬖'
	};
</script>

<section class="tape" aria-label="Live event strip">
	<a class="feed-title" href="/events" title="Open the full live event ledger">EVENT TAPE ↗</a>
	<span class="uplink" data-state={connection}>
		{#if connection === 'open'}UPLINK LIVE{:else if connection === 'connecting'}LINKING…{:else}UPLINK OFFLINE — RETRYING{/if}
	</span>
	{#if newest}<span class="tstat">last {fmtAge(newest.ts, nowMs)}</span>{/if}
	<span class="tstat">{totalEvents} total</span>
	<span class="spark" aria-label="Events per second, last 60 seconds" title="events/s · 60s">
		{#each histogram as count, i (i)}
			<i style:height="{Math.max(8, (count / histMax) * 100)}%" data-hot={count > 0}></i>
		{/each}
	</span>
	<span class="tstat">pk {histMax}/s</span>

	<div class="blip-zone" aria-live="off">
		{#if newest}
			{#key newest.id}
				<span class="blip" data-sev={newest.severity}>
					<i class="blip-glyph" aria-hidden="true">{SEV_GLYPH[newest.severity]}</i>
					{newest.event_type}{#if newest.agent_id}<em class="blip-who">{newest.agent_id}</em>{/if}
				</span>
			{/key}
		{:else if connection !== 'open'}
			<span class="blip blip--still">no telemetry: backend unreachable</span>
		{/if}
	</div>
</section>

<style>
	.tape {
		grid-area: tape;
		/* Above the deck's scanline film — see `.scan` in +page.svelte. */
		z-index: 1;
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 0 16px;
		border-top: 1px solid var(--deck-line);
		min-height: 0;
		overflow: hidden;
	}

	.feed-title {
		font: 600 9px/1 var(--font-display);
		letter-spacing: 0.3em;
		color: var(--deck-dim);
		text-decoration: none;
		white-space: nowrap;
	}
	.feed-title:hover { color: var(--deck-glow); }

	/* NOT `.link`: daisyUI underlines that class globally. */
	.uplink {
		font: 500 9px/1 var(--font-data);
		letter-spacing: 0.14em;
		color: var(--sev-ok);
		white-space: nowrap;
	}
	.uplink[data-state='connecting'] { color: var(--sev-warn); }
	.uplink[data-state='closed'],
	.uplink[data-state='error'] { color: var(--sev-err); animation: link-throb 1.4s ease-in-out infinite; }
	@keyframes link-throb { 50% { opacity: 0.45; } }

	/* NOT `.stat`: daisyUI ships a global `.stat` grid component and the
	   collision blew this span up to 625px, wrapping the whole head. */
	.tstat {
		font: 400 9px/1 var(--font-data);
		letter-spacing: 0.1em;
		color: var(--deck-dim);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.spark {
		display: flex;
		align-items: flex-end;
		gap: 1px;
		width: 140px;
		height: 16px;
		flex: none;
	}
	.spark i {
		flex: 1;
		min-height: 1px;
		background: color-mix(in srgb, var(--deck-glow) 20%, transparent);
	}
	.spark i[data-hot='true'] { background: color-mix(in srgb, var(--deck-glow) 75%, transparent); }

	/* ── the blip: one event, shown once, let go ───────────────────────── */
	.blip-zone {
		flex: 1;
		min-width: 0;
		display: flex;
		justify-content: flex-end;
	}

	.blip {
		display: inline-flex;
		align-items: baseline;
		gap: 8px;
		font: 400 10.5px/1 var(--font-data);
		color: var(--deck-text);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		/* Appear, hold a beat, fade. Keyed per row id, so every REAL frame
		   restarts the life cycle and silence lets the strip go quiet. */
		animation: blip-life 7s ease forwards;
	}
	@keyframes blip-life {
		0% { opacity: 0; transform: translateY(4px); }
		6% { opacity: 1; transform: translateY(0); }
		55% { opacity: 1; }
		100% { opacity: 0; }
	}
	.blip--still { animation: none; color: var(--deck-dim); }

	.blip-glyph { font-style: normal; }
	.blip[data-sev='info'] .blip-glyph { color: var(--deck-dim); }
	.blip[data-sev='success'] .blip-glyph { color: var(--sev-ok); }
	.blip[data-sev='warn'] .blip-glyph { color: var(--sev-warn); }
	.blip[data-sev='error'] .blip-glyph { color: var(--sev-err); }
	.blip[data-sev='hitl'] .blip-glyph { color: var(--sev-hitl); }
	.blip[data-sev='error'] { color: var(--sev-err); }

	.blip-who {
		font-style: normal;
		color: var(--deck-dim);
		max-width: 200px;
		overflow: hidden;
		text-overflow: ellipsis;
	}
</style>
