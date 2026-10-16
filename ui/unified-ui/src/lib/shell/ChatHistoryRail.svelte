<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onDestroy } from 'svelte';

	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { chatStore, type ChatSession } from '$lib/stores/chatStore';
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

	export let expanded = true;
	export let mobileOpen = false;
	export let threadFilter: string | null = null;
	export let initialTab: 'sessions' | 'threads' = 'sessions';

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
	$: pageStart = total === 0 ? 0 : offset + 1;
	$: pageEnd = Math.min(offset + PAGE_SIZE, total);
	$: hasPrevious = offset > 0;
	$: hasNext = offset + PAGE_SIZE < total;
	$: isGlobalSearch = search.length > 0;
	$: effectiveLane = activeTab === 'sessions' && threadFilter ? undefined : historyLane;
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: loadKey = isGlobalSearch
		? [scopeKey, 'search', search, offset].join('|')
		: [scopeKey, activeTab, effectiveLane ?? 'thread', threadFilter ?? '', archiveView, offset].join('|');

	$: if (loadKey && loadKey !== lastLoadKey) {
		lastLoadKey = loadKey;
		void loadHistory();
	}

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
					threadId: threadFilter,
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
		} catch (error) {
			if (generation !== loadGeneration) return;
			loadError = error instanceof Error ? error.message : 'Could not load history.';
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

	function reloadCurrentPage(): void {
		lastLoadKey = '';
	}

	function handleItemKeydown(event: KeyboardEvent, action: () => void): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			action();
		}
	}

	function toggleCollapse(): void {
		if (browser && window.innerWidth <= 1280) {
			mobileOpen = !mobileOpen;
		} else {
			expanded = !expanded;
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
		if (browser && window.innerWidth <= 1280) {
			mobileOpen = false;
		}
		if (isVibeDevSession(session) || isVibeDevThread(session.ui_thread_id)) {
			await goto(`/vibe?session=${encodeURIComponent(session.id)}`);
			return;
		}
		const targetThread = normalizeThreadId(session.ui_thread_id);
		if (targetThread === 'vibedev') {
			await goto(`/vibe?session=${encodeURIComponent(session.id)}`);
			return;
		}
		const chatPath = `/t/${encodeURIComponent(targetThread)}/chat`;
		const chatHref = `${chatPath}?session=${encodeURIComponent(session.id)}`;
		const onTargetChat = browser && window.location.pathname === chatPath;
		if (session.id === activeSessionId && !viewingArchivedId && onTargetChat) {
			return;
		}
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

	function selectThread(threadId: string): void {
		if (browser && window.innerWidth <= 1280) {
			mobileOpen = false;
		}
		if (isVibeDevThread(threadId)) {
			void goto('/vibe');
			return;
		}
		void goto(`/t/${encodeURIComponent(threadId)}`);
	}

	function newSession(): void {
		if (browser && window.innerWidth <= 1280) {
			mobileOpen = false;
		}
		void chatStore.newExecution(threadFilter ?? 'general');
	}

	function newThread(): void {
		if (browser && window.innerWidth <= 1280) {
			mobileOpen = false;
		}
		void goto('/t/new');
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

	function formatTimestamp(value?: string | number | null): string {
		if (value == null || value === '') return '';
		const date = new Date(value);
		if (Number.isNaN(date.getTime())) return '';
		const now = new Date();
		const isToday =
			date.getFullYear() === now.getFullYear() &&
			date.getMonth() === now.getMonth() &&
			date.getDate() === now.getDate();
		return isToday
			? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
			: date.toLocaleDateString([], { month: 'short', day: 'numeric' });
	}

	onDestroy(() => {
		if (searchTimer) clearTimeout(searchTimer);
	});
</script>

{#if mobileOpen}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="chat-history-backdrop" on:click={() => (mobileOpen = false)} role="presentation"></div>
{/if}

<aside
	class="chat-history-rail"
	class:chat-history-rail--collapsed={!expanded}
	class:chat-history-rail--mobile-open={mobileOpen}
	aria-label="Chat history"
>
	{#if expanded || mobileOpen}
		<header class="rail-header">
			<div class="rail-title-row">
				<button
					type="button"
					class="rail-toggle-btn"
					on:click={toggleCollapse}
					aria-label="Collapse history"
					title="Collapse history"
				>
					⟨
				</button>
				<h3 class="rail-title">History</h3>
				{#if total > 0}
					<span class="rail-count">{total}</span>
				{/if}
			</div>

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
		</header>

		{#if isGlobalSearch}
			<div class="global-search-scope" aria-label="Searching all history">
				<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M4 6h16M4 12h16M4 18h16"/></svg>
				<strong>All history</strong>
				<span>Personal + Auto</span>
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
			{#if !isGlobalSearch}
				<div class="lane-switch" role="group" aria-label="Archived state">
					<button class:active={archiveView === 'active'} aria-pressed={archiveView === 'active'} on:click={() => setArchiveView('active')}>Active</button>
					<button class:active={archiveView === 'archived'} aria-pressed={archiveView === 'archived'} on:click={() => setArchiveView('archived')}>Archived</button>
				</div>
			{/if}
			<label class="search-box">
				<span class="search-icon-wrap" aria-hidden="true">
					<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
				</span>
				<input
					type="search"
					value={searchInput}
					placeholder="Search history…"
					aria-label="Search history"
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
					{#each Array(6) as _}
						<div class="skeleton-row"><span></span><i></i></div>
					{/each}
				</div>
			{:else if loadError}
				<div class="state-message error-state">
					<p>History could not be loaded.</p>
					<button type="button" on:click={reloadCurrentPage}>Retry</button>
				</div>
			{:else if isGlobalSearch}
				{#if searchResults.length === 0}
					<p class="empty">No matching history.</p>
				{:else}
					{#each searchResults as result}
						{#if result.kind === 'session'}
							<div
								class="item"
								class:archived={result.session.status === 'archived'}
								class:active={result.session.id === currentViewId}
								role="button"
								tabindex="0"
								on:click={() => openSelectedSession(result.session)}
								on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(result.session))}
							>
								<div class="row1">
									<span class="dot" class:dim={result.session.status === 'archived'}></span>
									<span class="title" title={result.session.title || 'Untitled session'}>{result.session.title || 'Untitled session'}</span>
									<span class="ts">{formatTimestamp(result.session.updated_at)}</span>
								</div>
								<div class="row2">
									<span class="thread-tag">#{result.session.ui_thread_id || 'general'}</span>
									<span class="actions">
										{#if result.session.status === 'archived'}
											<button class="action" title="Restore" aria-label="Restore session" on:click={(e) => unarchiveSession(result.session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>
										{:else if !result.session.is_default_session && !result.session.internal_voice}
											<button class="action" title="Archive" aria-label="Archive session" on:click={(e) => archiveSession(result.session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>
										{/if}
										{#if !result.session.is_default_session && !result.session.internal_voice}
											<button class="action danger" title="Delete" aria-label="Delete session" on:click={(e) => deleteSession(result.session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>
										{/if}
									</span>
								</div>
							</div>
						{:else}
							<div
								class="item search-thread-item"
								class:archived={result.thread.archived}
								role="button"
								tabindex="0"
								on:click={() => selectThread(result.thread.id)}
								on:keydown={(event) => handleItemKeydown(event, () => selectThread(result.thread.id))}
							>
								<div class="row1">
									<span class="hash">#</span>
									<span class="title" title={result.thread.name || result.thread.id}>{result.thread.name || result.thread.id}</span>
									<span class="ts">{formatTimestamp(result.thread.updated_at)}</span>
								</div>
								<div class="row2">
									<span class="kind-tag">Thread</span>
									<span class="actions">
										{#if result.thread.archived}
											<button class="action" title="Restore" aria-label="Restore thread" on:click={(e) => restoreThread(result.thread.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>
										{:else if result.thread.id !== 'general'}
											<button class="action" title="Archive" aria-label="Archive thread" on:click={(e) => archiveThread(result.thread.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>
										{/if}
										{#if result.thread.id !== 'general'}
											<button class="action danger" title="Delete" aria-label="Delete thread" on:click={(e) => deleteThread(result.thread.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>
										{/if}
									</span>
								</div>
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
						<div
							class="item"
							class:active={session.id === currentViewId}
							role="button"
							tabindex="0"
							on:click={() => openSelectedSession(session)}
							on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(session))}
						>
							<div class="row1">
								<span class="dot"></span>
								<span class="title" title={session.title || 'Untitled session'}>{session.title || 'Untitled session'}</span>
								<span class="ts">{formatTimestamp(session.updated_at)}</span>
							</div>
							<div class="row2">
								<span class="thread-tag">#{session.ui_thread_id || 'general'}</span>
								{#if !session.is_default_session && !session.internal_voice}
									<span class="actions">
										<button class="action" title="Archive" aria-label="Archive session" on:click={(e) => archiveSession(session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M10 12h4"/></svg></button>
										<button class="action danger" title="Delete" aria-label="Delete session" on:click={(e) => deleteSession(session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>
									</span>
								{/if}
							</div>
						</div>
					{/each}

					{#if archivedSessions.length > 0}<div class="section-h">Archived</div>{/if}
					{#each archivedSessions as session (session.id)}
						<div
							class="item archived"
							class:active={session.id === currentViewId}
							role="button"
							tabindex="0"
							on:click={() => openSelectedSession(session)}
							on:keydown={(event) => handleItemKeydown(event, () => openSelectedSession(session))}
						>
							<div class="row1">
								<span class="dot dim"></span>
								<span class="title" title={session.title || 'Untitled session'}>{session.title || 'Untitled session'}</span>
								<span class="ts">{formatTimestamp(session.updated_at)}</span>
							</div>
							<div class="row2">
								<span class="thread-tag">#{session.ui_thread_id || 'general'}</span>
								<span class="actions">
									<button class="action" title="Restore" aria-label="Restore session" on:click={(e) => unarchiveSession(session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="3" y="3" width="18" height="5" rx="1"/><path d="M5 8v11h14V8M9 14l3-3 3 3M12 11v7"/></svg></button>
									{#if !session.is_default_session && !session.internal_voice}
										<button class="action danger" title="Delete" aria-label="Delete session" on:click={(e) => deleteSession(session.id, e)}><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/></svg></button>
									{/if}
								</span>
							</div>
						</div>
					{/each}
				{/if}
			{:else}
				{#if threads.length === 0}
					<p class="empty">{archiveView === 'archived' ? 'No archived threads.' : 'No matching threads.'}</p>
				{:else}
					{#each threads as thread (thread.id)}
						<div
							class="item thread-item"
							class:archived={thread.archived}
							role="button"
							tabindex="0"
							on:click={() => selectThread(thread.id)}
							on:keydown={(event) => handleItemKeydown(event, () => selectThread(thread.id))}
						>
							<span class="hash">#</span>
							<span class="title" title={thread.name || thread.id}>{thread.name || thread.id}</span>
							<span class="ts">{formatTimestamp(thread.updated_at)}</span>
						</div>
					{/each}
				{/if}
			{/if}
		</div>

		<footer class="pagination" aria-label="History pagination">
			<button type="button" on:click={() => (offset = Math.max(0, offset - PAGE_SIZE))} disabled={!hasPrevious || isLoading} aria-label="Previous page">
				<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m15 18-6-6 6-6"/></svg>
			</button>
			<span>{pageStart}-{pageEnd} of {total}</span>
			<button type="button" on:click={() => (offset += PAGE_SIZE)} disabled={!hasNext || isLoading} aria-label="Next page">
				<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m9 18 6-6-6-6"/></svg>
			</button>
		</footer>
	{:else}
		<!-- Collapsed narrow strip on desktop -->
		<div class="collapsed-strip">
			<button
				type="button"
				class="rail-toggle-btn rail-toggle-btn--collapsed"
				on:click={toggleCollapse}
				aria-label="Expand history"
				title="Expand history"
			>
				⟩
			</button>
			<button
				type="button"
				class="collapsed-new-btn"
				on:click={newSession}
				title="Start new chat"
				aria-label="Start new chat"
			>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
					<path d="M12 5v14M5 12h14" />
				</svg>
			</button>
			<div class="collapsed-indicator" title="History">
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
					<circle cx="12" cy="12" r="10"/>
					<polyline points="12 6 12 12 16 14"/>
				</svg>
			</div>
		</div>
	{/if}
</aside>

<style>
	.chat-history-backdrop {
		display: none;
	}

	.chat-history-rail {
		width: 260px;
		min-width: 260px;
		height: 100%;
		flex-shrink: 0;
		background: color-mix(in srgb, var(--bg-card, #fff) 88%, transparent);
		backdrop-filter: blur(12px);
		-webkit-backdrop-filter: blur(12px);
		border-right: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		display: flex;
		flex-direction: column;
		overflow: hidden;
		user-select: none;
		z-index: 10;
		transition: width 180ms ease, min-width 180ms ease;
	}

	.chat-history-rail--collapsed {
		width: 44px;
		min-width: 44px;
	}

	.rail-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 10px 10px 8px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		gap: 6px;
	}

	.rail-title-row {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
	}

	.rail-title {
		margin: 0;
		font-family: var(--font-display, sans-serif);
		font-size: 13.5px;
		font-weight: 650;
		color: var(--text-primary, #1a1a1a);
	}

	.rail-count {
		font-family: var(--font-mono, monospace);
		font-size: 10px;
		color: var(--text-muted, #777);
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		padding: 1px 5px;
		border-radius: 4px;
	}

	.rail-toggle-btn {
		width: 24px;
		height: 24px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		color: var(--text-muted, #777);
		border-radius: 5px;
		cursor: pointer;
		font-size: 12px;
		font-weight: 700;
		transition: background 120ms ease, color 120ms ease;
	}

	.rail-toggle-btn:hover {
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
	}

	.rail-new-wrap {
		position: relative;
		display: inline-flex;
	}

	.rail-new-btn {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		padding: 4px 8px;
		border-radius: 6px;
		border: 1px solid color-mix(in srgb, var(--accent-primary, #c2502a) 24%, transparent);
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.08));
		color: var(--accent-primary, #c2502a);
		font-family: var(--font-primary, sans-serif);
		font-size: 11.5px;
		font-weight: 600;
		cursor: pointer;
		transition: background 120ms ease, transform 120ms ease;
	}

	.rail-new-btn:hover {
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.16));
		transform: translateY(-1px);
	}

	.rail-new-btn.active {
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.2));
	}

	.new-chevron {
		transition: transform 150ms ease;
	}

	.new-chevron.open {
		transform: rotate(180deg);
	}

	.rail-new-menu {
		position: absolute;
		top: calc(100% + 4px);
		right: 0;
		z-index: 50;
		min-width: 140px;
		background: var(--bg-surface, #fff);
		border: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.12));
		border-radius: 8px;
		box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15);
		padding: 4px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.rail-new-menu-item {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		padding: 6px 10px;
		border: 0;
		border-radius: 6px;
		background: transparent;
		color: var(--text-primary, #1a1a1a);
		font-family: var(--font-primary, sans-serif);
		font-size: 12px;
		font-weight: 500;
		text-align: left;
		cursor: pointer;
		transition: background 120ms ease, color 120ms ease;
	}

	.rail-new-menu-item:hover {
		background: var(--bg-hover, rgba(0, 0, 0, 0.05));
		color: var(--accent-primary, #c2502a);
	}

	.collapsed-strip {
		display: flex;
		flex-direction: column;
		align-items: center;
		padding: 10px 0;
		gap: 12px;
		height: 100%;
	}

	.collapsed-new-btn {
		width: 30px;
		height: 30px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border-radius: 8px;
		border: 1px solid color-mix(in srgb, var(--accent-primary, #c2502a) 25%, transparent);
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.1));
		color: var(--accent-primary, #c2502a);
		cursor: pointer;
		transition: background 120ms ease, transform 120ms ease;
	}

	.collapsed-new-btn:hover {
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.2));
		transform: scale(1.05);
	}

	.collapsed-indicator {
		color: var(--text-faint, #aaa);
		margin-top: auto;
		margin-bottom: 8px;
	}

	.tabs {
		display: grid;
		grid-template-columns: 1fr 1fr;
		padding: 0 8px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
	}

	.tabs button {
		height: 34px;
		padding: 0 6px;
		font-family: var(--font-primary, sans-serif);
		font-size: 11.5px;
		font-weight: 550;
		color: var(--text-muted, #777);
		background: transparent;
		border: 0;
		border-bottom: 2px solid transparent;
		cursor: pointer;
	}

	.tabs button.active {
		color: var(--text-primary, #1a1a1a);
		border-bottom-color: var(--accent-primary, #c2502a);
	}

	.global-search-scope {
		min-height: 32px;
		display: grid;
		grid-template-columns: 16px 1fr auto;
		align-items: center;
		gap: 6px;
		padding: 0 10px;
		color: var(--text-muted, #777);
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
	}

	.global-search-scope strong {
		color: var(--text-primary, #1a1a1a);
		font-size: 11px;
	}

	.global-search-scope span {
		font: 9.5px var(--font-mono, monospace);
	}

	.controls {
		display: grid;
		gap: 6px;
		padding: 8px 8px 6px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.lane-switch {
		display: grid;
		grid-template-columns: 1fr 1fr;
		padding: 2px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		border-radius: 6px;
	}

	.lane-switch button {
		height: 24px;
		border: 0;
		border-radius: 4px;
		background: transparent;
		color: var(--text-muted, #777);
		font-size: 11px;
		cursor: pointer;
	}

	.lane-switch button.active {
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
	}

	.scope-label {
		height: 28px;
		display: flex;
		align-items: center;
		padding: 0 8px;
		overflow: hidden;
		color: var(--text-muted, #777);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border-radius: 6px;
		font: 10.5px var(--font-mono, monospace);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.search-box {
		height: 32px;
		box-sizing: border-box;
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0 8px;
		color: var(--text-muted, #777);
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		overflow: hidden;
		-webkit-tap-highlight-color: transparent;
	}

	.search-box:focus-within {
		border-color: var(--accent-primary, #c2502a);
		box-shadow: 0 0 0 2px var(--accent-primary-soft, rgba(194, 80, 42, 0.1));
	}

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
		font-size: 11.5px !important;
		line-height: 1.2 !important;
		border-radius: 0 !important;
		box-sizing: border-box !important;
	}

	.search-box input::-webkit-search-decoration,
	.search-box input::-webkit-search-cancel-button {
		-webkit-appearance: none;
		display: none;
	}

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

	.search-box button:hover {
		color: var(--text-primary, #1a1a1a);
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.list {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 6px 6px 10px;
	}


	.section-h {
		padding: 8px 8px 3px;
		color: var(--text-faint, #999);
		font: 9.5px var(--font-mono, monospace);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.item {
		padding: 7px 8px;
		margin-bottom: 2px;
		border-radius: 6px;
		cursor: pointer;
		transition: background 120ms ease;
		outline: none;
	}

	.item:hover,
	.item:focus-visible {
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	.item.active {
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.1));
		border-left: 2.5px solid var(--accent-primary, #c2502a);
	}

	.item.archived {
		opacity: 0.7;
	}

	.row1 {
		display: flex;
		align-items: baseline;
		gap: 6px;
		margin-bottom: 2px;
	}

	.row2 {
		display: flex;
		align-items: center;
		gap: 6px;
		min-height: 20px;
		padding-left: 12px;
	}

	.dot {
		width: 6px;
		height: 6px;
		flex: 0 0 6px;
		align-self: center;
		border-radius: 50%;
		background: var(--accent-primary, #c2502a);
	}

	.dot.dim {
		background: var(--text-faint, #aaa);
	}

	.hash {
		flex: 0 0 auto;
		color: var(--text-muted, #777);
		font: 12px var(--font-mono, monospace);
	}

	.title {
		min-width: 0;
		flex: 1;
		overflow: hidden;
		color: var(--text-primary, #1a1a1a);
		font-size: 12px;
		font-weight: 520;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.ts {
		flex: 0 0 auto;
		color: var(--text-muted, #888);
		font: 10px var(--font-mono, monospace);
	}

	.thread-tag {
		max-width: 140px;
		overflow: hidden;
		padding: 1px 5px;
		border-radius: 4px;
		color: var(--text-muted, #777);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		font: 10px var(--font-mono, monospace);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.kind-tag {
		flex: 0 0 auto;
		padding: 1px 5px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 4px;
		color: var(--text-muted, #777);
		font-size: 9.5px;
	}

	.actions {
		margin-left: auto;
		display: inline-flex;
		gap: 2px;
		opacity: 0;
	}

	.item:hover .actions,
	.item:focus-within .actions {
		opacity: 1;
	}

	.action {
		width: 20px;
		height: 20px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		color: var(--text-muted, #777);
		background: transparent;
		border: 1px solid transparent;
		border-radius: 4px;
		cursor: pointer;
	}

	.action:hover {
		color: var(--text-primary, #1a1a1a);
		background: var(--bg-elevated, #fff);
		border-color: var(--border-soft, rgba(0, 0, 0, 0.1));
	}

	.action.danger:hover {
		color: var(--color-error, #d4453a);
		background: var(--color-error-soft, rgba(212, 69, 58, 0.1));
	}

	.empty,
	.state-message {
		margin: 0;
		padding: 32px 16px;
		text-align: center;
		color: var(--text-muted, #777);
		font-size: 12px;
	}

	.state-message button {
		margin-top: 8px;
		padding: 4px 8px;
		color: var(--text-primary, #1a1a1a);
		background: var(--bg-soft, #f2f2f2);
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.12));
		border-radius: 5px;
		cursor: pointer;
	}

	.skeleton-list {
		padding: 2px;
	}

	.skeleton-row {
		height: 44px;
		display: grid;
		align-content: center;
		gap: 6px;
		padding: 0 8px;
	}

	.skeleton-row span,
	.skeleton-row i {
		display: block;
		border-radius: 4px;
		background: linear-gradient(90deg, var(--bg-soft, #eee) 25%, var(--bg-elevated, #f8f8f8) 50%, var(--bg-soft, #eee) 75%);
		background-size: 200% 100%;
		animation: shimmer 1.35s infinite linear;
	}

	.skeleton-row span {
		width: 70%;
		height: 9px;
	}

	.skeleton-row i {
		width: 36%;
		height: 7px;
	}

	.pagination {
		height: 40px;
		flex: 0 0 40px;
		display: grid;
		grid-template-columns: 24px 1fr 24px;
		align-items: center;
		gap: 6px;
		padding: 0 10px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		background: var(--bg-card, #fff);
	}

	.pagination span {
		text-align: center;
		color: var(--text-muted, #777);
		font: 10px var(--font-mono, monospace);
	}

	.pagination button {
		width: 24px;
		height: 24px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		background: transparent;
		border: 1px solid transparent;
		color: var(--text-muted, #777);
		border-radius: 4px;
		cursor: pointer;
	}

	.pagination button:hover:not(:disabled) {
		color: var(--text-primary, #1a1a1a);
		background: var(--bg-soft, #f1f1f1);
	}

	.pagination button:disabled {
		opacity: 0.3;
		cursor: default;
	}

	@keyframes shimmer {
		from { background-position: 200% 0; }
		to { background-position: -200% 0; }
	}

	@media (max-width: 1280px) {
		.chat-history-backdrop {
			display: block;
			position: fixed;
			inset: 0;
			top: var(--app-header-height, 48px);
			left: 48px;
			background: rgba(0, 0, 0, 0.45);
			backdrop-filter: blur(2px);
			-webkit-backdrop-filter: blur(2px);
			z-index: 115;
		}

		.chat-history-rail {
			position: fixed;
			top: var(--app-header-height, 48px);
			left: 48px;
			bottom: 0;
			width: 280px;
			max-width: calc(100vw - 56px);
			height: calc(100vh - var(--app-header-height, 48px));
			z-index: 120;
			box-shadow: 4px 0 24px rgba(0, 0, 0, 0.25);
			transition: transform 240ms cubic-bezier(0.16, 1, 0.3, 1), visibility 240ms ease, opacity 240ms ease;
			transform: translateX(-100%);
			visibility: hidden;
			opacity: 0;
			pointer-events: none;
		}

		.chat-history-rail--mobile-open {
			transform: translateX(0);
			visibility: visible;
			opacity: 1;
			pointer-events: auto;
		}
	}
</style>
