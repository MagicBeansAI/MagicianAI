<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { getCurrentScopeCredentialRevision, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { readCompletedTaskReport } from './completedTaskReport';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import AnswerVerificationInfo from '$lib/magician/chat/components/AnswerVerificationInfo.svelte';
	import { splitAnswerVerification } from '$lib/magician/chat/answerVerification';

	export let content: string;
	export let sessionId: string | null = null;
	export let verificationContent: string | null = null;
	export let taskId: string | null = null;
	export let executionId: string | null = null;
	export let showVerification = true;
	let root: HTMLDivElement;
	let visible = false;
	let disposed = false;
	let source: string | null = null;
	let sourceKey: string | null = null;
	let requestedKey: string | null = null;
	let retryTimer: ReturnType<typeof setTimeout> | null = null;
	let retries = 0;
	onMount(() => {
		if (typeof IntersectionObserver === 'undefined') { visible = true; return; }
		const observer = new IntersectionObserver(entries => {
			if (entries.some(entry => entry.isIntersecting)) { visible = true; observer.disconnect(); }
		});
		observer.observe(root);
		return () => observer.disconnect();
	});
	onDestroy(() => { disposed = true; if (retryTimer) clearTimeout(retryTimer); });
	$: targetKey = taskId && executionId && $scopeIdentityStore.isResolved
		? JSON.stringify([getCurrentScopeCredentialRevision(), $scopeIdentityStore.principal, $scopeIdentityStore.workspace, taskId, executionId]) : null;
	$: loadSource(targetKey, visible, taskId, executionId, $scopeIdentityStore.principal, $scopeIdentityStore.workspace);
	function loadSource(key: string | null, inView: boolean, task: string | null, execution: string | null, principal: string, workspace: string): void {
		if (key !== sourceKey) {
			if (retryTimer) clearTimeout(retryTimer);
			retryTimer = null; retries = 0; sourceKey = key; source = null; requestedKey = null;
		}
		if (!key || !inView || !task || !execution || requestedKey === key) return;
		requestedKey = key;
		void readCompletedTaskReport(task, execution, principal, workspace).then(report => {
			if (disposed || sourceKey !== key) return;
			source = report;
			// Final status can arrive just before its report, or a read can fail
			// transiently. Keep the summary visible and retry only this scoped run.
			if (!report && retries < 2) {
				retries += 1;
				retryTimer = setTimeout(() => {
					retryTimer = null;
					if (!disposed && sourceKey === key) {
						requestedKey = null;
						loadSource(key, true, task, execution, principal, workspace);
					}
				}, 500 * retries);
			}
		});
	}

	$: answer = splitAnswerVerification(source ?? content);
	// A report returned as one HTML fence is a document, not a source example.
	// Keep mixed prose/code untouched, and use Markdown's existing HTML sanitizer.
	// A summary cut to length can lose the closing fence; it is still a page.
	$: htmlReport = /^\s*```html[ \t]*\r?\n([\s\S]*?)(?:\r?\n```\s*)?$/i.exec(answer.body);
	$: unwrapped = htmlReport && !/^[ \t]*```/m.test(htmlReport[1]) && /^\s*<(?:!doctype\s+html|html\b|h[1-6]\b|p\b|table\b|div\b|section\b|article\b|ul\b|ol\b|aside\b)/i.test(htmlReport[1])
		? htmlReport[1] : answer.body;
	$: rendered = htmlReport && unwrapped !== answer.body ? splitAnswerVerification(unwrapped) : { body: unwrapped, verification: null };
	$: verification = answer.verification ?? rendered.verification ?? splitAnswerVerification(content).verification ?? splitAnswerVerification(verificationContent ?? '').verification;
	$: body = rendered.body;
</script>

<div bind:this={root} class="task-report-host">
{#if verification && showVerification}
	<div class="task-report__verification"><AnswerVerificationInfo {verification} /></div>
{/if}
{#if body}<div class="task-report"><ChatMarkdown content={body} {sessionId} /></div>{/if}
</div>

<style>
	.task-report-host { min-width: 0; max-width: 100%; }
	.task-report__verification { display: flex; justify-content: flex-end; }
	.task-report { min-width: 0; max-width: 100%; overflow-x: auto; }
	/* Keep each column's longest word readable. Let the report scroll on
	   narrow surfaces instead of shrinking status words to a few letters. */
	.task-report :global(th), .task-report :global(td) {
		overflow-wrap: break-word;
		word-break: normal;
	}
	.task-report :global(th) { white-space: nowrap; }
</style>
