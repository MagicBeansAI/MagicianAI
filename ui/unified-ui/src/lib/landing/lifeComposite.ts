// Putting a DOM device into a photographed room.
//
// THE INVERSION. The montage's first paid scene missed four of the six things
// the compositing contract asked of it — branded hardware, a lit screen, a
// push-in that carried the laptop out of frame by 3s, and the one nobody had
// thought to write down: the lid faced AWAY, so the screen was never visible
// at all. Every one of those failures is the generator being asked to render
// HARDWARE, which is the thing it is worst at.
//
// So the video stops supplying hardware. It supplies the room, the light and
// the people; this module and `LifeDevice` supply the whole device. That drops
// every hardware constraint out of the prompt at once, and it makes the device
// pixel-exact, unbranded by construction and art-directable — which is what we
// wanted for the SCREEN already. Extending it to the case is the same argument
// one step further.
//
// What is left to solve is the one thing a real plate gave us for free: a
// device sits at an ANGLE in a room, and a rectangle pasted square-on reads as
// a sticker. Hence the homography below.

/** A point in the plate's own frame pixels. */
export type Pt = readonly [number, number];
/** Four corners, clockwise from top-left of the face being placed. */
export type Quad = readonly [Pt, Pt, Pt, Pt];

/**
 * Solve the 3×3 homography taking the rectangle `(0,0)–(w,h)` onto `quad`, and
 * return it as a CSS `matrix3d`.
 *
 * A quad is not a rect at an angle — it is a rect under PERSPECTIVE, where the
 * far edge is shorter than the near one. That cannot be expressed by
 * translate/rotate/scale at any composition, which is why this returns a full
 * projective matrix and why the manifest stores four corners rather than a
 * rect plus a rotation.
 *
 * The eight unknowns come from eight equations — two per corner — solved by
 * plain Gaussian elimination with partial pivoting. Eight-by-eight is small
 * enough that clarity beats cleverness, and this runs once per scene, not per
 * frame.
 */
export function quadToMatrix3d(w: number, h: number, quad: Quad): string {
	const src: Quad = [
		[0, 0],
		[w, 0],
		[w, h],
		[0, h]
	];
	// For each corner: x' = (ax + by + c) / (gx + hy + 1), same for y'.
	// Rearranged into linear form, that is two rows of the system below.
	const A: number[][] = [];
	const b: number[] = [];
	for (let i = 0; i < 4; i++) {
		const [x, y] = src[i];
		const [X, Y] = quad[i];
		A.push([x, y, 1, 0, 0, 0, -X * x, -X * y]);
		b.push(X);
		A.push([0, 0, 0, x, y, 1, -Y * x, -Y * y]);
		b.push(Y);
	}
	const s = solve(A, b);
	if (!s) return 'none';
	const [a, bb, c, d, e, f, g, hh] = s;
	// CSS matrix3d is COLUMN-MAJOR, and the z row/column is identity because a
	// flat face has no depth of its own — the perspective lives in g and h.
	return (
		`matrix3d(${a}, ${d}, 0, ${g}, ` +
		`${bb}, ${e}, 0, ${hh}, ` +
		`0, 0, 1, 0, ` +
		`${c}, ${f}, 0, 1)`
	);
}

/** Gaussian elimination with partial pivoting. Returns null if singular. */
function solve(A: number[][], b: number[]): number[] | null {
	const n = b.length;
	const M = A.map((row, i) => [...row, b[i]]);
	for (let col = 0; col < n; col++) {
		let piv = col;
		for (let r = col + 1; r < n; r++) if (Math.abs(M[r][col]) > Math.abs(M[piv][col])) piv = r;
		if (Math.abs(M[piv][col]) < 1e-12) return null;
		[M[col], M[piv]] = [M[piv], M[col]];
		for (let r = 0; r < n; r++) {
			if (r === col) continue;
			const k = M[r][col] / M[col][col];
			if (k === 0) continue;
			for (let c = col; c <= n; c++) M[r][c] -= k * M[col][c];
		}
	}
	// Gauss-JORDAN: every other row was eliminated too, so M is diagonal here
	// and each unknown is one division. `row[i]` IS the pivot M[i][i].
	return M.map((row, i) => row[n] / row[i]);
}

/** Axis-aligned bounds of a quad — the box the device's layer must cover. */
export function quadBounds(quad: Quad): { x: number; y: number; w: number; h: number } {
	const xs = quad.map((p) => p[0]);
	const ys = quad.map((p) => p[1]);
	const x = Math.min(...xs);
	const y = Math.min(...ys);
	return { x, y, w: Math.max(...xs) - x, h: Math.max(...ys) - y };
}

/** Centroid — where the screen's glow spills from. */
export function quadCentre(quad: Quad): Pt {
	let x = 0;
	let y = 0;
	for (const p of quad) {
		x += p[0];
		y += p[1];
	}
	return [x / 4, y / 4];
}

/**
 * White balance, as a multiply colour.
 *
 * Our UI is authored in sRGB and glows sRGB-blue. A room shot at golden hour
 * is not sRGB-neutral, so an untreated overlay reads as a screenshot pasted
 * into a photograph — the contract names this as the fourth-biggest tell, and
 * it is the cheapest of the five to fix properly.
 *
 * The physically right operation is a per-channel gain, and multiply IS a
 * per-channel gain, so the grade is just the room's own colour normalised so
 * its brightest channel is untouched. A neutral room therefore returns white
 * and costs nothing; a golden one pulls blue down and leaves red alone.
 */
