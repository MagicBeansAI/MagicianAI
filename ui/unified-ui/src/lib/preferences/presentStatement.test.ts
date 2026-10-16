import { describe, expect, it } from 'vitest';

import { presentStatement } from './preferencesApi';

describe('presentStatement', () => {
	it('shows a stored key and json object as a title and fields', () => {
		expect(
			presentStatement(
				'output_format_preference: {"media_type":"text/markdown","scope":"both","depth":2}'
			)
		).toEqual({
			title: 'Output format preference',
			details: [
				{ label: 'Depth', value: '2' },
				{ label: 'Media type', value: 'text/markdown' },
				{ label: 'Scope', value: 'both' }
			]
		});
		expect(presentStatement('analytics.results_output_style: {"format":"plain"}')?.title).toBe(
			'Analytics results output style'
		);
		expect(presentStatement('ui_test_fallback: {"api_url":"x"}')).toEqual({
			title: 'UI test fallback',
			details: [{ label: 'API URL', value: 'x' }]
		});
	});

	it('leaves prose, arrays and broken json as they are', () => {
		expect(presentStatement('Check with me before ordering meat.')).toBeNull();
		expect(presentStatement('Note: {not json}')).toBeNull();
		expect(presentStatement('list: [1,2]')).toBeNull();
	});
});
