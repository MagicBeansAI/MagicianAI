import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import VibeComposerHarness from '../../test/fixtures/VibeComposerHarness.svelte';

describe('VibeComposer interactions', () => {
	it('shows a named Super God control without hiding it in the presence dot', () => {
		render(VibeComposerHarness);
		expect(screen.getByRole('button', { name: 'Super god mode' })).toHaveTextContent('Super God');
	});
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

	it('selects a coding model through the visible control', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);

		await user.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		await user.click(screen.getByRole('option', { name: /Deep/ }));
		expect(screen.getByTestId('vibe-profile')).toHaveTextContent('deep');
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('profile:deep');
		expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
	});

	it('shows a blocked engine with its reason in the list, never selectable', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness, {
			props: {
				blockedProfiles: [
					{
						id: 'claude-default',
						label: 'Claude',
						engine: 'claude_code',
						selectable: false,
						readiness: 'auth_required',
						reason: 'Sign in to Claude Code',
						supports_user_image_inputs: false,
						is_default: false
					}
				]
			}
		});
		await user.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		const group = screen.getByRole('group', { name: 'Claude Code' });
		const blocked = within(group).getByRole('option', { name: /Claude/ });
		expect(blocked).toHaveAttribute('aria-disabled', 'true');
		expect(blocked).toHaveTextContent('Sign in to Claude Code');
		expect(group).toHaveTextContent('Claude Code with your Claude login.');

		await user.click(blocked);
		expect(screen.getByTestId('vibe-profile')).not.toHaveTextContent('claude-default');
	});

	it('discloses what a gateway engine reaches before it is chosen', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness, {
			props: {
				extraProfiles: [
					{
						id: 'grok-default',
						label: 'Grok',
						engine: 'grok_acp',
						selectable: true,
						readiness: 'ready',
						gateway: { mcp_servers: ['github'], plugins: [], listed: true },
						supports_user_image_inputs: false,
						is_default: false
					}
				]
			}
		});
		await user.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		const grok = screen.getByRole('option', { name: /Grok/ });
		expect(grok).toHaveTextContent('MCP servers: github · plugins: none');
		expect(grok).toHaveTextContent('connectors on your xAI account');

		await user.keyboard('{ArrowDown}{ArrowDown}{Enter}');
		expect(screen.getByTestId('vibe-profile')).toHaveTextContent('grok-default');
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

	it('opens the projects and runs drawer when clicking the composer dock chooser', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);

		const chooserBtn = screen.getByRole('button', { name: 'Open runs and projects drawer' });
		expect(chooserBtn).toBeInTheDocument();
		expect(chooserBtn).toHaveTextContent('VibeDev Project');
		expect(chooserBtn).toHaveTextContent('Refactor the execution panel');

		await user.click(chooserBtn);
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('open-rail');
	});

	it('provides icon actions and routes delete through the existing owner action', async () => {
		const user = userEvent.setup();
		render(VibeComposerHarness);
		const actions = screen.getByRole('group', { name: 'Run actions' });
		expect(within(actions).getByRole('button', { name: /Dev is idle.*Open super god mode controls/ })).toBeInTheDocument();
		expect(within(actions).getByRole('button', { name: 'New run' })).toHaveTextContent('');
		await user.click(within(actions).getByRole('button', { name: 'Open left panel' }));
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('open-rail');
		await user.click(within(actions).getByRole('button', { name: 'Run options' }));
		expect(screen.getByRole('menuitem', { name: 'Delete run' })).toHaveFocus();
		await user.keyboard('{Escape}');
		expect(screen.queryByRole('menu')).not.toBeInTheDocument();
		expect(within(actions).getByRole('button', { name: 'Run options' })).toHaveFocus();
		await user.click(within(actions).getByRole('button', { name: 'Run options' }));
		await user.click(screen.getByRole('menuitem', { name: 'Delete run' }));
		expect(screen.getByTestId('vibe-action')).toHaveTextContent('delete-run');
		expect(screen.queryByRole('menu')).not.toBeInTheDocument();
	});

	it('omits delete actions when there is no selected run', () => {
		render(VibeComposerHarness, { showActiveRun: false });
		expect(screen.queryByRole('button', { name: 'Run options' })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'New run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Open left panel' })).toBeEnabled();
	});

	it('animates Dev activity and gives pending review priority over preparation', async () => {
		const { rerender } = render(VibeComposerHarness, { busy: true });
		expect(screen.getByTestId('presence-orb')).toHaveAttribute('data-state', 'thinking');
		await rerender({ busy: false, presenceState: 'working' });
		expect(screen.getByTestId('presence-orb')).toHaveAttribute('data-state', 'working');
		await rerender({ busy: true, presenceState: 'needs-you' });
		expect(screen.getByTestId('presence-orb')).toHaveAttribute('data-state', 'needs-you');
		await rerender({ busy: false, presenceState: 'idle' });
		expect(screen.getByTestId('presence-orb')).toHaveAttribute('data-state', 'idle');
	});

	it('hides the send control and displays voice control when composer is empty', async () => {
		const { container } = render(VibeComposerHarness, {
			stagedAttachments: []
		});

		expect(container.querySelector('button.vibe-composer__go')).toBeNull();
		expect(
			screen.getByRole('button', { name: /Voice options/ })
		).toBeInTheDocument();
	});

	it('reveals send control and hides voice control when typing draft, and reverts on clear', async () => {
		const user = userEvent.setup();
		const { container } = render(VibeComposerHarness, {
			stagedAttachments: []
		});

		const textbox = screen.getByRole('textbox');
		await user.type(textbox, 'Fix the styling');

		expect(container.querySelector('button.vibe-composer__go')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: /Voice options/ })).not.toBeInTheDocument();

		await user.clear(textbox);
		expect(container.querySelector('button.vibe-composer__go')).toBeNull();
		expect(
			screen.getByRole('button', { name: /Voice options/ })
		).toBeInTheDocument();
	});
});
