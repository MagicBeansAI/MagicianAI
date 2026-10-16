import { describe, expect, it } from 'vitest';
import { checkSourceLabel } from './checks';

describe('checkSourceLabel', () => {
	it('says where a project’s checks come from', () => {
		expect(checkSourceLabel({ checks_source: 'project' })).toBe('Set in this project');
		expect(
			checkSourceLabel({ checks_source: 'repo_file', checks_source_detail: '.magician/checks.yaml' })
		).toBe("From the repo's .magician/checks.yaml");
		expect(checkSourceLabel({ checks_source: 'task_runner', checks_source_detail: 'Makefile' })).toBe(
			"From the repo's Makefile"
		);
		expect(checkSourceLabel({ checks_source: 'preset', checks_source_detail: 'Go preset' })).toContain(
			'Go preset'
		);
		expect(checkSourceLabel({ checks_source: 'none' })).toBe('No checks found');
		expect(checkSourceLabel({})).toBe('No checks found');
	});
});