export function gradeColour(lightSample: readonly [number, number, number]): string {
	const peak = Math.max(...lightSample, 1);
	const [r, g, b] = lightSample.map((c) => Math.round((c / peak) * 255));
	return `rgb(${r}, ${g}, ${b})`;
}

/**
 * How strongly a scene's own light argues for grading. A room already close to
 * neutral should not be pushed around; a strongly-cast one should be matched.
 * Returns 0..1, for the overlay's opacity.
 */
export function gradeStrength(lightSample: readonly [number, number, number]): number {
	const peak = Math.max(...lightSample, 1);
	const trough = Math.min(...lightSample);
	// Full cast (a channel at zero) is 1; perfectly neutral is 0. Held under a
	// LOW ceiling, and the reference is why: in it the screen stays neutral and
	// bright against full daylight and reads entirely correctly, because a
	// display is a light SOURCE, not a lit surface. It is tinted by the camera's
	// white balance, not by the room — so the grade is a hint that the two were
	// photographed together, not a match. The first ceiling (0.62) warmed the UI
	// until it looked like a photograph of a screen rather than a screen.
	return Math.min(0.22, 1 - trough / peak);
}

/**
 * One plate in the montage.
 *
 * `screenQuad` is AUTHORED, not measured. Under the inversion the video holds
 * no device, so this is where we DECIDED to stand one, in the plate's own
 * frame pixels. Null until someone has placed it against the footage — and a
 * scene without one simply plays as a plate, which is the same absence
 * contract the reel itself follows for footage that has not landed.
 */
export interface LifeScene {
	id: string;
	start: number;
	end: number;
	device?: 'laptop' | 'phone';
	/** Which surface of the app this scene shows. The montage argues that all
	 *  of it runs unattended, so it has to show more than one pane. */
	screen?: 'chat' | 'tasks' | 'today' | 'vibe' | 'lock';
	/**
	 * The surfaces this scene walks through, in order, each taking an equal
	 * share of the plate. A person is not away from one TASK, they are away
	 * from a morning — and one surface held for a whole scene finishes its work
	 * early and then sits there. Falls back to `screen` when absent.
	 */
	cases?: ('chat' | 'tasks' | 'today' | 'vibe')[];
	/** What is waiting behind the running work, and drains across the case. */
	queued?: string[];
	/**
	 * A small chart of whatever the scene is actually counting. `v` is 0..1 of
	 * the plot height; `hi` marks the bar the note is about.
	 */
	chart?: {
		label: string;
		note: string;
		/** How the figure wants to be drawn. Defaults to columns. */
		kind?: 'bars' | 'line' | 'ring' | 'stack';
		bars: { v: number; hi?: boolean }[];
	};
	/** What the morning cost, at the end of it. The panel counts up to these. */
	spend?: number;
	calls?: number;
	minutes?: number;
	models?: number;
	screenQuad?: Quad | null;
	baseQuad?: Quad | null;
	/** Chrome text on the working screen. */
	title?: string;
	/** What the person asked for — the thread's opening message. */
	ask?: string;
	/** WHERE the ask came from — nobody typed it, so the thread has to say. */
	origin?: string;
	/**
	 * The OTHER things running in the same scene.
	 *
	 * One task per plate said the machine does one thing at a time, which is
	 * the opposite of the claim: the whole beat is that a day's worth of work
	 * happens while nobody is at the keyboard. Each entry is a second and
	 * third job, arriving later in the scene's own progress.
	 */
	also?: { ask: string; step: string; from?: string }[];
	/**
	 * Today's feed. `ask: true` marks the one card that genuinely needs a
	 * decision — the rest happened without anybody, which is the point.
	 */
	feed?: { title: string; body: string; meta: string; ask?: boolean }[];
	branch?: string;
	/**
	 * WHAT THE MACHINE IS ACTUALLY DOING, per scene.
	 *
	 * The screen used to show one hard-coded travel itinerary under every
	 * caption, so five of the six scenes had a tab reading `Q3-close.xlsx`
	 * above a list of flight times. The caption makes a specific claim; the
	 * screen has to be that claim, or the shot quietly says the product is a
	 * mock-up. `k` is the diff marker — '+' added, '-' removed, '' context.
	 */
	lines?: { k: string; t: string }[];
	/** The agent rail: short steps, in order, the last one still running. */
	rail?: string[];
	/** VibeDev's file rail. The second entry is the one being worked on. */
	files?: string[];
	/**
	 * THE ARGUMENT, IN TWO LINES.
	 *
	 * It used to be three beats lifted from the reference — what the machine
	 * was doing and for how long, then the person's name and role, then their
	 * hobby. Two things were wrong with that.
	 *
	 * The screen already shows the work, in detail, mid-run. Naming the role
	 * and the hobby underneath was the caption doing the screen's job a second
	 * time and worse.
	 *
	 * And attaching the DURATION TO THE TASK advertised a slow machine.
	 * "Magican has been shipping the release notes for 2 hours" describes
	 * something that should take minutes; it reads as grinding, not working.
	 * The same two hours attached to the PERSON says the opposite — that is
	 * how much of their evening they got back. So: what you did, then what it
	 * finished while you did it.
	 */
	caption?: {
		/** "An evening playing." — the time, and whose it was. */
		you: string;
		/** "The release notes went out." — finished, past tense, no duration. */
		it: string;
	};
	lightSample?: [number, number, number];
	drift?: number;
}

/** Where the plate is drawn, and which frame is showing — see LifeReel.geom. */
export interface LifeGeom {
	dx: number;
	dy: number;
	dw: number;
	dh: number;
	nw: number;
	nh: number;
	frame: number;
	scenes: LifeScene[];
}
