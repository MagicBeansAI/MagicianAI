import { describe, expect, it } from 'vitest';

import {
	buildCallsSql,
	buildEmbeddingByModelSql,
	buildEmbeddingByOperationSql,
	buildEmbeddingCallsSql,
	buildEmbeddingOverTimeSql,
	buildEmbeddingTotalsSql
} from './callsQuery';

const METADATA_COLUMNS = [
	'timestamp_ms',
	'operation',
	'provider',
	'model',
	'agent_id',
	'input_tokens',
	'output_tokens',
	'cost_usd',
	'latency_ms',
	'ttft_ms',
	'success',
	'llm_call_id'
];

describe('buildCallsSql', () => {
	it('selects every per-call metadata column', () => {
		const sql = buildCallsSql('TRUE', 50, 0);
		for (const col of METADATA_COLUMNS) {
			expect(sql).toContain(col);
		}
	});

	it('reads from the content-free llm_calls relation', () => {
		expect(buildCallsSql('TRUE', 50, 0)).toContain('FROM llm_calls');
	});

	it('embeds the supplied WHERE clause verbatim', () => {
		const where = 'timestamp_ms >= 100 AND timestamp_ms < 200';
		const sql = buildCallsSql(where, 50, 0);
		expect(sql).toContain(`WHERE ${where} `);
	});

	it('carries a complex WHERE (dimension filters) through unchanged', () => {
		const where =
			"timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) AND agent_id = 'planner'";
		expect(buildCallsSql(where, 50, 0)).toContain(where);
	});

	it('orders by recency (timestamp_ms DESC)', () => {
		expect(buildCallsSql('TRUE', 50, 0)).toMatch(/ORDER BY timestamp_ms DESC/);
	});

	it('reflects the requested limit', () => {
		expect(buildCallsSql('TRUE', 50, 0)).toContain('LIMIT 50');
		expect(buildCallsSql('TRUE', 25, 0)).toContain('LIMIT 25');
	});

	it('reflects the requested offset for pagination', () => {
		expect(buildCallsSql('TRUE', 50, 100)).toContain('OFFSET 100');
		expect(buildCallsSql('TRUE', 50, 50)).toContain('OFFSET 50');
	});

	it('never selects or references prompt / response / content columns (case-insensitive)', () => {
		const sql = buildCallsSql("timestamp_ms >= 0 AND note = 'PROMPT the ReSpOnSe_TeXt CONTENT'", 50, 0);
		// The WHERE literal above intentionally contains the words to prove the
		// assertion below is about the SELECT/relation, not incidental substrings —
		// so we assert only over the projected columns + relation, before the WHERE.
		const projection = sql.slice(0, sql.indexOf('WHERE'));
		expect(projection).not.toMatch(/prompt/i);
		expect(projection).not.toMatch(/response_text/i);
		expect(projection).not.toMatch(/content/i);
	});

	it('keeps the projection content-free even without an adversarial WHERE', () => {
		const sql = buildCallsSql('TRUE', 50, 0);
		expect(sql).not.toMatch(/prompt/i);
		expect(sql).not.toMatch(/response_text/i);
		expect(sql).not.toMatch(/content/i);
	});

	it('handles a zero limit (edge case)', () => {
		const sql = buildCallsSql('TRUE', 0, 0);
		expect(sql).toContain('LIMIT 0');
		expect(sql).toContain('OFFSET 0');
	});

	it('handles a large offset (deep pagination edge case)', () => {
		const sql = buildCallsSql('TRUE', 50, 1_000_000);
		expect(sql).toContain('LIMIT 50');
		expect(sql).toContain('OFFSET 1000000');
	});

	it('produces a single well-formed SELECT statement', () => {
		const sql = buildCallsSql('TRUE', 50, 0);
		expect(sql.startsWith('SELECT ')).toBe(true);
		// LIMIT/OFFSET are the tail of a single statement — no stray semicolons.
		expect(sql).not.toContain(';');
		expect(sql.indexOf('FROM llm_calls')).toBeGreaterThan(sql.indexOf('SELECT'));
		expect(sql.indexOf('ORDER BY')).toBeGreaterThan(sql.indexOf('WHERE'));
		expect(sql.indexOf('LIMIT')).toBeGreaterThan(sql.indexOf('ORDER BY'));
		expect(sql.indexOf('OFFSET')).toBeGreaterThan(sql.indexOf('LIMIT'));
	});
});

const EMBEDDING_COLUMNS = [
	'timestamp_ms',
	'operation',
	'provider',
	'model',
	'input_tokens',
	'batch_size',
	'cost_usd',
	'latency_ms',
	'success'
];

