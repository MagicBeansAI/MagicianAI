<script lang="ts">
	import { onMount } from 'svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		HARNESS_AUTH_ENGINES,
		fetchHarnessAuth,
		saveHarnessAuth,
		signInLabel,
		type HarnessAuthEngine,
		type HarnessAuthMode,
		type HarnessAuthState
	} from '$lib/settings/harnessAuth';

	const ENGINE_LABELS: Record<HarnessAuthEngine, string> = {
		claude_code: 'Claude Code',
		codex: 'Codex',
		grok: 'Grok',
		agy: 'Antigravity',
		pi: 'Pi'
	};
	const MODE_LABELS: Record<HarnessAuthMode, string> = {
		login_first: 'Own sign-in first',
		login_only: 'Own sign-in only',
		magician: 'Magician’s keys'
	};
	const MODES: HarnessAuthMode[] = ['login_first', 'login_only', 'magician'];

	let authState = $state<HarnessAuthState | null>(null);
	let busy = $state(false);
	let error = $state<string | null>(null);

	onMount(() => {
		void refresh();
	});

	async function refresh(): Promise<void> {
		try {
			authState = await fetchHarnessAuth();
			error = null;
		} catch (caught) {
			error = caught instanceof Error ? caught.message : 'Harness credentials are unavailable.';
		}
	}

	async function save(update: Parameters<typeof saveHarnessAuth>[0]): Promise<void> {
		busy = true;
		try {
			authState = await saveHarnessAuth(update);
			showSuccess('Harness credentials saved');
		} catch (caught) {
			showError(caught instanceof Error ? caught.message : 'Harness credentials could not be saved.');
		} finally {
			busy = false;
		}
	}

	function onDefaultChange(event: Event): void {
		void save({ mode: (event.currentTarget as HTMLSelectElement).value as HarnessAuthMode });
	}

	function onEngineChange(engine: HarnessAuthEngine, event: Event): void {
		const value = (event.currentTarget as HTMLSelectElement).value;
		void save({ engines: { [engine]: value === 'default' ? null : (value as HarnessAuthMode) } });
	}

	function usesLabel(engine: HarnessAuthEngine): string {
		const row = authState?.status[engine];
		if (!row) return '—';
		if (row.uses_magician_credentials) return 'Magician’s key';
		if (row.sign_in === 'signed_in') return 'Its own sign-in';
		if (row.key_fallback === 'ask' && row.magician_key_available) {
			return 'Nothing until you approve Magician’s key';
		}
		return 'Nothing (sign in first)';
	}

	/** The CLI's own words about its sign-in or last failure, when it said any. */
	function detailFor(engine: HarnessAuthEngine): string | null {
		const row = authState?.status[engine];
		if (!row) return null;
		if (row.sign_in !== 'signed_in' && row.sign_in_detail) return row.sign_in_detail;
		const failure = row.last_failure;
		if (failure) return failure.remedy ? `${failure.reason} ${failure.remedy}` : failure.reason;
		return null;
	}
</script>

<section class="auth-card" aria-labelledby="harness-auth-title">
	<div class="auth-header">
		<p class="auth-overline">Harness credentials</p>
		<h2 id="harness-auth-title">Whose account the coding CLIs use</h2>
		<p>
			Claude Code, Codex, Grok, Antigravity and Pi can run on their own sign-in (your subscription) or on Magician’s
			provider keys (billed to the API). This applies to chat, agent runs, VibeDev and background work. A CLI not on
			its own sign-in also hands background work back to Magician’s own models.
		</p>
	</div>
	{#if error}
		<p class="auth-error" role="alert">{error}</p>
	{:else if authState}
		<label class="auth-default">
			<span>Default</span>
			<select value={authState.mode} disabled={busy} onchange={onDefaultChange}>
				{#each MODES as mode (mode)}
					<option value={mode}>{MODE_LABELS[mode]}</option>
				{/each}
			</select>
		</label>
		<p class="auth-hint">
			Own sign-in first never switches to Magician’s key on its own: when a CLI’s sign-in is missing, expired or out
			of credits, the turn fails with the CLI’s reason and you are asked whether to use the key (this turn, always for
			that CLI, or no). API charges apply only after you approve. Own sign-in only never passes a key. Magician’s keys
			always do.
		</p>
		<div class="auth-rows" role="table" aria-label="Harness credentials per CLI">
			{#each HARNESS_AUTH_ENGINES as engine (engine)}
				<div class="auth-row" role="row">
					<span class="auth-engine" role="cell">{ENGINE_LABELS[engine]}</span>
					<span
						class="auth-signin"
						role="cell"
						class:auth-ok={authState.status[engine]?.sign_in === 'signed_in'}
						class:auth-expired={authState.status[engine]?.sign_in === 'expired'}
					>
						{signInLabel(authState.status[engine])}
					</span>
					<span class="auth-uses" role="cell">Uses: {usesLabel(engine)}</span>
					<select
						role="cell"
						aria-label={`${ENGINE_LABELS[engine]} credentials`}
						value={authState.engines[engine] ?? 'default'}
						disabled={busy}
						onchange={(event) => onEngineChange(engine, event)}
					>
						<option value="default">Follow default</option>
						{#each MODES as mode (mode)}
							<option value={mode}>{MODE_LABELS[mode]}</option>
						{/each}
					</select>
					{#if detailFor(engine)}
						<p class="auth-detail" role="note">{detailFor(engine)}</p>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<p>Loading harness credentials…</p>
	{/if}
</section>

<style>
	.auth-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1rem;
	}
	.auth-overline,
	h2,
	p {
		margin: 0;
	}
	.auth-overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}
	h2 {
		font-size: 1.05rem;
		margin: 0.15rem 0 0.35rem;
	}
	p {
		color: var(--text-secondary);
		line-height: 1.45;
	}
	.auth-default {
		align-items: center;
		display: flex;
		gap: 0.6rem;
		font-weight: 600;
	}
	select {
		background: var(--bg-input, var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		padding: 0.35rem 0.5rem;
	}
	.auth-hint {
		font-size: 0.875rem;
	}
	.auth-rows {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}
	.auth-row {
		align-items: center;
		border-top: 1px solid var(--border-soft);
		display: grid;
		gap: 0.5rem;
		grid-template-columns: minmax(7rem, 1fr) minmax(6rem, auto) minmax(10rem, 2fr) auto;
		padding-top: 0.4rem;
	}
	.auth-engine {
		font-weight: 600;
	}
	.auth-signin {
		color: var(--warning);
		font-size: 0.875rem;
	}
	.auth-signin.auth-ok {
		color: var(--success);
	}
	.auth-signin.auth-expired {
		color: var(--danger);
	}
	.auth-detail {
		font-size: 0.8rem;
		grid-column: 1 / -1;
		overflow-wrap: anywhere;
	}
	.auth-uses {
		color: var(--text-secondary);
		font-size: 0.875rem;
	}
	.auth-error {
		color: var(--danger);
		font-size: 0.875rem;
	}
	@media (max-width: 640px) {
		.auth-row {
			grid-template-columns: 1fr auto;
		}
		.auth-uses {
			grid-column: 1 / -1;
		}
	}
</style>
