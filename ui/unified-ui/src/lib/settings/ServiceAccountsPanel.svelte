<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';
	import BrowserSignInButton from '$lib/hitl/BrowserSignInButton.svelte';
	import ManagedBotConnect from './ManagedBotConnect.svelte';
	import GoogleProjectAccess from './GoogleProjectAccess.svelte';
	import { botMethods } from './managedBot';
	type AccountStatus = 'connected' | 'needs_sign_in' | 'authenticating' | 'unknown' | 'unavailable';
	/** Older engines omit `kind`, `bot`, `profile`, `methods` (and `status`); those rows are browser sign-ins. */
	type Account = {
		name: string; label: string; status?: AccountStatus; detail?: string; endpoint_alias?: string;
		kind?: 'governed_oauth' | 'browser_cli' | 'managed_bot' | string; bot?: string; profile?: string; methods?: unknown;
		/** A Google account whose Cloud project roles can be checked (`bot/project-access`). */
		project_access?: boolean;
	};
	const STATUS_TEXT: Record<AccountStatus, string> = {
		connected: 'Connected',
		needs_sign_in: 'Needs sign-in',
		authenticating: 'Signing in…',
		unknown: 'Unknown',
		unavailable: 'Unavailable'
	};
	export let expandedBySearch = false;
	let expanded = false;
	$: if (expandedBySearch) expanded = true;
	$: connectedCount = accounts.filter((account) => statusOf(account) === 'connected').length;
	$: needsSignInCount = accounts.filter((account) => statusOf(account) === 'needs_sign_in').length;
	const scope = get(scopeIdentityStore);
	let accounts: Account[] = [];
	let loading = true;
	let error = '';
	let disposed = false;
	let inFlight = false;
	let followUp: ReturnType<typeof setTimeout> | undefined;
	function statusOf(account: Account): AccountStatus {
		return account.status && account.status in STATUS_TEXT ? account.status : 'unknown';
	}
	function rowKey(account: Account): string { return `${account.name}:${account.bot ?? account.endpoint_alias ?? ''}`; }
	function managedBot(account: Account): string | undefined {
		return account.kind === 'managed_bot' && typeof account.bot === 'string' && account.bot ? account.bot : undefined;
	}
	/** The managed-bot row whose connect panel is open. */
	let connecting = '';
	function onBotDone(connected: boolean) {
		connecting = '';
		void load();
		if (connected) return;
		// A cancelled or failed flow can still settle on the engine; check once more shortly.
		if (followUp) clearTimeout(followUp);
		followUp = setTimeout(() => void load(), 5_000);
	}
	async function load() {
		if (disposed || inFlight) return;
		inFlight = true;
		try {
			const response = await timedFetch('/api/magician/v2/auth/browser-signin/accounts', {
				method: 'POST', cache: 'no-store', redirect: 'error', headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }), body: '{}'
			});
			if (!response.ok) throw new Error('Could not load service accounts. Check your connection and sign in again if needed.');
			const data = await response.json();
			if (!disposed) { accounts = Array.isArray(data.accounts) ? data.accounts : []; error = ''; }
		} catch (reason) {
			// Keep the last good list on a background refresh; only an empty panel shows the error.
			if (!disposed && accounts.length === 0) error = reason instanceof Error ? reason.message : 'Could not load service accounts.';
		} finally { inFlight = false; if (!disposed) loading = false; }
	}
	// Sign-in finishes in another window or on the computer: re-check when the owner comes back.
	function onVisible() { if (document.visibilityState === 'visible') void load(); }
	function onLaunched() {
		void load();
		if (followUp) clearTimeout(followUp);
		followUp = setTimeout(() => void load(), 15_000);
	}
	onMount(() => {
		void load();
		window.addEventListener('focus', onVisible);
		document.addEventListener('visibilitychange', onVisible);
	});
	onDestroy(() => {
		disposed = true;
		if (followUp) clearTimeout(followUp);
		if (typeof window !== 'undefined') window.removeEventListener('focus', onVisible);
		if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisible);
	});
</script>

