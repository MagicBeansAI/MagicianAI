/**
 * Snapshot differ for the office floor.
 *
 * `fleet-state` is a snapshot, not an event log. This emits a typed event only
 * when the change can be proved. Where a difference cannot be distinguished
 * from a re-derivation artefact, it emits nothing. The first poll (`prev` is
 * null) emits nothing — otherwise every citizen's current state would read as
 * a change and the floor would erupt on load.
 *
 * An `unavailable` section is treated as "we cannot see this", not as empty.
 * Social talks require a visible social section on both snapshots, a `post_id`
 * the previous snapshot did not have, and a seated author; replies hash the
 * root post id so a thread stays at one venue.
 */

import type {
	FleetSectionAvailability,
	FleetStateCitizen,
	FleetStateHandoff,
	FleetStateSocialPost,
	FleetStateSnapshot
} from '../fleetState';
import type { SocialVenue } from './actors';

export type OfficeEvent =
	| {
			type: 'delivery-landed';
			id: string;
			citizenId: string | null;
			createdAt: number;
			title: string;
	  }
	| {
			type: 'handoff';
			id: string;
			fromId: string;
			toId: string;
			questId: string;
	  }
	| {
			type: 'task-routed';
			citizenId: string;
			questId: string;
			title: string;
	  }
	| {
			type: 'blocked';
			citizenId: string;
			attentionId: string;
	  }
	| {
			type: 'unblocked';
			citizenId: string;
			attentionId: string;
	  }
	| {
			type: 'work-started';
			citizenId: string;
			questId: string | null;
	  }
	| {
			type: 'work-ended';
			citizenId: string;
			questId: string | null;
	  }
	| {
			type: 'social-talk';
			id: string;
			venue: SocialVenue;
			participantIds: string[];
			text: string;
	  };

const WORKING_TASK_STATUSES = new Set([
	'planning',
	'running',
	'in_progress',
	'active',
	'delivering',
	'synthesizing'
]);

export function handoffIdOf(handoff: FleetStateHandoff): string {
	return `${handoff.quest_id}:${handoff.parent_execution_id}:${handoff.child_execution_id ?? handoff.to.citizen_id}`;
}

export function diffOfficeEvents(
	prev: FleetStateSnapshot | null,
	next: FleetStateSnapshot
): OfficeEvent[] {
	if (!prev) return [];

	const events: OfficeEvent[] = [];

	if (sectionVisible(prev.availability.deliveries) && sectionVisible(next.availability.deliveries)) {
		const lastSeen = maxCreatedAt(prev.deliveries);
		for (const item of next.deliveries) {
			if (item.created_at > lastSeen) {
				events.push({
					type: 'delivery-landed',
					id: item.id,
					citizenId: item.citizen_id,
					createdAt: item.created_at,
					title: item.title
				});
			}
		}
	}

	if (sectionVisible(prev.availability.handoffs) && sectionVisible(next.availability.handoffs)) {
		const seated = seatedIds(next);
		const seen = new Set(prev.handoffs.map(handoffIdOf));
		for (const item of next.handoffs) {
			const id = handoffIdOf(item);
			if (seen.has(id)) continue;
			const fromId = item.from.citizen_id;
			const toId = item.to.citizen_id;
			if (!fromId || !toId || !seated.has(fromId) || !seated.has(toId)) continue;
			events.push({
				type: 'handoff',
				id,
				fromId,
				toId,
				questId: item.quest_id
			});
		}
	}

	const citizensVisible =
		sectionVisible(prev.availability.citizens) && sectionVisible(next.availability.citizens);

	if (citizensVisible) {
		const prevHeld = heldQuestIds(prev.citizens);
		for (const citizen of next.citizens) {
			for (const item of citizen.current_work) {
				if (prevHeld.has(item.quest_id)) continue;
				events.push({
					type: 'task-routed',
					citizenId: citizen.citizen_id,
					questId: item.quest_id,
					title: item.title
				});
			}
		}

		const prevById = new Map(prev.citizens.map((c) => [c.citizen_id, c]));
		for (const citizen of next.citizens) {
			const before = prevById.get(citizen.citizen_id);
			if (!before) continue;
			const wasWorking = isWorking(before);
			const nowWorking = isWorking(citizen);
			if (!wasWorking && nowWorking) {
				events.push({
					type: 'work-started',
					citizenId: citizen.citizen_id,
					questId: workingQuestId(citizen)
				});
			} else if (wasWorking && !nowWorking) {
				events.push({
					type: 'work-ended',
					citizenId: citizen.citizen_id,
					questId: workingQuestId(before)
				});
			}
		}
	}

	if (sectionVisible(prev.availability.attention) && sectionVisible(next.availability.attention)) {
		const prevById = new Map(
			prev.attention
				.filter((item) => item.citizen_id)
				.map((item) => [item.id, item.citizen_id as string])
		);
		const nextById = new Map(
			next.attention
				.filter((item) => item.citizen_id)
				.map((item) => [item.id, item.citizen_id as string])
		);
		for (const [id, citizenId] of nextById) {
			if (!prevById.has(id)) events.push({ type: 'blocked', citizenId, attentionId: id });
		}
		for (const [id, citizenId] of prevById) {
			if (!nextById.has(id)) events.push({ type: 'unblocked', citizenId, attentionId: id });
		}
	}

	const prevSocialVisible = sectionPresentAndVisible(prev.availability.social);
	const nextSocialVisible = sectionPresentAndVisible(next.availability.social);
	if (prevSocialVisible && nextSocialVisible) {
		const prevPosts = prev.ambient_social_activity ?? [];
		const nextPosts = next.ambient_social_activity ?? [];
		const seenPostIds = new Set(prevPosts.map((post) => post.post_id).filter(Boolean));
		// Older snapshots had feed rows without post_id. That history cannot be
		// distinguished from a first sighting, so emit nothing rather than a
		// burst of talks on the first poll after the field lands.
		const prevIdsAreUsable = prevPosts.length === 0 || seenPostIds.size > 0;
		if (prevIdsAreUsable) {
			const seated = seatedIds(next);
			const byId = new Map(
				nextPosts.filter((post) => post.post_id).map((post) => [post.post_id, post])
			);
			for (const post of prevPosts) {
				if (post.post_id) byId.set(post.post_id, post);
			}
			for (const post of nextPosts) {
				if (!post.post_id || seenPostIds.has(post.post_id)) continue;
				const text = post.recent_activity?.trim();
				if (!text) continue;
				const participantIds = talkParticipants(post, byId, seated);
				if (participantIds.length === 0) continue;
				events.push({
					type: 'social-talk',
					id: post.post_id,
					venue: socialVenueOf(socialRootPostId(post, byId)),
					participantIds,
					text
				});
			}
		}
	}

	return events;
}

