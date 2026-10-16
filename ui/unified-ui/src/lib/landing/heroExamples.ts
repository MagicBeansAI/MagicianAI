export const HERO_EXAMPLE_INTERVAL_MS = 6_000;

type HeroExample = { ask: string; result: string };
type HeroExampleCategory = {
	label: string;
	mood: 'working' | 'on-it' | 'side-quest';
	examples: readonly HeroExample[];
};

// Illustrative requests and reviewable outcomes, never completed live tasks.
export const HERO_EXAMPLE_CATEGORIES = [
	{
		label: 'Work',
		mood: 'working',
		examples: [
			{
				ask: 'Turn these meeting notes into a clear brief.',
				result: 'A brief you can review and share. Fewer loose ends.'
			},
			{
				ask: 'Catch me up on my inbox. What actually needs me?',
				result: 'The important bits, with the noise turned down.'
			},
			{
				ask: 'Compare these proposals and flag the trade-offs.',
				result: 'A clear comparison. You make the call.'
			},
			{
				ask: 'Turn this big idea into a launch plan.',
				result: 'Milestones, next steps, and somewhere to start.'
			},
			{
				ask: 'Review this pull request before I ship it.',
				result: 'Another pair of eyes. Especially on the tricky bits.'
			},
			{
				ask: 'Help me find out why this test keeps failing.',
				result: 'A shorter path from “huh?” to a useful fix.'
			},
			{
				ask: 'Find the story in this spreadsheet.',
				result: 'Patterns, a chart, and the questions worth asking.'
			},
			{
				ask: 'Draft a client reply that sounds like me.',
				result: 'Your voice. A first draft you can make your own.'
			},
			{
				ask: 'Prep me for tomorrow’s meeting.',
				result: 'The context and a few good questions. Less scrambling.'
			},
			{
				ask: 'Make this README useful to a new teammate.',
				result: 'Clear steps. Fewer “wait, how do I run this?” moments.'
			},
			{
				ask: 'Help me turn this pitch into a compelling deck.',
				result: 'A sharper story, one slide at a time.'
			},
			{
				ask: 'Group this customer feedback into useful themes.',
				result: 'What people keep asking for, without the tab safari.'
			}
		]
	},
	{
		label: 'Play',
		mood: 'on-it',
		examples: [
			{
				ask: 'Help me learn this guitar piece.',
				result: 'A practice plan, at your pace.'
			},
			{
				ask: 'Teach me enough Italian to order more than coffee.',
				result: 'Small lessons. Big main-character energy.'
			},
			{
				ask: 'Find a film our whole group will agree on.',
				result: 'A shortlist. Ideally before the popcorn goes cold.'
			},
			{
				ask: 'Plan a board-game night for six very different friends.',
				result: 'Good games, easy rules, and room for a little chaos.'
			},
			{
				ask: 'Explain cricket like I’m joining the group chat today.',
				result: 'Enough to follow the match. And the banter.'
			},
			{
				ask: 'Help me untangle the next chapter of my story.',
				result: 'A few possibilities. The plot twist is still yours.'
			},
			{
				ask: 'What can I cook with what’s already in my fridge?',
				result: 'Dinner ideas. The mystery vegetable gets a purpose.'
			},
			{
				ask: 'Plan a photo walk with a few creative challenges.',
				result: 'Fresh prompts for seeing familiar streets differently.'
			},
			{
				ask: 'Make a pub quiz for my oddly competitive friends.',
				result: 'Questions, answers, and friendly arguments incoming.'
			},
			{
				ask: 'Help me build a tiny game this weekend.',
				result: 'A small idea with just enough room to get carried away.'
			},
			{
				ask: 'Build me a reading list around this book I loved.',
				result: 'Your next rabbit hole, with a few unexpected turns.'
			},
			{
				ask: 'Help me learn the constellations I can see tonight.',
				result: 'A little more wonder on your evening walk.'
			}
		]
	},
	{
		label: 'Side quests',
		mood: 'side-quest',
		examples: [
			{
				ask: 'Find my refund email and draft a follow-up.',
				result: 'A follow-up, ready for your review.'
			},
			{
				ask: 'Compare flights for my dates. Show me the trade-offs.',
				result: 'A shortlist to review before you book.'
			},
			{
				ask: 'Turn my scattered errands into one sensible plan.',
				result: 'Fewer zigzags. More of your Saturday back.'
			},
			{
				ask: 'Find a nearby trail for a slow Sunday walk.',
				result: 'A few options for getting pleasantly off the clock.'
			},
			{
				ask: 'Compare these headphones without the jargon.',
				result: 'The differences that matter to how you’ll use them.'
			},
			{
				ask: 'Help me find a birthday gift that feels thoughtful.',
				result: 'Ideas that say “I know you.” Within your budget.'
			},
			{
				ask: 'Plan a weekend away with plenty of room to wander.',
				result: 'A loose plan for a proper little escape.'
			},
			{
				ask: 'Suggest a way to organise my chaotic Downloads folder.',
				result: 'A tidy plan to review before anything moves.'
			},
			{
				ask: 'Turn this pile of receipts into an expense table.',
				result: 'The numbers in order. The shoebox can retire.'
			},
			{
				ask: 'Make me a moving-house checklist I can actually use.',
				result: 'Less “did we forget something?” on moving day.'
			},
			{
				ask: 'Help me make sense of this appliance manual.',
				result: 'Plain steps. Fewer mysterious flashing lights.'
			},
			{
				ask: 'Help us find a time for dinner that works for everyone.',
				result: 'A few good options. One less scheduling saga.'
			}
		]
	}
] as const satisfies readonly HeroExampleCategory[];

// Round-robin order shows the breadth immediately and visits every example,
// including when a category later gains more examples than the others.
export const HERO_EXAMPLE_SEQUENCE = Array.from(
	{ length: Math.max(...HERO_EXAMPLE_CATEGORIES.map((category) => category.examples.length)) },
	(_, exampleIndex) => HERO_EXAMPLE_CATEGORIES.flatMap((category, categoryIndex) =>
		exampleIndex < category.examples.length ? [{ categoryIndex, exampleIndex }] : []
	)
).flat();
