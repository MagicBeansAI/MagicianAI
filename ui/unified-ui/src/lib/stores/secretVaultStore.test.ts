import { describe, expect, it } from 'vitest';
import { approvalDomainLabel } from './secretVaultStore';

describe('approvalDomainLabel', () => {
	it('shows a single domain, a domain set, or all sites', () => {
		expect(approvalDomainLabel({ domain: 'api.example.com' })).toBe('api.example.com');
		expect(approvalDomainLabel({ domains: ['a.example.com', 'b.example.com'] })).toBe(
			'a.example.com, b.example.com'
		);
		expect(approvalDomainLabel({ domains: ['*'] })).toBe('All sites');
		expect(approvalDomainLabel({})).toBe('n/a');
	});
});
