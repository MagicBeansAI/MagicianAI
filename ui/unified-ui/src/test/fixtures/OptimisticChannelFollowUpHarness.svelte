<script lang="ts">
	import ChannelFollowUpActions from '$lib/channel/ChannelFollowUpActions.svelte';
	import {
		followUpAttentionMutationKey,
		optimisticAttentionMutationQueue
	} from '$lib/attention/optimisticAttentionMutationQueue';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';

	export let followUps: ChannelFollowUp[] = [];
	$: visible = followUps.filter((followUp) =>
		!$optimisticAttentionMutationQueue.statusByKey.has(
			followUpAttentionMutationKey(followUp.annotation_id, $scopeIdentityStore)
		)
	);
</script>

{#each visible as followUp (followUp.annotation_id)}
	<article aria-label={followUp.subject ?? followUp.annotation_id}>
		<span>{followUp.subject}</span>
		<ChannelFollowUpActions {followUp} />
	</article>
{/each}
