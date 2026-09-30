<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { timedFetch } from '$lib/shared/fetch';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	type PreviewStatus = {
		status: string;
		local_url: string | null;
		port: number | null;
		ready: boolean;
		proxy_path: string | null;
		recent_log_tail?: string | null;
	};

	export let projectId: string | null = null;
	export let pinnedUrl: string | null = null;

	let preview: PreviewStatus | null = null;
	let loading = false;
	let error: string | null = null;
	let mounted = false;
	let refreshTimer: ReturnType<typeof setInterval> | null = null;
	let refreshGeneration = 0;
	let lastProjectId: string | null = null;

	$: openUrl = preview?.local_url ?? pinnedUrl ?? null;
	$: previewLabel = preview?.ready && preview.local_url
		? compactUrl(preview.local_url)
		: preview?.status ?? (projectId ? 'stopped' : 'no project');
	$: if (mounted && projectId !== lastProjectId) {
		lastProjectId = projectId;
		preview = null;
		void refreshPreview();
	}

	onMount(() => {
		mounted = true;
		lastProjectId = projectId;
		void refreshPreview();
		refreshTimer = setInterval(() => {
			void refreshPreview();
		}, 8_000);
	});

	onDestroy(() => {
		mounted = false;
		if (refreshTimer) clearInterval(refreshTimer);
	});

	async function refreshPreview(): Promise<void> {
		if (!projectId) {
			preview = null;
			loading = false;
			return;
		}
		const generation = ++refreshGeneration;
		loading = true;
		error = null;
		try {
			const response = await timedFetch(previewUrl(''));
			const payload = (await response.json().catch(() => null)) as PreviewStatus | { error?: string; message?: string } | null;
			if (!response.ok || !payload) {
				throw new Error((payload as { error?: string; message?: string } | null)?.message
					|| (payload as { error?: string } | null)?.error
					|| `server returned ${response.status}`);
			}
			if (!mounted || generation !== refreshGeneration) return;
			preview = payload as PreviewStatus;
		} catch (err) {
			if (!mounted || generation !== refreshGeneration) return;
			error = err instanceof Error ? err.message : 'Could not load preview';
		} finally {
			if (mounted && generation === refreshGeneration) {
				loading = false;
			}
		}
	}

	async function startPreview(): Promise<void> {
		if (!projectId) return;
		await mutatePreview('/start');
	}

	async function stopPreview(): Promise<void> {
		if (!projectId) return;
		await mutatePreview('/stop');
	}

	async function mutatePreview(suffix: '/start' | '/stop'): Promise<void> {
		if (!projectId) return;
		loading = true;
		error = null;
		try {
			const response = await timedFetch(previewUrl(suffix), { method: 'POST' });
			if (!response.ok) {
				const payload = (await response.json().catch(() => null)) as { error?: string; message?: string } | null;
				throw new Error(payload?.message || payload?.error || `server returned ${response.status}`);
			}
			await refreshPreview();
		} catch (err) {
			error = err instanceof Error ? err.message : 'Could not update preview';
		} finally {
			loading = false;
		}
	}

	function previewUrl(suffix: '' | '/start' | '/stop'): string {
		const id = encodeURIComponent(projectId ?? '');
		const params = scopedParams();
		const query = params.length > 0 ? `?${params}` : '';
		return `/api/magician/v2/vibedev/projects/${id}/preview${suffix}${query}`;
	}

	function scopedParams(): string {
		const scope = get(scopeIdentityStore);
		const params = new URLSearchParams();
		return params.toString();
	}

	function compactUrl(url: string): string {
		try {
			const parsed = new URL(url);
			return `${parsed.host}${parsed.pathname === '/' ? '' : parsed.pathname}`;
		} catch {
			return url;
		}
	}
</script>

