/**
 * Office Floor — the layout solver.
 *
 * A PURE, DETERMINISTIC function from (citizens, guilds) to a floor plan in
 * abstract plan units. Nothing here touches the DOM, a store, or the clock, so
 * the layout is unit-testable and — the point — it does not reshuffle between
 * polls. `/square` re-derives its view models every few seconds; a layout that
 * depended on iteration order or `Math.random` would make the crew jump desks
 * on every tick.
 *
 * WHY UNITS AND NOT PIXELS. The plan sizes itself to the roster: eleven
 * programs make a small building, forty crew make a bigger one. OfficeFloor
 * measures its viewport and scales the whole plan to fit, so "everything on one
 * screen with no camera" holds for any roster without this file knowing how
 * wide the browser is.
 *
 * WHY THE PLAN IS SOLVED TO AN ASPECT. A fit is `min(paneW/W, paneH/H)`, so the
 * tighter axis wins and the other one becomes dead margin — a plan at 2.05:1
 * drawn into a 1.74:1 pane letterboxes about a fifth of the pane away, and
 * scaling cannot recover it because the shape is wrong, not the size. So the
 * caller passes the pane's aspect and the solver LAYS OUT to it: it takes the
 * tightest natural plan it can build, then grows the short axis until the plan
 * and the pane are the same shape. The fit is then exact, the scale factor is
 * as large as the roster allows, and everyone on the floor is drawn bigger as a
 * consequence. The aspect is an ARGUMENT, never a measurement taken here — this
 * file still never touches the DOM.
 *
 * THE TIGHTEST NATURAL PLAN. Because the fit is `paneW / width`, every plan
 * unit of width that is not a desk costs the crew size directly. Room minimums,
 * padding, the corridor and the amenities are therefore sized to their contents
 * and no more, and a vacant program is charged a fraction of a staffed one's
 * width — its hatched floor and its `Vacant` stamp say what it is without
 * needing the frontage of a room with people in it.
 *
 * THE SHAPE. Two room bands with a corridor between them, which is what an
 * office floor actually is — every room opens onto the corridor, the corridor
 * carries the shared props (cooler, plants, printer), and the entrance is at
 * one end of it. Amenity rooms are fixed members of the bands rather than a
 * special case bolted on: the meeting room anchors the north band, the pantry
 * and lounge anchor the south.
 *
 * ROOMS COME FROM MEMBERSHIP, SEATS COME FROM THE PRIMARY PROGRAM. The room set
 * is the union of every program id anyone is attached to (`guildIds`), while a
 * citizen is seated in their PRIMARY program only (`guildId`). That difference
 * is what makes vacancy real information: on the live roster the CPO is
 * attached to product_ops and product_strategy, sits in product_ops, and
 * product_strategy therefore gets a room with nobody in it. Hiding those rooms
 * would hide the fact that four programs have nobody working them.
 */

import type { CitizenVM, GuildVM } from '../engine/types';
import { guildNameOf } from '../derive';

export type Band = 'north' | 'south';
export type RoomKind = 'program' | 'meeting' | 'pantry' | 'lounge';

export type PropKind =
	| 'plant'
	| 'plant-tall'
	| 'cooler'
	| 'coffee'
	| 'counter'
	| 'sofa'
	| 'lounge-table'
	| 'meeting-table'
	| 'whiteboard'
	| 'cabinet'
	| 'printer'
	| 'stacked-chairs'
	| 'boxes'
	| 'rug';

export interface Rect {
	x: number;
	y: number;
	w: number;
	h: number;
}

export interface Prop {
	id: string;
	kind: PropKind;
	/** Centre of the prop in plan units. */
	x: number;
	y: number;
	w: number;
	h: number;
	flip: boolean;
}

export interface Seat {
	id: string;
	roomId: string;
	/** null on a spare desk — rooms never render more desks than members, so
	 * this stays null only in defensive paths. */
	citizenId: string | null;
	index: number;
	/** Desk slab centre. */
	x: number;
	y: number;
	/** Seated figure's baseline (behind the desk). */
	personX: number;
	personY: number;
	/** Which side of the cell the monitor sits on, so faces stay unblocked. */
	monitorSide: -1 | 1;
}

/**
 * The sprites are authored against a 78-unit desk cell. The plan's cell is
 * wider than that — desks need a working gap to their neighbour — so the seat
 * group is SCALED at render rather than every sprite being redrawn at a second
 * size. One number to change if the cell ever moves again.
 */
export const SPRITE_CELL = 78;

