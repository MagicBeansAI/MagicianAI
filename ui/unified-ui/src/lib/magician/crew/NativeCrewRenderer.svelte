<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import NativeCrewNode from './NativeCrewNode.svelte';
	import type { CrewNativeComponent, CrewNativeInteractionEventDetail } from './nativeSurface';

	export let components: CrewNativeComponent[] = [];
	export let idNamespace = 'crew-native';
	export let validateRouteContract = false;

	const dispatch = createEventDispatcher<{ interaction: CrewNativeInteractionEventDetail }>();

	function forwardInteraction(event: CustomEvent<CrewNativeInteractionEventDetail>): void {
		dispatch('interaction', event.detail);
	}

	$: void validateRouteContract;
</script>

<div class="crew-native-surface" data-namespace={idNamespace}>
	{#each components as component (component.id)}
		<NativeCrewNode {component} on:interaction={forwardInteraction} />
	{/each}
</div>

<style>
	.crew-native-surface {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1rem;
		width: 100%;
		max-width: 100%;
		min-width: 0;
		box-sizing: border-box;
		color: var(--text-primary, #2d2a26);
	}
</style>
