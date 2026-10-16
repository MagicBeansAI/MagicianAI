import { describe, expect, it } from 'vitest';
import {
	buildChipHtml,
	chipIconSvg,
	chipTokenRegex,
	escapeChipAttr,
	escapeChipHtml,
	featureCommandForSlug,
	normalizeChipToken,
	normalizeKind,
	parseChipMatch,
	renderValueToHtml,
	serializeChipToken,
	type ChipToken
} from './chipMarkup';

function tokensIn(value: string): ChipToken[] {
	const tokens: ChipToken[] = [];
	const regex = chipTokenRegex();
	let match: RegExpExecArray | null;
	while ((match = regex.exec(value)) !== null) {
		const token = parseChipMatch(match);
		if (token) tokens.push(token);
	}
	return tokens;
}

describe('chat chip token grammar', () => {
	it('recognizes agent, personality, task, and routed skill tokens', () => {
		expect(tokensIn(
			'agent:forge personality:concise task:task_42 skill:web-search via agent:sleuth'
		)).toEqual([
			{ kind: 'agent', slug: 'forge' },
			{ kind: 'personality', slug: 'concise' },
			{ kind: 'task', slug: 'task_42' },
			{ kind: 'skill', slug: 'web-search', routeAgent: 'sleuth' }
		]);
	});

	it('is case-insensitive but preserves the captured slug spelling', () => {
		expect(tokensIn('AGENT:Forge SKILL:Web_Search')).toEqual([
			{ kind: 'agent', slug: 'Forge' },
			{ kind: 'skill', slug: 'Web_Search', routeAgent: undefined }
		]);
	});

	it.each([
		['http://agent:forge'],
		['version:1.2.3'],
		['agent:-bad'],
		['agent:_bad'],
		['preagent:forge'],
		['agent:forge.more']
	])('does not overmatch %j', (value) => {
		expect(tokensIn(value)).toEqual([]);
	});

	it('returns a fresh global regex for each caller', () => {
		const first = chipTokenRegex();
		const second = chipTokenRegex();
		expect(first).not.toBe(second);
		expect(first.exec('agent:one')?.[0]).toBe('agent:one');
		expect(second.exec('agent:two')?.[0]).toBe('agent:two');
	});

	it.each([
		['skill', 'skill'], ['tool', 'skill'], ['personality', 'personality'],
		['task', 'task'], ['feature', 'feature'], ['unknown', 'agent'], ['AGENT', 'agent']
	] as const)('normalizes kind %s to %s', (raw, expected) => {
		expect(normalizeKind(raw)).toBe(expected);
	});
});

describe('chat chip normalization and serialization', () => {
	it('extracts an embedded skill route from a legacy slug', () => {
		expect(normalizeChipToken('skill', ' web-search via agent:sleuth ')).toEqual({
			kind: 'skill',
			slug: 'web-search',
			routeAgent: 'sleuth'
		});
	});

	it('prefers an explicit route and strips routes from non-skill kinds', () => {
		expect(normalizeChipToken('skill', 'web-search', ' forge ')).toEqual({
			kind: 'skill', slug: 'web-search', routeAgent: 'forge'
		});
		expect(normalizeChipToken('agent', ' forge ', 'ignored')).toEqual({
			kind: 'agent', slug: 'forge', routeAgent: undefined
		});
	});

	it.each([
		['tutor', '@tutor'],
		['tutor_quick', '@tutor #quick'],
		['copilot', '@copilot'],
		['vibedev', '@vibedev'],
		// The space is load-bearing: `#` is a token char in the backend tokenizer,
		// so a glued `@vibedev#discuss` is a single token matching no marker.
		['vibedev_discuss', '@vibedev #discuss'],
		['future', '@future']
	])('maps feature %s to literal command %s', (slug, expected) => {
		expect(featureCommandForSlug(slug)).toBe(expected);
		expect(serializeChipToken('feature', slug)).toBe(expected);
	});

	it('serializes standard and routed tokens precisely', () => {
		expect(serializeChipToken('task', 'task-42')).toBe('task:task-42');
		expect(serializeChipToken('skill', 'web-search', 'sleuth')).toBe(
			'skill:web-search via agent:sleuth'
		);
		expect(serializeChipToken('skill', 'web-search via agent:sleuth')).toBe(
			'skill:web-search via agent:sleuth'
		);
	});
});

describe('chat chip HTML safety and rendering', () => {
	it('escapes text and attribute contexts independently', () => {
		expect(escapeChipHtml('<b>&')).toBe('&lt;b&gt;&amp;');
		expect(escapeChipAttr('"<&')).toBe('&quot;&lt;&amp;');
	});

	it.each(['agent', 'skill', 'personality', 'task', 'feature'] as const)(
		'provides a non-interactive currentColor SVG for %s',
		(kind) => {
			const svg = chipIconSvg(kind);
			expect(svg).toContain('<svg');
			expect(svg).toContain('currentColor');
			expect(svg).toContain('aria-hidden="true"');
		}
	);

	it('builds an atomic task chip with a human label and precise id', () => {
		const html = buildChipHtml('task', 'task-42', undefined, 'Quarterly Review');
		expect(html).toContain('contenteditable="false"');
		expect(html).toContain('data-kind="task"');
		expect(html).toContain('data-slug="task-42"');
		expect(html).toContain('data-label="Quarterly Review"');
		expect(html).toContain('title="task:task-42"');
		expect(html).toContain('>Quarterly Review</span>');
	});

	it('renders routed skill context in both data and visible labels', () => {
		const html = buildChipHtml('skill', 'web-search', 'sleuth');
		expect(html).toContain('data-route-agent="sleuth"');
		expect(html).toContain('title="skill:web-search via agent:sleuth"');
		expect(html).toContain('chip__label-main">web-search');
		expect(html).toContain('chip__route">via sleuth');
	});

	it('escapes malicious slug, route, and label content', () => {
		const html = buildChipHtml(
			'skill',
			'<img src=x onerror=1>',
			'evil" onclick="run',
			'<script>alert(1)</script>'
		);
		expect(html).not.toContain('<img src=x');
		expect(html).not.toContain('<script>');
		expect(html).not.toContain('onclick="run"');
		expect(html).toContain('&lt;img src=x onerror=1&gt;');
		expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
	});

	it('hydrates mixed text, chips, newlines, and escaped HTML', () => {
		const html = renderValueToHtml('Hi <team>\nagent:forge and skill:web-search via agent:sleuth.');
		expect(html).toContain('Hi &lt;team&gt;<br>');
		expect(html).toContain('data-kind="agent"');
		expect(html).toContain('data-slug="forge"');
		expect(html).toContain('data-kind="skill"');
		expect(html).toContain('data-route-agent="sleuth"');
		expect(html.endsWith('.')).toBe(true);
	});

	it('returns empty HTML for an empty value and leaves invalid tokens as text', () => {
		expect(renderValueToHtml('')).toBe('');
		expect(renderValueToHtml('agent:-bad')).toBe('agent:-bad');
	});
});
