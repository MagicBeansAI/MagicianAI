<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onDestroy } from 'svelte';
	import { fade, fly } from 'svelte/transition';

	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { chatStore, type ChatSession } from '$lib/stores/chatStore';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { threadStore } from '$lib/stores/threadStore';
	import type { UiThreadRecord } from '$lib/threads/types';
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

	export let open = false;
	export let threadFilter: string | null = null;
	export let initialTab: 'sessions' | 'threads' = 'sessions';
	/** Optional override for session selection. Surfaces that host their own
	 *  chat (the warroom deck) switch the session IN PLACE instead of
	 *  navigating to the thread's chat page. Null keeps the default
	 *  navigation, so existing mounts are untouched. */
	export let onSelectSession: ((session: ChatSession) => void | Promise<void>) | null = null;

	const COORDINATOR_ID = 'history-drawer';
	const PAGE_SIZE = 15;
	const MAX_SEARCH_LENGTH = 120;

	let activeTab: 'sessions' | 'threads' = initialTab;
	let historyLane: HistoryLane = 'personal';
	let searchInput = '';
	let search = '';
	let offset = 0;
	let total = 0;
	let sessions: ChatSession[] = [];
	let threads: UiThreadRecord[] = [];
	let searchResults: HistorySearchItem[] = [];
	let isLoading = false;
	let loadError: string | null = null;
	let lastLoadKey = '';
	let loadGeneration = 0;
	let searchTimer: ReturnType<typeof setTimeout> | null = null;

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
	$: effectiveLane = activeTab === 'sessions' && threadFilter ? undefined : historyLane;
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: loadKey = open
		? isGlobalSearch
			? [scopeKey, 'search', search, offset].join('|')
			: [scopeKey, activeTab, effectiveLane ?? 'thread', threadFilter ?? '', offset].join('|')
		: '';
	$: if (open && loadKey && loadKey !== lastLoadKey) {
		lastLoadKey = loadKey;
		void loadHistory();
	}
	$: if (browser) onOpenChange(open);

	async function loadHistory(): Promise<void> {
		const generation = ++loadGeneration;
		isLoading = true;
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
					threadId: threadFilter
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
					offset
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

	async function openSelectedSession(session: ChatSession): Promise<void> {
		if (onSelectSession) {
			close();
			await onSelectSession(session);
			return;
		}
		const targetThread = session.ui_thread_id || 'general';
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
		await chatStore.deleteSession?.(sessionId);
		reloadCurrentPage();
	}

	function newSession(): void {
		void chatStore.newExecution(threadFilter ?? 'general');
		close();
	}

	function selectThread(threadId: string): void {
		void goto(`/t/${encodeURIComponent(threadId)}`);
		close();
	}

	function newThread(): void {
		void goto('/t/new');
		close();
	}

	async function archiveThread(threadId: string, event: Event): Promise<void> {
		event.stopPropagation();
		if (threadId === 'general') return;
		await threadStore.updateThread?.(threadId, { archived: true });
		reloadCurrentPage();
	}

	async function restoreThread(threadId: string, event: Event): Promise<void> {
		event.stopPropagation();
		await threadStore.updateThread?.(threadId, { archived: false });
		reloadCurrentPage();
	}

	async function deleteThread(threadId: string, event: Event): Promise<void> {
		event.stopPropagation();
		if (threadId === 'general') return;
		const confirmed = await requestConfirmation({
			title: `Delete #${threadId}?`,
			message: 'This removes the thread and its chat sessions. Related tasks remain available from Tasks.',
			confirmLabel: 'Delete',
			destructive: true
		});
		if (!confirmed) return;
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

	<div class="drawer" role="dialog" aria-modal="true" aria-label="History" transition:fly={{ x: 360, duration: 220 }}>
		<header class="drawer-head">
			<div>
				<h3>History</h3>
				<span>{total} {isGlobalSearch ? 'results' : activeTab}</span>
			</div>
			<button class="icon-button" on:click={close} aria-label="Close history" title="Close">
				<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
			</button>
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
			{#if !isGlobalSearch && activeTab === 'sessions' && threadFilter}
				<div class="scope-label" title={`Sessions in #${threadFilter}`}>#{threadFilter}</div>
			{:else if !isGlobalSearch}
				<div class="lane-switch" role="group" aria-label="History source">
					<button class:active={historyLane === 'personal'} on:click={() => setLane('personal')}>Personal</button>
					<button class:active={historyLane === 'automated'} on:click={() => setLane('automated')}>Automated</button>
				</div>
			{/if}
			<label class="search-box">
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
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
					<button on:click={clearSearch} aria-label="Clear search" title="Clear search">
						<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
					</button>
				{/if}
			</label>
		</div>

		<div class="list" aria-busy={isLoading}>
			{#if !isGlobalSearch && ((activeTab === 'sessions' && (historyLane === 'personal' || threadFilter)) || (activeTab === 'threads' && historyLane === 'personal'))}
				<button class="create-button" on:click={activeTab === 'sessions' ? newSession : newThread}>
					<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>
					New {activeTab === 'sessions' ? 'session' : 'thread'}
				</button>
			{/if}

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
							<div class="item" class:archived={result.session.status === 'archived'} class:active={result.session.id === currentViewId} role="button" tabindex="0" on:click={() => openSelectedSession(result.session)} on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(result.session))}>
								<div class="row1"><span class="dot" class:dim={result.session.status === 'archived'}></span><span class="title" title={result.session.title || 'Untitled session'}>{result.session.title || 'Untitled session'}</span><span class="ts">{formatTimestamp(result.session.updated_at)}</span></div>
								<div class="row2"><span class="thread-tag">#{result.session.ui_thread_id || 'general'}</span><span class="kind-tag">{result.session.internal_voice?.kind === 'branch' ? 'Concurrent' : 'Session'}</span><span class="lane-tag" class:personal={result.history_lane === 'personal'}>{result.history_lane === 'personal' ? 'Personal' : 'Automated'}</span><span class="actions">{#if result.session.status === 'archived'}<button class="action" title="Restore" aria-label="Restore session" on:click={(event) => unarchiveSession(result.session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>{:else if !result.session.is_default_session && !result.session.internal_voice}<button class="action" title="Archive" aria-label="Archive session" on:click={(event) => archiveSession(result.session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>{/if}{#if !result.session.is_default_session && !result.session.internal_voice}<button class="action danger" title="Delete" aria-label="Delete session" on:click={(event) => deleteSession(result.session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span></div>
							</div>
						{:else}
							<div class="item search-thread-item" class:archived={result.thread.archived} role="button" tabindex="0" on:click={() => selectThread(result.thread.id)} on:keydown={(event) => handleItemKeydown(event, () => selectThread(result.thread.id))}>
								<div class="row1"><span class="hash">#</span><span class="title" title={result.thread.name || result.thread.id}>{result.thread.name || result.thread.id}</span><span class="ts">{formatTimestamp(result.thread.updated_at)}</span></div>
								<div class="row2"><span class="kind-tag">Thread</span><span class="lane-tag" class:personal={result.history_lane === 'personal'}>{result.history_lane === 'personal' ? 'Personal' : 'Automated'}</span><span class="actions">{#if result.thread.archived}<button class="action" title="Restore" aria-label="Restore thread" on:click={(event) => restoreThread(result.thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>{:else if result.thread.id !== 'general'}<button class="action" title="Archive" aria-label="Archive thread" on:click={(event) => archiveThread(result.thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>{/if}{#if result.thread.id !== 'general'}<button class="action danger" title="Delete" aria-label="Delete thread" on:click={(event) => deleteThread(result.thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span></div>
							</div>
						{/if}
					{/each}
				{/if}
			{:else if activeTab === 'sessions'}
				{#if sessions.length === 0}
					<p class="empty">No matching sessions.</p>
			{:else}
					{#if activeSessions.length > 0}<div class="section-h">Active</div>{/if}
					{#each activeSessions as session (session.id)}
						<div class="item" class:active={session.id === currentViewId} role="button" tabindex="0" on:click={() => openSelectedSession(session)} on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(session))}>
							<div class="row1"><span class="dot"></span><span class="title" title={session.title || 'Untitled session'}>{session.title || 'Untitled session'}</span><span class="ts">{formatTimestamp(session.updated_at)}</span></div>
							<div class="row2"><span class="thread-tag">#{session.ui_thread_id || 'general'}</span>{#if session.internal_voice?.kind === 'branch'}<span class="kind-tag">Concurrent</span>{/if}{#if !session.is_default_session && !session.internal_voice}<span class="actions"><button class="action" title="Archive" aria-label="Archive session" on:click={(event) => archiveSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button><button class="action danger" title="Delete" aria-label="Delete session" on:click={(event) => deleteSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button></span>{/if}</div>
						</div>
					{/each}
					{#if archivedSessions.length > 0}<div class="section-h">Archived</div>{/if}
					{#each archivedSessions as session (session.id)}
						<div class="item archived" class:active={session.id === currentViewId} role="button" tabindex="0" on:click={() => openSelectedSession(session)} on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(session))}>
							<div class="row1"><span class="dot dim"></span><span class="title" title={session.title || 'Untitled session'}>{session.title || 'Untitled session'}</span><span class="ts">{formatTimestamp(session.updated_at)}</span></div>
							<div class="row2"><span class="thread-tag">#{session.ui_thread_id || 'general'}</span>{#if session.internal_voice?.kind === 'branch'}<span class="kind-tag">Concurrent</span>{/if}<span class="actions"><button class="action" title="Restore" aria-label="Restore session" on:click={(event) => unarchiveSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>{#if !session.is_default_session && !session.internal_voice}<button class="action danger" title="Delete" aria-label="Delete session" on:click={(event) => deleteSession(session.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span></div>
						</div>
					{/each}
				{/if}
			{:else if threads.length === 0}
				<p class="empty">No matching threads.</p>
			{:else}
				{#if activeThreads.length > 0}<div class="section-h">Active</div>{/if}
				{#each activeThreads as thread (thread.id)}
					<div class="item thread-item" role="button" tabindex="0" on:click={() => selectThread(thread.id)} on:keydown={(event) => handleItemKeydown(event, () => selectThread(thread.id))}>
						<span class="hash">#</span><span class="title" title={thread.name || thread.id}>{thread.name || thread.id}</span><span class="actions">{#if thread.id !== 'general'}<button class="action" title="Archive" aria-label="Archive thread" on:click={(event) => archiveThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button><button class="action danger" title="Delete" aria-label="Delete thread" on:click={(event) => deleteThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>{/if}</span>
					</div>
				{/each}
				{#if archivedThreads.length > 0}<div class="section-h">Archived</div>{/if}
				{#each archivedThreads as thread (thread.id)}
					<div class="item thread-item archived" role="button" tabindex="0" on:click={() => selectThread(thread.id)} on:keydown={(event) => handleItemKeydown(event, () => selectThread(thread.id))}>
						<span class="hash">#</span><span class="title" title={thread.name || thread.id}>{thread.name || thread.id}</span><span class="actions"><button class="action" title="Restore" aria-label="Restore thread" on:click={(event) => restoreThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button><button class="action danger" title="Delete" aria-label="Delete thread" on:click={(event) => deleteThread(thread.id, event)}><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button></span>
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
	.drawer-shade { position: fixed; inset: 0; background: transparent; z-index: 540; }
	.drawer { position: fixed; inset: 0 0 0 auto; width: min(380px, 92vw); background: var(--bg-elevated, #fff); color: var(--text-primary, #1a1a1a); border-left: 1px solid var(--border-soft, rgba(0,0,0,.1)); box-shadow: var(--panel-shadow, -8px 0 32px rgba(0,0,0,.12)); display: flex; flex-direction: column; z-index: 550; overflow: hidden; }
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
	.scope-label { height: 32px; display: flex; align-items: center; padding: 0 10px; overflow: hidden; color: var(--text-muted, #777); background: var(--bg-soft, #f2f2f2); border: 1px solid var(--border-soft, rgba(0,0,0,.07)); border-radius: 7px; font: 11px var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
	.search-box { height: 34px; display: flex; align-items: center; gap: 7px; padding: 0 8px 0 10px; color: var(--text-muted, #777); background: var(--bg-elevated, #fff); border: 1px solid var(--border-default, rgba(0,0,0,.14)); border-radius: 7px; }
	.search-box:focus-within { border-color: var(--accent-primary, #c2502a); box-shadow: 0 0 0 2px var(--accent-primary-soft, rgba(194,80,42,.12)); }
	.search-box input { min-width: 0; flex: 1; padding: 0; border: 0; outline: 0; color: var(--text-primary, #1a1a1a); background: transparent; font: 12px var(--font-primary); }
	.search-box button { width: 22px; height: 22px; display: inline-flex; align-items: center; justify-content: center; padding: 0; border: 0; border-radius: 4px; color: var(--text-muted, #777); background: transparent; cursor: pointer; }
	.list { flex: 1; min-height: 0; overflow-y: auto; padding: 8px 8px 12px; }
	.create-button { width: 100%; min-height: 34px; display: flex; align-items: center; justify-content: center; gap: 6px; margin-bottom: 7px; color: var(--button-primary-color, #fff); background: var(--button-primary-bg, var(--text-ink, #1a1a1a)); border: 0; border-radius: 7px; font: 12px var(--font-primary); font-weight: 600; cursor: pointer; }
	.create-button:hover { opacity: .9; }
	.section-h { padding: 10px 10px 4px; color: var(--text-faint, #999); font: 10px var(--font-mono); font-weight: 600; letter-spacing: 0; text-transform: uppercase; }
	.item { padding: 9px 10px; margin-bottom: 1px; border-radius: 7px; cursor: pointer; transition: background 120ms ease; outline: none; }
	.item:hover, .item:focus-visible { background: var(--bg-soft, rgba(0,0,0,.04)); }
	.item.active { background: var(--accent-primary-soft, rgba(194,80,42,.1)); }
	.item.archived { opacity: .72; }
	.row1 { display: flex; align-items: baseline; gap: 8px; margin-bottom: 3px; }
	.row2 { display: flex; align-items: center; gap: 8px; min-height: 24px; padding-left: 14px; }
	.thread-item { min-height: 40px; display: flex; align-items: center; gap: 8px; }
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
	.actions { margin-left: auto; display: inline-flex; gap: 2px; opacity: 0; }
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
