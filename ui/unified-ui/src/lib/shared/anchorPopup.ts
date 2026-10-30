type Anchor = () => HTMLElement | null | undefined;
interface PopupAnchor {
	getAnchor: Anchor;
	placement?: 'top' | 'bottom';
	align?: 'start' | 'end';
}

/** Keep a viewport popup attached to its trigger as layout changes. */
export function anchorPopup(node: HTMLElement, anchor: Anchor | PopupAnchor): { destroy(): void } {
	const options = typeof anchor === 'function' ? { getAnchor: anchor } : anchor;
	const { getAnchor } = options;
	const position = (): void => {
		const anchor = getAnchor();
		if (!anchor) return;
		const rect = anchor.getBoundingClientRect();
		const width = node.getBoundingClientRect().width || 352;
		const edge = options.align === 'start' ? window.innerWidth - rect.left - width : window.innerWidth - rect.right;
		const right = Math.min(Math.max(8, edge), Math.max(8, window.innerWidth - width - 8));
		const below = window.innerHeight - rect.bottom - 14;
		const downward = options.placement === 'bottom' && (below >= 180 || below >= rect.top - 14);
		node.style.position = 'fixed';
		node.style.left = 'auto';
		node.style.transform = 'none';
		node.style.right = `${right}px`;
		node.style.top = downward ? `${rect.bottom + 6}px` : 'auto';
		node.style.bottom = downward ? 'auto' : `${window.innerHeight - rect.top + 6}px`;
		node.style.maxHeight = `${Math.max(0, Math.min(540, downward ? below : rect.top - 14))}px`;
	};
	position();
	const observer = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(position) : null;
	const trigger = getAnchor();
	if (trigger) {
		observer?.observe(trigger);
		const shell = trigger.closest('.composer-shell');
		if (shell) observer?.observe(shell);
	}
	window.addEventListener('resize', position);
	window.addEventListener('scroll', position, true);
	window.visualViewport?.addEventListener('resize', position);
	return { destroy() {
		observer?.disconnect();
		window.removeEventListener('resize', position);
		window.removeEventListener('scroll', position, true);
		window.visualViewport?.removeEventListener('resize', position);
	} };
}
