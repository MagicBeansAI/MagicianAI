import { describe, expect, it } from 'vitest';

import {
	APP_CONTRACT_FIXTURES,
	APP_CONTRACT_VERSION,
	APP_FIXTURE_DIGEST,
	APP_PROTOCOL_VERSION,
	APP_SCHEMA_DIGEST
} from './AppContractFixtures.generated';

describe('generated app contract fixtures', () => {
	it('round-trips every Rust-authored fixture through JSON', () => {
		for (const fixture of Object.values(APP_CONTRACT_FIXTURES)) {
			expect(JSON.parse(JSON.stringify(fixture))).toEqual(fixture);
		}
	});

	it('keeps the version and content digests explicit', () => {
		expect(APP_CONTRACT_VERSION).toBe('1.0.0');
		expect(APP_PROTOCOL_VERSION).toBe('1');
		expect(APP_SCHEMA_DIGEST).toMatch(/^blake3:[0-9a-f]{64}$/);
		expect(APP_FIXTURE_DIGEST).toMatch(/^blake3:[0-9a-f]{64}$/);
	});

	it('preserves query, replay, artifact and uncertain-effect semantics', () => {
		const query = APP_CONTRACT_FIXTURES.query_request;
		expect(query.predicate.root).toBe(2);
		expect(query.predicate.nodes[2]).toEqual({ kind: 'all', children: [0, 1] });
		expect(query.cursor).toMatch(/^cursor:v1:/);

		const mutation = APP_CONTRACT_FIXTURES.mutation_command;
		expect(mutation.idempotency_key).toMatch(/^mutation-key:/);
		expect(mutation.atomicity).toBe('all_or_nothing');
		const action = APP_CONTRACT_FIXTURES.action_invocation;
		expect(action.idempotency_key).toMatch(/^action-key:/);

		const artifact = APP_CONTRACT_FIXTURES.artifact_projection;
		expect(artifact.source).toBe('artifact_projection');
		expect(artifact.value.media_type).toBe('video/mp4');
		expect(artifact.value.content_digest).toMatch(/^blake3:[0-9a-f]{64}$/);

		const uncertain = APP_CONTRACT_FIXTURES.action_result_uncertain;
		expect(uncertain.status).toBe('uncertain');
		expect(uncertain.error.code).toBe('external_outcome_uncertain');
		expect(uncertain.error.disposition).toBe('outcome_uncertain');
		expect(uncertain.external_effect_receipt_refs).toHaveLength(1);
	});
});
