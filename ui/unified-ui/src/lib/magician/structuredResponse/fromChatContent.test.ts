import { describe, expect, it } from 'vitest';

import { getEscalationResolvedSummary, getMessageText } from '$lib/stores/chatStore';
import { adaptLegacyChatContentToStructuredResponse } from './fromChatContent';
import { toPlainText } from './toPlainText';

const textBlock = {
	type: 'text' as const,
	text: 'Summary from text block'
};

const urlBlock = {
	type: 'url' as const,
	label: 'Source link',
	url: 'https://example.com'
};

describe('adaptLegacyChatContentToStructuredResponse', () => {
	it('adapts plain text with markdown', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'text',
			text: 'Hello **assistant**\nsecond line'
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response).toEqual(
			expect.objectContaining({
				schema: 'magician.structured_response',
				version: 1,
				blocks: [{ kind: 'markdown', text: 'Hello **assistant**\nsecond line' }]
			})
		);
		expect(adapted.plain_text).toBe('Hello **assistant**\nsecond line');
		expect(adapted.plain_text).toBe(toPlainText(adapted.response!));
	});

	it('adapts tool call execution as an actionable callout-like view model', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'tool_call_executed',
			tool_name: 'search',
			summary: 'Found 3 results'
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response).toEqual(
			expect.objectContaining({
				blocks: [expect.objectContaining({ kind: 'callout', tone: 'info', text: 'Found 3 results' })]
			})
		);
		expect(adapted.plain_text).toBe(getMessageText({
			type: 'tool_call_executed',
			tool_name: 'search',
			summary: 'Found 3 results'
		}));
	});

	it('adapts rich tool results to markdown, artifacts, and sources', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'rich_tool_result',
			summary: 'Ran scan',
			content_blocks: [
				textBlock,
				urlBlock,
				{ type: 'file' as const, display_name: 'report.csv', source: { type: 'task_output', task_id: 'task-1' }, relative_path: 'report.csv' }
			]
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response?.blocks).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ kind: 'markdown', text: 'Ran scan' }),
				expect.objectContaining({ kind: 'markdown', text: 'Summary from text block' }),
				expect.objectContaining({
					kind: 'artifacts',
					items: [expect.objectContaining({
						label: 'report.csv',
						artifact_id: 'magician-artifact:session:report.csv'
					})]
				})
			])
	);
		expect(adapted.response?.meta?.provenance).toEqual([
			{ id: 'https://example.com', label: 'Source link', ref: 'https://example.com' }
		]);
	});

	it('adapts attachment and escalation outputs into artifact blocks', () => {
		const resolved = adaptLegacyChatContentToStructuredResponse({
			type: 'escalation_resolved',
			summary: 'Completed by tool',
			output_files: [{ type: 'file', source: { type: 'task_output', task_id: 'task-1' }, display_name: 'result.json', relative_path: 'result.json' }]
		});
		expect(resolved.supported).toBe(true);
		expect(resolved.response?.blocks).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ kind: 'callout', text: 'Completed by tool' }),
				expect.objectContaining({
					kind: 'artifacts',
					items: [expect.objectContaining({
						label: 'result.json',
						artifact_id: 'magician-artifact:session:result.json'
					})]
				})
			])
		);

		const attachment = adaptLegacyChatContentToStructuredResponse({
			type: 'attachment',
			label: 'Invoice PDF',
			filename: 'invoice.pdf',
			mime_type: 'application/pdf'
		});
		expect(attachment.supported).toBe(true);
		expect(attachment.response?.blocks).toEqual([
			expect.objectContaining({
				kind: 'artifacts',
				items: [expect.objectContaining({
					label: 'invoice.pdf',
					artifact_id: 'magician-artifact:session:invoice.pdf',
					mime_type: 'application/pdf'
				})]
			})
		]);
	});

	it('maps escalation resolved output files, links, and text into renderer blocks', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'escalation_resolved',
			summary: 'Execution resolved',
			task_id: 'task-1',
			output_files: [
				{ type: 'text', text: 'Resolved notes: done.' },
				{ type: 'file', source: { type: 'task_output', task_id: 'task-1' }, display_name: 'result.csv', relative_path: 'result.csv' },
				{ type: 'url', label: 'Report', url: 'https://example.com/report' }
			]
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response?.blocks).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ kind: 'artifacts', title: 'Output files' }),
				expect.objectContaining({ kind: 'sources', title: 'Output links' }),
				expect.objectContaining({ kind: 'markdown', text: 'Resolved notes: done.' })
			])
		);
		expect(adapted.response?.blocks).toEqual(
			expect.arrayContaining([
				expect.objectContaining({
					kind: 'artifacts',
					items: [expect.objectContaining({
						artifact_id: 'magician-artifact:task:task-1:result.csv'
					})]
				})
			])
		);
	});

	it('adapts task status updates to task summaries and output artifacts', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'task_status_update',
			task_id: 'task-1',
			execution_id: 'exec-1',
			status: 'completed',
			summary: 'Done',
			output_files: [{ type: 'file', display_name: 'out.csv', relative_path: 'out.csv', source: { type: 'session_output' } }]
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response?.meta).toEqual({ task_id: 'task-1', execution_id: 'exec-1' });
		expect(adapted.response?.actions).toEqual([
			expect.objectContaining({
				kind: 'open_task',
				label: 'Open task',
				task_id: 'task-1'
			})
		]);
		expect(adapted.plain_text).toContain('Task task-1: completed');
		expect(adapted.plain_text).toContain('Done');
		expect(adapted.plain_text).toContain('out.csv');
		expect(adapted.response?.blocks).toEqual(
			expect.arrayContaining([
				expect.objectContaining({
					kind: 'artifacts',
					items: [expect.objectContaining({
						artifact_id: 'magician-artifact:task:task-1:out.csv'
					})]
				})
			])
		);
	});

	it('uses speech_tts as task status summary when canonical summary is missing', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'task_status_update',
			task_id: 'task-2',
			status: 'completed',
			speech_tts: 'Task completed successfully'
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response?.summary).toBe('Task completed successfully');
		expect(adapted.response?.plain_text).toContain('Task completed successfully');
	});

	it('marks active escalation as unsupported but still text-safe', () => {
		const adaptation = adaptLegacyChatContentToStructuredResponse({
			type: 'escalation',
			question: 'Need confirmation',
			escalation_type: 'confirmation'
		});
		expect(adaptation.supported).toBe(false);
		expect(adaptation.plain_text).toBe('Action Required: Need confirmation');
		expect(adaptation.response).toBeNull();
	});

	it('uses canonical escalation resolved summary formatting', () => {
		const rawSummary = 'Execution completed: Done —    Extra details with spaces';
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'escalation_resolved',
			summary: rawSummary
		});
		expect(adapted.supported).toBe(true);
		expect(adapted.response?.summary).toBe(getEscalationResolvedSummary(rawSummary));
	});

	it('handles unknown content kinds without throwing', () => {
		const unknown = { type: 'brand_new' as unknown as 'text' } as any;
		const adaptation = adaptLegacyChatContentToStructuredResponse(unknown);
		expect(adaptation.supported).toBe(false);
		expect(adaptation.reason).toMatch('unsupported_content_type');
		expect(adaptation.plain_text).toBe('Unsupported chat content type: brand_new');
		expect(adaptation.response).toBeNull();
	});
});

