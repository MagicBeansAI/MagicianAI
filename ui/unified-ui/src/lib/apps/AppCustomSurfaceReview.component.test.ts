import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import AppCustomSurfaceReview from './AppCustomSurfaceReview.svelte';
import type { AppInstallationApproveRequest, AppReviewedCustomSurfaceRequest } from './installationReview';

afterEach(cleanup);

it('shows the reviewed code inventory and grants only the page the owner selects', async () => {
	const surface: AppReviewedCustomSurfaceRequest = {
		entry_points: [
			{ route: '/canvas', document: 'surfaces/canvas.html', document_digest: 'blake3:canvas' },
			{ route: '/board', document: 'surfaces/board.html', document_digest: 'blake3:board' }
		],
		executable_members: [{ path: 'surfaces/main.js', content_digest: 'blake3:main', byte_len: 24 }],
		scan_findings: [{ path: 'surfaces/main.js', pattern: 'fetch' }],
		request_digest: 'blake3:review', entry_point_count: 2, executable_member_count: 1,
		executable_bytes: 24, scan_finding_count: 1, sandbox: 'allow-scripts', csp: "connect-src 'none'"
	};
	const grant: AppInstallationApproveRequest = { review_material_digest: 'blake3:material', granted_custom_surface_entry_points: [] };
	const onChange = vi.fn();
	const view = render(AppCustomSurfaceReview, { props: { review: { requested_custom_surface: surface }, grant }, events: { change: onChange } });
	const canvas = view.getByRole('checkbox', { name: 'Allow interactive page /canvas' }) as HTMLInputElement;
	expect(canvas.checked).toBe(false);
	expect((view.getByRole('checkbox', { name: 'Allow interactive page /board' }) as HTMLInputElement).checked).toBe(false);
	await fireEvent.click(canvas);
	const selected = onChange.mock.calls[0][0].detail;
	expect(selected.granted_custom_surface_entry_points).toEqual([{
		route: '/canvas', document: 'surfaces/canvas.html', reviewed_request_digest: 'blake3:review'
	}]);
	await view.rerender({ review: { requested_custom_surface: surface }, grant: selected });
	await fireEvent.click(canvas);
	expect(onChange.mock.calls[1][0].detail.granted_custom_surface_entry_points).toEqual([]);
	await fireEvent.click(view.getByText('Review page code and isolation'));
	expect(view.getByText('blake3:canvas')).toBeTruthy();
	expect(view.getByText('blake3:main')).toBeTruthy();
	expect(view.getAllByText('surfaces/main.js')).toHaveLength(2);
});
