import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import FloatingComposerHarness from '../../test/fixtures/FloatingComposerHarness.svelte';
import type { SlashCommandContext } from '$lib/magician/chat/composerCommands';

const CONTEXT: SlashCommandContext = {
	engines: [
		{ name: 'magician', installed: true },
		{ name: 'claude_code', installed: true, models: ['default', 'opus'] }
	],
	chatEngine: { engine: 'magician', model: 'default' },
	serverChatEngine: { engine: 'magician', model: 'default' },
	runEngine: { engine: 'magician', model: 'default' },
	tasks: [{ id: 'task-0001-abcdef', title: 'Research ACME', status: 'paused' }],
	budgetDefaults: { harness_max_cost_usd: 100, native_max_cost_usd: 15 }
};

function options(): string[] {
	const picker = screen.getByTestId('slash-picker');
	return within(picker)
		.getAllByRole('option')
		.map((option) => option.querySelector('.slash-label')?.textContent ?? '');
}

function renderComposer() {
	return render(FloatingComposerHarness, {
		props: { slashCommandsEnabled: true, slashContext: CONTEXT }
	});
}

describe('FloatingComposer slash commands', () => {
	it('opens the command list on "/" and filters it as you type', async () => {
		const user = userEvent.setup();
		renderComposer();
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, '/');
		expect(options()).toEqual(['/budget', '/budget default', '/engine', '/run-engine']);

		await user.type(textbox, 'eng');
		expect(options()).toEqual(['/engine', '/run-engine']);
		expect(screen.getByTestId('composer-slash-navigations')).toHaveTextContent('(root)');
	});

	it('does not open without the opt-in, nor for ordinary text', async () => {
		const user = userEvent.setup();
		render(FloatingComposerHarness);
		await user.type(screen.getByRole('textbox'), '/budget');
		expect(screen.queryByTestId('slash-picker')).toBeNull();
	});

	it('walks /budget → task → grant with the keyboard and never sends a message', async () => {
		const user = userEvent.setup();
		renderComposer();
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, '/budget');
		await user.keyboard('{Enter}');
		expect(screen.getByTestId('composer-value').textContent).toBe('/budget ');
		expect(options()).toEqual(['Research ACME']);
		expect(screen.getByTestId('composer-slash-navigations')).toHaveTextContent('(root),budget');

		await user.keyboard('{Enter}');
		expect(options()).toEqual(['+$25', '+$100', '+1M tokens', '+24 hours', '+50 iterations', 'Unlimited']);

		await user.keyboard('{ArrowDown}{Enter}');
		expect(JSON.parse(screen.getByTestId('composer-slash-command').textContent ?? '')).toEqual({
			type: 'task-budget',
			taskId: 'task-0001-abcdef',
			taskLabel: 'Research ACME',
			grant: { add_cost_usd: 100 },
			grantLabel: '+$100'
		});
		expect(screen.getByTestId('composer-value').textContent).toBe('');
		expect(screen.getByTestId('composer-action').textContent).toBe('');
		expect(screen.queryByTestId('slash-picker')).toBeNull();
	});

	it('steps back with Backspace on an empty sub-query and with Escape', async () => {
		const user = userEvent.setup();
		renderComposer();
		const textbox = screen.getByRole('textbox');

		await user.type(textbox, '/engine');
		await user.keyboard('{Enter}');
		expect(options()).toContain('Claude Code · opus');

		await user.keyboard('{Backspace}');
		expect(screen.getByTestId('composer-value').textContent).toBe('/');
		expect(options()).toEqual(['/budget', '/budget default', '/engine', '/run-engine']);

		await user.keyboard('{ArrowDown}{ArrowDown}{Enter}');
		expect(screen.getByTestId('composer-value').textContent).toBe('/engine ');
		await user.keyboard('{Escape}');
		expect(screen.getByTestId('composer-value').textContent).toBe('/');

		// Escape at the top level closes the picker and leaves the text alone.
		await user.keyboard('{Escape}');
		expect(screen.queryByTestId('slash-picker')).toBeNull();
		expect(screen.getByTestId('composer-value').textContent).toBe('/');
	});

	it('marks the current option and runs a clicked one', async () => {
		const user = userEvent.setup();
		renderComposer();

		await user.type(screen.getByRole('textbox'), '/budget default');
		await user.keyboard('{Enter}');
		const picker = screen.getByTestId('slash-picker');
		const current = within(picker)
			.getAllByRole('option')
			.filter((option) => option.getAttribute('aria-current') === 'true')
			.map((option) => option.querySelector('.slash-label')?.textContent);
		expect(current).toEqual(['Harness runs · $100', 'Native runs · $15']);

		await user.click(within(picker).getByRole('option', { name: /Native runs · Unlimited/ }));
		expect(JSON.parse(screen.getByTestId('composer-slash-command').textContent ?? '')).toEqual({
			type: 'budget-default',
			field: 'native_max_cost_usd',
			value: 'unlimited'
		});
		expect(screen.getByTestId('composer-value').textContent).toBe('');
	});

	it('lets a "/" draft that matches no command send as an ordinary message', async () => {
		const user = userEvent.setup();
		renderComposer();
		await user.type(screen.getByRole('textbox'), '/usr/bin is full');
		expect(screen.queryByTestId('slash-picker')).toBeNull();
		await user.keyboard('{Enter}');
		expect(screen.getByTestId('composer-action')).toHaveTextContent('send:/usr/bin is full');
	});
});
