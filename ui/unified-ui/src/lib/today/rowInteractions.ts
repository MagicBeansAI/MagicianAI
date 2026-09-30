import type { TodaySectionId } from './types';

export function isInteractiveDescendantEvent(
	target: EventTarget | null,
	currentTarget: EventTarget | null
): boolean {
	const node = target instanceof Element ? target : null;
	if (!node) return false;
	const interactiveAncestor = node.closest(
		'button,input,select,textarea,a,label,[role="button"],[role="link"],[role="menu"]'
	);
	if (!interactiveAncestor) return false;
	return interactiveAncestor !== currentTarget;
}

export function showTodayPrimaryAction(section: TodaySectionId): boolean {
	return section !== 'followups';
}

export function todayRowClass(section: TodaySectionId, emphasized: boolean): string {
	return `today-row ui-no-press today-row--${section.replace(/_/g, '-')}${emphasized ? ' today-row--emphasis' : ''}`;
}
