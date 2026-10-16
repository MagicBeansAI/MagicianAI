<script lang="ts">
	/**
	 * The plan glyph: marks a planned task, placed just before its title (chat
	 * live task card, Tasks list row, task drawer header). Draws Material
	 * "account_tree" so it matches iOS / Android. Static; the status dot is
	 * separate and unchanged. Show it when `taskHasPlan` / `liveTaskHasPlan`
	 * (`./taskHasPlan`) says so.
	 */
	export let size = 13;
	export let title = 'Planned task';
</script>

<span
	class="plan-glyph"
	role="img"
	aria-label={title}
	{title}
	data-testid="plan-glyph"
	style:width="{size}px"
	style:height="{size}px"
	style:color="var(--accent-secondary)"
>
	<svg
		viewBox="0 0 24 24"
		width={size}
		height={size}
		fill="currentColor"
		aria-hidden="true"
		focusable="false"
	>
		<path d="M22 11V3h-7v3H9V3H2v8h7V8h2v10h4v3h7v-8h-7v3h-2V8h2v3z" />
	</svg>
</span>

<style>
	.plan-glyph {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
		vertical-align: middle;
		line-height: 0;
	}

	.plan-glyph svg {
		display: block;
	}
</style>
