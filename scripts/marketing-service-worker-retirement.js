/*
 * Marketing-site service-worker tombstone.
 *
 * The marketing origins no longer use a service worker. An installed worker
 * can otherwise keep returning cached index.html for hashed JavaScript modules
 * long after a new Pages deployment. This file is emitted under every historic
 * worker filename so the browser's normal update check installs it, clears only
 * this origin's Cache Storage, unregisters the stale registration, and reloads
 * controlled pages onto the network.
 */

const marketingHosts = new Set([
	'next.magican.ai',
	'magician-marketing.pages.dev'
]);
const hostname = self.location.hostname;
const isMarketingHost =
	marketingHosts.has(hostname) || hostname.endsWith('.magician-marketing.pages.dev');

if (isMarketingHost) {
	self.addEventListener('install', () => {
		self.skipWaiting();
	});

	self.addEventListener('activate', (event) => {
		event.waitUntil(
			(async () => {
				const cacheNames = await caches.keys();
				await Promise.all(cacheNames.map((cacheName) => caches.delete(cacheName)));
				await self.registration.unregister();

				const windows = await self.clients.matchAll({
					type: 'window',
					includeUncontrolled: true
				});
				await Promise.all(windows.map((client) => client.navigate(client.url)));
			})()
		);
	});
}
