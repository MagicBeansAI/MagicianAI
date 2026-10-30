<script lang="ts">
	import { get } from 'svelte/store';
	import { onDestroy } from 'svelte';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';
	import { browserSignInViewerUrl, isBrowserSignInTicket } from './browserSignInViewer';
	export let correlationId: string | undefined = undefined;
	export let skill: string | undefined = undefined;
	export let expectedScope: { principal: string; workspace: string } | undefined = undefined;
	/** Primary button text; defaults to "Connect account ↗" / "Finish sign-in here ↗". */
	export let label: string | undefined = undefined;
	/** One declared MCP endpoint alias of a multi-endpoint skill (Service-accounts row). */
	export let endpointAlias: string | undefined = undefined;
	/** Called after a launch succeeds with the server's mode (`connected`, `direct`, `computer`, `computer_opened`). */
	export let onLaunched: ((mode: string) => void) | undefined = undefined;
	function skillRequest(): Record<string, string> {
		return endpointAlias ? { skill: skill as string, endpoint_alias: endpointAlias } : { skill: skill as string };
	}
	let busy = false;
	let error = '';
	let notice = '';
	let disposed = false;
	let reservedPopup: Window | null = null;
	// On the Magician computer itself (loopback page or the desktop app) "here"
	// and "on the computer" are the same place, so one button opens the
	// sign-in in the computer's own browser. Elsewhere (phone, another
	// machine) both choices stay: sign in on this device, or on the computer.
	/** `auto` decides from where this page runs; tests and embedders may pin it. */
	export let placement: 'auto' | 'engine-host' | 'remote' = 'auto';
	$: onEngineHost = placement === 'engine-host' || (placement === 'auto' && typeof window !== 'undefined' && (
		['localhost', '127.0.0.1', '[::1]', '::1'].includes(window.location.hostname) || '__TAURI_INTERNALS__' in window
	));
	onDestroy(() => { disposed = true; reservedPopup?.close(); reservedPopup = null; });
	async function onComputer() {
		if (busy || (!correlationId && !skill)) return;
		const scope = get(scopeIdentityStore);
		if (expectedScope && (scope.principal !== expectedScope.principal || scope.workspace !== expectedScope.workspace)) {
			error = 'Workspace changed. Reload the account list before signing in.'; return;
		}
		busy = true; error = ''; notice = '';
		try {
			const response = await timedFetch('/api/magician/v2/auth/browser-signin/launch', {
                method: 'POST', cache: 'no-store', redirect: 'error', timeoutMs: 45_000, headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify({ ...(skill ? skillRequest() : { correlation_id: correlationId }), open_on_computer: true })
			});
			const data = await response.json();
			if (!response.ok) throw new Error(data.message ?? 'Could not open sign-in on the computer.');
			const current = get(scopeIdentityStore);
			if (scope.principal !== current.principal || scope.workspace !== current.workspace) throw new Error('Workspace changed. Reopen sign-in in its workspace.');
			if (disposed) return;
			notice = data.mode === 'connected' ? 'Connected.' : 'Opened on the computer. Waiting for the service to confirm sign-in.';
			onLaunched?.(String(data.mode ?? ''));
		} catch (reason) { error = reason instanceof Error ? reason.message : 'Could not open sign-in.'; }
		finally { busy = false; }
	}

	async function open() {
		if (busy) return;
		const scope = get(scopeIdentityStore);
		if (expectedScope && (scope.principal !== expectedScope.principal || scope.workspace !== expectedScope.workspace)) {
			error = 'Workspace changed. Reload the account list before signing in.'; return;
		}
		// Reserve the window in the actual click, before awaiting the network.
		const popup = window.open('about:blank', '_blank');
		if (!popup) { error = 'Allow this sign-in window to open, then try again.'; return; }
		popup.opener = null;
		reservedPopup = popup;
		let issuedTicket: string | undefined;
		let responseUrl = window.location.href;
		busy = true;
		error = '';
		notice = '';
		try {
			const response = await timedFetch('/api/magician/v2/auth/browser-signin/launch', {
                method: 'POST', cache: 'no-store', redirect: 'error', timeoutMs: 45_000,
				headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify(skill ? skillRequest() : { correlation_id: correlationId })
			});
			const data = await response.json();
			if (!response.ok) throw new Error(data.message ?? 'Could not open sign-in.');
			responseUrl = response.url || window.location.href;
			if (data.mode === 'computer' && isBrowserSignInTicket(data.ticket)) issuedTicket = data.ticket;
			if (disposed) throw new Error('Sign-in was closed before it opened.');
			const current = get(scopeIdentityStore);
			if (scope.principal !== current.principal || scope.workspace !== current.workspace) {
				throw new Error('Workspace changed. Reopen sign-in in its workspace.');
			}
			if (popup.closed) throw new Error('The sign-in window was closed.');
			if (data.mode === 'connected') { popup.close(); notice = 'Connected.'; onLaunched?.('connected'); return; }
			if (data.mode === 'direct') {
				const url = new URL(data.authorization_url);
				if (url.protocol !== 'https:' || url.username || url.password) throw new Error('Invalid sign-in destination.');
				popup.location.replace(url.href);
			} else {
				popup.location.replace(browserSignInViewerUrl(data, response.url || window.location.href).href);
			}
			// The independent sign-in window can outlive this settings/card view.
			reservedPopup = null;
			onLaunched?.(String(data.mode ?? ''));
		} catch (reason) {
			popup.close();
			if (issuedTicket) {
				void timedFetch(new URL('/api/magician/v2/auth/browser-signin/close', responseUrl), {
                    method: 'POST', cache: 'no-store', redirect: 'error', timeoutMs: 5_000,
					headers: { 'Content-Type': 'application/json', 'X-Magician-Signin': issuedTicket }, body: '{}'
				}).catch(() => {});
			}
			if (!disposed) error = reason instanceof Error ? reason.message : 'Could not open sign-in.';
		} finally { reservedPopup = null; busy = false; }
	}
