<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { get, writable } from 'svelte/store';
	import ThreadSessionBar from '$lib/threads/ThreadSessionBar.svelte';
	import ChatWorkspacePage from '$lib/threads/ChatWorkspacePage.svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { loadAgents, personalAgentList } from '$lib/stores/agentStore';
	import { resolveConfirmation } from '$lib/stores/confirmationStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { chatStore } from '$lib/stores/chatStore';
	import { taskStore } from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import { normalizeThreadId } from '$lib/threads/normalizeThreadId';
	import { createThreadAndOpen, promptForThreadName } from '$lib/threads/newThread';
	import { setThreadPageContext, type ThreadPageState } from '$lib/threads/threadPageContext';
	import { canonicalThreadUrl, findThreadByKey, isReservedThreadKey } from '$lib/threads/threadKeys';
	import { isInExclusiveKeyboardScope } from '$lib/shared/keyboardTargets';
	import { timedFetch } from '$lib/shared/fetch';

	// ── Thread shell ──────────────────────────────────────────────────────
	// Owns the per-thread lifecycle shared by the chat / tasks / settings child
	// routes: store start/stop, the data load, scope + route-param reactivity,
	// the tab bar, and the Developer-Mode keyboard shortcut. The resolved thread
	// identity + load state are published via context (see threadPageContext);
	// task-specific state stays in the Tasks route.
	//
	// The route param is a thread KEY: the thread's slug (follows its name), a
	// former slug, or its permanent id. The server resolves it
	// (`GET /ui-threads/{key}`); `threadName` is always the canonical id — it is
	// what sessions, tasks and filters use — and child routes render only once
	// it is known, so nothing is ever filed under a slug. When the URL does not
	// carry the current slug it is replaced in place (sub-path and query kept).
	// `/t/new` is the "name a new thread" page and never a thread.

	let mounted = false;
	let loadToken = 0;
	let lastScopeKey = '';
	let handledRouteKey: string | null = null;
	/** Canonical id of the open thread; '' until the route key resolves. */
	let threadName = '';
	/** The id whose data the last completed load fetched. */
	let loadedThreadId: string | null = null;
	let loadingThread = false;
	let threadError: string | null = null;
	let newThreadPromptOpen = false;
	let creatingThread = false;
	/** `/t/new` names a thread stored under the reserved id: checking / found. */
	let checkingReservedKey = false;
	let legacyReservedOpen = false;
	let activeTab: 'chat' | 'tasks' | 'settings' | 'files' | 'models' = 'chat';

	$: routeKey = normalizeThreadId($page.params.name);
	$: reservedRoute = isReservedThreadKey(routeKey);
	$: isNewThreadRoute = reservedRoute && !legacyReservedOpen && !checkingReservedKey;
	$: threadResolved = (!reservedRoute || legacyReservedOpen) && threadName !== '';
	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: threadDetail =
		$threadStore.activeThread && $threadStore.activeThread.id === threadName
			? $threadStore.activeThread
			: null;
	$: threadDisplayName = isNewThreadRoute
		? 'New thread'
		: threadDetail?.name ||
			$threadStore.threads.find((thread) => thread.id === threadName)?.name ||
			threadName ||
			routeKey;
	$: threadDisplayMode = (threadDetail?.display_mode ??
		$threadStore.threads.find((thread) => thread.id === threadName)?.display_mode ??
		'chat') as 'chat' | 'dev';

	$: pathname = $page.url.pathname;
	$: activeTab = pathname.endsWith('/models') ? 'models' : pathname.endsWith('/files') ? 'files' : pathname.endsWith('/tasks')
		? 'tasks'
		: pathname.endsWith('/settings')
			? 'settings'
			: 'chat';

    $: viewedId = $chatStore.viewingArchivedId ?? $chatStore.activeSessionId;
    $: viewedSession = $chatStore.sessions.find(session => session.id === viewedId);
    $: shellSessionId = $page.url.searchParams.get('session') || (viewedSession && normalizeThreadId(viewedSession.ui_thread_id) === threadName ? viewedId : null);

	// Publish shell state to child routes.
	const threadPageStore = writable<ThreadPageState>({
		threadName,
		threadDisplayName: threadName,
		threadDetail: null,
		loadingThread: false,
		threadError: null
	});
	setThreadPageContext(threadPageStore);
	$: threadPageStore.set({ threadName, threadDisplayName, threadDetail, loadingThread, threadError });

	async function ensurePersonalAgentsLoaded(): Promise<void> {
		if (get(personalAgentList).length > 0) return;
		try {
			await loadAgents({ replace: true, clearError: true });
		} catch {
			// Thread task surfaces handle the missing-agent case inline.
		}
	}

	/** Keep the sub-path and query; swap the first segment for the slug. */
	async function replaceWithCanonicalUrl(thread: { id: string; slug?: string | null }): Promise<void> {
		const canonical = canonicalThreadUrl(get(page).url, thread);
		if (!canonical) return;
		await goto(canonical, { replaceState: true, noScroll: true, keepFocus: true });
	}

	async function loadThread(key: string): Promise<void> {
		if (!browser) return;
		const token = ++loadToken;
		loadingThread = true;
		threadError = null;
		try {
			if (v2Events.getConnectionState() === 'CLOSED') {
				v2Events.connectGlobal();
			}
			const [, , detail] = await Promise.all([
				taskStore.loadTasks(),
				ensurePersonalAgentsLoaded(),
				threadStore.ensureThread(key)
			]);
			if (token !== loadToken) return;
			if (!detail) {
				// Unresolved (a failed request): open the key as an id, as before
				// slugs existed. The chat endpoints resolve a slug or alias
				// server-side too, so a session is still filed under the id.
				if (!threadName) threadName = key;
				return;
			}
			threadName = detail.id;
			loadedThreadId = detail.id;
			await replaceWithCanonicalUrl(detail);
		} catch (error) {
			if (token === loadToken) {
				threadError = error instanceof Error ? error.message : `Failed to load #${key}`;
				if (!threadName) threadName = key;
			}
		} finally {
			if (token === loadToken) {
				loadingThread = false;
			}
		}
	}

	async function openNewThreadPrompt(): Promise<void> {
		if (newThreadPromptOpen || creatingThread) return;
		newThreadPromptOpen = true;
		try {
			const name = await promptForThreadName();
			// Cancelled, or the user left `/t/new` while the prompt was open.
			if (!name || !isReservedThreadKey(handledRouteKey)) return;
			creatingThread = true;
			await createThreadAndOpen(name, { replaceState: true });
		} finally {
			newThreadPromptOpen = false;
			creatingThread = false;
		}
	}

	/** A thread whose permanent id is a now-reserved key, if one exists. */
	async function legacyReservedThread(key: string): Promise<{ id: string } | null> {
		const known = get(threadStore).threads.find((thread) => thread.id === key);
		if (known) return known;
		try {
			const response = await timedFetch(`/api/magician/v2/ui-threads/${encodeURIComponent(key)}`);
			if (!response.ok) return null;
			const payload = (await response.json()) as { thread?: { id?: string } };
			return payload.thread?.id === key ? { id: key } : null;
		} catch {
			return null;
		}
	}

	function handleRouteKey(key: string): void {
		if (key === handledRouteKey) return;
		handledRouteKey = key;
		if (isReservedThreadKey(key)) {
			// `/t/new`: ask for a name; never create a thread `new`. A thread
			// stored under the id `new` before it was reserved still opens
			// here (and moves to its slug once the server has one).
			loadToken += 1;
			const token = loadToken;
			loadingThread = false;
			threadError = null;
			threadName = '';
			loadedThreadId = null;
			legacyReservedOpen = false;
			checkingReservedKey = true;
			void legacyReservedThread(key).then((legacy) => {
				if (token !== loadToken || handledRouteKey !== key) return;
				checkingReservedKey = false;
				if (legacy) {
					legacyReservedOpen = true;
					threadName = legacy.id;
					void loadThread(legacy.id);
				} else {
					void openNewThreadPrompt();
				}
			});
			return;
		}
		legacyReservedOpen = false;
		checkingReservedKey = false;
		if (newThreadPromptOpen) {
			// Left `/t/new` with its prompt still up: dismiss it, create nothing.
			resolveConfirmation(false);
		}
		const known = findThreadByKey(get(threadStore).threads, key);
		if (known && known.id === loadedThreadId) {
			// Same thread under another key (our own slug redirect, a rename, an
			// id link): no reload, only make the URL canonical.
			threadName = known.id;
			void replaceWithCanonicalUrl(known);
			return;
		}
		// A known key renders at once; an unknown one waits for the server.
		threadName = known?.id ?? '';
		void loadThread(key);
	}

	// Route-param change (client nav between threads, slug redirects) → resolve.
	$: if (mounted && browser) {
		handleRouteKey(routeKey);
	}

	// Scope (principal/workspace) change → reload.
	$: if (browser && mounted && lastScopeKey && currentScopeKey !== lastScopeKey) {
		lastScopeKey = currentScopeKey;
		if (!reservedRoute) {
			void loadThread(routeKey);
		} else if (legacyReservedOpen && threadName) {
			void loadThread(threadName);
		}
	}

	onMount(() => {
		if (!browser) return;
		lastScopeKey = `${get(scopeIdentityStore).principal}:${get(scopeIdentityStore).workspace}`;
		taskStore.start();
		threadStore.start();
		if (v2Events.getConnectionState() === 'CLOSED') {
			v2Events.connectGlobal();
		}
		// Flipping `mounted` runs the route-key block above, which resolves
		// the thread (or opens the new-thread prompt).
		mounted = true;

		return () => {
			taskStore.stop();
			threadStore.stop();
			mounted = false;
			if (newThreadPromptOpen) resolveConfirmation(false);
		};
	});
