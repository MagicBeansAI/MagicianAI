/** Keep viewport overlays outside route layouts and their clipping/stacking contexts. */
export function rootPortal(node: HTMLElement) {
	document.body.appendChild(node);
	return {
		destroy() {
			node.remove();
		}
	};
}
