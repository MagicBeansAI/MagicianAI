/** Per-call metadata log query for the /llm Calls tab. Content-free. */
export function buildCallsSql(whereSql: string, limit: number, offset: number): string {
	return (
		`SELECT timestamp_ms, operation, provider, model, agent_id, ` +
		`input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, latency_ms, ttft_ms, success, llm_call_id ` +
		`FROM llm_calls WHERE ${whereSql} ` +
		`ORDER BY timestamp_ms DESC LIMIT ${limit} OFFSET ${offset}`
	);
}

/** Per-embedding-batch metadata log for the /llm Calls tab (Embeddings). Content-free. */
export function buildEmbeddingCallsSql(whereSql: string, limit: number, offset: number): string {
	return (
		`SELECT timestamp_ms, operation, provider, model, ` +
		`input_tokens, batch_size, cost_usd, latency_ms, success ` +
		`FROM llm_embeddings WHERE ${whereSql} ` +
		`ORDER BY timestamp_ms DESC LIMIT ${limit} OFFSET ${offset}`
	);
}

// ─── Embeddings summary query builders ──────────────────────────────────
//
// Aggregate widgets for the /llm Embeddings section, over the content-free
// `llm_embeddings` relation. Each takes a WHERE body (time range + the
// dimensions embeddings actually carry — operation/provider/model; embeddings
// have no agent_id/task_id/chat_session_id) so the section follows the same
// filter bar as the rest of the page. All metadata-only: counts, token sums,
// batch sizes, latency — no prompt/response/vector content exists here.

/** Total embedding batches (COUNT) + total input tokens (SUM) in the window. */
export function buildEmbeddingTotalsSql(whereSql: string): string {
	return (
		`SELECT COUNT(*) AS batches, ` +
		`COALESCE(SUM(batch_size), 0) AS vectors, ` +
		`COALESCE(SUM(input_tokens), 0) AS input_tokens ` +
		`FROM llm_embeddings WHERE ${whereSql}`
	);
}

/** Embedding input-token total grouped by model, biggest first. */
export function buildEmbeddingByModelSql(whereSql: string, limit = 12): string {
	return (
		`SELECT model, COUNT(*) AS batches, ` +
		`COALESCE(SUM(batch_size), 0) AS vectors, ` +
		`COALESCE(SUM(input_tokens), 0) AS input_tokens ` +
		`FROM llm_embeddings WHERE ${whereSql} AND model IS NOT NULL AND model != '' ` +
		`GROUP BY model ORDER BY input_tokens DESC LIMIT ${limit}`
	);
}

/** Embedding input-token total grouped by operation (purpose), biggest first. */
export function buildEmbeddingByOperationSql(whereSql: string, limit = 12): string {
	return (
		`SELECT operation, COUNT(*) AS batches, ` +
		`COALESCE(SUM(batch_size), 0) AS vectors, ` +
		`COALESCE(SUM(input_tokens), 0) AS input_tokens ` +
		`FROM llm_embeddings WHERE ${whereSql} AND operation IS NOT NULL AND operation != '' ` +
		`GROUP BY operation ORDER BY input_tokens DESC LIMIT ${limit}`
	);
}

/** Embedding vectors bucketed hourly (like the spend-over-time widget). */
export function buildEmbeddingOverTimeSql(whereSql: string): string {
	return (
		`SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, ` +
		`COALESCE(SUM(batch_size), 0) AS vectors ` +
		`FROM llm_embeddings WHERE ${whereSql} GROUP BY 1 ORDER BY 1`
	);
}
