<script lang="ts">
	import type {
		StructuredArtifactsBlockV1,
		StructuredArtifactRefV1,
		StructuredResponseActionV1
	} from '../types';

	export let block: StructuredArtifactsBlockV1;
	export let onAction: ((action: StructuredResponseActionV1) => void | Promise<void>) | null = null;

	const artifactAction = (artifact: StructuredArtifactRefV1): StructuredResponseActionV1 | null => {
		if (artifact.href) {
			return { kind: 'open_url', label: artifact.label, url: artifact.href };
		}
		if (artifact.artifact_id) {
			return { kind: 'open_artifact', label: artifact.label, artifact_id: artifact.artifact_id };
		}
		return null;
	};
</script>

<section class="sr-block sr-block--artifacts" data-block-kind="artifacts">
	{#if block.title}<h4 class="sr-block__title">{block.title}</h4>{/if}
	<ul class="sr-artifacts">
		{#each block.items as artifact}
			{@const action = artifactAction(artifact)}
			<li>
				{#if action}
					<button
						type="button"
						class="sr-link"
						disabled={!onAction}
						on:click={() => onAction && void onAction(action)}
					>
						{artifact.label}
					</button>
				{:else}
					<span>{artifact.label}</span>
				{/if}
				{#if artifact.size}
					<span class="sr-artifacts__meta">{artifact.size.toLocaleString()} B</span>
				{/if}
				{#if artifact.mime_type}
					<span class="sr-artifacts__meta">{artifact.mime_type}</span>
				{/if}
			</li>
		{/each}
	</ul>
</section>

<style>
	.sr-block {
		display: block;
	}

	.sr-block__title {
		font-size: var(--text-sm);
		font-weight: 650;
		margin: 0 0 0.35rem;
	}

	.sr-artifacts {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.sr-artifacts li {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem 0.55rem;
		padding: 0.25rem 0;
		line-height: 1.35;
	}

	.sr-link {
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--link);
		text-underline-offset: 2px;
		cursor: pointer;
	}

	.sr-link:disabled {
		color: inherit;
		cursor: default;
	}

	.sr-artifacts__meta {
		color: var(--text-muted);
		font-size: var(--text-xs);
	}
</style>
