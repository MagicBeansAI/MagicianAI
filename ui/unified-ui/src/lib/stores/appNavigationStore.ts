// First-party navigation contributed by installed apps (gate S3).
//
// ONE shared poller for the Apps directory (see sharedPoll.ts). The TopBar
// keeps it alive from every page at the idle cadence; `/observe` and the
// command palette read the same readable instead of fetching again. A
// navigation entry is chrome, not data, so the idle cadence is minutes — an
// app installed in another tab appears at the next tick, and `pollAppNavigationNow()`
// exists for the surfaces that just changed an installation themselves.
//
// The snapshot is bound to the scope credential it was read under. A workspace
// switch rotates the bearer, and navigation read for the previous workspace
// must vanish immediately rather than linger until the next poll: a stale link
// in first-party chrome is a claim about what this workspace has installed.

import { browser } from '$app/environment';
import { derived, get, type Readable } from 'svelte/store';

import { fetchAppDirectory } from '$lib/apps/appDirectory';
import { mountAppNavigation, type AppMountedNavigation } from '$lib/apps/appNavigation';
import {
	getCurrentScopeCredentialRevision,
	scopeCredentialIdentityIsCurrent,
	scopeIdentityStore,
	type ScopeIdentityState
} from './scopeIdentityStore';
import { createSharedPoll } from './sharedPoll';

const IDLE_MS = 5 * 60_000;
const FAST_MS = 60_000;
// One directory page is the whole admission set this shell will consider. An
// installation past the first page contributes no navigation rather than
// forcing the shell to walk a cursor on every poll; the ceiling is far above
// any real deployment's enabled-app count.
const DIRECTORY_PAGE_LIMIT = 48;

interface AppNavigationSnapshot {
	scopeKey: string;
	credentialRevision: number;
	entries: AppMountedNavigation[];
}

function scopeKeyOf(scope: Pick<ScopeIdentityState, 'principal' | 'workspace'>): string {
	return JSON.stringify([scope.principal, scope.workspace]);
}

const poll = createSharedPoll<AppNavigationSnapshot>({
	fetcher: async () => {
		// Capture the identity the read is made under *before* it starts, so a
		// workspace switch that lands mid-flight cannot have its result
		// attributed to the scope that happens to be current when it returns.
		const credentialRevision = getCurrentScopeCredentialRevision();
		const scope = scopeKeyOf(get(scopeIdentityStore));
		const page = await fetchAppDirectory({ section: 'installed', limit: DIRECTORY_PAGE_LIMIT });
		return {
			scopeKey: scope,
			credentialRevision,
			entries: mountAppNavigation(page.entries)
		};
	},
	idleMs: IDLE_MS,
	fastMs: FAST_MS
});

// A rotated bearer (the layout's mount-time session refresh, a workspace
// switch) invalidates the snapshot. Without this the failed read that raced the
// rotation would sit out a whole idle interval — plus its backoff — before the
// shell showed any app navigation at all. `pollNow` is inert while nothing is
// subscribed, so this costs nothing on a surface that shows no navigation.
if (browser) {
	let seenCredentialRevision = getCurrentScopeCredentialRevision();
	scopeIdentityStore.subscribe(() => {
		const revision = getCurrentScopeCredentialRevision();
		if (revision === seenCredentialRevision) return;
		seenCredentialRevision = revision;
		poll.pollNow();
	});
}

/**
 * Navigation admitted for the scope that is signed in *now*.
 *
 * Fail closed on identity: a snapshot read under a different principal,
 * workspace, or bearer generation contributes nothing at all. There is no
 * partial reuse — the whole list is the claim, and half of it is not a
 * smaller true claim.
 */
export const appNavigationEntries: Readable<AppMountedNavigation[]> = derived(
	[poll.value, scopeIdentityStore],
	([snapshot, scope]) => {
		if (!snapshot) return [];
		if (snapshot.scopeKey !== scopeKeyOf(scope)) return [];
		if (!scopeCredentialIdentityIsCurrent(snapshot.credentialRevision)) return [];
		return snapshot.entries;
	},
	[] as AppMountedNavigation[]
);

/** Refresh now — for a surface that just enabled, disabled or removed an app. */
export function pollAppNavigationNow(): void {
	poll.pollNow();
}

/** Hold a faster cadence while a navigation-editing surface is open. */
export function requestFastAppNavigationPolling(): () => void {
	return poll.requestFast();
}
