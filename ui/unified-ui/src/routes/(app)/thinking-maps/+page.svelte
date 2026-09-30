<!--
  /thinking-maps — the Live Thinking Map list surface.

  Loads maps in the current scope via `listMapsPage()` — server-paginated,
  most recently updated first, one bounded page at a time behind the shared
  `ServerPager` (same control as memory/evidence/history; client-only; scope
  is carried by the shared identity store inside the API client). Renders a
  responsive table with Maps / Deleted lifecycle tabs, title / lifecycle /
  revision / updated-at / actions, plus a server page-size selector. Deleted
  maps can be restored or permanently purged behind themed confirmation and a
  server lifecycle guard. Offers a "New map" flow (title → `createMap` →
  navigate to the detail route). Mirrors the tasks-page conventions (shell
  layout, Spinner while loading, EmptyState when empty, inline error banner).
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import {
		listMapsPage,
		createMap,
		patchMap,
		permanentlyDeleteMap
	} from '$lib/thinkingMaps/api';
	import type { MapSummary, MapLifecycle } from '$lib/types/thinkingMap';
	import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';

	let maps: MapSummary[] = [];
	let isLoading = true;
	let loadError: string | null = null;

	let showCreate = false;
	let newTitle = '';
	let isCreating = false;
	let createError: string | null = null;
	type LibraryTab = 'maps' | 'deleted';
	let activeTab: LibraryTab = 'maps';
	let restoringMapId: string | null = null;
	let deletingMapId: string | null = null;

	const PAGE_SIZE_OPTIONS = [25, 50, 100];
	let pageSize = PAGE_SIZE_OPTIONS[0];
	let total = 0;
	let listPage = 1;
	let isPaging = false;
	let requestSequence = 0;

	$: pageCount = Math.max(1, Math.ceil(total / pageSize));
	$: startItem = total === 0 ? 0 : (listPage - 1) * pageSize + 1;
	$: endItem = total === 0 ? 0 : Math.min(total, (listPage - 1) * pageSize + maps.length);

	/**
	 * Fetch one server page. If deletion makes the requested page disappear,
	 * clamp to the new final page and refetch it instead of showing a false
	 * empty state. A request sequence prevents a late response from replacing a
	 * newer page-size/page navigation.
	 */
	async function requestMaps(target: number, fullSurface = false): Promise<void> {
		const requestId = ++requestSequence;
		const requestedPage = Math.max(1, Math.floor(target));
		const requestedPageSize = pageSize;
		const requestedLifecycle: MapLifecycle | undefined =
			activeTab === 'deleted' ? 'deleted' : undefined;
		if (fullSurface) isLoading = true;
		else isPaging = true;
		loadError = null;
		try {
			let resolvedPage = requestedPage;
			let page = await listMapsPage(
				requestedPageSize,
				(requestedPage - 1) * requestedPageSize,
				requestedLifecycle
			);
			if (requestId !== requestSequence) return;

			const lastPage = Math.max(1, Math.ceil(page.total / requestedPageSize));
			if (page.total > 0 && requestedPage > lastPage) {
				resolvedPage = lastPage;
				page = await listMapsPage(
					requestedPageSize,
					(lastPage - 1) * requestedPageSize,
					requestedLifecycle
				);
				if (requestId !== requestSequence) return;
			}

			maps = page.maps;
			total = page.total;
			listPage = resolvedPage;
		} catch (err) {
			if (requestId !== requestSequence) return;
			loadError = err instanceof Error ? err.message : String(err);
			if (fullSurface) {
				maps = [];
				total = 0;
			}
		} finally {
			if (requestId === requestSequence) {
				isLoading = false;
				isPaging = false;
			}
		}
	}

	/** Initial load/refresh (full-surface Spinner) — always lands on page 1. */
	async function loadMaps(): Promise<void> {
		await requestMaps(1, true);
	}

	/** ServerPager navigation — swaps the table body in place. */
	async function setListPage(target: number): Promise<void> {
		if (isPaging || target === listPage) return;
		await requestMaps(target);
	}

	async function setPageSize(next: number): Promise<void> {
		if (isPaging || next === pageSize || !PAGE_SIZE_OPTIONS.includes(next)) return;
		pageSize = next;
		await requestMaps(1);
	}

	async function setActiveTab(tab: LibraryTab): Promise<void> {
		if (tab === activeTab || restoringMapId || deletingMapId) return;
		activeTab = tab;
		maps = [];
		total = 0;
		listPage = 1;
		loadError = null;
		await requestMaps(1, true);
	}

	async function restoreMap(map: MapSummary): Promise<void> {
		if (restoringMapId || deletingMapId) return;
		restoringMapId = map.map_id;
		loadError = null;
		try {
			await patchMap(map.map_id, { lifecycle: 'active' });
			showSuccess(`Restored ${map.title || 'Untitled map'}`);
			await requestMaps(listPage);
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			loadError = message;
			showError('Could not restore map', message);
		} finally {
			restoringMapId = null;
		}
	}

	async function deletePermanently(map: MapSummary): Promise<void> {
		if (restoringMapId || deletingMapId) return;
		const label = map.title || 'Untitled map';
		const confirmed = await requestConfirmation({
			title: `Permanently delete “${label}”?`,
			message:
				'This removes the map, every revision, its event history, and exports. This cannot be undone.',
			confirmLabel: 'Delete permanently',
			destructive: true
		});
		if (!confirmed) return;

		deletingMapId = map.map_id;
		loadError = null;
		try {
			await permanentlyDeleteMap(map.map_id);
			showSuccess(`Permanently deleted ${label}`);
			await requestMaps(listPage);
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			loadError = message;
			showError('Could not permanently delete map', message);
		} finally {
			deletingMapId = null;
		}
	}

	async function handleCreate(): Promise<void> {
		const title = newTitle.trim();
		if (!title || isCreating) return;
		isCreating = true;
		createError = null;
		try {
			const created = await createMap({ title });
			await goto(`/thinking-maps/${encodeURIComponent(created.map_id)}`);
		} catch (err) {
			createError = err instanceof Error ? err.message : String(err);
			isCreating = false;
		}
	}

	function badgeColor(lifecycle: MapLifecycle): 'success' | 'warning' | 'default' | 'error' {
		switch (lifecycle) {
			case 'active':
				return 'success';
			case 'paused':
				return 'warning';
			case 'deleted':
				return 'error';
			case 'archived':
			default:
				return 'default';
		}
	}

	function epochOf(iso: string): number | null {
		const t = Date.parse(iso);
		return Number.isFinite(t) ? t : null;
	}

	function absoluteTime(iso: string): string {
		const epoch = epochOf(iso);
		if (epoch === null) return 'Unknown update time';
		return new Date(epoch).toLocaleString();
	}

	onMount(loadMaps);
