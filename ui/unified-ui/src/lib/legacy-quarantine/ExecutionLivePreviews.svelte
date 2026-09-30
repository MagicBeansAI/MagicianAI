<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy } from 'svelte';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Card from '$lib/magician/components/generative/Card.svelte';
	import EmptyState from '$lib/magician/components/generative/EmptyState.svelte';
	import {
		createMagicutorLivePreviewController,
		type MagicutorLivePreviewController,
		type MagicutorLivePreviewSnapshot,
		type MagicutorPreviewTab,
	} from '$lib/stores/magicutorLivePreview';

	export let executionId: string | undefined = undefined;

	const emptySnapshot: MagicutorLivePreviewSnapshot = {
		viewerId: '',
		executionId: null,
		extensionAvailable: null,
		extensionVersion: null,
		status: 'idle',
		pendingCapture: false,
		tabs: [],
		lastUpdatedAt: null,
		error: null
	};

	let preview = emptySnapshot;
	let controller: MagicutorLivePreviewController | null = null;
	let unsubscribePreview: (() => void) | null = null;
	let currentExecutionId: string | null = null;

	function destroyController(): void {
		if (unsubscribePreview) {
			unsubscribePreview();
			unsubscribePreview = null;
		}
		if (controller) {
			controller.destroy();
			controller = null;
		}
	}

	function initController(nextExecutionId: string | null): void {
		destroyController();
		currentExecutionId = nextExecutionId;
		controller = createMagicutorLivePreviewController(nextExecutionId);
		unsubscribePreview = controller.subscribe((value) => {
			preview = value;
		});
	}

	$: if (browser && (executionId || null) !== currentExecutionId) {
		initController(executionId || null);
	}

	onDestroy(() => {
		destroyController();
	});

	// --- Helpers ---
	function refreshPreviews(): void {
		controller?.refresh();
	}

	function retryCapture(): void {
		controller?.retryCapture();
	}

	function formatUpdatedAt(timestamp: number | null): string {
		if (!timestamp) {
			return 'Waiting for frames';
		}
		return `Updated ${new Date(timestamp).toLocaleTimeString()}`;
	}

	function previewStateColor(tab: MagicutorPreviewTab): 'default' | 'success' | 'warning' | 'error' | 'info' {
		if (tab.previewState === 'streaming') {
			return 'success';
		}
		if (tab.previewState === 'closed') {
			return 'warning';
		}
		if (tab.previewState === 'error') {
			return 'error';
		}
		return 'info';
	}

	function browserStatusColor(tab: MagicutorPreviewTab): 'default' | 'success' | 'warning' | 'error' | 'info' {
		if (tab.tabStatus === 'complete') {
			return 'success';
		}
		if (tab.tabStatus === 'loading') {
			return 'warning';
		}
		return 'default';
	}

	function previewStateLabel(tab: MagicutorPreviewTab): string {
		if (tab.previewState === 'streaming') {
			return 'Streaming';
		}
		if (tab.previewState === 'closed') {
			return 'Closed';
		}
		if (tab.previewState === 'error') {
			return 'Error';
		}
		return 'Connecting';
	}

	$: visibleTabs = preview.tabs
		.filter((tab) => tab.frameSrc || tab.previewState !== 'closed')
		.sort((left, right) => (right.lastFrameAt || 0) - (left.lastFrameAt || 0));
</script>

