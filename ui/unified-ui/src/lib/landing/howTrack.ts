import { clamp } from './scrub';

export const HOW_CLAIMS = [
	{
		id: 'superapp',
		kicker: 'How',
		title: 'A personal AI SuperApp',
		line: 'Agents that can get work done on your Browser, Mac Apps and Mobile Apps'
	},
	{
		id: 'local',
		kicker: 'Local first',
		title: 'Your machine, your choice',
		line: 'Local memory, with your choice of local or hosted models for each supported operation.'
	},
	{
		id: 'security',
		kicker: 'Security',
		title: 'Enterprise-grade, personal',
		line: 'Passwords and cards stay sealed. The model never sees them.'
	},
	{
		id: 'bounds',
		kicker: 'Yours',
		title: 'You decide the bounds',
		line: 'Privacy, capability, speed, and spend. Your priorities should guide every operation.'
	},
	{
		id: 'crew',
		kicker: 'Crew',
		title: 'Your crew, on their own',
		line: 'A roster for mail, travel, research, the house — working without you standing over them.'
	}
] as const;

export type HowClaimId = (typeof HOW_CLAIMS)[number]['id'];

export const HOW_TRACK_VH = 540;
export const HOW_WEIGHTS = [1.4, 1, 1, 1, 1.2] as const;

const ENTER = 0.28;
const EXIT = 0.74;

function smooth(t: number): number {
	const x = clamp(t, 0, 1);
	return x * x * (3 - 2 * x);
}

export interface ClaimMotion {
	opacity: number;
	mediaX: number;
	copyX: number;
}

export function claimMotion(
	index: number,
	scene: number,
	local: number,
	last = HOW_CLAIMS.length - 1
): ClaimMotion {
	if (index !== scene) {
		return { opacity: 0, mediaX: index < scene ? -1 : 1, copyX: index < scene ? 1 : -1 };
	}

	// First station is already on when the how-track pins. Starting it at
	// opacity 0 made the field look empty — the fade never read as motion.
	if (index === 0 && local < ENTER) {
		return { opacity: 1, mediaX: 0, copyX: 0 };
	}

	if (local < ENTER) {
		const t = smooth(local / ENTER);
		return { opacity: t, mediaX: -1 + t, copyX: 1 - t };
	}

	if (index !== last && local > EXIT) {
		const t = smooth((local - EXIT) / (1 - EXIT));
		return { opacity: 1 - t, mediaX: -t, copyX: t };
	}

	return { opacity: 1, mediaX: 0, copyX: 0 };
}