/** Seat bounding box in PLAN units, relative to the desk slab centre. It spans
 * the NAME PLATE, the person and the desk — the whole seat block — and it is
 * both the click target (a head is not something to ask anyone to hit) and the
 * anchor the unscaled HTML plate hangs from: the plate's top edge sits on the
 * box's top edge, centred on the seat. */
export interface SeatBox {
	dx: number;
	dy: number;
	w: number;
	h: number;
	/** Height reserved at the TOP of the box for the name plate, so the row
	 * pitch keeps one row's plate clear of the row in front of it. */
	plateH: number;
}

export interface Room extends Rect {
	id: string;
	kind: RoomKind;
	name: string;
	band: Band;
	/** A program room nobody is primarily assigned to. */
	vacant: boolean;
	memberIds: string[];
	seats: Seat[];
	props: Prop[];
	/** Nameplate strip, on the OUTER wall so it never fouls the door. */
	labelStrip: Rect;
	/** Furnishable interior, inside the padding and below/above the nameplate. */
	content: Rect;
	/** Opening onto the corridor. */
	door: { x: number; y: number; w: number };
	/** Deterministic hue for the nameplate accent. */
	accentHue: number;
}

export interface FloorPlan {
	width: number;
	height: number;
	wall: number;
	/** The aspect the plan was solved to, and the one it actually achieved. They
	 * differ only when a tiny roster hit the stretch ceiling. */
	targetAspect: number;
	aspect: number;
	/** Desk cell in plan units, and the sprite scale that fills it. */
	deskCell: { w: number; h: number };
	/** Furniture scale: sprite units -> plan units. */
	seatScale: number;
	/** People are drawn SMALLER than the furniture around them — see
	 * PERSON_RATIO. Same map, different ratio, so the crew still scales with it. */
	personScale: number;
	seatBox: SeatBox;
	/** Building footprint, inside the plot. */
	shell: Rect;
	corridor: Rect;
	bands: Record<Band, Rect>;
	rooms: Room[];
	/** Corridor and open-floor props. */
	props: Prop[];
	/** Every seat on the floor, flattened in room order. */
	seats: Seat[];
	/** Main entrance, cut into the left shell wall at corridor height. */
	entrance: { x: number; y: number; h: number };
	stats: {
		crew: number;
		programs: number;
		vacantPrograms: number;
		desks: number;
	};
}

/* --- dimensions, all in plan units ------------------------------------- */

const WALL = 14;
const PARTITION = 8;
/** Desk cell WIDTH: the sprite cell plus a working gap to its neighbour. This
 * is the number the whole floor's sense of scale hangs off — the fit is
 * `paneWidth / planWidth`, so a plan unit spent anywhere other than here is a
 * plan unit taken off the size of the crew. */
const DESK_W = 96;
const SEAT_SCALE = DESK_W / SPRITE_CELL;

/**
 * How large a person is drawn RELATIVE TO the furniture around them.
 *
 * This is a top-down plan, and the figures standing on it are drawn
 * front-facing. That is the ordinary convention for a map and it reads fine —
 * but only while the figure is SMALL. Big enough to read a face and a
 * front-facing body on a plan view stops being a token that says "someone
 * works here" and becomes a portrait pasted onto a blueprint: the floor is
 * suddenly a row of people looking at you. Under about a quarter of a desk's
 * frontage the exact same sprite reads as a character on a map, which is all
 * a floor plan needs it to say. So a person is drawn SMALLER than their desk.
 *
 * This ratio was 1.45 for one release, aimed at a different problem — a figure
 * and a desk fusing into a single "desk unit with a face in it". The real
 * cause of that was an OUTLINED rug drawn tight around the pair (see
 * roomProps, which now draws it edgeless and generous), and it is fixed at the
 * source. Size was never the remedy, and making the crew larger than their
 * furniture to buy separation cost far more than it bought.
 */
export const PERSON_RATIO = 0.66;
const PERSON_SCALE = SEAT_SCALE * PERSON_RATIO;
/** How far the figure's baseline sits below the desk slab centre, so the slab
 * crosses the body and the pose reads as seated AT a desk. At this ratio the
 * slab lands just under the shoulders, so what clears it is head and shoulders
 * — which is what a person at a desk looks like from above and in front. */
const PERSON_DROP = 6;
/** Reach of the figure above the baseline, in PLAN units. Measured to the top
 * of the tallest thing the casting can draw over a head — a bun, a CEO crown,
 * a needs-you bubble — not to the body, because those are the parts that would
 * land on the name plate above or the row behind. */
