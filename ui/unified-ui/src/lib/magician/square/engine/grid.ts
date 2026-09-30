import type { CitizenVM, GuildVM } from './types';

/**
 * The campus tile grid: layout generation + A* pathfinding.
 *
 * The world is an orthogonal lattice: avenues on a regular grid, one 2x2 lot in
 * the north-west corner of every block, and an open court at the centre holding
 * the Hall. Avenues are paved BEFORE anything is placed and nothing is ever
 * placed on one, so every door and bench sits on the same connected network by
 * construction — no post-hoc carving, no road islands.
 *
 * The grid IS the movement contract: citizens route with A* over walkable tiles
 * only (roads + plaza), so they cannot cross buildings/trees — a hard
 * constraint, no physics engine needed (see the design doc's movement section).
 * Layout is deterministic for a given roster (seeded by guild/citizen ids) so
 * the campus doesn't reshuffle between visits.
 */

export const GRID_N = 48;

/**
 * Avenue spacing: paths run edge to edge every BLOCK tiles on both axes,
 * leaving (BLOCK-1)-square blocks between them. Even values only — an odd
 * BLOCK cannot centre the quad on a block. The floors below are why a bad edit
 * lands as a merely wrong layout rather than a corrupt one: a fractional phase
 * or half-extent yields fractional tile coordinates, and `idx()` would then
 * write string keys onto the tile array instead of throwing.
 */
const BLOCK = 8;

/**
 * Half-extent of the central court — the campus quad, in place of the old
 * plaza ring around a keep. BLOCK/2 - 1 makes the quad exactly the centre
 * block, so it is bounded by an avenue on all four sides.
 */
const QUAD_HALF = Math.floor(BLOCK / 2) - 1;

export type TileKind = 'grass' | 'road' | 'plaza' | 'building' | 'tree' | 'rock';

export interface Tile {
	x: number;
	z: number;
}

export interface BuildingPlot {
	guildId: string;
	/** SW corner tile of the 2x2 footprint. */
	x: number;
	z: number;
	size: number;
	/** Walkable tile in front of the door: the avenue tile due north of the lot. */
	door: Tile;
	/** Walkable bench tiles where members stand to work. */
	workSpots: Tile[];
}

export interface LandmarkPlot {
	id: 'armory' | 'council';
	/** SW corner tile of the 2x2 footprint. */
	x: number;
	z: number;
	size: number;
	door: Tile;
}

export interface Village {
	n: number;
	kinds: TileKind[]; // n*n, index z*n+x
	hall: { x: number; z: number; size: number; front: Tile };
	buildings: BuildingPlot[];
	landmarks: LandmarkPlot[];
	plazaTiles: Tile[];
	roadTiles: Tile[];
	/** citizenId -> assigned work spot */
	workSpotOf: Map<string, Tile>;
	/** citizenId -> assigned plaza spot (needs-you destination) */
	plazaSpotOf: Map<string, Tile>;
}

export function idx(n: number, x: number, z: number): number {
	return z * n + x;
}

export function inBounds(n: number, x: number, z: number): boolean {
	return x >= 0 && z >= 0 && x < n && z < n;
}

export function isWalkable(kind: TileKind): boolean {
	return kind === 'road' || kind === 'plaza';
}

/** Tile centre -> world coords (grid centred at origin, 1 tile = 1 unit). */
export function tileToWorld(n: number, t: Tile): { x: number; z: number } {
	return { x: t.x - n / 2 + 0.5, z: t.z - n / 2 + 0.5 };
}

/** Deterministic PRNG so the village layout is stable per roster. */
function lcg(seed: number): () => number {
	let s = seed >>> 0 || 1;
	return () => {
		s = (s * 1664525 + 1013904223) >>> 0;
		return s / 0xffffffff;
	};
}

function hashString(str: string): number {
	let h = 2166136261;
	for (let i = 0; i < str.length; i++) {
		h ^= str.charCodeAt(i);
		h = Math.imul(h, 16777619);
	}
	return h >>> 0;
}

