/**
 * Motion primitives for dashboard rendering.
 *
 * - `inView` Svelte action: adds `data-in-view="true"` to an element once
 *   it crosses the viewport threshold. Components style on the attribute
 *   to fade/slide in. Honors `prefers-reduced-motion`.
 *
 * - `staggerChildren` Svelte action: walks direct children of the target
 *   and sets per-child `animation-delay` based on `theme.motion.load_stagger_ms`.
 *   Use when a list is rendered after the chrome's own stagger window.
 *
 * Theme motion tokens live in CSS variables (`--theme-motion-load-*`); the
 * actions use them via `getComputedStyle`. Components don't need to import
 * the theme directly.
 */

const REDUCED_MOTION = (): boolean =>
	typeof window !== 'undefined' &&
	window.matchMedia?.('(prefers-reduced-motion: reduce)').matches === true;

export function inView(node: HTMLElement, params: { threshold?: number; once?: boolean } = {}) {
	const threshold = params.threshold ?? 0.15;
	const once = params.once ?? true;

	if (REDUCED_MOTION()) {
		// Skip the animation entirely — go straight to the visible state.
		node.dataset.inView = 'true';
		return {};
	}

	const observer = new IntersectionObserver(
		(entries) => {
			for (const entry of entries) {
				if (entry.isIntersecting) {
					node.dataset.inView = 'true';
					if (once) observer.unobserve(entry.target);
				} else if (!once) {
					node.dataset.inView = 'false';
				}
			}
		},
		{ threshold, rootMargin: '0px 0px -10% 0px' }
	);
	observer.observe(node);

	return {
		destroy() {
			observer.disconnect();
		}
	};
}

export function staggerChildren(node: HTMLElement, params: { stepMs?: number } = {}) {
	const styles = getComputedStyle(document.documentElement);
	const themeStaggerRaw = styles.getPropertyValue('--theme-motion-load-stagger').trim();
	const fallback = params.stepMs ?? 60;
	const stepMs =
		(themeStaggerRaw && parseInt(themeStaggerRaw.replace(/[^\d]/g, ''), 10)) || fallback;

	const apply = (): void => {
		Array.from(node.children).forEach((child, idx) => {
			(child as HTMLElement).style.animationDelay = `${idx * stepMs}ms`;
		});
	};
	apply();

	const observer = new MutationObserver(apply);
	observer.observe(node, { childList: true });

	return {
		destroy() {
			observer.disconnect();
		}
	};
}
