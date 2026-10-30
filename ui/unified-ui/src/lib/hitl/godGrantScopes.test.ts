import { describe, expect, it } from 'vitest';
import { godGrantScopes } from './godGrantScopes';

describe('God-mode scope choices', () => {
	it('keeps the distinct action and whole-integration duration ids', () => {
		const scopes = godGrantScopes([
			{ id: 'allow_once', label: 'Allow once' },
			{ id: 'session:mcp_tool', label: 'This session: this action' },
			{ id: 'always:mcp_tool', label: 'Always: this action' },
			{ id: 'session:capability', label: 'This session: whole groceries (personal)' },
			{ id: 'always:capability', label: 'Always: whole groceries (personal)' },
			{ id: 'deny', label: 'Deny' }
		]);
		expect(scopes.map(({ label }) => label)).toEqual(['This action', 'Whole tool']);
		expect(scopes[1]).toMatchObject({ sessionId: 'session:capability', alwaysId: 'always:capability' });
	});
	it('keeps both lasting choices for an exact shell invocation', () => {
		expect(godGrantScopes([
			{ id: 'allow_once', label: 'Allow once' },
			{ id: 'session:exact', label: 'This session: this exact shell invocation' },
			{ id: 'always:exact', label: 'Always: this exact shell invocation' },
			{ id: 'deny', label: 'Deny' }
		])).toEqual([{ id: 'exact', label: 'Exact command', detail: 'this exact shell invocation',
			sessionId: 'session:exact', alwaysId: 'always:exact' }]);
	});
	it('keeps server scope order, full paths and exact duration ids', () => {
		const path = `/notes/${'long-folder/'.repeat(30)}plan.md`;
		const scopes = godGrantScopes([
			{ id: 'allow_once', label: 'Allow once' },
			{ id: 'session:file', label: `This session: ${path}` },
			{ id: 'always:file', label: `Always: ${path}` },
			{ id: 'session:folder', label: 'This session: anything under /notes' },
			{ id: 'always:folder', label: 'Always: anything under /notes' },
			{ id: 'deny', label: 'Deny' }
		]);
		expect(scopes.map((scope) => scope.id)).toEqual(['file', 'folder']);
		expect(scopes[0]).toMatchObject({ detail: path, sessionId: 'session:file', alwaysId: 'always:file' });
		expect(scopes[1]).toMatchObject({ detail: 'anything under /notes', sessionId: 'session:folder', alwaysId: 'always:folder' });
	});

	it.each([
		[{ id: 'session:file', label: 'This session: plan.md' }],
		[{ id: 'allow_always', label: 'Allow for this run' }],
		[{ id: 'allow_once', label: 'Allow once' }, { id: 'deny', label: 'Deny' }]
	])('retains the original buttons for an incomplete or different option contract', (...options) => {
		expect(godGrantScopes(options)).toEqual([]);
	});
});