/**
 * Carve a connected road between two tiles.
 *
 * The campus lattice does not need this — its avenues are laid before any
 * structure and no structure is ever placed on one, so the network is
 * connected by construction. Kept for layouts that place first and connect
 * after.
 */
function carveRoad(kinds: TileKind[], n: number, from: Tile, to: Tile): void {
	// BFS over every non-building tile, then mark the found route as road.
	// (The old L-path skipped tiles blocked by other buildings, leaving road
	// spurs DISCONNECTED — which is what sent citizens through walls once the
	// A* fallback kicked in. This always routes AROUND buildings.)
	const start = idx(n, from.x, from.z);
	const goal = idx(n, to.x, to.z);
	if (start === goal) return;
	const prev = new Int32Array(n * n).fill(-1);
	const queue: number[] = [start];
	prev[start] = start;
	let found = false;
	for (let qi = 0; qi < queue.length && !found; qi++) {
		const cur = queue[qi];
		const cx = cur % n;
		const cz = Math.floor(cur / n);
		for (const [nx, nz] of [
			[cx + 1, cz],
			[cx - 1, cz],
			[cx, cz + 1],
			[cx, cz - 1]
		]) {
			if (!inBounds(n, nx, nz)) continue;
			const ni = idx(n, nx, nz);
			if (prev[ni] !== -1 || kinds[ni] === 'building') continue;
			prev[ni] = cur;
			if (ni === goal) {
				found = true;
				break;
			}
			queue.push(ni);
		}
	}
	if (!found) return;
	let cur = goal;
	while (cur !== start) {
		const k = kinds[cur];
		if (k === 'grass' || k === 'tree' || k === 'rock') kinds[cur] = 'road';
		cur = prev[cur];
	}
}

