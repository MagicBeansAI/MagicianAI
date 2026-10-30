import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, expect, it } from 'vitest';
import AnswerVerificationInfo from './AnswerVerificationInfo.svelte';

afterEach(cleanup);

it('shows the actual reason on the first click and supports scroll and dismissal', async () => {
	render(AnswerVerificationInfo, { verification: { details: 'figure 93217, 45330 not found in any tool result <script>bad()</script>' } });
	const button = screen.getByRole('button', { name: 'Not fully verified — view details' });
	expect(button.textContent?.trim()).toBe('');
	expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
	await fireEvent.click(button);
	const dialog = screen.getByRole('dialog', { name: 'Answer verification' });
	expect(screen.getByRole('button', { name: 'Close verification details' })).toHaveFocus();
	expect(dialog).toHaveTextContent('figure 93217, 45330 not found in any tool result');
	expect(dialog.querySelector('details')).toBeNull();
	expect(dialog.querySelector('script')).toBeNull();
	await fireEvent.scroll(window);
	expect(screen.getByRole('dialog', { name: 'Answer verification' })).toBeVisible();
	expect(dialog).toHaveTextContent('93217, 45330');
	await fireEvent.keyDown(window, { key: 'Escape' });
	expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
	expect(button).toHaveFocus();
	await fireEvent.click(button);
	await fireEvent.click(document.body);
	expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});