const PERSON_RISE = 68 * PERSON_SCALE;

/* The seat block: the name plate, the person under it, and their desk. The
 * DESK CELL HEIGHT is derived from it rather than picked, because the row
 * pitch has exactly one job — keep one row's plate off the row in front of it
 * — and that job is a function of how tall the block is. Change PERSON_RATIO
 * or the plate and the pitch follows. */
const SEAT_HALF_W = Math.max(36 * SEAT_SCALE, 22 * PERSON_SCALE);
/**
 * Plan units reserved above each head for the name plate.
 *
 * The plate is UNSCALED HTML at a fixed 11px face (two stacked lines: the name
 * and its role chip), so its real height is in CSS pixels and this is that
 * height carried back through a typical fit. It only has to be generous enough
 * that the plate never reaches the row in front; a unit or two either way just
 * changes how much air sits over the plate.
 */
const SEAT_PLATE_H = 34;
/** Air between the tallest head and the plate hanging over it. */
const SEAT_PLATE_GAP = 4;
const SEAT_TOP = PERSON_DROP - PERSON_RISE - SEAT_PLATE_GAP - SEAT_PLATE_H;
/** The desk's legs are the lowest thing drawn in the block. */
const SEAT_BOTTOM = Math.ceil(16 * SEAT_SCALE) + 4;
const DESK_H = Math.ceil(SEAT_BOTTOM - SEAT_TOP);
/** Desk slab centre relative to the cell centre — set so the seat block is
 * centred in its cell, which puts the plate high in the cell and the desk low. */
const DESK_SINK = -(SEAT_TOP + SEAT_BOTTOM) / 2;
const SEAT_BOX: SeatBox = {
	dx: -SEAT_HALF_W,
	dy: SEAT_TOP,
	w: 2 * SEAT_HALF_W,
	h: SEAT_BOTTOM - SEAT_TOP,
	plateH: SEAT_PLATE_H
};

const ROOM_PAD = 16;
const LABEL_H = 34;
/** A one-desk room: the cell, the padding, and nothing spare. */
const MIN_ROOM_W = DESK_W + 2 * ROOM_PAD + 14;
/* An unstaffed program is charged a THIRD of a staffed room's frontage. Width
 * is the scarce axis, and a program nobody is working has no contents to house;
 * the hatched floor, the stacked chairs and the `Vacant` stamp all still read
 * at this width, and everything reclaimed goes to the rooms with people in. */
const VACANT_ROOM_W = 96;
const VACANT_ROOM_H = 150;
/* Sized to the props that stand in it (a tall plant is 58) plus the walking
 * lane, and no more. The corridor is circulation, not a room. */
const CORRIDOR_H = 104;
const MIN_BAND_H = 176;
const DOOR_W = 44;

/* Amenity rooms are sized to their own furniture, same as a program room is
 * sized to its desks. They take a share of any surplus width later, so these
 * are floors and not fixed frontages. */
const AMENITY_SIZE: Record<Exclude<RoomKind, 'program'>, { w: number; h: number; band: Band }> = {
	meeting: { w: 300, h: 212, band: 'north' },
	pantry: { w: 214, h: 196, band: 'south' },
	lounge: { w: 196, h: 196, band: 'south' }
};

/**
 * Desk grids the solver may choose between, widest first.
 *
 * A room of six is 3x2 or 2x3, and which one is right is not a property of the
 * room — it is a property of the PLAN's shape. Many rooms already make a wide
 * building, so their desks should stack; few rooms make a narrow one, so their
 * desks should spread. The solver tries each bias and keeps whichever yields
 * the smallest solved width, i.e. the largest crew. Ties keep the first, so the
 * historical wide grid wins when nothing is gained by stacking.
 */
const GRID_BIASES = [1.35, 0.8, 0.45, 0.25] as const;
/** Ceiling on how far the natural plan may be stretched to reach the target
 * aspect. Without it a three-room office (nobody on the roster yet) would
 * inflate its lounge to half a screen to fill a wide pane. */
const MAX_STRETCH = 1.9;
/** Used when the caller has not measured its pane yet. */
const DEFAULT_ASPECT = 16 / 9;
/** Share of surplus height the corridor may take before the rooms get the rest.
 * The corridor is the least interesting floor on the plan; it does not get to
 * absorb the growth. */
const CORRIDOR_SURPLUS_SHARE = 0.28;
const CORRIDOR_SURPLUS_CAP = 72;

