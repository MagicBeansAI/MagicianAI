/**
 * Desktop Grounding SOTA tests — live-app eval cases for the pixel
 * grounding recipe (`POST /screen/ground` + zoom refine + `from_zoom`
 * click; plan: `docs/plans/2026-06-12-screen-grounding.md`).
 *
 * Unlike the browser SOTA fixtures (static HTML under
 * `static/tests/sota-tests/`), the "fixture" here is a real macOS app
 * whose CONTENT area exposes no AX tree — verified empirically
 * 2026-06-12: Freeform exposes ONLY the menu bar (even its toolbar is
 * AX-empty), Maps exposes controls but not the map canvas, Stocks
 * exposes no per-point chart elements. Chess is the inverse — every
 * piece is a labeled AXButton — which makes it the CALIBRATION case:
 * the AX position is automatic ground truth for the model's pixel
 * answer.
 *
 * Dispatch: the debug page runs these through the same agentic loop as
 * the browser SOTA tests, but with `env_mode = 'shell'` and NO
 * navigate-prefix — the goal is self-contained (the recipe contract is
 * embedded via GROUNDING_RECIPE_PREAMBLE so the raw loop doesn't depend
 * on skill activation).
 *
 * Scoring is the plan's G4 gate: each case names its expected EFFECT
 * before the click; the agent reports PASS/FAIL + the audit line
 * (target, coords, confidence, effect verdict). ≥90% across the suite
 * parks the local-specialist (OpenCUA) question permanently.
 */

export interface DesktopGroundingTest {
	id: string;
	name: string;
	app: 'Freeform' | 'Maps' | 'Stocks' | 'Chess';
	description: string;
	goal: string;
}

/**
 * The recipe contract, embedded in every goal so the raw debug loop
 * (shell env, no skill activation) executes the SAME steps the
 * `macos-ui-automation` skill prescribes. Kept in sync with the skill's
 * "Grounding fallback" section.
 */
export const GROUNDING_RECIPE_PREAMBLE = [
	'Use ONLY pixel grounding for clicks on the app content — never AX element clicks for the named target (this is a grounding eval).',
	'The recipe, exactly: ',
	'(1) Ensure the cua-driver daemon: `open -n -g -a CuaDriver --args serve` (idempotent), binary at ~/.local/bin/cua-driver.',
	'(2) Find the window: `cua-driver call list_windows \'{}\'` → pid + window_id of the app\'s main on-screen window.',
	'(3) Capture the frame: `cua-driver call get_window_state \'{"pid":P,"window_id":W,"include_accessibility_tree":false}\' --screenshot-out-file /tmp/win.png` → note screenshot_width/height, the CLICK SPACE (the saved image is that size; no AX walk).',
	'(4) Quote every JSON argument in single quotes — unquoted, the shell brace-expands it.',
	'(5) Coarse ground: POST http://localhost:3002/api/magician/v2/screen/ground with JSON {"target": "<plain-language target>", "image_b64": "<base64 of /tmp/win.png>"} and header Authorization: Bearer $MAGICIAN_BEARER_TOKEN → {found, x, y, confidence, image_width, image_height}. If found=false or confidence<0.5: STOP and report — never click a guess.',
	'(6) Scale to click space: sx = x*screenshot_width/image_width, sy = y*screenshot_height/image_height.',
	'(7) Zoom refine (MANDATORY): `cua-driver call zoom \'{"pid":P,"window_id":W,"x1":sx-100,"y1":sy-55,"x2":sx+100,"y2":sy+55}\' --screenshot-out-file /tmp/crop.jpg`, then ground AGAIN on the crop (JPEG is accepted) → corrected (x,y) in crop space.',
	'(8) Click: `cua-driver call click \'{"pid":P,"window_id":W,"x":X,"y":Y,"from_zoom":true}\'` immediately.',
	'(9) Verify the NAMED EFFECT (not the click), with a fresh capture + POST /screen/describe ({"question": ..., "image_b64": ...}) or the app\'s AppleScript where stated. One retry from step 7 on failure.',
	'(10) Report PASS or FAIL plus the audit line: target, final coords, confidence, effect verdict. Restore any state you changed.'
].join(' ');

