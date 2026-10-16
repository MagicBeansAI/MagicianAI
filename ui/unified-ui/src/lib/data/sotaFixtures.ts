/**
 * Shared SOTA fixture list and goal string.
 *
 * The debug page (`routes/(app)/debug/+page.svelte`) is the canonical
 * surface for SOTA test runs with rich metadata (group, suggestedGoal
 * overrides, etc.). The command palette only needs the bare fixture list
 * plus the default goal string for task-backed debug/Internal execution through
 * the v2 execution endpoint — that's what this module exposes.
 *
 * Add new fixtures here when the static files are added to
 * `static/tests/sota-tests/`. Both the palette and the debug page import
 * from this single source.
 */

export const SOTA_FIXTURES = [
	'01-cross-origin-iframe.html',
	'02-codesandbox-only.html',
	'03-dense-grid.html',
	'04-network-errors.html',
	'05-selector-healing.html',
	'06-bot-detection.html',
	'07-loading-states.html',
	'08-silent-failure.html',
	'09-hover-menus.html',
	'10-contrast-labels.html',
	'11-breadcrumbs.html',
	'12-checkout-flow.html',
	'13-payment-form-verification.html',
	'14-code-editor-verification.html',
	'15-cross-origin-typing.html',
	'16-scroll-at-element.html',
	'17-drag-and-drop.html',
	'18-dual-range-slider.html',
	'19-scroll-boundary-edge-cases.html',
	'20-captcha-interaction.html',
	'21-captcha-visual-spatial.html',
	'22-captcha-text-math.html',
	'23-download-upload-cycles.html',
	'24-credential-leak-detection.html',
	'25-diagram-editor-canvas.html',
	'26-whiteboard-spatial-mode.html',
	'27-map-pan-zoom-canvas.html',
	'28-chart-inspector-spatial.html',
	'29-cross-origin-sandboxed-canvas.html',
	'30-collaborative-noisy-board.html',
	'31-hybrid-diagram-connectors.html',
	'32-vision-only-targets.html',
	'33-parent-occlusion.html',
	'34-same-origin-iframe-routing.html',
	'35-screenshot-only-canvas.html',
	'36-live-concept-tutor-math.html',
	'37-live-concept-tutor-physics.html',
	'38-live-concept-tutor-cs.html',
	'39-cropped-capture-slope.html',
	'40-cropped-capture-app-help.html',
	'41-cropped-capture-code-memory.html'
] as const;

export type SotaFixture = (typeof SOTA_FIXTURES)[number];

/**
 * Default agent goal for any SOTA fixture. Composed with the fixture URL
 * at dispatch time as `Navigate to <url> and then <SOTA_GOAL>`.
 *
 * Kept in sync with the version in
 * `routes/(app)/debug/+page.svelte` (search for `SOTA_TEST_GOAL`).
 *
 * **Routing**: relies on the default chat / task agent (typically
 * personal-assistant) and on the tool-description anti-delegation guard
 * in `magician_v2/execution/agentic/native_catalog.rs` ("USE ONLY when …"
 * preambles on `delegate_to_agent` / `handover_to_agent`) to keep the
 * loop on whichever agent owns `browser`. Earlier versions of this
 * prompt carried an explicit `@agent:personal-assistant` mention and a
 * "Do NOT delegate" line; both became redundant once the tool
 * descriptions started carrying the routing constraint.
 */
export const SOTA_GOAL = [
	'Use the browser tool with connection_mode=headed.',
	'Execute every test case on this page one by one, starting with the first visible case and scrolling forward until no cases remain.',
	'Complete each test case by genuinely interacting with its real target elements (scroll them into view, then click / drag / type on the actual elements so the page registers the interaction). Do NOT click the Pass/Fail status controls; those radios are reserved for the human grader.',
	'When you terminate (call `yield`), pass `keep_browser_window_open: true` so the SoTA test page is handed off to the human for inspection.'
].join(' ');

export const LIVE_CONCEPT_TUTOR_SOTA_GOALS: Partial<Record<SotaFixture, string>> = {
	'36-live-concept-tutor-math.html': [
		'Use the browser tool with connection_mode=headed to keep this fixture visible.',
		'Then perform a Live Concept Tutor run for this exact prompt: "@tutor explain the slope relationship in this graph step by step."',
		'Use the current visible screen as the source of truth. Produce a grounded screen-draw overlay sequence: axes/line first, rise/run or measurement labels second, formula last.',
		'The final answer must mention the overlay evidence and keep the browser window open for human inspection by yielding with keep_browser_window_open:true.'
	].join(' '),
	'37-live-concept-tutor-physics.html': [
		'Use the browser tool with connection_mode=headed to keep this fixture visible.',
		'Then perform a Live Concept Tutor run for this exact prompt: "@tutor explain the forces on the block in this diagram step by step."',
		'Use the current visible screen as the source of truth. Produce a grounded screen-draw overlay sequence: body/axes first, force arrows second, net-force or equilibrium explanation last.',
		'The final answer must mention the overlay evidence and keep the browser window open for human inspection by yielding with keep_browser_window_open:true.'
	].join(' '),
	'38-live-concept-tutor-cs.html': [
		'Use the browser tool with connection_mode=headed to keep this fixture visible.',
		'Then perform a Live Concept Tutor run for this exact prompt: "@tutor explain the recursion stack and heap reference shown here step by step."',
		'Use the current visible screen as the source of truth. Produce a grounded screen-draw overlay sequence: code region first, stack frames second, heap/pointer relationship last.',
		'The final answer must mention the overlay evidence and keep the browser window open for human inspection by yielding with keep_browser_window_open:true.'
	].join(' '),
	'39-cropped-capture-slope.html': [
		'Use the browser tool with connection_mode=headed to keep this fixture visible.',
		'Then perform a rect-aware cropped Personal Tutor run for this exact prompt: "@tutor explain the slope relationship in this cropped graph step by step."',
		'Use the staged cropped screen attachment as the source of truth and draw using coordinate_space:"capture" when using crop-local coordinates.',
		'The final answer must mention that the crop was used and keep the browser window open for human inspection by yielding with keep_browser_window_open:true.'
	].join(' '),
	'40-cropped-capture-app-help.html': [
		'Use the browser tool with connection_mode=headed to keep this fixture visible.',
		'Then perform a rect-aware cropped App Copilot run for this exact prompt: "@copilot show me where to click to create a new note, but do not click it."',
		'Use the staged cropped screen attachment as the source of truth and draw using coordinate_space:"capture" when using crop-local coordinates.',
		'The final answer must mention the highlighted control and keep the browser window open for human inspection by yielding with keep_browser_window_open:true.'
	].join(' '),
	'41-cropped-capture-code-memory.html': [
		'Use the browser tool with connection_mode=headed to keep this fixture visible.',
		'Then perform a rect-aware cropped Live Concept Tutor run for this exact prompt: "@tutor explain how the stack frame points to the heap object in this crop."',
		'Use the staged cropped screen attachment as the source of truth and draw using coordinate_space:"capture" when using crop-local coordinates.',
		'The final answer must mention the overlay evidence and keep the browser window open for human inspection by yielding with keep_browser_window_open:true.'
	].join(' ')
};

