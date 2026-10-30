/**
 * Settings loads each panel's code on demand instead of importing all ~22
 * panels (and their media, plane and device modules) before anything can
 * paint. Over the phone apps' tunnel every module is a round trip, and
 * importing them all up front kept embedded Settings blank for seconds.
 *
 * The category being opened (the URL hash, else the first) loads first; the
 * rest wait until those arrive — or {@link RELEASE_FALLBACK_MS}, whichever
 * is sooner — so a slow link is not split twenty ways before the visible
 * cards are done.
 */
export const RELEASE_FALLBACK_MS = 1500;

export type PanelModule = { default: any };
export type PanelLoader = () => Promise<PanelModule>;

export interface PanelScheduler {
	readonly priorityCategory: string;
	/** Load `loader` for a panel in `category`, in priority order. */
	load(category: string, loader: PanelLoader): Promise<PanelModule>;
	/** Call once every panel has registered (the page's onMount). */
	start(): void;
	/** Settles when every registered panel has loaded or failed. */
	readonly allSettled: Promise<void>;
}

export function createPanelScheduler(
	priorityCategory: string,
	setTimer: (fn: () => void, ms: number) => unknown = (fn, ms) => setTimeout(fn, ms)
): PanelScheduler {
	const priority: Promise<unknown>[] = [];
	const everything: Promise<unknown>[] = [];
	let released = false;
	let releaseWaiters: (() => void)[] = [];
	let settle: () => void = () => {};
	const allSettled = new Promise<void>((resolve) => (settle = resolve));

	const release = () => {
		if (released) return;
		released = true;
		const waiting = releaseWaiters;
		releaseWaiters = [];
		for (const go of waiting) go();
	};

	return {
		priorityCategory,
		allSettled,
		load(category, loader) {
			const loaded =
				category === priorityCategory || released
					? loader()
					: new Promise<void>((go) => releaseWaiters.push(go)).then(loader);
			const outcome = loaded.catch(() => undefined);
			if (category === priorityCategory) priority.push(outcome);
			everything.push(outcome);
			return loaded;
		},
		start() {
			void Promise.all(priority).then(release);
			setTimer(release, RELEASE_FALLBACK_MS);
			// Every panel registered before start(); wait for those, then settle.
			void Promise.resolve().then(() => Promise.all(everything)).then(() => settle());
		}
	};
}
