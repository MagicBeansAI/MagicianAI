<script lang="ts">
	/**
	 * Task-card overflow menu (the `⋯` popup) with real menu semantics.
	 *
	 * The trigger is a MUIJ Button inside the task card JSON; the page routes
	 * its `menu` action here, passing the trigger element as `anchor`. The
	 * menu renders PAGE-level (not surface JSON) inside a `position: relative`
	 * wrapper around the task list, absolutely positioned under the anchor —
	 * no `position: fixed` + scroll-close hacks: it scrolls with the list.
	 *
	 * A11y contract: popup is `role="menu"` with `role="menuitem"` buttons;
	 * ArrowUp/Down/Home/End move focus, Escape closes and refocuses the
	 * trigger (shared `menuKeydown` mechanics), outside pointerdown closes
	 * (shared `clickOutside` action).
	 * The trigger's `aria-haspopup`/`aria-expanded` are managed imperatively
	 * on the anchor because MUIJ Buttons don't carry ARIA popup props.
	 *
	 * Item selection dispatches `select` with an existing ParsedTaskAction
	 * action name (schedule_today, priority_p1, menu_delete, …) so pages
	 * reuse their existing task-action dispatch unchanged; the custom date
	 * picker dispatches `dueDate` with a yyyy-mm-dd value.
	 */
	import { createEventDispatcher, onDestroy, onMount, tick } from 'svelte';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { createMenuKeydown, menuFocusableItems } from '$lib/shared/menuKeydown';

	export let anchor: HTMLElement;
	export let taskTitle = '';
	/** Manual (source === 'task') and not read-only: edit/cancel/delete section. */
	export let canEditTask = false;
	/** Non-terminal status — Cancel Task routes to the task-status endpoint. */
	export let canCancel = false;
	/**
	 * Recurring Monitors Phase 7: persistent, not already a monitor
	 * (`canConvertTaskToMonitor` in $lib/monitors/convert). Shows the
	 * explicit "Convert to monitor" item — conversion is user-driven only.
	 */
	export let canConvertToMonitor = false;
	/** Current priority ('P1'…'P4' or ''): marks the selected option. */
	export let priority = '';
	/** Raw yyyy-mm-dd due date (or '') for the custom date input. */
	export let dueDateRaw = '';
	export let hasDueDate = false;
	export let disabled = false;

	const dispatch = createEventDispatcher<{
		select: { action: string };
		dueDate: { value: string };
		close: void;
	}>();

	const PRIORITY_OPTIONS = [
		{ action: 'priority_p1', value: 'P1', label: 'P1 · Urgent' },
		{ action: 'priority_p2', value: 'P2', label: 'P2 · High' },
		{ action: 'priority_p3', value: 'P3', label: 'P3 · Medium' },
		{ action: 'priority_p4', value: 'P4', label: 'P4 · Low' }
	] as const;

	let menuEl: HTMLDivElement | null = null;
	let top = 0;
	let left = 0;
	let positioned = false;

	function position(): void {
		const host = menuEl?.offsetParent as HTMLElement | null;
		if (!menuEl || !host || !anchor?.isConnected) {
			positioned = true;
			return;
		}
		const hostRect = host.getBoundingClientRect();
		const anchorRect = anchor.getBoundingClientRect();
		const menuWidth = menuEl.offsetWidth || 220;
		const menuHeight = menuEl.offsetHeight || 0;
		// Right-align the menu to the trigger, clamped inside both the host
		// and the viewport (translated into the host's coordinate space).
		const leftBound = Math.max(hostRect.left, 8);
		const rightBound = Math.min(hostRect.right, window.innerWidth - 8);
		left =
			Math.max(leftBound, Math.min(anchorRect.right - menuWidth, rightBound - menuWidth)) -
			hostRect.left;
		// The menu is tall (due date + priority + edit sections). Dropping it
		// blindly below a low card ran it past the scrollport's edge, where the
		// route shell's `overflow: hidden` culled it. Flip above the anchor
		// when the viewport bottom is closer than the menu is tall; when
		// neither side fits, take the larger side and cap with a scroll.
		const belowTop = anchorRect.bottom + 4;
		const spaceBelow = window.innerHeight - 8 - belowTop;
		const spaceAbove = anchorRect.top - 8 - 4;
		if (menuHeight <= spaceBelow) {
			top = belowTop - hostRect.top;
		} else if (menuHeight <= spaceAbove) {
			top = anchorRect.top - menuHeight - 4 - hostRect.top;
		} else if (spaceAbove > spaceBelow) {
			top = 8 - hostRect.top;
			menuEl.style.maxHeight = `${Math.max(180, anchorRect.top - 12)}px`;
		} else {
			top = belowTop - hostRect.top;
			menuEl.style.maxHeight = `${Math.max(180, spaceBelow)}px`;
		}
		positioned = true;
	}

	function close(refocusTrigger: boolean): void {
		if (refocusTrigger && anchor?.isConnected) {
			anchor.focus();
		}
		dispatch('close');
	}

	function choose(action: string): void {
		if (disabled) return;
		dispatch('select', { action });
	}

	function handleDateChange(event: Event): void {
		if (disabled) return;
		const value = (event.currentTarget as HTMLInputElement).value.trim();
		if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return;
		dispatch('dueDate', { value });
	}

	const handleKeydown = createMenuKeydown({ getMenuEl: () => menuEl, close });

	onMount(async () => {
		anchor?.setAttribute('aria-haspopup', 'menu');
		anchor?.setAttribute('aria-expanded', 'true');
		await tick();
		position();
		menuFocusableItems(menuEl)[0]?.focus();
	});

	onDestroy(() => {
		if (anchor?.isConnected) {
			anchor.setAttribute('aria-expanded', 'false');
		}
	});
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
	bind:this={menuEl}
	class="task-card-menu"
	role="menu"
	aria-orientation="vertical"
	aria-label={taskTitle ? `Actions for ${taskTitle}` : 'Task actions'}
	tabindex="-1"
	style={`top:${top}px;left:${left}px;visibility:${positioned ? 'visible' : 'hidden'};`}
	use:clickOutside={{ handler: () => close(false), exclude: [anchor] }}
	on:keydown={handleKeydown}