const AMENITY_NAME: Record<Exclude<RoomKind, 'program'>, string> = {
	meeting: 'Meeting Room',
	pantry: 'Pantry',
	lounge: 'Lounge'
};

const clamp = (v: number, lo: number, hi: number): number => Math.max(lo, Math.min(hi, v));

/** FNV-ish string hash — the only source of "variety" in this file, so that
 * variety is a function of the id and never of call order. */
export function planHash(value: string): number {
	let h = 2166136261;
	for (let i = 0; i < value.length; i++) {
		h ^= value.charCodeAt(i);
		h = Math.imul(h, 16777619) >>> 0;
	}
	return h >>> 0;
}

/**
 * Desk grid for a room of `members`.
 *
 * `bias` above 1 spreads the desks wide, below 1 stacks them tall; the default
 * is the historical wide grid, because rooms line a corridor and a floor
 * usually has more width to spend than height. The solver overrides it when the
 * plan would come out wider than the pane it has to fit — see GRID_BIASES.
 */
export function deskGrid(
	members: number,
	bias: number = GRID_BIASES[0]
): { cols: number; rows: number } {
	if (members <= 0) return { cols: 0, rows: 0 };
	const cols = Math.max(1, Math.min(members, Math.ceil(Math.sqrt(members * bias))));
	return { cols, rows: Math.ceil(members / cols) };
}

interface Draft {
	id: string;
	kind: RoomKind;
	name: string;
	memberIds: string[];
	vacant: boolean;
	w: number;
	h: number;
	cols: number;
	rows: number;
}

function programDraft(id: string, name: string, memberIds: string[], bias: number): Draft {
	const { cols, rows } = deskGrid(memberIds.length, bias);
	const vacant = memberIds.length === 0;
	// Sized to what is in the room and nothing more. Any slack the shell has
	// left over is handed out afterwards, by spreadSlack, and only to rooms that
	// have someone in them.
	const w = vacant ? VACANT_ROOM_W : Math.max(MIN_ROOM_W, cols * DESK_W + 2 * ROOM_PAD);
	const h = vacant ? VACANT_ROOM_H : rows * DESK_H + 2 * ROOM_PAD + LABEL_H;
	return { id, kind: 'program', name, memberIds, vacant, w, h, cols, rows };
}

/** One candidate floor: the two bands' room lists and the shell they need
 * before any aspect solving. */
interface Shape {
	northRooms: Draft[];
	southRooms: Draft[];
	innerW: number;
	northH: number;
	southH: number;
	naturalW: number;
	naturalH: number;
}

/** Width a band's rooms occupy, partitions included. */
function bandContent(drafts: Draft[]): number {
	return drafts.reduce((sum, d) => sum + d.w, 0) + PARTITION * Math.max(0, drafts.length - 1);
}

function amenityDraft(kind: Exclude<RoomKind, 'program'>): Draft {
	const size = AMENITY_SIZE[kind];
	return {
		id: `amenity:${kind}`,
		kind,
		name: AMENITY_NAME[kind],
		memberIds: [],
		vacant: false,
		w: size.w,
		h: size.h,
		cols: 0,
		rows: 0
	};
}

/**
 * Spread a band's leftover width so both bands end flush with the shell.
 *
 * Vacant rooms are excluded from the spread — the brief is that an unstaffed
 * program stays visibly small, and a widened empty room reads as a room that
 * lost its people rather than one that never had any. The rest take a share
 * PROPORTIONAL to their own width, so a room of five stays visibly bigger than
 * a room of one instead of every room converging on the same frontage. The last
 * taker absorbs the rounding, which is what keeps the band exactly flush.
 */
function spreadSlack(drafts: Draft[], slack: number): number[] {
	const widths = drafts.map((d) => d.w);
	if (slack <= 0 || drafts.length === 0) return widths;
	let takers = drafts.map((_, i) => i).filter((i) => !drafts[i].vacant);
	if (takers.length === 0) takers = drafts.map((_, i) => i);
	const total = takers.reduce((sum, i) => sum + drafts[i].w, 0);
	let given = 0;
	takers.forEach((i, k) => {
		const share =
			k === takers.length - 1
				? slack - given
				: total > 0
					? Math.floor((slack * drafts[i].w) / total)
					: 0;
		widths[i] += share;
		given += share;
	});
	return widths;
}

