import type { IconName } from '$lib/shared/icons/paths';
import type { HowClaimId } from './howTrack';

/** Long enough to read a dense product frame; slow enough not to flicker. */
export const HOW_SHOT_HOLD_MS = 3200;

export const HOW_SHOT_FRAMES = {
	superapp: [
		{ id: 'today', chrome: 'Today', icon: 'calendar' },
		{ id: 'chat', chrome: 'Chat', icon: 'message' },
		{ id: 'tasks', chrome: 'Tasks', icon: 'check' }
	],
	local: [
		{ id: 'tiers', chrome: 'Memory', icon: 'archive' },
		{ id: 'synthesize', chrome: 'This Mac', icon: 'zap' }
	],
	security: [
		{ id: 'password', chrome: 'Vault', icon: 'eye' },
		{ id: 'card', chrome: 'Cards', icon: 'square' }
	],
	bounds: [
		{ id: 'ceilings', chrome: 'Resource Authority', icon: 'settings' },
		{ id: 'tokens', chrome: 'Spend tokens', icon: 'zap' }
	],
	crew: [
		{ id: 'roster', chrome: 'Crew', icon: 'inbox' },
		{ id: 'working', chrome: 'Travel', icon: 'play' }
	]
} as const satisfies Record<
	HowClaimId,
	readonly { id: string; chrome: string; icon: IconName }[]
>;

export type HowShotFrame = (typeof HOW_SHOT_FRAMES)[HowClaimId][number];
export type HowShotFrameId = HowShotFrame['id'];

export function howShotFrames(kind: HowClaimId): readonly HowShotFrame[] {
	return HOW_SHOT_FRAMES[kind];
}

export function shotCycleIndex(elapsedMs: number, count: number): number {
	if (count <= 1) return 0;
	return Math.floor(elapsedMs / HOW_SHOT_HOLD_MS) % count;
}
