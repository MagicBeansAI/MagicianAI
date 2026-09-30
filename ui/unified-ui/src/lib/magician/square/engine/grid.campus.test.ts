import { describe, expect, it } from 'vitest';

import { astar, buildVillage, GRID_N, idx, isWalkable, type Tile, type Village } from './grid';
import type { CitizenVM, GuildVM } from './types';

/**
 * Layout invariants for the village lattice.
 *
 * These pin the contract every generator must satisfy, independent of HOW the
 * town is shaped (radial ring, orthogonal campus, anything later). The failure
 * they exist to catch is silent: an unwalkable or unreachable door strands a
 * citizen at their bench forever, because `astar` returns an empty path and
 * `CitizenLayer.startRoute` then deliberately stops steering rather than
 * walking off-graph. Nothing logs, nothing throws — the crew member just
 * never moves again.
 */

const guild = (id: string): GuildVM => ({
	id,
	name: id,
	memberIds: [],
	activeQuestCount: 0,
	blockedQuestCount: 0,
	deliveredQuestCount: 0
});

// Only the fields buildVillage reads; the VM is much wider in production.
const citizen = (id: string, guildId: string): CitizenVM =>
	({ id, guildId }) as unknown as CitizenVM;

const WALKABLE_KINDS = ['road', 'plaza'];

/** Every walkable tile 4-connected to `seed` — the graph citizens actually move on. */
function reachableFrom(village: Village, seed: Tile): Set<number> {
	const { n, kinds } = village;
	const start = idx(n, seed.x, seed.z);
	const seen = new Set<number>([start]);
	const queue: number[] = [start];
	for (let qi = 0; qi < queue.length; qi++) {
		const cur = queue[qi];
		const cx = cur % n;
		const cz = Math.floor(cur / n);
		for (const [nx, nz] of [
			[cx + 1, cz],
			[cx - 1, cz],
			[cx, cz + 1],
			[cx, cz - 1]
		]) {
			if (nx < 0 || nz < 0 || nx >= n || nz >= n) continue;
			const ni = idx(n, nx, nz);
			if (seen.has(ni) || !isWalkable(kinds[ni])) continue;
			seen.add(ni);
			queue.push(ni);
		}
	}
	return seen;
}

const kindAt = (village: Village, t: Tile): string => village.kinds[idx(village.n, t.x, t.z)];

