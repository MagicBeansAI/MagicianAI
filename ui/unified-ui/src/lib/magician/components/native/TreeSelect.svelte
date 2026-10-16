<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	interface TreeSelectChild {
		value: string;
		label: string;
	}

	interface TreeSelectGroup {
		value: string;
		label: string;
		description?: string;
		children: TreeSelectChild[];
	}

	export let label = '';
	export let groups: TreeSelectGroup[] = [];
	export let values: string[] = [];
	export let disabled = false;
	export let idBase = '';
	export let single = false;

	const dispatch = createEventDispatcher<{ change: { values: string[]; groupValue?: string } }>();

	let selected = new Set<string>(values);
	let expandedGroups = new Set<string>();

	$: normalizedValues = new Set(
		Array.isArray(values) ? values.filter((value): value is string => typeof value === 'string') : []
	);
	$: if (!setEquals(selected, normalizedValues)) {
		selected = new Set(normalizedValues);
	}
	$: safeGroups = normalizeGroups(groups);

	function setEquals(left: Set<string>, right: Set<string>): boolean {
		if (left.size !== right.size) return false;
		for (const value of left) {
			if (!right.has(value)) return false;
		}
		return true;
	}

	function normalizeGroups(input: TreeSelectGroup[]): TreeSelectGroup[] {
		if (!Array.isArray(input)) return [];
		return input
			.filter(
				(group): group is TreeSelectGroup =>
					group != null &&
					typeof group === 'object' &&
					typeof group.value === 'string' &&
					typeof group.label === 'string' &&
					Array.isArray(group.children)
			)
			.map((group) => ({
				value: group.value,
				label: group.label,
				description: group.description,
				children: group.children
					.filter(
						(child): child is TreeSelectChild =>
							child != null &&
							typeof child === 'object' &&
							typeof child.value === 'string' &&
							typeof child.label === 'string'
					)
					.map((child) => ({ value: child.value, label: child.label }))
			}));
	}

	function groupState(group: TreeSelectGroup, current: Set<string>): 'all' | 'some' | 'none' {
		if (group.children.length === 0) return 'none';
		const selectedCount = group.children.filter((child) => current.has(child.value)).length;
		if (selectedCount === group.children.length) return 'all';
		if (selectedCount > 0) return 'some';
		return 'none';
	}

	function toggleExpand(groupValue: string): void {
		if (single) {
			expandedGroups = expandedGroups.has(groupValue) ? new Set() : new Set([groupValue]);
			return;
		}
		const next = new Set(expandedGroups);
		if (next.has(groupValue)) next.delete(groupValue);
		else next.add(groupValue);
		expandedGroups = next;
	}

	function toggleGroup(group: TreeSelectGroup): void {
		if (single) {
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
			selected = new Set([childValue]);
			dispatch('change', { values: [childValue], groupValue: parentGroupValue });
			return;
		}
		const next = new Set(selected);
		if (next.has(childValue)) next.delete(childValue);
		else next.add(childValue);
		selected = next;
		dispatch('change', { values: [...next] });
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

	function childId(groupIndex: number, childIndex: number): string {
		return `${idBase || 'native-tree'}-${groupIndex}-${childIndex}`;
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
<fieldset class="native-tree-select" {disabled} role="tree" aria-label={label || 'Tree select'}>
	{#if label}
		<legend class="native-tree-select__legend">{label}</legend>
	{/if}
	{#if single && selected.size > 0 && expandedGroups.size === 0}
		{@const selectedValue = [...selected][0]}
		{@const parentGroup = safeGroups.find((group) => group.children.some((child) => child.value === selectedValue))}
		<div class="native-tree-select__summary">
			<span class="native-tree-select__summary-chip">{selectedValue}</span>
			{#if parentGroup}
				<span class="native-tree-select__summary-via">via {parentGroup.label}</span>
			{/if}
		</div>
	{/if}

	{#each safeGroups as group, groupIndex (group.value)}
		{@const state = groupState(group, selected)}
		{@const expanded = expandedGroups.has(group.value)}
		{@const selectedChild = single ? group.children.find((child) => selected.has(child.value)) : null}
		<!-- svelte-ignore a11y_role_has_required_aria_props -->
		<div
			class="native-tree-select__group"
			role="treeitem"
			aria-expanded={expanded}
			aria-selected="false"
			aria-checked={state === 'all' ? 'true' : state === 'some' ? 'mixed' : 'false'}
		>
			<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
			<div
				class="native-tree-select__header"
				tabindex="0"
				role="button"
				on:keydown={(event) => handleGroupKeydown(event, group)}
				on:click={() => {
					if (single) toggleExpand(group.value);
				}}
			>
				<button
					type="button"
					class={['native-tree-select__arrow', expanded ? 'native-tree-select__arrow--expanded' : '']
						.filter(Boolean)
						.join(' ')}
					aria-label={expanded ? 'Collapse group' : 'Expand group'}
					on:click|stopPropagation={() => toggleExpand(group.value)}
				>
					›
				</button>
				{#if !single}
					<input
						class="native-tree-select__check"
						type="checkbox"
						checked={state === 'all'}
						indeterminate={state === 'some'}
						on:change|stopPropagation={() => toggleGroup(group)}
						{disabled}
					/>
				{/if}
				<button
					type="button"
					class="native-tree-select__group-label"
					on:click|stopPropagation={() => toggleExpand(group.value)}
				>
					{group.label}
				</button>
				{#if single && selectedChild && !expanded}
					<span class="native-tree-select__selected-badge">{selectedChild.label}</span>
				{/if}
			</div>
			{#if expanded}
				<div class="native-tree-select__children" role="group">
					{#each group.children as child, childIndex (child.value)}
						<label class="native-tree-select__child" for={childId(groupIndex, childIndex)}>
							<input
								id={childId(groupIndex, childIndex)}
								class="native-tree-select__check"
								type={single ? 'radio' : 'checkbox'}
								name={single ? `${idBase || 'native-tree'}-single-select` : undefined}
								checked={selected.has(child.value)}
								on:change={() => toggleChild(child.value, group.value)}
								{disabled}
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
	.native-tree-select {
		display: grid;
		gap: 0.25rem;
		margin: 0;
		padding: 0;
		border: 0;
	}

	.native-tree-select__legend {
		margin-bottom: 0.25rem;
		padding: 0;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		font-weight: 600;
	}

	.native-tree-select__group {
		overflow: hidden;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-card);
	}

	.native-tree-select__header {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		padding: 0.375rem 0.625rem;
		color: var(--text-primary);
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 700;
		transition: background 120ms ease;
	}

	.native-tree-select__header:hover {
		background: var(--bg-soft);
	}

	.native-tree-select__header:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	.native-tree-select__arrow {
		display: inline-grid;
		place-items: center;
		width: 1rem;
		height: 1rem;
		border: 0;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		font: inherit;
		padding: 0;
		transition: transform 120ms ease;
	}

	.native-tree-select__arrow--expanded {
		transform: rotate(90deg);
	}

	.native-tree-select__group-label {
		flex: 1;
		min-width: 0;
		border: 0;
		background: transparent;
		color: inherit;
		cursor: pointer;
		font: inherit;
		padding: 0;
		text-align: left;
		overflow-wrap: anywhere;
	}

	.native-tree-select__selected-badge,
	.native-tree-select__summary-chip {
		border-radius: 999px;
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		color: var(--accent-primary);
		font-size: 0.6875rem;
		font-weight: 700;
		padding: 0.0625rem 0.375rem;
	}

	.native-tree-select__summary {
		display: flex;
		align-items: center;
		gap: 0.375rem;
		padding: 0.25rem 0.625rem;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.6875rem;
	}

	.native-tree-select__summary-via {
		color: var(--text-muted);
	}

	.native-tree-select__children {
		display: grid;
		gap: 0.125rem;
		border-top: 1px solid var(--border-soft);
		padding: 0.25rem 0.625rem 0.375rem 2rem;
	}

	.native-tree-select__child {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--text-body);
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		padding: 0.125rem 0;
	}

	.native-tree-select__check {
		accent-color: var(--accent-primary);
		margin: 0;
	}

	.native-tree-select[disabled] {
		opacity: 0.55;
		pointer-events: none;
	}

	:global([data-theme^='retro-16bit']) .native-tree-select__group,
	:global([data-theme^='retro-16bit']) .native-tree-select__selected-badge,
	:global([data-theme^='retro-16bit']) .native-tree-select__summary-chip {
		border-radius: 0;
	}

	:global([data-theme^='retro-16bit']) .native-tree-select__legend,
	:global([data-theme^='retro-16bit']) .native-tree-select__header,
	:global([data-theme^='retro-16bit']) .native-tree-select__child,
	:global([data-theme^='retro-16bit']) .native-tree-select__summary {
		font-family: var(--font-mono);
		text-transform: uppercase;
	}
</style>
