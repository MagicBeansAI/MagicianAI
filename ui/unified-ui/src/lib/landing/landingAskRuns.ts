// Small, scripted examples for the landing-page composer. They are honest
// replays, not live work: the component labels them as examples and keeps
// the real input available throughout. The vocabulary mirrors user-facing
// product surfaces rather than internal tool or profile names.

export interface LandingAskBeat {
	step: number;
	tool: string;
	detail: string;
	ms: number;
	needsYou?: boolean;
}

export interface LandingAskRun {
	id: string;
	chip: string;
	ask: string;
	plan: string[];
	beats: LandingAskBeat[];
	result: {
		title: string;
		lines: string[];
	};
	filedAs: string;
}

export const LANDING_ASK_RUNS: readonly LandingAskRun[] = [
	{
		id: 'flights',
		chip: 'Find a flight',
		ask: 'Find the best Kyoto flights next month from Bengaluru',
		plan: ['search flexible dates', 'compare the sensible routes', 'shortlist the best three'],
		beats: [
			{ step: 0, tool: 'Research', detail: '14 routes across five dates', ms: 1750 },
			{ step: 1, tool: 'Browser', detail: 'Tuesday saves ₹4,100', ms: 1850 },
			{ step: 2, tool: 'Memory', detail: 'aisle seat and one-stop preference applied', ms: 1750 }
		],
		result: {
			title: 'Three options, ranked',
			lines: ['Best: ₹38,400 · one stop', 'Tuesday departure saves ₹4,100']
		},
		filedAs: 'Travel preferences remembered for next time'
	},
	{
		id: 'meeting',
		chip: 'Catch me up',
		ask: 'What did I miss in yesterday’s design sync?',
		plan: ['read the transcript', 'pull out decisions', 'find anything you owe'],
		beats: [
			{ step: 0, tool: 'Meetings', detail: '47 minutes · four speakers', ms: 1750 },
			{ step: 1, tool: 'Decisions', detail: 'two decisions and one resolved debate', ms: 1850 },
			{ step: 2, tool: 'Tasks', detail: 'API draft due Friday', ms: 1750 }
		],
		result: {
			title: 'Design sync, distilled',
			lines: ['Ship Friday · dark mode moved to v2', 'Your API-draft task is ready']
		},
		filedAs: 'Decision and task linked to the meeting'
	},
	{
		id: 'refund',
		chip: 'Chase a refund',
		ask: 'Chase the refund for my cancelled flight',
		plan: ['find the cancellation', 'check the refund policy', 'send a clear follow-up'],
		beats: [
			{ step: 0, tool: 'Mail', detail: 'cancellation found from 32 days ago', ms: 1750 },
			{ step: 1, tool: 'Browser', detail: 'the stated refund window was 14 days', ms: 1850 },
			{ step: 2, tool: 'Mail', detail: 'policy cited and case opened', ms: 1750 }
		],
		result: {
			title: 'Refund chased: ₹12,450',
			lines: ['Case #4417 opened', 'Follow-up scheduled if there is no reply']
		},
		filedAs: 'Case and next follow-up kept together'
	},
	{
		id: 'groceries',
		chip: 'Restock groceries',
		ask: 'Restock my usual groceries and stay under ₹2,000',
		plan: ['recall the usual basket', 'compare today’s prices', 'ask before placing the order'],
		beats: [
			{ step: 0, tool: 'Memory', detail: '14 usual items, pantry checked', ms: 1750 },
			{ step: 1, tool: 'Compare', detail: 'best basket is ₹1,640 today', ms: 1850 },
			{
				step: 2,
				tool: 'Needs you',
				detail: '₹1,640 of the ₹2,000 cap · approve?',
				ms: 2150,
				needsYou: true
			}
		],
		result: {
			title: 'Ready when you approve',
			lines: ['14 items · ₹1,640', '₹360 remains inside the cap']
		},
		filedAs: 'The basket stays editable until approval'
	}
];
