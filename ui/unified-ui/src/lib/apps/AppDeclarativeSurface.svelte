<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import AppSurfaceEditor from './AppSurfaceEditor.svelte';
	import type {
		AppSurfaceComponent,
		AppSurfaceFieldBinding,
		AppSurfaceRecord
	} from './appSurfaceRuntime';

	export let components: AppSurfaceComponent[] = [];
	export let records: AppSurfaceRecord[] = [];
	export let fieldBindings: AppSurfaceFieldBinding[] = [];
	export let saving = false;
	export let sortField = '';
	export let sortDirection: 'ascending' | 'descending' | undefined = undefined;

	const dispatch = createEventDispatcher<{
		create: { values: Record<string, unknown> };
		edit: { record: AppSurfaceRecord };
		delete: { record: AppSurfaceRecord };
		sort: { field: string; direction: 'ascending' | 'descending' };
	}>();

	function bindingsFor(fields: string[]): AppSurfaceFieldBinding[] {
		const selected = new Set(fields);
		return fieldBindings.filter((binding) => selected.has(binding.field));
	}

	function fieldLabel(field: string): string {
		return field
			.split(/[_-]+/g)
			.filter(Boolean)
			.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
			.join(' ');
	}

	function fieldValue(record: AppSurfaceRecord, field: string): string {
		const value = record.fields[field];
		if (value === null || value === undefined || value === '') return '—';
		if (typeof value === 'boolean') return value ? 'Yes' : 'No';
		return String(value);
	}

	function nextSortDirection(field: string): 'ascending' | 'descending' {
		return sortField === field && sortDirection === 'ascending' ? 'descending' : 'ascending';
	}

	function sortable(field: string): boolean {
		return fieldBindings.some((binding) => binding.field === field && binding.sortable);
	}
</script>

