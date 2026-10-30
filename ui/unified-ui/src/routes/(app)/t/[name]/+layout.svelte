<script lang="ts">
	import { browser } from '$app/environment';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { get, writable } from 'svelte/store';
	import ThreadSessionBar from '$lib/threads/ThreadSessionBar.svelte';
	import ChatWorkspacePage from '$lib/threads/ChatWorkspacePage.svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { loadAgents, personalAgentList } from '$lib/stores/agentStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { chatStore } from '$lib/stores/chatStore';
	import { taskStore } from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import { normalizeThreadId } from '$lib/threads/normalizeThreadId';
	import { setThreadPageContext, type ThreadPageState } from '$lib/threads/threadPageContext';

	// ── Thread shell ──────────────────────────────────────────────────────
	// Owns the per-thread lifecycle shared by the chat / tasks / settings child
	// routes: store start/stop, the data load, scope + route-param reactivity,
	// the tab bar, and the Developer-Mode keyboard shortcut. The resolved thread
	// identity + load state are published via context (see threadPageContext);
	// task-specific state stays in the Tasks route.

	let mounted = false;
	let loadToken = 0;
	let lastScopeKey = '';
	let threadName = normalizeThreadId(get(page).params.name);
	let loadingThread = false;
	let threadError: string | null = null;
	let activeTab: 'chat' | 'tasks' | 'settings' | 'files' | 'models' = 'chat';

	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: threadDetail =
		$threadStore.activeThread && $threadStore.activeThread.id === threadName
			? $threadStore.activeThread
			: null;
	$: threadDisplayName = threadDetail?.name || threadName;
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

	async function loadThread(nextThread: string): Promise<void> {
		if (!browser) return;
		const token = ++loadToken;
		loadingThread = true;
		threadError = null;
		try {
			if (v2Events.getConnectionState() === 'CLOSED') {
				v2Events.connectGlobal();
			}
			await Promise.all([
				taskStore.loadTasks(),
				ensurePersonalAgentsLoaded(),
				threadStore.ensureThread(nextThread)
			]);
		} catch (error) {
			if (token === loadToken) {
				threadError = error instanceof Error ? error.message : `Failed to load #${nextThread}`;
			}
		} finally {
			if (token === loadToken) {
				loadingThread = false;
			}
		}
	}

	// Route-param change (client nav between threads) → reload.
	$: if (mounted && browser) {
		const nextThread = normalizeThreadId($page.params.name);
		if (nextThread !== threadName) {
			threadName = nextThread;
			void loadThread(nextThread);
		}
	}

	// Scope (principal/workspace) change → reload.
	$: if (browser && mounted && lastScopeKey && currentScopeKey !== lastScopeKey) {
		lastScopeKey = currentScopeKey;
		void loadThread(threadName);
	}

	onMount(() => {
		if (!browser) return;
		mounted = true;
		threadName = normalizeThreadId(get(page).params.name);
		lastScopeKey = `${get(scopeIdentityStore).principal}:${get(scopeIdentityStore).workspace}`;
		taskStore.start();
		threadStore.start();
		if (v2Events.getConnectionState() === 'CLOSED') {
			v2Events.connectGlobal();
		}
		void loadThread(threadName);

		return () => {
			taskStore.stop();
			threadStore.stop();
			mounted = false;
		};
	});
</script>

<svelte:head>
	<title>#{threadDisplayName} · Magican</title>
</svelte:head>

<svelte:window
	on:keydown={(event) => {
		if (
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
</style>