<div class="live-previews-tab">
	<div class="live-previews-toolbar">
		<div class="live-previews-header">
			<h3 class="live-previews-title">Live Activity</h3>
			<p class="live-previews-subtitle">Browser preview frames for this execution.</p>
		</div>
		<div class="live-previews-actions">
			{#if preview.extensionVersion}
				<Badge text={`Extension ${preview.extensionVersion}`} color="info" />
			{/if}
			<Button label="Refresh" variant="outline" size="sm" on:click={refreshPreviews} disabled={!executionId} />
		</div>
	</div>

	{#if !executionId}
		<EmptyState
			title="No automation execution"
			description="Live previews are available only for tasks with a browser automation execution."
		/>
	{:else}
		<!-- Browser status messages (non-blocking — show alongside timeline) -->
		{#if preview.status === 'unavailable'}
			<EmptyState
				title="Extension not available"
				description={preview.error || 'Install or reload the Magicutor extension to view live previews here.'}
				actionLabel="Retry"
				on:action={refreshPreviews}
			/>
		{:else if preview.status === 'error' && preview.tabs.length === 0}
			<EmptyState
				title="Live previews failed"
				description={preview.error || 'The extension could not attach to the current automation tabs.'}
				actionLabel="Retry"
				on:action={refreshPreviews}
			/>
		{:else if preview.status === 'probing' || preview.status === 'connecting'}
			<div class="live-previews-loading">
				<div class="live-previews-loading-dot"></div>
				<span>{preview.status === 'probing' ? 'Checking for Magicutor extension...' : 'Connecting to automation tabs...'}</span>
			</div>
		{/if}

		{#if preview.pendingCapture}
			<div class="live-previews-pending-capture">
				<p>Live preview needs a user gesture to start capture.</p>
				<Button label="Click to start live preview" variant="primary" size="sm" on:click={retryCapture} />
			</div>
		{/if}

		{#if preview.error && preview.tabs.length > 0 && !preview.pendingCapture}
			<div class="live-previews-inline-error">
				<span>{preview.error}</span>
			</div>
		{/if}

		<div class="live-timeline">
			{#each visibleTabs as tab (tab.tabId)}
				<Card
					title={tab.title}
					subtitle={tab.url || `Tab ${tab.tabId}`}
					elevation={0}
					className="live-preview-card"
				>
					<div class="live-preview-card-meta">
						<div class="live-preview-badges">
							<Badge text="Browser" color="info" />
							{#if tab.active}
								<Badge text="Active" color="success" />
							{/if}
							<Badge text={tab.tabStatus} color={browserStatusColor(tab)} />
							<Badge text={previewStateLabel(tab)} color={previewStateColor(tab)} />
						</div>
						<span class="live-preview-updated">{formatUpdatedAt(tab.lastFrameAt)}</span>
					</div>

					{#if tab.frameSrc && tab.previewState !== 'closed'}
						<div class="live-preview-frame-wrap">
							<img
								src={tab.frameSrc}
								alt={`Live preview for ${tab.title}`}
								class="live-preview-frame"
							/>
						</div>
					{:else}
						<div class="live-preview-frame-placeholder">
							<p>{tab.previewState === 'closed' ? 'This tab is no longer available.' : 'Waiting for first frame...'}</p>
						</div>
					{/if}

					{#if tab.error}
						<p class="live-preview-tab-error">{tab.error}</p>
					{/if}
				</Card>
			{/each}

			{#if visibleTabs.length === 0 && preview.status === 'empty'}
				<div class="text-center text-base-content/50 py-8">
					No browser previews yet
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.live-previews-tab {
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
		padding: 1rem;
	}

	.live-previews-toolbar {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}

	.live-previews-header {
		min-width: 0;
	}

	.live-previews-title {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 700;
		color: var(--text-primary);
	}

	.live-previews-subtitle {
		margin: 0.2rem 0 0;
		font-size: 0.8rem;
		color: var(--text-muted);
	}

	.live-previews-actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-shrink: 0;
	}

	.live-previews-loading {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		padding: 0.85rem 1rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		background: var(--bg-card);
		color: var(--text-secondary);
		font-size: 0.84rem;
	}

	.live-previews-loading-dot {
		width: 0.55rem;
		height: 0.55rem;
		border-radius: 999px;
		background: var(--accent-primary);
		animation: live-preview-pulse 1.2s ease-in-out infinite;
	}

	.live-previews-pending-capture {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		padding: 1.25rem 1rem;
		border: 1px dashed color-mix(in srgb, var(--accent-primary) 40%, transparent);
		border-radius: var(--radius-md);
		background: color-mix(in srgb, var(--accent-primary) 5%, transparent);
		text-align: center;
	}

	.live-previews-pending-capture p {
		margin: 0;
		font-size: 0.84rem;
		color: var(--text-secondary);
	}

	.live-previews-inline-error {
		padding: 0.75rem 0.9rem;
		border: 1px solid color-mix(in srgb, var(--color-error) 20%, transparent);
		border-radius: var(--radius-md);
		background: color-mix(in srgb, var(--color-error) 7%, transparent);
		color: var(--color-error);
		font-size: 0.8rem;
	}

	/* Unified timeline layout */
	.live-timeline {
		display: grid;
		gap: 1rem;
	}

	/* Browser card styling (preserved from original) */
	:global(.live-preview-card) :global(.muij-card-slot) {
		margin-top: 0.9rem;
	}

	.live-preview-card-meta {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 0.9rem;
	}

	.live-preview-badges {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.live-preview-updated {
		font-size: 0.74rem;
		color: var(--text-muted);
		white-space: nowrap;
	}

	.live-preview-frame-wrap,
	.live-preview-frame-placeholder {
		overflow: hidden;
		border-radius: var(--radius-md);
		border: 1px solid var(--border-soft);
		background: color-mix(in srgb, var(--bg-soft) 82%, black 4%);
	}

	.live-preview-frame {
		display: block;
		width: 100%;
		aspect-ratio: 16 / 9;
		object-fit: contain;
		background: #050505;
	}

	.live-preview-frame-placeholder {
		display: flex;
		align-items: center;
		justify-content: center;
		min-height: 220px;
		padding: 1rem;
		text-align: center;
		color: var(--text-muted);
		font-size: 0.82rem;
	}

	.live-preview-tab-error {
		margin: 0.75rem 0 0;
		font-size: 0.78rem;
		color: var(--color-error);
	}

	@keyframes live-preview-pulse {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.8);
		}

		50% {
			opacity: 1;
			transform: scale(1);
		}
	}

	@media (max-width: 720px) {
		.live-previews-toolbar,
		.live-preview-card-meta {
			flex-direction: column;
			align-items: flex-start;
		}

		.live-previews-actions {
			width: 100%;
			justify-content: space-between;
		}

		.live-preview-updated {
			white-space: normal;
		}
	}
</style>
