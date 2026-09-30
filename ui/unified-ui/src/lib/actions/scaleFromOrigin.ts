// `use:scaleFromOrigin` — Svelte action that sets the modal's
// `transform-origin` to the most-recent click position, so a popIn
// transition reads as "growing out of the button you tapped" instead
// of teleporting in centred.
//
// Usage in a modal:
//   <div use:scaleFromOrigin transition:popIn> ... </div>
//
// The action listens once globally (lazily) for clicks and stamps the
// last point onto a module variable. When a new modal mounts and binds
// the action, it reads that point, converts to its own bounding box,
// and writes a `transform-origin` style.
//
// Falls back to centre origin if no click has been recorded yet (e.g.
// modal opened by keyboard).

import { browser } from '$app/environment';

let lastPointer: { x: number; y: number } | null = null;

if (browser) {
	const cap = (e: PointerEvent) => {
		lastPointer = { x: e.clientX, y: e.clientY };
	};
	window.addEventListener('pointerdown', cap, true);
}

export function scaleFromOrigin(node: HTMLElement) {
	const apply = () => {
		if (!lastPointer) {
			node.style.transformOrigin = 'center center';
			return;
		}
		const rect = node.getBoundingClientRect();
		const ox = ((lastPointer.x - rect.left) / rect.width) * 100;
		const oy = ((lastPointer.y - rect.top) / rect.height) * 100;
		// Clamp so a click far from the modal still produces a reasonable
		// origin (don't pull origin off-element entirely).
		const clamp = (v: number) => Math.max(-30, Math.min(130, v));
		node.style.transformOrigin = `${clamp(ox)}% ${clamp(oy)}%`;
	};
	apply();
	return {
		update: apply,
		destroy: () => {}
	};
}
