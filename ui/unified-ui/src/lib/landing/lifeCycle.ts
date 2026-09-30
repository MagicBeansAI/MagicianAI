export const ACTIVITIES = [
	'singing',
	'cooking',
	'drawing',
	'running',
	'dancing',
	'reading',
	'walking',
	'baking',
	'gardening',
	'painting',
	'listening',
	'composing',
	'rehearsing',
	'practising',
	'swimming',
	'hiking',
	'knitting',
	'journaling',
	'photographing',
	'travelling',
	'hosting',
	'studying',
	'parenting',
	'showing up',
	'saving',
	'briefing',
	'repairing',
	'shipping',
	'freelancing',
	'hiring',
	'filing',
	'coding',
	'coaching',
	'unpacking',
	'collecting',
	'exploring'
] as const;

export const CLOSER = 'all your side quests';
export const CYCLE = [...ACTIVITIES, CLOSER] as const;

/** Same palette BrandReveal used on the cycling personas. */
export const VERB_COLORS = [
	'#1e3a8a',
	'#064e3b',
	'#4c1d95',
	'#7f1d1d',
	'#7c2d12',
	'#0f172a',
	'#831843',
	'#3f6212',
	'#111827',
	'#581c87'
] as const;

export function colorAt(index: number): string {
	return VERB_COLORS[((index % VERB_COLORS.length) + VERB_COLORS.length) % VERB_COLORS.length];
}

export const HOLD_MS_START = 1000;
export const HOLD_MS_END = 160;
export const CLOSER_HOLD_MS = 2800;
export const CLOSER_INDEX = ACTIVITIES.length;

export function holdMsFor(index: number): number {
	if (index === CLOSER_INDEX) return CLOSER_HOLD_MS;
	const t = Math.min(1, Math.max(0, index / (CLOSER_INDEX - 1)));
	return Math.round(HOLD_MS_END + (HOLD_MS_START - HOLD_MS_END) * (1 - t) ** 2);
}

export function nextIndex(index: number): number {
	return (index + 1) % CYCLE.length;
}

export function labelAt(index: number): string {
	return CYCLE[index] ?? CLOSER;
}
