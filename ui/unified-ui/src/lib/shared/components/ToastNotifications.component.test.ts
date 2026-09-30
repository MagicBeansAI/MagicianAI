import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import { notifications, showSuccess } from '$lib/shared/stores/notifications';
import ToastNotifications from './ToastNotifications.svelte';

afterEach(() => {
	notifications.clear();
	cleanup();
});

describe('ToastNotifications actions', () => {
	it('renders an optional durable-result link without changing ordinary toasts', async () => {
		render(ToastNotifications);
		showSuccess('Task created', undefined, 0, {
			label: 'Open task',
			href: '/t/general/tasks?selected=task-1'
		});

		expect(await screen.findByRole('link', { name: 'Open task' })).toHaveAttribute(
			'href',
			'/t/general/tasks?selected=task-1'
		);
	});
});
