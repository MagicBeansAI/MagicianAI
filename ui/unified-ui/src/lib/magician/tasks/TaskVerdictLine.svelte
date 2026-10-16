<script lang="ts" context="module">
	import type { SemanticTone } from '$lib/shared/statusTone';
	import type { VerdictState } from './taskVerdict';

	/**
	 * The severity band each verdict state renders in. A `Record<VerdictState,
	 * SemanticTone>`, so a new state fails to compile here rather than
	 * rendering in the muted default with nothing to notice, and a mistyped band
	 * fails against the union rather than silently matching no CSS rule.
	 *
	 * **This is not `statusTone()`, on purpose.** That function is the single
	 * `status → colour` map for the whole UI and it is keyed on task *statuses*,
	 * which is a different string space: several verdict states are not statuses at
	 * all, and passing them through it degrades silently rather than loudly —
	 * `stalled` and `finished` are not keys, so both fall through to `neutral`,
	 * and the key `waiting` means the *status* waiting, which it bands as
	 * `paused`. Loudest state in the union, rendered as the quietest.
	 *
	 * What is reused is the vocabulary: the bands are `SemanticTone`'s and the
	 * colours are the `--status-*` tokens those bands name, so no new palette is
	 * invented here.
	 *
	 * One band is deliberately not the one `statusTone` gives the same word.
	 * `cancelled` is `neutral` here where the status map has it as `failed`,
	 * because the two maps answer different questions: a chip beside a task
	 * answers "what happened to it", where cancelled and failed are both "did not
	 * finish", while this line answers "is it okay?" — and a task the reader
	 * stopped on purpose is okay. Painting `You stopped this at step 3` in
	 * `--status-failed` asserts a problem that does not exist. Known cost: the
	 * task list's own chip still shows cancelled in the failed colour, so the two
	 * surfaces disagree until someone reconciles them.
	 *
	 * Module-scoped rather than exported: nothing outside this file reads it, and
	 * the component test asserts the bands as literals precisely so that it does
	 * not — importing the map would assert only that the map equals itself.
	 */
	const VERDICT_TONE: Record<VerdictState, SemanticTone> = {
		waiting: 'attention',
		failed: 'failed',
		// Needs a human as much as `waiting` does, and is neither deliberate
		// (`paused`) nor terminal (`failed`).
		stalled: 'attention',
		running: 'running',
		paused: 'neutral',
		cancelled: 'neutral',
		queued: 'neutral',
		archived: 'neutral',
		finished: 'completed'
	};

	/**
	 * The glyph each state renders, and the reason the states stay distinguishable
	 * once the colour is gone — greyscale, high-contrast mode, or a reader who
	 * cannot separate red from green. Bands are shared by design: several states
	 * share `attention` or `neutral`, so colour alone cannot carry every state
	 * even where it is visible.
	 *
	 * A `Record` for the same reason as the tone map. Every value must be
	 * distinct — that is the whole property, and the component test asserts it as
	 * a set rather than nine equalities, which would pass with a duplicate.
	 *
	 * `⟳` and `✓` are the design's own marks for a live and a completed step
	 * (§2's mock), reused here rather than invented. The stalled triangle carries
	 * U+FE0E so it renders as a glyph rather than as a colour emoji beside eight
	 * monochrome ones.
	 */
	export const VERDICT_MARKER: Record<VerdictState, string> = {
		waiting: '!',
		failed: '✕',
		stalled: '⚠︎',
		running: '⟳',
		paused: 'Ⅱ',
		cancelled: '■',
		queued: '⋯',
		archived: '◇',
		finished: '✓'
	};
</script>

<script lang="ts">
	/**
	 * L0 — the verdict line. Two lines at the top of the task panel: what state
	 * the task is in and how long it has been there, then what that means.
	 *
	 * A pure renderer of a `Verdict`. Both strings arrive composed and are printed
	 * verbatim, so their grammar belongs to whatever built the verdict — including
	 * the finished detail, which design §3 fills from the output summary and which
	 * therefore owns the fact that `Wrote no output` is not a sentence.
	 *
	 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §3.
	 */
	import MiddleEllipsis from '$lib/magician/components/chat/MiddleEllipsis.svelte';

	import type { Verdict } from './taskVerdict';

	export let verdict: Verdict;
	export let cue: string | null = null;
	/**
	 * The agent that ran the task, when the model names one — rendered beside the
	 * headline, never invented. `null` renders nothing.
	 */
	export let agent: string | null = null;

	$: tone = VERDICT_TONE[verdict.state];
	$: marker = VERDICT_MARKER[verdict.state];
	/**
	 * Whether the detail may wrap. A failure or an ask is the sentence the reader
	 * came for, so it keeps up to three lines; every other detail is one line,
	 * middle-ellipsized — it is usually `Wrote <a 90-character filename>`, and the
	 * file's extension is the half worth keeping. The full text is on `title=`.
	 */
	$: wraps = tone === 'failed' || tone === 'attention';
