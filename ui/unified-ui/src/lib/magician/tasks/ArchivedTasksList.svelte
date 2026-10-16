<script lang="ts">
	/**
	 * Archived tasks, paged by the server (`GET /v3/tasks?archived=only`).
	 *
	 * Archived tasks never enter the task store's corpus, so this list reads
	 * its own pages instead of filtering the store. Read-only: a row expands to
	 * show the task's description.
	 */
	import Select from '$lib/magician/components/native/Select.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

	interface ArchivedTaskRow {
		id: string;
		title: string;
		description: string;
		agent_id: string;
		updated_at: number | string;
	}

	// The same sizes the Work task list offers.
	const PAGE_SIZES = [25, 50, 100, 250];

	let pageSize = PAGE_SIZES[0];

	let search = '';
	let appliedSearch = '';
	let offset = 0;
	let total = 0;
	let rows: ArchivedTaskRow[] = [];
	let loading = false;
	let error: string | null = null;
	let expandedId: string | null = null;
	let generation = 0;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;

	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: void load(scopeKey, appliedSearch, offset, pageSize);
	$: pageStart = total === 0 ? 0 : offset + 1;
	$: pageEnd = Math.min(offset + pageSize, total);
	// One prop set for both pagers, so the copies above and below the list
	// cannot disagree.
	$: pagerProps = {
		currentPage: Math.floor(offset / pageSize) + 1,
		pageCount: Math.max(1, Math.ceil(total / pageSize)),
		startItem: pageStart,
		endItem: pageEnd,
		totalItems: total
	};

	function gotoPage(page: number): void {
		offset = (Math.max(1, page) - 1) * pageSize;
	}

	function setPageSize(size: number): void {
		if (!Number.isFinite(size) || size === pageSize) return;
		pageSize = size;
		offset = 0;
	}

	async function load(_scope: string, query: string, pageOffset: number, size: number): Promise<void> {
		const current = ++generation;
		loading = true;
		error = null;
		const params = new URLSearchParams({
			archived: 'only',
			limit: String(size),
			offset: String(pageOffset)
		});
		if (query) params.set('query', query);
		try {
			const response = await fetch(`/api/magician/v3/tasks?${params}`, {
				headers: scopedRequestHeaders({ Accept: 'application/json' })
			});
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const body = await response.json();
			if (current !== generation) return;
			rows = Array.isArray(body?.tasks) ? body.tasks : [];
			total = Number(body?.pagination?.total ?? rows.length);
		} catch (err) {
			if (current !== generation) return;
			error = err instanceof Error ? err.message : 'Archived tasks could not be loaded.';
			rows = [];
			total = 0;
		} finally {
			if (current === generation) loading = false;
		}
	}

	function onSearch(value: string): void {
		search = value;
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(() => {
			appliedSearch = search.trim().toLowerCase();
			offset = 0;
		}, 250);
	}

	function formatWhen(value: number | string): string {
		const date = typeof value === 'number' ? new Date(value) : new Date(Date.parse(value));
		return Number.isNaN(date.getTime()) ? '' : date.toLocaleString();
	}
</script>

<section class="archived-tasks" aria-label="Archived tasks">
	<div class="archived-tasks__head">
		<input
			type="search"
			value={search}
			placeholder="Search archived tasks"
			aria-label="Search archived tasks"
			on:input={(event) => onSearch(event.currentTarget.value)}
		/>
		{#if total > PAGE_SIZES[0]}
			<div class="archived-tasks__size">
				<Select
					label="Page size"
					value={String(pageSize)}
					options={PAGE_SIZES.map((size) => ({ value: String(size), label: String(size) }))}
					interactive={true}
					on:change={(event) => setPageSize(parseInt(event.detail.value, 10))}
				/>
			</div>
		{/if}
		{#if total > pageSize}
			<ServerPager
				{...pagerProps}
				ariaLabel="Archived task pages, above the list"
				{loading}
				on:pagechange={(event) => gotoPage(event.detail.page)}
			/>
		{:else}
			<span class="archived-tasks__count">{pageStart}–{pageEnd} of {total}</span>
		{/if}
	</div>

	{#if loading && rows.length === 0}
		<p class="archived-tasks__note" aria-busy="true">Loading archived tasks…</p>
	{:else if error}
		<p class="archived-tasks__note" role="alert">
			Archived tasks could not be loaded ({error}).
			<button type="button" on:click={() => load(scopeKey, appliedSearch, offset, pageSize)}>Retry</button>
		</p>
	{:else if rows.length === 0}
		<p class="archived-tasks__note">{appliedSearch ? 'No matching archived tasks.' : 'No archived tasks.'}</p>
	{:else}
		<ul class="archived-tasks__list" aria-busy={loading}>
			{#each rows as row (row.id)}
				<li>
					<button
						type="button"
						class="archived-tasks__row"
						aria-expanded={expandedId === row.id}
						on:click={() => (expandedId = expandedId === row.id ? null : row.id)}
					>
						<span class="archived-tasks__title">{row.title || row.id}</span>
						<span class="archived-tasks__meta">{row.agent_id} · {formatWhen(row.updated_at)}</span>
					</button>
					{#if expandedId === row.id}
						<div class="archived-tasks__detail">
							<code>{row.id}</code>
							{#if row.description}<p>{row.description}</p>{/if}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}

	{#if total > pageSize}
		<footer class="archived-tasks__pager">
			<ServerPager
				{...pagerProps}
				ariaLabel="Archived task pages, below the list"
				{loading}
				on:pagechange={(event) => gotoPage(event.detail.page)}
			/>
		</footer>
	{/if}
</section>

<style>
	.archived-tasks {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.archived-tasks__head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}
	.archived-tasks__head input {
		flex: 1;
		min-width: 0;
		font: inherit;
		font-size: 0.85rem;
		padding: 0.4rem 0.6rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
	}
	.archived-tasks__count,
	.archived-tasks__meta,
	.archived-tasks__note {
		font-size: 0.78rem;
		color: var(--text-muted, #777);
	}
	.archived-tasks__list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.archived-tasks__row {
		width: 100%;
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.15rem;
		text-align: left;
		font: inherit;
		padding: 0.55rem 0.7rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 8px;
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
		cursor: pointer;
	}
	.archived-tasks__title {
		font-size: 0.9rem;
		font-weight: 550;
		overflow-wrap: anywhere;
	}
	.archived-tasks__detail {
		padding: 0.4rem 0.7rem 0.6rem;
		font-size: 0.82rem;
		color: var(--text-primary, #1a1a1a);
		overflow-wrap: anywhere;
	}
	.archived-tasks__detail p {
		margin: 0.35rem 0 0;
		white-space: pre-wrap;
	}
	.archived-tasks__head {
		flex-wrap: wrap;
	}
	.archived-tasks__size :global(.native-select) {
		flex-direction: row;
		align-items: center;
		gap: 8px;
	}
	.archived-tasks__size :global(.native-select__label) {
		white-space: nowrap;
	}
	.archived-tasks__pager {
		display: flex;
		justify-content: flex-end;
	}
	.archived-tasks__note button {
		font: inherit;
		font-size: 0.8rem;
		padding: 0.3rem 0.7rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		background: transparent;
		color: var(--text-primary, #1a1a1a);
		cursor: pointer;
	}
</style>
