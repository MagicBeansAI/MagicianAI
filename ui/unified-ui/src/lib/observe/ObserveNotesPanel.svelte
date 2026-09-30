<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		backfillTaskNotes,
		fetchTaskNotes,
		promoteTaskNoteToMemory,
		type TaskNoteItem,
		type TaskNotePage
	} from '$lib/notes/taskNotes';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	const PAGE_SIZE_OPTIONS = [5, 10, 20, 50];
	let page: TaskNotePage = { items: [], offset: 0, limit: 5, total: 0, has_more: false };
	let currentPage = 1;
	let pageSize = 5;
	let searchDraft = '';
	let activeSearch = '';
	let loading = true;
	let promotingId: string | null = null;
	let backfilling = false;
	let requestId = 0;
	let controller: AbortController | null = null;
	let mounted = false;
	let loadedScope = '';

	$: pageCount = Math.max(1, Math.ceil(page.total / pageSize));
	$: startItem = page.total === 0 ? 0 : page.offset + 1;
	$: endItem = page.total === 0 ? 0 : Math.min(page.total, page.offset + page.items.length);
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (mounted && scopeKey !== loadedScope) {
		loadedScope = scopeKey;
		currentPage = 1;
		void load();
	}

	async function load(): Promise<void> {
		const id = ++requestId;
		controller?.abort();
		const nextController = new AbortController();
		controller = nextController;
		loading = true;
		try {
			const result = await fetchTaskNotes({
				offset: (currentPage - 1) * pageSize,
				limit: pageSize,
				query: activeSearch,
				signal: nextController.signal
			});
			if (id !== requestId) return;
			page = result;
			const actualPageCount = Math.max(1, Math.ceil(result.total / pageSize));
			if (currentPage > actualPageCount) {
				currentPage = actualPageCount;
				void load();
			}
		} catch (error) {
			if (id !== requestId || nextController.signal.aborted) return;
			showError(error instanceof Error ? error.message : 'Published notes could not be loaded');
		} finally {
			if (id === requestId) loading = false;
		}
	}

	function submitSearch(): void {
		activeSearch = searchDraft.trim();
		currentPage = 1;
		void load();
	}

	function clearSearch(): void {
		searchDraft = '';
		activeSearch = '';
		currentPage = 1;
		void load();
	}

	function changePage(next: number): void {
		if (loading || next === currentPage) return;
		currentPage = Math.max(1, Math.min(pageCount, next));
		void load();
	}

	function changePageSize(next: number): void {
		if (!PAGE_SIZE_OPTIONS.includes(next) || next === pageSize) return;
		pageSize = next;
		currentPage = 1;
		void load();
	}

	async function promote(note: TaskNoteItem): Promise<void> {
		promotingId = note.task_id;
		try {
			await promoteTaskNoteToMemory(note.task_id);
			showSuccess('Memory candidate created for review');
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Note could not be promoted');
		} finally {
			promotingId = null;
		}
	}

	async function backfill(): Promise<void> {
		backfilling = true;
		try {
			const result = await backfillTaskNotes(25);
			if (result.published.length > 0) {
				showSuccess(
					`Published ${result.published.length} completed task${result.published.length === 1 ? '' : 's'}`
					+ (result.pagination.has_more ? '; more remain for the next batch' : '')
				);
			} else if (result.errors.length > 0) {
				showError(`${result.errors.length} task page${result.errors.length === 1 ? '' : 's'} could not be published`);
			} else {
				showSuccess('Completed tasks are already published');
			}
			currentPage = 1;
			await load();
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Completed tasks could not be published');
		} finally {
			backfilling = false;
		}
	}

	function displayDate(value: string): string {
		const date = new Date(value);
		return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
	}

	function sourceDateLabel(note: TaskNoteItem): string {
		return note.task_completed_at
			? `completed ${displayDate(note.task_completed_at)}`
			: `updated ${displayDate(note.source_updated_at)}`;
	}

	onMount(() => {
		mounted = true;
		loadedScope = scopeKey;
		void load();
	});
	onDestroy(() => controller?.abort());
</script>

