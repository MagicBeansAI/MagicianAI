<script lang="ts">
	import type { StructuredListBlockV1 } from '../types';

	export let block: StructuredListBlockV1;

	const orderedType = block.style === 'checks' ? 'none' : block.style === 'steps' ? 'decimal' : 'disc';
</script>

<section class="sr-block sr-block--list" data-block-kind="list">
	{#if block.title}<h4 class="sr-block__title">{block.title}</h4>{/if}
	{#if block.style === 'checks'}
		<ul class="sr-list sr-list--checks">
			{#each block.items as item}
				<li>
					<span
						class={`sr-list__check ${item.checked ? 'sr-list__check--on' : 'sr-list__check--off'}`}
						aria-hidden="true"
					>
						{item.checked ? '✓' : '◻'}
					</span>
					<span class="sr-list__item">
						<span>{item.text}</span>
						{#if item.detail}
							<span class="sr-list__detail">{item.detail}</span>
						{/if}
					</span>
				</li>
			{/each}
		</ul>
	{:else if orderedType === 'decimal'}
		<ol class="sr-list sr-list--steps">
			{#each block.items as item}
				<li>
					<span>{item.text}</span>
					{#if item.detail}
						<span class="sr-list__detail">{item.detail}</span>
					{/if}
				</li>
			{/each}
		</ol>
	{:else}
		<ul class="sr-list sr-list--bullets">
			{#each block.items as item}
				<li>
					<span>{item.text}</span>
					{#if item.detail}
						<span class="sr-list__detail">{item.detail}</span>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
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

	.sr-list {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.sr-list li {
		display: grid;
		grid-template-columns: auto 1fr;
		column-gap: 0.4rem;
		align-items: start;
		margin: 0 0 0.4rem;
	}

	.sr-list--steps {
		counter-reset: steps-counter;
	}

	.sr-list--checks {
		counter-reset: none;
	}

	.sr-list__check {
		font-size: 0.78rem;
		line-height: 1.35;
		color: var(--text-muted);
	}

	.sr-list__check--on {
		color: rgb(16, 185, 129);
	}

	.sr-list__item,
	.sr-list li span:first-child {
		line-height: 1.35;
		font-size: var(--text-sm);
	}

	.sr-list--bullets {
		list-style: disc;
		padding-left: 1.2rem;
	}

	.sr-list--bullets li {
		grid-template-columns: 1fr;
	}

	.sr-list--steps li::before {
		content: counter(steps-counter, decimal) '.';
		counter-increment: steps-counter;
		font-weight: 700;
		color: var(--text-muted);
		min-width: 1.2rem;
	}

	.sr-list__detail {
		display: block;
		color: var(--text-muted);
		font-size: var(--text-xs);
		margin-top: 0.2rem;
	}
</style>