describe('buildEmbeddingCallsSql', () => {
	it('selects every per-embedding-batch metadata column, including batch_size', () => {
		const sql = buildEmbeddingCallsSql('TRUE', 50, 0);
		for (const col of EMBEDDING_COLUMNS) {
			expect(sql).toContain(col);
		}
		expect(sql).toContain('batch_size');
	});

	it('reads from the content-free llm_embeddings relation (not llm_calls)', () => {
		const sql = buildEmbeddingCallsSql('TRUE', 50, 0);
		expect(sql).toContain('FROM llm_embeddings');
		expect(sql).not.toContain('FROM llm_calls');
	});

	it('does not project llm_calls-only columns absent from embeddings', () => {
		const projection = buildEmbeddingCallsSql('TRUE', 50, 0);
		const beforeWhere = projection.slice(0, projection.indexOf('WHERE'));
		expect(beforeWhere).not.toMatch(/output_tokens/);
		expect(beforeWhere).not.toMatch(/ttft_ms/);
		expect(beforeWhere).not.toMatch(/agent_id/);
		expect(beforeWhere).not.toMatch(/llm_call_id/);
	});

	it('embeds the supplied WHERE clause verbatim', () => {
		const where = "timestamp_ms >= 100 AND operation = 'memory_index'";
		expect(buildEmbeddingCallsSql(where, 50, 0)).toContain(`WHERE ${where} `);
	});

	it('orders by recency and paginates', () => {
		const sql = buildEmbeddingCallsSql('TRUE', 25, 75);
		expect(sql).toMatch(/ORDER BY timestamp_ms DESC/);
		expect(sql).toContain('LIMIT 25');
		expect(sql).toContain('OFFSET 75');
	});

	it('keeps the projection content-free', () => {
		const sql = buildEmbeddingCallsSql('TRUE', 50, 0);
		const projection = sql.slice(0, sql.indexOf('WHERE'));
		expect(projection).not.toMatch(/prompt/i);
		expect(projection).not.toMatch(/response_text/i);
		expect(projection).not.toMatch(/content/i);
		expect(projection).not.toMatch(/vector/i);
		expect(projection).not.toMatch(/embedding_values/i);
	});

	it('produces a single well-formed SELECT statement', () => {
		const sql = buildEmbeddingCallsSql('TRUE', 50, 0);
		expect(sql.startsWith('SELECT ')).toBe(true);
		expect(sql).not.toContain(';');
		expect(sql.indexOf('FROM llm_embeddings')).toBeGreaterThan(sql.indexOf('SELECT'));
		expect(sql.indexOf('OFFSET')).toBeGreaterThan(sql.indexOf('LIMIT'));
	});
});

describe('embeddings summary query builders', () => {
	it('totals: COUNT + token/vector SUMs over llm_embeddings', () => {
		const sql = buildEmbeddingTotalsSql('TRUE');
		expect(sql).toContain('FROM llm_embeddings');
		expect(sql).toContain('COUNT(*)');
		expect(sql).toContain('SUM(input_tokens)');
		expect(sql).toContain('SUM(batch_size)');
		expect(sql).not.toContain('GROUP BY');
	});

	it('by-model: groups by model, aggregates tokens, content-free', () => {
		const sql = buildEmbeddingByModelSql('TRUE');
		expect(sql).toContain('FROM llm_embeddings');
		expect(sql).toMatch(/GROUP BY model/);
		expect(sql).toContain('SUM(input_tokens)');
		// Content-free: no prompt/response bodies. ("vectors" is a legitimate
		// count alias, not content, so it is intentionally not forbidden here.)
		expect(sql).not.toMatch(/prompt|response_text|content/i);
	});

	it('by-operation: groups by operation (purpose), aggregates tokens', () => {
		const sql = buildEmbeddingByOperationSql('TRUE');
		expect(sql).toContain('FROM llm_embeddings');
		expect(sql).toMatch(/GROUP BY operation/);
		expect(sql).toContain('SUM(input_tokens)');
	});

	it('over-time: hourly buckets of vectors over llm_embeddings', () => {
		const sql = buildEmbeddingOverTimeSql('TRUE');
		expect(sql).toContain('FROM llm_embeddings');
		expect(sql).toContain('3600000');
		expect(sql).toContain('SUM(batch_size)');
		expect(sql).toMatch(/GROUP BY 1 ORDER BY 1/);
	});

	it('every summary builder embeds the supplied WHERE and stays a single statement', () => {
		const where = "timestamp_ms >= 100 AND model = 'nomic-embed-text'";
		for (const sql of [
			buildEmbeddingTotalsSql(where),
			buildEmbeddingByModelSql(where),
			buildEmbeddingByOperationSql(where),
			buildEmbeddingOverTimeSql(where)
		]) {
			expect(sql).toContain(where);
			expect(sql).not.toContain(';');
			expect(sql.startsWith('SELECT ')).toBe(true);
		}
	});
});
