import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import AttentionInboxSurface from './AttentionInboxSurface.svelte';
import type { AttentionDisplayRow } from './model';

describe('AttentionInboxSurface', () => {
	it('renders terminal and special-action rows through the shared row markup', () => {
		const rows: AttentionDisplayRow[] = [
			{
				key: 'failed-1',
				source: 'agentic',
				prompt: 'Execution failed',
				scope: { task_id: 'task-123456789' },
				at: 1,
				correlation_id: 'failed-1',
				origin: 'feed',
				request: null,
				failed: true,
				feed_item_id: 'v3:attention:failed-1'
			},
			{
				key: 'skill-1',
				source: 'approval',
				prompt: 'Review skill proposal',
				scope: {},
				at: 2,
				correlation_id: 'skill-1',
				origin: 'feed',
				request: null,
				skillEvolution: {
					gate: 'proposal_review',
					action: 'approve',
					actionEnabled: true,
					candidateId: 'candidate-1'
				}
			}
		];

		const { body } = render(AttentionInboxSurface, {
			props: { rows, showSearch: false, showFilters: false }
		});

		expect(body).toContain('Execution failed');
		expect(body).toContain('Dismiss');
		expect(body).toContain('Review skill proposal');
		expect(body).toContain('Approve');
		expect(body).toContain('Reject');
		expect(body).toContain('--attention-row-color:');
		expect(body).not.toMatch(/#[0-9a-f]{6}(20|40)/i);
	});

	it('announces notices as status and errors as alerts', () => {
		const { body } = render(AttentionInboxSurface, {
			props: {
				rows: [],
				feedback: [
					{
						message: 'Feedback recorded; 12 related candidates re-scored.',
						action: { label: 'View affected candidates', href: '/attention?outcome=outcome-1' }
					},
					{ kind: 'error', message: 'Attention page failed to load.' }
				]
			}
		});

		expect(body).toContain('role="status"');
		expect(body).toContain('role="alert"');
		expect(body).toContain('Feedback recorded; 12 related candidates re-scored.');
		expect(body).toContain('View affected candidates');
		expect(body).toContain('/attention?outcome=outcome-1');
		expect(body).toContain('Attention page failed to load.');
	});
});
