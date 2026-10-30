import { cleanup, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { HitlRequest } from '$lib/hitl/types';
import type { VibeRow } from '$lib/stores/vibeHitlStore';
import StageDiff from './StageDiff.svelte';

function row(request: HitlRequest): VibeRow {
	return {
		key: request.schema.proposal_id ?? request.id,
		entry: { correlation_id: request.id, at: Date.now(), raw: request.raw ?? {} },
		request
	};
}

/** The run-ended owner briefing an Autopilot run files: Acknowledge is its only answer. */
const briefing = row({
	id: 'vibedev-run-ended:task_1:exec_1',
	source: 'user_request',
	input_type: 'choice',
	schema: {
		request_type: 'harness.notify_owner.briefing',
		options: [{ id: 'acknowledge', label: 'Acknowledge' }]
	},
	prompt:
		'Owner briefing: Autopilot run finished: Pi 1.3.0 live test\n\nAutopilot run finished: Pi 1.3.0 live test. Open it to review the result.',
	scope: { task_id: 'task_1' },
	identifiers: { request_id: 'vibedev-run-ended:task_1:exec_1' }
});

const question = row({
	id: 'ask_1',
	source: 'user_request',
	input_type: 'choice',
	schema: {
		request_type: 'user_input',
		options: [
			{ id: 'postgres', label: 'Postgres' },
			{ id: 'sqlite', label: 'SQLite' }
		]
	},
	prompt: 'Which database should the API use?',
	scope: { task_id: 'task_1' },
	identifiers: { request_id: 'ask_1' }
});

function proposal(id: string, historical: boolean): VibeRow {
	return row({
		id,
		source: 'diff_approval',
		input_type: 'diff_approval',
		schema: {
			proposal_id: id,
			approval_source: 'proposal',
			rationale: 'Bump the Pi engine',
			files: [{ path: 'package.json', status: 'M', additions: 1, deletions: 1, unified_diff: '' }]
		},
		prompt: 'Bump the Pi engine',
		scope: { task_id: 'task_1' },
		identifiers: { correlation_id: id },
		raw: historical ? { historical: true, status: 'applied' } : {}
	});
}

afterEach(() => cleanup());

describe('StageDiff', () => {
	it('shows a briefing as a notice to acknowledge, with no diff approve buttons', async () => {
		const user = userEvent.setup();
		const acknowledge = vi.fn();
		render(StageDiff, {
			props: { orderedDiffRows: [], orderedOtherRows: [briefing] },
			events: {
				acknowledge: (event: CustomEvent<{ row: VibeRow }>) => acknowledge(event.detail.row)
			}
		});

		expect(
			screen.getByText('Autopilot run finished: Pi 1.3.0 live test. Open it to review the result.')
		).toBeInTheDocument();
		expect(screen.getByText('1 notice')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Apply all changes' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Apply' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Respond' })).not.toBeInTheDocument();
		expect(screen.queryByText('Other decisions')).not.toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Acknowledge' }));
		expect(acknowledge).toHaveBeenCalledWith(briefing);
	});

	it('keeps a real question under decisions with Respond', () => {
		render(StageDiff, { orderedDiffRows: [], orderedOtherRows: [briefing, question] });
		expect(screen.getByText('Other decisions')).toBeInTheDocument();
		expect(screen.getByText('Which database should the API use?')).toBeInTheDocument();
		expect(screen.getAllByRole('button', { name: 'Respond' })).toHaveLength(1);
		expect(screen.getAllByRole('button', { name: 'Acknowledge' })).toHaveLength(1);
		expect(screen.getByText('1 decision · 1 notice')).toBeInTheDocument();
	});

	it('shows an already-applied proposal as history, never as a disabled review', () => {
		render(StageDiff, { orderedDiffRows: [proposal('ccp-1', true)], orderedOtherRows: [briefing] });
		expect(screen.getByText('Applied')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Apply' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Apply all changes' })).not.toBeInTheDocument();
	});

	it('offers Apply, Reject and Apply all only for a pending proposal', () => {
		render(StageDiff, { orderedDiffRows: [proposal('ccp-2', false)], orderedOtherRows: [] });
		expect(screen.getByRole('button', { name: 'Apply' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Reject' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Apply all changes' })).toBeEnabled();
		expect(screen.getByText('1 change set')).toBeInTheDocument();
	});
});