export function buildVillage(citizens: CitizenVM[], guilds: GuildVM[]): Village {
	const n = GRID_N;
	const kinds: TileKind[] = new Array(n * n).fill('grass');
	const c = Math.floor(n / 2);

	// --- Avenue lattice: straight paths edge to edge every BLOCK tiles on both
	//     axes. The phase offsets the lattice so the grid centre lands in the
	//     MIDDLE of a block rather than on a crossing. That centre block becomes
	//     the quad, and — because every plot sits inside a block — no structure
	//     ever lands on an avenue. The road network is therefore one connected
	//     component by construction, with no carving after the fact. ---
	const phase = (c + Math.floor(BLOCK / 2)) % BLOCK;
	const isAvenue = (i: number): boolean => i % BLOCK === phase;
	for (let z = 0; z < n; z++)
		for (let x = 0; x < n; x++) if (isAvenue(x) || isAvenue(z)) kinds[idx(n, x, z)] = 'road';

	// --- Hall (you, the Mayor) dead centre of the quad block: 2x2 footprint ---
	const hall = { x: c - 1, z: c - 1, size: 2, front: { x: c, z: c + 2 } };
	for (let z = hall.z; z < hall.z + hall.size; z++)
		for (let x = hall.x; x < hall.x + hall.size; x++) kinds[idx(n, x, z)] = 'building';

	// --- The quad: the centre block, paved and walkable, ringed by avenues.
	//     Paved rather than green because the terrain palette is a value ladder
	//     with ~10 L* per step and no room for a fifth rung; see the world
	//     palette section of docs/components/unified-ui/fleet-civilization-world.md. ---
	const plazaTiles: Tile[] = [];
	for (let z = c - QUAD_HALF; z <= c + QUAD_HALF; z++) {
		for (let x = c - QUAD_HALF; x <= c + QUAD_HALF; x++) {
			if (!inBounds(n, x, z)) continue;
			if (kinds[idx(n, x, z)] === 'building') continue;
			kinds[idx(n, x, z)] = 'plaza';
			plazaTiles.push({ x, z });
		}
	}

	// --- Lots: one 2x2 plot in the north-west corner of every block, so each
	//     one fronts two avenues. Nearest the quad first, so a small fleet
	//     builds a tight campus core and the outer blocks stay parkland. ---
	const lots: { x: number; z: number; d: number }[] = [];
	for (let bz = phase + 1; bz + 2 <= n; bz += BLOCK) {
		for (let bx = phase + 1; bx + 2 <= n; bx += BLOCK) {
			const overlapsQuad =
				bx <= c + QUAD_HALF &&
				bx + 1 >= c - QUAD_HALF &&
				bz <= c + QUAD_HALF &&
				bz + 1 >= c - QUAD_HALF;
			if (overlapsQuad) continue;
			lots.push({ x: bx, z: bz, d: Math.abs(bx + 0.5 - c) + Math.abs(bz + 0.5 - c) });
		}
	}
	// 48 tiles at BLOCK 8 yield 35 lots — the two landmarks plus 33 guilds. Past
	// that the campus is full and the remaining guilds go unhoused, which
	// degrades in two visible ways pinned by `campus lattice — beyond lot
	// capacity` in grid.campus.test.ts. More programs than this needs more lots
	// (denser lattice or bigger grid), not a softer fallback.
	lots.sort((a, b) => a.d - b.d || a.z - b.z || a.x - b.x);

	// Stamp the next free lot. Doors all face north onto the block's own avenue,
	// so the campus reads as one aligned frontage. Benches only ever take tiles
	// the lattice ALREADY paved — nothing is converted, so a bench can never
	// become a road island cut off from the network.
	let nextLot = 0;
	const takeLot = (): { x: number; z: number; door: Tile; workSpots: Tile[] } | null => {
		const lot = lots[nextLot];
		if (!lot) return null;
		nextLot += 1;
		const bx = lot.x;
		const bz = lot.z;
		for (let z = bz; z < bz + 2; z++)
			for (let x = bx; x < bx + 2; x++) kinds[idx(n, x, z)] = 'building';

		// `bz - 1` is an avenue row for every lot, so the door is already paved.
		// There is deliberately no fallback that paves it: converting a tile here
		// is exactly how the old generator minted isolated road islands. If a
		// future lot offset moves off the avenue the door goes unpaved and the
		// door-walkability cases in grid.campus.test.ts fail loudly.
		const door: Tile = { x: bx, z: bz - 1 };

		// Bench spots: door + paved tiles hugging the footprint along the two
		// avenues the lot fronts.
		const workSpots: Tile[] = [door];
		for (const t of [
			{ x: bx + 1, z: bz - 1 },
			{ x: bx - 1, z: bz },
			{ x: bx - 1, z: bz + 1 },
			{ x: bx - 1, z: bz - 1 },
			{ x: bx + 2, z: bz - 1 }
		]) {
			if (workSpots.length >= 6) break;
			if (!inBounds(n, t.x, t.z)) continue;
			if (isWalkable(kinds[idx(n, t.x, t.z)])) workSpots.push(t);
		}
		return { x: bx, z: bz, door, workSpots };
	};

	// --- Civic landmarks (Armory = tools, Council = command network) take the
	//     two lots nearest the quad ---
	const landmarks: LandmarkPlot[] = [];
	for (const id of ['armory', 'council'] as const) {
		const lot = takeLot();
		if (!lot) break;
		landmarks.push({ id, x: lot.x, z: lot.z, size: 2, door: lot.door });
	}

	// --- Guild buildings fill the remaining lots, nearest the quad first ---
	const buildings: BuildingPlot[] = [];
	for (const guild of guilds) {
		const lot = takeLot();
		if (!lot) break;
		buildings.push({
			guildId: guild.id,
			x: lot.x,
			z: lot.z,
			size: 2,
			door: lot.door,
			workSpots: lot.workSpots
		});
	}

	// --- Trees & rocks on remaining grass (seeded, away from structures) ---
	const seed = hashString(guilds.map((g) => g.id).join('|') + citizens.map((cz) => cz.id).join('|'));
	const rand = lcg(seed);
	for (let z = 0; z < n; z++) {
		for (let x = 0; x < n; x++) {
			if (kinds[idx(n, x, z)] !== 'grass') continue;
			// keep a clear apron next to buildings/roads
			let nearStructure = false;
			for (let dz = -1; dz <= 1 && !nearStructure; dz++) {
				for (let dx = -1; dx <= 1 && !nearStructure; dx++) {
					if (!inBounds(n, x + dx, z + dz)) continue;
					const k = kinds[idx(n, x + dx, z + dz)];
					if (k === 'building') nearStructure = true;
				}
			}
			if (nearStructure) continue;
			const r = rand();
			if (r < 0.04) kinds[idx(n, x, z)] = 'tree';
			else if (r < 0.048) kinds[idx(n, x, z)] = 'rock';
		}
	}

	const roadTiles: Tile[] = [];
	for (let z = 0; z < n; z++)
		for (let x = 0; x < n; x++) if (kinds[idx(n, x, z)] === 'road') roadTiles.push({ x, z });

	// --- Assign spots: work benches round-robin per guild; plaza spots hashed ---
	const workSpotOf = new Map<string, Tile>();
	const plazaSpotOf = new Map<string, Tile>();
	const perGuildCounter = new Map<string, number>();
	const openPlaza = plazaTiles.filter((t) => Math.abs(t.x - c) + Math.abs(t.z - c) >= 2);
	citizens.forEach((cz, i) => {
		const plot = buildings.find((b) => b.guildId === cz.guildId) ?? buildings[0];
		if (plot) {
			const k = perGuildCounter.get(plot.guildId) ?? 0;
			perGuildCounter.set(plot.guildId, k + 1);
			workSpotOf.set(cz.id, plot.workSpots[k % plot.workSpots.length]);
		} else {
			workSpotOf.set(cz.id, plazaTiles[i % plazaTiles.length]);
		}
		plazaSpotOf.set(cz.id, openPlaza[hashString(cz.id) % openPlaza.length] ?? plazaTiles[0]);
	});

	return { n, kinds, hall, buildings, landmarks, plazaTiles, roadTiles, workSpotOf, plazaSpotOf };
}