const SOCIAL_VENUES: readonly SocialVenue[] = ['cooler', 'pantry', 'lounge'];

/** Stable venue for a thread: hash of the root post id, not the latest reply. */
export function socialVenueOf(rootPostId: string): SocialVenue {
	let hash = 2166136261;
	for (let i = 0; i < rootPostId.length; i++) {
		hash ^= rootPostId.charCodeAt(i);
		hash = Math.imul(hash, 16777619);
	}
	return SOCIAL_VENUES[(hash >>> 0) % SOCIAL_VENUES.length];
}

export function socialRootPostId(
	post: FleetStateSocialPost,
	byId: ReadonlyMap<string, FleetStateSocialPost>
): string {
	let current: FleetStateSocialPost | undefined = post;
	const seen = new Set<string>();
	while (current) {
		if (seen.has(current.post_id)) return current.post_id;
		seen.add(current.post_id);
		const parentId = current.parent_id;
		if (!parentId) return current.post_id;
		const parent = byId.get(parentId);
		if (!parent) return parentId;
		current = parent;
	}
	return post.post_id;
}

function talkParticipants(
	post: FleetStateSocialPost,
	byId: ReadonlyMap<string, FleetStateSocialPost>,
	seated: ReadonlySet<string>
): string[] {
	if (!seated.has(post.author_id)) return [];
	const ids = [post.author_id];
	if (post.parent_id) {
		const parentAuthor = byId.get(post.parent_id)?.author_id;
		if (parentAuthor && seated.has(parentAuthor) && !ids.includes(parentAuthor)) {
			ids.push(parentAuthor);
		}
	}
	return ids;
}

function sectionVisible(section: FleetSectionAvailability | undefined): boolean {
	return section?.status !== 'unavailable';
}

function sectionPresentAndVisible(section: FleetSectionAvailability | undefined): boolean {
	return section != null && section.status !== 'unavailable';
}

function seatedIds(snapshot: FleetStateSnapshot): Set<string> {
	return new Set(snapshot.citizens.map((c) => c.citizen_id));
}

function heldQuestIds(citizens: readonly FleetStateCitizen[]): Set<string> {
	const ids = new Set<string>();
	for (const citizen of citizens) {
		for (const item of citizen.current_work) ids.add(item.quest_id);
	}
	return ids;
}

function isWorking(citizen: FleetStateCitizen): boolean {
	return citizen.current_work.some((item) => {
		if (item.is_blocked) return false;
		return WORKING_TASK_STATUSES.has(item.status.trim().toLowerCase());
	});
}

function workingQuestId(citizen: FleetStateCitizen): string | null {
	return (
		citizen.current_work.find((item) => WORKING_TASK_STATUSES.has(item.status.trim().toLowerCase()))
			?.quest_id ??
		citizen.current_work[0]?.quest_id ??
		null
	);
}

function maxCreatedAt(deliveries: FleetStateSnapshot['deliveries']): number {
	let max = Number.NEGATIVE_INFINITY;
	for (const item of deliveries) {
		if (item.created_at > max) max = item.created_at;
	}
	return Number.isFinite(max) ? max : Number.NEGATIVE_INFINITY;
}
