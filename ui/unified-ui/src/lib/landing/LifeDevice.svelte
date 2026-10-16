<script lang="ts">
	// THE DEVICE, SUPPLIED BY THE DOM.
	//
	// The montage's compositing contract already conceded the screen: a
	// generated screen is mush — invented interfaces, gibberish text,
	// flickering geometry — so the video supplies the room and the DOM supplies
	// what is on the glass. The first paid plate then proved the concession did
	// not go far enough. It missed four of six requirements, all of them about
	// HARDWARE: a lit Apple logo, a screen that was not blank, a push-in that
	// carried the laptop out of frame by three seconds, and the requirement
	// nobody had thought to write down until footage existed — the lid faced
	// away from camera, so the screen was never visible at all.
	//
	// So the concession goes one step further and the DOM supplies the whole
	// device. Every hardware constraint leaves the prompt at once: nothing can
	// be branded, mis-angled, lit or dollied out of frame if it was never
	// filmed. What remains for the plate is a room, some light and some people
	// living in it, which is the one thing these models are genuinely good at.
	//
	// WHAT THIS COSTS, and what pays it back. A fake device in a real room is
	// only convincing if it is lit like an object in that room. Five things
	// sell it, in descending order of how badly each gives the trick away when
	// missing, and all five are here:
	//
	//   0. A CONTACT SHADOW. Not in the contract's list of five, and on the
	//      first composite it was the worst offender by a distance: with the
	//      spill lighting the counter and nothing darkening beneath the device,
	//      the laptop hovered. An object resting on a surface OCCLUDES the
	//      room's light as well as adding its own, and the eye reads the
	//      occlusion first. Cheap to draw, and it is what puts the thing on
	//      the table rather than in front of it.
	//   1. GLOW SPILL — the light the screen throws onto the table. The
	//      biggest tell the contract did list, and the one thing a pasted
	//      rectangle can never fake.
	//   2. PERSPECTIVE — a real screen sits at the device's angle. A homography,
	//      not a rotation: under perspective the far edge is SHORTER than the
	//      near one, which no composition of rotate/scale can express.
	//   3. GLARE laid over our content, along the room's own light direction.
	//   4. WHITE BALANCE — our sRGB-blue UI must warm into a golden kitchen
	//      rather than glow out of it.
	//   5. GRAIN, matched to the plate.
	//
	// THE COMPOSITION IS THE DEVICE, NOT THE ROOM (owner reference, 2026-08-06).
	//
	// The plan assumed a WIDE DOMESTIC SHOT — a room, people living in it, a
	// laptop somewhere on a table — and derived a rule from that: at the
	// 200–400px a laptop occupies in such a frame, dense UI is unreadable mush,
	// so screen content must be one idea, large.
	//
	// The reference the owner actually wants inverts the frame. The LAPTOP is
	// the subject, filling most of the width, near square-on with a few degrees
	// of rotation, and CROPPED BY THE BOTTOM EDGE. The person doing what they
	// love is background-left, mid-distance, soft — secondary by contrast and
	// focus rather than by size. Three consequences, all of them helpful:
	//
	//   · THE SCREEN IS NOW 700–1000px, so real, dense product UI is legible.
	//     "One idea, large" was correct for a shot we are no longer making; at
	//     this size it reads as a toy. What sells the claim that it is working
	//     is that it visibly IS working — a diff, a plan, an agent panel mid-run.
	//   · CROPPING AT THE BOTTOM REMOVES THE HARDEST TELL. The contact shadow
	//     was the worst offender on the first composite; frame the contact point
	//     out and the problem does not exist. The shadow is kept for scenes
	//     where the foot is visible and is simply absent otherwise.
	//   · THE GRADE MUST BE LIGHT. In the reference, the screen stays neutral
	//     and bright against full daylight and reads correctly — because a
	//     display is a light SOURCE, not a lit surface. Warming it into the room
	//     is the mistake, not the fix.
	import { gradeColour, gradeStrength, quadBounds, quadCentre, quadToMatrix3d } from './lifeComposite';
	import type { LifeGeom, LifeScene, Quad } from './lifeComposite';
	import laptopGeom from './laptopGeom.json';

	// THE DEVICE IS A PHOTOGRAPH NOW, not CSS.
	//
	// The drawn laptop got as far as "recognisably a laptop" and stopped there:
	// the deck read as grey card whatever was done to its gradient, and the
	// hinge, the chamfer, the way aluminium actually catches a room are not
	// things a few box-shadows reach. This is a generated studio plate of an
	// unbranded space-grey machine with the screen OFF — which is the one thing
	// generative imaging is reliable at, because it is a single still that can
	// be inspected and re-rolled for pennies rather than a moving hardware
	// render that has to survive five seconds.
	//
	// Its screen area was MEASURED off the asset rather than eyeballed: the
	// largest contiguous near-black run, which is the dead screen. Everything
	// below hangs off those four numbers.
	// SHOT DEAD HEAD-ON, at screen height. The first asset was photographed
	// slightly from above with the lid reclined past 100°, so you saw a lot of
	// foreshortened deck and the whole machine read as LEANING BACK — which
	// looks like a tilt even though the transform carries no rotation at all
	// (the quad is axis-aligned and the matrix is pure scale-and-translate).
	// The fix was never a rotation to cancel; it was a photograph taken level
	// with an upright lid.
	// Geometry travels WITH the asset. The device is a photograph, and
	// photographs get re-rolled — each replacement has a different screen rect,
	// and hand-measuring it into four constants here is how a stale number ends
	// up stretching the machine in every scene. `scripts/life_laptop.py`
	// measures the dead screen and writes `laptop.json` beside the PNG.
	const LAPTOP = { src: '/landing/laptop.png', ...laptopGeom };

	/** The design box the screen face is authored in, before the homography. */
	const FACE_W = LAPTOP.sw;
	const FACE_H = LAPTOP.sh;

	let geom: LifeGeom | null = null;
	/**
	 * How far up the PLATE is, 0..1 — the same fade the footage rides.
	 *
	 * The device used to mount at full opacity the instant the reel had
	 * geometry, which put a laptop in empty paper, ticking tasks off, before
	 * the room it stands in had appeared. It fades with the picture, and its
	 * WORK does not begin until the picture is mostly there: `gate` below is
	 * what holds the first scene's screen still until the fishing plate is
	 * actually on screen to hold it.
	 */
	let fade = 1;
	/**
	 * WHAT CAN ACTUALLY BE SEEN, which is not the same as this layer's own
	 * opacity. The device sits inside the montage's frame, so the visitor sees
	 * the frame's fade multiplied by the plate's — and gating the work on
	 * `fade` alone had the laptop ticking steps off behind a frame still at two
	 * percent. The caller passes the product; the work waits on that.
	 */
	let vis = 1;
	/** 0..1 within the active scene — drives whatever the screen is doing. */
	let u = 0;
	/**
	 * THE SCREEN RUNS ON ITS OWN CLOCK, and faster than the scene does.
	 *
	 * The plate wants to be slow: it is a person fishing, and the station's
	 * weight was tripled twice so a visitor can dwell in it. But everything on
	 * the LAPTOP was keyed to the same progress, so tripling the scroll also
	 * tripled how long the machine took to tick a task off — which says the
	 * opposite of what the beat means. Magican finishing three jobs while someone
	 * casts a line once is the whole claim; a spinner that takes a third of a
	 * scene to resolve is a slow machine in a slow film.
	 *
	 * So the work clock runs at `WORK_RATE`× the scene's. The screen keeps its
	 * ordinary tempo — things land, complete and are replaced — while the
	 * world around it moves at the pace of an afternoon. It also reads as time
	 * DILATION, which is exactly the feeling: the machine is quick, and the
	 * day is long.
	 *
	 * (The CSS transitions were never the problem — those are real seconds and
	 * were always at normal speed. What slowed was the arrival CADENCE, which
	 * is all keyed to this.)
	 */
	const WORK_RATE = 3.2;

	/**
	 * TWO OR THREE CASES PER SCENE, not one surface held for the whole plate.
	 *
	 * With the station's weight at 25.2 a single surface finished its work in
	 * the first stretch and then sat there — the very complaint the slow-down
	 * was meant to fix, moved one level up. What a person is away FROM is not
	 * one task, it is a morning: a follow-up handled, then a question answered,
	 * then something built. So a scene names the surfaces it walks through and
	 * the screen moves between them, each taking an equal share of the plate.
	 *
	 * `w` restarts inside every case, so each plays its own arc at the
	 * machine's tempo instead of one arc stretched over the whole scene.
	 */
	/** Nothing on the screen moves until the SHOT is ~80% up — frame and plate
	 *  together, not this layer's own opacity. */
	$: gate = vis > 0.8;
	$: cases = (scene?.cases?.length ? scene.cases : [scene?.screen ?? 'chat']) as string[];
	$: caseIdx = Math.min(cases.length - 1, Math.floor(u * cases.length));
	$: view = cases[caseIdx] ?? 'chat';
	/** 0..1 inside the CURRENT case. */
	$: cu = gate ? Math.min(1, Math.max(0, u * cases.length - caseIdx)) : 0;
	$: w = gate ? Math.min(1, cu * WORK_RATE) : 0;

	/**
	 * THE PANEL BELONGS TO THE CASE, not to the scene.
	 *
	 * Steps and Cost were the scene's one `rail` and a hard-coded figure, so
	 * the right-hand column said exactly the same thing while the left-hand
	 * pane moved between Today, a task list and a thread — which reads as a
	 * panel that is not connected to anything. It should describe whatever is
	 * on screen: the follow-ups Today is showing, the queue the task list is
	 * draining, the run the thread is reporting.
	 */
	$: panelSteps =
		view === 'today'
			? (scene?.feed ?? []).map((c) => c.title)
			: view === 'tasks'
				? [...(scene?.rail ?? RAIL), ...(scene?.queued ?? [])]
				: (scene?.rail ?? RAIL);

	/**
	 * And the cost CLIMBS. A constant `$0.018 · 7 calls` under work that is
	 * visibly still happening says the meter stopped — the same mistake the
	 * drowning's counters made at the pivot. It accumulates across the whole
	 * scene, not the case, because the bill is for the morning.
	 */
	/** The chat's live task card: its steps, and how many have finished. */
	$: rail = (scene?.rail ?? RAIL).slice(0, 4);
	$: stepDone = rail.filter((_: string, i: number) => w > 0.24 + i * 0.11).length;
	$: chatLine =
		w > 0.5
			? 'Finished'
			: 'Step ' + Math.min(rail.length, stepDone + 1) + ' of ' + rail.length + ' · ' + (rail[Math.min(rail.length - 1, stepDone)] ?? '');

	$: spend = (scene?.spend ?? 0.018) * (0.25 + 0.75 * u);
	$: calls = Math.max(1, Math.round((scene?.calls ?? 7) * (0.2 + 0.8 * u)));
	$: mins = ((scene?.minutes ?? 3.2) * (0.15 + 0.85 * u)).toFixed(1);

	/**
	 * Called from MovieTrack's rAF with `LifeReel.geom()`. This component owns
	 * no loop: the reel is already being ticked, and a second timer for the
	 * thing painted on top of it is a second timer to keep in sync.
	 */
	export function place(g: LifeGeom | null, alpha = 1, seen = alpha): void {
		geom = g;
		fade = alpha;
		vis = seen;
		if (g && scene) {
			const span = Math.max(1, scene.end - scene.start);
			u = Math.min(1, Math.max(0, (g.frame - scene.start) / span));
		}
	}

	// The scene whose frame range contains the playhead. A montage is a
	// sequence of plates in one frame run, so "which scene" is a lookup, not
	// state — and a plate with no authored quad simply has no device, which is
	// the same absence contract the reel itself follows for missing footage.
	$: scene = geom
		? (geom.scenes.find((s) => geom!.frame >= s.start && geom!.frame <= s.end) ?? null)
		: null;
	$: quad = (scene?.screenQuad ?? null) as Quad | null;

	// Frame pixels → CSS pixels. The plate is `contain`-fitted inside the
	// canvas, so everything authored against the footage has to travel through
	// the same fit or the device drifts off its table the moment the viewport
	// changes aspect.
	$: fit = geom ? geom.dw / geom.nw : 1;

	$: faceMatrix = quad ? quadToMatrix3d(FACE_W, FACE_H, quad) : 'none';
	$: bounds = quad ? quadBounds(quad) : null;
	$: centre = quad ? quadCentre(quad) : null;
	$: light = scene?.lightSample ?? [255, 255, 255];
	$: grade = gradeColour(light as [number, number, number]);
	$: gradeAlpha = gradeStrength(light as [number, number, number]);

	// THE LID'S BASE, derived when it is not authored. A laptop is a screen
	// plus a deck, and the deck's quad is fully determined by the screen's
	// bottom edge plus how far it runs toward the viewer — so a scene works the
	// moment its screenQuad is placed, and `baseQuad` exists for the shots
	// where the derivation is not flattering enough.
	function derivedBase(q: Quad): Quad {
		const bl = q[3];
		const br = q[2];
		const tl = q[0];
		const tr = q[1];
		// "Down the screen" as the plate sees it, averaged over both sides, so
		// the deck follows the lid's own lean rather than the frame's axes.
		const dx = (bl[0] - tl[0] + (br[0] - tr[0])) / 2;
		const dy = (bl[1] - tl[1] + (br[1] - tr[1])) / 2;
		// A deck is about as deep as the lid is tall, but seen at a glancing
		// angle it foreshortens hard. It also widens toward the viewer.
		const D = 0.46;
		const SPREAD = 0.06;
		const wx = (br[0] - bl[0]) * SPREAD;
		const wy = (br[1] - bl[1]) * SPREAD;
		return [
			bl,
			br,
			[br[0] + dx * D + wx, br[1] + dy * D + wy],
			[bl[0] + dx * D - wx, bl[1] + dy * D - wy]
		];
	}
	$: baseQuad = quad ? ((scene?.baseQuad ?? derivedBase(quad)) as Quad) : null;
	$: baseMatrix = baseQuad ? quadToMatrix3d(FACE_W, FACE_H, baseQuad) : 'none';

	// THE ARGUMENT, IN THREE BEATS. The reference puts it bottom-left over the
	// plate, and its structure is the whole film in miniature: what the machine
	// is doing (with a duration, because duration is the proof), who the person
	// is, and what they are doing INSTEAD. This proof earns the “Do what you
	// love” transformation that follows the film, once per scene.
	// Fallbacks only. Every real scene supplies its own — see `life_scenes.json`.
	const LINES: { k: string; t: string }[] = [
		{ k: '', t: 'depart  10:40  NRT' },
		{ k: '-', t: 'seat    unassigned' },
		{ k: '+', t: 'seat    14A · aisle' },
		{ k: '', t: 'fare    held 20 min' },
		{ k: '+', t: 'hotel   4 nights, Gion' },
		{ k: '', t: 'transfer  Haruka express' },
		{ k: '+', t: 'dinner  Wed · booked' }
	];
	const RAIL = [
		'read the fare table',
		'compared 3 carriers',
		'held 14A',
		'drafting the itinerary'
	];

	/** The spill's reach, in frame px — a screen lights a table about its own width. */
	$: spillR = bounds ? Math.max(bounds.w, bounds.h) * 1.35 : 0;
	// Where the device MEETS the table: the near edge of the deck, which is the
	// only part of it actually touching anything.
	$: foot = baseQuad
		? {
				x: (baseQuad[2][0] + baseQuad[3][0]) / 2,
				y: (baseQuad[2][1] + baseQuad[3][1]) / 2,
				w: Math.hypot(baseQuad[2][0] - baseQuad[3][0], baseQuad[2][1] - baseQuad[3][1])
			}
		: null;
