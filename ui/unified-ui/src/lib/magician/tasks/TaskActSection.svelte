<script lang="ts" context="module">
	/**
	 * One row of provenance: a name, and the opaque thing it names. Exported so a
	 * panel can build these against a type rather than a shape.
	 */
	export interface ProvenanceEntry {
		label: string;
		value: string;
	}
</script>

<script lang="ts">
	/**
	 * One act of the task panel — a header that is always readable, and a body that
	 * expands to the act's content and its provenance.
	 *
	 * This component is where the disclosure ladder stops being a convention. L1
	 * is the header and carries the summary while closed; L2 is the body and exists only while
	 * the act is open. **Nothing from level N+1 renders at level N** — not hidden
	 * with CSS, absent from the DOM. See design §2.
	 *
	 * **The ladder is three levels, not four.** L3 was a `Details` disclosure over
	 * the provenance list, and it was retired once the content was measured: the
	 * Output act's held four rows of which three had their own value as their
	 * label, and the Run act's held two identifiers. A toggle and a container for
	 * one fact is the `Questions: 2` defect wearing a different label — a control
	 * whose only information is that information exists elsewhere. Provenance now
	 * renders with the body it belongs to.
	 *
	 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §2 and §4.
	 */
	import { createEventDispatcher } from 'svelte';

	import MiddleEllipsis from '$lib/magician/components/chat/MiddleEllipsis.svelte';
	import { rise } from '$lib/magician/chat/next/motion';

	import type { ActId } from './taskCapabilities';

	/** Which act this is. Carried on `toggle`, so a panel needs no closure per section. */
	export let id: ActId;
	/** The act's name — from `ACT_TITLES`, which is where the copy for the union lives. */
	export let title: string;
	/**
	 * The L1 line, from `actSummaries.ts`, rendered only while the act is closed.
	 * Empty shows the title alone rather than a placeholder.
	 */
	export let summary: string;
	/**
	 * Keep the summary in the header while the act is open. Only where the body
	 * does not restate it: the Plan act's body lists steps, never the plan's
	 * state (`approved 15m ago`, `draft`), so closing the summary there lost it.
	 */
	export let summaryWhenOpen = false;
	/** A chip on the header row's right (the plan graph), or `null` for none. */
	export let action: { label: string; ariaLabel?: string } | null = null;
	/** Whether this is the open act. Owned by the panel; see `toggle`. */
	export let open = false;
	/**
	 * Renders with the act's body, and not at all while the act is closed.
	 * `UnifiedTaskPanel` no longer passes it: since the drawer redesign (D3)
	 * every act's ids and paths live in the panel's collapsed *Technical
	 * details* section instead. Kept for a host that wants rows in the act.
	 */
	export let provenance: ProvenanceEntry[] = [];
	/**
	 * How long this card waits before it rises in, in ms — the panel staggers its
	 * cards by position. `rise` reads its duration from the `--dur-*` tokens and
	 * is instant under reduced motion.
	 */
	export let enterDelay = 0;
	/**
	 * Whether the `glance` slot has something to show under the closed header.
	 * A flag rather than `$$slots`: a host passing the slot to every act would
	 * otherwise give every closed card an empty second line.
	 */
	export let glance = false;

	/**
	 * The header reports that the reader acted on this act; it does not open
	 * itself. Only one act is open at a time, and a section that toggled its own
	 * prop could leave two open with nothing able to notice — that invariant
	 * belongs to whatever renders the column, so the decision does too.
	 */
	const dispatch = createEventDispatcher<{ toggle: ActId; action: ActId }>();

	let headerEl: HTMLButtonElement | null = null;
	let bodyEl: HTMLDivElement | null = null;

	/**
	 * Hand focus back to the header before the body it was in stops existing.
	 *
	 * The act that is open is not always the act the reader opened: with no
	 * choice made the panel follows the task's state, so a poll that moves the
	 * verdict moves the open act with it, and a different task resets the choice
	 * outright. Either way this act's body is removed while the reader may be
	 * standing in it — on an output row's control, in a provenance value — and focus falls
	 * to `<body>`, which is nowhere: the next Tab restarts from the top of a
	 * document the drawer has declared modal.
	 *
	 * The header is the sensible landing place rather than a merely valid one. It
	 * is the control that owns the body that just went away, it is still on
	 * screen, and it is the one thing that brings the body back.
	 *
	 * Called from the reactive block below, which runs before the DOM is patched,
	 * so both elements are still bound and moving focus is an ordinary
	 * synchronous call — no `tick()`, and nothing left to run after the node has
	 * gone.
	 */
	function releaseFocusFromBody(): void {
		if (bodyEl === null || headerEl === null) return;
		const active = document.activeElement;
		// Only when the caret is actually inside the body. Focus sitting on the
		// header already, or outside this act entirely, is not ours to move.
		if (!(active instanceof HTMLElement) || !bodyEl.contains(active)) return;
		headerEl.focus();
	}

	$: if (!open) {
		releaseFocusFromBody();
	}

