/** Operator opt-in, read through the existing authenticated runtime settings. */
import { isMarketingPath } from '$lib/shell/mobileAccess';
import { isPublicRoute } from '$lib/shared/publicRoutes';
type ClarityQueue = ((...args: unknown[]) => void) & { q?: unknown[][] };
type ClarityWindow = Window & { clarity?: ClarityQueue; requestIdleCallback?: (callback: () => void, options: { timeout: number }) => number };

let pending: Promise<void> | null = null;
let resolving = false;

export function isRuntimeClarityPending(): boolean { return resolving; }

export function requiresAnalyticsNavigationReload(from: string, to: string, recording: boolean, waiting: boolean): boolean {
	const category = (pathname: string) => {
		const path = pathname.replace(/\/+$/, '') || '/';
		return isMarketingPath(path) ? 'website' : isPublicRoute(path) ? 'public' : 'app';
	};
	return (recording || waiting) && category(from) !== category(to);
}

export function isLoopbackAnalyticsHost(host: string): boolean {
	return host === 'localhost' || host.endsWith('.localhost') || /^127\./.test(host) || host === '[::1]' || host === '::1';
}

export function startRuntimeClarity(): Promise<void> {
	if (typeof window === 'undefined' || isLoopbackAnalyticsHost(window.location.hostname)) return Promise.resolve();
	if (pending) return pending;
	resolving = true;
	const target = window as ClarityWindow;
	pending = new Promise<void>((resolve) => {
		const inject = async () => {
			try {
				// A separately configured website build may already have its own tag.
				if (target.clarity) return;
				const response = await fetch('/api/magician/v2/runtime/client-config', { cache: 'no-store', signal: AbortSignal.timeout(5000) });
				if (!response.ok) return;
				const config = await response.json();
				const id = config?.clarity_project_id;
				if (typeof id !== 'string' || !/^[a-z0-9]+$/i.test(id)) return;
				const queue: ClarityQueue = (...args) => { (queue.q ??= []).push(args); };
				target.clarity = queue;
				const script = document.createElement('script');
				script.async = true;
				script.src = `https://www.clarity.ms/tag/${id}`;
				document.head.appendChild(script);
			} catch {
				// Optional recording must never gate app startup or produce auth errors.
			} finally {
				resolving = false;
				resolve();
			}
		};
		const schedule = () => {
			if (target.requestIdleCallback) target.requestIdleCallback(() => { void inject(); }, { timeout: 2500 });
			else target.setTimeout(() => { void inject(); }, 1500);
		};
		if (document.readyState === 'complete') schedule();
		else target.addEventListener('load', schedule, { once: true });
	});
	return pending;
}
