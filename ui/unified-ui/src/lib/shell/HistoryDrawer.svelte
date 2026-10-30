<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { fade, fly } from 'svelte/transition';

	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { chatStore, type ChatSession } from '$lib/stores/chatStore';
	import { chatHistoryRevision } from '$lib/shell/chatHistoryRefresh';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { threadStore } from '$lib/stores/threadStore';
	import type { UiThreadRecord } from '$lib/threads/types';
	import {
		normalizeThreadId,
		isVibeDevThread,
		isVibeDevSession
	} from '$lib/threads/normalizeThreadId';
	import {
		fetchHistorySearch,
		fetchSessionHistory,
		fetchThreadHistory,
		type HistoryLane,
		type HistorySearchItem
	} from '$lib/shell/historyApi';
	import {
		OVERLAY_PRIORITIES,
		release,
		requestFocus
	} from '$lib/shell/overlayCoordinator';
	import { historyDrawerThreadFilter } from '$lib/shell/shellState';

	export let open = false;
	export let side: 'left' | 'right' = 'right';
	export let threadFilter: string | null = null;
	export let initialTab: 'sessions' | 'threads' = 'sessions';
	/** Optional override for session selection. Surfaces that host their own
	 *  chat (the warroom deck) switch the session IN PLACE instead of
	 *  navigating to the thread's chat page. Null keeps the default
	 *  navigation, so existing mounts are untouched. */
	export let onSelectSession: ((session: ChatSession) => void | Promise<void>) | null = null;
	export let onSelectCodingSession: ((session: ChatSession) => void | Promise<void>) | null = null;
	export let onSelectThread: ((threadId: string) => void | Promise<void>) | null = null;

	const COORDINATOR_ID = 'history-drawer';
	const PAGE_SIZE = 15;
	const MAX_SEARCH_LENGTH = 120;

	let activeTab: 'sessions' | 'threads' = initialTab;
	let historyLane: HistoryLane = 'personal';
	/** Active or archived rows, each paged by the server. */
	let archiveView: 'active' | 'archived' = 'active';
	let searchInput = '';
	let search = '';
	let offset = 0;
	let total = 0;
	let sessions: ChatSession[] = [];
	let threads: UiThreadRecord[] = [];
	/** Thread whose archive, restore, or delete is still waiting on the server. */
	let threadMutation: { id: string; label: string } | null = null;
	let searchResults: HistorySearchItem[] = [];
	let isLoading = false;
	let loadError: string | null = null;
	let lastLoadKey = '';
	let lastQueryKey = '';
	let loadGeneration = 0;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;
	/** Thread id whose session filter the reader dismissed. A later thread still scopes. */
	let dismissedThreadFilter: string | null = null;

	$: activeSessionId = $chatStore.activeSessionId;
	$: viewingArchivedId = $chatStore.viewingArchivedId;
	$: currentViewId = viewingArchivedId ?? activeSessionId;
	$: activeSessions = sessions.filter((session) => session.status === 'active');
	$: archivedSessions = sessions.filter((session) => session.status === 'archived');
	$: activeThreads = threads.filter((thread) => !thread.archived || thread.id === 'general');
	$: archivedThreads = threads.filter((thread) => thread.archived && thread.id !== 'general');
	$: pageStart = total === 0 ? 0 : offset + 1;
	$: pageEnd = Math.min(offset + PAGE_SIZE, total);
	$: hasPrevious = offset > 0;
	$: hasNext = offset + PAGE_SIZE < total;
	$: isGlobalSearch = search.length > 0;
	$: sessionThreadFilter =
		threadFilter && threadFilter !== dismissedThreadFilter ? threadFilter : null;
	$: effectiveLane = activeTab === 'sessions' && sessionThreadFilter ? undefined : historyLane;
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: loadKey = open
		? isGlobalSearch
			? [scopeKey, 'search', search, offset].join('|')
			: [scopeKey, activeTab, effectiveLane ?? 'thread', sessionThreadFilter ?? '', archiveView, offset].join('|')
		: '';
	$: refreshKey = `${loadKey}|${$chatHistoryRevision}`;
	$: if (open && loadKey && refreshKey !== lastLoadKey) {
		const background = loadKey === lastQueryKey && !isLoading;
		lastQueryKey = loadKey;
		lastLoadKey = refreshKey;
		void loadHistory(background);
	}
	$: if (browser) onOpenChange(open);

	async function loadHistory(background = false): Promise<void> {
		const generation = ++loadGeneration;
		if (!background) isLoading = true;
		loadError = null;
		try {
			if (isGlobalSearch) {
				const page = await fetchHistorySearch({
					search,
					limit: PAGE_SIZE,
					offset
				});
				if (generation !== loadGeneration) return;
				searchResults = page.items;
				sessions = [];
				threads = [];
				total = page.total;
			} else if (activeTab === 'sessions') {
				const page = await fetchSessionHistory({
					lane: effectiveLane,
					search,
					limit: PAGE_SIZE,
					offset,
					threadId: sessionThreadFilter,
					archived: archiveView === 'archived' ? 'only' : 'exclude'
				});
				if (generation !== loadGeneration) return;
				sessions = page.items;
				threads = [];
				searchResults = [];
				total = page.total;
			} else {
				const page = await fetchThreadHistory({
					lane: historyLane,
					search,
					limit: PAGE_SIZE,
					offset,
					archived: archiveView === 'archived' ? 'only' : 'exclude'
				});
				if (generation !== loadGeneration) return;
				threads = page.items;
				sessions = [];
				searchResults = [];
				total = page.total;
			}

			if (offset > 0 && total > 0 && offset >= total) {
				offset = Math.max(0, Math.floor((total - 1) / PAGE_SIZE) * PAGE_SIZE);
				lastLoadKey = '';
			}
		} catch (error) {
			if (generation !== loadGeneration) return;
			if (background) return;
			loadError = error instanceof Error ? error.message : 'Could not load history';
			sessions = [];
			threads = [];
			searchResults = [];
			total = 0;
		} finally {
			if (generation === loadGeneration) isLoading = false;
		}
	}

	function setTab(tab: 'sessions' | 'threads'): void {
		if (activeTab === tab) return;
		activeTab = tab;
		offset = 0;
		lastLoadKey = '';
	}

	function setArchiveView(view: 'active' | 'archived'): void {
		if (archiveView === view) return;
		archiveView = view;
		offset = 0;
		lastLoadKey = '';
	}

	function setLane(lane: HistoryLane): void {
		if (historyLane === lane) return;
		historyLane = lane;
		offset = 0;
		lastLoadKey = '';
	}

	function updateSearch(value: string): void {
		searchInput = Array.from(value).slice(0, MAX_SEARCH_LENGTH).join('');
		if (searchTimer) clearTimeout(searchTimer);
		searchTimer = setTimeout(() => {
			search = searchInput.trim();
			offset = 0;
			lastLoadKey = '';
		}, 250);
	}

	function clearSearch(): void {
		if (searchTimer) clearTimeout(searchTimer);
		searchInput = '';
		search = '';
		offset = 0;
		lastLoadKey = '';
	}

	function clearThreadFilter(): void {
		if (threadFilter && get(historyDrawerThreadFilter) === threadFilter) {
			historyDrawerThreadFilter.set(null);
		}
		dismissedThreadFilter = threadFilter;
		offset = 0;
	}

	function reloadCurrentPage(): void {
		lastLoadKey = '';
	}

	function close(): void {
		open = false;
		release(COORDINATOR_ID);
	}

	function handleBackdropClick(event: MouseEvent): void {
		if (event.target === event.currentTarget) close();
	}

	function handleItemKeydown(event: KeyboardEvent, action: () => void): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			action();
		}
	}

	let newMenuOpen = false;
	let newMenuTriggerEl: HTMLButtonElement | null = null;

	function handleNewMenuKeydown(e: KeyboardEvent): void {
		if (e.key === 'Escape') {
			e.preventDefault();
			newMenuOpen = false;
			newMenuTriggerEl?.focus();
		}
	}

	async function openSelectedSession(session: ChatSession): Promise<void> {
		if ($chatStore.deletingSessionIds?.includes(session.id)) return;
		if (isVibeDevSession(session) || isVibeDevThread(session.ui_thread_id)) {
			close();
			if (onSelectCodingSession) await onSelectCodingSession(session);
			else await goto(`/vibe?session=${encodeURIComponent(session.id)}`);
			return;
		}
		if (onSelectSession) {
			close();
			await onSelectSession(session);
			return;
		}
		const targetThread = normalizeThreadId(session.ui_thread_id);
		if (targetThread === 'vibedev') {
			close();
			await goto(`/vibe?session=${encodeURIComponent(session.id)}`);
			return;
		}
		const chatPath = `/t/${encodeURIComponent(targetThread)}/chat`;
		const chatHref = `${chatPath}?session=${encodeURIComponent(session.id)}`;
		const onTargetChat = browser && window.location.pathname === chatPath;
		if (session.id === activeSessionId && !viewingArchivedId && onTargetChat) {
			close();
			return;
		}
		close();
		try {
			await goto(chatHref, { keepFocus: true, noScroll: true });
			if (onTargetChat) await chatStore.openSession(session.id);
		} catch (error) {
			showError(
				'Could not open chat session',
				error instanceof Error ? error.message : 'The selected session could not be loaded.'
			);
		}
	}

	async function archiveSession(sessionId: string, event: Event): Promise<void> {
		event.stopPropagation();
		await chatStore.archiveSession?.(sessionId);
		reloadCurrentPage();
	}

	async function unarchiveSession(sessionId: string, event: Event): Promise<void> {
		event.stopPropagation();
		await chatStore.unarchiveSession?.(sessionId);
		reloadCurrentPage();
	}

	async function deleteSession(sessionId: string, event: Event): Promise<void> {
		event.stopPropagation();
		if ($chatStore.deletingSessionIds?.includes(sessionId)) return;
		const requestScope = scopeKey;
		try {
			await chatStore.deleteSession(sessionId);
			if (requestScope === scopeKey) reloadCurrentPage();
		} catch (error) {
			if (requestScope === scopeKey) showError(error instanceof Error ? error.message : 'Failed to delete session');
		}
	}

	function newSession(): void {
		void chatStore.newExecution(threadFilter ?? 'general');
		close();
	}

	function selectThread(threadId: string): void {
		if (onSelectThread) {
			close();
			void onSelectThread(threadId);
			return;
		}
		if (isVibeDevThread(threadId)) {
			close();
			void goto('/vibe');
			return;
		}
		void goto(`/t/${encodeURIComponent(threadId)}`);
		close();
	}

	function newThread(): void {
		void goto('/t/new');
		close();
	}

	function beginThreadMutation(threadId: string, label: string): boolean {
		if (threadId === 'general' || threadMutation) return false;
		threadMutation = { id: threadId, label };
		return true;
	}

	function finishThreadMutation(threadId: string): void {
		if (threadMutation?.id === threadId) threadMutation = null;
	}

	async function archiveThread(threadId: string, event: Event): Promise<void> {
		event.stopPropagation();
		if (!beginThreadMutation(threadId, 'Archiving…')) return;
		try {
			const updated = await threadStore.updateThread?.(threadId, { archived: true });
			if (!updated) {
				showError(`Could not archive #${threadId}`);
				return;
			}
			reloadCurrentPage();
		} finally {
			finishThreadMutation(threadId);
		}
	}

	async function restoreThread(threadId: string, event: Event): Promise<void> {
		event.stopPropagation();
		if (!beginThreadMutation(threadId, 'Restoring…')) return;
		try {
			const updated = await threadStore.updateThread?.(threadId, { archived: false });
			if (!updated) {
				showError(`Could not restore #${threadId}`);
				return;
			}
			reloadCurrentPage();
		} finally {
			finishThreadMutation(threadId);
		}
	}

	async function deleteThread(threadId: string, event: Event): Promise<void> {
		event.stopPropagation();
		if (threadId === 'general' || threadMutation) return;
		const confirmed = await requestConfirmation({
			title: `Delete #${threadId}?`,
			message: 'This removes the thread and its chat sessions. Related tasks remain available from Tasks.',
			confirmLabel: 'Delete',
			destructive: true
		});
		if (!confirmed || !beginThreadMutation(threadId, 'Deleting…')) return;
		try {
			const deleted = await threadStore.deleteThread?.(threadId);
			if (!deleted) {
				showError(`Could not delete #${threadId}`);
				return;
			}
			await chatStore.loadSessions?.();
			showSuccess(`Deleted #${threadId}.`);
			reloadCurrentPage();
			if (browser) {
				const path = window.location.pathname;
				const currentThreadId = path.startsWith('/t/')
					? decodeURIComponent(path.slice('/t/'.length).split('/')[0] ?? '')
					: null;
				if (currentThreadId === threadId) await goto('/t/general');
			}
		} catch (error) {
			showError(error instanceof Error ? error.message : `Failed to delete #${threadId}`);
		} finally {
			finishThreadMutation(threadId);
		}
	}

	function formatTimestamp(timestamp: number): string {
		const date = new Date(timestamp);
		const now = new Date();
		const isToday =
			date.getDate() === now.getDate() &&
			date.getMonth() === now.getMonth() &&
			date.getFullYear() === now.getFullYear();
		return isToday
			? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
			: date.toLocaleDateString([], { month: 'short', day: 'numeric' });
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (open && event.key === 'Escape') {
			event.preventDefault();
			close();
		}
	}

	function onOpenChange(isOpen: boolean): void {
		if (isOpen) {
			requestFocus({
				id: COORDINATOR_ID,
				priority: OVERLAY_PRIORITIES.historyDrawer,
				onClose: () => {
					open = false;
				}
			});
		} else {
			loadGeneration += 1;
			activeTab = initialTab;
			offset = 0;
			lastLoadKey = '';
			release(COORDINATOR_ID);
		}
	}

	onDestroy(() => {
		if (searchTimer) clearTimeout(searchTimer);
		loadGeneration += 1;
		release(COORDINATOR_ID);
	});
