<script lang="ts" context="module">
	import type { FleetStateSnapshot } from '../fleetState';

	/**
	 * Last fleet-state snapshot, held across mounts.
	 *
	 * Without it, toggling Campus -> Floor remounts with `fleetState = null`,
	 * and for the few hundred milliseconds before the first fetch lands the
	 * crew is mapped by derive.ts's focus-area fallback instead of by the
	 * snapshot's program refs. Those two disagree — the fallback picks an
	 * agent's HIGH-PRIORITY focus area, the snapshot its first program ref — so
	 * the floor visibly rebuilds itself the moment the fetch returns: on the
	 * live roster, seven fully-staffed rooms flick to eleven with four vacant.
	 *
	 * Module scope, not a store: this is a render-continuity cache, and nothing
	 * outside this component should be reading it or reacting to it.
	 */
	let lastFleetState: FleetStateSnapshot | null = null;
</script>

<script lang="ts">
	/**
	 * Office Floor — the 2D default view of /square.
	 *
	 * The whole crew on one screen, no camera. The campus (FleetWorld) is a
	 * navigable 3D world, which is the right container for exploring and the
	 * wrong one for "how is my crew doing" — you have to fly somewhere to read
	 * your own team. This renders the same roster as a fixed office floor plan:
	 * a room per program, a desk per person, and vacancy where nobody is
	 * working a program.
	 *
	 * TWO RENDER LAYERS, AND THE SPLIT IS DELIBERATE.
	 *
	 *   - GRAPHICS are inline SVG, scaled to fit the viewport. Vector, so the
	 *     floor is sharp at any size and at any device ratio.
	 *   - TYPE is HTML, positioned in CSS pixels over the SVG and NOT scaled
	 *     with it. Departure Mono is a bitmap design drawn on an 11px grid and
	 *     goes soft the moment it lands off that grid (see the note in
	 *     game-chrome.css). Text inside a scaled viewBox would be at
	 *     11 * whatever-the-fit-happens-to-be pixels — i.e. never on the grid.
	 *     Keeping labels in unscaled HTML is what buys crisp type, and it has a
	 *     second benefit: names stay legible when a forty-person floor scales
	 *     the graphics down.
	 *
	 * The layout itself is in floorPlan.ts — pure, deterministic, tested. This
	 * file only paints it and handles input.
	 */
	import { afterUpdate, createEventDispatcher, onDestroy, onMount } from 'svelte';
	import type { AgentSummary } from '$lib/stores/agentStore';
	import { loadAgents } from '$lib/stores/agentStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { hitlOpenTargetFromFeedItem, openHitlPrompt } from '$lib/attention';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import type { CitizenVM } from '../engine/types';
	import { plaqueRole, VIBE_LABEL } from '../derive';
	import { groupNeedsByAgent } from '../attentionGlue';
	import { fetchFleetState } from '../fleetState';
	import { buildFloorPlan, type Room, type Seat } from './floorPlan';
	import { buildOfficeCrew } from './officeCrew';
	import { resolveOfficeTone, type OfficeTone } from './officeTone';
	import {
		createActors,
		deskPoint,
		hopDuration,
		meetingPoints,
		socialVenuePoint,
		taskRouterPoint,
		type Point
	} from './actors';
	import { AMBIENT, lingerDuration, pickAmbientTrip, shouldTickAmbient } from './ambient';
	import { createEventQueue } from './eventQueue';
	import { diffOfficeEvents, type OfficeEvent } from './officeEvents';
	import { followPoint, spawnToken, startRoute, type FloorToken } from './officeMotion';
	import Desk from './sprites/Desk.svelte';
	import Door from './sprites/Door.svelte';
	import OfficeProp from './sprites/OfficeProp.svelte';
	import Person from './sprites/Person.svelte';
	import TaskToken from './sprites/TaskToken.svelte';
	import { personLookOf, STATUS_COLOUR, type PersonLook } from './sprites/palette';
	import './office-theme.css';
	import '../fleet-theme.css';
	import '../game-chrome.css';

	/** Same props the campus takes, so the hero can swap one for the other. */
	export let agents: AgentSummary[] = [];
	export let paused = false;

	const dispatch = createEventDispatcher<{ select: { citizenId: string | null } }>();

	/* --- live data ---------------------------------------------------------- */

	let fleetState = lastFleetState;
	let prevFleet = lastFleetState;
	let backendDown = false;
	let nowTick = Date.now();
	let timers: Array<ReturnType<typeof setInterval>> = [];
	let motionTimers: Array<ReturnType<typeof setTimeout>> = [];
	let unsubStatus: (() => void) | null = null;
	let unsubEvents: (() => void) | null = null;
	let fleetKick: ReturnType<typeof setTimeout> | null = null;
	let unsubMotion: (() => void) | null = null;
	let reducedMotion = false;

	const actors = createActors();
	let actorRev = 0;
	const routeGen = new Map<string, number>();
	let tokens: FloorToken[] = [];
	let talkBubbles: Array<{ citizenId: string; text: string }> = [];

	const queue = createEventQueue({
		intervalMs: 1_600,
		cap: 12,
		onRelease: (event) => playEvent(event)
	});

	async function refreshFleetState(): Promise<void> {
		if (paused) return;
		const next = await fetchFleetState();
		if (!next) return;
		const events = diffOfficeEvents(prevFleet, next);
		prevFleet = next;
		fleetState = next;
		lastFleetState = next;
		if (events.length > 0) queue.enqueue(events);
	}

	function onRealtimeEvents(events: unknown[]): void {
		const relevant = events.some((e) => {
			const rec = e as { event_type?: unknown; type?: unknown };
			return /task|execution|delegat/i.test(String(rec?.event_type ?? rec?.type ?? ''));
		});
		if (!relevant || fleetKick) return;
		fleetKick = setTimeout(() => {
			fleetKick = null;
			void refreshFleetState();
		}, 1_500);
	}

	$: needsByAgent = groupNeedsByAgent($attentionStore);
	$: crew = buildOfficeCrew(agents, fleetState, needsByAgent, { backendDown, now: nowTick });
	$: plan = buildFloorPlan(crew.citizens, crew.guilds, planAspect);
	$: citizenById = new Map(crew.citizens.map((c) => [c.id, c]));
	$: looks = new Map<string, PersonLook>(crew.citizens.map((c) => [c.id, personLookOf(c)]));
	let actorView = actors.all();
	$: rosterKey = plan.seats
		.map((seat) => `${seat.citizenId ?? ''}:${seat.personX}:${seat.personY}`)
		.join('|');
	$: if (rosterKey) {
		actors.reconcile(
			plan.seats
				.filter((seat): seat is Seat & { citizenId: string } => Boolean(seat.citizenId))
				.map((seat) => ({ id: seat.citizenId, desk: { x: seat.personX, y: seat.personY } }))
		);
		actorView = actors.all();
	}

	function later(fn: () => void, ms: number): void {
		const id = setTimeout(() => {
			motionTimers = motionTimers.filter((timer) => timer !== id);
			fn();
		}, ms);
		motionTimers.push(id);
	}

	function bumpActors(): void {
		actorRev += 1;
		actorView = actors.all();
	}

	function scheduleHop(fn: () => void, ms: number): void {
		bumpActors();
		later(() => {
			fn();
			bumpActors();
		}, ms);
	}

	function beginRoute(
		id: string,
		dest: Point,
		opts: { busy?: boolean; onArrive?: () => void } = {}
	): void {
		const gen = (routeGen.get(id) ?? 0) + 1;
		routeGen.set(id, gen);
		startRoute(actors, plan, id, dest, (fn, ms) => {
			scheduleHop(() => {
				if (routeGen.get(id) !== gen) return;
				fn();
			}, ms);
		}, { busy: opts.busy ?? true, reducedMotion, onArrive: opts.onArrive });
	}

	function sendHome(id: string, lingerMs: number, busy: boolean): void {
		const gen = routeGen.get(id);
		later(() => {
			if (routeGen.get(id) !== gen) return;
			const desk = actors.get(id)?.desk;
			if (!desk) return;
			beginRoute(id, desk, {
				busy,
				onArrive: () => {
					actors.settle(id);
					bumpActors();
				}
			});
		}, lingerMs);
	}

	function playEvent(event: OfficeEvent): void {
		switch (event.type) {
			case 'task-routed': {
				const dest = deskPoint(plan, event.citizenId);
				if (!dest) break;
				const from = taskRouterPoint(plan);
				const ms = reducedMotion ? 0 : Math.round(hopDuration(from, dest) * 1.15);
				const id = `task:${event.questId}`;
				tokens = spawnToken(tokens, {
					id,
					kind: 'task',
					title: event.title,
					from,
					to: dest,
					durationMs: ms,
					now: Date.now()
				});
				later(() => {
					tokens = tokens.filter((token) => token.id !== id);
				}, ms + 500);
				break;
			}
			case 'handoff': {
				const [left, right] = meetingPoints(plan);
				const linger = reducedMotion ? 0 : 1_800;
				beginRoute(event.fromId, left, {
					onArrive: () => sendHome(event.fromId, linger, true)
				});
				beginRoute(event.toId, right, {
					onArrive: () => sendHome(event.toId, linger, true)
				});
				break;
			}
			case 'social-talk': {
				const authorId = event.participantIds[0];
				if (!authorId) break;
				const author = actors.get(authorId);
				// Do not pre-empt a blocked signal or an in-flight handoff.
				if (!author || author.busy || author.state === 'signalling') break;
				const dest = socialVenuePoint(plan, event.venue);
				const linger = reducedMotion ? 1_800 : 3_400;
				const walkers = event.participantIds.filter((id) => {
					const actor = actors.get(id);
					if (!actor || actor.state === 'signalling') return false;
					if (id !== authorId && actor.busy) return false;
					return true;
				});
				if (!walkers.includes(authorId)) break;
				const spots = gatherPoints(dest, walkers.length);
				walkers.forEach((id, index) => {
					beginRoute(id, spots[index] ?? dest, {
						onArrive: () => {
							if (id === authorId) showTalkBubble(id, event.text, linger);
							sendHome(id, linger, true);
						}
					});
				});
				break;
			}
			case 'delivery-landed': {
				if (!event.citizenId) break;
				const dest = deskPoint(plan, event.citizenId);
				if (!dest) break;
				const from = { x: dest.x, y: dest.y - 26 };
				const ms = reducedMotion ? 0 : 720;
				const id = `parcel:${event.id}`;
				tokens = spawnToken(tokens, {
					id,
					kind: 'parcel',
					title: event.title,
					from,
					to: dest,
					durationMs: ms,
					now: Date.now()
				});
				later(() => {
					tokens = tokens.filter((token) => token.id !== id);
				}, ms + 900);
				break;
			}
			case 'blocked':
				actors.signal(event.citizenId);
				bumpActors();
				break;
			case 'unblocked':
				actors.settle(event.citizenId);
				bumpActors();
				break;
			case 'work-started': {
				const actor = actors.get(event.citizenId);
				// A proved start of work sits them down. Do not pre-empt a
				// handoff or a blocked signal — those are higher priority.
				if (!actor || actor.busy) break;
				if (actor.state !== 'at-desk') sendHome(event.citizenId, 0, true);
				break;
			}
			default:
				break;
		}
	}

	function tickAmbient(): void {
		if (paused || reducedMotion) return;
		if (!shouldTickAmbient(document.visibilityState)) return;
		const rng = Math.random;
		for (const citizen of crew.citizens) {
			const actor = actors.get(citizen.id);
			if (!actor) continue;
			const dest = pickAmbientTrip({ citizen, actor, plan, rng });
			if (!dest) continue;
			beginRoute(citizen.id, dest, {
				busy: false,
				onArrive: () => sendHome(citizen.id, lingerDuration(rng), false)
			});
			break;
		}
	}

	function playPreview(): void {
		const ids = crew.citizens.map((citizen) => citizen.id);
		if (ids.length === 0) return;
		const a = ids[0];
		const b = ids[1] ?? ids[0];
		queue.enqueue([
			{ type: 'task-routed', citizenId: a, questId: 'preview-route', title: 'Preview task' },
			{ type: 'handoff', id: 'preview-handoff', fromId: a, toId: b, questId: 'preview-route' },
			{
				type: 'delivery-landed',
				id: 'preview-parcel',
				citizenId: b,
				createdAt: Date.now(),
				title: 'Preview drop'
			},
			{ type: 'blocked', citizenId: a, attentionId: 'preview-block' }
		]);
		// After the higher-priority work spectacles, so a blocked/handoff
		// busy flag does not swallow the talk.
		later(() => {
			actors.settle(a);
			if (b !== a) actors.settle(b);
			bumpActors();
			queue.enqueue([
				{
					type: 'social-talk',
					id: 'preview-talk',
					venue: 'cooler',
					participantIds: a === b ? [a] : [b, a],
					text: 'Anyone heading to the cooler?'
				}
			]);
		}, 10_000);
	}

	function gatherPoints(center: Point, n: number): Point[] {
		if (n <= 1) return [{ ...center }];
		const radius = 16;
		return Array.from({ length: n }, (_, index) => {
			const angle = (Math.PI * 2 * index) / n - Math.PI / 2;
			return {
				x: center.x + Math.cos(angle) * radius,
				y: center.y + Math.sin(angle) * radius
			};
		});
	}

	function showTalkBubble(citizenId: string, text: string, lingerMs: number): void {
		talkBubbles = [
			...talkBubbles.filter((bubble) => bubble.citizenId !== citizenId),
			{ citizenId, text }
		];
		later(() => {
			talkBubbles = talkBubbles.filter((bubble) => bubble.citizenId !== citizenId);
		}, lingerMs);
	}

	async function resolveBlocked(citizenId: string): Promise<void> {
		const items = needsByAgent.get(citizenId);
		if (!items?.length) {
			selectId(selectedId === citizenId ? null : citizenId);
			return;
		}
		const entry = items[0];
		const target = hitlOpenTargetFromFeedItem(entry.item);
		if (!target) {
			selectId(citizenId);
			return;
		}
		const result = await openHitlPrompt(target);
		if (result.status === 'resolved') {
			actors.settle(citizenId);
			bumpActors();
		}
	}

	function onCitizenActivate(citizen: CitizenVM): void {
		const actor = actors.get(citizen.id);
		if (actor?.state === 'signalling' || citizen.vibe === 'needs') {
			void resolveBlocked(citizen.id);
			return;
		}
		selectId(selectedId === citizen.id ? null : citizen.id);
	}

	/* --- theming ------------------------------------------------------------- */

	let rootEl: HTMLElement | undefined;
	let tone: OfficeTone = 'light';
	let toneTimer: ReturnType<typeof setInterval> | null = null;
	function syncTone(): void {
		const next = resolveOfficeTone(rootEl);
		if (next !== tone) tone = next;
	}

	/* --- fit ------------------------------------------------------------------ */

	/**
	 * FIT IS A LAYOUT DECISION, NOT A SCALE FACTOR.
	 *
	 * `min(paneW/W, paneH/H)` is a `contain` fit: whenever the plan and the pane
	 * disagree on shape, the tighter axis wins and the other one becomes dead
	 * margin. No choice of scale recovers it, because the wrong thing is the
	 * plan's aspect. So the pane's aspect is measured HERE and handed to
	 * buildFloorPlan, which lays the floor out to that shape — band depths, room
	 * widths and corridor size all solved against it. The scale below then has
	 * nothing left to letterbox, and the crew come out as large as the roster
	 * allows.
	 *
	 * The aspect is QUANTISED to two decimals. buildFloorPlan is pure in
	 * (roster, aspect), so an aspect that changed by a thousandth on every
	 * animation frame of a window drag would re-solve — and visibly re-seat —
	 * the whole floor. Two decimals is finer than a person can see and coarse
	 * enough that ordinary resizes settle.
	 */

	/** Breathing room between the plan and the viewport edge, in CSS pixels. */
	const VIEWPORT_INSET = 8;
	/** Below this rendered desk width the name plaques stop fitting and are
	 * dropped rather than overlapped — the room labels carry on alone. */
	const NAME_PLATE_MIN_PX = 54;
	/** Used until the pane has been measured; replaced on the first tick. */
	const FALLBACK_ASPECT = 16 / 9;

	let viewportW = 0;
	let viewportH = 0;
	$: fitW = Math.max(1, viewportW - VIEWPORT_INSET * 2);
	$: fitH = Math.max(1, viewportH - VIEWPORT_INSET * 2);
	$: planAspect =
		viewportW > 0 && viewportH > 0 ? Math.round((fitW / fitH) * 100) / 100 : FALLBACK_ASPECT;
	$: scale =
		viewportW > 0 && viewportH > 0
			? Math.max(0, Math.min(fitW / plan.width, fitH / plan.height))
			: 0;
	$: renderW = plan.width * scale;
	$: renderH = plan.height * scale;
	$: offsetX = (viewportW - renderW) / 2;
	$: offsetY = (viewportH - renderH) / 2;
	$: showNamePlates = plan.deskCell.w * scale >= NAME_PLATE_MIN_PX;
	const px = (v: number, offset: number, s: number): number => offset + v * s;
	/** Seat bounding box, already in PLAN units — it spans the person AND their
	 * desk, and the solver owns it because the person's size is what sets it. */
	$: seatBox = plan.seatBox;

	/* --- selection ------------------------------------------------------------ */

	let selectedId: string | null = null;
	let hoveredId: string | null = null;
	$: if (selectedId && !citizenById.has(selectedId)) selectId(null);

	function selectId(id: string | null): void {
		selectedId = id;
		dispatch('select', { citizenId: id });
	}
	function toggleSeat(seat: Seat): void {
		if (!seat.citizenId) return;
		const citizen = citizenById.get(seat.citizenId);
		if (citizen) onCitizenActivate(citizen);
		else selectId(selectedId === seat.citizenId ? null : seat.citizenId);
	}
	function onSeatKey(event: KeyboardEvent, seat: Seat): void {
		if (event.key !== 'Enter' && event.key !== ' ') return;
		event.preventDefault();
		toggleSeat(seat);
	}

	/* --- labels ---------------------------------------------------------------- */

	function firstName(citizen: CitizenVM): string {
		return citizen.name.split(/\s+/)[0] || citizen.name;
	}
	$: guildNameById = new Map(crew.guilds.map((g) => [g.id, g.name]));
	/** The plaque's role word, from the same rule the campus uses. */
	const roleChip = (citizen: CitizenVM): string =>
		plaqueRole(citizen, guildNameById.get(citizen.guildId));
	function roomCount(room: Room): string {
		if (room.kind !== 'program' || room.vacant) return '';
		return `${room.memberIds.length}`;
	}
	/** Nameplate accent per program, so a long band of rooms is scannable. The
	 * amenities share one neutral accent — they are not programs and should not
	 * look like one. */
	function roomAccent(room: Room): string {
		if (room.kind !== 'program') return 'var(--office-metal-dark)';
		if (room.vacant) return 'var(--office-ink-soft)';
		return `hsl(${room.accentHue} 46% 46%)`;
	}

	/* --- standing name plaques ---------------------------------------------- */

	/**
	 * A plaque per crew member, over their own head, tracking them.
	 *
	 * SAME PLAQUE AS THE CAMPUS. `.fw-plaque` lives in game-chrome.css and the
	 * world HUD hangs the identical object over a 3D citizen; a crew member is
	 * a crew member whichever view you are in, and a second plaque style here
	 * would be the same information in two dialects. It carries the name and a
	 * status-coloured role chip, stacked, and it stays UNSCALED HTML over the
	 * scaled SVG for the reason at the top of this file.
	 *
	 * AND THEY COMPETE FOR ROOM, same as the campus. The crew are drawn small
	 * — small enough to read as tokens on a plan — so a plaque is now wider
	 * than the desk cell it belongs to, and five desks in Commons sit closer
	 * together than five plaques fit. Overlapping them into an unreadable pile
	 * is the one outcome worth avoiding, so they are placed in priority order
	 * and any plaque that would land on an already-placed one stands down.
	 * Nothing actionable hides behind this: a needs-you seat outranks everyone
	 * except your current selection.
	 */
	const PLAQUE_GAP_PX = 2;
	let plaqueEls = new Map<string, HTMLElement>();
	let culledPlaques = new Set<string>();

	function plaque(node: HTMLElement, id: string): { destroy: () => void } {
		plaqueEls.set(id, node);
		return {
			destroy() {
				plaqueEls.delete(id);
			}
		};
	}

	/** Who keeps their plaque when two land on each other. Lower wins — ranked
	 * by how much the crew member is asking of you, exactly as the campus ranks
	 * them, so the surviving set is the same set on either view. */
	function plaquePriority(citizen: CitizenVM): number {
		if (selectedId === citizen.id) return 0;
		if (hoveredId === citizen.id) return 1;
		if (citizen.vibe === 'needs') return 2;
		if (citizen.vibe === 'working') return 3;
		if (citizen.isPrimary) return 4;
		if (citizen.vibe === 'paused') return 5;
		return 6;
	}

	/**
	 * Cull the plaques that would overlap a more important one.
	 *
	 * Measured rather than estimated: the plaque is type, its width is whatever
	 * the name and the role word come to, and Departure Mono loads
	 * asynchronously. A culled plaque keeps its box (`visibility: hidden`, not
	 * display) so its measured width never depends on whether it is showing,
	 * which is what stops this from oscillating when it runs again.
	 */
	function cullPlaques(): void {
		const boxes: Array<{ id: string; l: number; t: number; r: number; b: number; p: number }> = [];
		for (const seat of plan.seats) {
			const citizen = seat.citizenId ? citizenById.get(seat.citizenId) : null;
			const el = citizen ? plaqueEls.get(citizen.id) : null;
			if (!citizen || !el) continue;
			const actor = actorView.get(citizen.id);
			if (actor && actor.state === 'walking') continue;
			const w = el.offsetWidth;
			const h = el.offsetHeight;
			if (w === 0 || h === 0) continue;
			const cx = px(seat.x, offsetX, scale);
			const top = px(seat.y + seatBox.dy, offsetY, scale);
			boxes.push({
				id: citizen.id,
				l: cx - w / 2 - PLAQUE_GAP_PX,
				t: top - PLAQUE_GAP_PX,
				r: cx + w / 2 + PLAQUE_GAP_PX,
				b: top + h + PLAQUE_GAP_PX,
				p: plaquePriority(citizen)
			});
		}
		// Ties break on id, never on iteration order, so a poll that returns the
		// roster in a different sequence does not swap which plaque survives.
		boxes.sort((a, b) => a.p - b.p || a.id.localeCompare(b.id));
		const kept: typeof boxes = [];
		const culled = new Set<string>();
		for (const box of boxes) {
			const clashes = kept.some((k) => box.l < k.r && box.r > k.l && box.t < k.b && box.b > k.t);
			if (clashes) culled.add(box.id);
			else kept.push(box);
		}
		if (culled.size !== culledPlaques.size || [...culled].some((id) => !culledPlaques.has(id))) {
			culledPlaques = culled;
		}
	}

	afterUpdate(cullPlaques);

	/* --- lifecycle -------------------------------------------------------------- */

	onMount(() => {
		syncTone();
		// Departure Mono loads with font-display: swap, so every plaque is
		// measured at fallback widths on the first pass and at its real width
		// only once the face lands. Re-cull then, or the floor keeps a set of
		// survivors chosen against the wrong metrics.
		void document.fonts?.ready.then(cullPlaques);
		// A theme switch rewrites custom properties without any event we can bind
		// to from here, so the tone is re-measured on a slow poll. Cheap: one
		// getComputedStyle read, and only repaints when the answer changes.
		toneTimer = setInterval(syncTone, 2_000);
		attentionStore.start();
		void refreshFleetState();
		const motion = window.matchMedia('(prefers-reduced-motion: reduce)');
		const syncMotion = (): void => {
			reducedMotion = motion.matches;
		};
		syncMotion();
		motion.addEventListener('change', syncMotion);
		unsubMotion = () => motion.removeEventListener('change', syncMotion);
		timers = [
			setInterval(() => void refreshFleetState(), 30_000),
			setInterval(() => {
				if (!backendDown && !paused) void loadAgents({ replace: true, clearError: true });
			}, 30_000),
			setInterval(() => (nowTick = Date.now()), 60_000),
			setInterval(() => tickAmbient(), AMBIENT.tickEveryMs)
		];
		unsubStatus = v2Events.connectionStatus.subscribe((s) => {
			backendDown = s === 'disconnected';
		});
		unsubEvents = v2Events.subscribe(onRealtimeEvents);
		if (/[?&]office-motion-preview=1\b/.test(window.location.search)) {
			later(playPreview, 900);
		}
	});

	onDestroy(() => {
		for (const timer of timers) clearInterval(timer);
		for (const timer of motionTimers) clearTimeout(timer);
		if (toneTimer) clearInterval(toneTimer);
		if (fleetKick) clearTimeout(fleetKick);
		timers = [];
		motionTimers = [];
		queue.stop();
		unsubStatus?.();
		unsubEvents?.();
		unsubMotion?.();
		attentionStore.stop();
	});
