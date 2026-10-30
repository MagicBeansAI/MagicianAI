export const MANIFESTO_DATE = 'Sunday, 13 September 2026';
export const MANIFESTO_TITLE = 'Personal Intelligence is your asset';
export const MANIFESTO_SIGN = 'Magican';
export const MANIFESTO_TAGLINE = 'More You';

export type ManifestoBlock =
	| { kind: 'prose'; text: string }
	| { kind: 'verse'; lines: readonly string[] };

export type ManifestoRun = { strong: boolean; text: string };

export function manifestoPlain(text: string): string {
	return text.replace(/\*\*(.+?)\*\*/g, '$1');
}

export function manifestoInline(text: string): ManifestoRun[] {
	const runs: ManifestoRun[] = [];
	const re = /\*\*(.+?)\*\*/g;
	let last = 0;
	let match: RegExpExecArray | null;
	while ((match = re.exec(text))) {
		if (match.index > last) {
			runs.push({ strong: false, text: text.slice(last, match.index) });
		}
		runs.push({ strong: true, text: match[1] ?? '' });
		last = match.index + match[0].length;
	}
	if (last < text.length) {
		runs.push({ strong: false, text: text.slice(last) });
	}
	return runs.length > 0 ? runs : [{ strong: false, text }];
}

export function manifestoText(blocks: readonly ManifestoBlock[]): string {
	return blocks
		.map((block) =>
			block.kind === 'prose' ? manifestoPlain(block.text) : block.lines.map(manifestoPlain).join('\n')
		)
		.join('\n');
}

export const MANIFESTO_EXCERPT: readonly ManifestoBlock[] = [
	{
		kind: 'prose',
		text: 'It is your responsibility to build the life you desire.'
	},
	{
		kind: 'prose',
		text: 'You have to find your own way, hold onto your self-confidence, and create your own opportunities. Your company will adapt, automate, reorganise, and move on. It is under no obligation to keep you relevant. **It is you who must.**'
	},
	{
		kind: 'prose',
		text: 'And in the day-to-day work that keeps you ahead, everything hinges on timing:'
	},
	{
		kind: 'verse',
		lines: [
			'The insight delivered at the right moment.',
			"The detail that wasn't overlooked.",
			'The preparation no one else thought to make.',
			'The work finished while everyone else was still discussing it.'
		]
	}
];

export const MANIFESTO_BODY: readonly ManifestoBlock[] = [
	...MANIFESTO_EXCERPT,
	{
		kind: 'prose',
		text: 'Life outside work is no different. Relationships are sustained through thousands of small acts of attention; families depend on someone holding the details together; and health, finances, learning, and ambition all compete for the same finite hours.'
	},
	{
		kind: 'verse',
		lines: [
			'No single person can keep all of it in their head.',
			'They were never meant to.'
		]
	},
	{
		kind: 'prose',
		text: 'Computers promised us freedom. Instead, they gave us more apps to babysit, more notifications to triage, and more silos to keep in sync. We became the middleware wiring our own lives together.'
	},
	{
		kind: 'prose',
		text: 'It does not have to be this way. A trusted machine can remember what you cannot. It can catch what is slipping through, prepare you before a moment arrives, and quietly carry momentum when your focus is elsewhere.'
	},
	{
		kind: 'prose',
		text: 'Yet none of this works trapped inside a single inbox, note app, or corporate account. You need one intelligence that understands the continuity of your life—across work and craft, commitments and relationships, plans and possibilities.'
	},
	{
		kind: 'verse',
		lines: [
			"You don't have to keep briefing another chatbot.",
			'A confidante that remembers.',
			'A collaborator that prepares.',
			'A machine capable of acting.',
			'An intelligence that becomes more valuable the longer you live.'
		]
	},
	{
		kind: 'prose',
		text: 'AI models have limits, and we should be honest about them. Small local models give you total privacy and instant response for focused tasks. Difficult reasoning and complex multi-step work still demand frontier cloud models.'
	},
	{
		kind: 'prose',
		text: '**Personal intelligence must put you in control:** you decide what stays on your device, which model touches each step, what context is shared, and what you spend. Your privacy boundaries must hold even when a model falls short. If a task cannot be completed within your rules, your AI stops, explains why, and asks. It never leaks.'
	},
	{
		kind: 'verse',
		lines: [
			'Your personal AI answers to you.',
			'**It is yours.**',
			'You determine what it remembers, what stays private, which model runs, what it can do, and where it must halt.'
		]
	},
	{
		kind: 'prose',
		text: 'That is the reason I developed Magican for myself. To build personal intelligence that grows alongside you, unlocks the leverage you need, and keeps every decision firmly in your hands.'
	},
	{
		kind: 'verse',
		lines: [
			'So you can carve your own path.',
			'Do work you are proud of.',
			'Show up for the people who matter.',
			'And spend more of your life on the things only you can do.'
		]
	}
];