<section id="observe-notes" class="notes-panel" aria-labelledby="observe-notes-title">
	<header class="notes-head">
		<div>
			<p class="eyebrow">Observed knowledge</p>
			<h2 id="observe-notes-title">Notes</h2>
			<p>Completed task pages you can browse, reopen, or deliberately hand to memory review.</p>
		</div>
		<div class="head-actions">
			<a href="/notes">Audio Notes</a>
			<button type="button" disabled={backfilling} on:click={() => void backfill()}>{backfilling ? 'Publishing…' : 'Publish next 25'}</button>
			<button type="button" disabled={loading} on:click={() => void load()}>Refresh</button>
		</div>
	</header>

	<div class="notes-toolbar">
		<form on:submit|preventDefault={submitSearch}>
			<input bind:value={searchDraft} placeholder="Search title, task, agent, or tag" aria-label="Search published task notes" />
			<button type="submit">Search</button>
			{#if activeSearch}<button type="button" on:click={clearSearch}>Clear</button>{/if}
		</form>
		<label>Page size
			<select value={pageSize} disabled={loading} on:change={(event) => changePageSize(Number((event.currentTarget as HTMLSelectElement).value))}>
				{#each PAGE_SIZE_OPTIONS as option}<option value={option}>{option}</option>{/each}
			</select>
		</label>
	</div>

	{#if !loading && page.items.length === 0}
		<div class="empty">{activeSearch ? 'No published notes match this search.' : 'No task pages have been published yet.'}</div>
	{:else}
		<div class:loading class="note-list" aria-busy={loading}>
			{#each page.items as note (note.projection_id)}
				<article>
					<div class="note-copy">
						<div class="title-row"><h3>{note.title}</h3><span>{note.mode}</span></div>
						<p>
							{note.status} · {note.agent_id} · {sourceDateLabel(note)}
							{note.task_due_date ? ` · due ${displayDate(note.task_due_date)}` : ''}
						</p>
						<small>Published {displayDate(note.published_at)} · {note.note_path}{note.assets.length ? ` · ${note.assets.length} assets` : ''}</small>
						<div class="tags">{#each note.tags.slice(0, 6) as tag}<span>{tag}</span>{/each}</div>
					</div>
					<div class="note-actions">
						{#if note.note_path}<a href={`/notes?path=${encodeURIComponent(note.note_path)}`}>Open</a>{/if}
						<button type="button" disabled={promotingId !== null} on:click={() => void promote(note)}>
							{promotingId === note.task_id ? 'Creating…' : 'Promote to memory'}
						</button>
					</div>
				</article>
			{/each}
		</div>
	{/if}

	<ServerPager
		{currentPage}
		{pageCount}
		{startItem}
		{endItem}
		totalItems={page.total}
		{loading}
		ariaLabel="Published task notes pagination"
		on:pagechange={(event) => changePage(event.detail.page)}
	/>
</section>

<style>
	.notes-panel { margin: 18px 0; padding: 20px; border: 1px solid var(--border-soft); border-radius: 18px; background: var(--bg-elevated); }
	.notes-head, .notes-toolbar, article, .title-row, .note-actions, .head-actions { display: flex; align-items: center; gap: 12px; }
	.notes-head, .notes-toolbar, article { justify-content: space-between; }
	h2, h3, p { margin: 0; }
	.notes-head > div:first-child > p:last-child { margin-top: 5px; color: var(--text-secondary); }
	.eyebrow { margin-bottom: 4px; color: var(--accent-primary); font-size: .7rem; font-weight: 800; letter-spacing: .1em; text-transform: uppercase; }
	button, a, input, select { border: 1px solid var(--border-soft); border-radius: 999px; background: var(--bg-card); color: var(--text-primary); font: inherit; font-size: .78rem; padding: 7px 11px; text-decoration: none; }
	button, a { cursor: pointer; }
	button:disabled { opacity: .5; cursor: default; }
	.notes-toolbar { margin: 16px 0 10px; }
	.notes-toolbar form { display: flex; flex: 1; gap: 7px; }
	.notes-toolbar input { min-width: 220px; }
	.notes-toolbar label { display: flex; align-items: center; gap: 7px; color: var(--text-secondary); font-size: .76rem; }
	.note-list { display: grid; gap: 8px; margin-bottom: 12px; }
	.note-list.loading { opacity: .55; }
	article { padding: 12px 13px; border: 1px solid var(--border-soft); border-radius: 13px; }
	.note-copy { min-width: 0; }
	.title-row { align-items: baseline; }
	.title-row h3 { font-size: .95rem; }
	.title-row > span { color: var(--accent-primary); font-size: .7rem; text-transform: uppercase; }
	.note-copy > p, .note-copy small { color: var(--text-muted); font-size: .75rem; }
	.note-copy small { overflow-wrap: anywhere; }
	.tags { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 6px; }
	.tags span { padding: 2px 6px; border-radius: 999px; background: var(--bg-soft); color: var(--text-secondary); font-size: .66rem; }
	.empty { margin-bottom: 12px; padding: 24px; border: 1px dashed var(--border-soft); border-radius: 12px; color: var(--text-muted); text-align: center; }
	@media (max-width: 760px) { .notes-head, .notes-toolbar, article { align-items: stretch; flex-direction: column; } .notes-toolbar form { flex-wrap: wrap; } .notes-toolbar input { flex: 1; min-width: 160px; } .note-actions { justify-content: flex-start; } }
</style>
