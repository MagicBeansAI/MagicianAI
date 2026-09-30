import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import { VALID_THEMES } from '$lib/shared/stores/themeStore';
import Page from './+page.svelte';
import ThemePlate from './ThemePlate.svelte';
import { families, plateIds, plateNumber } from './plates';

describe('theme specimen sheet', () => {
	it('documents every shipped theme, and nothing else', () => {
		expect([...plateIds].sort()).toEqual([...VALID_THEMES].sort());
	});

	it('renders every plate on the server with the full element sheet', () => {
		families.forEach((family, familyIndex) =>
			family.plates.forEach((plate, plateIndex) => {
				const { body } = render(ThemePlate, { props: { item: plate, number: plateNumber(familyIndex, plateIndex) } });
				expect(body).toContain(`data-theme="${plate.id}"`);
				// The controls are the app's own component library, so a theme whose
				// look lives inside a component (Retro's block shadows) renders here
				// exactly as it ships; daisyUI stand-ins would not.
				for (const marker of [
					'muij-button-primary', 'muij-button-secondary', 'muij-button-outline', 'muij-card', 'muij-input',
					'muij-select', 'muij-checkbox', 'muij-radio-group', 'muij-toggle', 'muij-tabs', 'muij-badge',
					'muij-progressbar', 'muij-progress-container', 'muij-alert', 'muij-table', 'chat-bubble', 'lv-err'
				]) {
					expect(body, `${plate.id} shows ${marker}`).toContain(marker);
				}
			})
		);
	});

	it('renders the neutral frame on the server and never an iframe', () => {
		// The sheet embeds itself per plate only after mount; a prerendered page
		// that already contained iframes would embed itself recursively.
		const { body } = render(Page);
		expect(body).not.toContain('<iframe');
		for (const family of families) {
			expect(body).toContain(family.name);
		}
	});
});