/** A* over walkable tiles (4-neighbour, Manhattan heuristic). */
export function astar(village: Village, from: Tile, to: Tile): Tile[] {
	const { n, kinds } = village;
	const start = idx(n, from.x, from.z);
	const goal = idx(n, to.x, to.z);
	if (start === goal) return [to];

	const walkableAt = (i: number): boolean => isWalkable(kinds[i]) || i === start || i === goal;

	const open = new Set<number>([start]);
	const came = new Map<number, number>();
	const gScore = new Map<number, number>([[start, 0]]);
	const fScore = new Map<number, number>([[start, 0]]);
	const h = (i: number): number => {
		const x = i % n;
		const z = Math.floor(i / n);
		return Math.abs(x - to.x) + Math.abs(z - to.z);
	};

	let guard = n * n * 4;
	while (open.size > 0 && guard-- > 0) {
		let current = -1;
		let bestF = Infinity;
		for (const i of open) {
			const f = fScore.get(i) ?? Infinity;
			if (f < bestF) {
				bestF = f;
				current = i;
			}
		}
		if (current === goal) {
			const path: Tile[] = [];
			let cur: number | undefined = current;
			while (cur !== undefined && cur !== start) {
				path.push({ x: cur % n, z: Math.floor(cur / n) });
				cur = came.get(cur);
			}
			path.reverse();
			return path;
		}
		open.delete(current);
		const cx = current % n;
		const cz = Math.floor(current / n);
		const neighbors = [
			[cx + 1, cz],
			[cx - 1, cz],
			[cx, cz + 1],
			[cx, cz - 1]
		];
		for (const [nx, nz] of neighbors) {
			if (!inBounds(n, nx, nz)) continue;
			const ni = idx(n, nx, nz);
			if (!walkableAt(ni)) continue;
			const tentative = (gScore.get(current) ?? Infinity) + 1;
			if (tentative < (gScore.get(ni) ?? Infinity)) {
				came.set(ni, current);
				gScore.set(ni, tentative);
				fScore.set(ni, tentative + h(ni));
				open.add(ni);
			}
		}
	}
	// No route — return empty so the citizen STAYS PUT. Never walk off-graph
	// (the old straight-line fallback is what sent citizens through walls).
	return [];
}
