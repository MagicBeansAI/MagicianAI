<script lang="ts">
	/**
	 * The drawer the task panel lives in: scrim, dialog, header, body.
	 *
	 * **Everything outside the panel's own column, once.** `TasksWorkspace` and
	 * `InternalTasksWorkspace` each hand-rolled this — the same scrim, the same
	 * dialog role, the same header, and ~50 lines of CSS with matching class
	 * names. Two copies was a recorded cost; five more surfaces are queued onto
	 * this panel, and seven copies would be seven copies of every defect in it.
	 *
	 * See `docs/components/unified-ui/unified-task-panel.md`, *The drawer shell*.
	 *
	 * What is **not** here: the header's action buttons. `/tasks` offers Stop, Run,
	 * Reset to Ready and `ExportMenu`; the internal route offers `ExecutionControls`,
	 * Stop and Retry synthesis; a chat mount may offer none. Those differ because
	 * the surfaces differ, so they arrive through the `actions` slot rather than
	 * being enumerated here — a shell that knew about `retryInternalTaskSynthesis`
	 * would be the type check the panel exists to avoid, moved one level out.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';

	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import Skeleton from '$lib/magician/components/native/Skeleton.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { taskStore } from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import { headerChips, nextHeaderCondensed } from './taskPanelHeader';
	import type { ActId } from './taskCapabilities';
	import type { TaskAskState } from './taskAsk';
	import type { TaskFilePreview } from './taskFilePreview';
	import UnifiedTaskPanel from './UnifiedTaskPanel.svelte';
	import type { TaskPanelFile, TaskPanelModel } from './UnifiedTaskPanel.svelte';

	/** The task, or `null` while nothing has loaded — which is the skeleton below. */
	export let task: TaskPanelModel | null = null;
	/**
	 * The drawer's own heading. `null` when the surface has no title yet, which
	 * reads `Task` — the fallback both surfaces already wrote, kept here so they
	 * cannot answer it differently. An empty string is *not* absent: a task really
	 * titled `''` renders empty rather than being relabelled.
	 */
	export let title: string | null = null;
	/**
	 * **The task in its own words** — row 3 of the header, and the row the owner
	 * called mandatory.
	 *
	 * A prop rather than a field on `TaskPanelModel`, for the same reason `title` is
	 * one: it is the drawer's *chrome*, not something the panel's column renders, and
	 * the model exists to say which acts a task has. `null` renders no row at all —
	 * a surface that has no description shows the title over the chips rather than an
	 * empty band, which is the absent-not-greyed rule this feature follows
	 * everywhere.
	 *
	 * It **hides entirely when the header condenses**, which is where the space for
	 * the sections below comes from. Five lines is a large fraction of a 560px
	 * drawer, and a reader who has scrolled into the run is no longer reading the
	 * brief.
	 */
	export let description: string | null = null;
	/**
	 * The thread this task currently sits in, or `null` when the surface cannot say.
	 *
	 * **Non-null is what turns the thread mover on**, and the reason it is a prop
	 * rather than something derived here is that the drawer would have to guess:
	 * `TaskPanelModel` carries no thread, the store row does, and the two internal
	 * surfaces have no thread at all. A surface that knows passes it and gets the
	 * control; one that does not gets no control rather than one that would move the
	 * task to `general` by default.
	 */
	export let threadId: string | null = null;
	/** Why the last load failed. Rendered by the panel (design §6, case 2 and 3). */
	export let loadError: string | null = null;
	/** When the state on screen was last known good. */
	export let lastLoadedAt: number | null = null;
	/** The clock, passed through. No default, for the reason the panel gives. */
	export let now: number;
	/** Whether each output row carries open and reveal controls. */
	export let outputActions = false;
	/**
	 * Whether the surface answers `previewFile` — that is, whether an output row
	 * can show what is inside it. Passed straight through; the panel's own doc
	 * comment says why it is off by default and why image rows never need it.
	 */
	export let filePreviews = false;
	/** The expanded row's contents, as the surface read them. Passed through. */
	export let filePreview: TaskFilePreview | null = null;
	/**
	 * Whether the surface answers `answer` — that is, whether the ask blocking
	 * this task carries a control. Passed straight through; the panel's own doc
	 * comment says why it is off by default.
	 */
	export let answerAsk = false;
	/** Where the surface has got to with the answer it is posting. Passed through. */
	export let askState: TaskAskState | null = null;
	/** Act to open/focus when drawer opens or updates. */
	export let preferredAct: ActId | null = null;
	/**
	 * Whether Escape closes **this** drawer.
	 *
	 * Escape dismisses the innermost thing first, and which layers exist is the
	 * surface's knowledge — `/tasks` has card menus and a delete dialog, the
	 * internal route has a message popover, a chat mount will have neither. So the
	 * surface says whether anything is over the drawer and the drawer does the
	 * closing: one mechanism each, rather than two things racing to close one
	 * drawer.
	 */
	export let closeOnEscape = true;
	/**
	 * The scrim's stacking level.
	 *
	 * **300, which is above the app's top bar and below its drawers.** It shipped
	 * at 60 and rendered *behind* the top bar, which sits at 200 with a 220 child:
	 * a modal dialog with `aria-modal="true"` was covered by the chrome it was
	 * modal over. The app's ladder, in order: atmosphere 0, context pill 50/60,
	 * workbench launcher 80, network banner 96, **top bar 200/220**, history drawer
	 * 540/550, command palette 600, floating composer 950.
	 *
	 * 300 clears the top bar and stays under the history drawer and the palette,
	 * which is the correct relationship rather than a compromise — a palette
	 * summoned over an open task panel should be on top of it, because it is the
	 * thing the reader just asked for.
	 *
	 * A prop rather than one hardcoded value, because the answer is about what
	 * each surface stacks beneath it: the internal route passes 1400 to cover a
	 * hover card at 1200 and a message popover at 1300, both triggered from rows
	 * this drawer is modal over.
	 */
	export let layer = 300;

	const dispatch = createEventDispatcher<{
		close: void;
		retry: void;
		openFile: { file: TaskPanelFile; index: number };
		revealFile: { file: TaskPanelFile; index: number };
		previewFile: { file: TaskPanelFile; index: number };
	}>();

	let dialogEl: HTMLDivElement | null = null;
	/**
	 * What had focus when this drawer opened, so closing can hand it back.
	 *
	 * Without it the reader's next Tab walks the list **behind** a scrim that
	 * `aria-modal="true"` has just hidden from assistive tech — the drawer
	 * announces itself as modal and then leaves the caret outside it. Captured on
	 * mount rather than from a prop: the trigger is whatever the reader activated,
	 * and every surface's is different.
	 */
	let trigger: HTMLElement | null = null;

	/**
	 * Whether the header is condensed — the title one line and smaller, the
	 * description gone, the space handed to the sections below.
	 *
	 * The threshold logic is `nextHeaderCondensed`, which is hysteretic; see its
	 * note for why one threshold cannot work here. The listener is on the **body**
	 * and the class goes on the **header**, which is its sibling: a header inside
	 * the scroll container would move with the content it is trying to stay above.
	 */
	let condensed = false;

	function onBodyScroll(event: Event): void {
		const target = event.target;
		if (!(target instanceof HTMLElement)) return;
		condensed = nextHeaderCondensed(condensed, target.scrollTop);
	}

	/**
	 * A different task is a different header. Without the reset, opening a second
	 * task from a scrolled panel lands the reader on a condensed header over a body
	 * that is back at the top — the description missing for no reason they can see.
	 *
	 * **Keyed on the task's identity, not on the prop.** This surface polls, so a
	 * new object arrives on `task` several times a minute; a block that watched the
	 * prop would pop the header open under a reader who had scrolled, over and over.
	 * The panel keys its chosen act on `task.id` for exactly this reason and this is
	 * the same fact one level out.
	 */
	let condensedFor: string | null = null;
	$: if (task !== null && task.id !== condensedFor) {
		condensedFor = task.id;
		condensed = false;
		titleExpanded = false;
		descriptionExpanded = false;
		titleOverflows = false;
		descriptionOverflows = false;
	}

	/**
	 * Tap the title or the description to see the rest of it.
	 *
	 * Four things here are decisions rather than mechanics:
	 *
	 * **The control exists only when the text is actually clamped.** A disclosure
	 * that opens onto nothing is the defect this panel is written against — the
	 * same rule that keeps an absent act absent instead of greyed. So it is
	 * measured, not assumed: a clamp is `scrollHeight` exceeding `clientHeight`,
	 * which is a fact about the rendered box and cannot be derived from the string.
	 *
	 * **`overflows` latches.** Expanding removes the clamp, at which point the
	 * element no longer overflows and a naive probe would conclude there is nothing
	 * to collapse — taking the control away mid-interaction and stranding the reader
	 * in the expanded state. So it is only ever measured while collapsed, and the
	 * answer is remembered until the task changes.
	 *
	 * **An expansion outranks the scroll condense.** Condensing hides the
	 * description to buy space; a reader who just asked to see all of it has said
	 * the opposite, and an automatic rule must not overwrite a deliberate one. The
	 * alternative is a panel that silently undoes what you asked for as soon as you
	 * scroll.
	 *
	 * **Reset is keyed on `task.id`**, above, for the same reason the condense is:
	 * the prop is re-created by polling several times a minute.
	 */
	let titleExpanded = false;
	let descriptionExpanded = false;
	let titleOverflows = false;
	let descriptionOverflows = false;

	/**
	 * Head and tail, because both ends are what a reader matches against a log
	 * line — a leading ellipsis would make the id unrecognisable and a trailing one
	 * would hide the part that distinguishes two runs of the same task.
	 *
	 * Short ids are returned whole: eliding something that already fits invents a
	 * truncation the reader then has to undo.
	 */
	function elideId(id: string, head = 8, tail = 4): string {
		return id.length <= head + tail + 1 ? id : `${id.slice(0, head)}…${id.slice(-tail)}`;
	}

	let copiedId = false;
	let copiedIdTimer: ReturnType<typeof setTimeout> | null = null;

	async function copyTaskId(): Promise<void> {
		if (task === null) return;
		try {
			await navigator.clipboard.writeText(task.id);
			copiedId = true;
			if (copiedIdTimer !== null) clearTimeout(copiedIdTimer);
			// Long enough to read, short enough that a stale tick never claims a copy
			// that a later failure did not make.
			copiedIdTimer = setTimeout(() => (copiedId = false), 1400);
		} catch {
			// A clipboard the browser refuses is not an error worth a banner — the
			// full id is already in the `title`, so the reader has it either way.
			copiedId = false;
		}
	}

	onDestroy(() => {
		if (copiedIdTimer !== null) clearTimeout(copiedIdTimer);
	});

	function clampProbe(
		el: HTMLElement,
		report: (overflowing: boolean) => void
	): { update: (next: typeof report) => void; destroy: () => void } {
		let notify = report;
		const measure = () => notify(el.scrollHeight > el.clientHeight + 1);
		measure();
		// The box changes without the text changing — the drawer resizes, a sibling
		// row appears, a font loads. `ResizeObserver` catches all three; a one-shot
		// measurement on mount catches none of them.
		const observer = new ResizeObserver(measure);
		observer.observe(el);
		return {
			update: (next) => {
				notify = next;
				measure();
			},
			destroy: () => observer.disconnect()
		};
	}

	/**
	 * Moving the task to another thread — the one task-level action that is the same
	 * on every surface, and therefore the one the shell owns rather than the
	 * `actions` slot.
	 *
	 * It was `ExecutionPanel`'s and went away with it. Restored here rather than in
	 * each surface because seven surfaces mount this drawer and the operation does
	 * not differ between them; the doc comment above about surface-specific actions
	 * is about `retryInternalTaskSynthesis`, not about this.
	 */
	let moveTarget: string | null = null;
	let moving = false;

	// The reader's pick, until they have made one — and reset by any *external*
	// change of thread, so a task moved from elsewhere does not leave a stale
	// selection sitting over it. Never while a move is in flight: that would fight
	// the optimistic patch `taskStore.updateTask` makes.
	$: if (!moving) moveTarget = threadId;

	// Archived threads are dropped, except the one the task is already in — a task
	// sitting in an archived thread must still show where it is.
	$: threadOptions = $threadStore.threads
		.filter((thread) => !thread.archived || thread.id === threadId)
		.map((thread) => ({ value: thread.id, label: `#${thread.name}` }));
	// One thread is not a choice, and rendering a select over it would promise one.
	$: canMoveThread = threadId !== null && threadOptions.length > 1;
	$: threadMoveReady = moveTarget !== null && moveTarget !== threadId;

	async function moveToThread(): Promise<void> {
		const target = (moveTarget ?? '').trim().toLowerCase();
		if (task === null || moving || target === '' || target === threadId) return;
		moving = true;
		try {
			// Create-if-missing first: `updateTask` would otherwise point the task at
			// a thread id nothing can open.
			await threadStore.ensureThread(target);
			await taskStore.updateTask(task.id, { uiThreadId: target });
			await Promise.all([taskStore.loadTasks(), threadStore.refresh()]);
			showSuccess(`Moved task to #${target}.`);
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to move task');
		} finally {
			moving = false;
		}
	}

	onMount(() => {
		const active = document.activeElement;
		trigger = active instanceof HTMLElement && active !== document.body ? active : null;
		dialogEl?.focus();
		// Ref-counted, and only when there is a thread to move between. A drawer on
		// a surface with no threads must not make the request.
		if (threadId !== null) threadStore.start();
	});

	onDestroy(() => {
		if (threadId !== null) threadStore.stop();
		const restoreTo = trigger;
		trigger = null;
		// The trigger can have left with the row that carried it — a card that
		// re-rendered under a poll, or a task that dropped out of the filter. Focus
		// then stays where the browser puts it rather than throwing.
		if (restoreTo?.isConnected) restoreTo.focus();
	});

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || !closeOnEscape) return;
		dispatch('close');
	}

	/**
	 * The ordinary loading case, which is neither a task nor a failure. Both
	 * surfaces rendered a header over an empty body here, because the panel
	 * deliberately renders nothing without a model and nothing else filled the
	 * gap. The two branches partition `task === null`: this one, and the panel's
	 * own `Can't load this task` when there is an error to show.
	 */
	$: loading = task === null && loadError === null;

	// Derived from the model, so every surface gets them with no wiring. See
	// `headerChips` for why there are two and why neither restates the verdict.
	$: chips = headerChips(task);