</script>

<!--
	`role="status"` wraps the whole block — both lines, the words on screen, one
	copy. The two alternatives both cost more than they save. Wrapping only the
	headline announces `Failed · at step 5 of 7` and drops the sentence saying
	what failed, so the reader hearing it learns less than the one glancing.
	Wrapping a hidden state summary instead means two copies of the verdict in the
	document, and the quiet one goes stale the moment a duration moves — design §6
	forbids rendering a stale fact as a current one, and it does not stop
	forbidding it because the fact is only spoken.

	The cost, stated rather than hidden: this announces on **every** change,
	including one where nothing happened. Two of the nine headlines carry a
	duration measured against `now` — `Waiting on you · 4m` and `Stalled · no
	progress for 6m` — so while either is on screen every poll re-reads a verdict
	that has not changed. That is not fixable here: this component cannot make an
	announcement lag its own text. It is fixable in the two layers that produce
	the churn — whatever owns `now` can advance it on a coarser cadence than it
	polls, and `deriveVerdict` could carry the duration separately from the
	headline instead of inside it.

	`status` and not `alert`: a background poll flipping a task to failed should
	not interrupt whatever the reader is listening to.
-->
<!--
	The marker is the **status dot**: a small disc in the band's soft colour with
	the state's glyph inside it, so the colour cue and the shape cue are one mark.
	The glyph stays character data (`aria-hidden`, because the headline already
	says the state in words) and is what survives greyscale.
