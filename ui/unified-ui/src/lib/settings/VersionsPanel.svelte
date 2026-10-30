<script lang="ts">
	// Settings → About: the version of every part of the stack this browser
	// talks to. The web app's own version is baked in at build time; the rest
	// come from the backend's `/health` contract (`versions` map, with the
	// older top-level `version` / `api_version` fields as fallbacks).
	import { onMount } from 'svelte';

	interface HealthVersions {
		magician?: string | null;
		magician_api?: string | null;
		magicutor?: string | null;
		desktop?: string | null;
	}

	interface HealthPayload {
		version?: string | null;
		api_version?: string | null;
		magician?: string | null;
		magicutor_status?: string | null;
		tauri_status?: string | null;
		versions?: HealthVersions | null;
	}

	interface VersionRow {
		id: string;
		label: string;
		version: string | null;
		/** Muted word beside the value: "healthy", "offline", "unknown". */
		note: string | null;
	}

	const HEALTH_TIMEOUT_MS = 5_000;
	const webVersion = typeof __APP_VERSION__ === 'string' ? __APP_VERSION__ : null;

	let health = $state<HealthPayload | null>(null);
	let loading = $state(false);
	let failed = $state(false);

	function text(value: unknown): string | null {
		return typeof value === 'string' && value.trim() ? value.trim() : null;
	}

	/** A part's note: its reported status, or why its version is missing. */
	function noteFor(version: string | null, status: string | null, reachable: boolean): string | null {
		if (status && status !== 'healthy') return 'offline';
		if (version) return status;
		return reachable || !failed ? 'unknown' : 'offline';
	}

	const rows = $derived.by((): VersionRow[] => {
		const reachable = !!health && !failed;
		const versions = health?.versions ?? {};
		const backend = text(versions.magician) ?? text(health?.version);
		const api = text(versions.magician_api) ?? text(health?.api_version);
		const magicutor = text(versions.magicutor);
		const desktop = text(versions.desktop);
		const backendStatus = reachable ? (text(health?.magician) ?? 'healthy') : null;
		return [
			{ id: 'web', label: 'Web app', version: webVersion, note: null },
			{ id: 'magician', label: 'Magician backend', version: backend, note: noteFor(backend, backendStatus, reachable) },
			{ id: 'magician-api', label: 'Magician API', version: api, note: noteFor(api, null, reachable) },
			{
				id: 'magicutor',
				label: 'Magicutor',
				version: magicutor,
				note: noteFor(magicutor, text(health?.magicutor_status), reachable)
			},
			{
				id: 'desktop',
				label: 'Desktop',
				version: desktop,
				note: noteFor(desktop, text(health?.tauri_status), reachable)
			}
		];
	});

	async function refresh(): Promise<void> {
		if (loading) return;
		loading = true;
		try {
			// `/health` is proxied at the root, not under `/api/magician`.
			const response = await fetch('/health', {
				headers: { Accept: 'application/json' },
				// `/health` sends no cache headers, so a browser may reuse an
				// old answer — which showed a pre-restart backend's versions.
				cache: 'no-store',
				signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS)
			});
			if (!response.ok) throw new Error(String(response.status));
			health = (await response.json()) as HealthPayload;
			failed = false;
		} catch {
			health = null;
			failed = true;
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		void refresh();
	});
</script>

<section class="versions-panel" aria-labelledby="versions-panel-title">
	<div class="header">
		<div>
			<p class="overline">About</p>
			<h2 id="versions-panel-title">Versions</h2>
			<p class="lede">What this browser and the Magician it talks to are running. Quote these when you report a problem.</p>
		</div>
		<div class="actions">
			<button class="refresh" type="button" disabled={loading} onclick={() => void refresh()}>
				{loading ? 'Checking…' : 'Refresh'}
			</button>
		</div>
	</div>

	<dl class="version-list">
		{#each rows as row (row.id)}
			<div class="version-row" data-version-row={row.id}>
				<dt>{row.label}</dt>
				<dd>
					<span class="version" class:missing={!row.version}>{row.version ?? '—'}</span>
					{#if row.note}<span class="note">{row.note}</span>{/if}
				</dd>
			</div>
		{/each}
	</dl>

	{#if failed}
		<p class="muted" role="status">Could not reach the Magician backend, so only the web app's version is known.</p>
	{/if}
</section>

<style>
	.versions-panel {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-sizing: border-box;
		color: var(--text-primary);
		display: grid;
		gap: 0.85rem;
		padding: 1rem;
	}

	.header {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem 1rem;
		justify-content: space-between;
	}

	.header > :first-child {
		flex: 1 1 16rem;
		min-width: 0;
	}

	.overline,
	h2,
	p,
	dl,
	dd {
		letter-spacing: 0;
		margin: 0;
	}

	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}

	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
		margin: 0.2rem 0 0.35rem;
	}

	.lede {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
	}

	.actions {
		display: flex;
		gap: 0.5rem;
	}

	.refresh {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.86rem;
		font-weight: 600;
		justify-content: center;
		line-height: 1.2;
		min-height: 2.25rem;
		padding: 0.55rem 0.8rem;
		white-space: nowrap;
	}

	.refresh:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.refresh:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 70%, transparent);
		outline-offset: 2px;
	}

	.version-list {
		border-top: 1px solid var(--border-soft);
		display: grid;
	}

	.version-row {
		align-items: baseline;
		border-bottom: 1px solid var(--border-soft);
		display: grid;
		gap: 0.75rem;
		grid-template-columns: minmax(0, 1fr) auto;
		padding: 0.5rem 0.15rem;
	}

	.version-row:last-child {
		border-bottom: 0;
	}

	dt {
		color: var(--text-secondary);
		font-size: 0.86rem;
		min-width: 0;
	}

	dd {
		align-items: baseline;
		display: inline-flex;
		gap: 0.45rem;
		justify-content: flex-end;
		min-width: 0;
		text-align: right;
	}

	.version {
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 0.82rem;
		overflow-wrap: anywhere;
	}

	.version.missing {
		color: var(--text-muted);
	}

	.note,
	.muted {
		color: var(--text-muted);
		font-size: 0.75rem;
	}

	.muted {
		line-height: 1.45;
	}
</style>