</script>

<svelte:window on:keydown={handleWindowKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="task-panel-backdrop"
	style={`--task-panel-layer: ${layer};`}
	on:click={() => dispatch('close')}
>
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="task-panel"
		role="dialog"
		aria-modal="true"
		aria-label="Task panel"
		tabindex="-1"
		bind:this={dialogEl}
		on:click|stopPropagation
	>
		<!--
			**Four rows, in this order, and the order is the whole design of the
			header.** It shipped as one row mixing the title, the surface's actions and
			the close control, which made the title compete with the buttons and left
			the description and the status nowhere to go.

			1. the controls, right-aligned, on a row of their own
			2. the title — optional, two lines, ellipsized
			3. the description — five lines, ellipsized, and the row that goes away first
			4. the status, as chips

			The header is a **sibling** of the scrolling body, not a child of it: it has
			to stay above content that moves, and the condense reads the body's
			`scrollTop` from out here.
		-->
		<header class="task-panel__header" class:task-panel__header--condensed={condensed}>
			<div class="task-panel__actions">
					{#if task !== null}
				<!--
					**Leading edge of the controls row, and it earned its way back here.** It
					spent one revision beside the status chips, on the reasoning that an id is a
					fact about the task rather than an action. On screen the owner read it the
					other way: the row that carries Export and Close is the row you look at to
					act on *this* task, and the id is what tells you which task that is.

					Still a deliberate exception to §2, which puts identifiers behind an
					affordance — correlating a panel against a log needs the id present, not
					reachable. Subtlety is what keeps it from being the noise §1 diagnosed:
					smallest tier, dimmed, monospace, and `margin-right: auto` so it sits
					opposite the controls rather than among them.

					`flex: none` with an ellipsis and deliberately **no**
					`overflow-wrap: anywhere` — an unbreakable hex string is the shape that
					rendered a provenance cell at 0px wide. That one wanted to wrap; this one
					wants to truncate.
				-->
						<button
							type="button"
							class="task-panel__id"
							title={copiedId ? 'Copied' : `${task.id} — click to copy`}
							on:click={() => void copyTaskId()}
						>
							<span class="task-panel__id-value">{elideId(task.id)}</span>
							<span class="task-panel__id-hint" aria-hidden="true">{copiedId ? '✓' : ''}</span>
						</button>
					{/if}
				<slot name="actions" />
				<!--
					`native/Button.svelte`, not an imitation of it. The thirteen declarations
					this replaces each carried a comment saying which of the component's
					variants they were copying — which is the whole of A2's finding: a panel
					that reproduces the design system in CSS gets none of what the components
					carry, including the per-theme treatments (`retro-16bit` squares every
					button in the app and reached nothing in this drawer).
				-->
				<Button
					label="✕"
					variant="outline"
					size="sm"
					className="task-panel__close"
					ariaLabel="Close task panel"
					on:click={() => dispatch('close')}
				/>
			</div>

			{#if canMoveThread}
				<!--
					**Its own row, with a label, because it was neither before.** The thread
					mover sat unlabelled in the controls row: a bare `Select` beside Export and
					Close, where a reader had to infer from its contents that it moved the task
					somewhere. A control whose purpose you deduce from its options is the same
					defect as a disclosure called "Details".

					"Switch to" names the action and the row gives it the width to be read as
					one, rather than competing with controls that act on the task in place.

					The `Move` control still appears only once the selection differs from the
					current thread, so the row carries one control at rest and the second is
					itself the confirmation.
				-->
				<div class="task-panel__move">
					<span class="task-panel__move-label">Move to</span>
					<div class="task-panel__thread">
						<!--
							`ariaLabel` contains the visible label rather than replacing it. A
							control whose accessible name shares no words with the text beside it
							is unaddressable by voice — the reader says "Switch to" and nothing
							matches. It stays longer than the visible label because "Switch to"
							alone does not say *to what*.
						-->
						<Select
							value={moveTarget ?? ''}
							interactive={true}
							ariaLabel="Move to thread"
							options={threadOptions}
							disabled={moving}
							on:change={(event) => (moveTarget = event.detail.value)}
						/>
						{#if threadMoveReady}
							<Button
								label={moving ? 'Moving…' : 'Move'}
								variant="outline"
								size="sm"
								disabled={moving}
								on:click={() => void moveToThread()}
							/>
						{/if}
					</div>
				</div>
			{/if}

			{#if title !== null}
				<!--
					**Optional, unlike the description.** A surface with no title renders no
					row rather than the word `Task` over a description that already says what
					this is — which is what the fallback used to do. `title` on the element
					because two lines is a clamp and the reader needs the rest somehow.
				-->
				<h2 class="task-panel__title-row">
					<!--
						**A button only when there is something to reveal, and plain text
						otherwise.** This was one element with `disabled={!titleOverflows}`, and
						a disabled button is not a neutral resting state for a heading: a
						screen reader announced the panel's title as an *unavailable control*,
						which is a worse claim than the one the clamp was hiding.

						It is the same absent-not-greyed rule the acts follow — a level with
						nothing behind it does not render a control that says otherwise. The
						probe rides both branches, because a title that fits today overflows
						when the drawer narrows, and the swap re-arms it either way.
					-->
					{#if titleOverflows}
						<button
							type="button"
							class="task-panel__title"
							class:task-panel__title--expanded={titleExpanded}
							title={title}
							aria-expanded={titleExpanded}
							use:clampProbe={(over) => {
								if (!titleExpanded && over) titleOverflows = true;
							}}
							on:click={() => (titleExpanded = !titleExpanded)}>{title}</button>
					{:else}
						<span
							class="task-panel__title task-panel__clamp--static"
							title={title}
							use:clampProbe={(over) => {
								if (!titleExpanded && over) titleOverflows = true;
							}}>{title}</span>
					{/if}
				</h2>
			{/if}

			<!--
				Hidden rather than restyled when condensed: five lines is a large fraction
				of the drawer, and the space is the point of condensing. `{#if}` rather
				than `display: none` for the reason the acts use it — a level that is not
				showing should not be in the document either.
			-->
			{#if description !== null && (!condensed || descriptionExpanded)}
				<!-- Same rule as the title above, for the same reason. -->
				{#if descriptionOverflows}
					<button
						type="button"
						class="task-panel__description"
						class:task-panel__description--expanded={descriptionExpanded}
						title={description}
						aria-expanded={descriptionExpanded}
						use:clampProbe={(over) => {
							if (!descriptionExpanded && over) descriptionOverflows = true;
						}}
						on:click={() => (descriptionExpanded = !descriptionExpanded)}>{description}</button>
				{:else}
					<p
						class="task-panel__description task-panel__clamp--static"
						title={description}
						use:clampProbe={(over) => {
							if (!descriptionExpanded && over) descriptionOverflows = true;
						}}>{description}</p>
				{/if}
			{/if}

			<!-- The row exists for either occupant: a surface with a task but no chips
			     still shows the id, and one with chips and no task still shows them. -->
			{#if chips.length > 0 || task !== null}
				<!--
					**`announce={false}`, and it is not optional.** `Badge` is a live region
					by default, which is right for a badge that *is* the news. These are
					not: the verdict line a few pixels below is the panel's one `status`
					region and already reads the state as a sentence with its step position
					and elapsed time in it. Two live regions on one fact means the reader
					hears the change twice, in an order nothing controls — and it is how
					this row first showed up, as `getByRole('status')` finding two elements
					where the panel is documented to have one.
				-->
				<div class="task-panel__chips">
					{#each chips as chip (chip.id)}
						<Badge
							text={chip.label}
							status={chip.tone}
							announce={false}
							className="task-panel__chip"
						/>
					{/each}

				</div>
			{/if}
		</header>

		<div class="task-panel__body" on:scroll={onBodyScroll}>
			{#if loading}
				<div class="task-panel__loading" role="status" aria-label="Loading task">
					<Skeleton variant="rect" height="2.75rem" />
					<Skeleton variant="rect" height="3.5rem" />
					<Skeleton variant="rect" height="3.5rem" />
				</div>
			{:else}
				<!--
					**No `{#key}` here, deliberately.** Selecting another task must not
					land the reader in the act they opened for the last one, and the panel
					does that itself off `TaskPanelModel.id`. Remounting on the id would do
					it too — and then two mechanisms would be clearing one piece of state,
					neither pinned by any test, and whichever one broke would break
					silently. The panel owns it; this renders it.
				-->
				<UnifiedTaskPanel
					{task}
					{loadError}
					{lastLoadedAt}
					{now}
					{outputActions}
					{filePreviews}
					{filePreview}
					{answerAsk}
					{askState}
					{preferredAct}
					on:openFile
					on:revealFile
					on:previewFile
					on:answer
					on:retry
					on:selectRun
				/>
			{/if}
		</div>
	</div>
</div>

<style>
	/* ── the task panel shell ─────────────────────────────────────────────
	   The chrome the panel does not own: a scrim, a titled dialog, a close
	   control, and a slot for the task-level actions the row behind it can no
	   longer offer while this is modal over it. The panel's own column —
	   verdict, then acts — is all that goes inside the body. */
	.task-panel-backdrop {
		position: fixed;
		inset: 0;
		/* The fallback is unreachable — the element that carries this class always
		   sets the property inline — and it matches the prop's default anyway, so a
		   reader who finds this rule first is not told a different number than the
		   one the drawer ships with. */
		z-index: var(--task-panel-layer, 300);
		display: flex;
		justify-content: flex-end;
		background: color-mix(in srgb, var(--bg-primary, #000) 55%, transparent);
	}

	.task-panel {
		display: flex;
		flex-direction: column;
		width: min(560px, 100%);
		/* **Without this the width above is advisory.** This is a flex item of the
		   scrim, so its default `min-width: auto` is a content-based floor: one
		   unbreakable 400-character token anywhere in the panel makes the *dialog*
		   wider than 560px and pushes its own left edge off screen, because the
		   scrim justifies to `flex-end`. The same omission as `.act__provenance dd`
		   — see the note there — and the one instance of it that can move the whole
		   drawer rather than one cell inside it. */
		min-width: 0;
		max-height: 100%;
		background: var(--bg-elevated, var(--bg-secondary));
		border-left: 1px solid var(--border-soft);
		box-shadow: var(--shadow-lg, 0 12px 48px rgb(0 0 0 / 35%));
	}

	/* ── the header's four rows ───────────────────────────────────────────
	   A column, not a row: the controls, the title, the description and the chips
	   are four different kinds of thing and only the first is right-aligned. It
	   was one `align-items: center` row, which is why the title competed with the
	   buttons beside it and the other two facts had nowhere to go.

	   `flex: none` so it never gives up height to the body under it, which is what
	   makes the condense the only thing that changes its size. */
	.task-panel__header {
		flex: none;
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
		padding: var(--space-md);
		border-bottom: 1px solid var(--border-soft);
		/* Padding rather than height, so nothing inside has to animate its own box.
		   Guarded below for a reader who asked for less motion. */
		transition: padding 0.18s ease;
	}

	/* Condensed: less air above and below, and the description row is gone from the
	   document entirely (see the `{#if}`). The title's own step down is on the title. */
	.task-panel__header--condensed {
		padding-top: var(--space-sm);
		padding-bottom: var(--space-sm);
	}

	/* **The only row that is right-aligned, which is why it is a row of its own.**
	   `flex-wrap` because a surface can slot four controls next to the thread
	   select and the close, and a 560px drawer fits about three — a row that
	   overflows would put the close control off screen. */
	.task-panel__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: var(--space-xs);
	}

	/* The select and its confirm, kept together so the pair reads as one control
	   rather than as two of the row's items. */
	/* A row of its own, so the label reads as this control's label and not as a
	   caption for the buttons above it. The `Select` takes the remaining width —
	   `min-width: 0` on the group below is what lets a long thread name shrink
	   instead of pushing `Move` off the row. */
	.task-panel__move {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		min-width: 0;
	}

	.task-panel__move-label {
		flex: none;
		font-size: 0.6875rem;
		color: var(--text-secondary);
		white-space: nowrap;
	}

	/* **The select takes the rest of the row.** `Select` exposes no `className`, so
	   the reach is `:global` — the same route `.task-panel__close` already takes to
	   say it does not stretch. Two levels, because both matter: the component's
	   wrapper has to grow inside this group, and its `<select>` has to fill the
	   wrapper. Setting only the outer one leaves a full-width box with a narrow
	   control sitting at its left edge.

	   `min-width: 0` on the wrapper for the reason every wrapping item in these
	   files carries it: a thread name is arbitrary text, and without the override
	   the control's content-based floor pushes `Move` off the row instead of
	   shrinking. */
	.task-panel__thread :global(.native-select) {
		flex: 1 1 auto;
		min-width: 0;
	}

	.task-panel__thread :global(.native-select__input) {
		width: 100%;
	}

	.task-panel__thread {
		flex: 1 1 auto;
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		/* It holds a thread name, which is arbitrary text — so it takes the floor
		   override every other item in these files that wraps or clips does. */
		min-width: 0;
		margin-right: auto;
	}

	/**
	 * **This steps down rather than the verdict stepping up.**
	 *
	 * It was `1rem/650` in the display face — identical to the verdict headline
	 * below it and a rounding error away from the act titles below that. Three
	 * nominal levels, one treatment. The two lines are answering different
	 * questions and only one of them is the reason the drawer opened: this names
	 * *which* task, which the reader already knew when they clicked it, while
	 * the verdict says whether it is okay, which is what they came to find out.
	 * So this drops to the body face, the bottom step of the scale and the
	 * secondary colour — four levers, all of which survive greyscale — and the
	 * verdict takes the top of the column back.
	 *
	 * It stays an `<h2>`. The heading level is the document's structure and owes
	 * nothing to the type size; a screen reader still hears the drawer's title
	 * first.
	 */
	/* The id sits at the leading edge of a row that is otherwise right-aligned, so
	   `margin-right: auto` rather than a second flex container. `flex: none` and an
	   ellipsis, never `overflow-wrap` — see the markup for why that distinction is
	   load-bearing here. */
	/* No `margin-right: auto` any more — that was for the controls row, where it
	   pushed the id to the leading edge of a right-aligned row. Here it simply
	   follows the chips. */
	/* `margin-right: auto` is what puts it opposite the controls rather than among
	   them: the row is right-aligned, so one auto margin on the first item pins that
	   item left and leaves everything else where it was. */
	.task-panel__id {
		flex: none;
		margin-right: auto;
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		max-width: 14rem;
		overflow: hidden;
		border: 0;
		background: none;
		padding: 0;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		color: var(--text-secondary);
		white-space: nowrap;
		text-overflow: ellipsis;
		cursor: pointer;
	}

	.task-panel__id:hover,
	.task-panel__id:focus-visible {
		color: var(--text-primary);
	}

	/* `min-width: 0` is what makes the ellipsis beside it reachable. This is a flex
	   item of the row above, so its default floor is content-based — and with the
	   inherited `white-space: nowrap` its min-content width is the *whole* string.
	   The cell therefore never shrank, its own `text-overflow` never triggered, and
	   a pathological id was hard-clipped by the parent's `overflow: hidden` instead
	   of being elided. `elideId` keeps the string to 13 characters so it was never
	   seen, but the pair of declarations here promised something they could not do. */
	.task-panel__id-value {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* The heading keeps the outline; the button inside it carries the text and the
	   disclosure, so every button declaration here is a reset rather than a style.
	   A clamped row is the interactive one — `--static` is the case with nothing to
	   reveal, and it must look and behave like the paragraph it used to be. */
	.task-panel__title-row {
		margin: 0;
		min-width: 0;
	}

	.task-panel__clamp--static {
		cursor: default;
	}

	/* Shared by both element types the clamp renders as — a `<button>` when there is
	   something to reveal, a `<span>`/`<p>` when there is not — so everything here
	   has to be true of plain text too. The button-only declarations live below. */
	.task-panel__title,
	.task-panel__description {
		display: -webkit-box;
		width: 100%;
		border: 0;
		background: none;
		padding: 0;
		text-align: left;
	}

	/* **`cursor` belongs to the interactive element, not to the class.** It was on
	   the rule above with a `.task-panel__clamp--static { cursor: default }` meant to
	   opt out — and that opt-out lost on source order, both being one class, so a
	   plain-text title still showed a pointer. Measured: `cursor: pointer` on a
	   `<span>` that does nothing.

	   Element-plus-class instead. It is order-independent, and it says the thing that
	   is actually true: the pointer is there because this one is a button. */
	button.task-panel__title,
	button.task-panel__description {
		cursor: pointer;
	}

	.task-panel__title {
		min-width: 0;
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 500;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
		/* **Two lines, then an ellipsis.** A task title is a sentence the person who
		   created it wrote, so it has no length bound, and an unbounded title in a
		   fixed header pushes the acts off screen. `-webkit-line-clamp` is the only
		   thing that clamps by *lines* rather than by height in every browser this
		   ships to; the prefixed and unprefixed forms are both declared because
		   `line-clamp` is the standard name and the prefixed one is what is
		   implemented. The full string is on `title=` for a reader who needs it. */
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		transition: font-size 0.18s ease;
	}

	/* Condensed: one line, and a step smaller. It is chrome naming a task the reader
	   already chose, so it is the first thing that should give up room. */
	.task-panel__header--condensed .task-panel__title {
		font-size: 0.75rem;
		-webkit-line-clamp: 1;
		line-clamp: 1;
	}

	/* **The brief, and the row the owner called mandatory.** Five lines, which is
	   enough for a real task description and short enough to leave the verdict above
	   the fold. Below the title in weight and colour: the title names the task, this
	   one is what the task was asked to do, and the reader who opened the drawer is
	   usually here for the verdict below both.

	   It keeps `--text-secondary` rather than dropping a tier for the reason stated
	   throughout this feature: `--text-muted` is under 4.5:1 on `--bg-elevated` in
	   this app's light themes, so the step below the title is spelled in size. */
	.task-panel__description {
		min-width: 0;
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		line-height: 1.45;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 5;
		line-clamp: 5;
		overflow: hidden;
	}

	/* The status, at a glance. Wraps because a chip carries a plan status whose
	   length nothing here controls. */
	.task-panel__chips {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-xs);
		margin-top: var(--space-xs);
	}

	/* **Squared off, which is the one thing this surface says about a chip.**
	   `Badge` is a 999px pill by default, and a row of pills under a row of
	   `--radius-sm` controls reads as two different vocabularies in one header. The
	   colour, the tone mapping and the per-theme treatments are all the
	   component's. `:global` because `className` puts this class on an element
	   inside `Badge`, which carries `Badge`'s scope. */
	.task-panel__chips :global(.task-panel__chip) {
		border-radius: var(--radius-sm);
	}

	/**
	 * The condense is a transition, so a reader who asked for less motion gets the
	 * end state without the movement. **Stated here rather than relied on from
	 * `app.css`**, whose `prefers-reduced-motion: reduce` block lists specific
	 * class-scoped selectors and reaches nothing in this file — the same gap
	 * `.ui-no-press` exists for one component over, and the same reason it is worth
	 * writing down: the obvious reading of "there is a reduced-motion block" is that
	 * this is already handled.
	 */
	@media (prefers-reduced-motion: reduce) {
		.task-panel__header,
		.task-panel__title {
			transition: none;
		}
	}

	/* **Nothing but its place in the row.** This was thirteen declarations
	   restating `native/Button.svelte`'s outline variant at its `sm` size, with a
	   comment on each one saying so; it is now that component, and the only thing
	   left to say is that it does not stretch. `:global` because the class reaches
	   the component through `className`. */
	.task-panel__header :global(.task-panel__close) {
		flex: none;
	}

	/* ── slotted header actions ───────────────────────────────────────────
	   The buttons themselves are the surface's — `/tasks` offers Stop/Run/Reset,
	   the internal route offers `ExecutionControls`, a chat mount may offer
	   none — but they all sit in this header and all read as one row, so their
	   *look* is the shell's. `:global` because slotted content carries the
	   parent's scope class, not this component's, and a scoped rule here would
	   silently match nothing.

	   One rule rather than one per surface: five more surfaces are queued onto
	   this drawer, and five copies of a button style is five places for the
	   focus ring or the hit target to drift. */
	.task-panel__header :global(.task-panel__action) {
		flex: none;
		display: inline-flex;
		align-items: center;
		/* `native/Button.svelte`'s smallest size, matching the close control. */
		min-height: 1.75rem;
		padding: var(--space-xs) var(--space-sm);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: none;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-primary);
		cursor: pointer;
	}

	.task-panel__header :global(.task-panel__action:hover:not(:disabled)) {
		background: var(--bg-soft);
		border-color: var(--text-secondary);
	}

	.task-panel__header :global(.task-panel__action:disabled) {
		opacity: 0.4;
		cursor: default;
	}

	.task-panel__header :global(.task-panel__action:focus-visible) {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.task-panel__body {
		/**
		 * **How far the panel's content is inset from the drawer's edges — declared
		 * here, and applied by the things inside rather than by this box.**
		 *
		 * The owner's report: "each section has padding and background colors just
		 * coloring the padded box, not entire row of section." That is exactly what a
		 * horizontal padding on the scroll container does. An act's hover wash and the
		 * verdict's tone band are both backgrounds on elements *inside* this box, so
		 * with `padding: var(--space-md)` they stopped a whole `--space-md` short of
		 * the drawer on both sides — a highlight floating in a gutter rather than a
		 * band across the panel.
		 *
		 * So this box insets **nothing** horizontally, and every section pads its own
		 * content by this variable. A section's background then starts at the drawer's
		 * left edge and ends at its right, and the text inside it still lines up with
		 * every other section's.
		 *
		 * **Negative margins were the other way to do it and this is better.** Those
		 * need two numbers that must agree — the container's padding and each
		 * section's pull-back — and the failure when they drift is a section a few
		 * pixels wider than the panel, which reads as a rendering bug rather than as a
		 * mistake. Here there is one number and nothing to keep in step.
		 *
		 * Readers spell the fallback `0px`, which is a *meaning* rather than a hidden
		 * copy of this value: a panel rendered outside a drawer — the component
		 * harness does exactly that — is inset by nothing, so its sections pad by
		 * nothing. `taskPanelPresentation.test.ts` asserts one declaration and one
		 * fallback spelling.
		 */
		--task-panel-bleed: var(--space-md);

		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: var(--space-md) 0;
	}

	/* The skeleton is a sibling of the panel rather than inside it, so it reads the
	   same inset directly. Without this the loading state would run edge to edge and
	   then jump inward the moment the task arrived. */
	.task-panel__loading {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		padding: 0 var(--task-panel-bleed, 0px);
	}

	/* **Last in the file, and doubled selectors, both deliberately.** Expanding
	   trades "five lines" for "scrolls in its own box" — never for "as tall as it
	   likes", because an unbounded description in a fixed header pushes the acts off
	   screen, which is the thing the clamp existed to prevent.

	   These lost to the base rules on the first attempt. Measured: `aria-expanded`
	   flipped to `true` and the class landed, and `-webkit-line-clamp` still computed
	   to `5`, because a single-class override written *above* a single-class base
	   loses on source order. The class pair takes it to two classes so it also beats
	   `.task-panel__header--condensed .task-panel__title`, which is the selector that
	   would otherwise re-clamp an expanded title the moment the reader scrolled. */
	.task-panel__title.task-panel__title--expanded,
	.task-panel__description.task-panel__description--expanded {
		display: block;
		-webkit-line-clamp: unset;
		line-clamp: unset;
		font-size: 0.75rem;
		max-height: 40vh;
		overflow-y: auto;
		overscroll-behavior: contain;
	}
</style>
