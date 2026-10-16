<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { NeedsItem } from '../attentionGlue';
	import type { FleetEngine } from '../engine/engine';
	import type { CitizenVM } from '../engine/types';
	import type { Quest } from '../fleetQuests';
	import { GameInspector } from '../ui';
	import CitizenDetail from './CitizenDetail.svelte';

	export let engine: FleetEngine;
	export let citizen: CitizenVM;
	export let guildName: string | undefined = undefined;
	export let needsItems: NeedsItem[] = [];
	export let quests: Quest[] = [];

	const dispatch = createEventDispatcher<{ close: void; focus: void; detail: void; task: string }>();
</script>

<GameInspector
	open
	title={citizen.name}
	eyebrow={citizen.isCeo ? 'Crew lead' : citizen.isPrimary ? 'Primary agent' : 'Crew member'}
	subtitle={`${citizen.title}${guildName ? ` · ${guildName}` : ''}`}
	tone={citizen.vibe === 'needs' ? 'attention' : citizen.vibe === 'working' ? 'active' : 'neutral'}
	width="wide"
	data-game-skin="pixel"
	on:back={() => dispatch('close')}
>
	<CitizenDetail
		{engine}
		{citizen}
		{guildName}
		{needsItems}
		{quests}
		showFocus
		on:focus
		on:detail
		on:task
	/>
</GameInspector>