<div class="declarative-components">
	{#each components as component (component.id)}
		{#if component.kind === 'section'}
			<section class="bound-section" aria-labelledby={`surface-section-${component.id}`}>
				<h2 id={`surface-section-${component.id}`}>{component.label}</h2>
				<svelte:self
					components={component.children}
					{records}
					{fieldBindings}
					{saving}
					{sortField}
					{sortDirection}
					on:create
					on:edit
					on:delete
					on:sort
				/>
			</section>
		{:else if component.kind === 'detail'}
			<section class="bound-card" aria-labelledby={`surface-detail-${component.id}`}>
				<h2 id={`surface-detail-${component.id}`}>{component.label}</h2>
				{#if records[0]}
					<dl class="detail-grid">
						{#each component.fields as field}
							<div><dt>{fieldLabel(field)}</dt><dd>{fieldValue(records[0], field)}</dd></div>
						{/each}
					</dl>
					<div class="record-actions">
						<button type="button" disabled={saving} on:click={() => dispatch('edit', { record: records[0] })}>Edit</button>
						<button class="danger" type="button" disabled={saving} on:click={() => dispatch('delete', { record: records[0] })}>Delete</button>
					</div>
				{:else}
					<p class="empty">No record is available.</p>
				{/if}
			</section>
		{:else if component.kind === 'form'}
			<section class="bound-form" aria-labelledby={`surface-form-${component.id}`}>
				<h2 class="sr-only" id={`surface-form-${component.id}`}>{component.label}</h2>
				<AppSurfaceEditor
					fields={bindingsFor(component.fields)}
					mode="create"
					{saving}
					embedded={true}
					on:save={(event) => dispatch('create', event.detail)}
				/>
			</section>
		{:else if component.kind === 'list'}
			<section class="bound-card" aria-labelledby={`surface-list-${component.id}`}>
				<h2 id={`surface-list-${component.id}`}>{component.label}</h2>
				{#if records.length > 0}
					<ul class="record-list">
						{#each records as record (record.record_id)}
							<li>
								<dl>
									{#each component.fields as field}
										<div><dt>{fieldLabel(field)}</dt><dd>{fieldValue(record, field)}</dd></div>
									{/each}
								</dl>
								<div class="record-actions">
									<button type="button" disabled={saving} on:click={() => dispatch('edit', { record })}>Edit</button>
									<button class="danger" type="button" disabled={saving} on:click={() => dispatch('delete', { record })}>Delete</button>
								</div>
							</li>
						{/each}
					</ul>
				{:else}
					<p class="empty">No records are available.</p>
				{/if}
			</section>
		{:else}
			<section class="bound-card" aria-labelledby={`surface-table-${component.id}`}>
				<h2 id={`surface-table-${component.id}`}>{component.label}</h2>
				<div class="table-scroll">
					<table>
						<caption class="sr-only">{component.label}</caption>
						<thead><tr>
							{#each component.columns as field}
								<th scope="col">
									{#if sortable(field)}
										<button
											type="button"
											disabled={saving}
											aria-label={`Sort ${fieldLabel(field)} ${nextSortDirection(field)}`}
											on:click={() => dispatch('sort', { field, direction: nextSortDirection(field) })}
										>{fieldLabel(field)}{sortField === field ? (sortDirection === 'descending' ? ' ↓' : ' ↑') : ''}</button>
									{:else}{fieldLabel(field)}{/if}
								</th>
							{/each}
							<th scope="col"><span class="sr-only">Record actions</span></th>
						</tr></thead>
						<tbody>
							{#each records as record (record.record_id)}
								<tr>
									{#each component.columns as field}<td>{fieldValue(record, field)}</td>{/each}
									<td class="table-actions">
										<button type="button" disabled={saving} on:click={() => dispatch('edit', { record })}>Edit</button>
										<button class="danger" type="button" disabled={saving} on:click={() => dispatch('delete', { record })}>Delete</button>
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
				{#if records.length === 0}<p class="empty">No records are available.</p>{/if}
			</section>
		{/if}
	{/each}
</div>

<style>
	.declarative-components { display: grid; gap: 1rem; min-width: 0; }
	.bound-section, .bound-card { min-width: 0; border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 14px); padding: clamp(.8rem, 2vw, 1.15rem); background: var(--bg-card); }
	.bound-section { display: grid; gap: .8rem; }
	h2 { margin: 0 0 .75rem; color: var(--text-primary); font-size: 1rem; }
	.bound-section > h2 { margin: 0; font-size: 1.12rem; }
	.detail-grid, .record-list dl { display: grid; gap: .55rem; margin: 0; }
	.detail-grid { grid-template-columns: repeat(auto-fit, minmax(min(12rem, 100%), 1fr)); }
	.detail-grid > div, .record-list dl > div { min-width: 0; }
	dt { color: var(--text-muted); font-size: .75rem; font-weight: 700; }
	dd { margin: .15rem 0 0; color: var(--text-primary); overflow-wrap: anywhere; }
	.record-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(17rem, 100%), 1fr)); gap: .75rem; padding: 0; margin: 0; list-style: none; }
	.record-list li { min-width: 0; border: 1px solid var(--border-soft); border-radius: var(--radius-md, 10px); padding: .8rem; background: var(--bg-soft); }
	.record-actions, .table-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: .4rem; margin-top: .75rem; }
	button { border: 1px solid var(--border-soft); border-radius: var(--radius-full, 999px); padding: .38rem .65rem; background: var(--bg-card); color: var(--text-secondary); font: inherit; font-size: .78rem; font-weight: 650; cursor: pointer; }
	button:disabled { opacity: .55; cursor: default; }
	button.danger { color: var(--color-error, #c43d4d); }
	.table-scroll { max-width: 100%; overflow-x: auto; border-radius: var(--radius-md, 10px); }
	table { width: 100%; min-width: 32rem; border-collapse: collapse; color: var(--text-primary); font-size: .84rem; }
	th, td { border-bottom: 1px solid var(--border-soft); padding: .65rem; text-align: left; vertical-align: top; overflow-wrap: anywhere; }
	th { color: var(--text-muted); font-size: .75rem; }
	th button { border: 0; padding: 0; background: transparent; color: inherit; }
	.table-actions { white-space: nowrap; margin: 0; }
	.empty { margin: .5rem 0 0; color: var(--text-muted); }
	.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
	@media (max-width: 720px) {
		.bound-section, .bound-card { padding: .7rem; }
		.detail-grid, .record-list { grid-template-columns: 1fr; }
	}
</style>
