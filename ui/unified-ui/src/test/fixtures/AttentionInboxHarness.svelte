<script lang="ts">
	import AttentionInboxSurface from '$lib/attention/AttentionInboxSurface.svelte';
	import type {
		AttentionDisplayRow,
		AttentionSourceFilter
	} from '$lib/attention/model';

	export let rows: AttentionDisplayRow[] = [];
	export let initialLoading = false;
	export let canShowMore = false;
	export let showMoreBusy = false;
	export let hydratingKey: string | null = null;
	export let skillEvolutionActionKey: string | null = null;

	let sourceFilter: AttentionSourceFilter = 'all';
	let search = '';
	let lastAction = '';
</script>

<AttentionInboxSurface
	{rows}
	{sourceFilter}
	{search}
	{initialLoading}
	{canShowMore}
	{showMoreBusy}
	{hydratingKey}
	{skillEvolutionActionKey}
	on:filterchange={(event) => (sourceFilter = event.detail.sourceFilter)}
	on:searchchange={(event) => (search = event.detail.search)}
	on:activate={(event) => (lastAction = `activate:${event.detail.row.key}`)}
	on:skillaction={(event) =>
		(lastAction = `skill:${event.detail.row.key}:${event.detail.action}`)}
	on:rollbackdecision={(event) =>
		(lastAction = `rollback:${event.detail.row.key}:${event.detail.decision}`)}
	on:showmore={() => (lastAction = 'showmore')}
/>

<output data-testid="attention-filter">{sourceFilter}</output>
<output data-testid="attention-search">{search}</output>
<output data-testid="attention-action">{lastAction}</output>
