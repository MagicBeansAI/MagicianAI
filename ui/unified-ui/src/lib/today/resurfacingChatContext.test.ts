import { describe, expect, it } from 'vitest';

import type { ResurfacingDetail } from './resurfacingQueries';
import {
	buildResurfacingChatContext,
	canonicalResurfacingChatThread,
	resurfacingChatContextFilename
} from './resurfacingChatContext';

function detail(): ResurfacingDetail {
	return {
		candidate_id: 'cand-1',
		source_kind: 'comm',
		status: 'newer_available',
		title: 'Card / policy update',
		summary: 'The monthly cap changes to 5,000 points.',
		brief: {
			schema_version: 2,
			key_facts: ['Affected card: Platinum'],
			changes: [
				{
					aspect: 'Monthly cap',
					before: '10,000 points',
					after: '5,000 points',
					effective_text: 'August 1'
				}
			],
			temporal_facts: [
				{ kind: 'effective', text: 'August 1', at_ms: null, timezone: null }
			],
			detail_status: 'source_omits_details',
			missing_details: ['Whether existing points are affected']
		},
		content_revision: 'rev-1',
		source_revision: 'source-rev-2',
		source_updated: true,
		has_newer: true,
		source_route: null,
		open_url: null,
		source: null,
		recommended_action: null,
		actions: [],
		original: {
			kind: 'comm',
			message_id: 'message-1',
			subject: 'Policy update',
			summary: null,
			received_at: 0,
			body: 'SECRET RAW BODY MUST NOT LEAK',
			evidence_messages: []
		},
		temporal_anchor_at: null
	};
}

describe('Ask Presto resurfacing context', () => {
	it('includes the safe structured brief but never the live original', () => {
		const context = buildResurfacingChatContext(detail());
		expect(context).toContain('Candidate ID: cand-1');
		expect(context).toContain('Monthly cap: before 10,000 points; after 5,000 points');
		expect(context).toContain('Information not supplied: Whether existing points are affected');
		expect(context).toContain('newer content may exist');
		expect(context).not.toContain('SECRET RAW BODY MUST NOT LEAK');
	});

	it('creates a bounded filesystem-safe label', () => {
		expect(resurfacingChatContextFilename(detail())).toBe('Worth a look - Card policy update.txt');
	});

	it('selects an existing scoped owner chat and never invents a fallback', () => {
		const thread = (id: string, archived = false, display_mode: 'chat' | 'dev' = 'chat') => ({
			principal: 'owner',
			workspace: 'project',
			id,
			name: id,
			archived,
			sort_order: 0,
			created_at: 1,
			updated_at: 1,
			display_mode
		});
		expect(canonicalResurfacingChatThread([thread('dev', false, 'dev'), thread('owner-chat')])).toBe(
			'owner-chat'
		);
		expect(canonicalResurfacingChatThread([thread('archived', true)])).toBeNull();
		expect(canonicalResurfacingChatThread([])).toBeNull();
	});
});
