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
	import type { Verdict } from './taskVerdict';

	export let verdict: Verdict;
	export let cue: string | null = null;

	$: tone = VERDICT_TONE[verdict.state];
	$: marker = VERDICT_MARKER[verdict.state];
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
<div class="verdict" data-verdict-state={verdict.state} data-tone={tone} role="status" aria-live="polite">
	<span class="verdict__marker" aria-hidden="true">{marker}</span>
	<div class="verdict__lines">
		<p class="verdict__headline">{verdict.headline}</p>
		<p class="verdict__detail">{verdict.detail}</p>
		{#if cue !== null}<p class="verdict__cue">{cue}</p>{/if}
	</div>
</div>

<style>
	/* The band reaches CSS as one attribute rather than as a rule per state, so
	   the state → band mapping stays in TypeScript where the union checks it.
	   Selecting on `data-verdict-state` here would be a second enumeration of
	   `VerdictState` typed against nothing. */
	.verdict {
		--verdict-tone: var(--text-secondary);
		--verdict-tone-soft: transparent;

		display: flex;
		align-items: baseline;
		gap: var(--space-sm);
		/* **The horizontal inset is `--task-panel-bleed`, which is the drawer's own,
		   and that is what makes this wash a band rather than a rectangle.**
		   Previously `0`, over a scroll container that padded itself by
		   `--space-md`, so the tone band stopped a whole `--space-md` short of the
		   drawer on both sides — the owner's "just coloring the padded box". The
		   container now insets nothing and this pads its own content, so the wash
		   runs edge to edge while the marker stays on the column every act shares.

		   `.act__header` reads the same variable, which is why the two still share
		   one left edge and why the presentation test can compare them by reading
		   one value out of both. The fallback is `0px` in every reader — see the
		   note where it is declared. */
		padding: var(--space-sm) var(--task-panel-bleed, 0px) var(--space-md);
		/* No radius: a rounded band with square edges outside it reads as a card
		   that failed to load. It was `--radius-sm` while the block was inset and
		   had corners to round. */
		background: var(--verdict-tone-soft);
	}

	/* All five non-neutral bands, including `paused`, which nothing currently maps
	   to: a band with no rule falls back to the quiet default above, which is the
	   same silent degradation the tone map exists to prevent. `neutral` is that
	   default and gets no wash — an idle state has nothing to shout about, and it
	   is why the type scale and not the wash has to be what ranks this line.

	   `paused` being unreachable is load-bearing for the contrast sweep in
	   `taskPanelPresentation.test.ts`, which measures the marker against the four
	   bands a verdict can actually be in. `VERDICT_TONE` above is the map that
	   makes it unreachable, and the component test pins its seven rows. */
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

	/* The glyph takes the band's colour; the prose does not. Body text in
	   `--status-failed` is a contrast problem in every theme, and the wash behind
	   the block already carries the colour at a size the eye catches.

	   Pulled toward `--text-primary` rather than used neat, which is the same
	   move `Badge` makes on its paused row and for the same reason: a solid
	   status colour on its own soft wash falls under the 3:1 floor for a
	   meaningful glyph in roughly half this app's themes. At this ratio the band
	   is still legible as a band and the floor holds in every one of them. */
	.verdict__marker {
		flex: none;
		/* The disclosure column, declared once on `.panel` — see the note there.
		   Reading it from the shared property rather than restating the number is
		   what makes "the verdict and the acts share one left edge" checkable
		   instead of a coincidence two files have to keep agreeing about. */
		width: var(--task-panel-disclosure);
		font-size: 0.8125rem;
		color: color-mix(in srgb, var(--verdict-tone) 60%, var(--text-primary));
		text-align: center;
	}

	.verdict__lines {
		min-width: 0;
	}

	/* **L0, and the only thing on screen that says so without colour.**
	   Several states are `neutral` and get no wash at all, so a reader
	   looking at a queued or a cancelled task sees this line, the act titles and
	   the drawer's own heading with nothing but type to rank them. It used to be
	   `1rem/650` against act titles at `0.9375rem/650` — one weight, one family,
	   one colour and a 1.067 ratio, which is a rounding error rather than a
	   step. The scale is now a step of 1.2 down to the act title and 1.154 again
	   down to the drawer heading, with the weight falling 700 → 600 → 500
	   alongside it, so the ranking survives greyscale, a disabled wash and the
	   monochrome themes that collapse all four washes into one grey.

	   `taskPanelPresentation.test.ts` holds the three levels to that shape. */
	.verdict__headline {
		margin: 0;
		font-family: var(--font-display);
		font-size: 1.125rem;
		font-weight: 700;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.verdict__detail {
		/* One number, used twice: the reserved height has to track the line height
		   or the empty line reserves the wrong amount of space. */
		--verdict-detail-leading: 1.45;

		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		line-height: var(--verdict-detail-leading);
		/* A finished task's detail is empty until the output summary fills it, and
		   a verdict that changes height as the task moves is the panel twitching
		   under the reader. The element always renders; this keeps its line box. */
		min-height: calc(1em * var(--verdict-detail-leading));
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.verdict__cue {
		width: fit-content;
		margin: var(--space-xs) 0 0;
		padding: 0.15rem 0.45rem;
		border: 1px solid var(--border-subtle);
		border-radius: var(--radius-full);
		font-size: 0.75rem;
		font-weight: 650;
		color: var(--text-secondary);
		background: var(--surface-raised);
	}
</style>
