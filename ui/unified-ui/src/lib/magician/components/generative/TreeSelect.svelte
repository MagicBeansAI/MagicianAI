<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	interface TreeSelectChild {
		value: string;
		label: string;
	}

	interface TreeSelectGroup {
		value: string;
		label: string;
		children: TreeSelectChild[];
	}

	export let label: string = '';
	export let groups: TreeSelectGroup[] = [];
	export let values: string[] = [];
	export let disabled: boolean = false;
	export let idBase: string = '';
	export let single: boolean = false;

	const dispatch = createEventDispatcher<{ change: { values: string[]; groupValue?: string } }>();

	let selected = new Set<string>(values);
	let expandedGroups = new Set<string>();

	function setEquals(left: Set<string>, right: Set<string>): boolean {
		if (left.size !== right.size) return false;
		for (const v of left) {
			if (!right.has(v)) return false;
		}
		return true;
	}

	$: normalizedValues = new Set(
		Array.isArray(values) ? values.filter((v): v is string => typeof v === 'string') : []
	);
	$: if (!setEquals(selected, normalizedValues)) {
		selected = new Set(normalizedValues);
	}

	$: safeGroups = normalizeGroups(groups);

	function normalizeGroups(input: TreeSelectGroup[]): TreeSelectGroup[] {
		if (!Array.isArray(input)) return [];
		return input
			.filter(
				(g): g is TreeSelectGroup =>
					g != null &&
					typeof g === 'object' &&
					typeof g.value === 'string' &&
					typeof g.label === 'string' &&
					Array.isArray(g.children)
			)
			.map((g) => ({
				value: g.value,
				label: g.label,
				children: g.children
					.filter(
						(c): c is TreeSelectChild =>
							c != null &&
							typeof c === 'object' &&
							typeof c.value === 'string' &&
							typeof c.label === 'string'
					)
					.map((c) => ({ value: c.value, label: c.label }))
			}));
	}

	function groupState(
		group: TreeSelectGroup,
		sel: Set<string>
	): 'all' | 'some' | 'none' {
		if (group.children.length === 0) return 'none';
		let count = 0;
		for (const child of group.children) {
			if (sel.has(child.value)) count++;
		}
		if (count === group.children.length) return 'all';
		if (count > 0) return 'some';
		return 'none';
	}

	function toggleGroup(group: TreeSelectGroup): void {
		if (single) {
			// In single mode, group headers only expand/collapse
			toggleExpand(group.value);
			return;
		}
		const state = groupState(group, selected);
		const next = new Set(selected);
		if (state === 'all') {
			for (const child of group.children) next.delete(child.value);
		} else {
			for (const child of group.children) next.add(child.value);
		}
		selected = next;
		dispatch('change', { values: [...next] });
	}

	function toggleChild(childValue: string, parentGroupValue?: string): void {
		if (single) {
			// In single mode, clear all others and set only this value
			const next = new Set<string>([childValue]);
			selected = next;
			// Include the parent group so callers know which agent/category was chosen
			const groupValue = parentGroupValue
				?? safeGroups.find(g => g.children.some(c => c.value === childValue))?.value;
			dispatch('change', { values: [childValue], groupValue });
			return;
		}
		const next = new Set(selected);
		if (next.has(childValue)) next.delete(childValue);
		else next.add(childValue);
		selected = next;
		dispatch('change', { values: [...next] });
	}

	function toggleExpand(groupValue: string): void {
		if (single) {
			// In single mode, auto-collapse other groups
			expandedGroups = expandedGroups.has(groupValue) ? new Set() : new Set([groupValue]);
			return;
		}
		const next = new Set(expandedGroups);
		if (next.has(groupValue)) next.delete(groupValue);
		else next.add(groupValue);
		expandedGroups = next;
	}

	function handleGroupKeydown(event: KeyboardEvent, group: TreeSelectGroup): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			toggleGroup(group);
		} else if (event.key === 'ArrowRight') {
			event.preventDefault();
			expandedGroups = new Set([...expandedGroups, group.value]);
		} else if (event.key === 'ArrowLeft') {
			event.preventDefault();
			const next = new Set(expandedGroups);
			next.delete(group.value);
			expandedGroups = next;
		}
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
<fieldset class="muij-treeselect" disabled={disabled} role="tree" aria-label={label || 'Tree select'}>
	{#if label}
		<legend class="muij-treeselect-legend">{label}</legend>
	{/if}
	{#if single && selected.size > 0 && expandedGroups.size === 0}
		{@const selectedValue = [...selected][0]}
		{@const parentGroup = safeGroups.find(g => g.children.some(c => c.value === selectedValue))}
		<div class="muij-treeselect-selection-summary">
			<span class="muij-treeselect-selection-chip">{selectedValue}</span>
			{#if parentGroup}
				<span class="muij-treeselect-selection-via">via {parentGroup.label}</span>
			{/if}
		</div>
	{/if}
	{#each safeGroups as group, gIdx (group.value)}
		{@const gState = groupState(group, selected)}
		{@const isExpanded = expandedGroups.has(group.value)}
		{@const selectedChild = single ? group.children.find(c => selected.has(c.value)) : null}
		<!-- svelte-ignore a11y_role_has_required_aria_props -->
		<div class="muij-treeselect-group" role="treeitem" aria-expanded={isExpanded} aria-selected="false" aria-checked={gState === 'all' ? 'true' : gState === 'some' ? 'mixed' : 'false'}>
			<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
			<div
				class="muij-treeselect-group-header"
				tabindex="0"
				role="button"
				on:keydown={(e) => handleGroupKeydown(e, group)}
				on:click={() => { if (single) toggleExpand(group.value); }}
			>
				<span
					class="muij-treeselect-arrow"
					class:expanded={isExpanded}
					aria-hidden="true"
					on:click|stopPropagation={() => toggleExpand(group.value)}
				>&#x25B8;</span>
				{#if !single}
					<input
						id={buildStableDomId(`muij-ts-g-${gIdx}`, idBase)}
						type="checkbox"
						checked={gState === 'all'}
						indeterminate={gState === 'some'}
						on:change|stopPropagation={() => toggleGroup(group)}
						disabled={disabled}
					/>
				{/if}
				<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
				<span class="muij-treeselect-group-label" on:click|stopPropagation={() => toggleExpand(group.value)}>{group.label}</span>
				{#if single && selectedChild && !isExpanded}
					<span class="muij-treeselect-selected-badge">{selectedChild.label}</span>
				{/if}
			</div>
			{#if isExpanded}
				<div class="muij-treeselect-children" role="group">
					{#each group.children as child, cIdx (child.value)}
						<label class="muij-treeselect-child" for={buildStableDomId(`muij-ts-c-${gIdx}-${cIdx}`, idBase)}>
							<input
								id={buildStableDomId(`muij-ts-c-${gIdx}-${cIdx}`, idBase)}
								type={single ? 'radio' : 'checkbox'}
								name={single ? `${idBase}-single-select` : undefined}
								checked={selected.has(child.value)}
								on:change={() => toggleChild(child.value, group.value)}
								disabled={disabled}
							/>
							<span>{child.label}</span>
						</label>
					{/each}
				</div>
			{/if}
		</div>
	{/each}
</fieldset>

<style>
	.muij-treeselect {
		border: none;
		padding: 0;
		margin: 0;
		display: grid;
		gap: 4px;
	}

	.muij-treeselect-legend {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
		padding: 0;
		margin-bottom: 4px;
	}

	.muij-treeselect-group {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		overflow: hidden;
	}

	.muij-treeselect-group-header {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 10px;
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-primary);
		background: transparent;
		border: none;
		width: 100%;
		text-align: left;
		transition: background var(--transition-fast);
	}

	.muij-treeselect-group-header:hover {
		background: var(--bg-soft);
	}

	.muij-treeselect-group-header:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -1px;
	}

	.muij-treeselect-arrow {
		display: inline-flex;
		color: var(--text-muted);
		transition: transform var(--transition-fast);
		font-size: 0.625rem;
		flex-shrink: 0;
		cursor: pointer;
	}

	.muij-treeselect-arrow.expanded {
		transform: rotate(90deg);
	}

	.muij-treeselect-group-label {
		flex: 1;
		cursor: pointer;
	}

	.muij-treeselect-selected-badge {
		font-size: 0.625rem;
		font-weight: 500;
		color: var(--accent-primary, #2563eb);
		background: color-mix(in srgb, var(--accent-primary, #2563eb) 12%, transparent);
		padding: 1px 6px;
		border-radius: 9999px;
		white-space: nowrap;
		flex-shrink: 0;
	}

	.muij-treeselect-selection-summary {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 4px 10px;
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-treeselect-selection-chip {
		font-weight: 600;
		color: var(--accent-primary, #2563eb);
	}

	.muij-treeselect-selection-via {
		color: var(--text-muted);
	}

	.muij-treeselect-children {
		display: grid;
		gap: 2px;
		padding: 2px 10px 6px 32px;
		border-top: 1px solid var(--border-soft);
	}

	.muij-treeselect-child {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
		padding: 2px 0;
		cursor: pointer;
	}

	.muij-treeselect[disabled] {
		opacity: 0.55;
		pointer-events: none;
	}

	/* --- Retro 16-bit Theme Overrides --- */
	:global([data-theme^="retro-16bit"]) .muij-treeselect-group {
		border-radius: 0;
		border: 1px solid var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-treeselect-group-header {
		font-family: var(--font-mono);
		text-transform: uppercase;
		font-size: 0.7rem;
	}

	:global([data-theme^="retro-16bit"]) .muij-treeselect-legend {
		font-family: var(--font-mono);
		text-transform: uppercase;
		color: var(--text-primary);
		font-size: 0.65rem;
	}

	:global([data-theme^="retro-16bit"]) .muij-treeselect-child {
		font-family: var(--font-mono);
		font-size: 0.7rem;
	}

	:global([data-theme^="retro-16bit"]) .muij-treeselect-children {
		border-top: 1px dashed var(--text-muted);
	}

	:global([data-theme^="retro-16bit"]) .muij-treeselect-selected-badge {
		font-family: var(--font-mono);
		border-radius: 0;
		font-size: 0.6rem;
	}

	:global([data-theme^="retro-16bit"]) .muij-treeselect-selection-summary {
		font-family: var(--font-mono);
		font-size: 0.6rem;
	}
</style>