<section class="vibe-preview" aria-labelledby="vibe-preview-heading">
	<header class="vibe-preview__head">
		<div>
			<h2 id="vibe-preview-heading">Preview</h2>
			<p>{previewLabel}{preview?.port ? ` · :${preview.port}` : ''}</p>
		</div>
		<div class="vibe-preview__actions">
			<button type="button" class="vibe-preview__btn" on:click={() => void startPreview()} disabled={!projectId || Boolean(preview?.ready)}>
				Start
			</button>
			<button type="button" class="vibe-preview__btn" on:click={() => void stopPreview()} disabled={!projectId || !Boolean(preview?.ready)}>
				Stop
			</button>
			<button type="button" class="vibe-preview__btn" disabled={loading} on:click={() => void refreshPreview()}>
				{loading ? 'Refreshing' : 'Refresh'}
			</button>
			{#if openUrl}
				<a class="vibe-preview__btn vibe-preview__btn--primary" href={openUrl} target="_blank" rel="noreferrer">
					Open
				</a>
			{/if}
		</div>
	</header>

	{#if error}
		<div class="vibe-preview__empty vibe-preview__empty--error">{error}</div>
	{:else if preview?.ready && preview.local_url}
		<!-- Load the dev server DIRECTLY (cross-origin), not the same-origin proxy
		     sub-path. Generated apps (Next/Vite) emit ROOT-ABSOLUTE asset URLs
		     (`/_next/*`, `/foo.svg` from `public/`) that only resolve when the app
		     is served at the origin root — through a sub-path proxy they 404 and
		     the page renders blank. Cross-origin keeps the app at root so it renders
		     (and matches "Open in new tab"). Trade-off: the same-origin click-to-edit
		     overlay (injected by the proxy) is unavailable here; restoring it needs a
		     cross-origin-safe injection (follow-up). Same scheme (http↔http) so no
		     mixed-content block. -->
		<div class="vibe-preview__frame-shell">
			<iframe title="VibeDev preview" src={preview.local_url} referrerpolicy="no-referrer"></iframe>
		</div>
	{:else}
		<div class="vibe-preview__empty">
			{#if preview?.status === 'starting'}
				Starting dev server…
			{:else if preview?.status === 'crashed'}
				Dev server crashed.
			{:else}
				Dev server not running.
			{/if}
			{#if preview?.recent_log_tail}
				<pre>{preview.recent_log_tail}</pre>
			{/if}
		</div>
	{/if}
</section>

<style>
	.vibe-preview {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		border: 1px solid var(--vibe-border, var(--border-soft, #e5e2dc));
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-surface, #fff) 96%, transparent);
		padding: 0.9rem;
		box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.1));
		min-width: 0;
	}

	.vibe-preview__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}

	.vibe-preview h2 {
		margin: 0;
		font-size: 0.98rem;
		line-height: 1.2;
		letter-spacing: 0;
	}

	.vibe-preview p {
		margin: 0.25rem 0 0;
		color: var(--vibe-text-muted, var(--text-secondary, #6b6258));
		font-size: 0.78rem;
		overflow-wrap: anywhere;
	}

	.vibe-preview__actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		justify-content: flex-end;
	}

	.vibe-preview__btn {
		border: 1px solid var(--vibe-border-strong, var(--border-soft, #d8d2c8));
		border-radius: 8px;
		background: var(--input-bg, var(--vibe-surface, #fff));
		color: var(--vibe-text, var(--text-primary, #2d2a26));
		font: inherit;
		font-size: 0.78rem;
		font-weight: 800;
		text-decoration: none;
		cursor: pointer;
		min-height: 2rem;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0.38rem 0.65rem;
	}

	.vibe-preview__btn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent, #c2502a) 72%, transparent);
		background: var(--vibe-accent, #c2502a);
		color: var(--button-primary-color, #fff);
	}

	.vibe-preview__btn:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.vibe-preview__btn:not(:disabled):hover {
		border-color: var(--vibe-accent, #c2502a);
		transform: translateY(-1px);
	}

	.vibe-preview__btn:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent, #c2502a) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-preview__frame-shell {
		height: clamp(20rem, 42vh, 34rem);
		border: 1px solid var(--vibe-border, var(--border-soft, #e5e2dc));
		border-radius: 8px;
		overflow: hidden;
		background: #fff;
	}

	.vibe-preview__frame-shell iframe {
		width: 100%;
		height: 100%;
		border: 0;
		background: #fff;
	}

	.vibe-preview__empty {
		border: 1px dashed var(--vibe-border, var(--border-soft, #e5e2dc));
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-page-surface, #f8f6f2) 72%, transparent);
		color: var(--vibe-text-muted, var(--text-secondary, #6b6258));
		padding: 1rem;
		font-size: 0.86rem;
	}

	.vibe-preview__empty pre {
		max-height: 12rem;
		margin: 0.75rem 0 0;
		overflow: auto;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font-size: 0.74rem;
		line-height: 1.45;
	}

	.vibe-preview__empty--error {
		border-color: color-mix(in srgb, var(--vibe-error, #d23a3a) 48%, transparent);
		background: color-mix(in srgb, var(--vibe-error, #d23a3a) 8%, var(--vibe-surface, #fff));
		color: var(--vibe-error, #d23a3a);
		font-weight: 800;
	}

	@media (max-width: 720px) {
		.vibe-preview__head {
			align-items: stretch;
			flex-direction: column;
		}

		.vibe-preview__actions,
		.vibe-preview__btn {
			width: 100%;
		}

		.vibe-preview__frame-shell {
			height: min(60vh, 28rem);
		}
	}
</style>
