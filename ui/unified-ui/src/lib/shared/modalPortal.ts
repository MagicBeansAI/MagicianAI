const ATTENTION_MODAL_HOST_ATTRIBUTE = 'data-attention-modal-portal-host';
const ATTENTION_MODAL_SCROLL_LOCK_CLASS = 'attention-modal-scroll-lock';

interface IsolationSnapshot {
	inertAttribute: string | null;
	ariaHidden: string | null;
}

let portalHost: HTMLElement | null = null;
let isolationCount = 0;
let bodyHadScrollLockClass = false;
let bodyObserver: MutationObserver | null = null;
const isolationSnapshots = new Map<HTMLElement, IsolationSnapshot>();

function ensurePortalHost(): HTMLElement {
	if (portalHost?.isConnected) return portalHost;

	const existing = Array.from(document.body.children).find(
		(element): element is HTMLElement =>
			element instanceof HTMLElement && element.hasAttribute(ATTENTION_MODAL_HOST_ATTRIBUTE)
	);
	if (existing) {
		portalHost = existing;
		return existing;
	}

	portalHost = document.createElement('div');
	portalHost.setAttribute(ATTENTION_MODAL_HOST_ATTRIBUTE, '');
	document.body.appendChild(portalHost);
	return portalHost;
}

function isolateBodyChild(element: HTMLElement, host: HTMLElement): void {
	if (element === host) return;
	if (!isolationSnapshots.has(element)) {
		isolationSnapshots.set(element, {
			inertAttribute: element.getAttribute('inert'),
			ariaHidden: element.getAttribute('aria-hidden')
		});
	}
	element.setAttribute('inert', '');
	element.setAttribute('aria-hidden', 'true');
}

function restoreAttribute(element: HTMLElement, name: string, value: string | null): void {
	if (value === null) element.removeAttribute(name);
	else element.setAttribute(name, value);
}

function acquireModalIsolation(host: HTMLElement): void {
	isolationCount += 1;
	if (isolationCount > 1) return;

	bodyHadScrollLockClass = document.body.classList.contains(ATTENTION_MODAL_SCROLL_LOCK_CLASS);
	document.body.classList.add(ATTENTION_MODAL_SCROLL_LOCK_CLASS);
	for (const child of Array.from(document.body.children)) {
		if (child instanceof HTMLElement) isolateBodyChild(child, host);
	}

	bodyObserver = new MutationObserver((mutations) => {
		for (const mutation of mutations) {
			for (const addedNode of Array.from(mutation.addedNodes)) {
				if (addedNode instanceof HTMLElement && addedNode.parentElement === document.body) {
					isolateBodyChild(addedNode, host);
				}
			}
		}
	});
	bodyObserver.observe(document.body, { childList: true });
}

function releaseModalIsolation(): void {
	if (isolationCount === 0) return;
	isolationCount -= 1;
	if (isolationCount > 0) return;

	bodyObserver?.disconnect();
	bodyObserver = null;
	for (const [element, snapshot] of isolationSnapshots) {
		restoreAttribute(element, 'inert', snapshot.inertAttribute);
		restoreAttribute(element, 'aria-hidden', snapshot.ariaHidden);
	}
	isolationSnapshots.clear();
	if (!bodyHadScrollLockClass) {
		document.body.classList.remove(ATTENTION_MODAL_SCROLL_LOCK_CLASS);
	}
	bodyHadScrollLockClass = false;
}

function removeHostWhenUnused(host: HTMLElement): void {
	queueMicrotask(() => {
		if (isolationCount === 0 && host.childNodes.length === 0 && portalHost === host) {
			host.remove();
			portalHost = null;
		}
	});
}

/**
 * Portals Attention child modals outside the Attention Center's inert subtree.
 * The action also isolates every other body root for the lifetime of the modal.
 */
export function attentionModalPortal(node: HTMLElement, enabled: boolean) {
	let active = false;
	let host: HTMLElement | null = null;
	let anchor: Comment | null = null;

	function enable(): void {
		if (active) return;
		anchor = document.createComment('attention-modal-portal');
		node.parentNode?.insertBefore(anchor, node);
		host = ensurePortalHost();
		host.appendChild(node);
		acquireModalIsolation(host);
		active = true;
	}

	function disable(): void {
		if (!active) return;
		const previousHost = host;
		if (anchor?.parentNode) anchor.parentNode.insertBefore(node, anchor);
		active = false;
		host = null;
		releaseModalIsolation();
		if (previousHost) removeHostWhenUnused(previousHost);
	}

	if (enabled) enable();

	return {
		update(nextEnabled: boolean) {
			if (nextEnabled) enable();
			else disable();
		},
		destroy() {
			const previousHost = host;
			if (active) {
				active = false;
				host = null;
				releaseModalIsolation();
			}
			anchor?.remove();
			anchor = null;
			if (previousHost) removeHostWhenUnused(previousHost);
		}
	};
}
