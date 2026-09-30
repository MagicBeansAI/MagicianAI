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
	 * is the header and carries the summary; L2 is the body and exists only while
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

	import type { ActId } from './taskCapabilities';

	/** Which act this is. Carried on `toggle`, so a panel needs no closure per section. */
	export let id: ActId;
	/** The act's name — from `ACT_TITLES`, which is where the copy for the union lives. */
	export let title: string;
	/** The L1 line, from `actSummaries.ts`. Empty shows the title alone rather than a placeholder. */
	export let summary: string;
	/** Whether this is the open act. Owned by the panel; see `toggle`. */
	export let open = false;
	/** Renders with the act's body, and not at all while the act is closed. */
	export let provenance: ProvenanceEntry[] = [];

	/**
	 * The header reports that the reader acted on this act; it does not open
	 * itself. Only one act is open at a time, and a section that toggled its own
	 * prop could leave two open with nothing able to notice — that invariant
	 * belongs to whatever renders the column, so the decision does too.
	 */
	const dispatch = createEventDispatcher<{ toggle: ActId }>();

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

<section class="act" class:act--open={open} data-act={id}>
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
		aria-expanded={open}
		bind:this={headerEl}
		on:click={() => dispatch('toggle', id)}
	>
		<span class="act__marker" aria-hidden="true">{open ? '▾' : '▸'}</span>
		<span class="act__title">{title}</span>
		{#if summary}
			<span class="act__summary">{summary}</span>
		{/if}
	</button>

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
	.act {
		border-top: 1px solid var(--border-soft);
	}

	/* A row, not a card. The old panel's equal rectangles are what made an
	   opaque id read as loudly as the one cell that answered the question. */
	.act__header {
		display: flex;
		align-items: baseline;
		gap: var(--space-sm);
		width: 100%;
		/* **The horizontal inset is the drawer's own, so the hover wash below is a
		   full-width band rather than a rectangle floating in a gutter.** It was `0`
		   over a scroll container that padded itself, which is the owner's "background
		   colors just coloring the padded box": the wash on this row stopped a whole
		   `--space-md` short of the drawer on each side. The container now insets
		   nothing and this pads its own content, so the row spans the panel and the
		   marker stays on the column the verdict shares — which reads the same
		   variable, for that reason. */
		padding: var(--space-sm) var(--task-panel-bleed, 0px);
		border: 0;
		background: none;
		font: inherit;
		color: var(--text-primary);
		text-align: left;
		cursor: pointer;
	}

	/* The primary control in the panel, and until now the only signal it was a
	   control at all was `:focus-visible` — nothing at all for a mouse. The
	   treatment is `native/Button.svelte`'s outline hover, which is a background
	   and a colour rather than the lift it gives its solid variants: a row that
	   rises off the column is the card gesture again.

	   It now spans the drawer, because the row does — see the padding above. */
	.act__header:hover,
	/* **The open act's own highlight, and `act--open` finally has a rule.** The
	   class was on the markup from the first commit with nothing selecting it: which
	   act was open could only be read from the body under it, so a reader scrolled
	   into a long Run act had nothing at the top of the column telling them which
	   section they were inside.

	   The *same* wash as the hover rather than a second colour, which is what keeps
	   this from being a new visual language: `--bg-soft` is already the panel's word
	   for "this row is the one", and it is already in the contrast sweep as one of
	   the two surfaces an act's text is measured on. On the header only — the body
	   below carries `--bg-soft` blocks of its own (a shell row's stdout, an opened
	   preview) and a wash of the same colour behind them would flatten both. */
	.act--open > .act__header {
		background: var(--bg-soft);
	}

	.act__header:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.act__marker {
		flex: none;
		/* The disclosure column, declared once on `.panel` — see the note there.
		   Centred like the verdict's marker so the two glyph shapes sit on one
		   axis rather than merely in one column. */
		width: var(--task-panel-disclosure);
		font-size: 0.75rem;
		color: var(--text-secondary);
		text-align: center;
	}

	/* L1. One step below the verdict headline and one above the drawer's heading
	   — see the note on `.verdict__headline` for the scale and what it is for. */
	.act__title {
		flex: none;
		font-family: var(--font-display);
		font-size: 0.9375rem;
		font-weight: 600;
	}

	/* The title says which act; the summary says what happened in it. One line,
	   two weights — the reader gets the whole story from the closed column. */
	.act__summary {
		min-width: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.act__body {
		/* The third reader of the disclosure column: the body hangs under the
		   header's text, not under its marker — and now also under the drawer's own
		   inset, so its content edge still matches the header's above it. Written as
		   the sum of the three things it is rather than as a number that happens to
		   equal them, which is the rule both custom properties in this feature
		   follow: changing either must move this with it. */
		padding: 0 var(--task-panel-bleed, 0px) var(--space-md)
			calc(var(--task-panel-bleed, 0px) + var(--task-panel-disclosure) + var(--space-sm));
	}

	/* L3's own step down, and the air that separates it from the body above it.
	   Both were lost for one commit: retiring the `Details` control removed the
	   `.act__body :global(.act__details)` rule and the tail of its comment but left
	   the opening `/*`, so this rule sat *inside* a comment that then ran on to the
	   next one — the list rendered at the body's size with no separation, and no
	   test could see it because the guard strips comments before it reads anything.
	   A dangling comment opener is now a case in `taskPanelPresentation.test.ts`. */
	.act__provenance {
		margin: var(--space-sm) 0 0;
		font-size: 0.75rem;
	}

	/* **Grid, not flex, and `minmax(0, 1fr)` is the whole point.**
	   As a flex row this collapsed catastrophically: measured in a real browser,
	   a 100-character output path rendered at **0px wide and 110 lines tall** —
	   one character per line — while a 36-character execution id beside it was
	   fine at 266px. Four candidates were measured against the live DOM; only
	   this one and "value on its own line" fixed it, and this one keeps the
	   two-column alignment the `<dt>`/`<dd>` pairing exists for.

	   A grid track floors itself, so the value column cannot be squeezed below
	   its share by a long sibling — which is a property of the track rather than
	   a property the item has to remember to declare. The first column matches
	   `dt`'s own `min-width`. */
	.act__provenance-row {
		display: grid;
		grid-template-columns: 7rem minmax(0, 1fr);
		gap: var(--space-sm);
	}

	/* L3 keeps a step of its own — the label names the thing, the value *is* the
	   thing the reader came down here to copy — but it is now a step between two
	   readable colours. `--text-muted` does not clear 4.5:1 against this panel's
	   surface in more than half the themes in `app.css`, and it was carrying
	   three separate pieces of text in this component. */
	.act__provenance dt {
		flex: none;
		min-width: 7rem;
		color: var(--text-secondary);
	}

	/* **The `min-width: 0` here was not the fix, it was half the bug.** The
	   previous comment claimed it "makes the wrap rule reachable". Measured in a
	   real browser, the opposite: `min-width: auto` would have floored this cell
	   at its min-content width, and `overflow-wrap: anywhere` makes min-content
	   one character — so the automatic floor was one character, and declaring
	   `0` removed even that. The cell rendered at **0px wide and 110 lines
	   tall**.

	   It is now a grid item in a `minmax(0, 1fr)` track, so the floor lives on
	   the track and this declaration is inert either way. Kept because a grid
	   item's `min-width: auto` is still content-based, and leaving it makes the
	   cell's behaviour independent of which layout mode the row uses.

	   **The lesson for the guard, not just for this rule.** A presentation test
	   that asserts `min-width: 0` is *present* cannot catch this: by that rule
	   the 0-width, 110-line cell was correct. A guard written against one
	   direction of a two-sided failure blesses the other, and this is the site
	   where it did. */
	.act__provenance dd {
		margin: 0;
		min-width: 0;
		font-family: var(--font-mono);
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}
</style>