</script>

<div class="office-floor" data-office-tone={tone} bind:this={rootEl}>
	<div
		class="office-floor__viewport"
		bind:clientWidth={viewportW}
		bind:clientHeight={viewportH}
	>
		{#if scale > 0}
			<svg
				class="office-floor__plan"
				width={renderW}
				height={renderH}
				viewBox="0 0 {plan.width} {plan.height}"
				style="left: {offsetX}px; top: {offsetY}px;"
				role="group"
				aria-label="Office floor plan with {plan.stats.crew} crew across {plan.stats.programs} programs"
			>
				<defs>
					<pattern id="office-vacant-hatch" width="14" height="14" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
						<rect width="14" height="14" fill="var(--office-vacant)" />
						<rect width="5" height="14" fill="var(--office-vacant-line)" />
					</pattern>
					<pattern id="office-corridor-tile" width="46" height="46" patternUnits="userSpaceOnUse">
						<rect width="46" height="46" fill="var(--office-corridor)" />
						<path d="M46,0 L46,46 M0,46 L46,46" stroke="var(--office-corridor-line)" stroke-width="1.5" fill="none" />
					</pattern>
				</defs>

				<!-- shell: the dark band around the whole plate is the outer wall -->
				<rect x="0" y="0" width={plan.width} height={plan.height} rx="4" fill="var(--office-shell)" />
				<!-- interior starts as solid partition; rooms and corridor are cut out of
				     it, so every gap the layout leaves shows up as a real wall -->
				<rect
					x={plan.wall}
					y={plan.wall}
					width={plan.width - plan.wall * 2}
					height={plan.height - plan.wall * 2}
					fill="var(--office-partition)"
				/>

				<!-- corridor, with a runner down the walking lane -->
				<rect
					x={plan.corridor.x}
					y={plan.corridor.y}
					width={plan.corridor.w}
					height={plan.corridor.h}
					fill="url(#office-corridor-tile)"
				/>
				<rect
					x={plan.corridor.x + 10}
					y={plan.corridor.y + plan.corridor.h / 2 - 19}
					width={plan.corridor.w - 20}
					height="38"
					rx="5"
					fill="var(--office-rug)"
					opacity="0.11"
				/>
				<rect
					x={plan.corridor.x + 10}
					y={plan.corridor.y + plan.corridor.h / 2 - 19}
					width={plan.corridor.w - 20}
					height="38"
					rx="5"
					fill="none"
					stroke="var(--office-rug)"
					stroke-width="1.2"
					opacity="0.26"
				/>

				<!-- rooms -->
				{#each plan.rooms as room (room.id)}
					<g class="office-floor__room" class:is-vacant={room.vacant}>
						<rect
							x={room.x}
							y={room.y}
							width={room.w}
							height={room.h}
							fill={room.vacant
								? 'url(#office-vacant-hatch)'
								: room.kind === 'program'
									? 'var(--office-room)'
									: 'var(--office-room-alt)'}
						/>
						<!-- nameplate band on the outer wall, accented per program -->
						<rect
							x={room.labelStrip.x}
							y={room.labelStrip.y}
							width={room.labelStrip.w}
							height={room.labelStrip.h}
							fill={roomAccent(room)}
							opacity={room.vacant ? 0.14 : 0.16}
						/>
						<rect
							x={room.labelStrip.x}
							y={room.band === 'north' ? room.labelStrip.y : room.labelStrip.y + room.labelStrip.h - 3}
							width={room.labelStrip.w}
							height="3"
							fill={roomAccent(room)}
						/>
						{#if room.kind === 'meeting'}
							<!-- the meeting room is the glass box every office has -->
							<rect
								x={room.x}
								y={room.y}
								width={room.w}
								height={room.h}
								fill="var(--office-glass)"
							/>
						{/if}
					</g>
				{/each}

				<!-- doors, cut through the partition onto the corridor -->
				{#each plan.rooms as room (`door-${room.id}`)}
					<g transform="translate({room.door.x}, {room.door.y})">
						<Door width={room.door.w} band={room.band} />
					</g>
				{/each}

				<!-- main entrance, through the left shell wall at corridor height -->
				<rect
					x={plan.entrance.x}
					y={plan.entrance.y}
					width={plan.wall}
					height={plan.entrance.h}
					fill="var(--office-corridor)"
				/>
				<rect
					x={plan.wall + 5}
					y={plan.corridor.y + plan.corridor.h / 2 - 30}
					width="46"
					height="60"
					rx="4"
					fill="var(--office-rug)"
					opacity="0.34"
				/>
				<rect
					x={plan.wall + 5}
					y={plan.corridor.y + plan.corridor.h / 2 - 30}
					width="46"
					height="60"
					rx="4"
					fill="none"
					stroke="var(--office-rug)"
					stroke-width="1.4"
					opacity="0.55"
				/>

				<!-- room furniture -->
				{#each plan.rooms as room (`props-${room.id}`)}
					{#each room.props as prop (prop.id)}
						<OfficeProp {prop} />
					{/each}
				{/each}

				<!-- corridor furniture -->
				{#each plan.props as prop (prop.id)}
					<OfficeProp {prop} />
				{/each}

				<!-- Task router: intake at the entrance. Tokens leave from here. -->
				<g
					class="office-floor__router"
					transform="translate({taskRouterPoint(plan).x}, {taskRouterPoint(plan).y})"
				>
					<rect x="-16" y="-11" width="32" height="18" rx="2" fill="var(--office-wood)" />
					<rect x="-14" y="-16" width="28" height="8" rx="1.5" fill="var(--office-paper)" />
					<rect x="-8" y="8" width="16" height="4" rx="1" fill="var(--office-metal-dark)" />
				</g>

				<!-- desks stay put. People are a separate layer so a walk can
				     leave the chair without dragging the furniture. -->
				{#each plan.seats as seat (seat.id)}
					{@const citizen = seat.citizenId ? citizenById.get(seat.citizenId) : null}
					{#if citizen}
						<g
							class="office-floor__seat"
							class:is-selected={selectedId === citizen.id}
							class:is-hovered={hoveredId === citizen.id}
							role="button"
							tabindex="0"
							aria-label="{citizen.name}, {VIBE_LABEL[citizen.vibe]}"
							on:click={() => toggleSeat(seat)}
							on:keydown={(event) => onSeatKey(event, seat)}
							on:mouseenter={() => (hoveredId = citizen.id)}
							on:mouseleave={() => (hoveredId = null)}
							on:focus={() => (hoveredId = citizen.id)}
							on:blur={() => (hoveredId = null)}
						>
							{#if selectedId === citizen.id || hoveredId === citizen.id}
								<ellipse
									cx={seat.x}
									cy={seat.y + plan.seatScale * 11}
									rx={seatBox.w * 0.46}
									ry={seatBox.w * 0.17}
									fill={selectedId === citizen.id
										? 'var(--office-ink)'
										: 'var(--office-ink-soft)'}
									opacity={selectedId === citizen.id ? 0.2 : 0.12}
								/>
							{/if}
							<g transform="translate({seat.x}, {seat.y}) scale({plan.seatScale})">
								<Desk
									monitorSide={seat.monitorSide}
									vibe={citizen.vibe}
									showStatusChip={!showNamePlates}
								/>
							</g>
							<rect
								x={seat.x + seatBox.dx}
								y={seat.y + seatBox.dy}
								width={seatBox.w}
								height={seatBox.h}
								fill="transparent"
							/>
						</g>
					{/if}
				{/each}

				<!-- routed tokens, under the walkers so a person can "catch" one -->
				{#each tokens as token (token.id)}
					<g
						class="office-floor__token"
						use:followPoint={{
							from: token.from,
							to: token.to,
							startedAt: token.startedAt,
							until: token.until,
							reducedMotion
						}}
					>
						<TaskToken kind={token.kind} title={token.title} />
					</g>
				{/each}

				<!-- the crew, keyed by citizen so a poll does not remount a walker -->
				{#each plan.seats as seat (`person-${seat.citizenId ?? seat.id}`)}
					{@const citizen = seat.citizenId ? citizenById.get(seat.citizenId) : null}
					{@const actor = citizen ? actorView.get(citizen.id) : undefined}
					{@const from = actor?.from ?? { x: seat.personX, y: seat.personY }}
					{@const to = actor?.to ?? { x: seat.personX, y: seat.personY }}
					{#if citizen}
						<g
							class="office-floor__person"
							class:is-walking={actor?.state === 'walking'}
							class:is-signalling={actor?.state === 'signalling'}
							role="button"
							tabindex="0"
							aria-label="{citizen.name}, {VIBE_LABEL[citizen.vibe]}"
							use:followPoint={{
								from,
								to,
								startedAt: actor?.startedAt ?? 0,
								until: actor?.until ?? 0,
								scale: plan.personScale,
								face: actor?.state === 'walking',
								reducedMotion
							}}
							on:click={() => onCitizenActivate(citizen)}
							on:keydown={(event) => onSeatKey(event, seat)}
							on:mouseenter={() => (hoveredId = citizen.id)}
							on:mouseleave={() => (hoveredId = null)}
							on:focus={() => (hoveredId = citizen.id)}
							on:blur={() => (hoveredId = null)}
						>
							<Person
								look={looks.get(citizen.id) ?? personLookOf(citizen)}
								vibe={citizen.vibe}
								resting={citizen.resting}
								pose={!actor || actor.state === 'at-desk' ? 'seated' : 'standing'}
								signalling={actor?.state === 'signalling'}
							/>
							<rect x="-20" y="-72" width="40" height="96" fill="transparent" />
						</g>
					{/if}
				{/each}
			</svg>

			<!-- unscaled type over the scaled plan -->
			<div class="office-floor__labels" aria-hidden="true">
				{#each plan.rooms as room (`label-${room.id}`)}
					<div
						class="office-floor__room-label"
						class:is-vacant={room.vacant}
						style="left: {px(room.labelStrip.x, offsetX, scale)}px;
						       top: {px(room.labelStrip.y, offsetY, scale)}px;
						       width: {room.labelStrip.w * scale}px;
						       height: {room.labelStrip.h * scale}px;
						       --label-pad: {roomCount(room) ? 21 : 5}px;"
					>
						<span class="office-floor__room-name">{room.name}</span>
						{#if roomCount(room)}
							<span class="office-floor__room-count">{roomCount(room)}</span>
						{/if}
					</div>
					{#if room.vacant}
						<!-- Vacancy is stamped across the empty floor rather than chipped
						     into the nameplate: a narrow unstaffed room has no width to
						     spare next to its own name, and the middle of the room is
						     exactly the space that is free. -->
						<div
							class="office-floor__vacant-stamp"
							style="left: {px(room.x, offsetX, scale)}px;
							       top: {px(room.content.y + room.content.h / 2 + 4, offsetY, scale)}px;
							       width: {room.w * scale}px;"
						>
							Vacant
						</div>
					{/if}
				{/each}

				{#if showNamePlates}
					{#each plan.seats as seat (`name-${seat.id}`)}
						{@const citizen = seat.citizenId ? citizenById.get(seat.citizenId) : null}
						{#if citizen}
							{@const actor = actorView.get(citizen.id)}
							{@const plaqueAt =
								actor && actor.state !== 'at-desk' && actor.state !== 'walking'
									? actor.to
									: { x: seat.x, y: seat.y + seatBox.dy }}
							<div
								class="fw-plaque fw-plaque--stacked office-floor__plaque"
								class:is-culled={culledPlaques.has(citizen.id) || actor?.state === 'walking'}
								class:is-selected={selectedId === citizen.id}
								use:plaque={citizen.id}
								style="left: {px(plaqueAt.x, offsetX, scale)}px;
								       top: {px(actor && actor.state !== 'at-desk' && actor.state !== 'walking' ? plaqueAt.y - 10 : plaqueAt.y, offsetY, scale)}px;"
							>
								<span class="fw-plaque__name">{firstName(citizen)}</span>
								<span class="fw-plaque__role" data-vibe={citizen.vibe}>{roleChip(citizen)}</span>
							</div>
						{/if}
					{/each}
				{/if}

				{#each talkBubbles as bubble (`talk-${bubble.citizenId}`)}
					{@const actor = actorView.get(bubble.citizenId)}
					{#if actor && actor.state !== 'walking'}
						<div
							class="office-floor__talk"
							style="left: {px(actor.to.x, offsetX, scale)}px;
							       top: {px(actor.to.y - 22, offsetY, scale)}px;"
						>
							{bubble.text}
						</div>
					{/if}
				{/each}
			</div>
		{/if}

		{#if agents.length === 0}
			<div class="office-floor__empty">Opening the office…</div>
		{/if}
	</div>
</div>

<style>
	.office-floor {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		background: var(--office-plot);
		overflow: hidden;
	}
	.office-floor__viewport {
		position: relative;
		flex: 1 1 auto;
		min-height: 0;
		background:
			repeating-linear-gradient(
				90deg,
				var(--office-plot) 0 42px,
				var(--office-plot-line) 42px 43px
			),
			var(--office-plot);
	}
	.office-floor__plan {
		position: absolute;
		display: block;
		/* shape-rendering stays at auto: these are diagonal arcs and rounded
		   corners, and crispEdges would alias every one of them. Sharpness comes
		   from the geometry being vector, not from snapping it. */
	}

	/* --- unscaled type layer --- */
	.office-floor__labels {
		position: absolute;
		inset: 0;
		pointer-events: none;
	}
	/* Program names are long ("Engineering Strategy") and rooms are narrow, so
	   the nameplate WRAPS rather than truncating — a name cut to "ENGINEERING
	   ST…" fails at the one job the plate has. The count sits absolutely at the
	   right so it never competes with the wrap for width, and the side padding
	   only reserves room for it when there IS one: an unstaffed program's room
	   is a third the width of a staffed one, and a fixed inset ate enough of it
	   to truncate the very name the room exists to show. */
	.office-floor__room-label {
		position: absolute;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 0 var(--label-pad, 1.35rem);
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: var(--game-display-track-sm, 1px);
		line-height: 1.08;
		text-align: center;
		text-transform: uppercase;
		color: var(--office-ink);
		overflow: hidden;
	}
	.office-floor__room-name {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		overflow-wrap: anywhere;
	}
	.office-floor__room-count {
		position: absolute;
		right: 0.25rem;
		top: 50%;
		transform: translateY(-50%);
		padding: 0 0.22rem;
		border: 1px solid var(--office-ink-soft);
		color: var(--office-ink);
		opacity: 0.8;
	}
	.office-floor__room-label.is-vacant {
		color: var(--office-ink);
		opacity: 0.62;
	}
	.office-floor__vacant-stamp {
		position: absolute;
		display: flex;
		align-items: center;
		justify-content: center;
		height: 24px;
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: 2px;
		text-transform: uppercase;
		color: var(--office-ink);
		opacity: 0.55;
	}

	/* The plaque itself (border, fill, chip colours) is .fw-plaque in
	   game-chrome.css, shared with the campus HUD. All this adds is where it
	   hangs: centred on the seat, its top on the seat block's top edge, which
	   the solver already reserved above the tallest head in the room. */
	.office-floor__plaque {
		position: absolute;
		transform: translateX(-50%);
	}
	/* Stood down because a more important plaque got there first. Hidden, not
	   removed: its box stays measurable, so the cull that hid it computes the
	   same answer next pass instead of oscillating. */
	.office-floor__plaque.is-culled {
		visibility: hidden;
	}
	/* Selection inverts, the same hard swap the rest of the game chrome uses. */
	.office-floor__plaque.is-selected {
		background: #1f2b33;
		color: rgba(252, 248, 238, 0.96);
	}

	.office-floor__talk {
		position: absolute;
		transform: translate(-50%, calc(-100% - 8px));
		max-width: 11.5rem;
		padding: 0.28rem 0.42rem 0.34rem;
		background: var(--office-paper);
		color: var(--office-ink);
		border: 1px solid var(--office-ink);
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: 0.02em;
		line-height: 1.2;
		overflow-wrap: anywhere;
		z-index: 3;
	}
	.office-floor__talk::after {
		content: '';
		position: absolute;
		left: 50%;
		bottom: -5px;
		width: 8px;
		height: 8px;
		background: var(--office-paper);
		border-right: 1px solid var(--office-ink);
		border-bottom: 1px solid var(--office-ink);
		transform: translateX(-50%) rotate(45deg);
	}

	/* --- crew interaction --- */
	.office-floor__seat {
		cursor: pointer;
		outline: none;
	}
	.office-floor__person,
	.office-floor__token {
		cursor: pointer;
		outline: none;
		transform-box: view-box;
		transform-origin: 0 0;
		will-change: transform;
	}
	.office-floor__token {
		pointer-events: none;
	}
	@media (prefers-reduced-motion: reduce) {
		.office-floor__person,
		.office-floor__token {
			transition: none !important;
		}
	}

	.office-floor__empty {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		color: var(--office-ink);
		opacity: 0.6;
		font-family: var(--font-primary, system-ui);
		font-size: 0.9rem;
		letter-spacing: 0.04em;
		pointer-events: none;
	}
</style>
