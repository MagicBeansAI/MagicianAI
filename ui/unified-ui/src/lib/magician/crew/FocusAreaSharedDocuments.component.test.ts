import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import FocusAreaSharedDocuments from './FocusAreaSharedDocuments.svelte';
import type { FocusAreaSharedDocs } from './focusAreaSharedPrograms';

const programs = {
	programs: [
		{ name: 'facts.md', title: 'Company facts' },
		{ name: 'positioning.md', title: 'Positioning' }
	]
};

function areaGroup(name: RegExp) {
	return screen.getByRole('group', { name });
}

describe('FocusAreaSharedDocuments', () => {
	it('shows one radio group per area and emits mode and document changes', async () => {
		installFetchMock([{ method: 'GET', match: '/api/magician/v2/programs', handle: () => jsonResponse(programs) }]);
		const changes: Array<{ index: number; docs: FocusAreaSharedDocs }> = [];
		const { rerender } = render(FocusAreaSharedDocuments, {
			props: {
				areas: [
					{ name: 'inbound sweep', docs: { mode: 'global', selected: [] }, retained: [] },
					{ name: 'weekly review', docs: { mode: 'choose', selected: ['facts.md', 'old.md'] }, retained: ['facts.md', 'old.md'] }
				]
			},
			events: { change: (event: CustomEvent<{ index: number; docs: FocusAreaSharedDocs }>) => changes.push(event.detail) }
		});

		expect(screen.getByText(/appended to every cycle/i)).toBeTruthy();
		const first = areaGroup(/^Focus area 1: inbound sweep$/);
		expect((within(first).getByRole('radio', { name: /Use global list/ }) as HTMLInputElement).checked).toBe(true);
		expect(within(first).queryByRole('checkbox')).toBeNull();

		const second = areaGroup(/^Focus area 2: weekly review$/);
		await within(second).findByRole('checkbox', { name: /Positioning/ });
		const stale = within(second).getByRole('checkbox', { name: /old\.md/ }) as HTMLInputElement;
		expect(stale.checked).toBe(true);
		expect(stale.disabled).toBe(false);
		expect(within(second).getByText('not found')).toBeTruthy();
		expect((within(second).getByRole('checkbox', { name: /Company facts/ }) as HTMLInputElement).checked).toBe(true);

		const user = userEvent.setup();
		await user.click(within(first).getByRole('radio', { name: /^None/ }));
		expect(changes.at(-1)).toEqual({ index: 0, docs: { mode: 'none', selected: [] } });

		await user.click(stale);
		expect(changes.at(-1)).toEqual({ index: 1, docs: { mode: 'choose', selected: ['facts.md'] } });

		await rerender({
			areas: [
				{ name: 'inbound sweep', docs: { mode: 'none', selected: [] }, retained: [] },
				{ name: 'weekly review', docs: { mode: 'choose', selected: [] }, retained: ['facts.md', 'old.md'] }
			]
		});
		expect(within(areaGroup(/^Focus area 2: weekly review$/)).getByText(/No documents chosen/)).toBeTruthy();
		// A stored name stays listed after it is unchecked, so it can be checked again.
		expect((within(areaGroup(/^Focus area 2: weekly review$/)).getByRole('checkbox', { name: /old\.md/ }) as HTMLInputElement).checked).toBe(false);
	});

	it('keeps the controls usable when the program list fails, and retries', async () => {
		let requests = 0;
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/programs',
				handle: () => (++requests === 1
					? jsonResponse({ error: 'boom', message: 'Programs unavailable' }, { status: 503 })
					: jsonResponse(programs))
			}
		]);
		const onChange = vi.fn();
		render(FocusAreaSharedDocuments, {
			props: { areas: [{ name: 'inbound sweep', docs: { mode: 'choose', selected: ['facts.md'] }, retained: ['facts.md'] }] },
			events: { change: onChange }
		});

		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toMatch(/Programs unavailable/);
		const stored = screen.getByRole('checkbox', { name: /facts\.md/ }) as HTMLInputElement;
		expect(stored.checked).toBe(true);
		expect(screen.queryByText('not found')).toBeNull();

		const user = userEvent.setup();
		await user.click(screen.getByRole('radio', { name: /Use global list/ }));
		expect(onChange).toHaveBeenCalledTimes(1);

		await user.click(within(alert).getByRole('button', { name: 'Retry' }));
		await screen.findByRole('checkbox', { name: /Positioning/ });
		await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
		expect(screen.getByRole('checkbox', { name: /Company facts/ })).toBeTruthy();
	});
});
