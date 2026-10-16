import { clamp } from './scrub';

// Seven things only Magican does, told in the film's own lines (the intro
// film's narration): a kicker, an outcome title, the capability as the line,
// and one true aside. Each has a card at static/landing/how/<id>.{mp4,webp}.
export const HOW_CLAIMS = [
	{
		id: 'present',
		kicker: 'Wherever you are',
		title: 'Hold a key, and just talk.',
		line: 'An orb on your laptop that can see what’s on your screen. WhatsApp when you’re out. A seat in the meetings you’re optional for. A little device when you don’t want a phone at all.',
		aside: 'Maa’s water bills aren’t missed anymore.'
	},
	{
		id: 'hands',
		kicker: 'Real apps',
		title: 'Your phone’s at home. Your ride’s booked on it.',
		line: 'Magican works the apps you already use: your browser, your Mac, even the Android in another room. A Rapido to your usual Starbucks, dinner on Swiggy, the OTP from your other phone, put right where you need it.',
		aside: 'It talked you out of the fried chicken.'
	},
	{
		id: 'learns',
		kicker: 'Today',
		title: 'What needs you, and what’s worth a look.',
		line: 'Every morning, one page. Swipe right on what’s useful, left on what isn’t, and it learns what matters to you, right on your machine.',
		aside: 'Kyoto fares dropped ₹4,000. Want it held?'
	},
	{
		id: 'relentless',
		kicker: 'Relentless',
		title: 'Tell it what done looks like. Then let it go.',
		line: 'It tries, checks its own work against your bar, and tries again until it’s good enough. It does the grind. You make the calls.',
		aside: 'Five perfect clients for a coaching practice, found.'
	},
	{
		id: 'builds',
		kicker: 'Builds',
		title: 'Describe it. It builds it, and ships it.',
		line: 'A site, a small app, a tool you wished existed: Magican writes it, checks it, and puts it on the web for you.',
		aside: 'Live before you’ve finished your coffee.'
	},
	{
		id: 'agents',
		kicker: 'Your agents',
		title: 'A public agent, with its own email and WhatsApp.',
		line: 'Twenty-four-odd agents and personalities, ninety skills prebuilt, and the option to bring the engine you already pay for. An agent the world can talk to, inside the limits you set.',
		aside: 'Runs a one-person company. Doesn’t ask for equity.'
	},
	{
		id: 'yours',
		kicker: 'Yours',
		title: 'The models never see any of it.',
		line: 'Not the codes, not your passwords, not your cards, not your keys. The runtime runs on your own system with MagicVault, and you decide precisely what stays with you and what goes out.',
		aside: 'An intelligence that belongs to you.'
	}
] as const satisfies readonly { id: string; kicker: string; title: string; line: string; aside: string }[];

export type HowClaimId = (typeof HOW_CLAIMS)[number]['id'];

// seven stations at the pace the five had (540svh for five)
export const HOW_TRACK_VH = 760;
export const HOW_WEIGHTS = [1.4, 1, 1, 1, 1, 1, 1.2] as const;

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