>
	<div class="tcm-section-label" aria-hidden="true">Due date</div>
	<button type="button" role="menuitem" class="tcm-item" {disabled} on:click={() => choose('schedule_today')}>
		Today
	</button>
	<button type="button" role="menuitem" class="tcm-item" {disabled} on:click={() => choose('schedule_tomorrow')}>
		Tomorrow
	</button>
	<button type="button" role="menuitem" class="tcm-item" {disabled} on:click={() => choose('schedule_next_week')}>
		Next week
	</button>
	<label class="tcm-date-row">
		<span>Custom</span>
		<input type="date" value={dueDateRaw} {disabled} on:change={handleDateChange} />
	</label>
	{#if hasDueDate}
		<button type="button" role="menuitem" class="tcm-item tcm-item-danger" {disabled} on:click={() => choose('schedule_clear')}>
			Remove due date
		</button>
	{/if}

	<div class="tcm-divider" role="separator"></div>
	<div class="tcm-section-label" aria-hidden="true">Priority</div>
	{#each PRIORITY_OPTIONS as option (option.action)}
		<button
			type="button"
			role="menuitem"
			class="tcm-item tcm-priority tcm-priority-{option.value.toLowerCase()}"
			class:is-selected={priority === option.value}
			{disabled}
			on:click={() => choose(option.action)}
		>
			{option.label}
		</button>
	{/each}
	{#if priority}
		<button type="button" role="menuitem" class="tcm-item tcm-item-danger" {disabled} on:click={() => choose('priority_clear')}>
			Remove priority
		</button>
	{/if}

	{#if canEditTask}
		<div class="tcm-divider" role="separator"></div>
		{#if canConvertToMonitor}
			<button type="button" role="menuitem" class="tcm-item" {disabled} on:click={() => choose('convert_monitor')}>
				Convert to monitor
			</button>
		{/if}
		<button type="button" role="menuitem" class="tcm-item" {disabled} on:click={() => choose('edit_description')}>
			Edit description
		</button>
		{#if canCancel}
			<button type="button" role="menuitem" class="tcm-item" {disabled} on:click={() => choose('cancel')}>
				Cancel task
			</button>
		{/if}
		<button type="button" role="menuitem" class="tcm-item tcm-item-danger" {disabled} on:click={() => choose('menu_delete')}>
			Delete…
		</button>
	{/if}
</div>

<style>
	.task-card-menu {
		position: absolute;
		z-index: 120;
		min-width: 200px;
		max-width: 260px;
		padding: 6px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		box-shadow: var(--shadow-md, 0 4px 12px rgba(0, 0, 0, 0.12));
		display: flex;
		flex-direction: column;
		gap: 1px;
		overflow-y: auto;
	}

	.tcm-section-label {
		font-size: var(--text-2xs);
		font-weight: 600;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--text-muted);
		padding: 4px 8px 2px;
	}

	.tcm-item {
		appearance: none;
		border: none;
		background: transparent;
		text-align: left;
		width: 100%;
		font-family: var(--font-primary);
		font-size: 0.78rem;
		color: var(--text-primary);
		padding: 5px 8px;
		border-radius: var(--radius-sm, 0.375rem);
		cursor: pointer;
	}

	.tcm-item:hover:not(:disabled),
	.tcm-item:focus-visible {
		background: var(--bg-soft);
		outline: none;
	}

	.tcm-item:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.tcm-item.is-selected {
		background: var(--bg-soft);
		font-weight: 600;
	}

	.tcm-priority-p1 {
		color: var(--color-error, #d35c5c);
	}
	.tcm-priority-p2 {
		color: var(--color-warning, #d4a843);
	}
	.tcm-priority-p3 {
		color: var(--accent-secondary, #6b9080);
	}
	.tcm-priority-p4 {
		color: var(--text-muted);
	}

	.tcm-item-danger {
		color: var(--color-error, #d35c5c);
	}

	.tcm-divider {
		height: 1px;
		background: var(--border-soft);
		margin: 4px 2px;
	}

	.tcm-date-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 3px 8px;
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	.tcm-date-row input {
		font-family: var(--font-primary);
		font-size: 0.72rem;
		color: var(--text-primary);
		background: var(--input-bg, var(--bg-card));
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: var(--radius-sm, 0.375rem);
		padding: 2px 4px;
		min-width: 0;
	}
</style>