describe('campus lattice', () => {
	const guilds = [guild('alpha'), guild('beta'), guild('gamma'), guild('delta')];
	const citizens = [citizen('a', 'alpha'), citizen('b', 'beta'), citizen('c', 'gamma')];
	const village = buildVillage(citizens, guilds);

	it('reports the grid size it was built with', () => {
		expect(village.n).toBe(GRID_N);
		expect(village.kinds).toHaveLength(GRID_N * GRID_N);
	});

	it('always leaves plaza tiles — four fallbacks index plazaTiles[0]', () => {
		expect(village.plazaTiles.length).toBeGreaterThan(0);
		// Every plaza tile must really be plaza, or the fallbacks hand callers a
		// tile citizens cannot stand on.
		for (const t of village.plazaTiles) expect(kindAt(village, t)).toBe('plaza');
	});

	it('keeps the hall front walkable — every citizen faces and routes to it', () => {
		expect(WALKABLE_KINDS).toContain(kindAt(village, village.hall.front));
	});

	it('publishes a road index that matches the tile map exactly', () => {
		// startWander draws its walk destination from roadTiles (weighted towards
		// the citizen's own block), so a stale or hand-built index sends citizens
		// at tiles that are not roads at all.
		expect(village.roadTiles.length).toBeGreaterThan(0);
		for (const t of village.roadTiles) expect(kindAt(village, t)).toBe('road');
		const indexed = new Set(village.roadTiles.map((t) => idx(village.n, t.x, t.z)));
		const onMap = village.kinds.filter((k) => k === 'road').length;
		expect(indexed.size).toBe(onMap);
	});

	it('builds one plot per guild, so the door checks below are never vacuous', () => {
		expect(village.buildings).toHaveLength(guilds.length);
		expect(village.buildings.map((b) => b.guildId)).toEqual(guilds.map((g) => g.id));
		expect(village.landmarks.map((l) => l.id)).toEqual(['armory', 'council']);
	});

	it('puts every guild door on a walkable tile', () => {
		for (const b of village.buildings) {
			expect(WALKABLE_KINDS).toContain(kindAt(village, b.door));
		}
	});

	it('puts every landmark door on a walkable tile', () => {
		for (const l of village.landmarks) {
			expect(WALKABLE_KINDS).toContain(kindAt(village, l.door));
		}
	});

	it('routes from every guild door to the hall front', () => {
		for (const b of village.buildings) {
			expect(astar(village, b.door, village.hall.front).length).toBeGreaterThan(0);
		}
	});

	it('routes from every landmark door to the hall front', () => {
		for (const l of village.landmarks) {
			expect(astar(village, l.door, village.hall.front).length).toBeGreaterThan(0);
		}
	});

	it('routes doors over walkable tiles only — never through a wall', () => {
		// astar exempts its own start/goal from the walkability filter, so a
		// reachability assertion alone can hide a door sunk into a building.
		for (const b of village.buildings) {
			for (const t of astar(village, b.door, village.hall.front)) {
				expect(WALKABLE_KINDS).toContain(kindAt(village, t));
			}
		}
	});

	it('leaves every door in the same connected component as the hall', () => {
		const reachable = reachableFrom(village, village.hall.front);
		for (const b of village.buildings) {
			expect(reachable.has(idx(village.n, b.door.x, b.door.z))).toBe(true);
		}
		for (const l of village.landmarks) {
			expect(reachable.has(idx(village.n, l.door.x, l.door.z))).toBe(true);
		}
	});

	it('never overlaps two building footprints', () => {
		const seen = new Set<number>();
		for (const b of [...village.buildings, ...village.landmarks]) {
			for (let z = b.z; z < b.z + b.size; z++) {
				for (let x = b.x; x < b.x + b.size; x++) {
					const key = idx(GRID_N, x, z);
					expect(seen.has(key)).toBe(false);
					seen.add(key);
				}
			}
		}
	});

	it('never puts a footprint tile outside the grid', () => {
		for (const b of [...village.buildings, ...village.landmarks]) {
			expect(b.x).toBeGreaterThanOrEqual(0);
			expect(b.z).toBeGreaterThanOrEqual(0);
			expect(b.x + b.size).toBeLessThanOrEqual(GRID_N);
			expect(b.z + b.size).toBeLessThanOrEqual(GRID_N);
		}
	});

	it('gives every citizen a work spot and a plaza spot', () => {
		for (const c of citizens) {
			expect(village.workSpotOf.get(c.id)).toBeDefined();
			expect(village.plazaSpotOf.get(c.id)).toBeDefined();
		}
	});

	it('puts every assigned spot on a tile a citizen can stand on', () => {
		for (const c of citizens) {
			expect(WALKABLE_KINDS).toContain(kindAt(village, village.workSpotOf.get(c.id)!));
			expect(WALKABLE_KINDS).toContain(kindAt(village, village.plazaSpotOf.get(c.id)!));
		}
	});

	it('lets every citizen commute between their bench and their plaza spot', () => {
		// The vibe machine drives work <-> plaza on every status change. If either
		// leg has no route the citizen freezes at whichever end they were on.
		for (const c of citizens) {
			const work = village.workSpotOf.get(c.id)!;
			const plaza = village.plazaSpotOf.get(c.id)!;
			expect(astar(village, work, plaza).length).toBeGreaterThan(0);
			expect(astar(village, plaza, work).length).toBeGreaterThan(0);
		}
	});

	it('is deterministic for a given roster', () => {
		const again = buildVillage(citizens, guilds);
		expect(again.kinds).toEqual(village.kinds);
		expect(again.buildings).toEqual(village.buildings);
		expect(again.landmarks).toEqual(village.landmarks);
	});
});

describe('campus lattice — roster shapes', () => {
	// A layout that only works at one roster size is a layout that strands crews
	// on real fleets. Structural invariants must hold from an empty town upward.
	const sizes = [0, 1, 2, 3, 4, 8, 12];

	it.each(sizes)('holds the layout contract with %i guilds', (count) => {
		const guilds = Array.from({ length: count }, (_, i) => guild(`g${i}`));
		const citizens = Array.from({ length: count * 3 || 1 }, (_, i) =>
			citizen(`c${i}`, `g${i % (count || 1)}`)
		);
		const village = buildVillage(citizens, guilds);

		expect(village.plazaTiles.length).toBeGreaterThan(0);
		expect(village.buildings).toHaveLength(count);
		expect(village.landmarks).toHaveLength(2);
		expect(WALKABLE_KINDS).toContain(kindAt(village, village.hall.front));

		const reachable = reachableFrom(village, village.hall.front);
		for (const b of village.buildings) {
			expect(WALKABLE_KINDS).toContain(kindAt(village, b.door));
			expect(reachable.has(idx(village.n, b.door.x, b.door.z))).toBe(true);
		}
		for (const l of village.landmarks) {
			expect(WALKABLE_KINDS).toContain(kindAt(village, l.door));
			expect(reachable.has(idx(village.n, l.door.x, l.door.z))).toBe(true);
		}

		const seen = new Set<number>();
		for (const b of [...village.buildings, ...village.landmarks]) {
			for (let z = b.z; z < b.z + b.size; z++) {
				for (let x = b.x; x < b.x + b.size; x++) {
					const key = idx(village.n, x, z);
					expect(seen.has(key)).toBe(false);
					seen.add(key);
				}
			}
		}

		// The zero-guild town is the one that actually exercises the three
		// `plazaTiles[0]` fallbacks, so assert the spots survive it.
		for (const c of citizens) {
			expect(village.workSpotOf.get(c.id)).toBeDefined();
			expect(village.plazaSpotOf.get(c.id)).toBeDefined();
		}
	});
});

