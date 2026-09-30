import { describe, expect, it, vi } from 'vitest';

import type { ResurfacingCard, ResurfacingDetail } from './resurfacingQueries';
import {
	buildResurfacingActionInput,
	defaultResurfacingActionDraft,
	mergeResurfacingCapabilities,
	resurfacingHasMissingDetails,
	resurfacingPrimaryAction,
	resurfacingRowFacts
} from './resurfacingPresentation';

function card(overrides: Partial<ResurfacingCard> = {}): ResurfacingCard {
	return {
		candidate_id: 'cand-1',
		line: 'Policy update',
		why_now: 'Effective soon',
		source_title: 'Card policy update',
		summary: 'The monthly reward cap changes to 5,000 points.',
		source_kind: 'comm',
		source_ref: 'comm:1',
		detail_label: 'Summary',
		temporal_anchor_at: null,
		brief: null,
		brief_status: 'legacy',
		content_revision: 'rev-1',
		source_updated: false,
		recommended_action: null,
		actions: [],
		...overrides
	};
}

describe('resurfacing row presentation', () => {
	it('prioritizes concrete changes, then dates, then general facts', () => {
		const facts = resurfacingRowFacts({
			schema_version: 2,
			key_facts: ['Affected cards: Platinum'],
			changes: [
				{
					aspect: 'Monthly reward cap',
					before: '10,000 points',
					after: '5,000 points',
					effective_text: 'August 1'
				}
			],
			temporal_facts: [
				{ kind: 'effective', text: 'August 1', at_ms: null, timezone: null }
			],
			detail_status: 'complete',
			missing_details: []
		});

		expect(facts).toEqual([
			{
				id: 'change-0',
				kind: 'change',
				label: 'Monthly reward cap',
				value: '10,000 points to 5,000 points · August 1'
			},
			{ id: 'date-0', kind: 'date', label: 'Effective', value: 'August 1' },
			{ id: 'fact-0', kind: 'fact', label: 'Fact', value: 'Affected cards: Platinum' }
		]);
	});

	it('recognizes explicit information gaps', () => {
		expect(
			resurfacingHasMissingDetails({
				schema_version: 2,
				key_facts: [],
				changes: [],
				temporal_facts: [],
				detail_status: 'source_omits_details',
				missing_details: ['The changed fee']
			})
		).toBe(true);
		expect(resurfacingHasMissingDetails(null)).toBe(false);
	});
});

describe('resurfacing action presentation', () => {
	it('falls back to Details for old payloads and rejects unavailable recommendations', () => {
		const legacy = card({
			recommended_action: {
				kind: 'share',
				label: 'Send this to Accounts',
				rationale: 'They should know.',
				confidence: 0.9,
				content_revision: 'rev-1',
				source: 'curator'
			}
		});
		expect(mergeResurfacingCapabilities(legacy).map((action) => action.kind)).toEqual([
			'view_details'
		]);
		expect(resurfacingPrimaryAction(legacy)).toMatchObject({
			kind: 'view_details',
			label: 'Details',
			isRecommendation: false
		});
	});

	it('uses a current, supported content-specific recommendation', () => {
		const rich = card({
			recommended_action: {
				kind: 'create_reminder',
				label: 'Remind me before August 1',
				rationale: 'The cap changes on a known date.',
				confidence: 0.88,
				content_revision: 'rev-1',
				source: 'curator'
			},
			actions: [
				{
					kind: 'create_reminder',
					label: 'Create reminder',
					requires_input: true,
					side_effect: 'creates_reminder'
				}
			]
		});
		expect(resurfacingPrimaryAction(rich)).toEqual({
			kind: 'create_reminder',
			label: 'Remind me before August 1',
			rationale: 'The cap changes on a known date.',
			contentRevision: 'rev-1',
			isRecommendation: true
		});
	});

	it('lets current detail capabilities supersede list capabilities', () => {
		const item = card({
			actions: [
				{ kind: 'open_source', label: 'Open', requires_input: false, side_effect: 'none' }
			]
		});
		const detail = {
			actions: [
				{ kind: 'show_original', label: 'Original', requires_input: false, side_effect: 'none' }
			]
		} as unknown as ResurfacingDetail;
		expect(mergeResurfacingCapabilities(item, detail).map((action) => action.kind)).toEqual([
			'show_original'
		]);
	});

	it('treats empty loaded detail actions and recommendation as authoritative', () => {
		const item = card({
			actions: [{ kind: 'open_source', label: 'Open', requires_input: false, side_effect: 'none' }],
			recommended_action: {
				kind: 'open_source',
				label: 'Open now',
				rationale: 'Review the source.',
				confidence: 0.9,
				content_revision: 'rev-1',
				source: 'curator'
			}
		});
		const detail = {
			content_revision: null,
			actions: [],
			recommended_action: null
		} as unknown as ResurfacingDetail;
		expect(mergeResurfacingCapabilities(item, detail)).toEqual([]);
		expect(resurfacingPrimaryAction(item, detail)).toMatchObject({
			kind: 'view_details',
			contentRevision: null,
			isRecommendation: false
		});
	});
});

describe('resurfacing action dialog inputs', () => {
	it('prefills a future unambiguous temporal fact and builds a once reminder', () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date('2026-07-12T06:00:00.000Z'));
		const at = new Date('2026-07-15T06:30:00.000Z').getTime();
		const item = card({
			brief: {
				schema_version: 2,
				key_facts: [],
				changes: [],
				temporal_facts: [{ kind: 'deadline', text: 'July 15', at_ms: at, timezone: 'UTC' }],
				detail_status: 'complete',
				missing_details: []
			}
		});
		const draft = defaultResurfacingActionDraft(item, null, 'Asia/Kolkata');
		expect(draft.atLocal).not.toBe('');
		const result = buildResurfacingActionInput('create_reminder', draft, 'owner-chat');
		expect(result).toMatchObject({
			ok: true,
			input: {
				delivery: 'host_apple_reminders',
				timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
				ui_thread_id: 'owner-chat'
			}
		});
		vi.useRealTimers();
	});

	it('keeps invalid dialogs local and never guesses a share recipient', () => {
		const draft = defaultResurfacingActionDraft(card(), null, 'UTC');
		expect(buildResurfacingActionInput('share', draft, 'general')).toEqual({
			ok: false,
			error: 'Recipient is required.'
		});
		draft.fact = '   ';
		expect(buildResurfacingActionInput('save_to_memory', draft, 'general')).toEqual({
			ok: false,
			error: 'Fact is required.'
		});
	});
});
