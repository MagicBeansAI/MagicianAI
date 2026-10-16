// Motion helpers for the chat-next renderers.
//
// Durations come from the liveliness tokens in app.css (`--dur-*`), so a
// theme or the reduced-motion media block changes every helper at once.
// Reduced-motion contract: when `prefers-reduced-motion: reduce` matches,
// `readDuration` returns 0, `rise` returns `{ duration: 0 }`, `revealWords`
// leaves the DOM untouched and `morph` runs the update without a view
// transition. General-purpose presets live in `$lib/motion`; these are
// specific to chat-next.

import type { TransitionConfig } from 'svelte/transition';

export type DurationToken = '--dur-quick' | '--dur-base' | '--dur-slow';

/** Token values from app.css, used when the computed style cannot be read. */
const DURATION_DEFAULTS: Record<DurationToken, number> = {
	'--dur-quick': 160,
	'--dur-base': 280,
	'--dur-slow': 520
};

function prefersReducedMotion(): boolean {
	return (
		typeof window !== 'undefined' &&
		typeof window.matchMedia === 'function' &&
		window.matchMedia('(prefers-reduced-motion: reduce)').matches
	);
}

/**
 * A `--dur-*` token in milliseconds. 0 under reduced motion; the app.css
 * default when the token is unreadable (SSR, jsdom, unparsable value).
 */
export function readDuration(token: DurationToken): number {
	if (typeof window === 'undefined' || typeof document === 'undefined') {
		return DURATION_DEFAULTS[token];
	}
	if (prefersReducedMotion()) return 0;
	const raw = window.getComputedStyle(document.documentElement).getPropertyValue(token).trim();
	const match = /^(\d*\.?\d+)(ms|s)$/.exec(raw);
	if (!match) return DURATION_DEFAULTS[token];
	return parseFloat(match[1]) * (match[2] === 's' ? 1000 : 1);
}

/**
 * Ease-out with a small overshoot (~3%), approximating `--spring-gentle`.
 * Svelte `css` transitions sample a JS easing into their own keyframes, so
 * the CSS `linear()` token cannot be passed through here.
 */
function gentleOut(t: number): number {
	const s = 0.9;
	const f = t - 1;
	return f * f * ((s + 1) * f + s) + 1;
}

/**
 * Svelte transition: rises `y` px, scales .98 → 1 and fades in over
 * `--dur-base`. Use as `in:rise={{ delay }}`.
 */
export function rise(
	_node: Element,
	{ delay = 0, y = 10 }: { delay?: number; y?: number } = {}
): TransitionConfig {
	const duration = readDuration('--dur-base');
	if (duration === 0) return { duration: 0 };
	return {
		delay,
		duration,
		easing: gentleOut,
		css: (t, u) =>
			`opacity: ${Math.min(1, t)}; transform: translateY(${(u * y).toFixed(2)}px) scale(${(0.98 + 0.02 * t).toFixed(4)});`
	};
}

const WORDS_PER_GROUP = 3;
/** Groups past this share the last delay, so long answers finish promptly. */
const MAX_STAGGERED_GROUPS = 40;

/**
 * Svelte action: fades a finished answer in word-group by word-group.
 * Wraps each run of three words in `<span class="cn-word">` with a
 * staggered `animation-delay` (`--dur-quick` / 4 per group); text inside
 * `pre` and `code` is left alone. The `.cn-word` keyframes live in app.css.
 *
 * NEVER use on streaming text: it rewrites text nodes once at mount, so
 * callers pass `enabled: false` for a streaming turn. `enabled` is read at
 * mount only; flipping it later does not re-run the reveal (text the user
 * already watched stream in must not animate again). Only use on content
 * Svelte will not patch afterwards (e.g. `{@html}` of a completed answer),
 * since the original text nodes are replaced.
 */
export function revealWords(node: HTMLElement, { enabled = true }: { enabled?: boolean } = {}): void {
	if (!enabled) return;
	const step = readDuration('--dur-quick') / 4;
	if (step === 0) return;

	const textNodes: Text[] = [];
	const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT, {
		acceptNode: (text) =>
			!text.nodeValue?.trim() || text.parentElement?.closest('pre, code')
				? NodeFilter.FILTER_REJECT
				: NodeFilter.FILTER_ACCEPT
	});
	while (walker.nextNode()) textNodes.push(walker.currentNode as Text);

	let group = 0;
	for (const text of textNodes) {
		const value = text.nodeValue ?? '';
		const fragment = document.createDocumentFragment();
		const leading = /^\s*/.exec(value)?.[0] ?? '';
		if (leading) fragment.append(leading);
		const words = value.slice(leading.length).match(/\S+\s*/g) ?? [];
		for (let i = 0; i < words.length; i += WORDS_PER_GROUP) {
			const span = document.createElement('span');
			span.className = 'cn-word';
			span.style.animationDelay = `${Math.min(group, MAX_STAGGERED_GROUPS - 1) * step}ms`;
			span.textContent = words.slice(i, i + WORDS_PER_GROUP).join('');
			fragment.append(span);
			group++;
		}
		text.replaceWith(fragment);
	}
}

type StartViewTransition = (update: () => Promise<void>) => {
	finished: Promise<void>;
	updateCallbackDone: Promise<void>;
};

let morphSeq = 0;

function setTransitionName(el: Element | null, name: string | null): void {
	if (!(el instanceof HTMLElement)) return;
	if (name) el.style.setProperty('view-transition-name', name);
	else el.style.removeProperty('view-transition-name');
}

/**
 * Shared-element morph: `source` (e.g. a live task card) visually becomes
 * the element `target()` returns after `update()` has run (e.g. the opened
 * panel). Uses the View Transitions API with a unique `cn-morph-<n>` name,
 * so it never collides with the app's fixed names such as `command-ask`.
 *
 *   await morph(cardEl, () => document.querySelector('[data-morph-target]'), openPanel);
 *
 * Without view transitions, under reduced motion, without a source, or while
 * another view transition is starting, it just awaits `update()`. Resolves
 * once the animation has finished; rejects if `update()` rejects.
 */
export async function morph(
	source: HTMLElement | null,
	target: () => Element | null,
	update: () => void | Promise<void>
): Promise<void> {
	const start = (document as Document & { startViewTransition?: StartViewTransition })
		.startViewTransition;
	if (!source || typeof start !== 'function' || prefersReducedMotion()) {
		await update();
		return;
	}

	const name = `cn-morph-${++morphSeq}`;
	let destination: Element | null = null;
	setTransitionName(source, name);
	let transition: ReturnType<StartViewTransition>;
	try {
		transition = start.call(document, async () => {
			setTransitionName(source, null);
			await update();
			destination = target();
			setTransitionName(destination, name);
		});
	} catch {
		// InvalidStateError: a transition is already starting. Skip the morph.
		setTransitionName(source, null);
		await update();
		return;
	}

	try {
		await transition.updateCallbackDone;
	} finally {
		await transition.finished.catch(() => {});
		setTransitionName(source, null);
		setTransitionName(destination, null);
	}
}
