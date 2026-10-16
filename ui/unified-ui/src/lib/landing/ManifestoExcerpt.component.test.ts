import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import ManifestoBlocks from './ManifestoBlocks.svelte';
import ManifestoExcerpt from './ManifestoExcerpt.svelte';

afterEach(cleanup);

describe('ManifestoExcerpt', () => {
	it('shows the dated title, the timing stanza, and a text link into the full document', () => {
		const { container } = render(ManifestoExcerpt);

		expect(screen.getByRole('heading', { name: 'Personal Intelligence is your asset' })).toBeInTheDocument();
		expect(screen.getByText('Sunday, 13 September 2026')).toBeInTheDocument();
		expect(container.textContent).toContain(
			'The work was finished while everyone else was still discussing it.'
		);
		expect(container.textContent).not.toContain('Your company will adapt');
		expect(screen.getByRole('link', { name: /Continue reading the manifesto/ })).toHaveAttribute(
			'href',
			'/manifesto'
		);
	});
});

describe('ManifestoBlocks', () => {
	it('renders It is you who do in bold', () => {
		render(ManifestoBlocks, {
			props: {
				blocks: [
					{
						kind: 'prose',
						text: 'Keeping you relevant. **It is you who do.**'
					}
				]
			}
		});

		const strong = screen.getByText('It is you who do.');
		expect(strong.tagName).toBe('STRONG');
	});

	it('renders It is yours in bold', () => {
		render(ManifestoBlocks, {
			props: {
				blocks: [
					{
						kind: 'verse',
						lines: ['Your personal AI must not.', '**It is yours.**', 'You need to determine.']
					}
				]
			}
		});

		const strong = screen.getByText('It is yours.');
		expect(strong.tagName).toBe('STRONG');
	});
});
