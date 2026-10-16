import { describe, expect, it } from 'vitest';

import type {
	FleetSectionAvailability,
	FleetStateAvailability,
	FleetStateAttention,
	FleetStateCitizen,
	FleetStateDelivery,
	FleetStateHandoff,
	FleetStateSocialPost,
	FleetStateSnapshot
} from '../fleetState';
import {
	diffOfficeEvents,
	socialRootPostId,
	socialVenueOf,
	type OfficeEvent
} from './officeEvents';

const available: FleetSectionAvailability = { status: 'available', sources: [], limitations: [] };
const unavailable: FleetSectionAvailability = {
	status: 'unavailable',
	sources: [],
	limitations: ['fixture']
};

const citizen = (id: string, work: FleetStateCitizen['current_work'] = []): FleetStateCitizen => ({
	citizen_id: id,
	display_name: id,
	aliases: [],
	role: 'personal',
	description: id,
	version: 1,
	disabled: false,
	is_primary: id === 'presto',
	program_refs: ['commons'],
	current_work: work
});

const work = (
	questId: string,
	status = 'running',
	extra: Partial<FleetStateCitizen['current_work'][number]> = {}
): FleetStateCitizen['current_work'][number] => ({
	quest_id: questId,
	title: questId,
	status,
	execution_id: extra.execution_id ?? `exec-${questId}`,
	current_step: extra.current_step ?? null,
	current_substep: extra.current_substep ?? null,
	is_blocked: extra.is_blocked ?? false,
	updated_at: extra.updated_at ?? '2026-08-15T10:00:00Z'
});

const handoff = (
	from: string,
	to: string,
	extra: Partial<FleetStateHandoff> = {}
): FleetStateHandoff => ({
	quest_id: extra.quest_id ?? 'task-handoff',
	parent_execution_id: extra.parent_execution_id ?? 'exec-parent',
	child_execution_id: extra.child_execution_id ?? 'exec-child',
	parent_step_id: extra.parent_step_id ?? 'step-1',
	from: { citizen_id: from, display_name: from },
	to: { citizen_id: to, display_name: to },
	reason: extra.reason ?? 'delegate',
	status: extra.status ?? 'running',
	active: extra.active ?? true,
	outcome_type: extra.outcome_type ?? null,
	requested_at: extra.requested_at ?? '2026-08-15T09:00:00Z',
	updated_at: extra.updated_at ?? '2026-08-15T10:00:00Z'
});

const delivery = (
	id: string,
	createdAt: number,
	citizenId: string | null = 'cto'
): FleetStateDelivery => ({
	id,
	kind: 'artifact',
	title: id,
	summary: null,
	status: 'ready',
	task_id: null,
	citizen_id: citizenId,
	created_at: createdAt,
	updated_at: createdAt,
	metadata: null
});

const attention = (id: string, citizenId: string): FleetStateAttention => ({
	id,
	kind: 'approval',
	title: id,
	summary: null,
	task_id: null,
	citizen_id: citizenId,
	actions: [],
	untyped_action_count: 0,
	created_at: 1,
	updated_at: 1
});

// `availability` is widened to a Partial on purpose. Every caller below patches
// ONE section — that is the whole point of the helper — and
// `Partial<FleetStateSnapshot>` types the field as the complete
// `FleetStateAvailability`, so `{ social: unavailable }` is a type error while
// being exactly what the test means. The body already merges the patch over a
// full set of defaults, so the value handed back is complete either way.
function snapshot(
	partial: Partial<Omit<FleetStateSnapshot, 'availability'>> & {
		availability?: Partial<FleetStateAvailability>;
	} = {}
): FleetStateSnapshot {
	const { availability: availabilityPatch, ...rest } = partial;
	return {
		schema_version: 'fleet_state.v1alpha1',
		generated_at: '2026-08-15T10:00:00Z',
		scope: { principal: 'anonymous', workspace: 'default' },
		citizens: [citizen('cto'), citizen('frontend-engineer'), citizen('presto')],
		guilds: [],
		quests: [],
		attention: [],
		handoffs: [],
		deliveries: [],
		economy: null,
		ambient_social_activity: [],
		...rest,
		availability: {
			citizens: available,
			guilds: available,
			quests: available,
			attention: available,
			handoffs: available,
			deliveries: available,
			economy: available,
			social: available,
			...(availabilityPatch ?? {})
		}
	};
}

