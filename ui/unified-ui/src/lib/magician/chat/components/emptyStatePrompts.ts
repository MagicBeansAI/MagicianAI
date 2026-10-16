import type { IconName } from '$lib/shared/icons/paths';

// Suggested prompts shown when a chat session has no messages yet, shared by
// the classic `ChatEmptyState` and chat-next's `ChatEmptyStateNext`. Voice
// and several prompts are borrowed from the landing page's sample prompts.
// Each maps to a real capability: research, meetings, ordering, screen
// watching, coding (VibeDev) and briefings.
export const EMPTY_STATE_PROMPTS: readonly { icon: IconName; label: string; prompt: string }[] = [
	{
		icon: 'search',
		label: 'Research flights',
		prompt: 'Find the cheapest Kyoto flights for next month and shortlist the three best options.'
	},
	{
		icon: 'calendar',
		label: 'Catch up on a meeting',
		prompt: 'What did I miss in yesterday’s design sync? Decisions, and anything I owe people.'
	},
	{
		icon: 'rotate-ccw',
		label: 'Restock the groceries',
		prompt: 'We’re out of groceries. Order my usual list and stay within my spending limit.'
	},
	{
		icon: 'monitor',
		label: 'Review my screen time',
		prompt: 'Look at my recent screen observations and summarize where my time went today.'
	},
	{
		icon: 'git-branch',
		label: 'Build a small app',
		prompt: 'Build me a small web app — a habit tracker to start.'
	},
	{
		icon: 'inbox',
		label: 'Morning briefing',
		prompt: 'Give me a briefing: calendar, inbox, and anything I missed since yesterday.'
	}
];
