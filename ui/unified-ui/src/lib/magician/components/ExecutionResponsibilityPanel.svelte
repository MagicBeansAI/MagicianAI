<script lang="ts">
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Card from '$lib/magician/components/native/Card.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import Stack from '$lib/magician/components/native/Stack.svelte';
	import Text from '$lib/magician/components/native/Text.svelte';
	import type {
		ExecutionPanelResponsibilityChild as ExecutionResponsibilityChild,
		ExecutionPanelResponsibilityState as ExecutionResponsibilitySnapshot
	} from '$lib/types/executionPanel';

	export let snapshot: ExecutionResponsibilitySnapshot | null = null;
	export let loading = false;
	export let error: string | null = null;

	function statusColor(waitingState: string): 'default' | 'success' | 'warning' | 'error' | 'info' {
		switch (waitingState) {
			case 'Completed':
				return 'success';
			case 'Failed':
			case 'Cancelled':
				return 'error';
			case 'WaitingChildren':
			case 'WaitingUser':
			case 'Paused':
				return 'warning';
			case 'Executing':
			case 'Runnable':
				return 'info';
			default:
				return 'default';
		}
	}

	function childActivity(child: ExecutionResponsibilityChild): string {
		const latestSummary = child.latest_summary?.summary?.trim();
		if (latestSummary) return latestSummary;
		if (child.current_stage) return `Stage: ${child.current_stage}`;
		return `State: ${child.waiting_state}`;
	}
</script>

<Card
	title="Responsibility"
	subtitle={snapshot?.responsibility_summary}
	className="execution-responsibility-card"
	elevation={0}
>
	{#if loading && !snapshot}
		<Stack direction="row" align="center" gap="var(--space-sm)">
			<Spinner size="md" label="Loading responsibility view" />
		</Stack>
	{:else if error && !snapshot}
		<Stack gap="var(--space-xs)">
			<Text children="Responsibility view is unavailable right now." />
			<Text variant="caption" children={error} />
		</Stack>
	{:else if snapshot}
		<Stack gap="var(--space-md)">
			<Stack direction="row" wrap gap="var(--space-xs)" className="responsibility-badges">
				<Badge text={`Owner: ${snapshot.active_owner_agent_id}`} color="info" />
				<Badge text={snapshot.waiting_state} color={statusColor(snapshot.waiting_state)} />
				{#if snapshot.handover_active}
					<Badge text="Handover active" color="warning" />
				{/if}
				{#if snapshot.waiting_on_children}
					<Badge text={`${snapshot.active_child_count} delegated child${snapshot.active_child_count === 1 ? '' : 'ren'}`} color="warning" />
				{/if}
			</Stack>

			<div class="responsibility-meta">
				<Text variant="overline" children="Owner chain" />
				<Stack direction="row" wrap gap="var(--space-xs)">
					{#each snapshot.owner_chain ?? [] as owner, index}
						<div class="owner-pill">
							<Text variant="code" children={owner} />
						</div>
						{#if index < (snapshot.owner_chain?.length ?? 0) - 1}
							<Text variant="caption" children="→" />
						{/if}
					{/each}
				</Stack>
			</div>

			{#if snapshot.current_stage || snapshot.current_provider || snapshot.paused_from_state}
				<Stack gap="var(--space-2xs)" className="responsibility-meta">
					<Text variant="overline" children="Current execution" />
					{#if snapshot.current_stage}
						<Text variant="caption" children={`Stage: ${snapshot.current_stage}`} />
					{/if}
					{#if snapshot.current_provider}
						<Text variant="caption" children={`Provider: ${snapshot.current_provider}`} />
					{/if}
					{#if snapshot.paused_from_state}
						<Text variant="caption" children={`Paused from: ${snapshot.paused_from_state}`} />
					{/if}
				</Stack>
			{/if}

			{#if (snapshot.active_children?.length ?? 0) > 0}
				<Stack gap="var(--space-sm)">
					<Text variant="overline" children="Parallel delegated work" />
					{#each snapshot.active_children ?? [] as child}
						<Card className="responsibility-child-card" elevation={0}>
							<Stack gap="var(--space-xs)">
								<Stack direction="row" justify="space-between" align="center" wrap gap="var(--space-xs)">
								<Stack gap="var(--space-2xs)">
										<Text children={child.title?.trim() ? child.title : child.execution_id} />
										<Text variant="caption" children={`Owner: ${child.active_owner_agent_id}`} />
									</Stack>
									<Stack direction="row" wrap gap="var(--space-xs)">
										{#if child.is_blocking}
											<Badge text="Blocking parent" color="warning" />
										{/if}
										<Badge text={child.waiting_state} color={statusColor(child.waiting_state)} />
									</Stack>
								</Stack>

								<Text variant="caption" children={childActivity(child)} />

								{#if (child.delegation_chain?.length ?? 0) > 0}
									<Stack gap="var(--space-2xs)">
										<Text variant="overline" children="Delegation chain" />
										<Stack direction="row" wrap gap="var(--space-xs)">
											{#each child.delegation_chain ?? [] as owner, index}
												<div class="owner-pill">
													<Text variant="code" children={owner} />
												</div>
												{#if index < (child.delegation_chain?.length ?? 0) - 1}
													<Text variant="caption" children="→" />
												{/if}
											{/each}
										</Stack>
									</Stack>
								{/if}
							</Stack>
						</Card>
					{/each}
				</Stack>
			{/if}
		</Stack>
	{/if}
</Card>

<style>

	.responsibility-meta {
		padding: var(--space-sm);
		border-radius: var(--radius-md);
		background: color-mix(in srgb, var(--bg-soft) 78%, transparent);
	}

	.owner-pill {
		display: inline-flex;
		align-items: center;
	}

</style>