function roomProps(room: Room, draft: Draft): Prop[] {
	const props: Prop[] = [];
	const seed = planHash(room.id);
	const { content } = room;
	const push = (kind: PropKind, x: number, y: number, w: number, h: number, flip = false): void => {
		props.push({ id: `${room.id}:${kind}:${props.length}`, kind, x, y, w, h, flip });
	};
	// The OUTER wall is the one without a door in it, so shelving, boards and
	// counters go there. `againstWall` returns the centre y for a prop of height
	// h standing flush against it — placing by centre without accounting for the
	// prop's own height is what leaves furniture floating mid-room.
	const wallY = room.band === 'north' ? room.y + LABEL_H : room.y + room.h - LABEL_H;
	const againstWall = (h: number): number =>
		room.band === 'north' ? wallY + h / 2 + 4 : wallY - h / 2 - 4;
	/** …and the same for the door wall, where the loose clutter lives. */
	const nearDoor = (h: number): number =>
		room.band === 'north' ? room.y + room.h - h / 2 - 8 : room.y + h / 2 + 8;

	if (room.kind === 'meeting') {
		// The table grows with the room it is in. Amenities take a share of any
		// surplus width the shell has, so a fixed 240 would sit in a lake of floor
		// on a wide pane and overhang its own walls on a narrow one.
		const tableW = clamp(content.w - 56, 180, 360);
		const tableH = clamp(content.h - 46, 96, 150);
		push('meeting-table', content.x + content.w / 2, content.y + content.h / 2 + 4, tableW, tableH);
		push('whiteboard', content.x + content.w / 2, againstWall(22), clamp(content.w - 90, 110, 220), 22);
		push('plant-tall', content.x + 20, nearDoor(58) + 22, 40, 58);
		push('plant-tall', content.x + content.w - 20, nearDoor(58) + 22, 40, 58);
		return props;
	}
	if (room.kind === 'pantry') {
		push('counter', content.x + content.w / 2, againstWall(34), content.w - 8, 34);
		push('coffee', content.x + 40, againstWall(34) - 22, 30, 40);
		push('lounge-table', content.x + content.w / 2 + 12, nearDoor(56), 74, 56);
		push('plant', content.x + content.w - 22, nearDoor(42) + 4, 34, 42);
		return props;
	}
	if (room.kind === 'lounge') {
		push('sofa', content.x + content.w / 2, nearDoor(56), clamp(content.w - 40, 110, 230), 56);
		push('lounge-table', content.x + content.w / 2, nearDoor(56) + (room.band === 'north' ? -50 : 50), 64, 42);
		push('cooler', content.x + 24, againstWall(54) + 12, 32, 54);
		push('plant-tall', content.x + content.w - 24, againstWall(58) + 14, 40, 58);
		return props;
	}
	if (room.vacant) {
		// Deliberately sparse: a hatched floor, stacked chairs and a carton is
		// what an unstaffed program looks like from the corridor.
		//
		// Placed off the CONTENT box rather than the room, and in the same order
		// in both bands, because the "Vacant" stamp is drawn by the label layer
		// at the content's midline — the one band-relative placement collided
		// with the chairs in the north band and read fine in the south, which is
		// exactly the kind of asymmetry content-relative offsets remove.
		push('stacked-chairs', content.x + content.w / 2, content.y + 54, 44, 64);
		push('boxes', content.x + content.w / 2 + 2, content.y + content.h - 24, 48, 32);
		return props;
	}

	// Staffed program room. The desk pod stands on an area rug — without it a
	// single-desk room is one small object adrift in a large empty rectangle,
	// which reads as an unfinished room rather than a working one.
	//
	// It is drawn as a flat tint with NO border (see OfficeProp): outlined and
	// drawn tight to the pod it stopped being a rug and became a card around the
	// person and their desk, which is the single strongest cue that the two are
	// one object. So it is edgeless, and sized generously past the pod — in a
	// one-desk room it runs the full width of the floor, which reads as the room
	// being carpeted rather than as the occupant being boxed.
	push(
		'rug',
		content.x + content.w / 2,
		content.y + content.h / 2,
		Math.min(content.w, draft.cols * DESK_W + 108),
		Math.min(content.h, draft.rows * DESK_H + 54)
	);
	const plantOnLeft = (seed & 1) === 0;
	push(
		draft.rows > 1 ? 'plant-tall' : 'plant',
		plantOnLeft ? content.x + 16 : content.x + content.w - 16,
		nearDoor(draft.rows > 1 ? 58 : 42) + (draft.rows > 1 ? 24 : 16),
		34,
		draft.rows > 1 ? 58 : 42
	);
	if (room.w >= 230) {
		push('cabinet', content.x + content.w / 2, againstWall(22), Math.min(140, content.w - 50), 22);
	}
	if (room.w >= 300 && (seed & 2) === 0) {
		push(
			'printer',
			plantOnLeft ? content.x + content.w - 22 : content.x + 22,
			againstWall(30),
			32,
			30
		);
	}
	return props;
}

