<script lang="ts">
	/**
	 * Per-skill env editor — the `config/.env` inside one scope's copy of a
	 * skill (the same file skill dispatch reads at run time). Key names come
	 * from the manifest's requires.env plus the skill's config/.env.example
	 * template; values are write-only, never echoed.
	 *
	 * Data: GET/POST /api/magician/v2/skills/catalog/{name}/env (setup token
	 * for writes).
	 */
	import { onMount } from 'svelte';
	import { timedFetch } from '$lib/shared/fetch';

	const SETUP_TOKEN_STORAGE_KEY = 'magician:vault:setup-token';

	export let skill: string;
	export let scope: string;

	interface SkillEnvKey {
		key: string;
		source: 'required' | 'template' | 'extra';
		set: boolean;
	}
	interface SkillEnvStatus {
		skill: string;
		scope: string;
		keys: SkillEnvKey[];
	}

	let status: SkillEnvStatus | null = null;
	let loading = true;
	let error: string | null = null;
	let drafts: Record<string, string> = {};
	let inflight: Record<string, boolean> = {};
	let messages: Record<string, { kind: 'ok' | 'err'; text: string }> = {};

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
			const headers: Record<string, string> = {};
			const token = readStoredToken();
			if (token) headers['X-Magician-Setup-Token'] = token;
			const res = await timedFetch(
				`/api/magician/v2/skills/catalog/${encodeURIComponent(skill)}/env?scope=${encodeURIComponent(scope)}`,
				{ headers }
			);
			if (res.status === 401) {
				throw new Error('Setup token required (find it on /vault).');
			}
			if (!res.ok) throw new Error(`server returned ${res.status}`);
			status = (await res.json()) as SkillEnvStatus;
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	}

	onMount(load);
	// The scope prop changes with the active workspace — the editor must not
	// keep showing the previous scope's key status.
	let loadedScope = scope;
	$: if (scope !== loadedScope) {
		loadedScope = scope;
		drafts = {};
		messages = {};
		void load();
	}

	async function write(key: string, value: string | null) {
		if (inflight[key]) return;
		const token = readStoredToken();
		if (!token) {
			messages = { ...messages, [key]: { kind: 'err', text: 'Setup token required (/vault).' } };
			return;
		}
		inflight = { ...inflight, [key]: true };
		try {
			const res = await timedFetch(
				`/api/magician/v2/skills/catalog/${encodeURIComponent(skill)}/env`,
				{
					method: 'POST',
					headers: {
						'Content-Type': 'application/json',
						'X-Magician-Setup-Token': token
					},
					body: JSON.stringify({ workspaces: [scope], updates: { [key]: value } })
				}
			);
			const data = await res.json().catch(() => null);
			if (!res.ok) {
				messages = {
					...messages,
					[key]: {
						kind: 'err',
						text:
							(data && (data.message || data.reason || data.error)) ||
							`server returned ${res.status}`
					}
				};
				return;
			}
			messages = {
				...messages,
				[key]: {
					kind: 'ok',
					text: value === null ? `${key} deleted` : `${key} saved to this skill's config/.env`
				}
			};
			drafts = { ...drafts, [key]: '' };
			await load();
		} catch (e) {
			messages = { ...messages, [key]: { kind: 'err', text: e instanceof Error ? e.message : String(e) } };
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
		if (!window.confirm(`Delete ${key} from ${skill}'s config/.env?`)) return;
		void write(key, null);
	}
</script>

<div class="skl-env">
	<div class="skl-env-head">
		Env for <code>{skill}</code> in <code>{scope}</code> — values are write-only, saved to the
		skill's <code>config/.env</code>.
	</div>
	{#if error}
		<div class="skl-env-msg skl-env-msg--err">Error: {error}</div>
	{:else if loading}
		<div class="skl-env-empty">Loading…</div>
	{:else if status}
		{#if status.keys.length === 0}
			<div class="skl-env-empty">No env keys declared for this skill.</div>
		{/if}
		{#each status.keys as entry (entry.key)}
			<div class="skl-env-row">
				<div class="skl-env-keyline">
					<code>{entry.key}</code>
					<span class="skl-env-flag" class:skl-env-flag--set={entry.set}>
						{entry.set ? 'set' : 'unset'}
					</span>
					{#if entry.source !== 'extra'}
						<span class="skl-env-flag">{entry.source}</span>
					{/if}
				</div>
				<div class="skl-env-editline">
					<input
						type="password"
						placeholder={entry.set ? 'set — type to replace' : 'not set'}
						bind:value={drafts[entry.key]}
						on:keydown={(e) => e.key === 'Enter' && setKey(entry.key)}
						autocomplete="off"
					/>
					<button type="button" on:click={() => setKey(entry.key)} disabled={inflight[entry.key]}>
						{inflight[entry.key] ? '…' : 'Set'}
					</button>
					{#if entry.set}
						<button
							type="button"
							class="skl-env-del"
							on:click={() => deleteKey(entry.key)}
							disabled={inflight[entry.key]}
						>
							Delete
						</button>
					{/if}
				</div>
				{#if messages[entry.key]}
					<div class="skl-env-msg skl-env-msg--{messages[entry.key].kind}">
						{messages[entry.key].text}
					</div>
				{/if}
			</div>
		{/each}
	{/if}
</div>

<style>
	.skl-env {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		margin-top: 0.5rem;
		padding: 0.6rem 0.8rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-soft, var(--bg-card));
	}

	.skl-env-head {
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.skl-env-empty {
		font-size: 0.8rem;
		color: var(--text-muted);
		padding: 0.4rem 0;
	}

	.skl-env-row {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0.25rem 0;
		border-top: 1px solid var(--border-soft);
	}

	.skl-env-keyline {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.skl-env-keyline code {
		font-size: 0.78rem;
		font-weight: 600;
	}

	.skl-env-flag {
		font-size: 0.66rem;
		padding: 0.03rem 0.3rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.25rem;
		color: var(--text-muted);
	}

	.skl-env-flag--set {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.skl-env-editline {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.skl-env-editline input {
		flex: 1;
		max-width: 22rem;
		padding: 0.25rem 0.5rem;
		font-size: 0.78rem;
		font-family: var(--font-mono);
		border: 1px solid var(--border-soft);
		border-radius: 0.3rem;
		background: var(--bg-card);
		color: var(--text-primary);
	}

	.skl-env-editline button {
		padding: 0.2rem 0.6rem;
		font-size: 0.74rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.3rem;
		background: var(--bg-card);
		color: var(--text-primary);
		cursor: pointer;
	}

	.skl-env-editline button:hover:not(:disabled) {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.skl-env-editline button.skl-env-del:hover:not(:disabled) {
		background: var(--text-danger, #c33);
		border-color: var(--text-danger, #c33);
	}

	.skl-env-editline button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.skl-env-msg {
		padding: 0.2rem 0.4rem;
		border-radius: 0.3rem;
		font-size: 0.72rem;
	}

	.skl-env-msg--ok {
		background: var(--surface-success, rgba(40, 160, 80, 0.1));
		color: var(--text-success, #2a8a4a);
	}

	.skl-env-msg--err {
		background: var(--surface-danger, rgba(204, 51, 51, 0.1));
		color: var(--text-danger, #c33);
	}
</style>
