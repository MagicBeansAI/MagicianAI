import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import TaskCardMenu from './TaskCardMenu.svelte';

vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({ url: new URL('http://localhost/work'), params: {} }) }));

afterEach(() => { cleanup(); document.body.replaceChildren(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function mountInScrollport(top: number, bottom: number, anchorTop: number) {
	vi.stubGlobal('innerHeight', 740);
	vi.stubGlobal('innerWidth', 320);
	const clip = document.createElement('div');
	clip.style.overflowY = 'auto';
	const host = document.createElement('div');
	host.style.position = 'relative';
	const anchor = document.createElement('button');
	host.append(anchor); clip.append(host); document.body.append(clip);
	const bounds = () => new DOMRect(0, top, 320, bottom - top);
	vi.spyOn(clip, 'getBoundingClientRect').mockImplementation(bounds);
	vi.spyOn(host, 'getBoundingClientRect').mockImplementation(bounds);
	vi.spyOn(anchor, 'getBoundingClientRect').mockReturnValue(new DOMRect(270, anchorTop, 32, 44));
	vi.spyOn(HTMLElement.prototype, 'offsetParent', 'get').mockImplementation(function (this: HTMLElement) { return this.classList.contains('task-card-menu') ? host : null; });
	vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(220);
	vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(720);
	render(TaskCardMenu, { target: host, props: { anchor, taskId: 'task', canEditTask: true } });
	return { host, anchor };
}

it.each([
	{ top: 300, bottom: 700, anchorTop: 596 },
	{ top: 300, bottom: 700, anchorTop: 320 },
	{ top: 500, bottom: 620, anchorTop: 540 }
])('positions the menu in viewport coordinates anchored to the trigger (anchor $anchorTop)', async ({ top, bottom, anchorTop }) => {
	mountInScrollport(top, bottom, anchorTop);
	const menu = await screen.findByRole('menu');
	await waitFor(() => expect(menu.style.visibility).toBe('visible'));
	const menuTop = parseFloat(menu.style.top);
	const maxHeight = menu.style.maxHeight ? parseFloat(menu.style.maxHeight) : 720;
	expect(menuTop).toBeGreaterThanOrEqual(8);
	expect(menuTop + maxHeight).toBeLessThanOrEqual(window.innerHeight - 8);
	expect(screen.getByRole('menuitem', { name: 'Improve quality' })).toBeInTheDocument();
});

it('repositions and recaps the open menu when the viewport shrinks', async () => {
	mountInScrollport(300, 700, 320);
	const menu = await screen.findByRole('menu');
	await waitFor(() => expect(menu.style.visibility).toBe('visible'));
	const originalHeight = parseFloat(menu.style.maxHeight || '720');
	vi.stubGlobal('innerHeight', 500);
	await fireEvent(window, new Event('resize'));
	await waitFor(() => expect(parseFloat(menu.style.maxHeight)).toBeLessThan(originalHeight));
	expect(parseFloat(menu.style.top) + parseFloat(menu.style.maxHeight)).toBeLessThanOrEqual(492);
});
