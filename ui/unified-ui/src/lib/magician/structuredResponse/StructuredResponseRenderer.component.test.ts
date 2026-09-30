import { cleanup, render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { StructuredResponseV1 } from './types';
import StructuredResponseRenderer from './StructuredResponseRenderer.svelte';

afterEach(() => {
	cleanup();
});

const blockShowcase: StructuredResponseV1 = {
	schema: 'magician.structured_response',
	version: 1,
	plain_text: 'Summary text',
	title: 'Execution result',
	summary: 'Completed successfully',
	tone: 'success',
	blocks: [
		{ kind: 'markdown', text: '### Headline', title: 'Summary' },
		{ kind: 'text', text: 'Raw text block' },
		{ kind: 'callout', tone: 'info', title: 'Info', text: 'Background context' },
		{
			kind: 'key_values',
			title: 'Details',
			items: [{ label: 'Mode', value: 'safe', hint: 'no rewrite' }]
		},
		{
			kind: 'table',
			title: 'Scores',
			columns: [
				{ key: 'metric', label: 'Metric' },
				{ key: 'value', label: 'Value' }
			],
			rows: [{ metric: 'Accuracy', value: '99' }]
		},
		{
			kind: 'list',
			style: 'checks',
			title: 'Checklist',
			items: [
				{ text: 'Start', checked: true },
				{ text: 'Finish', checked: false, detail: 'review logs' }
			]
		},
		{
			kind: 'artifacts',
			title: 'Artifacts',
			items: [{ label: 'report', href: 'https://example.com/api/reports/1', mime_type: 'text/csv', size: 321 }]
		},
		{
			kind: 'sources',
			title: 'Sources',
			items: [{ label: 'Docs', href: 'https://example.com' }]
		},
		{
			kind: 'metrics',
			title: 'Metrics',
			items: [{ label: 'Latency', value: '102', unit: 'ms', trend: 'down' }]
		}
	],
	actions: [
		{ kind: 'copy_text', label: 'Copy result', text: 'done' },
		{ kind: 'open_url', label: 'Open link', url: 'https://example.com' }
	]
};

describe('StructuredResponseRenderer', () => {
	it('renders all block kinds and inert actions in one pass', () => {
		render(StructuredResponseRenderer, { response: blockShowcase });

		expect(screen.getByTestId('sr-renderer')).toBeInTheDocument();
		expect(screen.getByText('Execution result')).toBeInTheDocument();
		expect(screen.getByText('Completed successfully')).toBeInTheDocument();
		expect(screen.getByText('Headline')).toBeInTheDocument();
		expect(screen.getByText('Raw text block')).toBeInTheDocument();
		expect(screen.getByText('Background context')).toBeInTheDocument();
		expect(screen.getByText('Mode')).toBeInTheDocument();
		expect(screen.getByText('Scores')).toBeInTheDocument();
		expect(screen.getByText('Accuracy')).toBeInTheDocument();
		expect(screen.getByText('Checklist')).toBeInTheDocument();
		expect(screen.getByText('Artifacts')).toBeInTheDocument();
		expect(screen.getByText('Sources')).toBeInTheDocument();
		expect(screen.getByText('Metrics')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Copy result' })).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Open link' })).toBeDisabled();
	});

	it('invokes the action handler when an action is click-activated', async () => {
		const onAction = vi.fn();
		render(StructuredResponseRenderer, {
			response: blockShowcase,
			onAction
		});

		await fireEvent.click(screen.getByRole('button', { name: 'Copy result' }));

		expect(onAction).toHaveBeenCalledTimes(1);
		expect(onAction).toHaveBeenCalledWith(
			blockShowcase.actions?.[0],
			null
		);
	});

	it('fails closed for deprecated server actions', async () => {
		render(StructuredResponseRenderer, {
			response: {
				...blockShowcase,
				actions: [{ kind: 'invoke_server_action', label: 'Run', action_ref: 'do.not' }]
			}
		});

		expect(screen.getByTestId('sr-fallback')).toBeInTheDocument();
	});

	it('keeps artifact actions active and passes them through to handler', async () => {
		const onAction = vi.fn();
		render(StructuredResponseRenderer, {
			response: {
				...blockShowcase,
				actions: [{ kind: 'open_artifact', label: 'Open artifact', artifact_id: 'artifact-1' }]
			},
			onAction
		});

		await fireEvent.click(screen.getByRole('button', { name: 'Open artifact' }));

		expect(onAction).toHaveBeenCalledTimes(1);
		expect(onAction).toHaveBeenCalledWith(
			{ kind: 'open_artifact', label: 'Open artifact', artifact_id: 'artifact-1' },
			null
		);
	});

	it('falls back when schema is unsupported', () => {
		render(StructuredResponseRenderer, {
			response: {
				...blockShowcase,
				schema: 'legacy.response' as StructuredResponseV1['schema']
			}
		});

		expect(screen.getByTestId('sr-fallback')).toBeInTheDocument();
		expect(screen.getAllByText(/Unable to render this structured response/)).toHaveLength(2);
		expect(screen.getAllByText(/schema must be magician.structured_response/)).toHaveLength(2);
	});

	it('falls back when encountering an unsupported block kind', () => {
		render(StructuredResponseRenderer, {
			response: {
				...blockShowcase,
				blocks: [
					{ kind: 'text', text: 'good' },
					{ kind: 'widget' as any, label: 'x' }
				] as unknown as StructuredResponseV1['blocks']
			}
		});

		expect(screen.getByTestId('sr-fallback')).toBeInTheDocument();
		expect(screen.getAllByText(/unsupported: widget/)).toHaveLength(2);
	});
});