export const DESKTOP_GROUNDING_TESTS: DesktopGroundingTest[] = [
	// ── Freeform — the gold surface: even the TOOLBAR is AX-empty ──────
	{
		id: 'dg-freeform-sticky',
		name: 'Freeform · Sticky-Note Roundtrip',
		app: 'Freeform',
		description:
			'Place a sticky note via the (AX-empty) toolbar, label it, deselect, then re-select it by grounding. Effect: selection handles around the GROUND-A note.',
		goal: [
			'Open the Freeform app (`open -a Freeform`) and create a new board (⌘N via `osascript -e \'tell application "System Events" to keystroke "n" using command down\'` after activating Freeform).',
			'Using the grounding recipe, click the STICKY-NOTE tool in the toolbar (its toolbar exposes no AX — grounding is the only path), then click an empty spot on the canvas to place the note, type GROUND-A, then click a distant empty canvas area to deselect.',
			'Now the scored step: ground and click the sticky note that says GROUND-A. Expected effect (name it before clicking): the note gains selection handles.',
			'Verify with a fresh capture + /screen/describe asking whether the GROUND-A note is selected. Report PASS/FAIL + audit line. Close the board tab/window afterwards without saving if prompted.'
		].join(' ')
	},
	{
		id: 'dg-freeform-shape',
		name: 'Freeform · Shape Select',
		app: 'Freeform',
		description:
			'Insert a shape via the toolbar, deselect, re-select it by grounding click. Effect: selection handles on the shape.',
		goal: [
			'In a new Freeform board (open the app, ⌘N), use the grounding recipe to click the SHAPES tool in the toolbar, pick any circle/round shape from the picker (ground it too — the picker is also AX-empty), and place it on the canvas; click far away on empty canvas to deselect.',
			'Scored step: ground and click the circle shape on the canvas. Expected effect: selection handles appear around it.',
			'Verify via fresh capture + /screen/describe. Report PASS/FAIL + audit line. Clean up the board afterwards.'
		].join(' ')
	},
	{
		id: 'dg-freeform-tool',
		name: 'Freeform · Toolbar Tool State',
		app: 'Freeform',
		description:
			'Activate the pen/draw tool purely by grounding. Effect: the tool shows as active (highlighted) in the toolbar.',
		goal: [
			'In Freeform (new board), scored step: ground and click the PEN / drawing tool in the bottom toolbar. Expected effect: the pen tool becomes the active tool (highlighted) and a drawing style strip appears.',
			'Verify via fresh capture + /screen/describe asking which toolbar tool is active. Report PASS/FAIL + audit line. Press Escape and close the board afterwards.'
		].join(' ')
	},

	// ── Maps — controls have AX, the map canvas does not ─────────────
	{
		id: 'dg-maps-landmark',
		name: 'Maps · Landmark Label Click',
		app: 'Maps',
		description:
			'Search a landmark, then click its LABEL on the map canvas (not the sidebar). Effect: the place card opens.',
		goal: [
			'Open the Maps app and search for "Cubbon Park" (the search field HAS AX — using it is fine; the scored click is on the canvas). Press Return and wait ~3s for the map to settle, then press Escape or click on the map area margin so the sidebar/search results are dismissed but the landmark label remains visible on the MAP.',
			'Scored step: ground and click the "Cubbon Park" LABEL/pin on the map canvas itself. Expected effect: the place card for Cubbon Park opens.',
			'Verify: fresh `get_window_state` — the opened place card DOES gain AX elements (its title appears in the tree), or use /screen/describe. Report PASS/FAIL + audit line. Close the card and quit Maps afterwards.'
		].join(' ')
	},
	{
		id: 'dg-maps-poi',
		name: 'Maps · POI Discovery Click',
		app: 'Maps',
		description:
			'Pick any visible restaurant/cafe icon on the map canvas and open it by grounding. Effect: a place card opens naming the POI.',
		goal: [
			'Open the Maps app on its current/default city view, zoom in one or two steps (the +/- controls have AX; fine to use) until individual restaurant/cafe POI icons are visible on the map canvas.',
			'Scored step: pick ONE clearly visible food/cafe POI icon, state which one you are targeting (its label text), then ground and click it on the canvas. Expected effect: a place card opens whose title matches the POI you named.',
			'Verify via fresh get_window_state (card gains AX) or /screen/describe; the card title must match your named target — a card for a DIFFERENT place is a FAIL (that is a mis-grounded click). Report PASS/FAIL + audit line. Quit Maps afterwards.'
		].join(' ')
	},

	// ── Stocks — chart exposes no per-point AX ────────────────────────
	{
		id: 'dg-stocks-peak',
		name: 'Stocks · Chart Peak Scrub',
		app: 'Stocks',
		description:
			'Click on the price chart near its highest peak. Effect: the scrub readout shows a date/price at that position.',
		goal: [
			'Open the Stocks app, select any symbol from the (AX-exposed) sidebar, and switch the chart to the 1Y range (range buttons may have AX; fine to use).',
			'Scored step: ground and click ON THE PRICE CHART at its highest visible peak. Expected effect: a scrub marker/readout appears showing the date and price at the clicked position.',
			'Verify via fresh capture + /screen/describe: ask what date/price the readout shows and whether the marker sits at the chart\'s peak region. Report PASS/FAIL + audit line + the readout values. Quit Stocks afterwards.'
		].join(' ')
	},
	{
		id: 'dg-stocks-early',
		name: 'Stocks · Chart Left-Edge Scrub',
		app: 'Stocks',
		description:
			'Click the earliest (left-edge) region of the chart. Effect: the readout shows a date near the range start.',
		goal: [
			'In the Stocks app with any symbol on the 1Y chart: scored step — ground and click the chart line near its LEFT edge (the earliest dates).',
			'Expected effect: the scrub readout shows a date close to one year ago. Verify via fresh capture + /screen/describe (ask for the readout date); a date in the most recent quarter is a FAIL (mis-grounded x).',
			'Report PASS/FAIL + audit line + the readout date. Quit Stocks afterwards.'
		].join(' ')
	},

	// ── Chess — fully AX-exposed: the CALIBRATION case ────────────────
	{
		id: 'dg-chess-calibration',
		name: 'Chess · Grounding Calibration (AX ground truth)',
		app: 'Chess',
		description:
			'Ground "the white queen" by pixels and score the answer against the AX element\'s known position — automatic ground truth, no human judging. No click needed.',
		goal: [
			'Open the Chess app (a fresh game shows the standard start position). Take get_window_state: the board IS fully AX-exposed — find the element "white queen, d1" and note the board geometry (the 8x8 grid spans the window; derive d1\'s square center from the window/board bounds, or click-free: just note which square the queen occupies).',
			'Scored step: capture the window frame and POST /screen/ground with target "the white queen on the chess board". Convert the answer to click space.',
			'Score WITHOUT clicking: PASS if the grounded point falls inside the white queen\'s square (d1 at game start), FAIL otherwise. Report the pixel offset between the grounded point and the square center — this number is the raw grounding-error measurement.',
			'Report PASS/FAIL + the offset + confidence. Quit Chess afterwards.'
		].join(' ')
	}
];
