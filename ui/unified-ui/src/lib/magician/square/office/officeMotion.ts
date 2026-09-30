/**
 * Motion helpers for the office floor: a CSS-transform action that does not
 * restart mid-walk, and a small hop runner that walks a plan-derived route
 * one segment at a time.
 */

import {
	hopDuration,
	walkRoute,
	type Actors,
	type Point
} from './actors';
import type { FloorPlan } from './floorPlan';

export interface FloorToken {
	id: string;
	kind: 'task' | 'parcel';
	title: string;
	from: Point;
	to: Point;
	startedAt: number;
	until: number;
}

export function followPoint(
	node: Element & { style: CSSStyleDeclaration; dataset: DOMStringMap },
	params: {
		from: Point;
		to: Point;
		startedAt: number;
		until: number;
		scale?: number;
		face?: boolean;
		reducedMotion?: boolean;
	}
): { update: (next: typeof params) => void } {
	const apply = (next: typeof params): void => {
		const sig = `${next.startedAt}:${next.from.x},${next.from.y}->${next.to.x},${next.to.y}:${next.scale ?? 1}`;
		if (node.dataset.walkSig === sig) return;
		node.dataset.walkSig = sig;
		const scale = next.scale ?? 1;
		const flip = next.face && next.to.x + 0.01 < next.from.x ? -1 : 1;
		const transformOf = (point: Point): string =>
			`translate(${point.x}px, ${point.y}px) scale(${scale * flip}, ${scale})`;
		if (next.reducedMotion || next.until <= next.startedAt) {
			(node as HTMLElement).style.transitionDuration = '0ms';
			(node as HTMLElement).style.transform = transformOf(next.to);
			return;
		}
		(node as HTMLElement).style.transitionProperty = 'transform';
		(node as HTMLElement).style.transitionTimingFunction = 'linear';
		(node as HTMLElement).style.transitionDuration = '0ms';
		(node as HTMLElement).style.transform = transformOf(next.from);
		requestAnimationFrame(() => {
			if (node.dataset.walkSig !== sig) return;
			(node as HTMLElement).style.transitionDuration = `${Math.max(0, next.until - next.startedAt)}ms`;
			(node as HTMLElement).style.transform = transformOf(next.to);
		});
	};
	apply(params);
	return { update: apply };
}

export interface HopOptions {
	busy?: boolean;
	reducedMotion?: boolean;
	onArrive?: () => void;
}

/**
 * Walk `id` along the plan-derived route from their current position to `dest`.
 * One hop at a time; the caller owns the timeout list so destroy can cancel.
 */
export function startRoute(
	actors: Actors,
	plan: FloorPlan,
	id: string,
	dest: Point,
	schedule: (fn: () => void, ms: number) => void,
	opts: HopOptions = {}
): void {
	const from = actors.position(id);
	if (!from) return;
	const route = walkRoute(plan, from, dest);
	let i = 1;

	const step = (): void => {
		if (i >= route.length) {
			opts.onArrive?.();
			return;
		}
		const next = route[i++];
		const here = actors.position(id) ?? next;
		const ms = opts.reducedMotion ? 0 : hopDuration(here, next);
		actors.walkTo(id, next, ms, { busy: opts.busy ?? true });
		schedule(step, ms);
	};
	step();
}

export function spawnToken(
	tokens: FloorToken[],
	token: Omit<FloorToken, 'startedAt' | 'until'> & { durationMs: number; now: number }
): FloorToken[] {
	const startedAt = token.now;
	const next: FloorToken = {
		id: token.id,
		kind: token.kind,
		title: token.title,
		from: token.from,
		to: token.to,
		startedAt,
		until: startedAt + token.durationMs
	};
	return [...tokens.filter((t) => t.id !== token.id), next];
}
