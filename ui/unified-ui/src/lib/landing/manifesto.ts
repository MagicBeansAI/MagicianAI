export const MANIFESTO_DATE = 'Sunday, 13 September 2026';
export const MANIFESTO_TITLE = 'Personal Intelligence is your asset';
export const MANIFESTO_SIGN = 'Magican';
export const MANIFESTO_TAGLINE = 'Superpowers for work, play and all your side quests';

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
		text: 'It is your responsibility to help yourself in building the life you desire.'
	},
	{
		kind: 'prose',
		text: 'You need to discover your own way, keep your self-confidence and make opportunities for yourself. You must stay closely informed about your work, keep your promises and be there for the people who matter.'
	},
	{
		kind: 'verse',
		lines: [
			'Everything at work depends on timing.',
			'The insight given at the right time.',
			'The subsequent one which was not overlooked.',
			'A preparation that no one else had thought to carry out.',
			'The work was finished while everyone else was still discussing it.'
		]
	},
	{
		kind: 'prose',
		text: '**Personal intelligence should put you in control:** what stays private, where you need more capability, and which model handles each part of the work.'
	}
];

export const MANIFESTO_BODY: readonly ManifestoBlock[] = [
	...MANIFESTO_EXCERPT,
	{
		kind: 'prose',
		text: 'Your company will adapt, automate, reorganise and move on; it does not have the responsibility of keeping you relevant. **It is you who do.**'
	},
	{
		kind: 'prose',
		text: 'The situation in life when not at work is no different. People build up their relationships through a multitude of small acts of attention; families depend on someone remembering all the details; and health, money, education, ideas, and ambitions all have to compete for the same limited amount of time.'
	},
	{
		kind: 'verse',
		lines: [
			'It is impossible for a single person to keep all of it in their head.',
			"Probably they didn't need to."
		]
	},
	{
		kind: 'prose',
		text: 'What the computer was meant to do is give us more freedom; instead it provided us with more applications to operate, more notifications to deal with and more systems to keep up to date. We became the machinery that linked our own lives.'
	},
	{
		kind: 'prose',
		text: 'The situation could be reversed. A machine that you can trust is able to recall things that you cannot. It can pick up on the things that are being overlooked, get you ready before it becomes important, carries on when your attention has shifted elsewhere and gently looks after the various tasks around your life.'
	},
	{
		kind: 'prose',
		text: "Yet it isn't genuinely capable of doing so within a single document, a single inbox, or a single company account."
	},
	{
		kind: 'prose',
		text: 'You need one intelligence which is able to understand the continuity of your life—whether in regard to work and play, your responsibilities and relationships, your plans and possibilities.'
	},
	{
		kind: 'verse',
		lines: [
			"You don't have to keep briefing another chatbot.",
			'A confidante that remembers.',
			'A collaborator that prepares.',
			'A machine capable of acting.',
			'An intelligence that is more useful to you the more your life progresses.'
		]
	},
	{
		kind: 'prose',
		text: 'AI models can already help us do valuable work, but their limits matter. Local models can be useful for focused tasks and private processing. Today, they cannot reliably handle every complex task on the hardware people have. Difficult reasoning, coding, or work across many steps may need a more capable hosted model. We should be honest about that, and about the fact that hosted models can fail too.'
	},
	{
		kind: 'prose',
		text: 'You should be able to decide where privacy is essential, where performance matters most, how long you can wait, and what you are willing to spend. Those choices belong to you, and they can be different for every operation within the same task.'
	},
	{
		kind: 'prose',
		text: 'Keep the understanding of your private journal on your machine. Choose a stronger hosted model for a difficult coding step using only the files you allow. Give routine sorting to a smaller model when it does the job well. One personal intelligence should be able to work with different models under your rules.'
	},
	{
		kind: 'prose',
		text: 'A choice about models is also a choice about who receives your information. You should be able to see which model handles an operation and what context it receives. Approving a hosted model for one step should never mean handing it your whole history. Local processing, shared context, speed, cost, and capability must be choices you can understand and change.'
	},
	{
		kind: 'prose',
		text: '**Your privacy boundaries must hold when a model falls short.** If the work cannot be completed within them, your AI should say so. It should help you narrow the task or choose another permitted model, and ask before sending information beyond the limits you set. Automatic routing should follow your rules.'
	},
	{
		kind: 'verse',
		lines: [
			'Your personal AI should answer to you.',
			'**It is yours.**',
			'You determine what it remembers, what stays private, which model handles each operation, what it is allowed to do and where it must halt.'
		]
	},
	{
		kind: 'prose',
		text: 'That is the reason I developed Magican for myself. To build personal intelligence that grows with you, gives you access to the capabilities you need, and keeps the decisions in your hands.'
	},
	{
		kind: 'verse',
		lines: [
			'So you can make your own path.',
			'Carry out work that you are proud of.',
			'Turn up for the people who matter.',
			'And use more of your life for the things that no one else can do.'
		]
	}
];
