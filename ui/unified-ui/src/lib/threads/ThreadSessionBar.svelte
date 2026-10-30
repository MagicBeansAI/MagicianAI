<script lang="ts">
	import UsageTotals from '$lib/magician/components/UsageTotals.svelte';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import type { Task } from '$lib/stores/taskStore';
	import { normalizeThreadId } from './normalizeThreadId';
	import { workThreadHref } from '$lib/work/routes';

	export let threadId = 'general';
	export let live = false;
	export let threadName: string | null = null;
	export let activeTab: 'chat' | 'tasks' | 'settings' | 'files' | 'models' = 'chat';
	export let chatHref: string | null = null;
	export let sessionId: string | null = null;
	export let filesHref: string | null = null;
	export let modelsHref: string | null = null;
	export let tasks: Pick<Task, 'uiThreadId' | 'status'>[] = [];
	export let showNavigation = true;
	$: normalizedId = normalizeThreadId(threadId);
	$: base = `/t/${encodeURIComponent(normalizedId)}`;
	$: sessionQuery = sessionId ? `?session=${encodeURIComponent(sessionId)}` : '';
	$: threadTasks = tasks.filter(task => normalizeThreadId(task.uiThreadId) === normalizedId);
	$: running = threadTasks.filter(task => task.status === 'running' || task.status === 'planning').length;
	$: waiting = threadTasks.filter(task => task.status === 'paused').length;
	$: failed = threadTasks.filter(task => task.status === 'failed').length;
</script>

<nav class="thread-session-bar" aria-label={showNavigation ? 'Thread workspace' : 'Chat session details'}>
	{#if showNavigation}
		<span class="thread-session-bar__title" title={`#${threadName || normalizedId}`}>#{threadName || normalizedId}</span>
		<a class="thread-session-bar__tab" class:active={activeTab === 'chat'} aria-current={activeTab === 'chat' ? 'page' : undefined} href={chatHref || `${base}/chat${sessionQuery}`}>Chat</a>
		<a class="thread-session-bar__tab" class:active={activeTab === 'tasks'} aria-current={activeTab === 'tasks' ? 'page' : undefined} href={workThreadHref(normalizedId)}>Tasks <span class="thread-session-bar__count">{threadTasks.length}</span></a>
		<a class="thread-session-bar__tab" class:active={activeTab === 'settings'} aria-current={activeTab === 'settings' ? 'page' : undefined} href={`${base}/settings${sessionQuery}`}>Settings</a>
	{/if}
	<a class="thread-session-bar__tab" class:active={activeTab === 'files'} aria-current={activeTab === 'files' ? 'page' : undefined} href={filesHref || `${base}/files${sessionQuery}`}>Files</a>
	<a class="thread-session-bar__tab" class:active={activeTab === 'models'} aria-current={activeTab === 'models' ? 'page' : undefined} href={modelsHref || `${base}/models${sessionQuery}`}>Models</a>
	{#if showNavigation && (running || waiting || failed)}
		<div class="thread-session-bar__badges">
			{#if running}<Badge text={`${running} running`} color="info" />{/if}
			{#if waiting}<a href={workThreadHref(normalizedId)}><Badge text={`${waiting} waiting`} color="warning" /></a>{/if}
			{#if failed}<a href={workThreadHref(normalizedId)}><Badge text={`${failed} failed`} color="error" /></a>{/if}
		</div>
	{/if}
	{#if sessionId || $$slots.details}<div class="thread-session-bar__details">
		{#if sessionId}<UsageTotals kind="chat" id={sessionId} {live} />{/if}
		<slot name="details" />
	</div>{/if}
</nav>

<style>
	.thread-session-bar { display: flex; flex-wrap: wrap; align-items: center; gap: .2rem; flex-shrink: 0; min-width: 0; padding: .3rem 1rem; border-bottom: 1px solid var(--border-soft); }
	.thread-session-bar__title { min-width: 0; max-width: 15rem; overflow: hidden; text-overflow: ellipsis; font-size: .9rem; font-weight: 700; color: var(--text-primary); white-space: nowrap; margin-right: .3rem; }
	.thread-session-bar__tab { padding: .4rem .6rem; font-size: .78rem; font-weight: 600; color: var(--text-secondary); text-decoration: none; white-space: nowrap; border-bottom: 2px solid transparent; }
	.thread-session-bar__tab:hover { color: var(--text-primary); }
	.thread-session-bar__tab.active { color: var(--accent-primary); border-bottom-color: var(--accent-primary); }
	.thread-session-bar__count { font-size: .65rem; opacity: .7; }
	.thread-session-bar__badges { display: flex; flex-wrap: wrap; align-items: center; gap: .35rem; margin-left: .4rem; }
	.thread-session-bar__badges a { text-decoration: none; }
	.thread-session-bar__details { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: .65rem; margin-left: auto; min-width: 0; max-width: 100%; }
	@media (max-width: 640px) {
		.thread-session-bar { padding-inline: .5rem; }
		.thread-session-bar__title { max-width: 10rem; }
		.thread-session-bar__details { flex-basis: 100%; gap: .4rem; }
	}
</style>
