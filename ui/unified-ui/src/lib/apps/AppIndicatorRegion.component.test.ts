import { render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import AppIndicatorRegion from './AppIndicatorRegion.svelte';

const DIGEST = `blake3:${'c'.repeat(64)}`;

afterEach(() => vi.unstubAllGlobals());

describe('AppIndicatorRegion', () => {
	it('renders bounded materialized values as text', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
			schema_version: 1, revision: DIGEST, etag: DIGEST, generated_at: new Date().toISOString(),
			indicators: [{
				installation_id: 'learning', installation_generation: 1, indicator_id: 'due', title: '<b>Due</b>', revision: DIGEST,
				evaluated_at: new Date().toISOString(), expires_at: new Date(Date.now() + 60_000).toISOString(), model: { kind: 'badge', count: 3 }
			}]
		}), { headers: { ETag: `"${DIGEST}"` } })));
		const { container } = render(AppIndicatorRegion, { ariaLabel: 'App status indicators' });
		expect(await screen.findByLabelText('App status indicators')).toHaveTextContent(/<b>Due<\/b>\s*3/);
		expect(container.querySelector('b')).toBeNull();
	});
});
