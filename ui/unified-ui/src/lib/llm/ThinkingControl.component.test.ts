import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import ThinkingControl from './ThinkingControl.svelte';

describe('thinking controls follow model capabilities', () => {
	it('previews qualified steps while dragging and commits the final native value on release', async () => {
		const change = vi.fn();
		render(ThinkingControl, { props: { control: { mode: 'effort', levels: ['low', 'high'], default: 'low' }, value: 'auto' }, events: { change } });
		const slider = screen.getByRole('slider', { name: 'Thinking level' });
		expect(slider).toHaveAttribute('aria-valuemax', '1');
		expect(screen.queryByText('Medium')).toBeNull();
		const track = slider.closest('[data-slider-root]')!;
		vi.spyOn(track, 'getBoundingClientRect').mockReturnValue({ left: 0, right: 100, top: 0, bottom: 36, width: 100, height: 36, x: 0, y: 0, toJSON: () => ({}) });
		await fireEvent.pointerDown(slider, { button: 0, clientX: 0 });
		await fireEvent.pointerMove(document, { clientX: 100 });
		expect(change).not.toHaveBeenCalled();
		expect(slider.getAttribute('aria-valuetext')).toBe('High');
		await fireEvent.pointerUp(document);
		expect(change).toHaveBeenCalledTimes(1);
		expect(change.mock.calls[0][0].detail).toBe('high');
	});
	it('renders a binary model as a switch', async () => {
		const change = vi.fn();
		render(ThinkingControl, { props: { control: { mode: 'toggle', levels: ['none', 'on'], default: 'none' }, value: 'auto' }, events: { change } });
		expect(screen.queryByRole('slider')).toBeNull();
		await fireEvent.click(screen.getByRole('switch', { name: 'Enable thinking' }));
		expect(change.mock.calls[0][0].detail).toBe('on');
	});
	it('sends explicit Off for both switch and native effort models', async () => {
		const binaryChange = vi.fn();
		const binary = render(ThinkingControl, { props: { control: { mode: 'toggle', levels: ['none', 'on'], default: 'on' }, value: 'auto' }, events: { change: binaryChange } });
		await fireEvent.click(screen.getByRole('switch', { name: 'Enable thinking' }));
		expect(binaryChange.mock.calls[0][0].detail).toBe('none');
		binary.unmount();
		const effortChange = vi.fn();
		render(ThinkingControl, { props: { control: { mode: 'effort', levels: ['none', 'low', 'high'], default: 'low' }, value: 'high' }, events: { change: effortChange } });
		await fireEvent.click(screen.getByRole('button', { name: 'Set thinking to Off' }));
		expect(effortChange.mock.calls[0][0].detail).toBe('none');
	});
	it('preserves an unknown CLI default until the user chooses an effort', async () => {
		const change = vi.fn();
		render(ThinkingControl, { props: { control: { mode: 'effort', levels: ['low', 'medium', 'high'], default: null }, value: 'auto' }, events: { change } });
		expect(screen.getByText('Engine default', { selector: '.thinking-value' })).toBeInTheDocument();
		const slider = screen.getByRole('slider', { name: 'Thinking level' });
		expect(slider).toHaveAttribute('aria-valuetext', 'Auto · engine default');
		expect(change).not.toHaveBeenCalled();
		await fireEvent.keyDown(slider, { key: 'End' });
		expect(change.mock.calls[0][0].detail).toBe('high');
	});
	it('supports direct step selection and returning to Auto', async () => {
		const change = vi.fn();
		render(ThinkingControl, { props: { control: { mode: 'effort', levels: ['low', 'high'], default: 'low' }, value: 'high' }, events: { change } });
		await fireEvent.click(screen.getByRole('button', { name: 'Set thinking to Low' }));
		expect(change.mock.calls[0][0].detail).toBe('low');
		await fireEvent.click(screen.getByRole('button', { name: 'Auto' }));
		expect(change.mock.calls[1][0].detail).toBe('auto');
	});
	it('blocks dragging, keyboard changes and step buttons while saving', async () => {
		const change = vi.fn();
		render(ThinkingControl, { props: { control: { mode: 'effort', levels: ['low', 'high'], default: 'low' }, disabled: true }, events: { change } });
		const slider = screen.getByRole('slider', { name: 'Thinking level' });
		expect(slider).toHaveAttribute('aria-disabled', 'true');
		await fireEvent.keyDown(slider, { key: 'End' });
		await fireEvent.pointerDown(slider, { button: 0, clientX: 100 });
		await fireEvent.pointerUp(document);
		expect(screen.getByRole('button', { name: 'Set thinking to High' })).toBeDisabled();
		expect(change).not.toHaveBeenCalled();
	});
	it('hides controls for a fixed model', () => {
		render(ThinkingControl, { props: { control: { mode: 'none', levels: [], default: null } } });
		expect(screen.queryByRole('slider')).toBeNull();
		expect(screen.queryByRole('switch')).toBeNull();
	});
});