</script>

{#if geom && quad && bounds && centre}
	<!-- One scaled coordinate space, so everything inside is authored in the
	     plate's own frame pixels and nothing has to know the viewport. -->
	<div
		class="ld"
		style="transform: translate({geom.dx}px, {geom.dy}px) scale({fit});
		       opacity: {fade.toFixed(3)};"
		aria-hidden="true"
	>
		<!-- 0 · THE CONTACT SHADOW, beneath everything. Multiply, because this is
		     light being BLOCKED — a screen-blended dark patch would do nothing at
		     all, and a plain translucent black would fog the counter rather than
		     shade it. Tight and dark at the foot, falling away fast: a laptop
		     touches its table along one narrow edge. -->
		{#if foot}
			<div
				class="ld-contact"
				style="left: {foot.x}px; top: {foot.y}px; width: {foot.w * 1.15}px;
				       height: {foot.w * 0.3}px;"
			></div>
		{/if}

		<!-- 1 · THE SPILL, over the shadow and under the device. Screen blend,
		     because light ADDS to a room; a translucent white overlay would
		     wash the table out instead of illuminating it. -->
		<div
			class="ld-spill"
			style="left: {centre[0]}px; top: {centre[1]}px; width: {spillR * 2}px;
			       height: {spillR * 2}px;"
		></div>

		{#if scene?.caption}
			<!-- Anchored by TOP, in frame pixels. `bottom` would resolve against
			     `.ld`'s own box — which is the frame's height, not the plate's.

			     AND CLEAR OF THE FEATHER — but only just, because the corner is
			     where this line wants to be. The frame has no border now; it
			     dissolves into the page at its edges, and at 4.5%/99.5% the
			     mask was quietly eating the type it was meant to leave alone.
			     Pulling the caption fully inboard put it in the middle of the
			     picture instead. So the two meet: the caption sits at 7.6% and
			     95%, and the mask's left and bottom fades were shortened to
			     clear it. The type stays solid and the corner stays soft. -->
			<div class="ld-cap" style="left: {geom.nw * 0.076}px; top: {geom.nh * 0.95}px;">
				<p class="ld-cap-a">{scene.caption.you}</p>
				<p class="ld-cap-b">{scene.caption.it}</p>
			</div>
		{/if}

		{#if scene?.device === 'phone'}
			<div class="ld-face ld-phone" style="transform: {faceMatrix};">
				<div class="ld-screen ld-lock">
					<p class="ld-lock-clock">21:07</p>
					<div class="ld-notif" class:in={u > 0.28}>
						<span class="ld-glyph"></span>
						<span class="ld-notif-text">
							<b>Magican</b>
							Booked. Receipt in your inbox.
						</span>
					</div>
				</div>
				<div class="ld-grade" style="background: {grade}; opacity: {gradeAlpha};"></div>
				<div class="ld-glare"></div>
				<div class="ld-grain"></div>
			</div>
		{:else}
			<!-- The whole machine, placed by the SAME homography that places its
			     screen. The image is first translated so its own screen's
			     top-left sits at the origin, then mapped — so the screen lands
			     exactly on the authored quad and the case, hinge and deck come
			     along with it, correctly foreshortened, for free. Transform
			     functions apply right to left, which is why the translate is
			     written second. -->
			<img
				class="ld-shell"
				src={LAPTOP.src}
				alt=""
				width={LAPTOP.w}
				height={LAPTOP.h}
				loading="lazy"
				decoding="async"
				fetchpriority="low"
				style="width: {LAPTOP.w}px; height: {LAPTOP.h}px;
				       transform: {faceMatrix} translate({-LAPTOP.sx}px, {-LAPTOP.sy}px);"
			/>
			<!-- Our screen, on the glass. No case, no bezel, no border: the
			     photograph supplies all of that, and drawing our own on top is
			     how a composite starts looking like a sticker. -->
			<div
				class="ld-face ld-glass"
				style="width: {FACE_W}px; height: {FACE_H}px; transform: {faceMatrix};"
			>
				<div class="ld-screen">
					<!-- THE APP, NOT ONE PANE OF IT.
					     The first recreation was a bare thread on a blank field — no
					     top bar, no rail, a composer too small to see — so it read as
					     a chat widget rather than as Magican.
					     Traced from the shell: TopBar's brand mark (30px, radius 9, the
					     accent gradient) beside the name, its ⌘K pill (bg-soft on
					     border-soft, radius 8) and its icon cluster; the nav rail the
					     layout carries as `.with-sidebar`; then ChatPanel,
					     TaskStatusCard and FloatingComposer in the main pane, with the
					     activity panel beside them.
					     SIZES ARE DELIBERATELY LARGER than the real app's. At the size
					     this occupies in a wide shot, faithful type is a grey smear —
					     the shot has to be RECOGNISABLE, not to scale, which is the
					     same liberty every other screen on this page takes. -->
					<!-- TODAY'S APP (v0.1, re-traced 2026-10-13 from the live shell): the star
					     and lowercase name with a version chip, Chat · Work · Observe · Today
					     as pills, the ⌘K search and the bell; the icon rail on the left; the
					     Sorbet ground. The surfaces are the current ones — Today's
					     newspaper and Reading Room, the Work list, the chat's live task card
					     with its step bar, and the VibeDev studio. Sizes stay deliberately
					     larger than the real app's (see `.ld-ap`): recognisable, not to
					     scale. -->
					<div class="ld-ap">
						<div class="ld-ap-top">
							<img class="ld-ap-star" src="/brand/star-light.png" alt="" width="64" height="64" loading="lazy" decoding="async" />
							<span class="ld-ap-name">magican</span>
							<span class="ld-ap-ver mt-mono">v0.1.41</span>
							<nav class="ld-ap-tabs">
								{#each [['Chat', 'chat'], ['Work', 'tasks'], ['Observe', 'observe'], ['Today', 'today']] as [n, v] (n)}
									<span class="ld-ap-tab" class:on={v === view}>{n}</span>
								{/each}
							</nav>
							<span class="ld-ap-k"><i>Search, jump…</i><kbd class="mt-mono">⌘K</kbd></span>
							<span class="ld-ap-bell"><b>3</b></span>
						</div>
						<div class="ld-ap-frame">
							<div class="ld-ap-rail"><i></i><i></i><i></i><i></i><i></i><i></i><span class="ld-ap-av">A</span></div>
							<!-- VibeDev draws its own columns, so the shell reserves no side panel there. -->
							<div class="ld-ap-body" class:solo={view === 'vibe'}>
								{#if view === 'today'}
									<!-- TODAY is what the machine does with nobody there: the
									     newspaper masthead, then the Reading Room, each card
									     something it finished — and the one that genuinely owes a
									     decision carries the card's own actions. -->
									<div class="ld-td">
										<div class="ld-td-mast">
											<div class="ld-td-dl mt-mono">
												<span>VOL. IV · NO. 277</span><span>{scene?.branch ?? 'This morning'}</span><span>ALL QUIET</span>
											</div>
											<div class="ld-td-name">Today's</div>
											<div class="ld-td-sub">Here is your brief.</div>
										</div>
										<div class="ld-td-rr">
											<b>Reading Room</b>
											<span class="ld-td-tabs"><i class="on">All {scene?.feed?.length ?? 0}</i><i>For You</i><i>Worth a Look</i></span>
										</div>
										{#each scene?.feed ?? [] as card, i}
											{@const shown = w > 0.14 + i * 0.13}
											<div class="ld-td-c" class:in={shown} class:hi={card.ask}>
												<span class="ld-td-tag mt-mono">{card.ask ? 'NEEDS YOU' : 'DONE FOR YOU'} · {card.meta}</span>
												<b>{card.title}</b>
												<span class="ld-td-s">{card.body}</span>
												{#if card.ask}
													<span class="ld-td-a"><i>✕ Dismiss</i><i>✓ Useful</i><em>⚡ Do it</em></span>
												{/if}
											</div>
										{/each}
									</div>
								{:else if view === 'vibe'}
									<!-- VIBEDEV, its own surface: the runs panel (New build, the
									     workspace, the files of the run being worked on), the
									     conversation stepping through the build, and the stage
									     with its checks and the diff landing. -->
									<div class="ld-vb">
										<div class="ld-vb-rail">
											<span class="ld-vb-new">+ New build</span>
											<span class="ld-vb-ws">{scene?.title ?? 'Workspace'}</span>
											<span class="ld-vb-lbl mt-mono">RUN · FILES</span>
											{#each scene?.files ?? ['main', 'src', 'assets', 'build'] as f, i (f)}
												<span class="ld-vb-f" class:on={i === 1}>{f}</span>
											{/each}
										</div>
										<div class="ld-vb-conv">
											<span class="ld-cw-from"><i></i>{scene?.origin ?? 'From an earlier ask'}</span>
											<span class="ld-cw-quote ld-vb-ask"><em class="mt-mono">PICKED UP</em>{scene?.ask ?? 'Build it.'}</span>
											{#each scene?.rail ?? RAIL as step, i}
												<span class="ld-cw-card ld-vb-step" class:done={w > 0.24 + i * 0.1}>
													<i class="ld-cw-ico"></i><span class="ld-cw-c"><b>{step}</b></span>
												</span>
											{/each}
										</div>
										<div class="ld-vb-stage">
											<div class="ld-vb-checks">
												{#each [['build', 'ok'], ['tests', '212'], ['lint', 'ok']] as [c, v] (c)}
													<span class="ld-vb-chk">{c}<b>{v}</b></span>
												{/each}
												<span class="ld-vb-chip">${spend.toFixed(3)}</span>
											</div>
											<div class="ld-vb-code mt-mono">
												{#each scene?.lines ?? LINES as ln, i}
													<span class="ld-vb-ln" class:add={ln.k === '+'} class:del={ln.k === '-'}
														class:in={w > 0.2 + i * 0.06}>{ln.k || ' '} {ln.t}</span>
												{/each}
											</div>
										</div>
									</div>
								{:else if view === 'tasks'}
									<!-- WORK, the list as the app draws it now: the page header,
									     All work · Needs you · Crew, and rows with a status pill.
									     The queue drains on the SCENE clock, so something is always
									     just starting while nobody watches. -->
									<div class="ld-ap-main ld-wk">
										<div class="ld-wk-h"><b>Work</b><span>Your tasks, crew and ongoing work, together.</span></div>
										<div class="ld-wk-tabs"><i class="on">All work</i><i>Needs you</i><i>Crew</i></div>
										{#each scene?.rail ?? RAIL as t, i}
											{@const done = w > 0.22 + i * 0.09}
											<div class="ld-tk" class:done>
												<i class="ld-tk-i"></i><span class="ld-tk-t">{t}</span>
												<span class="ld-tk-s">{done ? 'Done' : 'Running'}</span>
											</div>
										{/each}
										{#each scene?.queued ?? ['reconcile the vendor ledger', 'watch the Kyoto fare'] as t, qi (t)}
											{@const start = 0.3 + qi * 0.26}
											<div class="ld-tk" class:queued={cu < start} class:done={cu > start + 0.17}>
												<i class="ld-tk-i"></i><span class="ld-tk-t">{t}</span>
												<span class="ld-tk-s">{cu > start + 0.17 ? 'Done' : cu > start ? 'Running' : 'Queued'}</span>
											</div>
										{/each}
									</div>
								{:else}
									<!-- CHAT. Nobody is at the keyboard, so nobody typed this: the
									     thread opens with where the ask came from, then the ask it
									     picked up, then the live task card stepping through the run,
									     the result, and the next jobs arriving across the scene. -->
									<div class="ld-ap-main">
										<div class="ld-cw-thread">
											<div class="ld-cw-row">
												<span class="ld-cw-from"><i></i>{scene?.origin ?? 'Picked up from an earlier ask'}</span>
											</div>
											<div class="ld-cw-row end">
												<span class="ld-cw-quote"><em class="mt-mono">PICKED UP</em>{scene?.ask ?? 'Book the Kyoto leg.'}</span>
											</div>
											<div class="ld-cw-row">
												<span class="ld-cw-task" class:done={w > 0.5}>
													<span class="ld-cw-th"><i class="ld-cw-dot"></i><b>{scene?.title ?? 'Thread'}</b><em>Magician</em></span>
													<span class="ld-cw-st">{chatLine}</span>
													<span class="ld-cw-bar">
														{#each rail as _, i}<i class:done={i < stepDone} class:live={i === stepDone && w <= 0.5}></i>{/each}
													</span>
												</span>
											</div>
											{#if w > 0.5}
												<div class="ld-cw-row">
													<span class="ld-cw-res">
														<b>Done</b>
														{#each (scene?.lines ?? LINES).slice(0, 3) as ln}
															<span class="ld-cw-l" class:add={ln.k === '+'}>{ln.t}</span>
														{/each}
													</span>
												</div>
											{/if}
											{#each scene?.also ?? [] as job, ji}
												{@const t = 0.34 + ji * 0.3}
												{#if cu > t}
													<div class="ld-cw-row">
														<span class="ld-cw-from"><i></i>{job.from ?? 'Also, as you approved'}</span>
													</div>
													<div class="ld-cw-row">
														<span class="ld-cw-card" class:done={cu > t + 0.05} class:live={cu <= t + 0.05}>
															<i class="ld-cw-ico"></i><span class="ld-cw-c"><b>{job.step}</b></span>
														</span>
													</div>
												{/if}
											{/each}
										</div>
										<div class="ld-cw-composer">
											<div class="ld-cw-dock mt-mono">#general / {scene?.branch ?? 'New session'}</div>
											<div class="ld-cw-input">Message or describe a task</div>
											<div class="ld-cw-chips"><em>DO · ASK</em><i>PLAN</i><b class="mt-mono">Magician · ADAPTIVE</b></div>
										</div>
									</div>
								{/if}
								<!-- THE SIDE PANEL describes whatever is on screen: the activity
								     behind it, what it is counting, and what the morning cost. -->
								{#if view !== 'vibe'}
									<div class="ld-ap-side">
										<div class="ld-ap-sg ld-ap-sg-steps">
											<div class="ld-ap-h">Activity</div>
											<div class="ld-ap-steps">
												{#each panelSteps as a, i}
													<span class="ld-ac" class:on={w > 0.16 + i * 0.07}>{a}</span>
												{/each}
											</div>
										</div>
										{#if scene?.chart}
											<div class="ld-ap-sg">
												<div class="ld-ap-h">{scene.chart.label}</div>
												{#if scene.chart.kind === 'ring'}
													{@const p = scene.chart.bars[0].v * (w > 0.2 ? 1 : 0)}
													<div class="ld-ch ld-ch-ring">
														<svg viewBox="0 0 36 36" aria-hidden="true">
															<circle class="ld-rg-t" cx="18" cy="18" r="15.9" />
															<circle class="ld-rg-v" cx="18" cy="18" r="15.9" style="stroke-dasharray: {(p * 100).toFixed(1)} 100" />
														</svg>
														<b class="mt-mono">{Math.round(p * 100)}%</b>
													</div>
												{:else if scene.chart.kind === 'line'}
													{@const pts = scene.chart.bars}
													<div class="ld-ch">
														<svg class="ld-ln" viewBox="0 0 100 34" preserveAspectRatio="none" aria-hidden="true">
															<polyline
																points={pts.map((b, i) => `${(i / (pts.length - 1)) * 100},${34 - b.v * 32}`).join(' ')}
																style="stroke-dashoffset: {w > 0.15 ? 0 : 240}"
															/>
															{#each pts.filter((b) => b.hi) as b}
																{@const i = pts.indexOf(b)}
																<circle cx={(i / (pts.length - 1)) * 100} cy={34 - b.v * 32} r="2.4" class="ld-ln-hi" style="opacity: {w > 0.45 ? 1 : 0}" />
															{/each}
														</svg>
													</div>
												{:else if scene.chart.kind === 'stack'}
													<div class="ld-ch ld-ch-stack">
														{#each scene.chart.bars as b, i}
															<span class="ld-st" class:hi={b.hi} style="flex: {w > 0.12 + i * 0.05 ? b.v : 0.001}"></span>
														{/each}
													</div>
												{:else}
													<div class="ld-ch">
														{#each scene.chart.bars as b, i}
															<span class="ld-ch-b" class:hi={b.hi} style="--h:{w > 0.12 + i * 0.05 ? b.v : 0}"></span>
														{/each}
													</div>
												{/if}
												<span class="ld-ac on ld-ch-n mt-mono">{scene.chart.note}</span>
											</div>
										{/if}
										<div class="ld-ap-sg ld-ap-cost">
											<div class="ld-ap-h">Cost</div>
											<span class="ld-ac on mt-mono">${spend.toFixed(3)} · {calls} calls</span>
											<span class="ld-ac on mt-mono">{scene?.models ?? 2} models · {mins}m</span>
										</div>
									</div>
								{/if}
							</div>
						</div>
					</div>
				</div>
				<div class="ld-grade" style="background: {grade}; opacity: {gradeAlpha};"></div>
				<div class="ld-glare"></div>
				<div class="ld-grain"></div>
			</div>
		{/if}
	</div>
{/if}

<style>
	.ld {
		position: absolute;
		inset: 0;
		transform-origin: 0 0;
		pointer-events: none;
	}
	/* THE SINGLE BIGGEST TELL. A screen throws light; a sticker does not. This
	   sits above the plate and below the device, additive, so the table it
	   lands on brightens toward the screen's own colour instead of being
	   veiled by a grey film. */
	.ld-contact {
		position: absolute;
		translate: -50% -62%;
		border-radius: 50%;
		mix-blend-mode: multiply;
		background: radial-gradient(
			closest-side,
			rgba(28, 20, 12, 0.86),
			rgba(40, 30, 18, 0.4) 46%,
			transparent 78%
		);
		filter: blur(9px);
	}
	.ld-spill {
		position: absolute;
		translate: -50% -50%;
		border-radius: 50%;
		mix-blend-mode: screen;
		background: radial-gradient(
			closest-side,
			rgba(150, 190, 255, 0.42),
			rgba(120, 160, 245, 0.16) 42%,
			transparent 72%
		);
		filter: blur(18px);
	}
	/* Every face is authored in one design box and then mapped onto its quad,
	   so screen content is written at a comfortable resolution and the
	   perspective is applied once, to the whole thing. */
	.ld-face {
		position: absolute;
		left: 0;
		top: 0;
		transform-origin: 0 0;
		overflow: hidden;
	}
	/* THE PHOTOGRAPH. Natural size, mapped by the homography — so it is never
	   resampled twice and the case keeps the sharpness that makes it read as a
	   real object next to a real room. */
	.ld-shell {
		position: absolute;
		left: 0;
		top: 0;
		transform-origin: 0 0;
		max-width: none;
	}
	/* Sized from the asset's own screen rect — see LAPTOP. */
	/* Our pixels, on the glass. Nothing draws a case here — the plate does. */
	.ld-glass {
		border-radius: 3px;
		background: #0d1024;
	}
	.ld-phone {
		width: 1000px;
		height: 625px;
		border-radius: 46px;
		background: #0a0a0d;
		box-shadow: inset 0 0 0 14px #1b1c22;
	}
	.ld-screen {
		position: absolute;
		inset: 0;
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		overflow: hidden;
		text-align: left;
		color: #f4f6ff;
		font-family: var(--lp-font, system-ui);
		background:
			radial-gradient(120% 80% at 50% 0%, rgba(72, 96, 190, 0.5), transparent 62%),
			linear-gradient(180deg, #0d1024, #10142c 60%, #0b0e1e);
	}
	.ld-cap {
		position: absolute;
		translate: 0 -100%;
		/* ONE LINE EACH. Both lines are short enough to hold, and wrapping turns
		   a caption into a paragraph — it grows upward into the picture,
		   changes height from scene to scene so the block never settles, and
		   makes the second line look like body copy rather than a subtitle.
		   The width is whatever the longer line needs; the machine's own left
		   edge is the ceiling, and every line clears it with room to spare. */
		width: max-content;
		max-width: 47%;
		text-align: left;
		color: #fff;
		font-family: var(--lp-font, system-ui);
		/* WHITE TYPE NEEDS SOMETHING TO SIT ON. The plates are bright by
		   requirement — open water, pale sky, sunlit grass — and a text-shadow
		   alone left the second and third lines washed out over the brightest
		   parts. A soft radial scrim under the corner darkens only what the
		   type occupies and fades out before it reads as a box. */
		text-shadow: 0 2px 16px rgba(0, 0, 0, 0.8);
		/* NO PADDING, NO MARGIN. They were left over from an earlier scrim that
		   needed room inside the box, and they quietly moved the caption off
		   its own anchor — 16px left and 26px up, which landed 7.6%/95% at
		   6.3%/91.4% and made every position note a guess. The scrim below is
		   inset by PERCENTAGES of this box, so it reaches past the type on its
		   own and the anchor can mean exactly what it says. */
	}
	.ld-cap::before {
		content: '';
		position: absolute;
		inset: -26% -26% -46% -44%;
		z-index: -1;
		background: radial-gradient(
			70% 60% at 30% 55%,
			rgba(6, 10, 20, 0.62),
			rgba(6, 10, 20, 0.34) 52%,
			transparent 78%
		);
	}
	/* THE HUMAN LINE, and the one the eye lands on. */
	.ld-cap-a {
		margin: 0 0 6px;
		font-size: 26px;
		line-height: 1.3;
		font-weight: 600;
		white-space: nowrap;
	}
			/* What it finished while they did. Quieter — the screen has already said it
	   in detail, so this is the summary, not the claim. */
	.ld-cap-b {
		margin: 0;
		font-size: 21px;
		line-height: 1.4;
		opacity: 0.82;
		white-space: nowrap;
	}

	/* THE SHELL, as the app is now (v0.1): the Sorbet ground, the star and the
	   lowercase name, Chat · Work · Observe · Today as pills, the ⌘K pill and the
	   bell, and the icon rail on the left. Scaled UP from the real app, because
	   faithful type at this size is a grey smear — recognisable, not to scale. */
	.ld-ap {
		--ink: #2b2230;
		--sec: #5a4e60;
		--muted: #716577;
		--line: #f1e4dc;
		--coral: #e8505f;
		--coral-soft: #ffe1e4;
		--info: #4a78e0;
		--ok: #16794f;
		--ok-soft: #dcf5ea;
		--user: #5a68f0;
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		color: var(--ink);
		background:
			radial-gradient(60% 50% at 10% 0%, #ffe4e6 0%, transparent 70%),
			radial-gradient(50% 60% at 100% 10%, #ece5ff 0%, transparent 70%),
			#fff9f4;
		/* Every size in here is an `em` of this base — a fraction of the design
		   box, not the browser's 16px, or the screen renders at three pixels
		   once it scales into the shot. */
		font-size: 50px;
	}
	.ld-ap-top {
		display: flex;
		align-items: center;
		gap: 0.4em;
		padding: 0.32em 0.7em;
		border-bottom: 1px solid var(--line);
		background: rgba(255, 249, 244, 0.92);
		flex: none;
	}
	.ld-ap-star {
		width: 0.95em;
		height: 0.95em;
		flex: none;
	}
	.ld-ap-name {
		font-size: 0.7em;
		font-weight: 600;
		letter-spacing: -0.01em;
	}
	.ld-ap-ver {
		font-size: 0.36em;
		opacity: 0.5;
	}
	.ld-ap-tabs {
		display: flex;
		align-items: center;
		gap: 0.15em;
		margin: 0 auto;
		min-width: 0;
	}
	.ld-ap-tab {
		padding: 0.18em 0.55em;
		border-radius: 0.4em;
		font-size: 0.5em;
		font-weight: 600;
		color: var(--sec);
		white-space: nowrap;
	}
	.ld-ap-tab.on {
		background: #fbd5dc;
		color: var(--ink);
	}
	.ld-ap-k {
		display: inline-flex;
		align-items: center;
		gap: 0.5em;
		padding: 0.2em 0.45em;
		border-radius: 0.45em;
		border: 1px solid var(--line);
		background: #fff;
		font-size: 0.42em;
	}
	.ld-ap-k i {
		font-style: normal;
		color: var(--muted);
		white-space: nowrap;
	}
	.ld-ap-k kbd {
		padding: 0 0.3em;
		border-radius: 0.25em;
		background: #f6efe9;
		color: var(--muted);
		font-size: 0.85em;
	}
	.ld-ap-bell {
		position: relative;
		width: 0.55em;
		height: 0.55em;
		margin-left: 0.25em;
		border-radius: 0.16em;
		border: 2px solid var(--muted);
		flex: none;
	}
	.ld-ap-bell b {
		position: absolute;
		top: -0.55em;
		right: -0.75em;
		padding: 0.05em 0.35em;
		border-radius: 1em;
		background: var(--coral);
		color: #fff;
		font-size: 0.5em;
	}
	/* rail · body — the icon rail the app keeps on the left. */
	.ld-ap-frame {
		flex: 1;
		min-height: 0;
		display: flex;
	}
	.ld-ap-rail {
		position: relative;
		flex: none;
		width: 0.95em;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.42em;
		padding-top: 0.45em;
		border-right: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.55);
	}
	.ld-ap-rail i {
		width: 0.34em;
		height: 0.34em;
		border-radius: 0.09em;
		border: 2px solid var(--muted);
		opacity: 0.65;
	}
	.ld-ap-av {
		position: absolute;
		bottom: 0.35em;
		width: 0.62em;
		height: 0.62em;
		border-radius: 50%;
		display: grid;
		place-items: center;
		background: #b46bd6;
		color: #fff;
		font-size: 0.36em;
		font-weight: 700;
	}
	.ld-ap-body {
		flex: 1;
		min-width: 0;
		min-height: 0;
		display: grid;
		grid-template-columns: 1fr 8em;
		overflow: hidden;
	}
	.ld-ap-body.solo {
		grid-template-columns: 1fr;
	}
	.ld-ap-main {
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow: hidden;
	}
	.ld-ap-h {
		padding: 0.3em 0.6em 0.15em;
		font-size: 0.42em;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--muted);
		flex: none;
	}
	/* The side panel, in the app's card style. */
	.ld-ap-side {
		display: flex;
		flex-direction: column;
		gap: 0.18em;
		margin: 0.3em 0.3em 0.3em 0;
		padding-bottom: 0.2em;
		border: 1px solid var(--line);
		border-radius: 0.35em;
		background: #fff;
		overflow: hidden;
	}
	/* Three blocks, and only the activity list gives — a flex item's default
	   floor is its content, which would shove the cost out of the frame. */
	.ld-ap-sg {
		display: flex;
		flex-direction: column;
		gap: 0.12em;
		flex: none;
	}
	.ld-ap-sg-steps {
		flex: 1 1 0;
		min-height: 0;
		overflow: hidden;
	}
	.ld-ap-steps {
		display: flex;
		flex-direction: column;
		gap: 0.12em;
		min-height: 0;
		overflow: hidden;
		mask-image: linear-gradient(to bottom, #000 0 74%, transparent 100%);
	}
	.ld-ac {
		padding: 0.08em 0.6em;
		font-size: 0.44em;
		line-height: 1.35;
		opacity: 0.3;
		transition: opacity 750ms ease;
	}
	.ld-ac.on {
		opacity: 0.85;
	}
	.ld-ap-cost {
		border-top: 1px solid var(--line);
		padding-top: 0.1em;
	}
	/* TODAY: the newspaper masthead, then the Reading Room. */
	.ld-td {
		display: flex;
		flex-direction: column;
		gap: 0.2em;
		min-height: 0;
		overflow: hidden;
		padding: 0.3em 0.6em 0.5em;
	}
	.ld-td-mast {
		text-align: center;
		border-bottom: 2px solid var(--ink);
		padding-bottom: 0.12em;
		flex: none;
	}
	.ld-td-dl {
		display: flex;
		justify-content: space-between;
		font-size: 0.3em;
		letter-spacing: 0.08em;
		color: var(--muted);
	}
	.ld-td-name {
		font-family: Georgia, 'Times New Roman', serif;
		font-size: 1.15em;
		font-weight: 700;
		line-height: 1.05;
	}
	.ld-td-sub {
		font-family: Georgia, 'Times New Roman', serif;
		font-style: italic;
		font-size: 0.38em;
		color: var(--sec);
	}
	.ld-td-rr {
		display: flex;
		align-items: center;
		gap: 0.4em;
		margin-top: 0.1em;
		flex: none;
	}
	.ld-td-rr b {
		font-family: Georgia, 'Times New Roman', serif;
		font-size: 0.6em;
	}
	.ld-td-tabs {
		margin-left: auto;
		display: flex;
		gap: 0.3em;
		font-size: 0.36em;
	}
	.ld-td-tabs i {
		font-style: normal;
		padding: 0.15em 0.55em;
		border-radius: 1em;
		color: var(--sec);
	}
	.ld-td-tabs i.on {
		background: #fff;
		border: 1px solid var(--line);
		color: var(--ink);
		font-weight: 600;
	}
	.ld-td-c {
		display: flex;
		flex-direction: column;
		gap: 0.06em;
		padding: 0.32em 0.5em;
		border: 1px solid var(--line);
		border-radius: 0.3em;
		background: #fff;
		box-shadow: 0 0.2em 0.5em -0.35em rgba(60, 30, 40, 0.4);
		opacity: 0;
		transform: translateY(4px);
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}
	.ld-td-c.in {
		opacity: 1;
		transform: none;
	}
	/* The one card that wants you is marked, and it is the only one. */
	.ld-td-c.hi {
		border-color: rgba(232, 80, 95, 0.45);
		box-shadow: 0 0 0 2px rgba(232, 80, 95, 0.12);
	}
	.ld-td-tag {
		font-size: 0.3em;
		letter-spacing: 0.08em;
		color: var(--coral);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ld-td-c b {
		font-family: Georgia, 'Times New Roman', serif;
		font-size: 0.52em;
		font-weight: 700;
	}
	.ld-td-s {
		font-size: 0.4em;
		line-height: 1.4;
		color: var(--sec);
	}
	.ld-td-a {
		display: flex;
		gap: 0.3em;
		margin-top: 0.14em;
		font-size: 0.36em;
	}
	.ld-td-a i,
	.ld-td-a em {
		font-style: normal;
		padding: 0.15em 0.55em;
		border-radius: 0.35em;
		border: 1px solid var(--line);
		color: var(--sec);
		font-weight: 600;
	}
	.ld-td-a em {
		background: var(--user);
		border-color: var(--user);
		color: #fff;
	}

	/* VIBEDEV: the runs panel, the conversation (the wider pane) and the stage. */
	.ld-vb {
		display: grid;
		grid-template-columns: 4.6em 1fr 1.15fr;
		min-height: 0;
		overflow: hidden;
	}
	.ld-vb-rail {
		display: flex;
		flex-direction: column;
		gap: 0.14em;
		padding: 0.35em 0.3em;
		border-right: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.75);
		min-width: 0;
	}
	.ld-vb-new {
		padding: 0.2em 0;
		border-radius: 0.3em;
		background: var(--coral);
		color: #fff;
		text-align: center;
		font-size: 0.4em;
		font-weight: 600;
	}
	.ld-vb-ws {
		padding: 0.15em 0.35em;
		border: 1px solid var(--line);
		border-radius: 0.3em;
		font-size: 0.36em;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ld-vb-lbl {
		margin-top: 0.2em;
		font-size: 0.28em;
		letter-spacing: 0.1em;
		color: var(--muted);
	}
	.ld-vb-f {
		padding: 0.12em 0.3em;
		border-radius: 0.25em;
		font-size: 0.36em;
		color: var(--sec);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ld-vb-f.on {
		background: #fbd5dc;
		color: var(--ink);
		font-weight: 600;
	}
	.ld-vb-conv {
		display: flex;
		flex-direction: column;
		gap: 0.2em;
		padding: 0.35em 0.4em 0.4em;
		font-size: 0.86em;
		border-right: 1px solid var(--line);
		min-height: 0;
		overflow: hidden;
	}
	.ld-vb-ask {
		max-width: 100%;
		font-size: 0.48em;
	}
	.ld-vb-step {
		max-width: 100%;
	}
	.ld-vb-stage {
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow: hidden;
	}
	.ld-vb-checks {
		display: flex;
		align-items: center;
		gap: 0.3em;
		padding: 0.4em 0.5em;
		flex: none;
	}
	.ld-vb-chk,
	.ld-vb-chip {
		display: inline-flex;
		align-items: baseline;
		gap: 0.25em;
		padding: 0.12em 0.45em;
		border-radius: 999px;
		background: #fff;
		border: 1px solid var(--line);
		font-size: 0.4em;
	}
	.ld-vb-chk b {
		color: var(--ok);
		font-weight: 600;
	}
	.ld-vb-chip {
		margin-left: auto;
		background: var(--coral-soft);
		border-color: transparent;
		color: #b03847;
	}
	.ld-vb-code {
		flex: 1;
		min-height: 0;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		margin: 0 0.4em 0.4em;
		padding: 0.3em 0.5em;
		border: 1px solid var(--line);
		border-radius: 0.3em;
		font-size: 0.38em;
		line-height: 1.75;
		background: #fff;
	}
	.ld-vb-ln {
		white-space: nowrap;
		opacity: 0;
		transform: translateX(-3px);
		transition:
			opacity 700ms ease,
			transform 700ms ease;
	}
	.ld-vb-ln.in {
		opacity: 0.72;
		transform: none;
	}
	.ld-vb-ln.add.in {
		opacity: 1;
		color: var(--ok);
		background: rgba(22, 121, 79, 0.08);
	}
	.ld-vb-ln.del.in {
		opacity: 0.7;
		color: #a8352f;
		text-decoration: line-through;
	}

	/* The chart, growing on the WORK clock so the shape is arrived at. */
	.ld-ch {
		display: flex;
		align-items: flex-end;
		gap: 0.14em;
		height: 2em;
		padding: 0 0.6em;
		margin-bottom: 0.15em;
	}
	.ld-ch-b {
		flex: 1;
		min-width: 0;
		height: calc(var(--h) * 100%);
		border-radius: 0.08em 0.08em 0 0;
		background: #c9bdf7;
		transition: height 850ms cubic-bezier(0.2, 0.7, 0.3, 1);
	}
	.ld-ch-b.hi {
		background: var(--coral);
	}
	.ld-ch-n {
		padding-top: 0;
		opacity: 0.6;
	}
	.ld-ch-ring {
		position: relative;
		justify-content: center;
		height: 2.2em;
	}
	.ld-ch-ring svg {
		height: 100%;
		transform: rotate(-90deg);
	}
	.ld-ch-ring circle {
		fill: none;
		stroke-width: 3.4;
	}
	.ld-rg-t {
		stroke: #f1e4dc;
	}
	.ld-rg-v {
		stroke: var(--coral);
		stroke-linecap: round;
		transition: stroke-dasharray 0.7s cubic-bezier(0.2, 0.7, 0.3, 1);
	}
	.ld-ch-ring b {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 0.4em;
		font-weight: 600;
	}
	.ld-ln {
		width: 100%;
		height: 100%;
		overflow: visible;
	}
	.ld-ln polyline {
		fill: none;
		stroke: #7a5cf0;
		stroke-width: 1.6;
		stroke-linecap: round;
		stroke-linejoin: round;
		vector-effect: non-scaling-stroke;
		stroke-dasharray: 240;
		transition: stroke-dashoffset 900ms ease;
	}
	.ld-ln-hi {
		fill: var(--coral);
		transition: opacity 800ms ease 0.5s;
	}
	.ld-ch-stack {
		align-items: stretch;
		height: 1em;
		gap: 0.06em;
	}
	.ld-st {
		border-radius: 0.06em;
		background: #c9bdf7;
		transition: flex 0.6s cubic-bezier(0.2, 0.7, 0.3, 1);
	}
	.ld-st.hi {
		background: var(--coral);
	}

	/* WORK: the page header, its tabs, and rows with a status pill. */
	.ld-wk-h {
		display: flex;
		align-items: baseline;
		gap: 0.6em;
		padding: 0.4em 0.6em 0.1em;
		flex: none;
	}
	.ld-wk-h b {
		font-size: 0.78em;
		font-weight: 600;
	}
	.ld-wk-h span {
		font-size: 0.38em;
		color: var(--sec);
	}
	.ld-wk-tabs {
		display: flex;
		gap: 0.3em;
		padding: 0.1em 0.6em 0.3em;
		font-size: 0.38em;
		flex: none;
	}
	.ld-wk-tabs i {
		font-style: normal;
		padding: 0.2em 0.65em;
		border-radius: 0.5em;
		color: var(--sec);
	}
	.ld-wk-tabs i.on {
		background: #fbd5dc;
		color: var(--ink);
		font-weight: 600;
	}
	.ld-tk {
		display: flex;
		align-items: center;
		gap: 0.5em;
		margin: 0 0.6em 0.2em;
		padding: 0.32em 0.55em;
		border: 1px solid var(--line);
		border-radius: 0.3em;
		background: #fff;
		font-size: 0.5em;
		box-shadow: 0 0.2em 0.5em -0.4em rgba(60, 30, 40, 0.4);
	}
	.ld-tk-i {
		width: 0.7em;
		height: 0.7em;
		border-radius: 50%;
		flex: none;
		border: 2px solid var(--info);
		border-right-color: transparent;
		animation: ld-spin 0.9s linear infinite;
	}
	.ld-tk.done .ld-tk-i {
		border-color: var(--ok);
		background: var(--ok);
		animation: none;
	}
	.ld-tk.queued {
		opacity: 0.5;
	}
	.ld-tk.queued .ld-tk-i {
		border-color: rgba(0, 0, 0, 0.25);
		animation: none;
	}
	.ld-tk-t {
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
	}
	.ld-tk-s {
		margin-left: auto;
		padding: 0.08em 0.55em;
		border-radius: 1em;
		font-size: 0.76em;
		font-weight: 600;
		color: var(--info);
		background: #e4ecff;
		white-space: nowrap;
	}
	.ld-tk.done .ld-tk-s {
		color: var(--ok);
		background: var(--ok-soft);
	}
	.ld-tk.queued .ld-tk-s {
		color: var(--muted);
		background: #f3ede8;
	}

	@keyframes ld-spin {
		to {
			transform: rotate(360deg);
		}
	}

	/* CHAT. A thread fills from the bottom, against the composer, the way every
	   chat surface does. */
	.ld-cw-thread {
		flex: 1;
		min-height: 0;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		justify-content: flex-end;
		gap: 0.26em;
		width: 100%;
		padding: 0.2em 0.8em 0.35em;
	}
	.ld-cw-row {
		display: flex;
	}
	.ld-cw-row.end {
		justify-content: flex-end;
	}
	/* Where it came from — provenance, not speech. */
	.ld-cw-from {
		display: inline-flex;
		align-items: center;
		gap: 0.35em;
		font-size: 0.4em;
		color: var(--muted);
	}
	.ld-cw-from i {
		width: 0.55em;
		height: 0.55em;
		border-radius: 50%;
		background: var(--coral);
		opacity: 0.6;
		flex: none;
	}
	/* The ask it picked up, in the user bubble's colour, labelled as picked up
	   rather than typed. */
	.ld-cw-quote {
		display: flex;
		flex-direction: column;
		gap: 0.15em;
		max-width: 80%;
		padding: 0.3em 0.55em;
		border-radius: 0.4em 0.4em 0.1em 0.4em;
		background: var(--user);
		color: #fff;
		font-size: 0.5em;
		line-height: 1.4;
		font-weight: 500;
	}
	.ld-cw-quote em {
		font-style: normal;
		font-size: 0.62em;
		letter-spacing: 0.1em;
		opacity: 0.8;
	}
	/* The live task card: title, agent, the step line and the step bar. */
	.ld-cw-task {
		display: flex;
		flex-direction: column;
		gap: 0.14em;
		width: 88%;
		padding: 0.35em 0.55em 0.4em;
		border: 1px solid var(--line);
		border-radius: 0.35em;
		background: #fff;
		box-shadow: 0 0.2em 0.6em -0.4em rgba(60, 30, 40, 0.4);
		font-size: 0.5em;
	}
	.ld-cw-th {
		display: flex;
		align-items: center;
		gap: 0.45em;
	}
	.ld-cw-th em {
		margin-left: auto;
		font-style: normal;
		font-size: 0.78em;
		font-weight: 600;
		color: #7a5cf0;
		background: #ece5ff;
		padding: 0.05em 0.5em;
		border-radius: 1em;
	}
	.ld-cw-dot {
		width: 0.55em;
		height: 0.55em;
		border-radius: 50%;
		background: var(--info);
		flex: none;
	}
	.ld-cw-task.done .ld-cw-dot {
		background: var(--ok);
	}
	.ld-cw-st {
		font-size: 0.82em;
		color: var(--sec);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.ld-cw-bar {
		display: flex;
		gap: 0.15em;
		margin-top: 0.1em;
	}
	.ld-cw-bar i {
		flex: 1;
		height: 0.28em;
		border-radius: 1em;
		background: #efe6df;
		transition: background 500ms ease;
	}
	.ld-cw-bar i.live {
		background: var(--info);
	}
	.ld-cw-bar i.done {
		background: var(--ok);
	}
	/* The next jobs, as compact task rows in their running and done tones. */
	.ld-cw-card {
		display: flex;
		align-items: flex-start;
		gap: 0.5em;
		max-width: 82%;
		padding: 0.32em 0.55em;
		border-radius: 0.3em;
		border: 1px solid #d9e3ff;
		background: #f2f5ff;
		font-size: 0.48em;
		opacity: 0.45;
		transition: opacity 750ms ease;
	}
	.ld-cw-card.live,
	.ld-cw-card.done {
		opacity: 1;
	}
	.ld-cw-card.done {
		border-color: #bfe6d4;
		background: #eefaf4;
	}
	.ld-cw-ico {
		width: 0.8em;
		height: 0.8em;
		margin-top: 0.15em;
		border-radius: 50%;
		flex: none;
		border: 2px solid var(--info);
		border-right-color: transparent;
		animation: ld-spin 0.9s linear infinite;
	}
	.ld-cw-card.done .ld-cw-ico {
		border-color: var(--ok);
		background: var(--ok);
		animation: none;
	}
	.ld-cw-c {
		display: flex;
		flex-direction: column;
		gap: 0.1em;
		min-width: 0;
	}
	.ld-cw-c b {
		font-weight: 600;
	}
	/* The result that lands when the run closes. */
	.ld-cw-res {
		display: flex;
		flex-direction: column;
		gap: 0.12em;
		max-width: 88%;
		padding: 0.35em 0.55em;
		border-radius: 0.3em;
		border: 1px solid var(--line);
		background: #fff;
		font-size: 0.46em;
		font-family: var(--lp-mono, ui-monospace, monospace);
	}
	.ld-cw-res b {
		font-family: var(--lp-font, system-ui);
		font-size: 1.1em;
		color: var(--ok);
	}
	.ld-cw-l.add {
		color: var(--ok);
	}
	/* The composer as it is now: the session line, the input, then the chips. */
	.ld-cw-composer {
		flex: none;
		margin: 0 0.8em 0.55em;
		padding: 0.25em 0.45em 0.3em;
		border: 1px solid var(--line);
		border-radius: 0.4em;
		background: rgba(255, 255, 255, 0.92);
		box-shadow: 0 0.3em 0.8em -0.5em rgba(60, 30, 40, 0.4);
	}
	.ld-cw-dock {
		font-size: 0.34em;
		color: var(--muted);
	}
	.ld-cw-input {
		margin: 0.2em 0;
		font-size: 0.46em;
		color: #b3a8b8;
	}
	.ld-cw-chips {
		display: flex;
		align-items: center;
		gap: 0.45em;
		font-size: 0.34em;
	}
	.ld-cw-chips em {
		font-style: normal;
		font-weight: 700;
		color: var(--coral);
		background: var(--coral-soft);
		padding: 0.1em 0.55em;
		border-radius: 1em;
	}
	.ld-cw-chips i {
		font-style: normal;
		font-weight: 600;
		color: var(--muted);
	}
	.ld-cw-chips b {
		margin-left: auto;
		font-weight: 400;
		color: var(--muted);
	}

	/* 4 · WHITE BALANCE. Multiply IS a per-channel gain, which is the correct
	   operation for tinting emitted light toward a room's camera balance. A
	   neutral room grades to white at zero opacity and costs nothing. */
	.ld-grade {
		position: absolute;
		inset: 0;
		mix-blend-mode: multiply;
		pointer-events: none;
	}
	/* 3 · GLARE, over our content rather than under it — a reflection lives on
	   the glass, in front of whatever the screen is showing. */
	.ld-glare {
		position: absolute;
		inset: 0;
		mix-blend-mode: screen;
		background: linear-gradient(
			104deg,
			transparent 26%,
			rgba(255, 244, 224, 0.13) 44%,
			rgba(255, 248, 235, 0.05) 55%,
			transparent 66%
		);
		pointer-events: none;
	}
	/* 5 · GRAIN. A perfectly clean rectangle inside a grainy photograph reads
	   as a hole cut in the picture. */
	.ld-grain {
		position: absolute;
		inset: 0;
		opacity: 0.16;
		mix-blend-mode: overlay;
		background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='120' height='120'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='3'/%3E%3C/filter%3E%3Crect width='120' height='120' filter='url(%23n)'/%3E%3C/svg%3E");
		background-size: 220px 220px;
		pointer-events: none;
	}

	/* The phone's lock screen — the notification the film already speaks in. */
	.ld-lock {
		place-content: start center;
		padding-top: 90px;
		gap: 60px;
		background:
			radial-gradient(130% 66% at 50% -4%, rgba(96, 58, 178, 0.42), transparent 58%),
			linear-gradient(180deg, #0b0b18, #14142a 55%, #0a0a16);
	}
	.ld-lock-clock {
		margin: 0;
		font-size: 130px;
		font-weight: 300;
		letter-spacing: -0.02em;
	}
	.ld-notif {
		display: flex;
		align-items: center;
		gap: 22px;
		padding: 24px 30px;
		border-radius: 30px;
		background: rgba(240, 240, 255, 0.14);
		backdrop-filter: blur(12px);
		text-align: left;
		font-size: 32px;
		line-height: 1.25;
		opacity: 0;
		transform: translateY(16px);
		transition:
			opacity 850ms ease,
			transform 850ms ease;
	}
	.ld-notif.in {
		opacity: 1;
		transform: none;
	}
	.ld-notif b {
		display: block;
		font-size: 26px;
		opacity: 0.7;
		font-weight: 600;
	}
	.ld-glyph {
		width: 54px;
		height: 54px;
		border-radius: 14px;
		flex: none;
		background: conic-gradient(from 20deg, #6ff2c2, #7aa2ff, #b78cff, #6ff2c2);
	}

	/* Reduced motion: the composite still composites — it is a picture, not an
	   animation — but nothing arrives, ticks or spins. The screen simply shows
	   the finished state of the work it was doing. */
	@media (prefers-reduced-motion: reduce) {
		.ld-cw-card,
		.ld-notif {
			transition: none;
			opacity: 0.88;
			transform: none;
		}
		.ld-cw-ico {
			animation: none;
			border-right-color: #7aa2ff;
		}
	}
</style>
