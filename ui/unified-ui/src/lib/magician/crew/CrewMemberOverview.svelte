<script lang="ts">
	import NativeCrewRenderer from './NativeCrewRenderer.svelte';
	import {
		buildCrewMemberOverviewComponent,
		type CrewMemberOverviewModel
	} from './overview';

	export let overview: CrewMemberOverviewModel;
	export let idNamespace = 'crew-member-overview';

	$: components = [buildCrewMemberOverviewComponent(overview)];
</script>

<section class="crew-member-overview" aria-label="Crew member overview">
	<NativeCrewRenderer {components} {idNamespace} validateRouteContract={false} />
</section>

<style>
	.crew-member-overview {
		min-width: 0;
		color: var(--overview-text, var(--text-primary, #2d2a26));
	}
</style>