describe('structured plain text projector compatibility', () => {
	it('keeps non-structured plain text behavior stable for canonical variants', () => {
		const fixtures = [
			{ type: 'text', text: 'Assistant note with unicode 🚀' },
			{ type: 'tool_call_executed', tool_name: 'search', summary: 'Found 3' },
			{
				type: 'rich_tool_result',
				summary: 'Summary',
				content_blocks: [{ type: 'text' as const, text: 'Row 1' }, { type: 'url' as const, label: 'Docs', url: 'https://docs' }]
			},
			{ type: 'attachment', filename: 'invoice.pdf', label: 'Invoice' },
			{ type: 'task_status_update', task_id: 't1', status: 'running', summary: 'Running' },
			{ type: 'escalation', question: 'Approve?' }
		];

		for (const fixture of fixtures) {
			const adaptation = adaptLegacyChatContentToStructuredResponse(fixture as any);
			if (!adaptation.supported) {
				continue;
			}
			expect(adaptation.plain_text).toBe(toPlainText(adaptation.response!));
			if (fixture.type !== 'rich_tool_result' && fixture.type !== 'attachment') {
				const canonicalText = getMessageText(fixture as any);
				expect(adaptation.plain_text).toContain(canonicalText);
			}
		}
	});

	it('keeps generated plain text deterministic under repeated calls', () => {
		const adapted = adaptLegacyChatContentToStructuredResponse({
			type: 'task_status_update',
			task_id: 'task-1',
			status: 'running',
			summary: 'In progress',
			execution_id: 'exec-1',
			output_files: [{ type: 'file' as const, display_name: 'out.txt', relative_path: 'out.txt' }]
		});
		const second = adaptLegacyChatContentToStructuredResponse({
			type: 'task_status_update',
			task_id: 'task-1',
			status: 'running',
			summary: 'In progress',
			execution_id: 'exec-1',
			output_files: [{ type: 'file' as const, display_name: 'out.txt', relative_path: 'out.txt' }]
		});
		expect(adapted.plain_text).toBe(second.plain_text);
	});

	it('handles missing fields for every legacy kind without throwing', () => {
		expect(adaptFrom({ type: 'text' }).supported).toBe(true);
		expect(adaptFrom({ type: 'tool_call_executed' }).supported).toBe(true);
		expect(adaptFrom({ type: 'rich_tool_result' }).supported).toBe(true);
		expect(adaptFrom({ type: 'attachment' }).supported).toBe(true);
		expect(adaptFrom({ type: 'task_status_update', task_id: 't1' }).supported).toBe(true);
		expect(adaptFrom({ type: 'escalation_resolved' }).supported).toBe(true);
		expect(adaptFrom({ type: 'escalation' }).supported).toBe(false);

		function adaptFrom(content: Parameters<typeof adaptLegacyChatContentToStructuredResponse>[0]) {
			return adaptLegacyChatContentToStructuredResponse(content);
		}
	});
});

