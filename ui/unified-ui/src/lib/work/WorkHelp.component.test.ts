import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, expect, it } from 'vitest';
import WorkHelp from './WorkHelp.svelte';
import { WORK_HELP } from './help';

afterEach(cleanup);

it('keeps tapped help open until dismissed and supports keyboard and hover discovery', async () => {
	render(WorkHelp, { topic: WORK_HELP.programs });
	const button = screen.getByRole('button', { name: 'About Playbooks' });
	expect(button).toHaveAttribute('type', 'button');
	await fireEvent.click(button);
	expect(screen.getByRole('tooltip')).toHaveTextContent(WORK_HELP.programs.description);
	await fireEvent.mouseLeave(button.parentElement!);
	expect(screen.getByRole('tooltip')).toBeVisible();
	await fireEvent.keyDown(window, { key: 'Escape' });
	expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
	await fireEvent.focus(button);
	expect(screen.getByRole('tooltip')).toBeVisible();
	await fireEvent.blur(button);
	expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
	await fireEvent.mouseEnter(button);
	expect(screen.getByRole('tooltip')).toBeVisible();
	await fireEvent.click(document.body);
	expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
});
