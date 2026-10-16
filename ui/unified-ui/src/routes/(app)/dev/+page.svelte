<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	/**
	 * /dev — the standalone full-screen Workbench (its canonical home), for
	 * terminal work outside the VibeDev cockpit. A slim header keeps an easy
	 * way back to /vibe. (Inside the cockpit, prefer the terminal drawer's
	 * in-place Maximize — it goes full-screen WITHOUT leaving /vibe, so nothing
	 * in the cockpit is lost. Terminal sessions are server-side + unscoped, so
	 * the same running sessions appear here either way.)
	 */
	import WorkbenchColumn from '$lib/shell/WorkbenchColumn.svelte';
</script>

<svelte:head>
	<title>Workbench · {PRODUCT_NAME}</title>
</svelte:head>

<section class="dev-workbench" aria-label="Workbench">
	<header class="dev-workbench__bar">
		<a class="dev-workbench__back" href="/vibe" title="Back to VibeDev">← VibeDev</a>
		<span class="dev-workbench__title">Workbench</span>
		<span class="dev-workbench__hint">All interactive sessions</span>
	</header>
	<div class="dev-workbench__body">
		<WorkbenchColumn threadId={null} placement="main" />
	</div>
</section>

<style>
	.dev-workbench {
		/* Full-bleed below the top bar — opt out of the centered content cap. */
		width: 100%;
		max-width: none;
		height: calc(100vh - var(--app-header-height, 48px));
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		background: var(--bg-surface);
	}
	.dev-workbench__bar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.4rem 0.8rem;
		border-bottom: 1px solid var(--border-soft);
		background: var(--bg-card);
	}
	.dev-workbench__back {
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--text-secondary);
		text-decoration: none;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-full, 999px);
		padding: 0.2rem 0.7rem;
	}
	.dev-workbench__back:hover {
		color: var(--accent-primary);
		border-color: var(--accent-primary);
	}
	.dev-workbench__title {
		font-family: var(--font-display, inherit);
		font-weight: 600;
		font-size: 0.9rem;
		color: var(--text-primary);
	}
	.dev-workbench__hint {
		margin-left: auto;
		font-size: 0.74rem;
		color: var(--text-muted);
	}
	.dev-workbench__body {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}
	.dev-workbench__body :global(> *) {
		flex: 1;
		min-height: 0;
	}
</style>