</script>

<!--
	**A card.** Each act is its own bounded card (`--bg-card`, a soft border,
	`--radius-md`) stacked with a small gap — the redesign that replaced the
	hairline-ruled rows and the full-bleed wash on the open act. The open act is
	marked by its chevron alone (no accent bar — tried and dropped), never by
	a fill.
-->
<section
	class="act"
	class:act--open={open}
	data-act={id}
	in:rise={{ delay: enterDelay, y: 6 }}
>
	<!--
		`ui-no-press` is the house opt-out from `app.css`'s app-wide
		`button:active { transform: scale(0.96) }`. On a 40px control that reads as
		a press; on a row spanning a 560px drawer it is a ~21px squeeze, which is
		exactly the card-shaped gesture this component exists to stop making. The
		same class is how Today and Square rows opt out. Note the reduced-motion
		guard does not reach the bare `button:active` selector, so without this a
		reader who asked for less motion still gets the squeeze.
	-->
	<button
		type="button"
		class="act__header ui-no-press"
		class:act__header--with-action={action !== null}
		aria-expanded={open}
		bind:this={headerEl}
		on:click={() => dispatch('toggle', id)}
	>
		<span class="act__marker" aria-hidden="true">{open ? '▾' : '▸'}</span>
		<span class="act__title">{title}</span>
		{#if summary && (!open || summaryWhenOpen)}
			<!-- One line. The summary is often `Wrote <a 90-character filename>`, so
			     the middle goes and the extension stays; the full line is on `title=`.
			     **Closed acts only.** The summary stands in for a body the reader
			     cannot see; once the act is open the body says the same thing in full
			     (the Output act's summary is the file names listed right below it),
			     so it is not rendered — not hidden — and the header reads as the
			     title alone. -->
			<span class="act__summary" title={summary}><MiddleEllipsis text={summary} tail={18} /></span>
		{/if}
	</button>

	{#if action !== null}
		<!-- An act-level control (the plan graph chip), on the header row at the
		     right. Beside the header button rather than inside it: a button cannot
		     hold a button, and the header must stay the section's direct child
		     (the panel anchors scrolling on `> .act__header`). -->
		<span class="act__action">
			<button
				type="button"
				class="mg-chip-action"
				aria-label={action.ariaLabel ?? action.label}
				on:click={() => dispatch('action', id)}
			>
				{action.label}
			</button>
		</span>
	{/if}

	{#if !open && glance}
		<!--
			**The closed card's glance** — an L1 picture of the act under its header
			(the Run card's step dots). Only while closed: once open, the body says
			the same thing at full resolution.
		-->
		<div class="act__glance">
			<slot name="glance" />
		</div>
	{/if}

	{#if open}
		<div class="act__body" bind:this={bodyEl}>
			<slot />

			{#if provenance.length > 0}
				<!--
					**There is no `Details` button any more, and the ladder is three levels
					deep rather than four.**

					It shipped as one, and the content never justified it. Measured on a real
					task, the Output act's disclosure held four rows of which three had their
					own value as their label — `out_task_…json = outputs/out_task_…json` — and
					the Run act's held two identifiers, one of which has since moved to the
					header. A toggle, a label promising more, and a container, for one fact.

					A disclosure called "Details" is also the same defect as `Questions: 2`,
					which §1 names as the original complaint: a control whose only information
					is that information exists elsewhere, so the reader cannot decide *not* to
					open it. The fix for a thin disclosure is not better copy on the button.

					So provenance renders with the act. It is a handful of rows, it is
					genuinely useful now that the Run act's cost aggregate lives here, and the
					click it used to cost bought nothing.
				-->
				<dl class="act__provenance">
					{#each provenance as entry}
						<div class="act__provenance-row">
							<dt>{entry.label}</dt>
							<dd>{entry.value}</dd>
						</div>
					{/each}
				</dl>
			{/if}
		</div>
	{/if}
</section>

<style>
	/* **A card.** It was a hairline-ruled row whose open state washed the header
	   edge to edge; the redesign makes each act a bounded card on the body's tint,
	   inset by the drawer's `--task-panel-bleed` — the same inset the verdict's
	   status block pads by, so the card edges and the status dot share one left
	   edge. `--act-pad` is the card's own inner padding, declared once here and
	   read by the header and the body.

	   `overflow: hidden` clips the header's hover wash to the card's radius. */
	.act {
		--act-pad: 0.75rem;

		position: relative;
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		margin: 0 var(--task-panel-bleed, 0px);
		overflow: hidden;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
	}

	.act__action {
		display: flex;
		align-items: center;
		grid-column: 2;
		grid-row: 1;
		padding: 0.3rem var(--act-pad) 0.3rem 0;
	}

	.act__header {
		grid-column: 1;
		grid-row: 1;
		min-width: 0;
		display: flex;
		align-items: baseline;
		gap: var(--space-sm);
		width: 100%;
		padding: 0.6rem var(--act-pad);
		border: 0;
		background: none;
		font: inherit;
		color: var(--text-primary);
		text-align: left;
		cursor: pointer;
	}

	/* The primary control in the panel; without this the only signal it was a
	   control was `:focus-visible`. The treatment is `native/Button.svelte`'s
	   outline hover — a background, not a lift — clipped to the card's radius. */
	.act__header:hover {
		background: var(--bg-soft);
	}

	/* Inside the card rather than outside it: the card clips its overflow, so an
	   outset ring would be cut off at the radius. */
	.act__header:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: -2px;
	}

	.act__marker {
		flex: none;
		/* The disclosure column, declared once on `.panel` — see the note there. */
		width: var(--task-panel-disclosure);
		font-size: 0.7rem;
		color: var(--text-secondary);
		text-align: center;
	}

	/* L1: the act's name, compact — the card's own heading. */
	.act__title {
		flex: none;
		font-family: var(--font-display);
		font-size: 0.82rem;
		font-weight: 650;
	}

	/* The title says which act; the summary says what happened in it. One line:
	   `MiddleEllipsis` keeps the tail and drops the middle, full text on `title=`. */
	.act__summary {
		flex: 1 1 auto;
		display: flex;
		min-width: 0;
		overflow: hidden;
		font-family: var(--font-primary);
		font-size: 0.72rem;
		color: var(--text-secondary);
		white-space: nowrap;
	}

	/* Hangs off the title's left edge (past the chevron column), so the glance
	   reads as the header's second line rather than as body content. */
	.act__glance {
		grid-column: 1 / -1;
		margin-top: -0.2rem;
		padding: 0 var(--act-pad) 0.6rem
			calc(var(--act-pad) + var(--task-panel-disclosure) + var(--space-sm));
	}

	/* The body spans the card under its header. It no longer hangs off the
	   disclosure column: inside a bounded card the indent bought nothing and cost
	   a third of a 560px drawer's output rows. */
	.act__body {
		grid-column: 1 / -1;
		min-width: 0;
		padding: 0 var(--act-pad) var(--act-pad);
	}

	/* L3's own step down, and the air that separates it from the body above it.
	   A dangling comment opener once swallowed this rule; that is now a case in
	   `taskPanelPresentation.test.ts`. */
	.act__provenance {
		margin: var(--space-sm) 0 0;
		font-size: 0.7rem;
	}

	/* **Grid, not flex, and `minmax(0, 1fr)` is the whole point.**
	   As a flex row this collapsed: a 100-character output path rendered at
	   **0px wide and 110 lines tall**. A grid track floors itself, so the value
	   column cannot be squeezed below its share by a long sibling.

	   The label track was a fixed `7rem` with a `dt` that could not wrap, so a
	   longer label (`Source execution id`, a filename) printed over its value.
	   It now sizes to its content up to 40% of the row, and the label wraps. */
	.act__provenance-row {
		display: grid;
		grid-template-columns: fit-content(40%) minmax(0, 1fr);
		gap: var(--space-sm);
	}

	/* L3 keeps a step of its own, between two readable colours — `--text-muted`
	   does not clear 4.5:1 against this panel's surface in more than half the
	   themes in `app.css`. */
	.act__provenance dt {
		min-width: 0;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	/* A grid item in a `minmax(0, 1fr)` track, so the floor lives on the track and
	   `min-width: 0` here is inert either way; kept so the cell's behaviour does
	   not depend on the row's layout mode. */
	.act__provenance dd {
		margin: 0;
		min-width: 0;
		font-family: var(--font-mono);
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}
</style>