describe('toPlainText', () => {
	it('covers every structured block kind in deterministic projection', () => {
		const plainText = toPlainText({
			schema: 'magician.structured_response',
			version: 1,
			plain_text: '',
			title: 'Summary header',
			summary: 'Summary body',
			blocks: [
				{ kind: 'text', text: 'Text block line' },
				{ kind: 'key_values', items: [{ label: 'A', value: '1' }, { label: 'B', value: '2', hint: 'ignored' }] },
				{ kind: 'table', columns: [{ key: 'k1', label: 'Name' }, { key: 'k2', label: 'Value' }], rows: [{ k1: 'row1', k2: '42' }] },
				{ kind: 'list', items: [{ text: 'First', detail: 'Detail', checked: true }, { text: 'Second' }] },
				{ kind: 'artifacts', title: 'Files', items: [{ label: 'artifact-1', href: '/a' }, { label: 'artifact-2' }] },
				{ kind: 'sources', items: [{ label: 'Docs', href: 'https://docs' }] },
				{ kind: 'metrics', items: [{ label: 'Latency', value: '120', unit: 'ms' }, { label: 'Accuracy', value: '99', trend: 'up' }] }
			]
		});

		expect(plainText).toContain('Summary header');
		expect(plainText).toContain('Summary body');
		expect(plainText).toContain('Text block line');
		expect(plainText).toContain('A: 1');
		expect(plainText).toContain('Name | Value');
		expect(plainText).toContain('First');
		expect(plainText).toContain('Files');
		expect(plainText).toContain('artifact-1');
		expect(plainText).toContain('Docs');
		expect(plainText).toContain('Latency: 120 ms');
		expect(plainText).toContain('Accuracy: 99');
	});
});
