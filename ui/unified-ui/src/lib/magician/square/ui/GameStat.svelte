<script lang="ts">
	import type { GameTone } from './types';
	import '../game-chrome.css';

	export let label: string;
	export let value: string | number;
	export let detail = '';
	export let tone: GameTone = 'neutral';
	export let live: 'off' | 'polite' = 'off';
	export let className = '';
</script>

<dl
	{...$$restProps}
	class={`game-ui-stat ${className}`.trim()}
	data-game-tone={tone}
	aria-live={live === 'off' ? undefined : live}
>
	<dt class="game-ui-stat__term">
		<span class="game-ui-stat__icon" aria-hidden="true"><slot name="icon" /></span>
		<span class="game-ui-stat__label">{label}</span>
	</dt>
	<dd class="game-ui-stat__value">{value}</dd>
	{#if detail}<dd class="game-ui-stat__detail">{detail}</dd>{/if}
</dl>
