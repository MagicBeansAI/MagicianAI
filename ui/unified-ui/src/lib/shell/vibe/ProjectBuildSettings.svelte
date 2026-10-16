<script lang="ts">
	import { onMount } from 'svelte';
	import {
		checkSourceLabel,
		fetchProjectInfo,
		type ProjectInfo
	} from '$lib/shell/vibe/stage/checks';
	import {
		vibeDevProjectStore,
		type BuildCache,
		type ProjectCheck
	} from '$lib/stores/vibeDevProjectStore';
	import { get } from 'svelte/store';

	export let projectId: string;

	/** Common toolchain caches; Rust's CARGO_TARGET_DIR is always applied. */
	const CACHE_PRESETS: { label: string; env: string }[] = [
		{ label: 'Gradle', env: 'GRADLE_USER_HOME' },
		{ label: 'Elixir (Mix build)', env: 'MIX_BUILD_PATH' },
		{ label: 'Elixir (Hex)', env: 'HEX_HOME' },
		{ label: 'Go build cache', env: 'GOCACHE' },
		{ label: 'npm cache', env: 'npm_config_cache' }
	];

	let info: ProjectInfo | null = null;
	let loading = true;
	let saving = false;
	let error: string | null = null;
	let editingChecks = false;
	let checks: ProjectCheck[] = [];
	let caches: BuildCache[] = [];
	let cachesDirty = false;
	let writePaths: string[] = [];
	let newWritePath = '';

	async function load(): Promise<void> {
		loading = true;
		info = await fetchProjectInfo(projectId);
		loading = false;
		editingChecks = info?.checks_source === 'project';
		checks = (info?.configured_checks ?? []).map((check) => ({ ...check }));
		caches = (info?.build_caches ?? []).map((cache) => ({ ...cache }));
		cachesDirty = false;
		writePaths = [...(info?.sandbox_write_paths ?? [])];
		newWritePath = '';
	}

	onMount(() => {
		void load();
	});

	function customize(): void {
		checks = (info?.checks ?? []).map((check) => ({
			kind: check.kind,
			run: check.display,
			timeout_secs: check.timeout_secs ?? null
		}));
		editingChecks = true;
	}

	function addCheck(): void {
		checks = [...checks, { kind: 'test', run: '', timeout_secs: null }];
	}

	function addCache(env = ''): void {
		if (env && caches.some((cache) => cache.env === env)) return;
		caches = [...caches, { env, path: null }];
		cachesDirty = true;
	}

	async function save(patch: {
		checks?: ProjectCheck[] | null;
		build_caches?: BuildCache[];
		sandbox_write_paths?: string[];
	}): Promise<void> {
		saving = true;
		error = null;
		const updated = await vibeDevProjectStore.updateProject(projectId, patch);
		saving = false;
		if (!updated) {
			error = get(vibeDevProjectStore).error ?? 'Could not save.';
			return;
		}
		await load();
	}

	function saveChecks(): void {
		const cleaned = checks
			.map((check) => ({
				kind: check.kind.trim(),
				run: check.run.trim(),
				...(check.timeout_secs ? { timeout_secs: Number(check.timeout_secs) } : {})
			}))
			.filter((check) => check.kind && check.run);
		void save({ checks: cleaned });
	}

	function saveCaches(): void {
		const cleaned = caches
			.map((cache) => ({
				env: cache.env.trim(),
				...(cache.path?.trim() ? { path: cache.path.trim() } : {})
			}))
			.filter((cache) => cache.env);
		void save({ build_caches: cleaned });
	}
</script>

