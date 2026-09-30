<script lang="ts">
	/**
	 * Environment panel — the runtime's `.env` / `.env.development` value
	 * layer, managed from the UI instead of setup scripts.
	 *
	 * Values are WRITE-ONLY: the API returns key names with set/unset status
	 * and never echoes values, so edits replace blind and deletes are the only
	 * way out. Lockout-risk keys (bearer/admin/Cloudflare Access) demand an
	 * explicit acknowledge on the retry after the server refuses the first
	 * write.
	 *
	 * Data: GET/PUT /api/magician/v2/runtime/env (setup token for writes).
	 */
	import { onMount } from 'svelte';
	import { timedFetch } from '$lib/shared/fetch';

	const SETUP_TOKEN_STORAGE_KEY = 'magician:vault:setup-token';

	interface EnvKeyStatus {
		set: boolean;
		process_env_set: boolean;
	}
	interface EnvFileStatus {
		path: string;
		keys: Record<string, EnvKeyStatus>;
	}
	interface CatalogKey {
		key: string;
		section: string;
		secret: boolean;
	}
	interface RuntimeEnvStatus {
		active_mode: string;
		env: EnvFileStatus;
		env_development: EnvFileStatus;
		catalog: CatalogKey[];
	}

	type EnvFile = '.env' | '.env.development';

	let status: RuntimeEnvStatus | null = null;
	let loading = true;
	let error: string | null = null;
	let file: EnvFile = '.env';
	// Auto-select the active-mode file ONCE; after the user picks a file it
	// stays put across reloads (load() runs after every write).
	let fileTouched = false;
	// Draft values per key — never populated from the server (write-only).
	let drafts: Record<string, string> = {};
	let inflight: Record<string, boolean> = {};
	let rowMessages: Record<string, { kind: 'ok' | 'err'; text: string }> = {};
	let search = '';

	$: sections = buildSections(status, file, search);
	$: extraKeys = buildExtraKeys(status, file, search);

	function statusFor(key: string): EnvKeyStatus | null {
		const f = file === '.env' ? status?.env : status?.env_development;
		return f?.keys[key] ?? null;
	}

	function buildSections(
		s: RuntimeEnvStatus | null,
		f: EnvFile,
		filter: string
	): Array<{ section: string; keys: CatalogKey[] }> {
		if (!s) return [];
		const needle = filter.trim().toLowerCase();
		const bySection = new Map<string, CatalogKey[]>();
		for (const entry of s.catalog) {
			if (needle && !entry.key.toLowerCase().includes(needle)) continue;
			const list = bySection.get(entry.section) ?? [];
			list.push(entry);
			bySection.set(entry.section, list);
		}
		return [...bySection.entries()].map(([section, keys]) => ({ section, keys }));
	}

	function buildExtraKeys(
		s: RuntimeEnvStatus | null,
		f: EnvFile,
		filter: string
	): string[] {
		if (!s) return [];
		const fstatus = f === '.env' ? s.env : s.env_development;
		const known = new Set(s.catalog.map((c) => c.key));
		const needle = filter.trim().toLowerCase();
		return Object.keys(fstatus.keys)
			.filter((k) => !known.has(k))
			.filter((k) => !needle || k.toLowerCase().includes(needle))
			.sort();
	}

	function readStoredToken(): string {
		try {
			return sessionStorage.getItem(SETUP_TOKEN_STORAGE_KEY) ?? '';
		} catch {
			return '';
		}
	}

	async function load() {
		loading = true;
		error = null;
		try {
			const res = await timedFetch('/api/magician/v2/runtime/env');
			if (!res.ok) throw new Error(`server returned ${res.status}`);
			status = (await res.json()) as RuntimeEnvStatus;
			if (!fileTouched && status.active_mode === 'development' && file === '.env') {
				file = '.env.development';
			}
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function write(
		key: string,
		value: string | null,
		acknowledge = false
	): Promise<void> {
		if (inflight[key]) return;
		const token = readStoredToken();
		if (!token) {
			rowMessages = {
				...rowMessages,
				[key]: { kind: 'err', text: 'Setup token required (find it on /vault).' }
			};
			return;
		}
		inflight = { ...inflight, [key]: true };
		try {
			const res = await timedFetch('/api/magician/v2/runtime/env', {
				method: 'PUT',
				headers: {
					'Content-Type': 'application/json',
					'X-Magician-Setup-Token': token
				},
				body: JSON.stringify({
					file,
					updates: { [key]: value },
					acknowledge_lockout_risk: acknowledge
				})
			});
			const data = await res.json().catch(() => null);
			if (res.status === 422 && data?.message?.includes('acknowledge_lockout_risk')) {
				rowMessages = {
					...rowMessages,
					[key]: { kind: 'err', text: `${data.message}` }
				};
				const confirmed = window.confirm(
					`${data.message}\n\nRetry with the lockout risk acknowledged?`
				);
				if (confirmed) {
					inflight = { ...inflight, [key]: false };
					await write(key, value, true);
				}
				return;
			}
			if (!res.ok) {
				rowMessages = {
					...rowMessages,
					[key]: {
						kind: 'err',
						text:
							(data && (data.message || data.reason || data.error)) ||
							`server returned ${res.status}`
					}
				};
				return;
			}
			rowMessages = {
				...rowMessages,
				[key]: {
					kind: 'ok',
					text: value === null ? `${key} deleted from ${file}` : `${key} saved to ${file}`
				}
			};
			drafts = { ...drafts, [key]: '' };
			await load();
		} catch (e) {
			rowMessages = {
				...rowMessages,
				[key]: { kind: 'err', text: e instanceof Error ? e.message : String(e) }
			};
		} finally {
			inflight = { ...inflight, [key]: false };
		}
	}

	function setKey(key: string) {
		const value = (drafts[key] ?? '').trim();
		if (!value) return;
		void write(key, value);
	}

	function deleteKey(key: string) {
		if (!window.confirm(`Delete ${key} from ${file}?`)) return;
		void write(key, null);
	}
</script>

<div class="env-panel">
	<div class="env-toolbar">
		<div class="env-file-switch" role="tablist" aria-label="Env file">
			<button
				type="button"
				class:active={file === '.env'}
				role="tab"
				aria-selected={file === '.env'}
				on:click={() => { file = '.env'; fileTouched = true; }}
			>
				.env
			</button>
			<button
				type="button"
				class:active={file === '.env.development'}
				role="tab"
				aria-selected={file === '.env.development'}
				on:click={() => { file = '.env.development'; fileTouched = true; }}
			>
				.env.development
			</button>
		</div>
		{#if status}
			<span
				class="env-mode"
				title="Files load at boot (.env.development first, dev-wins); real environment variables outrank both files"
			>
				active mode: {status.active_mode}
			</span>
		{/if}
		<input type="search" placeholder="Filter keys…" bind:value={search} class="env-search" />
	</div>

	<p class="env-note">
		Values are write-only — the server never shows them. Set replaces blind; delete removes the
		line. Keys carried by the live process environment outrank file edits until restart.
	</p>

	{#if error}
		<div class="env-error">Error loading environment: {error}</div>
	{:else if loading}
		<div class="env-empty">Loading…</div>
	{:else if status}
		{#each sections as section (section.section)}
			<section class="env-section">
				<h3>{section.section}</h3>
				{#each section.keys as entry (entry.key)}
					{@const st = statusFor(entry.key)}
					<div class="env-row" class:env-row--set={st?.set}>
						<div class="env-key-line">
							<code class="env-key">{entry.key}</code>
							{#if st?.set}
								<span class="env-flag env-flag--set">set</span>
							{:else}
								<span class="env-flag">unset</span>
							{/if}
							{#if st?.process_env_set}
								<span class="env-flag env-flag--proc" title="The live process env carries this key; file edits apply after restart">
									env override
								</span>
							{/if}
							{#if entry.secret}
								<span class="env-flag env-flag--secret">secret</span>
							{/if}
						</div>
						<div class="env-edit-line">
							<input
								type="password"
								placeholder={st?.set ? 'set — type to replace' : 'not set'}
								bind:value={drafts[entry.key]}
								on:keydown={(e) => e.key === 'Enter' && setKey(entry.key)}
								autocomplete="off"
							/>
							<button type="button" on:click={() => setKey(entry.key)} disabled={inflight[entry.key]}>
								{inflight[entry.key] ? '…' : 'Set'}
							</button>
							{#if st?.set}
								<button
									type="button"
									class="env-delete"
									on:click={() => deleteKey(entry.key)}
									disabled={inflight[entry.key]}
								>
									Delete
								</button>
							{/if}
						</div>
						{#if rowMessages[entry.key]}
							<div class="env-msg env-msg--{rowMessages[entry.key].kind}">
								{rowMessages[entry.key].text}
							</div>
						{/if}
					</div>
				{/each}
			</section>
		{/each}

		{#if extraKeys.length > 0}
			<section class="env-section">
				<h3>Extra keys in {file} (outside the template catalog)</h3>
				{#each extraKeys as key (key)}
					{@const st = statusFor(key)}
					<div class="env-row" class:env-row--set={st?.set}>
						<div class="env-key-line">
							<code class="env-key">{key}</code>
							<span class="env-flag env-flag--set">set</span>
							{#if st?.process_env_set}
								<span class="env-flag env-flag--proc">env override</span>
							{/if}
						</div>
						<div class="env-edit-line">
							<input
								type="password"
								placeholder="set — type to replace"
								bind:value={drafts[key]}
								on:keydown={(e) => e.key === 'Enter' && setKey(key)}
								autocomplete="off"
							/>
							<button type="button" on:click={() => setKey(key)} disabled={inflight[key]}>
								{inflight[key] ? '…' : 'Set'}
							</button>
							<button
								type="button"
								class="env-delete"
								on:click={() => deleteKey(key)}
								disabled={inflight[key]}
							>
								Delete
							</button>
						</div>
						{#if rowMessages[key]}
							<div class="env-msg env-msg--{rowMessages[key].kind}">
								{rowMessages[key].text}
							</div>
						{/if}
					</div>
				{/each}
			</section>
		{/if}
	{/if}
</div>

<style>
	.env-panel {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.env-toolbar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.env-file-switch {
		display: inline-flex;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		overflow: hidden;
	}

	.env-file-switch button {
		padding: 0.3rem 0.8rem;
		font-size: 0.8rem;
		font-family: var(--font-mono);
		border: none;
		background: var(--bg-card);
		color: var(--text-primary);
		cursor: pointer;
	}

	.env-file-switch button.active {
		background: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.env-mode {
		font-size: 0.75rem;
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	.env-search {
		margin-left: auto;
		max-width: 14rem;
		padding: 0.35rem 0.6rem;
		font-size: 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-card);
		color: var(--text-primary);
	}

	.env-note {
		margin: 0;
		font-size: 0.78rem;
		color: var(--text-muted);
	}

	.env-error,
	.env-empty {
		padding: 1.25rem;
		text-align: center;
		color: var(--text-muted);
	}

	.env-error {
		color: var(--text-danger, #c33);
	}

	.env-section {
		border: 1px solid var(--border-soft);
		border-radius: 0.5rem;
		background: var(--bg-card);
		padding: 0.6rem 0.9rem;
	}

	.env-section h3 {
		margin: 0 0 0.4rem;
		font-size: 0.8rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}

	.env-row {
		padding: 0.35rem 0;
		border-top: 1px solid var(--border-soft);
	}

	.env-row:first-of-type {
		border-top: none;
	}

	.env-key-line {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.env-key {
		font-size: 0.82rem;
		font-weight: 600;
	}

	.env-flag {
		font-size: 0.68rem;
		padding: 0.05rem 0.35rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.25rem;
		color: var(--text-muted);
	}

	.env-flag--set {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.env-flag--proc {
		border-color: var(--text-warning, #b8860b);
		color: var(--text-warning, #b8860b);
	}

	.env-flag--secret {
		border-style: dashed;
	}

	.env-edit-line {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		margin-top: 0.25rem;
	}

	.env-edit-line input {
		flex: 1;
		max-width: 26rem;
		padding: 0.3rem 0.55rem;
		font-size: 0.8rem;
		font-family: var(--font-mono);
		border: 1px solid var(--border-soft);
		border-radius: 0.35rem;
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
	}

	.env-edit-line button {
		padding: 0.25rem 0.7rem;
		font-size: 0.78rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.35rem;
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		cursor: pointer;
	}

	.env-edit-line button:hover:not(:disabled) {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.env-edit-line button.env-delete:hover:not(:disabled) {
		background: var(--text-danger, #c33);
		border-color: var(--text-danger, #c33);
	}

	.env-edit-line button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.env-msg {
		margin-top: 0.25rem;
		padding: 0.25rem 0.45rem;
		border-radius: 0.3rem;
		font-size: 0.75rem;
	}

	.env-msg--ok {
		background: var(--surface-success, rgba(40, 160, 80, 0.1));
		color: var(--text-success, #2a8a4a);
	}

	.env-msg--err {
		background: var(--surface-danger, rgba(204, 51, 51, 0.1));
		color: var(--text-danger, #c33);
	}
</style>
