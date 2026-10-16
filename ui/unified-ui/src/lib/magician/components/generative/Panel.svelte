<script lang="ts">
	import { buildStableDomId } from './idUtil';

	export let header: string = '';
	export let footer: string = '';
	export let collapsible: boolean = false;
	export let collapsed: boolean = false;
	/** R207: When false, collapsible header renders as non-interactive div. */
	export let interactive: boolean = false;
	/** R213: Stable ID base for aria-controls linkage. */
	export let idBase: string = '';

	// R330: Local user override so toggling doesn't fight the server prop.
	// When the server changes `collapsed`, the override resets.
	let userOverride: boolean | null = null;
	let lastServerCollapsed: boolean = collapsed;

	$: hasHeader = header.trim().length > 0;
	$: effectiveCollapsible = collapsible && hasHeader;
	// R207: Only allow toggle when both collapsible and interactive
	$: isInteractive = effectiveCollapsible && interactive;
	// R330: Reset userOverride when server pushes a new collapsed value
	$: if (collapsed !== lastServerCollapsed) {
		lastServerCollapsed = collapsed;
		userOverride = null;
	}
	// R405: Clear stale userOverride when panel becomes non-interactive
	$: if (!interactive && userOverride !== null) {
		userOverride = null;
	}
	// R345: Preserve server-provided collapsed state for display fidelity.
	// R330: Prefer local userOverride when the user has toggled.
	$: effectiveCollapsed = effectiveCollapsible ? (userOverride ?? collapsed) : false;
	// R213: Stable body ID for aria-controls
	// R350: Use sanitized deterministic ID construction to avoid invalid ARIA link targets.
	$: bodyId = idBase ? buildStableDomId('muij-panel-body', idBase) : '';

	function toggle() {
		if (isInteractive) {
			userOverride = !effectiveCollapsed;
		}
	}
</script>

<div class="muij-panel">
		{#if hasHeader}
			{#if isInteractive}
				<button
				type="button"
				class="muij-panel-header muij-panel-header-collapsible"
					aria-expanded={!effectiveCollapsed}
					aria-controls={bodyId || undefined}
					on:click={toggle}
				>
					<span class="muij-panel-toggle" class:muij-panel-toggle-collapsed={effectiveCollapsed} aria-hidden="true">&#9654;</span>
					<span class="muij-panel-header-text">{header}</span>
				</button>
		{:else if effectiveCollapsible}
			<!-- R671: No toggle arrow when non-interactive — arrow implies
			     affordance (clickable) that doesn't exist, confusing users. -->
			<div class="muij-panel-header">
				<span class="muij-panel-header-text">{header}</span>
			</div>
		{:else}
			<div class="muij-panel-header">
				<span class="muij-panel-header-text">{header}</span>
			</div>
		{/if}
	{/if}
		<div class="muij-panel-body" id={bodyId || undefined} aria-hidden={effectiveCollapsed || undefined} class:muij-panel-body-hidden={effectiveCollapsed}>
			{#if !effectiveCollapsed}
				<slot />
			{/if}
		</div>
	{#if footer && !effectiveCollapsed}
		<div class="muij-panel-footer">{footer}</div>
	{/if}
</div>

<style>
	.muij-panel {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		background: var(--bg-card);
		overflow: hidden;
	}

	.muij-panel-header {
		padding: 8px 12px;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 600;
		color: var(--text-primary);
		border-bottom: 1px solid var(--border-soft);
		display: flex;
		align-items: center;
		gap: 6px;
		overflow-wrap: anywhere;
	}

	.muij-panel-header-collapsible {
		width: 100%;
		text-align: left;
		background: transparent;
		border: none;
		cursor: pointer;
		user-select: none;
	}

	.muij-panel-header-collapsible:hover {
		background: var(--bg-soft);
	}

	.muij-panel-header-collapsible:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	.muij-panel-toggle {
		font-size: 0.625rem;
		transition: transform 0.15s;
		display: inline-block;
	}

	.muij-panel-toggle:not(.muij-panel-toggle-collapsed) {
		transform: rotate(90deg);
	}

	.muij-panel-body {
		padding: var(--space-md);
		overflow-wrap: anywhere;
	}

	/* R580: Use CSS instead of invalid `hidden` attribute on div */
	.muij-panel-body-hidden {
		display: none;
	}

	.muij-panel-footer {
		padding: 8px 12px;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-muted);
		border-top: 1px solid var(--border-soft);
		overflow-wrap: anywhere;
	}
</style>