<div class="build-settings">
	<section class="build-settings__section" aria-label="Checks">
		<div class="build-settings__head">
			<span class="build-settings__title">Checks</span>
			{#if info && !editingChecks}
				<button type="button" class="build-settings__link" on:click={customize}>Customize</button>
			{/if}
		</div>
		{#if loading}
			<p class="build-settings__hint">Loading…</p>
		{:else if !editingChecks}
			<p class="build-settings__hint">
				{info ? checkSourceLabel(info) : 'Could not read project checks'}. Verification and the
				Checks tab run these; any language works — set your own or add
				<code>.magician/checks.yaml</code> to the repo.
			</p>
			{#if info && info.checks.length > 0}
				<ul class="build-settings__list">
					{#each info.checks as check (check.kind + check.display)}
						<li><span class="build-settings__kind">{check.kind}</span><code>{check.display}</code></li>
					{/each}
				</ul>
			{/if}
		{:else}
			<p class="build-settings__hint">
				Shell commands run from the project root. An empty list means no checks.
			</p>
			{#each checks as check, index (index)}
				<div class="build-settings__row">
					<input
						class="build-settings__kind-input"
						aria-label="Check kind"
						placeholder="test"
						bind:value={check.kind}
					/>
					<input
						class="build-settings__cmd-input"
						aria-label="Check command"
						placeholder="mix test"
						spellcheck="false"
						bind:value={check.run}
					/>
					<input
						class="build-settings__num-input"
						aria-label="Timeout seconds"
						type="number"
						min="1"
						max="3600"
						placeholder="240s"
						bind:value={check.timeout_secs}
					/>
					<button
						type="button"
						class="build-settings__x"
						aria-label="Remove check"
						on:click={() => (checks = checks.filter((_, i) => i !== index))}>✕</button
					>
				</div>
			{/each}
			<div class="build-settings__actions">
				<button type="button" class="build-settings__link" on:click={addCheck}>+ Add check</button>
				<span class="build-settings__spacer"></span>
				<button
					type="button"
					class="build-settings__link"
					disabled={saving}
					on:click={() => void save({ checks: null })}>Use detected</button
				>
				<button type="button" class="build-settings__save" disabled={saving} on:click={saveChecks}
					>{saving ? 'Saving…' : 'Save checks'}</button
				>
			</div>
		{/if}
	</section>

	<section class="build-settings__section" aria-label="Build caches">
		<div class="build-settings__head">
			<span class="build-settings__title">Build caches</span>
		</div>
		<p class="build-settings__hint">
			Environment variables your toolchain caches under. Magician shares each across this
			project's runs and checks and empties it when disk runs low. Leave the path blank for a
			managed folder.
		</p>
		{#if info?.build_env?.length}
			<ul class="build-settings__list">
				{#each info.build_env as entry (entry.env)}
					<li><span class="build-settings__kind">{entry.env}</span><code>{entry.path}</code></li>
				{/each}
			</ul>
		{/if}
		{#each caches as cache, index (index)}
			<div class="build-settings__row">
				<input
					class="build-settings__env-input"
					aria-label="Cache environment variable"
					placeholder="GRADLE_USER_HOME"
					spellcheck="false"
					bind:value={cache.env}
					on:input={() => (cachesDirty = true)}
				/>
				<input
					class="build-settings__cmd-input"
					aria-label="Cache path"
					placeholder="Managed by Magician"
					spellcheck="false"
					bind:value={cache.path}
					on:input={() => (cachesDirty = true)}
				/>
				<button
					type="button"
					class="build-settings__x"
					aria-label="Remove cache"
					on:click={() => {
						caches = caches.filter((_, i) => i !== index);
						cachesDirty = true;
					}}>✕</button
				>
			</div>
		{/each}
		<div class="build-settings__actions">
			<select
				class="build-settings__preset"
				aria-label="Add a cache preset"
				on:change={(event) => {
					const env = event.currentTarget.value;
					if (env) addCache(env);
					event.currentTarget.value = '';
				}}
			>
				<option value="">+ Add preset…</option>
				{#each CACHE_PRESETS as preset (preset.env)}
					<option value={preset.env}>{preset.label} ({preset.env})</option>
				{/each}
			</select>
			<button type="button" class="build-settings__link" on:click={() => addCache()}>+ Custom</button>
			<span class="build-settings__spacer"></span>
			<button
				type="button"
				class="build-settings__save"
				disabled={saving || !cachesDirty}
				on:click={saveCaches}>{saving ? 'Saving…' : 'Save caches'}</button
			>
		</div>
	</section>

	<section class="build-settings__section" aria-label="Writable folders">
		<div class="build-settings__head">
			<span class="build-settings__title">Writable folders</span>
		</div>
		<p class="build-settings__hint">
			Coding runs may write only their workspace, build caches, temp and tool caches. When a run
			is refused a write you are asked; folders you allow appear here. Your home folder, SSH keys,
			the keychain and system folders can never be added.
		</p>
		{#if writePaths.length > 0}
			<ul class="build-settings__list">
				{#each writePaths as path (path)}
					<li>
						<code>{path}</code>
						<button
							type="button"
							class="build-settings__x"
							aria-label={`Remove ${path}`}
							disabled={saving}
							on:click={() =>
								void save({ sandbox_write_paths: writePaths.filter((other) => other !== path) })}
							>✕</button
						>
					</li>
				{/each}
			</ul>
		{/if}
		<div class="build-settings__row">
			<input
				class="build-settings__cmd-input"
				aria-label="Add a writable folder"
				placeholder="/absolute/path/to/folder"
				spellcheck="false"
				bind:value={newWritePath}
			/>
			<button
				type="button"
				class="build-settings__save"
				disabled={saving || !newWritePath.trim()}
				on:click={() =>
					void save({ sandbox_write_paths: [...writePaths, newWritePath.trim()] })}>Add</button
			>
		</div>
	</section>

	{#if error}
		<p class="build-settings__error" role="alert">{error}</p>
	{/if}
</div>

<style>
	.build-settings {
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
	}
	.build-settings__section {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.build-settings__head,
	.build-settings__actions,
	.build-settings__row {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}
	.build-settings__title {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 750;
		color: var(--text-primary);
	}
	.build-settings__hint {
		margin: 0;
		font-size: var(--text-xs, 0.75rem);
		color: var(--text-muted);
		line-height: 1.4;
	}
	.build-settings__list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}
	.build-settings__list li {
		display: flex;
		gap: 0.5rem;
		align-items: baseline;
		min-width: 0;
		font-size: var(--text-xs, 0.75rem);
	}
	.build-settings__list code {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.build-settings__kind {
		flex: 0 0 5.5rem;
		color: var(--text-secondary);
		font-weight: 650;
	}
	.build-settings__row input {
		min-width: 0;
		padding: 0.3rem 0.45rem;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-surface);
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-xs, 0.75rem);
	}
	.build-settings__kind-input {
		flex: 0 0 5.5rem;
	}
	.build-settings__env-input {
		flex: 0 0 11rem;
		font-family: var(--font-mono);
	}
	.build-settings__cmd-input {
		flex: 1;
		font-family: var(--font-mono);
	}
	.build-settings__num-input {
		flex: 0 0 4.5rem;
	}
	.build-settings__x,
	.build-settings__link {
		border: 0;
		background: transparent;
		color: var(--text-secondary);
		font: inherit;
		font-size: var(--text-xs, 0.75rem);
		cursor: pointer;
	}
	.build-settings__link {
		font-weight: 650;
		color: var(--accent-primary);
	}
	.build-settings__link:disabled,
	.build-settings__save:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.build-settings__spacer {
		flex: 1;
	}
	.build-settings__save {
		padding: 0.25rem 0.7rem;
		border: 1px solid var(--border-default);
		border-radius: 7px;
		background: var(--bg-card);
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-xs, 0.75rem);
		font-weight: 700;
		cursor: pointer;
	}
	.build-settings__preset {
		font-size: var(--text-xs, 0.75rem);
		max-width: 14rem;
	}
	.build-settings__error {
		margin: 0;
		color: var(--color-error);
		font-size: var(--text-xs, 0.75rem);
	}
	@media (max-width: 640px) {
		.build-settings__row {
			flex-wrap: wrap;
		}
		.build-settings__env-input,
		.build-settings__kind-input {
			flex: 1 1 100%;
		}
	}
</style>