export const SOTA_FIXTURE_ENV_MODES: Partial<Record<SotaFixture, string | null>> = {
	'36-live-concept-tutor-math.html': null,
	'37-live-concept-tutor-physics.html': null,
	'38-live-concept-tutor-cs.html': null,
	'39-cropped-capture-slope.html': null,
	'40-cropped-capture-app-help.html': null,
	'41-cropped-capture-code-memory.html': null
};

export const LIVE_CONCEPT_TUTOR_CHAT_PROMPTS: Partial<Record<SotaFixture, string>> = {
	'36-live-concept-tutor-math.html': [
		'@tutor explain the slope relationship in this graph step by step.',
		'Use the current visible screen as the source of truth.',
		'Produce a grounded overlay sequence: axes/line first, rise/run or measurement labels second, formula last.'
	].join(' '),
	'37-live-concept-tutor-physics.html': [
		'@tutor explain the forces on the block in this diagram step by step.',
		'Use the current visible screen as the source of truth.',
		'Produce a grounded overlay sequence: body/axes first, force arrows second, net-force or equilibrium explanation last.'
	].join(' '),
	'38-live-concept-tutor-cs.html': [
		'@tutor explain the recursion stack and heap reference shown here step by step.',
		'Use the current visible screen as the source of truth.',
		'Produce a grounded overlay sequence: code region first, stack frames second, heap/pointer relationship last.'
	].join(' '),
	'39-cropped-capture-slope.html': [
		'@tutor explain the slope relationship in this cropped graph step by step.',
		'Use the attached cropped screen capture as the source of truth.',
		'When drawing from crop-local coordinates, use coordinate_space:"capture" so the overlay maps through the stored crop rect.'
	].join(' '),
	'40-cropped-capture-app-help.html': [
		'@copilot show me where to click to create a new note, but do not click it.',
		'Use the attached cropped screen capture as the source of truth.',
		'When drawing from crop-local coordinates, use coordinate_space:"capture" so the overlay maps through the stored crop rect.'
	].join(' '),
	'41-cropped-capture-code-memory.html': [
		'@tutor explain how the stack frame points to the heap object in this crop.',
		'Use the attached cropped screen capture as the source of truth.',
		'When drawing from crop-local coordinates, use coordinate_space:"capture" so the overlay maps through the stored crop rect.'
	].join(' ')
};

export const SOTA_REGION_CAPTURE_SELECTORS: Partial<Record<SotaFixture, string>> = {
	'39-cropped-capture-slope.html': '[data-sota-crop-target="slope-panel"]',
	'40-cropped-capture-app-help.html': '[data-sota-crop-target="app-toolbar"]',
	'41-cropped-capture-code-memory.html': '[data-sota-crop-target="code-memory"]'
};

export function sotaRegionCaptureSelectorForFixture(file: string): string | null {
	return SOTA_REGION_CAPTURE_SELECTORS[file as SotaFixture] ?? null;
}

export function sotaGoalForFixture(file: string): string {
	return LIVE_CONCEPT_TUTOR_SOTA_GOALS[file as SotaFixture] ?? SOTA_GOAL;
}

export function sotaDebugGoalForFixture(file: string): string {
	return LIVE_CONCEPT_TUTOR_CHAT_PROMPTS[file as SotaFixture] ?? sotaGoalForFixture(file);
}

export function sotaEnvModeForFixture(file: string): string | null {
	const fixture = file as SotaFixture;
	if (Object.prototype.hasOwnProperty.call(SOTA_FIXTURE_ENV_MODES, fixture)) {
		return SOTA_FIXTURE_ENV_MODES[fixture] ?? null;
	}
	return 'browser';
}

/**
 * Title-case display name for a SOTA fixture filename. E.g.
 *   `17-drag-and-drop.html` → `17 · Drag And Drop`
 */
export function sotaDisplayName(file: string): string {
	const num = file.slice(0, 2);
	const base = file
		.replace(/\.html$/, '')
		.replace(/^\d+-/, '')
		.replace(/-/g, ' ')
		.replace(/\b\w/g, (c) => c.toUpperCase());
	return `${num} · ${base}`;
}
