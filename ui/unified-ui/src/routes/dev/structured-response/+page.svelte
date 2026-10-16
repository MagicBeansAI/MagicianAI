<script lang="ts">
	import type { StructuredResponseV1 } from '$lib/magician/structuredResponse/types';
	import StructuredResponseRenderer from '$lib/magician/structuredResponse/StructuredResponseRenderer.svelte';

	const supported: StructuredResponseV1 = {
		schema: 'magician.structured_response',
		version: 1,
		plain_text: 'Completed task output',
		title: 'Structured response fixture',
		summary: 'This fixture validates v1 block rendering in isolation.',
		tone: 'info',
		blocks: [
			{ kind: 'callout', tone: 'info', title: 'Summary', text: 'All blocks below should render.' },
			{ kind: 'text', text: 'This text block uses safe plain rendering.' },
			{ kind: 'markdown', title: 'Markdown', text: 'A tiny markdown list:\n\n- one\n- two' },
			{ kind: 'list', style: 'checks', title: 'Checklist', items: [{ text: 'Build blocks', checked: true }, { text: 'Validate', checked: false }] },
			{ kind: 'metrics', title: 'Metrics', items: [{ label: 'Latency', value: '124', unit: 'ms', trend: 'down' }] }
		],
		actions: [
			{ kind: 'copy_text', label: 'Copy summary', text: 'Completed task output' }
		]
	};

	const fallback: StructuredResponseV1 = {
		schema: 'magician.structured_response',
		version: 1,
		plain_text: 'Fallback fixture',
		title: 'Structured response fallback fixture',
		blocks: [
			{ kind: 'text', text: 'fallback-only-line' },
			{ kind: 'table' as const, columns: [], rows: [] }
		] as StructuredResponseV1['blocks']
	};
</script>

<main class="sr-page">
	<h1>Structured Response Fixtures</h1>
	<section>
		<h2>Supported response</h2>
		<StructuredResponseRenderer response={supported} />
	</section>
	<section>
		<h2>Unsupported response fallback</h2>
		<StructuredResponseRenderer response={fallback} />
	</section>
</main>

<style>
	.sr-page {
		padding: 1rem;
		display: grid;
		gap: 1rem;
	}
	
	section {
		display: grid;
		gap: 0.45rem;
	}
</style>
