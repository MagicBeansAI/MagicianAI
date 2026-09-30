<script lang="ts">
	import EntityGrid from '$lib/magician/components/generative/EntityGrid.svelte';

	export let rows: Array<Record<string, unknown>> = [];
	export let currentPage = 1;
	export let pageCount = 1;
	export let totalItems = 0;
	export let startItem = 0;
	export let endItem = 0;

	let requestedPage = 0;
	let requestedSort = '';
</script>

<EntityGrid
	columns={[{ key: 'title', label: 'Title', sortable: true }]}
	{rows}
	pageSize={1}
	paginationMode="server"
	{currentPage}
	{pageCount}
	{totalItems}
	{startItem}
	{endItem}
	on:pagechange={(event) => (requestedPage = event.detail.page)}
	on:sortchange={(event) => (requestedSort = `${event.detail.sortKey}:${event.detail.sortDir}`)}
/>

<output data-testid="requested-page">{requestedPage}</output>
<output data-testid="requested-sort">{requestedSort}</output>
