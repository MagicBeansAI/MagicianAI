import type { StructuredResponseBlockV1, StructuredResponseV1 } from './types';

const UNIQUE_PLAIN_TEXT_LINES_LIMIT = 24;

function normalizedText(value: unknown): string {
	if (typeof value !== 'string') return '';
	return value.trim();
}

function addTextLine(lines: string[], seen: Set<string>, value: unknown): void {
	const text = normalizedText(value);
	if (!text || seen.has(text)) {
		return;
	}
	seen.add(text);
	lines.push(text);
	if (lines.length >= UNIQUE_PLAIN_TEXT_LINES_LIMIT) {
		return;
	}
}

function addBlockLines(lines: string[], seen: Set<string>, block: StructuredResponseBlockV1): void {
	switch (block.kind) {
		case 'markdown':
		case 'text':
			addTextLine(lines, seen, block.text);
			return;
		case 'callout':
			addTextLine(lines, seen, block.text);
			addTextLine(lines, seen, block.title);
			return;
		case 'key_values':
			addTextLine(lines, seen, block.title);
			for (const item of block.items) {
				const detail = `${item.label}: ${item.value}`;
				addTextLine(lines, seen, detail);
			}
			return;
		case 'table': {
			addTextLine(lines, seen, block.title);
			const headers = block.columns
				.map((column) => column.label)
				.filter(Boolean)
				.join(' | ');
			addTextLine(lines, seen, headers);
			for (const row of block.rows) {
				const rowValue = block.columns
					.map((column) => normalizedText(row[column.key]))
					.filter(Boolean)
					.join(' | ');
				addTextLine(lines, seen, rowValue);
			}
			return;
		}
		case 'list': {
			addTextLine(lines, seen, block.title);
			for (const item of block.items) {
				addTextLine(lines, seen, item.text);
				if (item.detail) addTextLine(lines, seen, item.detail);
			}
			return;
		}
		case 'artifacts': {
			addTextLine(lines, seen, block.title);
			for (const item of block.items) {
				addTextLine(lines, seen, item.label);
			}
			return;
		}
		case 'sources': {
			addTextLine(lines, seen, block.title);
			for (const source of block.items) {
				addTextLine(lines, seen, source.label || source.href);
			}
			return;
		}
		case 'metrics': {
			addTextLine(lines, seen, block.title);
			for (const metric of block.items) {
				const metricText = `${metric.label}: ${metric.value}${metric.unit ? ` ${metric.unit}` : ''}`;
				addTextLine(lines, seen, metricText);
			}
		}
	}
}

export function toPlainText(response: StructuredResponseV1): string {
	if (response.plain_text?.trim()) {
		return response.plain_text.trim();
	}

	const lines: string[] = [];
	const seen = new Set<string>();

	addTextLine(lines, seen, response.title);
	addTextLine(lines, seen, response.summary);

	for (const block of response.blocks) {
		addBlockLines(lines, seen, block);
		if (lines.length >= UNIQUE_PLAIN_TEXT_LINES_LIMIT) break;
	}

	if (lines.length === 0) {
		return response.plain_text?.trim() || '';
	}

	return lines.join('\n');
}
