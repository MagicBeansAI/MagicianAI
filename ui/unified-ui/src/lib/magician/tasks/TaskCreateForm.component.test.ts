import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import TaskCreateFormHarness from '../../../test/fixtures/TaskCreateFormHarness.svelte';

describe('TaskCreateForm interactions', () => {
	it('submits the user-selected owner, thread, output mode, and content', async () => {
		const user = userEvent.setup();
		render(TaskCreateFormHarness);

		await user.type(screen.getByLabelText('Title'), 'Prepare launch brief');
		await user.type(screen.getByLabelText('Description'), 'Summarize the launch decisions.');
		await user.selectOptions(screen.getByLabelText('Assign crew member'), 'agent-2');
		await user.selectOptions(screen.getByLabelText('Run output mode'), 'overwrite');
		await user.selectOptions(screen.getByLabelText('Thread'), 'product');
		await user.click(screen.getByRole('button', { name: 'Create Task' }));

		expect(JSON.parse(screen.getByTestId('task-create-submit').textContent ?? '{}')).toEqual({
			task_title: 'Prepare launch brief',
			task_description: 'Summarize the launch decisions.',
			task_output_mode: 'overwrite',
			task_agent: 'agent-2',
			task_thread: 'product',
			schedule_cron: '',
			schedule_timezone: ''
		});
	});

	it('applies and removes schedule presets without losing the task draft', async () => {
		const user = userEvent.setup();
		render(TaskCreateFormHarness);

		await user.type(screen.getByLabelText('Title'), 'Daily account review');
		await user.click(screen.getByRole('button', { name: /Add Schedule/ }));
		await user.click(screen.getByRole('button', { name: 'Daily at 9 AM' }));

		expect(screen.getByLabelText('Custom cron')).toHaveValue('0 9 * * *');
		expect(screen.getByLabelText('Timezone')).toHaveValue('UTC');
		await user.click(screen.getByRole('button', { name: 'Remove Schedule' }));
		expect(screen.queryByLabelText('Custom cron')).not.toBeInTheDocument();
		expect(screen.getByLabelText('Title')).toHaveValue('Daily account review');
	});

	it('clears draft fields and restores the accumulate output mode', async () => {
		const user = userEvent.setup();
		render(TaskCreateFormHarness);

		await user.type(screen.getByLabelText('Title'), 'Temporary title');
		await user.type(screen.getByLabelText('Description'), 'Temporary detail');
		await user.selectOptions(screen.getByLabelText('Run output mode'), 'overwrite');
		await user.click(screen.getByRole('button', { name: 'Clear' }));

		expect(screen.getByLabelText('Title')).toHaveValue('');
		expect(screen.getByLabelText('Description')).toHaveValue('');
		expect(screen.getByLabelText('Run output mode')).toHaveValue('accumulate');
		expect(screen.getByTestId('task-create-clears')).toHaveTextContent('1');
	});

	it('disables every mutating control while task creation is in progress', () => {
		render(TaskCreateFormHarness, { disabled: true });

		expect(screen.getByLabelText('Title')).toBeDisabled();
		expect(screen.getByLabelText('Description')).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Creating...' })).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Clear' })).toBeDisabled();
	});
});