</script>

<svelte:window on:keydown={handleKeydown} />

{#if open}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="drawer-shade" on:click={handleBackdropClick} transition:fade={{ duration: 120 }}></div>

	<div class="drawer" class:drawer--left={side === 'left'} role="dialog" aria-modal="true" aria-label="History" transition:fly={{ x: side === 'left' ? -360 : 360, duration: 220 }}>
		<header class="drawer-head">
			<div>
				<h3>History</h3>
				<span>{total} {isGlobalSearch ? 'results' : activeTab}</span>
			</div>
			<div class="drawer-head-actions">
				<div class="rail-new-wrap">
					<button
						type="button"
						class="rail-new-btn"
						class:active={newMenuOpen}
						bind:this={newMenuTriggerEl}
						on:click|stopPropagation={() => (newMenuOpen = !newMenuOpen)}
						title="Create new session or thread"
						aria-label="Start new chat"
						aria-haspopup="menu"
						aria-expanded={newMenuOpen}
					>
						<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round">
							<path d="M12 5v14M5 12h14" />
						</svg>
						<span>New</span>
						<svg class="new-chevron" class:open={newMenuOpen} width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
							<polyline points="6 9 12 15 18 9"/>
						</svg>
					</button>

					{#if newMenuOpen}
						<div
							class="rail-new-menu"
							role="menu"
							tabindex="-1"
							aria-label="New creation options"
							use:clickOutside={{ handler: () => (newMenuOpen = false), exclude: [newMenuTriggerEl] }}
							on:keydown={handleNewMenuKeydown}
						>
							<button
								type="button"
								role="menuitem"
								class="rail-new-menu-item"
								on:click={() => { newMenuOpen = false; newSession(); }}
							>
								<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
									<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
								</svg>
								<span>New Session</span>
							</button>
							<button
								type="button"
								role="menuitem"
								class="rail-new-menu-item"
								on:click={() => { newMenuOpen = false; newThread(); }}
							>
								<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
									<line x1="4" y1="9" x2="20" y2="9"/>
									<line x1="4" y1="15" x2="20" y2="15"/>
									<line x1="10" y1="3" x2="8" y2="21"/>
									<line x1="16" y1="3" x2="14" y2="21"/>
								</svg>
								<span>New Thread</span>
							</button>
						</div>
					{/if}
				</div>
				<button class="icon-button" on:click={close} aria-label="Close history" title="Close">
					<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
				</button>
			</div>
		</header>

		{#if isGlobalSearch}
			<div class="global-search-scope" aria-label="Searching all history">
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M4 6h16M4 12h16M4 18h16"/></svg>
				<strong>All sessions and threads</strong>
				<span>Personal + Automated</span>
			</div>
		{:else}
			<div class="tabs" role="tablist" aria-label="History type">
				<button role="tab" aria-selected={activeTab === 'sessions'} class:active={activeTab === 'sessions'} on:click={() => setTab('sessions')}>Sessions</button>
				<button role="tab" aria-selected={activeTab === 'threads'} class:active={activeTab === 'threads'} on:click={() => setTab('threads')}>Threads</button>
			</div>
		{/if}

		<div class="controls">
			{#if !isGlobalSearch && activeTab === 'sessions' && sessionThreadFilter}
				<div class="scope-label" title={`Sessions in #${sessionThreadFilter}`}>
					<span class="scope-label-name">#{sessionThreadFilter}</span>
					<button
						type="button"
						class="scope-clear"
						aria-label="Show sessions from all threads"
						title="Show all threads"
						on:click={clearThreadFilter}
					>
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M18 6 6 18M6 6l12 12"/></svg>
					</button>
				</div>
			{:else if !isGlobalSearch}
				<div class="lane-switch" role="group" aria-label="History source">
					<button class:active={historyLane === 'personal'} on:click={() => setLane('personal')}>Personal</button>
					<button class:active={historyLane === 'automated'} on:click={() => setLane('automated')}>Automated</button>
				</div>
			{/if}
			{#if !isGlobalSearch}
				<div class="lane-switch" role="group" aria-label="Archived state">
					<button class:active={archiveView === 'active'} aria-pressed={archiveView === 'active'} on:click={() => setArchiveView('active')}>Active</button>
					<button class:active={archiveView === 'archived'} aria-pressed={archiveView === 'archived'} on:click={() => setArchiveView('archived')}>Archived</button>
				</div>
			{/if}
			<label class="search-box">
				<span class="search-icon-wrap" aria-hidden="true">
					<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
				</span>
				<input
					type="search"
					value={searchInput}
					placeholder="Search all history"
					aria-label="Search all history"
					on:input={(event) => {
						const value = event.currentTarget.value;
						updateSearch(value);
						if (value !== searchInput) event.currentTarget.value = searchInput;
					}}
				/>
				{#if searchInput}
					<button type="button" on:click={clearSearch} aria-label="Clear search" title="Clear search">
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
					</button>
				{/if}
			</label>
		</div>

		<div class="list" aria-busy={isLoading}>

			{#if isLoading}
				<div class="skeleton-list" aria-label="Loading history">
					{#each Array(7) as _}
						<div class="skeleton-row"><span></span><i></i></div>
					{/each}
				</div>
			{:else if loadError}
				<div class="state-message error-state">
					<p>History could not be loaded.</p>
					<button on:click={reloadCurrentPage}>Retry</button>
				</div>
			{:else if isGlobalSearch}
				{#if searchResults.length === 0}
					<p class="empty">No matching history.</p>
				{:else}
					{#each searchResults as result}
						{#if result.kind === 'session'}
							<div class="item" class:archived={result.session.status === 'archived'} class:active={result.session.id === currentViewId} aria-busy={!!$chatStore.deletingSessionIds?.includes(result.session.id)} aria-disabled={!!$chatStore.deletingSessionIds?.includes(result.session.id)} role="button" tabindex="0" on:click={() => openSelectedSession(result.session)} on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(result.session))}>
								<div class="row1"><span class="dot" class:dim={result.session.status === 'archived'}></span><span class="title" title={result.session.title || 'Untitled session'}>{result.session.title || 'Untitled session'}</span>{#if $chatStore.deletingSessionIds?.includes(result.session.id)}<span class="deleting-status" role="status">Deleting…</span>{/if}<span class="ts">{formatTimestamp(result.session.updated_at)}</span></div>
								<div class="row2"><span class="thread-tag">#{result.session.ui_thread_id || 'general'}</span><span class="kind-tag">{result.session.internal_voice?.kind === 'branch' ? 'Concurrent' : 'Session'}</span><span class="lane-tag" class:personal={result.history_lane === 'personal'}>{result.history_lane === 'personal' ? 'Personal' : 'Automated'}</span><span class="actions">{#if result.session.status === 'archived'}<button disabled={!!$chatStore.deletingSessionIds?.includes(result.session.id)} class="action" title="Restore" aria-label="Restore session" on:click={(event) => unarchiveSession(result.session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>{:else if !result.session.is_default_session && !result.session.internal_voice}<button disabled={!!$chatStore.deletingSessionIds?.includes(result.session.id)} class="action" title="Archive" aria-label="Archive session" on:click={(event) => archiveSession(result.session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>{/if}{#if !result.session.is_default_session && !result.session.internal_voice}<button disabled={!!$chatStore.deletingSessionIds?.includes(result.session.id)} class="action danger" title="Delete" aria-label="Delete session" on:click={(event) => deleteSession(result.session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span></div>
							</div>
						{:else}
							{@const threadName = result.thread.name || result.thread.id}
							{@const busy = threadMutation?.id === result.thread.id}
							<div class="item search-thread-item" class:archived={result.thread.archived} aria-busy={busy} role="button" tabindex="0" on:click={() => { if (!busy) selectThread(result.thread.id); }} on:keydown={(event) => handleItemKeydown(event, () => { if (!busy) selectThread(result.thread.id); })}>
								<div class="row1"><span class="hash">#</span><span class="title" title={threadName}>{threadName}</span>{#if busy}<span class="deleting-status" role="status">{threadMutation?.label}</span>{/if}<span class="ts">{formatTimestamp(result.thread.updated_at)}</span></div>
								<div class="row2"><span class="kind-tag">Thread</span><span class="lane-tag" class:personal={result.history_lane === 'personal'}>{result.history_lane === 'personal' ? 'Personal' : 'Automated'}</span>{#if result.thread.id !== 'general'}<span class="actions">{#if result.thread.archived}<button disabled={busy} class="action" title="Restore" aria-label="Restore thread" on:click={(event) => restoreThread(result.thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>{:else}<button disabled={busy} class="action" title="Archive" aria-label="Archive thread" on:click={(event) => archiveThread(result.thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>{/if}<button disabled={busy} class="action danger" title="Delete" aria-label="Delete thread" on:click={(event) => deleteThread(result.thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button></span>{/if}</div>
							</div>
						{/if}
					{/each}
				{/if}
			{:else if activeTab === 'sessions'}
				{#if sessions.length === 0}
					<p class="empty">{archiveView === 'archived' ? 'No archived sessions.' : 'No matching sessions.'}</p>
			{:else}
					{#if activeSessions.length > 0}<div class="section-h">Active</div>{/if}
					{#each activeSessions as session (session.id)}
						<div class="item" class:active={session.id === currentViewId} aria-busy={!!$chatStore.deletingSessionIds?.includes(session.id)} aria-disabled={!!$chatStore.deletingSessionIds?.includes(session.id)} role="button" tabindex="0" on:click={() => openSelectedSession(session)} on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(session))}>
							<div class="row1"><span class="dot"></span><span class="title" title={session.title || 'Untitled session'}>{session.title || 'Untitled session'}</span>{#if $chatStore.deletingSessionIds?.includes(session.id)}<span class="deleting-status" role="status">Deleting…</span>{/if}<span class="ts">{formatTimestamp(session.updated_at)}</span></div>
							<div class="row2"><span class="thread-tag">#{session.ui_thread_id || 'general'}</span>{#if session.internal_voice?.kind === 'branch'}<span class="kind-tag">Concurrent</span>{/if}{#if !session.is_default_session && !session.internal_voice}<span class="actions"><button disabled={!!$chatStore.deletingSessionIds?.includes(session.id)} class="action" title="Archive" aria-label="Archive session" on:click={(event) => archiveSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button><button disabled={!!$chatStore.deletingSessionIds?.includes(session.id)} class="action danger" title="Delete" aria-label="Delete session" on:click={(event) => deleteSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button></span>{/if}</div>
						</div>
					{/each}
					{#if archivedSessions.length > 0}<div class="section-h">Archived</div>{/if}
					{#each archivedSessions as session (session.id)}
						<div class="item archived" class:active={session.id === currentViewId} aria-busy={!!$chatStore.deletingSessionIds?.includes(session.id)} aria-disabled={!!$chatStore.deletingSessionIds?.includes(session.id)} role="button" tabindex="0" on:click={() => openSelectedSession(session)} on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(session))}>
							<div class="row1"><span class="dot dim"></span><span class="title" title={session.title || 'Untitled session'}>{session.title || 'Untitled session'}</span>{#if $chatStore.deletingSessionIds?.includes(session.id)}<span class="deleting-status" role="status">Deleting…</span>{/if}<span class="ts">{formatTimestamp(session.updated_at)}</span></div>
							<div class="row2"><span class="thread-tag">#{session.ui_thread_id || 'general'}</span>{#if session.internal_voice?.kind === 'branch'}<span class="kind-tag">Concurrent</span>{/if}<span class="actions"><button disabled={!!$chatStore.deletingSessionIds?.includes(session.id)} class="action" title="Restore" aria-label="Restore session" on:click={(event) => unarchiveSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>{#if !session.is_default_session && !session.internal_voice}<button disabled={!!$chatStore.deletingSessionIds?.includes(session.id)} class="action danger" title="Delete" aria-label="Delete session" on:click={(event) => deleteSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span></div>
						</div>
					{/each}
				{/if}
			{:else if threads.length === 0}
				<p class="empty">{archiveView === 'archived' ? 'No archived threads.' : 'No matching threads.'}</p>
			{:else}
				{#if activeThreads.length > 0}<div class="section-h">Active</div>{/if}
				{#each activeThreads as thread (thread.id)}
					{@const threadName = thread.name || thread.id}
					{@const busy = threadMutation?.id === thread.id}
					<div class="item thread-item" aria-busy={busy} role="button" tabindex="0" on:click={() => { if (!busy) selectThread(thread.id); }} on:keydown={(event) => handleItemKeydown(event, () => { if (!busy) selectThread(thread.id); })}>
						<span class="hash">#</span><span class="title" title={threadName}>{threadName}</span>{#if busy}<span class="deleting-status" role="status">{threadMutation?.label}</span>{/if}<span class="actions">{#if thread.id !== 'general'}<button disabled={busy} class="action" title="Archive" aria-label="Archive thread" on:click={(event) => archiveThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button><button disabled={busy} class="action danger" title="Delete" aria-label="Delete thread" on:click={(event) => deleteThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span>
					</div>
				{/each}
				{#if archivedThreads.length > 0}<div class="section-h">Archived</div>{/if}
				{#each archivedThreads as thread (thread.id)}
					{@const threadName = thread.name || thread.id}
					{@const busy = threadMutation?.id === thread.id}
					<div class="item thread-item archived" aria-busy={busy} role="button" tabindex="0" on:click={() => { if (!busy) selectThread(thread.id); }} on:keydown={(event) => handleItemKeydown(event, () => { if (!busy) selectThread(thread.id); })}>
						<span class="hash">#</span><span class="title" title={threadName}>{threadName}</span>{#if busy}<span class="deleting-status" role="status">{threadMutation?.label}</span>{/if}<span class="actions">{#if thread.id !== 'general'}<button disabled={busy} class="action" title="Restore" aria-label="Restore thread" on:click={(event) => restoreThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button><button disabled={busy} class="action danger" title="Delete" aria-label="Delete thread" on:click={(event) => deleteThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span>
					</div>
				{/each}
			{/if}
		</div>

		<footer class="pagination" aria-label="History pagination">
			<button on:click={() => (offset = Math.max(0, offset - PAGE_SIZE))} disabled={!hasPrevious || isLoading} aria-label="Previous page" title="Previous page"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="m15 18-6-6 6-6"/></svg></button>
			<span>{pageStart}-{pageEnd} of {total}</span>
			<button on:click={() => (offset += PAGE_SIZE)} disabled={!hasNext || isLoading} aria-label="Next page" title="Next page"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="m9 18 6-6-6-6"/></svg></button>
		</footer>
	</div>
{/if}

<style>
	.deleting-status { flex-shrink: 0; color: var(--text-muted); font-size: 0.7rem; white-space: nowrap; }
	.item[aria-busy="true"] .actions { opacity: 1; }
	.action:disabled { cursor: wait; opacity: 0.5; }
	.drawer-shade { position: fixed; inset: 0; background: transparent; z-index: 540; }
	.drawer { position: fixed; inset: 0 0 0 auto; width: min(380px, 92vw); background: var(--bg-elevated, #fff); color: var(--text-primary, #1a1a1a); border-left: 1px solid var(--border-soft, rgba(0,0,0,.1)); box-shadow: var(--panel-shadow, -8px 0 32px rgba(0,0,0,.12)); display: flex; flex-direction: column; z-index: 550; overflow: hidden; }
	.drawer--left { inset: 0 auto 0 0; border-left: 0; border-right: 1px solid var(--border-soft); box-shadow: var(--panel-shadow, 8px 0 32px rgba(0,0,0,.12)); }
	.drawer-head { min-height: 58px; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 16px; border-bottom: 1px solid var(--border-soft, rgba(0,0,0,.1)); }
	.drawer-head > div { min-width: 0; display: flex; align-items: baseline; gap: 8px; }
	.drawer-head h3 { margin: 0; font-family: var(--font-display); font-size: 15px; font-weight: 650; letter-spacing: 0; }
	.drawer-head span { color: var(--text-muted, #777); font: 10.5px var(--font-mono); }
	.icon-button, .pagination button { width: 28px; height: 28px; display: inline-flex; align-items: center; justify-content: center; padding: 0; background: transparent; border: 1px solid transparent; color: var(--text-muted, #777); border-radius: 6px; cursor: pointer; }
	.icon-button:hover, .pagination button:hover:not(:disabled) { color: var(--text-primary, #1a1a1a); background: var(--bg-soft, #f1f1f1); border-color: var(--border-soft, rgba(0,0,0,.08)); }
	.tabs { display: grid; grid-template-columns: 1fr 1fr; padding: 0 12px; border-bottom: 1px solid var(--border-soft, rgba(0,0,0,.1)); }
	.tabs button { height: 38px; padding: 0 10px; font-family: var(--font-primary); font-size: 12px; font-weight: 550; color: var(--text-muted, #777); background: transparent; border: 0; border-bottom: 2px solid transparent; cursor: pointer; }
	.tabs button.active { color: var(--text-primary, #1a1a1a); border-bottom-color: var(--accent-primary, #c2502a); }
	.global-search-scope { min-height: 39px; display: grid; grid-template-columns: 18px 1fr auto; align-items: center; gap: 7px; padding: 0 14px; color: var(--text-muted, #777); border-bottom: 1px solid var(--border-soft, rgba(0,0,0,.1)); background: var(--bg-soft, #f2f2f2); }
	.global-search-scope strong { color: var(--text-primary, #1a1a1a); font: 12px var(--font-primary); font-weight: 600; }
	.global-search-scope span { font: 10px var(--font-mono); }
	.controls { display: grid; gap: 8px; padding: 10px 10px 8px; border-bottom: 1px solid var(--border-soft, rgba(0,0,0,.08)); }
	.lane-switch { display: grid; grid-template-columns: 1fr 1fr; padding: 2px; background: var(--bg-soft, #f2f2f2); border: 1px solid var(--border-soft, rgba(0,0,0,.07)); border-radius: 7px; }
	.lane-switch button { height: 28px; border: 0; border-radius: 5px; background: transparent; color: var(--text-muted, #777); font: 11.5px var(--font-primary); cursor: pointer; }
	.lane-switch button.active { background: var(--bg-elevated, #fff); color: var(--text-primary, #1a1a1a); box-shadow: 0 1px 3px rgba(0,0,0,.09); }
	.scope-label { height: 32px; display: flex; align-items: center; gap: 4px; padding: 0 4px 0 10px; overflow: hidden; color: var(--text-muted, #777); background: var(--bg-soft, #f2f2f2); border: 1px solid var(--border-soft, rgba(0,0,0,.07)); border-radius: 7px; font: 11px var(--font-mono); }
	.scope-label-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.scope-clear, .scope-clear:hover, .scope-clear:focus, .scope-clear:focus-visible, .scope-clear:active { width: 18px; height: 18px; flex-shrink: 0; display: inline-flex; align-items: center; justify-content: center; padding: 0; border: 0 !important; outline: none !important; box-shadow: none !important; border-radius: 4px; color: var(--text-muted, #777); background: transparent; cursor: pointer; }
	.scope-clear:hover { color: var(--text-primary, #1a1a1a); background: var(--bg-elevated, #fff); }
	.search-box {
		height: 32px;
		box-sizing: border-box;
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 8px;
		color: var(--text-muted, #777);
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-default, rgba(0,0,0,.14));
		border-radius: 7px;
		overflow: hidden;
		-webkit-tap-highlight-color: transparent;
	}
	.search-box:focus-within { border-color: var(--accent-primary, #c2502a); box-shadow: 0 0 0 2px var(--accent-primary-soft, rgba(194,80,42,.12)); }
	.search-icon-wrap {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 14px;
		height: 14px;
		flex-shrink: 0;
		color: inherit;
		pointer-events: none;
	}
	.search-box input,
	.search-box input:hover,
	.search-box input:focus,
	.search-box input:focus-visible,
	.search-box input:active {
		min-width: 0;
		flex: 1;
		height: 100% !important;
		min-height: 0 !important;
		max-height: 100% !important;
		padding: 0 !important;
		margin: 0 !important;
		border: 0 !important;
		border-width: 0 !important;
		border-color: transparent !important;
		outline: none !important;
		box-shadow: none !important;
		-webkit-appearance: none !important;
		appearance: none !important;
		-webkit-tap-highlight-color: transparent !important;
		-webkit-focus-ring-color: transparent !important;
		color: var(--text-primary, #1a1a1a);
		background: transparent !important;
		background-color: transparent !important;
		font: 12px var(--font-primary) !important;
		line-height: 1.2 !important;
		border-radius: 0 !important;
		box-sizing: border-box !important;
	}
	.search-box input::-webkit-search-decoration, .search-box input::-webkit-search-cancel-button { -webkit-appearance: none; display: none; }
	.search-box button,
	.search-box button:hover,
	.search-box button:focus,
	.search-box button:focus-visible,
	.search-box button:active {
		width: 18px;
		height: 18px;
		flex-shrink: 0;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		border: 0 !important;
		border-width: 0 !important;
		border-color: transparent !important;
		outline: none !important;
		box-shadow: none !important;
		-webkit-tap-highlight-color: transparent !important;
		-webkit-focus-ring-color: transparent !important;
		border-radius: 4px;
		color: var(--text-muted, #777);
		background: transparent;
		cursor: pointer;
	}
	.search-box button:hover { color: var(--text-primary, #1a1a1a); background: var(--bg-soft, rgba(0,0,0,.05)); }
	.drawer-head-actions { display: flex; align-items: center; gap: 6px; }
	.rail-new-wrap { position: relative; display: inline-flex; }
	.rail-new-btn { display: inline-flex; align-items: center; gap: 4px; padding: 4px 8px; border-radius: 6px; border: 1px solid color-mix(in srgb, var(--accent-primary, #c2502a) 24%, transparent); background: var(--accent-primary-soft, rgba(194, 80, 42, 0.08)); color: var(--accent-primary, #c2502a); font-family: var(--font-primary, sans-serif); font-size: 11.5px; font-weight: 600; cursor: pointer; transition: background 120ms ease, transform 120ms ease; }
	.rail-new-btn:hover { background: var(--accent-primary-soft, rgba(194, 80, 42, 0.16)); transform: translateY(-1px); }
	.rail-new-btn.active { background: var(--accent-primary-soft, rgba(194, 80, 42, 0.2)); }
	.new-chevron { transition: transform 150ms ease; }
	.new-chevron.open { transform: rotate(180deg); }
	.rail-new-menu { position: absolute; top: calc(100% + 4px); right: 0; z-index: 560; min-width: 140px; background: var(--bg-surface, #fff); border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12)); border-radius: 8px; box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15); padding: 4px; display: flex; flex-direction: column; gap: 2px; }
	.rail-new-menu-item { display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 10px; border: 0; border-radius: 6px; background: transparent; color: var(--text-primary, #1a1a1a); font-family: var(--font-primary, sans-serif); font-size: 12px; font-weight: 500; text-align: left; cursor: pointer; transition: background 120ms ease, color 120ms ease; }
	.rail-new-menu-item:hover { background: var(--bg-hover, rgba(0, 0, 0, 0.05)); color: var(--accent-primary, #c2502a); }
	.list { flex: 1; min-height: 0; overflow-y: auto; padding: 8px 8px 12px; }
	.section-h { padding: 10px 10px 4px; color: var(--text-faint, #999); font: 10px var(--font-mono); font-weight: 600; letter-spacing: 0; text-transform: uppercase; }
	.item { padding: 9px 10px; margin-bottom: 1px; border-radius: 7px; cursor: pointer; transition: background 120ms ease; outline: none; }
	.item:hover, .item:focus-visible { background: var(--bg-soft, rgba(0,0,0,.04)); }
	.item.active { background: var(--accent-primary-soft, rgba(194,80,42,.1)); }
	.item.archived { opacity: .72; }
	.row1 { display: flex; align-items: baseline; gap: 8px; margin-bottom: 3px; }
	.row2 { display: flex; align-items: center; gap: 8px; min-height: 24px; padding-left: 14px; }
	.thread-item { min-height: 40px; display: flex; align-items: center; gap: 8px; min-width: 0; overflow: hidden; }
	.dot { width: 6px; height: 6px; flex: 0 0 6px; align-self: center; border-radius: 50%; background: var(--accent-primary, #c2502a); }
	.dot.dim { background: var(--text-faint, #aaa); }
	.hash { flex: 0 0 auto; color: var(--text-muted, #777); font: 13px var(--font-mono); }
	.title { min-width: 0; flex: 1; overflow: hidden; color: var(--text-primary, #1a1a1a); font: 13px var(--font-primary); font-weight: 520; text-overflow: ellipsis; white-space: nowrap; }
	.ts { flex: 0 0 auto; color: var(--text-muted, #888); font: 10.5px var(--font-mono); }
	.thread-tag { max-width: 205px; overflow: hidden; padding: 2px 6px; border-radius: 4px; color: var(--text-muted, #777); background: var(--bg-soft, rgba(0,0,0,.04)); font: 10.5px var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
	.kind-tag, .lane-tag { flex: 0 0 auto; padding: 2px 6px; border: 1px solid var(--border-soft, rgba(0,0,0,.08)); border-radius: 4px; color: var(--text-muted, #777); background: var(--bg-soft, rgba(0,0,0,.04)); font: 10px var(--font-primary); line-height: 1.2; }
	.lane-tag { color: var(--text-primary, #1a1a1a); background: var(--bg-elevated, #fff); }
	.lane-tag.personal { color: var(--accent-primary, #c2502a); border-color: var(--accent-primary-soft, rgba(194,80,42,.18)); background: var(--accent-primary-soft, rgba(194,80,42,.08)); }
	.search-thread-item .row2 { padding-left: 21px; }
	.actions { margin-left: auto; display: inline-flex; flex-shrink: 0; gap: 2px; opacity: 0; }
	.item:hover .actions, .item:focus-within .actions { opacity: 1; }
	.action { width: 24px; height: 24px; display: inline-flex; align-items: center; justify-content: center; padding: 0; color: var(--text-muted, #777); background: transparent; border: 1px solid transparent; border-radius: 5px; cursor: pointer; }
	.action:hover { color: var(--text-primary, #1a1a1a); background: var(--bg-elevated, #fff); border-color: var(--border-soft, rgba(0,0,0,.1)); }
	.action.danger:hover { color: var(--color-error, #d4453a); background: var(--color-error-soft, rgba(212,69,58,.1)); }
	.empty, .state-message { margin: 0; padding: 42px 22px; text-align: center; color: var(--text-muted, #777); font: 12.5px var(--font-primary); }
	.state-message p { margin: 0 0 10px; }
	.state-message button { padding: 6px 10px; color: var(--text-primary, #1a1a1a); background: var(--bg-soft, #f2f2f2); border: 1px solid var(--border-default, rgba(0,0,0,.12)); border-radius: 6px; cursor: pointer; }
	.skeleton-list { padding: 2px; }
	.skeleton-row { height: 52px; display: grid; align-content: center; gap: 7px; padding: 0 9px; }
	.skeleton-row span, .skeleton-row i { display: block; border-radius: 4px; background: linear-gradient(90deg, var(--bg-soft, #eee) 25%, var(--bg-elevated, #f8f8f8) 50%, var(--bg-soft, #eee) 75%); background-size: 200% 100%; animation: shimmer 1.35s infinite linear; }
	.skeleton-row span { width: 72%; height: 10px; }
	.skeleton-row i { width: 38%; height: 8px; }
	.pagination { height: 48px; flex: 0 0 48px; display: grid; grid-template-columns: 28px 1fr 28px; align-items: center; gap: 8px; padding: 0 12px; border-top: 1px solid var(--border-soft, rgba(0,0,0,.1)); background: var(--bg-elevated, #fff); }
	.pagination span { text-align: center; color: var(--text-muted, #777); font: 10.5px var(--font-mono); }
	.pagination button:disabled { opacity: .3; cursor: default; }
	@keyframes shimmer { from { background-position: 200% 0; } to { background-position: -200% 0; } }
	@media (prefers-reduced-motion: reduce) { .skeleton-row span, .skeleton-row i { animation: none; } }
</style>
