import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import CallsTable from './CallsTable.svelte';

const SAMPLE_ROW = {
	timestamp_ms: Date.UTC(2026, 6, 24, 15, 30, 45),
	operation: 'chat.reply',
	provider: 'openai',
	model: 'gpt-5.6',
	agent_id: 'planner',
	input_tokens: 1234,
	output_tokens: 567,
	cost_usd: 0.0123,
	latency_ms: 2500,
	ttft_ms: 320,
	success: true,
	llm_call_id: 'call-abc',
	kind: 'llm'
};

// An embedding batch row: no output_tokens/ttft_ms/agent_id/llm_call_id, but a
// batch_size. Shape mirrors `buildEmbeddingCallsSql` + the page's `kind` tag.
const EMBEDDING_ROW = {
	timestamp_ms: Date.UTC(2026, 6, 24, 15, 31, 0),
	operation: 'memory_index',
	provider: 'ollama',
	model: 'nomic-embed-text',
	input_tokens: 4096,
	batch_size: 32,
	cost_usd: 0,
	latency_ms: 180,
	success: true,
	kind: 'embedding'
};

describe('CallsTable', () => {
	it('labels Decision Models and retains tiny costs and missing cache', () => {
		render(CallsTable, { rows: [{ ...SAMPLE_ROW, provider: 'decision:typesafe', model: 'jev-1.13.0', cost_usd: 0.000042, cache_read_tokens: null, cache_creation_tokens: null }], loading: false });
		expect(screen.getByText('decision')).toBeInTheDocument();
		expect(screen.getByText('$0.00004200')).toBeInTheDocument();
		expect(screen.getByRole('columnheader', { name: 'Cache read' })).toBeInTheDocument();
		expect(screen.getAllByText('—').length).toBeGreaterThanOrEqual(3);
	});

	it("renders a row's per-call metadata fields", () => {
		render(CallsTable, { rows: [SAMPLE_ROW], loading: false });

		expect(screen.getByText('chat.reply')).toBeInTheDocument();
		expect(screen.getByText('openai')).toBeInTheDocument();
		expect(screen.getByText('gpt-5.6')).toBeInTheDocument();
		expect(screen.getByText('planner')).toBeInTheDocument();
		// Token counts render as localized integers.
		expect(screen.getByText('1,234')).toBeInTheDocument();
		expect(screen.getByText('567')).toBeInTheDocument();
		// Cost formatted with 4 decimals for sub-dollar calls.
		expect(screen.getByText('$0.0123')).toBeInTheDocument();
		// Latency in seconds, TTFT in ms.
		expect(screen.getByText('2.50s')).toBeInTheDocument();
		expect(screen.getByText('320ms')).toBeInTheDocument();
		// Success maps to an "ok" status cell.
		expect(screen.getByText('ok')).toBeInTheDocument();
	});

	it('shows a friendly empty state when there are no rows and not loading', () => {
		render(CallsTable, { rows: [], loading: false });

		expect(screen.getByText(/No calls matched these filters/i)).toBeInTheDocument();
		// No "Load more" affordance when the table is empty.
		expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
	});

	it('hides the empty state and the Load more button while loading', () => {
		render(CallsTable, { rows: [], loading: true });

		expect(screen.queryByText(/No calls matched these filters/i)).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
		expect(screen.getByText(/Loading calls/i)).toBeInTheDocument();
	});

	it('fires onLoadMore when the Load more button is clicked', async () => {
		const user = userEvent.setup();
		const onLoadMore = vi.fn();
		render(CallsTable, { rows: [SAMPLE_ROW], loading: false, onLoadMore });

		await user.click(screen.getByRole('button', { name: 'Load more' }));

		expect(onLoadMore).toHaveBeenCalledTimes(1);
	});

	it('marks failed calls distinctly', () => {
		render(CallsTable, { rows: [{ ...SAMPLE_ROW, success: false }], loading: false });

		expect(screen.getByText('failed')).toBeInTheDocument();
	});

	it('tags an LLM-op row with an "llm" type badge', () => {
		render(CallsTable, { rows: [SAMPLE_ROW], loading: false });

		expect(screen.getByText('llm')).toBeInTheDocument();
	});

	it("renders an embedding row's metadata and an 'embed' type badge", () => {
		render(CallsTable, { rows: [EMBEDDING_ROW], loading: false });

		// Type badge distinguishes embeddings from LLM ops.
		expect(screen.getByText('embed')).toBeInTheDocument();
		// Embedding-specific fields render (operation = purpose, batch size).
		expect(screen.getByText('memory_index')).toBeInTheDocument();
		expect(screen.getByText('ollama')).toBeInTheDocument();
		expect(screen.getByText('nomic-embed-text')).toBeInTheDocument();
		expect(screen.getByText('4,096')).toBeInTheDocument();
		expect(screen.getByText('32')).toBeInTheDocument();
		// Local embeds cost $0.
		expect(screen.getByText('$0')).toBeInTheDocument();
		expect(screen.getByText('180ms')).toBeInTheDocument();
		expect(screen.getByText('ok')).toBeInTheDocument();
	});

	it('gracefully em-dashes columns absent on an embedding row (out tokens, TTFT, agent)', () => {
		render(CallsTable, { rows: [EMBEDDING_ROW], loading: false });

		// output_tokens, ttft_ms, and agent_id don't exist on embeddings —
		// each missing cell degrades to an em dash rather than throwing.
		const dashes = screen.getAllByText('—');
		// At least the three absent metadata cells render an em dash.
		expect(dashes.length).toBeGreaterThanOrEqual(3);
	});

	it('falls back to batch_size presence when a row is untagged (kind absent)', () => {
		const { kind, ...untagged } = EMBEDDING_ROW;
		void kind;
		render(CallsTable, { rows: [untagged], loading: false });

		// No `kind` tag, but the batch_size column marks it as an embedding.
		expect(screen.getByText('embed')).toBeInTheDocument();
	});

	it('mixes LLM and embedding rows in the "All" view', () => {
		render(CallsTable, { rows: [SAMPLE_ROW, EMBEDDING_ROW], loading: false });

		expect(screen.getByText('llm')).toBeInTheDocument();
		expect(screen.getByText('embed')).toBeInTheDocument();
		expect(screen.getByText('chat.reply')).toBeInTheDocument();
		expect(screen.getByText('memory_index')).toBeInTheDocument();
	});
});
