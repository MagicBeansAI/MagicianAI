import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import ResurfacingCardActions from './ResurfacingCardActions.svelte';
import type { ResurfacingCard } from './resurfacingQueries';

function card(): ResurfacingCard {
	return {
		candidate_id: 'worth-1',
		line: 'A candidate',
		why_now: 'Due soon',
		source_title: 'Source',
		summary: 'Summary',
		source_kind: 'web',
		source_ref: 'https://example.test',
		open_url: 'https://example.test',
		detail_label: 'Details',
		temporal_anchor_at: 1,
		brief: null,
		// 'legacy', not 'ready': the union is 'v2' | 'legacy', and the parser
		// coerces anything that is not 'v2' to 'legacy' — so a fixture saying
		// 'ready' was exercising a shape the app can never receive.
		brief_status: 'legacy',
		content_revision: '1',
		source_updated: false,
		recommended_action: null,
		actions: [
			{ kind: 'create_task', label: 'Create task', requires_input: true, side_effect: 'creates_task' }
		]
	};
}

afterEach(() => cleanup());

describe('ResurfacingCardActions', () => {
	it('closes the menu on a click anywhere outside it', async () => {
		render(ResurfacingCardActions, { card: card() });
		await fireEvent.click(screen.getByRole('button', { name: 'More actions' }));
		expect(screen.getByRole('menu')).toBeInTheDocument();

		// A menu that only closes by pressing its own trigger again traps the
		// user; the next click anywhere else has to dismiss it.
		await fireEvent.click(document.body);
		expect(screen.queryByRole('menu')).not.toBeInTheDocument();
	});

	it('keeps the menu open while the click lands inside it', async () => {
		render(ResurfacingCardActions, { card: card() });
		await fireEvent.click(screen.getByRole('button', { name: 'More actions' }));
		await fireEvent.click(screen.getByRole('menu'));
		expect(screen.getByRole('menu')).toBeInTheDocument();
	});

	it('closes the menu on Escape', async () => {
		render(ResurfacingCardActions, { card: card() });
		await fireEvent.click(screen.getByRole('button', { name: 'More actions' }));
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.queryByRole('menu')).not.toBeInTheDocument();
	});

	it('flips the menu above the trigger when the viewport bottom is too close', async () => {
		// jsdom lays nothing out, so the menu's height and the trigger's
		// position come from these overrides. innerHeight defaults to 768:
		// space below the trigger is 768 - 8 - 744 = 16px, less than the
		// 120px menu, so the menu must open upward instead of getting culled
		// at the lane's overflow edge.
		const heightDescriptor = Object.getOwnPropertyDescriptor(
			HTMLElement.prototype,
			'offsetHeight'
		);
		Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
			configurable: true,
			get: () => 120
		});
		try {
			render(ResurfacingCardActions, { card: card() });
			const trigger = screen.getByRole('button', { name: 'More actions' });
			trigger.getBoundingClientRect = (() => ({
				x: 380,
				y: 720,
				top: 720,
				bottom: 740,
				left: 380,
				right: 400,
				width: 20,
				height: 20,
				toJSON: () => ({})
			})) as () => DOMRect;
			await fireEvent.click(trigger);
			const menu = screen.getByRole('menu');
			expect(menu.style.top).toBe('596px');
			expect(menu.style.visibility).toBe('visible');
		} finally {
			if (heightDescriptor) {
				Object.defineProperty(HTMLElement.prototype, 'offsetHeight', heightDescriptor);
			}
		}
	});
});
