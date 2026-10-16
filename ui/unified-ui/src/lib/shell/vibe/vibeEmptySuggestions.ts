import type { IconName } from '$lib/shared/icons/paths';

export interface VibeEmptySuggestion {
	icon: IconName;
	label: string;
	hint: string;
	prompt: string;
}

export const VIBE_EMPTY_SUGGESTIONS: readonly VibeEmptySuggestion[] = [
	{
		icon: 'sparkle',
		label: 'Habit-tracker PWA',
		hint: 'Streaks, local storage & progress charts',
		prompt: 'Build a habit-tracker PWA with streaks, local storage and progress charts'
	},
	{
		icon: 'moon',
		label: 'Dark mode & theme',
		hint: 'Theme toggle with smooth CSS transitions',
		prompt: 'Add dark mode support with smooth theme transitions and color tokens'
	},
	{
		icon: 'git-branch',
		label: 'Wire up auth',
		hint: 'User login, session management & guards',
		prompt: 'Wire up user authentication with session management and protected routes'
	},
	{
		icon: 'monitor',
		label: 'Landing page & pricing',
		hint: 'Hero section, features and pricing tiers',
		prompt: 'Create a responsive landing page with a hero section, features and pricing tiers'
	}
];
