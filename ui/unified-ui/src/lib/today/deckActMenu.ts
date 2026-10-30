/**
 * The Morning Brief deck's "Act" menu: the real actions one card offers.
 *
 * The deck used to fire a single primary action per card (`approve` on a
 * follow-up, `open` on a Worth card), which started a task with no chance to
 * add an instruction and hid every other action behind the broadsheet. The
 * menu lists what the card's own data declares, in a fixed order, and each
 * entry names the existing flow that runs it. Pure so the entry set per card
 * kind can be tested without rendering the deck.
 */
import {
	channelProviderText,
	type ChannelActionDescriptor,
	type ChannelFollowUp
} from '$lib/stores/channelNeedsYouStore';
import {
	mergeResurfacingCapabilities,
	resurfacingActionDisplayLabel
} from './resurfacingPresentation';
import type { ResurfacingActionKind, ResurfacingCard } from './resurfacingQueries';

/** Follow-up snooze carries no duration: the card leaves Today. */
export const FOLLOW_UP_SNOOZE_LABEL = 'Snooze — hide from Today';
export const START_FOLLOW_UP_TASK_LABEL = 'Start follow-up task…';
export const DECK_DETAILS_LABEL = 'Details';
export const DECK_OPEN_LABEL = 'Open';

/** `Open` and `Details` already cover these, so the menu never repeats them. */
const WORTH_MENU_EXCLUDED = new Set<ResurfacingActionKind>(['view_details', 'open_source']);

export type DeckActEntry =
	| { id: string; kind: 'channel_action'; label: string; icon?: string; descriptor: ChannelActionDescriptor }
	| { id: string; kind: 'open_provider'; label: string; href: string }
	| { id: string; kind: 'start_task'; label: string }
	| { id: string; kind: 'snooze'; label: string }
	| { id: string; kind: 'open'; label: string }
	| { id: string; kind: 'resurfacing_action'; label: string; action: ResurfacingActionKind }
	| { id: string; kind: 'details'; label: string };

export interface DeckActOptions {
	/** The primary agent's display name, shown in place of the `ask_presto` wire name. */
	agentName: string;
	/** Ask <agent> posts into an existing chat thread; without one it is hidden. */
	chatThreadId?: string | null;
}

/** Entries resolve the card (it leaves the deck) only for these kinds. */
export function deckActEntryResolvesCard(entry: DeckActEntry): boolean {
	return entry.kind === 'snooze' || entry.kind === 'open';
}

function testIdPart(value: string): string {
	return value.trim().replace(/[^a-zA-Z0-9]+/g, '-').replace(/^-+|-+$/g, '').toLowerCase();
}

export function followUpActEntries(followUp: ChannelFollowUp): DeckActEntry[] {
	const entries: DeckActEntry[] = [];
	const seen = new Set<string>();
	for (const descriptor of followUp.available_actions ?? []) {
		if (!descriptor?.id || !descriptor.label?.trim()) continue;
		// Ids key the rendered menu, so two descriptors that normalise to the
		// same id keep only the first rather than break the list.
		const id = `channel-${testIdPart(descriptor.id)}`;
		if (seen.has(id)) continue;
		seen.add(id);
		entries.push({
			id,
			kind: 'channel_action',
			label: descriptor.label.trim(),
			...(descriptor.icon ? { icon: descriptor.icon } : {}),
			descriptor
		});
	}
	const href = followUp.open_url?.trim();
	if (href) {
		const provider = followUp.provider?.trim();
		entries.push({
			id: 'open-provider',
			kind: 'open_provider',
			label: `Open in ${provider ? channelProviderText(provider) : 'app'}`,
			href
		});
	}
	// A stale draft is re-opened for review instead of approved; Details
	// carries that control, so the menu does not offer a task it would refuse.
	if (!followUp.review_required) {
		entries.push({ id: 'start-task', kind: 'start_task', label: START_FOLLOW_UP_TASK_LABEL });
	}
	entries.push({ id: 'snooze', kind: 'snooze', label: FOLLOW_UP_SNOOZE_LABEL });
	entries.push({ id: 'details', kind: 'details', label: DECK_DETAILS_LABEL });
	return entries;
}

export function worthActEntries(card: ResurfacingCard, options: DeckActOptions): DeckActEntry[] {
	const entries: DeckActEntry[] = [{ id: 'open', kind: 'open', label: DECK_OPEN_LABEL }];
	const hasThread = Boolean(options.chatThreadId?.trim());
	for (const capability of mergeResurfacingCapabilities(card)) {
		if (WORTH_MENU_EXCLUDED.has(capability.kind)) continue;
		if (capability.kind === 'ask_presto' && !hasThread) continue;
		entries.push({
			id: testIdPart(capability.kind),
			kind: 'resurfacing_action',
			label: resurfacingActionDisplayLabel(capability, options.agentName),
			action: capability.kind
		});
	}
	entries.push({ id: 'details', kind: 'details', label: DECK_DETAILS_LABEL });
	return entries;
}

/** The Act menu for one deck card; empty for a card with no backing record. */
export function deckActEntries(
	card: { rawFollowUp?: ChannelFollowUp; rawWorth?: ResurfacingCard },
	options: DeckActOptions
): DeckActEntry[] {
	if (card.rawFollowUp) return followUpActEntries(card.rawFollowUp);
	if (card.rawWorth) return worthActEntries(card.rawWorth, options);
	return [];
}

/**
 * Worth "Open" for a source with no in-app route opens its external link
 * right here, inside the click: the useful-feedback call and the deck's exit
 * animation finish later, outside the user gesture, where popup blockers
 * would stop `window.open`. The host's open step then skips the repeat.
 */
const worthSourcesOpenedInGesture = new Set<string>();

export function openWorthLinkInGesture(card: ResurfacingCard | null | undefined): void {
	if (!card || card.source_route?.trim().startsWith('/')) return;
	const url = card.open_url?.trim();
	if (!url || typeof window === 'undefined') return;
	window.open(url, '_blank', 'noopener');
	worthSourcesOpenedInGesture.add(card.candidate_id);
}

/** True once per card that `openWorthLinkInGesture` already opened. */
export function takeWorthLinkOpenedInGesture(candidateId: string): boolean {
	return worthSourcesOpenedInGesture.delete(candidateId);
}