</script>

<svelte:head>
	<title>#{threadDisplayName} · Magican</title>
</svelte:head>

<svelte:window
	on:keydown={(event) => {
		if (
			threadResolved &&
			!isInExclusiveKeyboardScope(event.target) &&
			(event.key === 'D' || event.key === 'd') &&
			event.shiftKey &&
			(event.metaKey || event.ctrlKey) &&
			!event.altKey
		) {
			event.preventDefault();
			const nextMode = threadDisplayMode === 'dev' ? 'chat' : 'dev';
			void threadStore.updateThread(threadName, { display_mode: nextMode });
		}
	}}
/>

{#if isNewThreadRoute}
	<div class="thread-new">
		<h1 class="thread-new__title">New thread</h1>
		<p class="thread-new__hint">Name the thread to create it. Its link follows the name.</p>
		<div class="thread-new__actions">
			<button
				type="button"
				class="thread-new__button"
				disabled={newThreadPromptOpen || creatingThread}
				on:click={() => void openNewThreadPrompt()}
			>
				{creatingThread ? 'Creating…' : 'Name a new thread'}
			</button>
			<a class="thread-new__back" href="/t/general/chat">Back to #general</a>
		</div>
	</div>
{:else if threadResolved}
	<div class="thread-page" class:thread-page--chat-workspace={activeTab !== 'tasks'}>
		{#if activeTab === 'settings' || activeTab === 'files' || activeTab === 'models'}
			<ChatWorkspacePage threadId={threadName}>
				<ThreadSessionBar slot="header" threadId={threadName} threadName={threadDisplayName} {activeTab} sessionId={shellSessionId} tasks={$taskStore.tasks} />
				{#if threadError}<p class="thread-error">{threadError}</p>{/if}
				<slot />
			</ChatWorkspacePage>
		{:else}
			{#if activeTab === 'tasks'}
				<ThreadSessionBar threadId={threadName} threadName={threadDisplayName} {activeTab} sessionId={shellSessionId} tasks={$taskStore.tasks} />
			{/if}
			{#if threadError}<p class="thread-error">{threadError}</p>{/if}
			<slot />
		{/if}
	</div>
{:else}
	<div class="thread-page thread-page--chat-workspace">
		{#if threadError}
			<p class="thread-error">{threadError}</p>
		{:else}
			<p class="thread-resolving">Opening thread…</p>
		{/if}
	</div>
{/if}

<style>
	.thread-page {
		width: 100%;
		height: calc(100vh - 4rem);
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.5rem 1rem 0;
	}

	.thread-page--chat-workspace {
		height: 100%;
		min-height: 0;
		gap: 0;
		padding: 0;
	}

	.thread-error {
		margin: 0;
		padding: 0.85rem 1rem;
		border-radius: var(--radius-sm, 12px);
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
		border: 1px solid var(--color-error, #ff6b6b);
		color: var(--text-secondary, #5f6668);
	}

	.thread-resolving {
		margin: 0;
		padding: 1.5rem 1rem;
		color: var(--text-secondary, #5f6668);
		font-size: 0.85rem;
	}

	.thread-new {
		max-width: 28rem;
		margin: 4rem auto 0;
		padding: 0 1rem;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.thread-new__title {
		margin: 0;
		font-size: 1.2rem;
		font-family: var(--font-display, var(--font-primary));
		color: var(--text-primary);
	}

	.thread-new__hint {
		margin: 0;
		font-size: 0.85rem;
		color: var(--text-secondary, #5f6668);
	}

	.thread-new__actions {
		display: flex;
		align-items: center;
		gap: 0.9rem;
		margin-top: 0.4rem;
	}

	.thread-new__button {
		font-family: var(--font-primary);
		font-size: 0.85rem;
		padding: 0.45rem 1rem;
		border-radius: 8px;
		border: 1px solid transparent;
		background: var(--accent-fill);
		color: var(--text-on-accent, #fff);
		cursor: pointer;
	}

	.thread-new__button:disabled {
		opacity: 0.6;
		cursor: default;
	}

	.thread-new__back {
		font-size: 0.8rem;
		color: var(--text-secondary, #5f6668);
	}
</style>