const typesOf = (events: OfficeEvent[]): string[] => events.map((e) => e.type).sort();

describe('diffOfficeEvents', () => {
	it('emits nothing on the first poll after mount', () => {
		const next = snapshot({
			citizens: [citizen('cto', [work('task-1')])],
			handoffs: [handoff('cto', 'frontend-engineer')],
			deliveries: [delivery('d1', 1_700_000_000_000)],
			attention: [attention('a1', 'cto')]
		});
		expect(diffOfficeEvents(null, next)).toEqual([]);
	});

	it('emits nothing for a re-ordered identical roster', () => {
		const prev = snapshot({
			citizens: [citizen('cto', [work('task-1')]), citizen('presto'), citizen('frontend-engineer')]
		});
		const next = snapshot({
			citizens: [citizen('frontend-engineer'), citizen('presto'), citizen('cto', [work('task-1')])],
			generated_at: '2026-08-15T10:00:30Z'
		});
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});

	it('emits nothing for a re-derived identical snapshot', () => {
		const prev = snapshot({
			citizens: [citizen('cto', [work('task-1')])],
			handoffs: [handoff('cto', 'frontend-engineer')],
			deliveries: [delivery('d1', 1_700_000_000_000)],
			attention: [attention('a1', 'cto')]
		});
		const next = structuredClone(prev);
		next.generated_at = '2026-08-15T10:00:30Z';
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});

	it('emits nothing from an unavailable section', () => {
		const prev = snapshot();
		const next = snapshot({
			availability: {
				citizens: unavailable,
				guilds: unavailable,
				quests: unavailable,
				attention: unavailable,
				handoffs: unavailable,
				deliveries: unavailable,
				economy: unavailable
			},
			citizens: [citizen('cto', [work('new-task')])],
			handoffs: [handoff('cto', 'frontend-engineer')],
			deliveries: [delivery('d-new', 1_700_000_000_100)],
			attention: [attention('block-1', 'cto')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});

	it('emits delivery-landed only for a created_at newer than the last seen', () => {
		const prev = snapshot({
			deliveries: [delivery('old', 1_700_000_000_000, 'cto')]
		});
		const next = snapshot({
			deliveries: [
				delivery('old', 1_700_000_000_000, 'cto'),
				delivery('new', 1_700_000_000_500, 'cto')
			]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([
			{
				type: 'delivery-landed',
				id: 'new',
				citizenId: 'cto',
				createdAt: 1_700_000_000_500,
				title: 'new'
			}
		]);
	});

	it('emits handoff only when the id is new and both ends are seated crew', () => {
		const prev = snapshot();
		const seated = snapshot({
			handoffs: [handoff('cto', 'frontend-engineer')]
		});
		expect(diffOfficeEvents(prev, seated)).toEqual([
			{
				type: 'handoff',
				id: 'task-handoff:exec-parent:exec-child',
				fromId: 'cto',
				toId: 'frontend-engineer',
				questId: 'task-handoff'
			}
		]);
		const stranger = snapshot({
			handoffs: [handoff('cto', 'ghost-agent')]
		});
		expect(diffOfficeEvents(prev, stranger)).toEqual([]);
	});

	it('emits task-routed when a citizen gains work no one held before', () => {
		const prev = snapshot({
			citizens: [citizen('cto'), citizen('frontend-engineer')]
		});
		const next = snapshot({
			citizens: [citizen('cto', [work('brand-new', 'ready')]), citizen('frontend-engineer')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([
			{
				type: 'task-routed',
				citizenId: 'cto',
				questId: 'brand-new',
				title: 'brand-new'
			}
		]);
	});

	it('does not emit task-routed when work only moved between citizens', () => {
		const prev = snapshot({
			citizens: [citizen('cto', [work('shared')]), citizen('frontend-engineer')]
		});
		const next = snapshot({
			citizens: [citizen('cto'), citizen('frontend-engineer', [work('shared')])]
		});
		expect(typesOf(diffOfficeEvents(prev, next))).not.toContain('task-routed');
	});

	it('emits blocked and unblocked from attention appearing and disappearing', () => {
		const idle = snapshot();
		const blocked = snapshot({
			attention: [attention('need-1', 'cto')]
		});
		expect(diffOfficeEvents(idle, blocked)).toEqual([
			{ type: 'blocked', citizenId: 'cto', attentionId: 'need-1' }
		]);
		expect(diffOfficeEvents(blocked, idle)).toEqual([
			{ type: 'unblocked', citizenId: 'cto', attentionId: 'need-1' }
		]);
	});

	it('emits work-started and work-ended when vibe crosses working', () => {
		const idle = snapshot({
			citizens: [citizen('cto'), citizen('frontend-engineer')]
		});
		const working = snapshot({
			citizens: [citizen('cto', [work('live', 'running')]), citizen('frontend-engineer')]
		});
		expect(diffOfficeEvents(idle, working)).toEqual(
			expect.arrayContaining([
				{ type: 'work-started', citizenId: 'cto', questId: 'live' },
				expect.objectContaining({ type: 'task-routed', citizenId: 'cto', questId: 'live' })
			])
		);
		expect(diffOfficeEvents(working, idle)).toEqual([
			{ type: 'work-ended', citizenId: 'cto', questId: 'live' }
		]);
	});
});

const socialPost = (
	postId: string,
	authorId: string,
	body: string,
	parentId: string | null = null
): FleetStateSocialPost => ({
	post_id: postId,
	parent_id: parentId,
	author_id: authorId,
	display_name: authorId,
	recent_activity: body,
	created_at: '2026-08-21T10:00:00.000Z'
});

describe('social-talk', () => {
	it('emits nothing on the first poll even when the feed already has posts', () => {
		const next = snapshot({
			ambient_social_activity: [socialPost('p1', 'cto', 'hello pantry')]
		});
		expect(diffOfficeEvents(null, next)).toEqual([]);
	});

	it('emits social-talk for a new public post whose author is seated', () => {
		const prev = snapshot();
		const next = snapshot({
			ambient_social_activity: [socialPost('p1', 'cto', 'coffee run?')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([
			{
				type: 'social-talk',
				id: 'p1',
				venue: socialVenueOf('p1'),
				participantIds: ['cto'],
				text: 'coffee run?'
			}
		]);
	});

	it('skips an author who is not seated', () => {
		const prev = snapshot();
		const next = snapshot({
			ambient_social_activity: [socialPost('p1', 'ghost-agent', 'hello')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});

	it('walks a reply to the parent venue and includes the seated parent author', () => {
		const parent = socialPost('root-1', 'cto', 'anyone around?');
		const reply = socialPost('reply-1', 'presto', 'on my way', 'root-1');
		const prev = snapshot({ ambient_social_activity: [parent] });
		const next = snapshot({ ambient_social_activity: [parent, reply] });
		expect(diffOfficeEvents(prev, next)).toEqual([
			{
				type: 'social-talk',
				id: 'reply-1',
				venue: socialVenueOf('root-1'),
				participantIds: ['presto', 'cto'],
				text: 'on my way'
			}
		]);
	});

	it('keeps a thread on one venue even when the parent has scrolled out', () => {
		const reply = socialPost('reply-2', 'presto', 'still here', 'root-gone');
		expect(socialVenueOf(socialRootPostId(reply, new Map()))).toBe(socialVenueOf('root-gone'));
	});

	it('emits nothing when previous social availability is missing', () => {
		const prev = snapshot();
		const { social: _social, ...availability } = prev.availability;
		prev.availability = availability;
		const next = snapshot({
			ambient_social_activity: [socialPost('p1', 'cto', 'hello')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});

	it('emits nothing from an unavailable social section', () => {
		const prev = snapshot();
		const next = snapshot({
			availability: { social: unavailable },
			ambient_social_activity: [socialPost('p1', 'cto', 'hello')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});

	it('does not erupt when previous rows have no post_id', () => {
		const prev = snapshot({
			ambient_social_activity: [
				{
					post_id: '',
					parent_id: null,
					author_id: 'cto',
					display_name: 'cto',
					recent_activity: 'old shape',
					created_at: '2026-08-21T09:00:00.000Z'
				}
			]
		});
		const next = snapshot({
			ambient_social_activity: [socialPost('p1', 'cto', 'new shape')]
		});
		expect(diffOfficeEvents(prev, next)).toEqual([]);
	});
});
