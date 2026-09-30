import type { GameSurfaceFocus } from './types';

const FOCUSABLE_SELECTOR = [
	'[data-game-initial-focus]',
	'button:not([disabled])',
	'[href]',
	'input:not([disabled])',
	'select:not([disabled])',
	'textarea:not([disabled])',
	'[tabindex]:not([tabindex="-1"])'
].join(',');

export const GAME_SURFACE_PRIORITY = {
	commandBar: 10,
	inspector: 20,
	workspace: 30
} as const;

export function captureActiveElement(): HTMLElement | null {
	if (typeof document === 'undefined') return null;
	return document.activeElement instanceof HTMLElement ? document.activeElement : null;
}

export function focusGameSurface(surface: HTMLElement | null, mode: GameSurfaceFocus): void {
	if (!surface || mode === 'none') return;
	if (mode === 'first') {
		const target = Array.from(surface.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).find(
			(element) => !element.hidden && element.getAttribute('aria-hidden') !== 'true'
		);
		if (target) {
			target.focus({ preventScroll: true });
			return;
		}
	}
	surface.focus({ preventScroll: true });
}

export function restoreGameFocus(target: HTMLElement | null): void {
	if (!target?.isConnected) return;
	try {
		target.focus({ preventScroll: true });
	} catch {
		target.focus();
	}
}

export function isTopmostGameSurface(surface: HTMLElement | null): boolean {
	if (!surface || typeof document === 'undefined') return false;
	const candidates = Array.from(
		document.querySelectorAll<HTMLElement>('[data-game-dismiss-priority]')
	).filter((candidate) => !candidate.hidden && candidate.getAttribute('aria-hidden') !== 'true');
	let topmost: HTMLElement | null = null;
	let topPriority = Number.NEGATIVE_INFINITY;
	for (const candidate of candidates) {
		const priority = Number(candidate.dataset.gameDismissPriority ?? 0);
		if (priority >= topPriority) {
			topPriority = priority;
			topmost = candidate;
		}
	}
	return topmost === surface;
}