function layoutSeats(room: Room, draft: Draft): Seat[] {
	const seats: Seat[] = [];
	const n = draft.memberIds.length;
	if (n === 0) return seats;
	const { cols, rows } = draft;
	const gridH = rows * DESK_H;
	const startY = room.content.y + (room.content.h - gridH) / 2;
	for (let i = 0; i < n; i++) {
		const r = Math.floor(i / cols);
		const c = i % cols;
		const inRow = Math.min(cols, n - r * cols);
		const rowStartX = room.content.x + (room.content.w - inRow * DESK_W) / 2;
		const cx = rowStartX + c * DESK_W + DESK_W / 2;
		const cy = startY + r * DESK_H + DESK_H / 2;
		const citizenId = draft.memberIds[i];
		const deskY = cy + DESK_SINK;
		seats.push({
			id: `${room.id}:desk:${i}`,
			roomId: room.id,
			citizenId,
			index: i,
			x: cx,
			// The desk sits low in the cell so the figure and the name plate over
			// it have the room above; the figure's baseline is just BELOW the slab
			// centre, which is what makes the slab cross the body and the pose
			// read as seated at the desk rather than parked beside it.
			y: deskY,
			personX: cx,
			personY: deskY + PERSON_DROP,
			monitorSide: planHash(citizenId) % 2 === 0 ? -1 : 1
		});
	}
	return seats;
}

/**
 * Build the floor.
 *
 * @param citizens the crew, in any order — seating is sorted internally.
 * @param guilds programs known to the fleet snapshot. Programs referenced only
 *   by a citizen's `guildIds` are added, so nothing a citizen belongs to is
 *   silently missing a room.
 * @param targetAspect width/height of the pane this plan will be drawn into.
 *   The plan is LAID OUT to it rather than fitted into it, so the fit is exact
 *   and the scale factor is as large as the roster allows. Callers should
 *   quantise it (two decimals is plenty) so a drag-resize does not re-solve the
 *   floor on every animation frame. Still pure: this is an argument, and
 *   (roster, aspect) always gives the same plan.
 */
