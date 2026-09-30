import { describe, expect, it } from 'vitest';

import { isPublicRoute } from './publicRoutes';

/** Every page in the app, as the URL it serves, derived from the route files. */
function routePaths(): string[] {
	const files = Object.keys(import.meta.glob('/src/routes/**/+page.svelte'));
	return files
		.map((file) =>
			file
				.replace(/^\/src\/routes/, '')
				.replace(/\/\([^)]+\)/g, '')
				.replace(/\/\+page\.svelte$/, '')
		)
		.map((path) => (path === '' ? '/' : path))
		.filter((path) => !path.includes('['))
		.sort();
}

describe('public routes', () => {
	it('lists exactly the pages a visitor may open without a session', () => {
		const publicPaths = routePaths().filter(isPublicRoute);
		expect(publicPaths).toEqual(['/', '/dev/theme-gallery', '/login', '/manifesto', '/privacy', '/terms']);
	});

	it('gates every Magician surface, including pages outside the app group', () => {
		for (const path of ['/warroom', '/hud', '/draw-overlay', '/notify-overlay', '/contextual-assist', '/screen-region-picker', '/dev/structured-response', '/chat', '/tasks']) {
			expect(isPublicRoute(path), path).toBe(false);
		}
	});

	it('normalizes a trailing slash and ignores the query', () => {
		expect(isPublicRoute('/manifesto/')).toBe(true);
		expect(isPublicRoute('/dev/theme-gallery/')).toBe(true);
		expect(isPublicRoute('/warroom/')).toBe(false);
	});
});