</script>

<div class="signin-controls" class:with-trailing={!!$$slots.trailing}>
<div class="signin-actions" role="group" aria-label="Sign-in actions">
{#if onEngineHost}
<button class="signin-button signin-primary" type="button" disabled={busy} on:click={onComputer}>
	{busy ? 'Opening sign-in…' : label ?? (skill ? 'Connect account ↗' : 'Finish sign-in ↗')}
</button>
{:else}
<button class="signin-button signin-primary" type="button" disabled={busy} on:click={open}>
	{busy ? 'Opening sign-in…' : label ?? (skill ? 'Connect on this device ↗' : 'Finish sign-in here ↗')}
</button>
{#if correlationId || skill}<button class="signin-button signin-secondary" type="button" disabled={busy} on:click={onComputer}>Open on computer</button>{/if}
{/if}
<slot name="trailing" />
</div>
{#if error}<p class="signin-error" role="alert">{error}</p>{/if}
{#if notice}<p role="status">{notice}</p>{/if}
</div>

<style>
	.signin-controls { width: 100%; min-width: 0; max-width: 100%; }
	.signin-actions { display: flex; flex-wrap: wrap; justify-content: flex-start; align-items: stretch; gap: .375rem; }
	.with-trailing .signin-actions { flex-wrap: nowrap; }
	.signin-button { display: inline-flex; align-items: center; justify-content: center; flex: 0 1 auto; min-width: 0; width: auto; max-width: 100%; min-height: 32px; padding: .375rem .5rem; border: 1px solid transparent; border-radius: 8px; font-family: inherit; font-size: .8rem; font-weight: 600; line-height: 1.25; text-align: center; white-space: normal; overflow-wrap: anywhere; cursor: pointer; }
	.signin-primary { background: var(--button-primary-bg, var(--accent-fill)); color: var(--button-primary-color, var(--text-on-accent)); }
	.signin-primary:hover:not(:disabled) { background: var(--accent-fill-hover, var(--button-primary-bg)); }
	.signin-secondary { background: var(--button-secondary-bg); color: var(--button-secondary-color, var(--text-primary)); border-color: var(--button-secondary-border, var(--border-soft)); }
	.signin-secondary:hover:not(:disabled) { background: var(--button-secondary-hover-bg); border-color: var(--accent-primary); }
	.signin-button:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	.signin-button:disabled { opacity: .5; cursor: wait; }
	.signin-controls p { margin: .4rem 0 0; font-size: .8rem; }
	.signin-error { color: var(--status-error, #dc4b4b); font-size: .85rem; }
	@media (pointer: coarse) { .signin-button { min-height: 40px; } }
</style>
