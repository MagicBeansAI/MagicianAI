import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import AudioChoicePicker from './AudioChoicePicker.svelte';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });
const choices = [{ id: '', label: 'Profile order' }, { id: 'blocked', label: 'Unavailable', disabled: true }, { id: 'bulbul-v4', label: 'Bulbul v4 Flash' }];

describe('AudioChoicePicker', () => {
	it('opens beside the trigger and follows it when the panel scrolls', async () => {
		const user = userEvent.setup();
		let y = 500;
		vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => ({ x: 400, y, left: 400, top: y, right: 600, bottom: y + 28, width: 200, height: 28, toJSON() {} }));
		vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(120);
		render(AudioChoicePicker, { label: 'Speech model', choices });
		await user.click(screen.getByRole('combobox'));
		const panel = screen.getByRole('listbox').parentElement!;
		const expectedTop = window.innerHeight - 528 - 18 >= 120 ? 534 : 374;
		await waitFor(() => expect(panel.style.top).toBe(`${expectedTop}px`));
		expect(panel.style.left).toBe(`${Math.max(12, Math.min(400, window.innerWidth - 260 - 12))}px`);
		y = 400;
		window.dispatchEvent(new Event('scroll'));
		await waitFor(() => expect(panel.style.top).toBe('434px'));
	});

	it('uses the actual height when a short list opens upward', async () => {
		const user = userEvent.setup();
		const y = window.innerHeight - 50;
		vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => ({ left: 40, top: y, bottom: y + 28, width: 200 } as DOMRect));
		vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(120);
		render(AudioChoicePicker, { label: 'Dictation profile', choices });
		await user.click(screen.getByRole('combobox'));
		await waitFor(() => expect(screen.getByRole('listbox').parentElement!.style.top).toBe(`${y - 126}px`));
	});

	it('skips unavailable choices with the keyboard and restores trigger focus', async () => {
		const user = userEvent.setup();
		const select = vi.fn();
		render(AudioChoicePicker, { props: { label: 'Speech model', choices }, events: { select } });
		const trigger = screen.getByRole('combobox');
		await user.click(trigger);
		await user.keyboard('{ArrowDown}{Enter}');
		expect(select.mock.calls[0][0].detail).toEqual({ id: 'bulbul-v4' });
		expect(screen.queryByRole('listbox')).toBeNull();
		expect(trigger).toHaveFocus();
	});
	it('keeps a no-match search open instead of resetting the preference on Enter', async () => {
		const user = userEvent.setup();
		const select = vi.fn();
		render(AudioChoicePicker, { props: { label: 'Voice', choices: Array.from({ length: 20 }, (_, i) => ({ id: String(i), label: `Voice ${i}` })) }, events: { select } });
		await user.click(screen.getByRole('combobox'));
		await user.type(screen.getByRole('searchbox'), 'no-such-voice{Enter}');
		expect(select).not.toHaveBeenCalled();
		expect(screen.getByText('No matches')).toBeInTheDocument();
		expect(screen.getByRole('combobox')).toHaveAttribute('aria-expanded', 'true');
	});

});
