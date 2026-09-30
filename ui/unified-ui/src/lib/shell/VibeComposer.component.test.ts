import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import VibeComposerHarness from '../../test/fixtures/VibeComposerHarness.svelte';

describe('VibeComposer interactions', () => {
	it('keeps the coding prompt bound and submits with the command chord', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, 'Extract the observe request client');
		expect(screen.getByTestId('vibe-value')).toHaveTextContent(
			'Extract the observe request client'
		);
		await user.keyboard('{Control>}{Enter}{/Control}');
		expect(screen.getByTestId('vibe-action')).toHaveTextContent(
			'submit:Extract the observe request client'
		);
	});

	it('changes the next-run intent without submitting the request', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);
		const modeGroup = screen.getByRole('group', { name: 'Build, discuss, or autopilot mode' });

		await user.click(within(modeGroup).getByRole('button', { name: 'Discuss' }));
		expect(screen.getByTestId('vibe-mode')).toHaveTextContent('discuss');
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('mode:discuss');
		expect(screen.getByRole('button', { name: /Discuss.*read-only/ })).toBeEnabled();

		await user.click(within(modeGroup).getByRole('button', { name: 'Autopilot' }));
		expect(screen.getByTestId('vibe-mode')).toHaveTextContent('autopilot');
	});

	it('selects a coding profile through the visible profile control', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);

		await user.selectOptions(screen.getByRole('combobox', { name: 'Coding profile' }), 'deep');
		expect(screen.getByTestId('vibe-profile')).toHaveTextContent('deep');
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('profile:deep');
	});

	it('shows a blocked engine disabled with its reason, never selectable', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness, {
			props: {
				blockedProfiles: [
					{
						id: 'grok-default',
						label: 'Grok',
						engine: 'grok_acp',
						selectable: false,
						readiness: 'incompatible',
						reason: 'Grok advertises its MCP gateway tools',
						supports_user_image_inputs: false,
						is_default: false
					}
				]
			}
		});
		const picker = screen.getByRole('combobox', { name: 'Coding profile' });
		const blocked = within(picker).getByRole('option', {
			name: 'Grok (unavailable: Grok advertises its MCP gateway tools)'
		});
		expect(blocked).toBeDisabled();
		expect(blocked).toHaveAttribute('title', 'Grok advertises its MCP gateway tools');
		expect(within(picker).getByRole('group', { name: 'Grok' })).toContainElement(blocked);

		await user.selectOptions(picker, 'grok-default');
		expect(screen.getByTestId('vibe-profile')).not.toHaveTextContent('grok-default');
	});

	it('removes staged context and exposes the active-run reset action', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);

		await user.click(screen.getByRole('button', { name: 'Remove architecture.md' }));
		expect(screen.queryByText('architecture.md')).not.toBeInTheDocument();
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('remove:attachment-1');

		await user.click(screen.getByRole('button', { name: 'New run' }));
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('new-run');
	});

	it('shows the submit blocker and prevents command-chord submission', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness, {
			submitDisabled: true,
			submitBlocker: 'Select a writable project first.'
		});
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, 'Make this change');
		expect(screen.getByText('Select a writable project first.')).toBeInTheDocument();
		await user.keyboard('{Meta>}{Enter}{/Meta}');
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('');
		expect(screen.getByRole('button', { name: 'Select a writable project first.' })).toBeDisabled();
	});
});