-->
<div class="verdict" data-verdict-state={verdict.state} data-tone={tone} role="status" aria-live="polite">
	<span class="verdict__marker" aria-hidden="true">{marker}</span>
	<div class="verdict__lines">
		<div class="verdict__top">
			<p class="verdict__headline">{verdict.headline}</p>
			{#if agent}<span class="verdict__agent" title={agent}>{agent}</span>{/if}
		</div>
		<p class="verdict__detail" class:verdict__detail--wrap={wraps} title={verdict.detail || undefined}>{#if wraps}{verdict.detail}{:else}<MiddleEllipsis text={verdict.detail} tail={24} />{/if}</p>
		{#if cue !== null}<p class="verdict__cue">{cue}</p>{/if}
	</div>
</div>

<style>
	/* The band reaches CSS as one attribute rather than as a rule per state, so
	   the state → band mapping stays in TypeScript where the union checks it.
	   Selecting on `data-verdict-state` here would be a second enumeration of
	   `VerdictState` typed against nothing.

	   **A compact status block, not a slab.** This painted the band's soft wash
	   across the whole drawer — a mint or peach slab that out-shouted everything
	   under it. The colour now lives in the status dot only; the block itself is
	   the header card's surface, continuing the drawer header flush above it, with
	   the card's one soft bottom border.

	   The horizontal inset is `--task-panel-bleed`, the drawer's own, which is
	   also the margin every act card takes — so the dot and the card edges share
	   one left edge. */
	.verdict {
		--verdict-tone: var(--text-secondary);
		--verdict-tone-soft: var(--bg-soft);

		display: flex;
		align-items: flex-start;
		gap: var(--space-sm);
		padding: var(--space-xs) var(--task-panel-bleed, 0px) var(--space-sm);
		background: var(--bg-card);
		border-bottom: 1px solid var(--border-soft);
	}

	/* All five non-neutral bands, including `paused`, which nothing currently maps
	   to: a band with no rule falls back to the quiet default above, which is the
	   same silent degradation the tone map exists to prevent. `neutral` is that
	   default, a grey dot — an idle state has nothing to shout about.

	   `paused` being unreachable is load-bearing for the contrast sweep in
	   `taskPanelPresentation.test.ts`, which measures the marker against the bands
	   a verdict can actually be in. `VERDICT_TONE` above is the map that makes it
	   unreachable, and the component test pins its rows. */
	.verdict[data-tone='attention'] {
		--verdict-tone: var(--status-attention);
		--verdict-tone-soft: var(--status-attention-soft);
	}

	.verdict[data-tone='failed'] {
		--verdict-tone: var(--status-failed);
		--verdict-tone-soft: var(--status-failed-soft);
	}

	.verdict[data-tone='running'] {
		--verdict-tone: var(--status-running);
		--verdict-tone-soft: var(--status-running-soft);
	}

	.verdict[data-tone='paused'] {
		--verdict-tone: var(--status-paused);
		--verdict-tone-soft: var(--status-paused-soft);
	}

	.verdict[data-tone='completed'] {
		--verdict-tone: var(--status-completed);
		--verdict-tone-soft: var(--status-completed-soft);
	}

	/* **The status dot.** The glyph takes the band's colour, pulled 60% toward
	   `--text-primary` — the same move `Badge` makes on its paused row — because
	   a solid status colour on its own soft wash falls under the 3:1 floor for a
	   meaningful glyph in roughly half this app's themes. The prose beside it
	   stays in the text tokens. */
	.verdict__marker {
		flex: none;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.125rem;
		height: 1.125rem;
		margin-top: 0.05rem;
		border-radius: var(--radius-full);
		background: var(--verdict-tone-soft);
		font-size: 0.625rem;
		line-height: 1;
		color: color-mix(in srgb, var(--verdict-tone) 60%, var(--text-primary));
		text-align: center;
	}

	/* The chat live card's semantics: a live run breathes, a run that needs a
	   human pulses. Durations are the liveliness tokens, so the reduced-motion
	   block below (and any theme) reaches them. */
	.verdict[data-tone='running'] .verdict__marker {
		animation: verdict-breathe calc(var(--dur-slow, 520ms) * 4) ease-in-out infinite;
	}

	.verdict[data-tone='attention'] .verdict__marker {
		animation: verdict-pulse calc(var(--dur-slow, 520ms) * 3) ease-in-out infinite;
	}

	@media (prefers-reduced-motion: reduce) {
		.verdict[data-tone='running'] .verdict__marker,
		.verdict[data-tone='attention'] .verdict__marker {
			animation: none;
		}
	}

	@keyframes verdict-breathe {
		0%,
		100% {
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--verdict-tone) 0%, transparent);
		}
		50% {
			box-shadow: 0 0 0 3px color-mix(in srgb, var(--verdict-tone) 28%, transparent);
		}
	}

	@keyframes verdict-pulse {
		0%,
		100% {
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--verdict-tone) 45%, transparent);
		}
		50% {
			box-shadow: 0 0 0 4px color-mix(in srgb, var(--verdict-tone) 0%, transparent);
		}
	}

	.verdict__lines {
		flex: 1 1 auto;
		min-width: 0;
	}

	/* The headline and, when the model names one, the agent beside it. Wraps so a
	   long agent name drops under the headline rather than squeezing it. */
	.verdict__top {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0 var(--space-sm);
		min-width: 0;
	}

	/* **L0 of the panel's column.** Bold display face at the card's own scale —
	   below the drawer's title, which now names the task in words, and level with
	   the act titles it introduces. `taskPanelPresentation.test.ts` holds the
	   levels to that shape. */
	.verdict__headline {
		margin: 0;
		min-width: 0;
		font-family: var(--font-display);
		font-size: 0.85rem;
		font-weight: 700;
		line-height: 1.35;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.verdict__agent {
		min-width: 0;
		max-width: 100%;
		overflow: hidden;
		font-family: var(--font-primary);
		font-size: 0.7rem;
		color: var(--text-secondary);
		white-space: nowrap;
		text-overflow: ellipsis;
	}

	.verdict__detail {
		/* One number, used twice: the reserved height has to track the line height
		   or the empty line reserves the wrong amount of space. */
		--verdict-detail-leading: 1.45;

		margin: 0;
		min-width: 0;
		font-family: var(--font-primary);
		font-size: 0.72rem;
		line-height: var(--verdict-detail-leading);
		/* A finished task's detail is empty until the output summary fills it, and
		   a verdict that changes height as the task moves is the panel twitching
		   under the reader. The element always renders; this keeps its line box. */
		min-height: calc(1em * var(--verdict-detail-leading));
		color: var(--text-secondary);
		overflow: hidden;
		white-space: nowrap;
	}

	/* A failure or an ask: up to three lines, then an ellipsis, full text on
	   `title=`. Both clamp spellings, for the reason the drawer's title gives. */
	.verdict__detail--wrap {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 3;
		line-clamp: 3;
		white-space: normal;
		overflow-wrap: anywhere;
	}

	.verdict__cue {
		width: fit-content;
		margin: var(--space-xs) 0 0;
		padding: 0.15rem 0.45rem;
		border: 1px solid var(--border-subtle);
		border-radius: var(--radius-chip);
		font-size: 0.7rem;
		font-weight: 650;
		color: var(--text-secondary);
		background: var(--surface-raised);
	}
</style>
