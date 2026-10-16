// The hook's role list. Order matters: the expected roles establish the
// pattern and the last one breaks it. A small, unimpressive role sitting in
// the same list as a professional is the argument the page is making.
export const ROLES = [
	'a professional',
	'a parent',
	'a partner',
	'somebody’s kid',
	'a music lover',
	'a movie buff',
	'a sports enthusiast',
	'a tinkerer',
	'a storyteller',
	'someone who still checks the score'
] as const;

/** 2.4s per role — long enough to read, slow enough to feel unhurried. */
export const HOLD_MS = 2400;

export function cycleIndex(elapsedMs: number, count: number): number {
	if (count <= 0) return 0;
	return Math.floor(elapsedMs / HOLD_MS) % count;
}
