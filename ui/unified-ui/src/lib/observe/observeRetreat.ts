/**
 * The `/observe` retreat (gate M4) — consumption, not demolition.
 *
 * The meetings package ships system-class with widgets pinned as workspace
 * defaults into `/observe`'s slots. Once one of those widgets actually renders,
 * the first-party section it duplicates has nothing left to add, and standing
 * down is the cleanup. Nothing is deleted: the section returns the moment the
 * widget stops rendering — a disabled app, an unavailable installation, a
 * refused render, an opted-out slot — because the retreat is a *consequence* of
 * the widget being there, never a decision taken ahead of it.
 *
 * Capture visibility is not negotiable and is not represented here at all. The
 * meetings increment pins it in two places at once: the host-rendered TopBar
 * dot and `/observe`'s own capture sections. An active capture must be visible
 * in both, so no widget — pinned, system-class or otherwise — can retire
 * either. `observeSectionMayRetreat` refuses every name outside the retreatable
 * set rather than accepting an unknown one, so adding a section to this file is
 * the only way to make it retreatable.
 */

/** Sections a pinned app widget may take over. */
export type ObserveRetreatableSection = 'recent_meetings';

/**
 * Sections that carry the capture-visibility invariant. Listed so the rule is
 * readable next to the mechanism it constrains — they are deliberately absent
 * from the retreatable map below, and this array is what a test asserts about.
 */
export const OBSERVE_CAPTURE_VISIBILITY_SECTIONS: readonly string[] = [
	'active_captures',
	'capture_metrics',
	'capture_controls'
];

/** The page whose slots this retreat is about. */
export const OBSERVE_RETREAT_PAGE = '/observe';

/**
 * Which slot region must be filled before a section stands down. The mapping is
 * closed: a region with no entry here retires nothing, however it was pinned.
 */
const RETREAT_REGION: Readonly<Record<ObserveRetreatableSection, string>> = {
	recent_meetings: 'history'
};

export function observeSectionMayRetreat(section: string): section is ObserveRetreatableSection {
	return Object.prototype.hasOwnProperty.call(RETREAT_REGION, section);
}

/**
 * The sections that may stand down for this render.
 *
 * `page` is checked rather than assumed: region owners on other pages emit the
 * same event, and `history` on `/` is a different slot entirely. A mismatch
 * retires nothing.
 */
export function observeRetreatedSections(
	page: string,
	filledRegions: Iterable<string>
): Set<ObserveRetreatableSection> {
	const retreated = new Set<ObserveRetreatableSection>();
	if (page !== OBSERVE_RETREAT_PAGE) return retreated;
	const filled = new Set(filledRegions);
	// The guard, not the loop, decides what may retreat. Iterating the map and
	// trusting its keys would make the rule true only by construction; running
	// every candidate past `observeSectionMayRetreat` keeps one admission point
	// even if a later change widens where candidates come from.
	for (const section of Object.keys(RETREAT_REGION)) {
		if (!observeSectionMayRetreat(section)) continue;
		if (filled.has(RETREAT_REGION[section])) retreated.add(section);
	}
	return retreated;
}
