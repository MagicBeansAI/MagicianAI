import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import ContextPill from './ContextPill.svelte';
import ContextPillHarness from '../../test/fixtures/ContextPillHarness.svelte';

const baseProps = {
	threadId: 'general',
	sessionTitle: 'Launch notes',
	updatedAt: null
};

describe('ContextPill inline variant', () => {
	it('floats by default', () => {
		const { container } = render(ContextPill, { props: baseProps });
		expect(container.querySelector('.ctx-pill')).not.toHaveClass('ctx-pill--inline');
	});

	it('drops the floating treatment when docked', () => {
		const { container } = render(ContextPill, { props: { ...baseProps, inline: true } });
		expect(container.querySelector('.ctx-pill')).toHaveClass('ctx-pill--inline');
	});

	it('keeps every affordance when docked — nothing is trimmed', () => {
		render(ContextPill, { props: { ...baseProps, inline: true } });

		expect(screen.getByText('#general')).toBeInTheDocument();
		expect(screen.getByText(/Launch notes/)).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'New session' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Open history' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'More actions' })).toBeInTheDocument();
	});

	it('still renders the archived affordance when docked and read-only', () => {
		render(ContextPill, { props: { ...baseProps, inline: true, isReadOnly: true } });
		expect(screen.getByRole('button', { name: /archived/i })).toBeInTheDocument();
	});

	it('falls back to a placeholder title so the docked row never collapses empty', () => {
		render(ContextPill, { props: { threadId: null, sessionTitle: null, inline: true } });
		expect(screen.getByText('No session')).toBeInTheDocument();
	});
});

describe('ContextPill session-name click', () => {
	it('opens history when the docked session name is clicked', async () => {
		const user = userEvent.setup();
		render(ContextPillHarness, { props: { inline: true } });

		await user.click(screen.getByRole('button', { name: /open sessions and threads/i }));
		expect(screen.getByTestId('ctx-event')).toHaveTextContent('open-history');
		expect(screen.getByTestId('ctx-open-history-count')).toHaveTextContent('1');
	});

	it('leaves the floating name inert — it must not swallow thread-bar clicks', () => {
		render(ContextPillHarness);
		expect(screen.queryByRole('button', { name: /open sessions and threads/i })).toBeNull();
	});

	it('still exposes the dedicated history button when docked', async () => {
		const user = userEvent.setup();
		render(ContextPillHarness, { props: { inline: true } });

		await user.click(screen.getByRole('button', { name: 'Open history' }));
		expect(screen.getByTestId('ctx-open-history-count')).toHaveTextContent('1');
	});

	it('keeps the conversation-wide VibeDev action in the more menu', async () => {
		const user = userEvent.setup();
		render(ContextPillHarness, { props: { inline: true, canBuild: true } });

		expect(screen.queryByRole('menuitem', { name: /build in vibedev/i })).toBeNull();
		await user.click(screen.getByRole('button', { name: 'More actions' }));
		await user.click(screen.getByRole('menuitem', { name: /build in vibedev/i }));

		expect(screen.getByTestId('ctx-event')).toHaveTextContent('build');
		expect(screen.queryByRole('menuitem', { name: /build in vibedev/i })).toBeNull();
	});

	it('does not offer a VibeDev seed for an empty conversation', async () => {
		const user = userEvent.setup();
		render(ContextPillHarness, { props: { inline: true, canBuild: false } });

		await user.click(screen.getByRole('button', { name: 'More actions' }));
		expect(screen.queryByRole('menuitem', { name: /build in vibedev/i })).toBeNull();
	});
});
