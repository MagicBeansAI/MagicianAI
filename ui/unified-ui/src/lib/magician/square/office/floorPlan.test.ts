import { describe, expect, it } from 'vitest';

import { buildFloorPlan, deskGrid, PERSON_RATIO, type FloorPlan, type Rect } from './floorPlan';
import type { CitizenVM, GuildVM } from '../engine/types';

/**
 * Layout invariants for the office floor.
 *
 * The failures these exist to catch are all silent-but-wrong rather than
 * throwing: a crew member seated twice reads as two people, a crew member
 * seated nowhere reads as someone who quit, two rooms overlapping reads as one
 * larger room, a plan that does not match the pane it is drawn into loses a
 * fifth of the screen to letterbox, and a layout that is not a pure function of
 * the roster makes the whole floor twitch on every poll.
 */

/** Panes the floor actually gets drawn into, measured off the /square hero.
 * Width/height of the inset fit box, not of the browser window. */
const PANES: Array<[string, number]> = [
	['1440x900', 1.75],
	['1920x1080', 1.92],
	['a tall split pane', 1.1],
	['a wide ultrawide', 2.6]
];

/**
 * How close the solved plan must come to the aspect it was asked for.
 *
 * 1.5% — the residual is integer rounding of the shell's width and height, not
 * slack in the solver, so it shrinks as the plan grows. The one case allowed to
 * miss is a roster too small to stretch that far (see MAX_STRETCH), which is
 * asserted separately.
 */
const ASPECT_TOLERANCE = 0.015;

const citizen = (id: string, guildId: string, extra: Partial<CitizenVM> = {}): CitizenVM =>
	({
		id,
		name: id,
		guildId,
		guildIds: [guildId],
		vibe: 'idle',
		...extra
	}) as unknown as CitizenVM;

const guild = (id: string, memberIds: string[] = []): GuildVM => ({
	id,
	name: id,
	memberIds,
	activeQuestCount: 0,
	blockedQuestCount: 0,
	deliveredQuestCount: 0
});

/** The roster the owner actually has: 11 crew over 11 programs, 4 unstaffed. */
function liveRoster(): { citizens: CitizenVM[]; guilds: GuildVM[] } {
	const citizens = [
		citizen('ceo', 'company_strategy', { guildIds: ['company_strategy', 'daily_ops'] }),
		citizen('cmo', 'content_calendar', { guildIds: ['content_calendar', 'marketing_strategy'] }),
		citizen('cpo', 'product_ops', { guildIds: ['product_ops', 'product_strategy'] }),
		citizen('cro', 'revenue_strategy', { guildIds: ['revenue_strategy'] }),
		citizen('cto', 'engineering_strategy', { guildIds: ['engineering_strategy', 'sprint_plan'] }),
		citizen('harness-sre', 'harness_reliability', { guildIds: ['harness_reliability'] }),
		citizen('presto', 'commons'),
		citizen('envoy', 'commons'),
		citizen('scribe', 'commons'),
		citizen('sleuth', 'commons'),
		citizen('tally', 'commons')
	];
	return { citizens, guilds: [] };
}

