/**
 * Desktop App Copilot SOTA tests — live macOS app onboarding/help cases for the
 * shared draw/observe/action rolling loop:
 *
 * observe -> resolve -> draw/explain -> check user action -> act immediately unless preempted -> observe -> verify.
 *
 * These are not browser fixtures. They launch real desktop apps, require copilot
 * invoke markers, and intentionally run with the full agent tool set so the
 * chat runtime can use runtime tools, screen-draw, and delegated mac-operator
 * actions. The prompts stay app-scenario based; the agent must resolve current
 * state from fresh observations instead of following stale coordinates.
 */

export interface DesktopAppTutorTest {
	id: string;
	name: string;
	app: 'Notes' | 'Calculator' | 'TextEdit' | 'Music';
	description: string;
	goal: string;
}

export const DESKTOP_APP_TUTOR_CONTRACT = [
	'This is an App Copilot SOTA run. @copilot is the hybrid action rail; @tutor is the explanation-only rail.',
	'Use the live app state as source of truth: observe, draw or label the next visible target, explain briefly, check whether the user already acted, automate immediately when they have not, and let backend preemption cancel automation if the user acts while it is queued or running.',
	'Refresh coordinates after any app state change and replace stale overlay marks.',
	'Only clean up items created during this run; never alter preexisting user content.',
	'Finish with PASS or FAIL and a short audit line.'
].join(' ');

export const DESKTOP_APP_TUTOR_TESTS: DesktopAppTutorTest[] = [
	{
		id: 'dt-notes-new-note-onboarding',
		name: 'Notes · New Note Guided Onboarding',
		app: 'Notes',
		description:
			'Walk through creating a temporary note, show where title/body entry happens, type demo content, verify it, then clean up only the created note.',
		goal: [
			'@copilot teach the user how to create a new note in Notes.',
			'Create one temporary note titled with the prefix TUTOR-SOTA-NOTES, show where the title and body go, enter one short body line, verify it is visible, then clean up only that temporary note.'
		].join(' ')
	},
	{
		id: 'dt-calculator-guided-sum',
		name: 'Calculator · Guided Keypad Demo',
		app: 'Calculator',
		description:
			'Use Calculator as a harmless app demo: explain display/keypad, press a simple expression through tutor actions, and verify the result.',
		goal: [
			'@copilot teach the user how Calculator display and keys work.',
			'Demonstrate by calculating 7 + 5 with visible UI actions, drawing before the key sequence and verifying the display shows 12.'
		].join(' ')
	},
	{
		id: 'dt-textedit-typing-help',
		name: 'TextEdit · Writing Area Help',
		app: 'TextEdit',
		description:
			'Open a temporary TextEdit document, show where text goes, type a short sample, verify it, then close without saving.',
		goal: [
			'@copilot teach the user where to type in TextEdit.',
			'Use a new run-owned unsaved document, highlight the writing area, type "Tutor SOTA writing area demo.", verify the line is visible, then close only that temporary document without saving.'
		].join(' ')
	},
	{
		id: 'dt-music-playback-controls-help',
		name: 'Apple Music · Playback Controls Help',
		app: 'Music',
		description:
			'Explain the main Music window and playback controls, then safely demonstrate play/pause only on visible existing content.',
		goal: [
			'@copilot teach the user what the main Music window and playback controls do.',
			'Label the sidebar/content, playback controls, and now-playing area. If safe existing playable content is already visible, demonstrate play/pause and restore the prior state; otherwise stay visual-only for the unsafe step. Do not modify library, account, playlists, purchases, or subscriptions.'
		].join(' ')
	}
];
