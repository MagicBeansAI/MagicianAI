import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import { goto } from '$app/navigation';
import ComposerTalkCodeSwitch from './ComposerTalkCodeSwitch.svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn().mockResolvedValue(undefined) }));

describe('full-page Talk and Code navigation', () => {
	it.each([
		{ active: 'talk' as const, destination: 'Code mode', path: '/vibe' },
		{ active: 'code' as const, destination: 'Talk mode', path: '/chat' }
	])('can return from $active with the unsent draft', async ({ active, destination, path }) => {
		vi.mocked(goto).mockClear();
		render(ComposerTalkCodeSwitch, { props: { active, draftText: 'Fix layout & spacing' } });
		await fireEvent.click(screen.getByRole('radio', { name: destination }));
		expect(goto).toHaveBeenCalledExactlyOnceWith(`${path}?draft=Fix%20layout%20%26%20spacing`);
	});
});