<section class="service-accounts-panel" aria-labelledby="service-accounts-title">
	<details bind:open={expanded}>
		<summary aria-label="Service accounts" aria-describedby="service-accounts-summary">
			<h2 id="service-accounts-title">Service accounts {#if !loading}<span class="account-count" aria-hidden="true">{accounts.length}</span>{/if}</h2>
			<span id="service-accounts-summary" class="summary-status">
				{#if loading}Loading accounts…
				{:else if error}Could not load accounts
				{:else if connectedCount || needsSignInCount}{connectedCount} connected{#if needsSignInCount} · {needsSignInCount} need sign-in{/if}
				{:else}Manage sign-ins{/if}
			</span>
			<span class="chevron" aria-hidden="true">›</span>
		</summary>
		<div class="panel-body">
			<p class="panel-description">Connect an installed service. Credentials stay on your Magican computer in this workspace.</p>
			{#if loading}<p role="status">Loading accounts…</p>
			{:else if error}<p role="alert">{error}</p>
			{:else if accounts.length === 0}<p>No supported service accounts found. Install a service skill from <a href="/skills">Skills</a> first.</p>
			{:else}
				<div class="account-grid">
					{#each accounts as account (rowKey(account))}
						{@const status = statusOf(account)}
						{@const bot = managedBot(account)}
						<div class="service-account" data-testid="service-account">
							<div class="account-label">
								<div class="account-title"><strong>{account.name}</strong><span class="status-chip status-{status}">{STATUS_TEXT[status]}</span></div>
								<p>{account.label}</p>
								{#if account.detail}<p class="account-detail">{account.detail}</p>{/if}
							</div>
							<!-- Checked lazily, once each time the panel opens. -->
							{#if expanded && bot && status === 'connected' && account.project_access === true && connecting !== rowKey(account)}
								<GoogleProjectAccess {bot} expectedScope={scope} />
							{/if}
							{#if status === 'unavailable'}
								<!-- Missing binary or environment: nothing to connect until it is installed. -->
							{:else if bot}
								{@const methods = botMethods(account)}
								{#if connecting === rowKey(account)}
									<ManagedBotConnect {bot} {methods} expectedScope={scope} onDone={onBotDone} />
								{:else if methods.length}
									<button class="bot-connect-button" type="button" disabled={connecting !== ''} on:click={() => (connecting = rowKey(account))}>{status === 'connected' ? 'Reconnect' : 'Connect'}</button>
								{/if}
							{:else}
								<BrowserSignInButton skill={account.name} endpointAlias={account.endpoint_alias} expectedScope={scope} label={status === 'connected' ? 'Reconnect ↗' : 'Connect ↗'} {onLaunched} />
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</details>
</section>

<style>
	.service-accounts-panel { background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: 8px; }
	summary { display: flex; align-items: center; flex-wrap: wrap; gap: .5rem .75rem; padding: 1rem; list-style: none; cursor: pointer; }
	summary::-webkit-details-marker { display: none; }
	summary:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; border-radius: 8px; }
	summary h2 { display: flex; align-items: center; flex: 1; gap: .5rem; margin: 0; color: var(--text-primary); font-size: 1rem; line-height: 1.4; }
	.account-count { padding: .1rem .45rem; border-radius: 6px; background: var(--bg-secondary); color: var(--text-secondary); font-size: .75rem; font-weight: 500; }
	.summary-status { color: var(--text-secondary); font-size: .75rem; }
	.chevron { color: var(--text-secondary); font-size: 1.5rem; line-height: 1; transition: transform .15s; }
	details[open] .chevron { transform: rotate(90deg); }
	.panel-body { padding: 0 1rem 1rem; }
	.panel-description { margin: 0 0 .75rem; color: var(--text-secondary); font-size: .8rem; }
	.account-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 14rem), 1fr)); gap: .75rem; }
	.service-account { display: flex; flex-direction: column; align-items: stretch; justify-content: space-between; gap: .75rem; min-width: 0; padding: .75rem; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-secondary); }
	.account-label { min-width: 0; overflow-wrap: anywhere; }
	.account-label p { color: var(--text-secondary); font-size: .8rem; line-height: 1.4; margin: .35rem 0 0; }
	.account-label .account-detail { font-size: .75rem; }
	.account-title { display: flex; align-items: center; gap: .4rem; flex-wrap: wrap; font-size: .85rem; }
	.status-chip { display: inline-flex; align-items: center; padding: .1rem .4rem; border-radius: 6px; font-size: .7rem; font-weight: 600; line-height: 1.4; border: 1px solid var(--border-soft); color: var(--text-secondary); }
	.status-connected { background: var(--status-success-soft, rgba(0,187,127,.14)); border-color: var(--status-success,#00bb7f); color: var(--text-primary); }
	.status-needs_sign_in { background: var(--color-warning-soft,rgba(255,230,109,.22)); border-color: var(--color-warning,#ffe66d); color: var(--text-primary); }
	.status-authenticating { border-color: var(--accent-primary); color: var(--text-primary); }
	.status-unavailable { background: var(--bg-card); color: var(--text-secondary); opacity: .8; }
	.bot-connect-button { align-self: flex-start; min-height: 32px; padding: .375rem .625rem; border: 1px solid transparent; border-radius: 8px; background: var(--button-primary-bg, var(--accent-fill)); color: var(--button-primary-color, var(--text-on-accent)); font: inherit; font-size: .8rem; font-weight: 600; cursor: pointer; }
	.bot-connect-button:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	.bot-connect-button:disabled { opacity: .5; cursor: default; }
	@media (pointer: coarse) { .bot-connect-button { min-height: 40px; } }
	@media (max-width: 600px) {
		summary { padding: .75rem; }
		summary h2 { flex-basis: calc(100% - 2rem); }
		.summary-status { flex: 1; order: 3; }
		.panel-body { padding: 0 .75rem .75rem; }
		.account-grid { grid-template-columns: repeat(auto-fill, minmax(min(100%, 8rem), 1fr)); gap: .5rem; }
		.service-account { padding: .625rem; gap: .5rem; }
	}
	@media (prefers-reduced-motion: reduce) { .chevron { transition: none; } }
</style>