describe('campus lattice — beyond lot capacity', () => {
	/**
	 * The lattice has a FIXED number of lots, so a roster can outgrow the map:
	 * 48 tiles at BLOCK 8 give 35, which is the two landmarks plus 33 programs.
	 * What happens past that is degraded, and pinned here so it stays a decision
	 * rather than a surprise. Two consequences, both silent in the running app:
	 *
	 *   - `civWorld.ts`'s `if (!plot) return` skips the building, its pick mesh
	 *     and its `buildingCenterOf` entry, so `engine.ts` returns null and
	 *     selecting or focusing that program from the HUD does nothing at all.
	 *   - the `?? buildings[0]` fallback at the end of buildVillage parks every
	 *     unhoused guild's crew on the FIRST guild's benches — crew standing at
	 *     another program's building.
	 *
	 * If the campus is ever expected to carry more programs than this, the fix
	 * is more lots (a denser lattice or a bigger grid), not a softer fallback.
	 */
	const OVER = 40;
	const guilds = Array.from({ length: OVER }, (_, i) => guild(`g${i}`));
	const citizens = Array.from({ length: OVER * 3 }, (_, i) => citizen(`c${i}`, `g${i % OVER}`));
	const village = buildVillage(citizens, guilds);
	const CAPACITY = 33;

	it('houses guilds up to the lot supply and no further', () => {
		expect(village.buildings).toHaveLength(CAPACITY);
		expect(village.buildings.length).toBeLessThan(guilds.length);
		// Housed guilds are a prefix of the roster: adding a guild never evicts
		// one that already had a plot.
		expect(village.buildings.map((b) => b.guildId)).toEqual(
			guilds.slice(0, CAPACITY).map((g) => g.id)
		);
	});

	it('still holds every layout invariant for the guilds it did house', () => {
		const reachable = reachableFrom(village, village.hall.front);
		for (const b of village.buildings) {
			expect(WALKABLE_KINDS).toContain(kindAt(village, b.door));
			for (const t of b.workSpots) {
				expect(WALKABLE_KINDS).toContain(kindAt(village, t));
				expect(reachable.has(idx(village.n, t.x, t.z))).toBe(true);
			}
		}
	});

	it('parks unhoused crew on the first guild bench — the known degradation', () => {
		const housed = new Set(village.buildings.map((b) => b.guildId));
		const firstBenches = new Set(
			village.buildings[0].workSpots.map((t) => idx(village.n, t.x, t.z))
		);
		const unhoused = citizens.filter((c) => !housed.has(c.guildId));
		expect(unhoused.length).toBeGreaterThan(0);
		for (const c of unhoused) {
			const spot = village.workSpotOf.get(c.id)!;
			// They can stand and route — they are simply at the wrong building.
			expect(WALKABLE_KINDS).toContain(kindAt(village, spot));
			expect(firstBenches.has(idx(village.n, spot.x, spot.z))).toBe(true);
		}
	});
});

describe('campus lattice — known defect', () => {
	/**
	 * PRE-EXISTING BUG in the radial generator, measured 2026-08-15 — NOT caused
	 * by the campus rewrite. `buildVillage` carves each guild's bench tiles
	 * straight out of the grass hugging the footprint, but only ever roads the
	 * door through to the plaza. The benches on the far side of a building form
	 * an isolated 2-4 tile road island touching nothing.
	 *
	 * It stays invisible on small fleets because work spots are handed out
	 * round-robin and index 0 is always the (connected) door. It bites as soon as
	 * a guild has more members than it has connected benches:
	 *   4 guilds /  3 citizens -> 11 orphan bench tiles,  0 citizens stranded
	 *   8 guilds / 24 citizens -> 17 orphan bench tiles,  8 citizens stranded
	 *  12 guilds / 36 citizens -> 27 orphan bench tiles, 13 citizens stranded
	 * A stranded citizen never moves again and nothing reports it.
	 *
	 * The campus lattice closes this by construction: avenues are paved before
	 * any structure, every lot fronts one, and benches only ever take tiles the
	 * lattice already paved — no tile is converted, so no island can form.
	 */
	it('leaves no guild bench cut off from the road network', () => {
		const guilds = Array.from({ length: 12 }, (_, i) => guild(`g${i}`));
		const citizens = Array.from({ length: 36 }, (_, i) => citizen(`c${i}`, `g${i % 12}`));
		const village = buildVillage(citizens, guilds);
		const reachable = reachableFrom(village, village.hall.front);

		const orphans: string[] = [];
		for (const b of village.buildings) {
			for (const t of b.workSpots) {
				if (!reachable.has(idx(village.n, t.x, t.z))) orphans.push(`${b.guildId}(${t.x},${t.z})`);
			}
		}
		expect(orphans).toEqual([]);
	});
});
