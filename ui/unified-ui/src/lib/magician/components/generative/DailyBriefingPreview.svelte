<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Button from './Button.svelte';
	import Card from './Card.svelte';
	import Form from './Form.svelte';
	import Grid from './Grid.svelte';
	import Text from './Text.svelte';

	interface ScrollWidget {
		id: string;
		title: string;
		headline: string;
		items: readonly string[];
		footer: string;
	}

	export let showHeader: boolean = false;

	const dispatch = createEventDispatcher<{ addWidget: { action: 'addWidget' } }>();

	const MOCK_WIDGETS: readonly ScrollWidget[] = [
		{
			id: 'messages',
			title: 'Messages from Tripti',
			headline: 'LinkedIn: "Hey, did you see the latest designs?"',
			items: ['Gmail: "Re: Project Alpha - Attaching the deck."', 'Instagram: "OMG that photo!."'],
			footer: 'Last run: 2 mins ago'
		},
		{
			id: 'ai-news',
			title: 'Latest AI News',
			headline: 'New model release changes everything',
			items: ['OpenAI Dev Day key takeaways', 'Automated personal scrolls'],
			footer: 'Last run: 15 mins ago'
		}
	];

	function handleAddWidget(): void {
		dispatch('addWidget', { action: 'addWidget' });
	}
</script>

<div class="daily-briefing-preview">
	{#if showHeader}
		<div class="daily-briefing-header">
			<h2>Your Scroll</h2>
			<p>What your Doubles handled while you were living your life</p>
		</div>
	{/if}

	<div class="daily-briefing-shell">
		<div class="daily-briefing-gaui">
			<Grid columns={3} gap="0.875rem">
				{#each MOCK_WIDGETS as widget (widget.id)}
					<Card title={widget.title} body={widget.headline}>
						{#each widget.items as line (line)}
							<Text variant="caption" children={line} />
						{/each}
						<Text variant="caption" children={widget.footer} />
					</Card>
				{/each}
				<Card>
					<Button
						label="+ Add Task to Briefing"
						variant="primary"
						size="sm"
						interactive={true}
						on:click={handleAddWidget}
					/>
				</Card>
			</Grid>

			<Form
				idBase="presto-daily-briefing-contract"
				disabled={true}
				showSubmit={false}
				fields={[]}
			/>
		</div>

		<div class="daily-briefing-overlay" aria-hidden="true">
			<span class="coming-soon-badge">COMING SOON</span>
			<h3>Automated Personal Scrolls</h3>
		</div>
	</div>
</div>

<style>
	.daily-briefing-preview {
		width: min(100%, 1080px);
		margin: 0 auto;
	}

	.daily-briefing-header {
		margin-bottom: 0.75rem;
	}

	.daily-briefing-header h2 {
		margin: 0;
		font-size: 1.9rem;
		line-height: 1.15;
	}

	.daily-briefing-header p {
		margin: 0.4rem 0 0;
		color: var(--muted-text, #7f7872);
		font-size: 1rem;
		line-height: 1.4;
	}

	.daily-briefing-shell {
		position: relative;
		padding: 1rem;
		border: 1px solid color-mix(in srgb, var(--ui-border, #ddd6cf) 80%, transparent);
		border-radius: 1.25rem;
		background: color-mix(in srgb, var(--ui-bg, #ffffff) 92%, transparent);
		overflow: hidden;
	}

	.daily-briefing-gaui {
		filter: blur(1.2px);
		opacity: 0.6;
	}

	.daily-briefing-overlay {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.55rem;
		pointer-events: none;
		text-align: center;
	}

	.coming-soon-badge {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0.35rem 0.85rem;
		border-radius: 999px;
		border: 1px solid color-mix(in srgb, var(--accent, #d66a5f) 70%, transparent);
		background: color-mix(in srgb, var(--accent, #d66a5f) 10%, transparent);
		color: color-mix(in srgb, var(--accent, #d66a5f) 88%, #000);
		font-size: 0.78rem;
		font-weight: 600;
		letter-spacing: 0.08em;
	}

	.daily-briefing-overlay h3 {
		margin: 0;
		font-size: 1.45rem;
		line-height: 1.2;
		font-weight: 600;
		color: var(--text-primary, #2d2b29);
	}

	.daily-briefing-shell :global(form) {
		display: none;
	}

	.daily-briefing-shell :global(.muij-grid) {
		align-items: stretch;
	}

	.daily-briefing-shell :global(.muij-card) {
		min-height: 11.5rem;
	}
</style>
