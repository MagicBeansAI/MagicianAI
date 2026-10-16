import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import MiddleEllipsis from './MiddleEllipsis.svelte';

afterEach(() => cleanup());

describe('MiddleEllipsis', () => {
	it('keeps the extension in a non-shrinking tail and the full name in the title', () => {
		const { container } = render(MiddleEllipsis, { text: 'quarterly-report-final.html' });
		const root = container.querySelector('.mg-middle-ellipsis');
		expect(root).toHaveAttribute('title', 'quarterly-report-final.html');
		expect(container.querySelector('.mg-middle-ellipsis__tail')).toHaveTextContent('final.html');
		expect(container.querySelector('.mg-middle-ellipsis__head')).toHaveTextContent('quarterly-report-');
		expect(root).toHaveTextContent('quarterly-report-final.html');
	});

	it('renders a short name entirely in the tail', () => {
		const { container } = render(MiddleEllipsis, { text: 'a.md' });
		expect(container.querySelector('.mg-middle-ellipsis__head')).toBeNull();
		expect(container.querySelector('.mg-middle-ellipsis__tail')).toHaveTextContent('a.md');
	});

	it('copies copyText when the name is clicked and confirms', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		render(MiddleEllipsis, { text: 'report.html', copyText: '/Users/me/out/report.html' });
		await fireEvent.click(screen.getByRole('button', { name: 'report.html — copy path' }));
		await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith('/Users/me/out/report.html'));
		await vi.waitFor(() => expect(screen.getByRole('button', { name: 'Path copied' })).toBeInTheDocument());
	});

	it('is plain text without copyText', () => {
		render(MiddleEllipsis, { text: 'report.html' });
		expect(screen.queryByRole('button')).toBeNull();
	});
});