export function buildFloorPlan(
	citizens: readonly CitizenVM[],
	guilds: readonly GuildVM[],
	targetAspect: number = DEFAULT_ASPECT
): FloorPlan {
	/* --- 1. the room set --------------------------------------------------- */
	const names = new Map<string, string>();
	const ids = new Set<string>();
	for (const guild of guilds) {
		if (!guild.id) continue;
		ids.add(guild.id);
		names.set(guild.id, guild.name || guildNameOf(guild.id));
	}
	for (const citizen of citizens) {
		for (const id of [citizen.guildId, ...(citizen.guildIds ?? [])]) {
			if (id) ids.add(id);
		}
	}

	const members = new Map<string, string[]>();
	for (const id of ids) members.set(id, []);
	// Sort the crew once so a member list never depends on roster arrival order.
	const roster = [...citizens].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
	for (const citizen of roster) {
		const id = citizen.guildId || 'commons';
		if (!members.has(id)) {
			ids.add(id);
			members.set(id, []);
		}
		members.get(id)!.push(citizen.id);
	}

	const programEntries = [...ids]
		.map((id) => ({ id, name: names.get(id) ?? guildNameOf(id), memberIds: members.get(id) ?? [] }))
		.sort((a, b) => b.memberIds.length - a.memberIds.length || a.id.localeCompare(b.id));

	/* --- 2. draft a candidate floor for one desk-grid bias ------------------- */
	const draftFloor = (bias: number): Shape => {
		const programs = programEntries.map((p) => programDraft(p.id, p.name, p.memberIds, bias));

		const north: Draft[] = [];
		const south: Draft[] = [];
		// Seeded with the amenities each band will end with, so the greedy split
		// balances against the real finished width rather than the programs alone.
		let northW = AMENITY_SIZE.meeting.w;
		let southW = AMENITY_SIZE.pantry.w + AMENITY_SIZE.lounge.w;
		for (const draft of programs) {
			if (northW <= southW) {
				north.push(draft);
				northW += draft.w;
			} else {
				south.push(draft);
				southW += draft.w;
			}
		}
		// Spread the unstaffed rooms through each band instead of letting them pile
		// up at the end of it. They sort last (fewest members), so left alone they
		// form one continuous hatched wing, which reads as a rendering fault rather
		// than as four separate programs nobody is working. Interleaved, each vacancy
		// sits next to its peers and is legible as one room — the hatch, the missing
		// desks and the stamp already say what it is.
		const interleave = (drafts: Draft[]): Draft[] => {
			const staffed = drafts.filter((d) => !d.vacant);
			const vacant = drafts.filter((d) => d.vacant);
			if (vacant.length === 0 || staffed.length === 0) return drafts;
			const out: Draft[] = [];
			const gap = (staffed.length + 1) / (vacant.length + 1);
			let placed = 0;
			staffed.forEach((draft, i) => {
				out.push(draft);
				while (placed < vacant.length && i + 1 >= Math.round(gap * (placed + 1))) {
					out.push(vacant[placed]);
					placed += 1;
				}
			});
			for (; placed < vacant.length; placed++) out.push(vacant[placed]);
			return out;
		};
		const northRooms = interleave(north);
		const southRooms = interleave(south);
		northRooms.push(amenityDraft('meeting'));
		southRooms.push(amenityDraft('pantry'), amenityDraft('lounge'));

		const innerW = Math.max(bandContent(northRooms), bandContent(southRooms));
		const northH = Math.max(MIN_BAND_H, ...northRooms.map((d) => d.h));
		const southH = Math.max(MIN_BAND_H, ...southRooms.map((d) => d.h));
		return {
			northRooms,
			southRooms,
			innerW,
			northH,
			southH,
			naturalW: innerW + 2 * WALL,
			naturalH: northH + CORRIDOR_H + southH + 2 * WALL
		};
	};

	/* --- 3. solve the shell to the pane's shape ------------------------------ */
	// A plan drawn at `min(paneW/W, paneH/H)` loses whichever axis is not
	// binding, so the shape is chosen rather than accepted. Two moves, in order:
	//
	//   a. pick the desk grid that yields the SMALLEST solved width. Solved width
	//      is `max(naturalW, naturalH * aspect)` — the width the plan ends at once
	//      it has been squared up to the pane — and the fit is paneW/thatWidth, so
	//      minimising it maximises how big everyone is drawn. Stacking a room's
	//      desks trades width for height, which is the right trade only when the
	//      building is already wider than the pane; the search decides, not a
	//      guess baked into the grid.
	//   b. grow the short axis to match. Growth lands INSIDE rooms and the
	//      corridor, where it reads as floor, instead of outside the shell, where
	//      it reads as letterbox.
	const aspect =
		Number.isFinite(targetAspect) && targetAspect > 0.25 && targetAspect < 6
			? targetAspect
			: DEFAULT_ASPECT;
	let shape = draftFloor(GRID_BIASES[0]);
	let bestCost = Math.max(shape.naturalW, shape.naturalH * aspect);
	for (let i = 1; i < GRID_BIASES.length; i++) {
		const candidate = draftFloor(GRID_BIASES[i]);
		const cost = Math.max(candidate.naturalW, candidate.naturalH * aspect);
		// Strictly better only — ties keep the wider grid, which keeps the plan
		// stable when stacking buys nothing.
		if (cost < bestCost - 0.5) {
			shape = candidate;
			bestCost = cost;
		}
	}
	const { northRooms, southRooms } = shape;

	const width = Math.round(
		Math.max(shape.naturalW, Math.min(shape.naturalH * aspect, shape.naturalW * MAX_STRETCH))
	);
	const height = Math.max(shape.naturalH, Math.round(width / aspect));
	const innerW = width - 2 * WALL;

	// Hand the surplus height out: a capped slice to the corridor, the rest to
	// the two bands in proportion to what they already needed, so the band that
	// is carrying a two-row room stays the deeper one. The shares are integers
	// and sum exactly, so the bands still tile the shell.
	const heightSurplus = height - shape.naturalH;
	const corridorGain = Math.min(
		Math.round(heightSurplus * CORRIDOR_SURPLUS_SHARE),
		CORRIDOR_SURPLUS_CAP
	);
	const bandGain = heightSurplus - corridorGain;
	const naturalBands = shape.northH + shape.southH;
	const northGain =
		naturalBands > 0 ? Math.round((bandGain * shape.northH) / naturalBands) : Math.round(bandGain / 2);
	const northH = shape.northH + northGain;
	const southH = shape.southH + (bandGain - northGain);
	const corridorH = CORRIDOR_H + corridorGain;

	const bands: Record<Band, Rect> = {
		north: { x: WALL, y: WALL, w: innerW, h: northH },
		south: { x: WALL, y: WALL + northH + corridorH, w: innerW, h: southH }
	};
	const corridor: Rect = { x: WALL, y: WALL + northH, w: innerW, h: corridorH };

	/* --- 4. place the rooms -------------------------------------------------- */
	const rooms: Room[] = [];
	const place = (drafts: Draft[], band: Band): void => {
		const widths = spreadSlack(drafts, innerW - bandContent(drafts));
		let cursor = WALL;
		drafts.forEach((draft, i) => {
			const rect = bands[band];
			const w = widths[i];
			const room: Room = {
				id: draft.id,
				kind: draft.kind,
				name: draft.name,
				band,
				vacant: draft.vacant,
				memberIds: draft.memberIds,
				x: cursor,
				y: rect.y,
				w,
				h: rect.h,
				seats: [],
				props: [],
				labelStrip:
					band === 'north'
						? { x: cursor, y: rect.y, w, h: LABEL_H }
						: { x: cursor, y: rect.y + rect.h - LABEL_H, w, h: LABEL_H },
				content:
					band === 'north'
						? {
								x: cursor + ROOM_PAD,
								y: rect.y + LABEL_H + ROOM_PAD,
								w: w - 2 * ROOM_PAD,
								h: rect.h - LABEL_H - 2 * ROOM_PAD
							}
						: {
								x: cursor + ROOM_PAD,
								y: rect.y + ROOM_PAD,
								w: w - 2 * ROOM_PAD,
								h: rect.h - LABEL_H - 2 * ROOM_PAD
							},
				door: {
					// Doors sit on the corridor edge, nudged off-centre by the room id
					// so a long band does not read as a row of identical cells.
					x: cursor + w / 2 + ((planHash(draft.id) % 3) - 1) * Math.max(0, Math.min(34, w / 2 - DOOR_W)),
					y: band === 'north' ? rect.y + rect.h : rect.y,
					w: Math.min(DOOR_W, w - 24)
				},
				accentHue: planHash(draft.id) % 360
			};
			room.seats = layoutSeats(room, draft);
			room.props = roomProps(room, draft);
			rooms.push(room);
			cursor += w + PARTITION;
		});
	};
	place(northRooms, 'north');
	place(southRooms, 'south');

	/* --- 5. the corridor's own furniture -------------------------------------- */
	const props: Prop[] = [];
	const midY = corridor.y + corridor.h / 2;
	const stops: Array<{ kind: PropKind; w: number; h: number }> = [
		{ kind: 'plant-tall', w: 42, h: 58 },
		{ kind: 'cooler', w: 32, h: 54 },
		{ kind: 'plant', w: 34, h: 42 },
		{ kind: 'printer', w: 34, h: 30 },
		{ kind: 'plant-tall', w: 42, h: 58 },
		{ kind: 'plant', w: 34, h: 42 }
	];
	// Evenly spaced down the corridor and alternated above/below the centre line
	// so the walking lane stays open in the middle.
	const runStart = corridor.x + Math.min(96, corridor.w / 6);
	const runEnd = corridor.x + corridor.w - Math.min(96, corridor.w / 6);
	const step = stops.length > 1 ? (runEnd - runStart) / (stops.length - 1) : 0;
	stops.forEach((stop, i) => {
		props.push({
			id: `corridor:${stop.kind}:${i}`,
			kind: stop.kind,
			x: runStart + step * i,
			y: midY + (i % 2 === 0 ? -corridor.h / 2 + stop.h / 2 + 8 : corridor.h / 2 - stop.h / 2 - 8),
			w: stop.w,
			h: stop.h,
			flip: i % 2 === 1
		});
	});

	const seats = rooms.flatMap((room) => room.seats);
	const entranceInset = Math.min(26, corridor.h / 4);
	return {
		width,
		height,
		wall: WALL,
		targetAspect: aspect,
		aspect: width / height,
		deskCell: { w: DESK_W, h: DESK_H },
		seatScale: SEAT_SCALE,
		personScale: PERSON_SCALE,
		seatBox: SEAT_BOX,
		shell: { x: 0, y: 0, w: width, h: height },
		corridor,
		bands,
		rooms,
		props,
		seats,
		entrance: { x: 0, y: corridor.y + entranceInset, h: corridor.h - 2 * entranceInset },
		stats: {
			crew: citizens.length,
			programs: programEntries.length,
			vacantPrograms: programEntries.filter((p) => p.memberIds.length === 0).length,
			desks: seats.length
		}
	};
}
