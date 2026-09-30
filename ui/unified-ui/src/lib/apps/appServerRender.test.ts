import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import AppIndicatorRegion from './AppIndicatorRegion.svelte';
import AppSlotRegion from './AppSlotRegion.svelte';
import AppSurfacePage from './AppSurfacePage.svelte';

// Svelte runs `onDestroy` during server rendering (it does not run `onMount`),
// so a component that detaches window/document listeners in `onDestroy` throws
// `window is not defined` and the whole page answers 500. These components sit
// on server-rendered routes (TopBar on every page, the slot region on /today and
// /observe, the surface page under /apps), so each must render on the server
// without touching browser globals.
describe('app components render on the server', () => {
	it('AppIndicatorRegion renders without browser globals', () => {
		expect(() => render(AppIndicatorRegion, { props: { ariaLabel: 'App status indicators' } })).not.toThrow();
	});

	it('AppSlotRegion renders without browser globals', () => {
		expect(() =>
			render(AppSlotRegion, { props: { page: '/', regions: ['primary', 'secondary'], ariaLabel: 'Today app widgets' } })
		).not.toThrow();
	});

	it('AppSurfacePage renders without browser globals', () => {
		expect(() => render(AppSurfacePage, { props: { installationId: 'learning', surfacePath: '' } })).not.toThrow();
	});
});
