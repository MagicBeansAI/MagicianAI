import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { StructuredSourcesBlockV1 } from '$lib/magician/structuredResponse/types';
import SourcesStrip from './SourcesStrip.svelte';

afterEach(() => cleanup());

const block = (items: StructuredSourcesBlockV1['items']): StructuredSourcesBlockV1 => ({
	kind: 'sources',
	title: 'Sources',
	items
});

describe('SourcesStrip', () => {
	it('renders one link per source with host text and a lettermark', () => {
		render(SourcesStrip, {
			block: block([
				{ label: 'Learn Rust', href: 'https://www.rust-lang.org/learn' },
				{ label: 'MDN', href: 'https://developer.mozilla.org/en-US/' }
			])
		});

		const links = screen.getAllByRole('link');
		expect(links).toHaveLength(2);
		expect(links[0]).toHaveAttribute('href', 'https://www.rust-lang.org/learn');
		expect(links[0]).toHaveAttribute('target', '_blank');
		expect(links[0]).toHaveAttribute('rel', 'noopener noreferrer');
		expect(links[0]).toHaveAccessibleName(/Learn Rust/);
		expect(links[0]).toHaveAccessibleName(/rust-lang\.org/);
		expect(screen.getByText('rust-lang.org')).toBeInTheDocument();
		expect(links[0].querySelector('.cn-lettermark')?.textContent?.trim()).toBe('R');
		expect(links[0].querySelector('.cn-lettermark')?.getAttribute('style')).toContain('--cn-tint');
	});

	it('shows the first six sources and a +N chip that reveals the rest', async () => {
		const items = Array.from({ length: 8 }, (_, i) => ({
			label: `Source ${i + 1}`,
			href: `https://site${i + 1}.example.com/page`
		}));
		render(SourcesStrip, { block: block(items) });

		expect(screen.getAllByRole('link')).toHaveLength(6);
		const more = screen.getByRole('button', { name: /\+2/ });
		expect(more).toHaveAttribute('aria-expanded', 'false');

		await fireEvent.click(more);

		expect(screen.getAllByRole('link')).toHaveLength(8);
		expect(screen.queryByRole('button', { name: /\+2/ })).not.toBeInTheDocument();
	});

	it('does not render an unsafe href as a link', () => {
		render(SourcesStrip, {
			block: block([
				{ label: 'Evil', href: 'javascript:alert(1)' },
				{ label: 'Good', href: 'https://example.com/' }
			])
		});

		const links = screen.getAllByRole('link');
		expect(links).toHaveLength(1);
		expect(links[0]).toHaveAccessibleName(/Good/);
		expect(screen.getByText('Evil')).toBeInTheDocument();
		expect(document.querySelector('a[href^="javascript"]')).toBeNull();
	});

	it('routes clicks through onAction when the host provides one', async () => {
		const onAction = vi.fn();
		render(SourcesStrip, {
			block: block([{ label: 'Docs', href: 'https://example.com/docs' }]),
			onAction
		});

		await fireEvent.click(screen.getByRole('link'));

		expect(onAction).toHaveBeenCalledWith({
			kind: 'open_url',
			label: 'Docs',
			url: 'https://example.com/docs'
		});
	});
});