</script>

<svelte:head>
	<title>Thinking Maps · Magican</title>
</svelte:head>

<div class="tm-list-shell">
	<header class="tm-list-header">
		<div class="tm-list-heading">
			<h1>Thinking Maps</h1>
			<p>Live brainstorm maps — every idea, question and decision as it takes shape.</p>
		</div>
		<div class="tm-list-actions">
			<Button
				variant="secondary"
				size="sm"
				label="Refresh"
				interactive={!isLoading && !isPaging}
				on:click={loadMaps}
			/>
			<Button
				variant="primary"
				size="sm"
				label="New map"
				on:click={() => {
					showCreate = true;
					createError = null;
				}}
			/>
		</div>
	</header>

	{#if showCreate}
		<div class="tm-create">
			<input
				class="tm-create__input"
				type="text"
				placeholder="Map title…"
				bind:value={newTitle}
				on:keydown={(e) => {
					if (e.key === 'Enter') handleCreate();
					if (e.key === 'Escape') showCreate = false;
				}}
			/>
			<Button
				variant="primary"
				size="sm"
				label={isCreating ? 'Creating…' : 'Create'}
				interactive={!isCreating && newTitle.trim().length > 0}
				on:click={handleCreate}
			/>
			<Button
				variant="outline"
				size="sm"
				label="Cancel"
				on:click={() => {
					showCreate = false;
					newTitle = '';
				}}
			/>
			{#if createError}
				<span class="tm-inline-error">{createError}</span>
			{/if}
		</div>
	{/if}

	<div class="tm-tabs" role="tablist" aria-label="Thinking map library views">
		<button
			type="button"
			role="tab"
			aria-selected={activeTab === 'maps'}
			class:tm-tab--active={activeTab === 'maps'}
			class="tm-tab"
			disabled={!!restoringMapId || !!deletingMapId}
			on:click={() => void setActiveTab('maps')}
		>
			Maps
		</button>
		<button
			type="button"
			role="tab"
			aria-selected={activeTab === 'deleted'}
			class:tm-tab--active={activeTab === 'deleted'}
			class="tm-tab"
			disabled={!!restoringMapId || !!deletingMapId}
			on:click={() => void setActiveTab('deleted')}
		>
			Deleted
		</button>
	</div>

	{#if loadError}
		<div class="tm-error" role="alert">
			<span>Couldn't load thinking maps: {loadError}</span>
			<Button variant="outline" size="sm" label="Retry" on:click={loadMaps} />
		</div>
	{/if}

	{#if isLoading}
		<div class="tm-loading">
			<Spinner size="md" label="Loading maps…" centered />
		</div>
	{:else if maps.length === 0 && !loadError}
		<EmptyState
			icon="✦"
			title={activeTab === 'deleted' ? 'No deleted maps' : 'No thinking maps yet'}
			description={activeTab === 'deleted'
				? 'Soft-deleted maps will remain here until restored or permanently deleted.'
				: 'Create your first map to start capturing a brainstorm.'}
			actionLabel={activeTab === 'deleted' ? '' : 'New map'}
			on:action={() => {
				if (activeTab === 'maps') showCreate = true;
			}}
		/>
	{:else if maps.length > 0}
		<section class="tm-table-panel" aria-label="Thinking maps">
			<div class="tm-table-toolbar">
				<p class="tm-table-count">
					<strong>{total}</strong> {activeTab === 'deleted'
						? total === 1
							? 'deleted map'
							: 'deleted maps'
						: total === 1
							? 'map'
							: 'maps'}
					<span>Most recently updated first</span>
				</p>
				<div class="tm-table-controls">
					<label class="tm-page-size">
						<span>Rows</span>
						<select
							value={pageSize}
							disabled={isPaging || !!restoringMapId || !!deletingMapId}
							on:change={(event) =>
								void setPageSize(Number((event.target as HTMLSelectElement).value))}
						>
							{#each PAGE_SIZE_OPTIONS as option (option)}
								<option value={option}>{option}</option>
							{/each}
						</select>
					</label>
					<ServerPager
						currentPage={listPage}
						{pageCount}
						{startItem}
						{endItem}
						totalItems={total}
						loading={isPaging}
						disabled={!!restoringMapId || !!deletingMapId}
						ariaLabel="Thinking maps pagination"
						on:pagechange={(event) => void setListPage(event.detail.page)}
					/>
				</div>
			</div>

			<div class:tm-table-wrap--paging={isPaging} class="tm-table-wrap">
				<table class="tm-table">
					<thead>
						<tr>
							<th scope="col">Map</th>
							<th scope="col">Status</th>
							<th scope="col" class="tm-table__revision">Revision</th>
							<th scope="col">{activeTab === 'deleted' ? 'Deleted' : 'Updated'}</th>
							<th scope="col"><span class="tm-visually-hidden">Actions</span></th>
						</tr>
					</thead>
					<tbody>
						{#each maps as m (m.map_id)}
							<tr>
								<td class="tm-table__map">
									<a href={`/thinking-maps/${encodeURIComponent(m.map_id)}`}>
										{m.title || 'Untitled map'}
									</a>
									<span title={m.map_id}>{m.map_id}</span>
								</td>
								<td><Badge text={m.lifecycle} color={badgeColor(m.lifecycle)} /></td>
								<td class="tm-table__revision">
									<span class="tm-revision">{m.latest_revision}</span>
								</td>
								<td class="tm-table__updated" title={absoluteTime(m.updated_at)}>
									{formatRelativeTime(epochOf(m.updated_at)) || 'Unknown'}
								</td>
								<td class="tm-table__action">
									{#if activeTab === 'deleted'}
										<div class="tm-row-actions">
											<button
												type="button"
												class="tm-row-action tm-row-action--restore"
												disabled={!!restoringMapId || !!deletingMapId}
												on:click={() => void restoreMap(m)}
											>
												{restoringMapId === m.map_id ? 'Restoring…' : 'Restore'}
											</button>
											<button
												type="button"
												class="tm-row-action tm-row-action--delete"
												disabled={!!restoringMapId || !!deletingMapId}
												on:click={() => void deletePermanently(m)}
											>
												{deletingMapId === m.map_id ? 'Deleting…' : 'Delete permanently'}
											</button>
										</div>
									{:else}
										<a href={`/thinking-maps/${encodeURIComponent(m.map_id)}`} aria-label={`Open ${m.title || 'untitled map'}`}>
											Open <span aria-hidden="true">→</span>
										</a>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>

			{#if pageCount > 1}
				<div class="tm-table-pager-bottom">
					<ServerPager
						currentPage={listPage}
						{pageCount}
						{startItem}
						{endItem}
						totalItems={total}
						loading={isPaging}
						disabled={!!restoringMapId || !!deletingMapId}
						ariaLabel="Thinking maps pagination"
						on:pagechange={(event) => void setListPage(event.detail.page)}
					/>
				</div>
			{/if}
		</section>
	{/if}
</div>

<style>
	.tm-list-shell {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.1rem 1.45rem 2rem;
		box-sizing: border-box;
		min-height: 0;
		overflow-y: auto;
	}

	.tm-list-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
	}

	.tm-list-heading h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.4rem;
		font-weight: 700;
		letter-spacing: -0.01em;
		color: var(--text-primary);
	}

	.tm-list-heading p {
		margin: 0.3rem 0 0;
		font-size: 0.85rem;
		line-height: 1.5;
		color: var(--text-secondary);
	}

	.tm-list-actions {
		display: flex;
		gap: 0.5rem;
		flex-shrink: 0;
	}

	.tm-tabs {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		width: fit-content;
		padding: 0.2rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--bg-soft) 72%, var(--bg-card));
	}

	.tm-tab {
		min-height: 1.85rem;
		padding: 0.3rem 0.72rem;
		border: 1px solid transparent;
		border-radius: var(--radius-sm, 6px);
		background: transparent;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.76rem;
		font-weight: 700;
		cursor: pointer;
	}

	.tm-tab:hover:not(:disabled) {
		color: var(--text-primary);
		background: color-mix(in srgb, var(--bg-card) 76%, transparent);
	}

	.tm-tab--active {
		border-color: var(--border-soft);
		background: var(--bg-card);
		color: var(--text-primary);
		box-shadow: var(--shadow-xs, 0 1px 2px rgb(0 0 0 / 0.06));
	}

	.tm-tab:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.tm-tab:disabled {
		cursor: default;
		opacity: 0.55;
	}

	.tm-create {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		padding: 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		animation: tm-create-in var(--transition-base, 0.25s) var(--ease-settle, ease) both;
	}

	@keyframes tm-create-in {
		from {
			opacity: 0;
			transform: translateY(-6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.tm-create {
			animation: none;
		}
	}

	.tm-create__input {
		flex: 1;
		min-width: 12rem;
		padding: 0.5rem 0.7rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.85rem;
		transition: border-color var(--transition-fast, 0.15s ease), background var(--transition-fast, 0.15s ease);
	}

	.tm-create__input::placeholder {
		color: var(--text-faint);
	}

	.tm-create__input:focus {
		background: var(--bg-card);
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 45%, var(--border-soft));
	}

	.tm-create__input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.tm-inline-error {
		font-size: 0.75rem;
		color: var(--color-error, var(--status-failed));
	}

	.tm-error {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.6rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.82rem;
	}

	.tm-loading {
		padding: 2.5rem 0;
	}

	.tm-table-panel {
		min-width: 0;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg, 12px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		overflow: hidden;
	}

	.tm-table-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.85rem;
		padding: 0.65rem 0.75rem;
		border-bottom: 1px solid var(--border-soft);
		background: color-mix(in srgb, var(--bg-soft) 68%, var(--bg-card));
	}

	.tm-table-count {
		display: flex;
		align-items: baseline;
		gap: 0.3rem;
		margin: 0;
		color: var(--text-secondary);
		font-size: 0.76rem;
	}

	.tm-table-count strong {
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.tm-table-count span {
		margin-left: 0.35rem;
		color: var(--text-muted);
	}

	.tm-table-controls {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.65rem;
		min-width: 0;
	}

	.tm-page-size {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--text-muted);
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.tm-page-size select {
		min-height: 1.85rem;
		padding: 0.25rem 1.55rem 0.25rem 0.48rem;
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: var(--radius-sm, 6px);
		background: var(--input-bg, var(--bg-card));
		color: var(--text-primary);
		font: inherit;
		font-variant-numeric: tabular-nums;
		text-transform: none;
		letter-spacing: normal;
	}

	.tm-page-size select:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.tm-table-wrap {
		width: 100%;
		overflow-x: auto;
		transition: opacity var(--transition-fast, 0.15s ease);
	}

	.tm-table-wrap--paging {
		opacity: 0.56;
		pointer-events: none;
	}

	.tm-table {
		width: 100%;
		min-width: 39rem;
		border-collapse: collapse;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-body, var(--text-primary));
	}

	.tm-table th,
	.tm-table td {
		padding: 0.7rem 0.8rem;
		border-bottom: 1px solid var(--border-soft);
		text-align: left;
		vertical-align: middle;
	}

	.tm-table th {
		background: color-mix(in srgb, var(--bg-soft) 58%, var(--bg-card));
		color: var(--text-secondary);
		font-size: 0.7rem;
		font-weight: 750;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.tm-table tbody tr:last-child td {
		border-bottom: 0;
	}

	.tm-table tbody tr {
		transition: background var(--transition-fast, 0.15s ease);
	}

	.tm-table tbody tr:hover {
		background: color-mix(in srgb, var(--bg-soft) 72%, transparent);
	}

	.tm-table__map {
		width: 58%;
		min-width: 16rem;
	}

	.tm-table__map > a {
		display: block;
		width: fit-content;
		max-width: min(34rem, 50vw);
		color: var(--text-primary);
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.9rem;
		font-weight: 680;
		line-height: 1.35;
		text-decoration: none;
		overflow-wrap: anywhere;
	}

	.tm-table__map > a:hover,
	.tm-table__map > a:focus-visible {
		color: var(--accent-primary);
		text-decoration: underline;
		text-underline-offset: 0.16em;
	}

	.tm-table__map > span {
		display: block;
		max-width: 28rem;
		margin-top: 0.18rem;
		color: var(--text-faint, var(--text-muted));
		font-family: var(--font-mono, monospace);
		font-size: 0.65rem;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.tm-revision {
		display: inline-flex;
		min-width: 2rem;
		justify-content: center;
		padding: 0.1rem 0.42rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--text-primary) 6%, transparent);
		color: var(--text-muted);
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		font-variant-numeric: tabular-nums;
	}

	.tm-table__updated {
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.tm-table__action {
		text-align: right !important;
		white-space: nowrap;
	}

	.tm-table__action a {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		padding: 0.28rem 0.48rem;
		border-radius: var(--radius-sm, 6px);
		color: var(--accent-primary);
		font-size: 0.74rem;
		font-weight: 700;
		text-decoration: none;
	}

	.tm-table__action a:hover,
	.tm-table__action a:focus-visible {
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		outline: none;
	}

	.tm-row-actions {
		display: inline-flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.35rem;
	}

	.tm-row-action {
		min-height: 1.75rem;
		padding: 0.26rem 0.52rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: transparent;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.7rem;
		font-weight: 700;
		white-space: nowrap;
		cursor: pointer;
	}

	.tm-row-action--restore:hover:not(:disabled),
	.tm-row-action--restore:focus-visible {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-primary) 9%, transparent);
		color: var(--accent-primary);
	}

	.tm-row-action--delete {
		border-color: color-mix(in srgb, var(--color-error, var(--status-failed)) 30%, var(--border-soft));
		color: var(--color-error, var(--status-failed));
	}

	.tm-row-action--delete:hover:not(:disabled),
	.tm-row-action--delete:focus-visible {
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
	}

	.tm-row-action:focus-visible {
		outline: 2px solid color-mix(in srgb, currentColor 48%, transparent);
		outline-offset: 1px;
	}

	.tm-row-action:disabled {
		cursor: default;
		opacity: 0.5;
	}

	.tm-table-pager-bottom {
		padding: 0.6rem 0.75rem;
		border-top: 1px solid var(--border-soft);
	}

	.tm-visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	@media (max-width: 760px) {
		.tm-list-shell {
			padding: 0.9rem 0.85rem 1.5rem;
		}

		.tm-table-toolbar {
			align-items: flex-start;
			flex-direction: column;
		}

		.tm-table-controls {
			width: 100%;
			justify-content: space-between;
			flex-wrap: wrap;
		}

		.tm-table-count span {
			display: none;
		}

		.tm-table__revision {
			display: none;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.tm-table-wrap,
		.tm-table tbody tr {
			transition: none;
		}
	}

	:global([data-theme^='retro-16bit']) .tm-table-panel,
	:global([data-theme^='retro-16bit']) .tm-tabs,
	:global([data-theme^='retro-16bit']) .tm-tab,
	:global([data-theme^='retro-16bit']) .tm-page-size select,
	:global([data-theme^='retro-16bit']) .tm-revision,
	:global([data-theme^='retro-16bit']) .tm-table__action a,
	:global([data-theme^='retro-16bit']) .tm-row-action {
		border-radius: 0;
	}
</style>
