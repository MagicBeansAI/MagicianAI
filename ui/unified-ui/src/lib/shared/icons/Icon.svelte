<script lang="ts">
	/**
	 * Icon — the one shared stroke icon system (replaces emoji / ASCII markers /
	 * one-off inline SVGs). Names + path data live in ./paths.ts (Lucide-sourced,
	 * 24×24 grid). Inherits currentColor so it works in all themes for free.
	 * Decorative by default (aria-hidden); pass `label` for a semantic icon.
	 */
	import type { SVGAttributes } from 'svelte/elements';
	import { ICON_PATHS, type IconName } from './paths';

	let {
		name,
		size = 16,
		label = '',
		...rest
	}: { name: IconName; size?: number; label?: string } & SVGAttributes<SVGSVGElement> = $props();
</script>

<svg
	{...rest}
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width="1.5"
	stroke-linecap="round"
	stroke-linejoin="round"
	role={label ? 'img' : 'presentation'}
	aria-label={label || undefined}
	aria-hidden={label ? undefined : 'true'}
>
	{#each ICON_PATHS[name] as d (d)}
		<path {d} />
	{/each}
</svg>
