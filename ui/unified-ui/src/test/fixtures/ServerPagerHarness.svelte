<script lang="ts">
	import ServerPager from '$lib/shared/components/ServerPager.svelte';

	export let currentPage = 1;
	export let pageCount = 1;
	export let startItem = 0;
	export let endItem = 0;
	export let totalItems = 0;
	export let pageCountExact = true;
	export let totalItemsExact = true;
	export let disabled = false;
	export let loading = false;
</script>

<ServerPager
	{currentPage}
	{pageCount}
	{startItem}
	{endItem}
	{totalItems}
	{pageCountExact}
	{totalItemsExact}
	{disabled}
	{loading}
	on:pagechange={(event) => (currentPage = event.detail.page)}
/>

<output data-testid="pager-page">{currentPage}</output>