function overlaps(a: Rect, b: Rect): boolean {
	return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

function insideShell(plan: FloorPlan, r: Rect): boolean {
	return (
		r.x >= plan.wall - 0.001
		&& r.y >= plan.wall - 0.001
		&& r.x + r.w <= plan.width - plan.wall + 0.001
		&& r.y + r.h <= plan.height - plan.wall + 0.001
	);
}

describe('deskGrid', () => {
	it('gives one desk one cell and never more columns than desks', () => {
		expect(deskGrid(0)).toEqual({ cols: 0, rows: 0 });
		expect(deskGrid(1)).toEqual({ cols: 1, rows: 1 });
		for (let n = 1; n <= 60; n++) {
			const { cols, rows } = deskGrid(n);
			expect(cols).toBeGreaterThanOrEqual(1);
			expect(cols).toBeLessThanOrEqual(n);
			expect(cols * rows).toBeGreaterThanOrEqual(n);
			expect((rows - 1) * cols).toBeLessThan(n);
		}
	});
});

describe('buildFloorPlan — the live roster', () => {
	const { citizens, guilds } = liveRoster();
	const plan = buildFloorPlan(citizens, guilds);

	it('gives every program a room, including the unstaffed ones', () => {
		const programRooms = plan.rooms.filter((r) => r.kind === 'program');
		expect(programRooms.map((r) => r.id).sort()).toEqual([
			'commons',
			'company_strategy',
			'content_calendar',
			'daily_ops',
			'engineering_strategy',
			'harness_reliability',
			'marketing_strategy',
			'product_ops',
			'product_strategy',
			'revenue_strategy',
			'sprint_plan'
		]);
		expect(plan.stats.programs).toBe(11);
	});

	it('marks exactly the four unworked programs vacant', () => {
		const vacant = plan.rooms.filter((r) => r.vacant).map((r) => r.id).sort();
		expect(vacant).toEqual(['daily_ops', 'marketing_strategy', 'product_strategy', 'sprint_plan']);
		expect(plan.stats.vacantPrograms).toBe(4);
		for (const room of plan.rooms.filter((r) => r.vacant)) {
			expect(room.seats).toHaveLength(0);
			expect(room.memberIds).toHaveLength(0);
		}
	});

	it('sizes rooms by team — Commons carries five desks, singles carry one', () => {
		const commons = plan.rooms.find((r) => r.id === 'commons')!;
		expect(commons.seats).toHaveLength(5);
		expect(plan.rooms.find((r) => r.id === 'revenue_strategy')!.seats).toHaveLength(1);
		expect(commons.w).toBeGreaterThan(plan.rooms.find((r) => r.id === 'daily_ops')!.w);
	});

	it('always builds the meeting room, the pantry and the lounge', () => {
		expect(plan.rooms.filter((r) => r.kind === 'meeting')).toHaveLength(1);
		expect(plan.rooms.filter((r) => r.kind === 'pantry')).toHaveLength(1);
		expect(plan.rooms.filter((r) => r.kind === 'lounge')).toHaveLength(1);
	});

	it('puts a water cooler on the floor', () => {
		const coolers = [...plan.props, ...plan.rooms.flatMap((r) => r.props)].filter(
			(p) => p.kind === 'cooler'
		);
		expect(coolers.length).toBeGreaterThanOrEqual(1);
	});
});

describe('buildFloorPlan — invariants across roster sizes', () => {
	const rosters: Array<[string, CitizenVM[], GuildVM[]]> = [
		['0 crew', [], []],
		['0 crew but known programs', [], [guild('commons'), guild('daily_ops')]],
		['1 crew', [citizen('solo', 'commons')], []],
		['11 crew (live)', liveRoster().citizens, liveRoster().guilds],
		[
			'40 crew',
			Array.from({ length: 40 }, (_, i) =>
				citizen(`agent-${String(i).padStart(2, '0')}`, `program_${i % 7}`)
			),
			Array.from({ length: 9 }, (_, i) => guild(`program_${i}`))
		]
	];

	for (const [label, citizens, guilds] of rosters) {
		describe(label, () => {
			const plan = buildFloorPlan(citizens, guilds);

			it('seats every crew member exactly once', () => {
				const seated = plan.seats.map((s) => s.citizenId);
				expect(seated).toHaveLength(citizens.length);
				expect(new Set(seated).size).toBe(citizens.length);
				expect([...seated].sort()).toEqual(citizens.map((c) => c.id).sort());
			});

			it('never shares a desk', () => {
				const ids = plan.seats.map((s) => s.id);
				expect(new Set(ids).size).toBe(ids.length);
				const points = plan.seats.map((s) => `${s.x.toFixed(3)}:${s.y.toFixed(3)}`);
				expect(new Set(points).size).toBe(points.length);
			});

			it('seats people in the room they belong to', () => {
				for (const room of plan.rooms) {
					expect(room.seats.map((s) => s.citizenId)).toEqual(room.memberIds);
				}
				for (const c of citizens) {
					const room = plan.rooms.find((r) => r.memberIds.includes(c.id))!;
					expect(room.id).toBe(c.guildId);
				}
			});

			it('never overlaps two rooms', () => {
				for (let i = 0; i < plan.rooms.length; i++) {
					for (let j = i + 1; j < plan.rooms.length; j++) {
						expect(
							overlaps(plan.rooms[i], plan.rooms[j]),
							`${plan.rooms[i].id} overlaps ${plan.rooms[j].id}`
						).toBe(false);
					}
				}
			});

			it('keeps every room inside the shell and clear of the corridor', () => {
				for (const room of plan.rooms) {
					expect(insideShell(plan, room), `${room.id} escapes the shell`).toBe(true);
					expect(overlaps(room, plan.corridor), `${room.id} eats the corridor`).toBe(false);
				}
			});

			it('fills both bands flush to the shell', () => {
				for (const band of ['north', 'south'] as const) {
					const inBand = plan.rooms.filter((r) => r.band === band);
					expect(inBand.length).toBeGreaterThan(0);
					const left = Math.min(...inBand.map((r) => r.x));
					const right = Math.max(...inBand.map((r) => r.x + r.w));
					expect(left).toBe(plan.wall);
					expect(right).toBe(plan.width - plan.wall);
				}
			});

			it('keeps every desk inside its own room', () => {
				for (const room of plan.rooms) {
					for (const seat of room.seats) {
						expect(seat.x).toBeGreaterThan(room.x);
						expect(seat.x).toBeLessThan(room.x + room.w);
						expect(seat.y).toBeGreaterThan(room.y);
						expect(seat.y).toBeLessThan(room.y + room.h);
					}
				}
			});

			it('opens every door onto the corridor', () => {
				for (const room of plan.rooms) {
					const edge = room.band === 'north' ? plan.corridor.y : plan.corridor.y + plan.corridor.h;
					expect(room.door.y).toBe(edge);
					expect(room.door.w).toBeGreaterThan(0);
					expect(room.door.x - room.door.w / 2).toBeGreaterThanOrEqual(room.x);
					expect(room.door.x + room.door.w / 2).toBeLessThanOrEqual(room.x + room.w);
				}
			});

			it('is deterministic for a given roster', () => {
				const again = buildFloorPlan(citizens, guilds);
				expect(JSON.stringify(again)).toEqual(JSON.stringify(plan));
			});

			it('does not depend on the order the roster arrives in', () => {
				const shuffled = buildFloorPlan([...citizens].reverse(), [...guilds].reverse());
				expect(JSON.stringify(shuffled)).toEqual(JSON.stringify(plan));
			});
		});
	}
});

describe('buildFloorPlan — nothing escapes the shell, at any roster or any pane', () => {
	const rosters: Array<[string, CitizenVM[], GuildVM[]]> = [
		['0 crew', [], []],
		['1 crew', [citizen('solo', 'commons')], []],
		['11 crew (live)', liveRoster().citizens, liveRoster().guilds],
		[
			'40 crew',
			Array.from({ length: 40 }, (_, i) =>
				citizen(`agent-${String(i).padStart(2, '0')}`, `program_${i % 7}`)
			),
			Array.from({ length: 9 }, (_, i) => guild(`program_${i}`))
		]
	];

	for (const [label, citizens, guilds] of rosters) {
		for (const [paneLabel, aspect] of PANES) {
			it(`${label} in ${paneLabel}`, () => {
				const plan = buildFloorPlan(citizens, guilds, aspect);
				// The shell tiles exactly: two bands and a corridor, wall to wall.
				expect(plan.bands.north.y).toBe(plan.wall);
				expect(plan.bands.north.y + plan.bands.north.h).toBe(plan.corridor.y);
				expect(plan.corridor.y + plan.corridor.h).toBe(plan.bands.south.y);
				expect(plan.bands.south.y + plan.bands.south.h).toBe(plan.height - plan.wall);
				expect(plan.corridor.h).toBeGreaterThan(0);

				for (const room of plan.rooms) {
					expect(insideShell(plan, room), `${room.id} escapes the shell`).toBe(true);
				}
				for (const band of ['north', 'south'] as const) {
					const inBand = plan.rooms.filter((r) => r.band === band);
					expect(Math.min(...inBand.map((r) => r.x))).toBe(plan.wall);
					expect(Math.max(...inBand.map((r) => r.x + r.w))).toBe(plan.width - plan.wall);
				}
				// Every prop, in a room or in the corridor, stays on the plate.
				for (const prop of [...plan.props, ...plan.rooms.flatMap((r) => r.props)]) {
					expect(prop.x - prop.w / 2, `${prop.id} runs off the left`).toBeGreaterThanOrEqual(-0.001);
					expect(prop.x + prop.w / 2, `${prop.id} runs off the right`).toBeLessThanOrEqual(
						plan.width + 0.001
					);
					expect(prop.y - prop.h / 2).toBeGreaterThanOrEqual(-0.001);
					expect(prop.y + prop.h / 2).toBeLessThanOrEqual(plan.height + 0.001);
					expect(prop.w).toBeGreaterThan(0);
					expect(prop.h).toBeGreaterThan(0);
				}
				// And every seat block, which is taller than the desk it belongs to.
				for (const seat of plan.seats) {
					expect(seat.y + plan.seatBox.dy).toBeGreaterThanOrEqual(plan.wall - 0.001);
					expect(seat.y + plan.seatBox.dy + plan.seatBox.h).toBeLessThanOrEqual(
						plan.height - plan.wall + 0.001
					);
				}
			});
		}
	}
});

describe('buildFloorPlan — solves to the pane it is drawn into', () => {
	const rosters: Array<[string, CitizenVM[], GuildVM[]]> = [
		['1 crew', [citizen('solo', 'commons')], []],
		['11 crew (live)', liveRoster().citizens, liveRoster().guilds],
		[
			'40 crew',
			Array.from({ length: 40 }, (_, i) =>
				citizen(`agent-${String(i).padStart(2, '0')}`, `program_${i % 7}`)
			),
			Array.from({ length: 9 }, (_, i) => guild(`program_${i}`))
		]
	];

	for (const [label, citizens, guilds] of rosters) {
		for (const [paneLabel, aspect] of PANES) {
			it(`${label} in ${paneLabel} comes out the pane's shape`, () => {
				const plan = buildFloorPlan(citizens, guilds, aspect);
				expect(plan.targetAspect).toBe(aspect);
				// A roster with enough rooms to fill the pane must match it. A tiny
				// one is allowed to stop short rather than inflate a lounge to half a
				// screen — but it may never come out WIDER than asked for, because
				// that is the letterbox this whole solve exists to remove.
				expect(plan.aspect).toBeLessThanOrEqual(aspect * (1 + ASPECT_TOLERANCE));
				if (citizens.length >= 11) {
					expect(Math.abs(plan.aspect - aspect) / aspect).toBeLessThan(ASPECT_TOLERANCE);
				}
			});
		}
	}

	it('fills more of a 16:9 pane than the same plan fitted at a fixed shape', () => {
		const { citizens, guilds } = liveRoster();
		const solved = buildFloorPlan(citizens, guilds, 1.75);
		// The shape the plan would take if it ignored the pane — the plan solved
		// for some OTHER pane, then contained into this one.
		const naive = buildFloorPlan(citizens, guilds, 3.2);
		const coverage = (plan: FloorPlan): number => {
			const s = Math.min(1.75 / plan.width, 1 / plan.height);
			return plan.width * s * plan.height * s;
		};
		expect(coverage(solved)).toBeGreaterThan(coverage(naive) * 1.1);
		expect(coverage(solved)).toBeGreaterThan(1.75 * 0.98);
	});

	it('re-solves for a different pane without disturbing who sits where', () => {
		const { citizens, guilds } = liveRoster();
		const wide = buildFloorPlan(citizens, guilds, 2.4);
		const tall = buildFloorPlan(citizens, guilds, 1.2);
		const seating = (plan: FloorPlan): string =>
			plan.rooms
				.map((r) => `${r.id}:${r.memberIds.join(',')}`)
				.sort()
				.join('|');
		expect(seating(wide)).toEqual(seating(tall));
	});

	it('is deterministic for a given roster AND aspect', () => {
		const { citizens, guilds } = liveRoster();
		for (const [, aspect] of PANES) {
			expect(JSON.stringify(buildFloorPlan(citizens, guilds, aspect))).toEqual(
				JSON.stringify(buildFloorPlan([...citizens].reverse(), guilds, aspect))
			);
		}
	});

	it('ignores an aspect that is not a usable number', () => {
		const { citizens, guilds } = liveRoster();
		const fallback = JSON.stringify(buildFloorPlan(citizens, guilds));
		for (const bad of [Number.NaN, 0, -3, Number.POSITIVE_INFINITY, 99]) {
			expect(JSON.stringify(buildFloorPlan(citizens, guilds, bad))).toEqual(fallback);
		}
	});
});

describe('buildFloorPlan — width goes to the crew, not to empty rooms', () => {
	const { citizens, guilds } = liveRoster();

	for (const [paneLabel, aspect] of PANES) {
		it(`keeps every unstaffed room narrower than every staffed one in ${paneLabel}`, () => {
			const plan = buildFloorPlan(citizens, guilds, aspect);
			const staffed = plan.rooms.filter((r) => r.kind === 'program' && !r.vacant);
			const vacant = plan.rooms.filter((r) => r.vacant);
			expect(vacant.length).toBeGreaterThan(0);
			expect(staffed.length).toBeGreaterThan(0);
			// Not "on average" — the widest vacancy must be narrower than the
			// narrowest staffed room, or a program nobody works still costs the
			// frontage of one that someone does.
			expect(Math.max(...vacant.map((r) => r.w))).toBeLessThan(
				Math.min(...staffed.map((r) => r.w))
			);
		});
	}

	it('hands surplus width to staffed rooms only', () => {
		const plan = buildFloorPlan(citizens, guilds, 2.6);
		const natural = buildFloorPlan(citizens, guilds, 1.0);
		const vacantW = (p: FloorPlan): number[] => p.rooms.filter((r) => r.vacant).map((r) => r.w);
		// The wide pane needs far more width than the narrow one, and none of it
		// lands in a room with nobody in it.
		expect(plan.width).toBeGreaterThan(natural.width);
		expect(vacantW(plan)).toEqual(vacantW(natural));
	});

	it('scales a room to its team rather than levelling every room', () => {
		const plan = buildFloorPlan(citizens, guilds, 1.75);
		const commons = plan.rooms.find((r) => r.id === 'commons')!;
		const single = plan.rooms.find((r) => r.id === 'revenue_strategy')!;
		expect(commons.w).toBeGreaterThan(single.w * 1.5);
	});
});

describe('buildFloorPlan — the crew read as tokens on a map', () => {
	const plan = buildFloorPlan(liveRoster().citizens, liveRoster().guilds, 1.75);

	it('draws people smaller than the furniture around them', () => {
		// This is a top-down plan with front-facing figures on it. That reads as a
		// map only while a figure is small; drawn large it becomes a portrait
		// pasted onto a blueprint and the whole floor stares back. The head is the
		// part that gives it away, so the assertion is on the head: it stays a
		// small fraction of the desk it sits behind.
		expect(plan.personScale).toBeCloseTo(plan.seatScale * PERSON_RATIO, 6);
		expect(PERSON_RATIO).toBeLessThan(1);
		const headW = 24 * plan.personScale;
		const deskW = 72 * plan.seatScale;
		expect(headW / deskW).toBeLessThan(0.28);
		// …and not so small that a seat reads as empty floor.
		expect(headW / deskW).toBeGreaterThan(0.14);
	});

	it('gives the row pitch enough clearance for the figure and its plaque', () => {
		// The seat box spans the name plaque, the figure under it and the desk. If
		// the pitch were shorter than the box, one row's plaque would be drawn
		// through the desk of the row in front of it.
		expect(plan.deskCell.h).toBeGreaterThanOrEqual(plan.seatBox.h);
		expect(plan.seatBox.dy).toBeLessThan(0);
		// The plaque hangs from the TOP of the box, above the tallest head.
		expect(plan.seatBox.plateH).toBeGreaterThan(0);
		expect(plan.seatBox.plateH).toBeLessThan(plan.seatBox.h);
	});

	it('keeps the whole seat block — plaque, person and desk — inside its room', () => {
		for (const room of plan.rooms) {
			for (const seat of room.seats) {
				expect(seat.y + plan.seatBox.dy).toBeGreaterThanOrEqual(room.y - 0.001);
				expect(seat.y + plan.seatBox.dy + plan.seatBox.h).toBeLessThanOrEqual(
					room.y + room.h + 0.001
				);
				expect(seat.x + plan.seatBox.dx).toBeGreaterThanOrEqual(room.x - 0.001);
				expect(seat.x + plan.seatBox.dx + plan.seatBox.w).toBeLessThanOrEqual(
					room.x + room.w + 0.001
				);
			}
		}
	});

	it('seats the figure below the desk centre so the slab crosses the body', () => {
		for (const seat of plan.seats) {
			expect(seat.personY).toBeGreaterThan(seat.y);
			expect(seat.personX).toBe(seat.x);
		}
	});
});

describe('buildFloorPlan — degenerate input', () => {
	it('still builds an office with nobody in it', () => {
		const plan = buildFloorPlan([], []);
		expect(plan.seats).toHaveLength(0);
		expect(plan.width).toBeGreaterThan(0);
		expect(plan.height).toBeGreaterThan(0);
		expect(plan.rooms.map((r) => r.kind).sort()).toEqual(['lounge', 'meeting', 'pantry']);
	});

	it('invents a room for a citizen whose program nobody declared', () => {
		const plan = buildFloorPlan([citizen('orphan', 'nowhere_program')], []);
		const room = plan.rooms.find((r) => r.id === 'nowhere_program');
		expect(room).toBeDefined();
		expect(room!.seats.map((s) => s.citizenId)).toEqual(['orphan']);
	});

	it('grows the building rather than overflowing it when the crew grows', () => {
		const small = buildFloorPlan(
			Array.from({ length: 4 }, (_, i) => citizen(`a${i}`, 'commons')),
			[]
		);
		const large = buildFloorPlan(
			Array.from({ length: 60 }, (_, i) => citizen(`a${i}`, 'commons')),
			[]
		);
		expect(large.width).toBeGreaterThan(small.width);
		expect(large.seats).toHaveLength(60);
	});
});
