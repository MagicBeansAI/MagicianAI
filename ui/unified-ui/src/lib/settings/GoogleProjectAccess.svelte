<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';
	import { parseProjectAccess, type ProjectAccessReport } from './managedBot';

	/** Google Workspace bot, e.g. `gmail-work`; its row carries `project_access: true`. */
	export let bot: string;
	export let expectedScope: { principal: string; workspace: string } | undefined = undefined;

	const BASE = '/api/magician/v2/auth/browser-signin/bot/project-access';
	// The engine runs gcloud for this (up to 45 s per call); give it room.
	const TIMEOUT_MS = 120_000;
	let report: ProjectAccessReport | null = null;
	let checking = false;
	let granting = false;
	let error = '';
	let copied = false;
	/** Not a Google account after all: no step to show. */
	let unsupported = false;
	let disposed = false;
	let latest = 0;

	$: busy = checking || granting;
	$: commandText = report?.commands.join('\n') ?? '';

	function scopeChanged(): boolean {
		if (!expectedScope) return false;
		const scope = get(scopeIdentityStore);
		return scope.principal !== expectedScope.principal || scope.workspace !== expectedScope.workspace;
	}

	async function call(path: string, body: Record<string, string | boolean>): Promise<ProjectAccessReport | null> {
		if (scopeChanged()) throw new Error('Workspace changed. Reload the account list first.');
		const mine = ++latest;
		const response = await timedFetch(path, {
			method: 'POST', cache: 'no-store', redirect: 'error', timeoutMs: TIMEOUT_MS,
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({ bot, ...body })
		});
		const data = await response.json().catch(() => ({}));
		if (!response.ok) {
			if (data?.error === 'bot_method_unsupported') { unsupported = true; return null; }
			throw new Error(typeof data?.message === 'string' && data.message ? data.message : 'Could not check Cloud project access.');
		}
		if (disposed || mine !== latest) return null;
		return parseProjectAccess(bot, data);
	}

	async function check(fresh = false) {
		if (busy) return;
		checking = true; error = ''; copied = false;
		try { const next = await call(BASE, fresh ? { fresh: true } : {}); if (next) report = next; }
		catch (reason) { if (!disposed) error = reason instanceof Error ? reason.message : 'Could not check Cloud project access.'; }
		finally { checking = false; }
	}

	async function grant() {
		if (busy) return;
		granting = true; error = ''; copied = false;
		try { const next = await call(`${BASE}/grant`, {}); if (next) report = next; }
		catch (reason) { if (!disposed) error = reason instanceof Error ? reason.message : 'Could not grant access.'; }
		finally { granting = false; }
	}

	async function copyCommands() {
		if (!commandText) return;
		try { await navigator.clipboard.writeText(commandText); copied = true; }
		catch { error = 'Copy failed. Select the commands and copy them.'; }
	}

	onMount(() => void check());
	onDestroy(() => { disposed = true; });
</script>

{#if !unsupported}
	<div class="project-access" data-testid="project-access" aria-live="polite">
		{#if !report}
			{#if checking}<p class="quiet">Checking Cloud project access…</p>{/if}
		{:else if report.status === 'granted'}
			<p class="quiet ok">Cloud project access OK</p>
		{:else}
			{#if report.detail}<p class="detail">{report.detail}</p>{/if}
			{#if report.status === 'missing' && report.can_grant}
				<div class="actions">
					<button class="primary" type="button" disabled={busy} on:click={grant}>{granting ? 'Granting…' : 'Grant access'}</button>
				</div>
				{#if report.grant_account}<p class="quiet">Uses {report.grant_account} on the computer.</p>{/if}
			{:else if report.commands.length}
				<pre class="commands" aria-label="Commands to grant access"><code>{commandText}</code></pre>
				<div class="actions">
					<button type="button" on:click={copyCommands}>{copied ? 'Copied' : 'Copy commands'}</button>
					{#if report.status === 'missing' && report.console_url}
						<a href={report.console_url} target="_blank" rel="noopener noreferrer">Open Cloud console ↗</a>
					{/if}
				</div>
			{/if}
		{/if}
		{#if report || error}
			<button class="link" type="button" disabled={busy} on:click={() => check(true)}>{checking ? 'Checking…' : 'Check again'}</button>
		{/if}
		{#if error}<p class="error" role="alert">{error}</p>{/if}
	</div>
{/if}

<style>
	.project-access { display: flex; flex-direction: column; align-items: flex-start; gap: .4rem; font-size: .75rem; }
	p { margin: 0; line-height: 1.4; overflow-wrap: anywhere; }
	.quiet, .detail { color: var(--text-secondary); }
	.ok::before { content: '✓ '; color: var(--status-success, #00bb7f); }
	.error { color: var(--status-error, #dc4b4b); }
	.actions { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem; }
	.commands { box-sizing: border-box; width: 100%; max-height: 10rem; overflow: auto; margin: 0; padding: .45rem .55rem; border-radius: 8px; background: var(--bg-card); color: var(--text-primary); font: .7rem/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap; overflow-wrap: anywhere; user-select: all; }
	button { min-height: 32px; padding: .375rem .625rem; border: 1px solid var(--button-secondary-border, var(--border-soft)); border-radius: 8px; background: var(--button-secondary-bg); color: var(--button-secondary-color, var(--text-primary)); font: inherit; font-size: .8rem; font-weight: 600; cursor: pointer; }
	button.primary { border-color: transparent; background: var(--button-primary-bg, var(--accent-fill)); color: var(--button-primary-color, var(--text-on-accent)); }
	button.link { min-height: 0; padding: 0; border: 0; background: none; color: var(--accent-primary); font-size: .75rem; font-weight: 500; }
	button:focus-visible, a:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	button:disabled { opacity: .5; cursor: default; }
	a { color: var(--accent-primary); font-size: .8rem; font-weight: 600; }
	@media (pointer: coarse) { button:not(.link) { min-height: 40px; } }
</style>
