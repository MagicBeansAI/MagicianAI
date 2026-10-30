<!--
  The Settings skeleton: search, category chips, a heading and cards, so the
  first screen has Settings' shape while its code and data arrive. The same
  markup is written into app.html before any script runs; the styles live in
  app.html (`.msk-*`) so both copies match and theme variables apply as soon
  as they load. `cards` alone renders just card outlines (a loading panel).
-->
<script lang="ts">
	export let variant: 'page' | 'card' = 'page';
	export let cards = 4;
</script>

{#if variant === 'card'}
	<div class="msk-card" aria-hidden="true">
		<div class="msk-bar msk-line msk-w30"></div>
		<div class="msk-bar msk-line msk-title msk-w55"></div>
		<div class="msk-bar msk-line msk-w90"></div>
		<div class="msk-bar msk-line msk-w75"></div>
		<div class="msk-bar msk-pill"></div>
	</div>
{:else}
	<div class="msk" role="status" aria-label="Loading settings">
		<div class="msk-in">
			<div class="msk-bar msk-search"></div>
			<div class="msk-chips">
				{#each [0, 1, 2, 3] as chip (chip)}<div class="msk-bar msk-chip"></div>{/each}
			</div>
			<div class="msk-bar msk-heading"></div>
			{#each Array(cards) as _, i (i)}
				<div class="msk-card">
					<div class="msk-bar msk-line msk-w30"></div>
					<div class="msk-bar msk-line msk-title msk-w55"></div>
					<div class="msk-bar msk-line msk-w90"></div>
					<div class="msk-bar msk-line msk-w75"></div>
					<div class="msk-bar msk-pill"></div>
				</div>
			{/each}
		</div>
	</div>
{/if}
