<script lang="ts">
	import { browser } from '$app/environment';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { get, writable } from 'svelte/store';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { loadAgents, personalAgentList } from '$lib/stores/agentStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
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

	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: threadDetail =
		$threadStore.activeThread && $threadStore.activeThread.id === threadName
			? $threadStore.activeThread
			: null;
	$: threadDisplayName = threadDetail?.name || threadName;
	$: threadDisplayMode = (threadDetail?.display_mode ??
		$threadStore.threads.find((thread) => thread.id === threadName)?.display_mode ??
		'chat') as 'chat' | 'dev';
	$: threadTasks = $taskStore.tasks.filter(
		(task) => normalizeThreadId(task.uiThreadId) === threadName
	);
	$: threadCounts = {
		running: threadTasks.filter((task) => task.status === 'running' || task.status === 'planning')
			.length,
		needsAction: threadTasks.filter((task) => task.status === 'paused' || task.status === 'failed')
			.length,
		total: threadTasks.length
	};

	$: pathname = $page.url.pathname;
	$: activeTab = pathname.endsWith('/tasks')
		? 'tasks'
		: pathname.endsWith('/settings')
			? 'settings'
			: 'chat';
	$: threadBase = `/t/${encodeURIComponent(threadName)}`;

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

<div class="thread-page">
	<nav class="thread-bar">
		<span class="thread-bar__title">#{threadDisplayName}</span>
		<span class="thread-bar__sep">·</span>
		<a class="thread-bar__tab" class:active={activeTab === 'chat'} href={`${threadBase}/chat`}>
			Chat
		</a>
		<a class="thread-bar__tab" class:active={activeTab === 'tasks'} href={`${threadBase}/tasks`}>
			Tasks <span class="thread-bar__count">{threadCounts.total}</span>
		</a>
		<a
			class="thread-bar__tab"
			class:active={activeTab === 'settings'}
			href={`${threadBase}/settings`}
		>
			Settings
		</a>
		<div class="thread-bar__badges">
			{#if threadCounts.running > 0}
				<Badge text={`${threadCounts.running} running`} color="info" />
			{/if}
			{#if threadCounts.needsAction > 0}
				<Badge text={`${threadCounts.needsAction} needs action`} color="warning" />
			{/if}
		</div>
	</nav>

	{#if threadError}
		<p class="thread-error">{threadError}</p>
	{/if}

	<slot />
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

	.thread-bar {
		display: flex;
		align-items: center;
		/* Left-aligned: the floating chat ContextPill owns the top-center zone,
		   so centered tabs end up hidden behind it. Badges stay pinned right. */
		justify-content: flex-start;
		gap: 0.15rem;
		position: relative;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
		padding-bottom: 0.35rem;
	}

	.thread-bar__title {
		font-size: 0.9rem;
		font-weight: 700;
		color: var(--text-primary, #2d3436);
		white-space: nowrap;
	}

	.thread-bar__sep {
		color: var(--text-muted, #8f9799);
		margin: 0 0.2rem;
	}

	.thread-bar__tab {
		position: relative;
		padding: 0.3rem 0.65rem;
		border: none;
		background: none;
		font-family: var(--font-primary);
		font-size: 0.78rem;
		font-weight: 600;
		color: var(--text-muted, #8f9799);
		cursor: pointer;
		transition: color 0.15s;
		white-space: nowrap;
		text-decoration: none;
	}

	.thread-bar__tab:hover {
		color: var(--text-primary, #2d3436);
	}

	.thread-bar__tab.active {
		color: var(--accent-primary, #ff6b6b);
	}

	.thread-bar__tab.active::after {
		content: '';
		position: absolute;
		bottom: -0.4rem;
		left: 0.25rem;
		right: 0.25rem;
		height: 2px;
		background: var(--accent-primary, #ff6b6b);
		border-radius: 2px 2px 0 0;
	}

	.thread-bar__count {
		font-size: 0.65rem;
		font-weight: 700;
		margin-left: 0.2rem;
		opacity: 0.7;
	}

	.thread-bar__badges {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		position: absolute;
		right: 0;
		top: 50%;
		transform: translateY(-50%);
	}

	.thread-error {
		margin: 0;
		padding: 0.85rem 1rem;
		border-radius: var(--radius-sm, 12px);
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
		border: 1px solid var(--color-error, #ff6b6b);
		color: var(--text-secondary, #5f6668);
	}

	@media (max-width: 900px) {
		.thread-bar {
			flex-wrap: wrap;
		}

		.thread-bar__badges {
			position: static;
			transform: none;
			width: 100%;
			justify-content: center;
			margin-top: 0.25rem;
		}
	}
</style>
