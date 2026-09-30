<script lang="ts">
	// Atmosphere layer — fixed-position decorative ground for the new shell.
	// Two stacked layers driven by theme tokens (--body-decoration / --body-pattern).
	//
	// Rendered once at the (app) layout root. NOT rendered inside the
	// standalone-pane branch (so /about and / keep their own dedicated
	// backgrounds). Cheap CSS-only; no JS state or subscriptions.
</script>

<div class="atmosphere atmosphere-pools" aria-hidden="true"></div>
<div class="atmosphere atmosphere-grain" aria-hidden="true"></div>

<style>
	.atmosphere {
		position: fixed;
		inset: 0;
		pointer-events: none;
		z-index: 0;
	}

	/* Layer 1 — coloured radial pools (rust top-left, cool blue bottom-right,
	   plum bottom-left). Themes provide the actual gradient stack via
	   --body-decoration. */
	.atmosphere-pools {
		background: var(--body-decoration, none);
	}

	/* Layer 2 — faint film grain. Themes provide --body-pattern (radial dot
	   tiles) and --body-pattern-size / --body-pattern-opacity. Blend mode
	   adapts: multiply on light grounds, overlay on dark. */
	.atmosphere-grain {
		background-image: var(--body-pattern, none);
		background-size: var(--body-pattern-size, auto);
		opacity: var(--body-pattern-opacity, 0);
		mix-blend-mode: multiply;
	}

	/* Light themes use multiply; dark themes use overlay. Both dispatches
	   themes set their own grain via tokens, so the blend-mode flip is the
	   only per-theme override needed here. */
	:global([data-theme="longhand-dark"]) .atmosphere-grain,
	:global([data-theme="arcane-terminal"]) .atmosphere-grain,
	:global([data-theme="soft-machine-dark"]) .atmosphere-grain,
	:global([data-theme="retro-16bit"]) .atmosphere-grain,
	:global([data-theme="bubbly-dark"]) .atmosphere-grain {
		mix-blend-mode: overlay;
	}
</style>
