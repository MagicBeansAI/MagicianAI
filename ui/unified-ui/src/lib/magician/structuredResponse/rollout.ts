export const STRUCTURED_RESPONSE_ROLLOUT_PERCENT_STEPS = [0, 1, 5, 25, 50, 100] as const;
export const STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SAMPLE_BASE = 10000;

export type StructuredResponseRolloutPercentStep =
	(typeof STRUCTURED_RESPONSE_ROLLOUT_PERCENT_STEPS)[number];

function isStructuredResponseRolloutPercentStep(
	value: number
): value is StructuredResponseRolloutPercentStep {
	return STRUCTURED_RESPONSE_ROLLOUT_PERCENT_STEPS.some((step) => step === value);
}

export function hashRolloutSeed(seed: string): number {
	let hash = 2166136261;
	for (let i = 0; i < seed.length; i += 1) {
		hash ^= seed.charCodeAt(i);
		hash = Math.imul(hash, 16777619) >>> 0;
	}
	return hash;
}

export function parseRolloutPercent(value: string | null): number | null {
	if (!value) return null;
	const normalized = value.trim().replace(/%$/, '');
	if (!normalized) return null;
	const parsed = Number(normalized);
	if (!Number.isFinite(parsed)) return null;
	if (!isStructuredResponseRolloutPercentStep(parsed)) return null;
	return parsed / 100;
}

export function shouldEnableStructuredResponseRollout({
	seed,
	percent,
	sampleBase = STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SAMPLE_BASE
}: {
	seed: string;
	percent: number;
	sampleBase?: number;
}): boolean {
	if (percent <= 0) return false;
	const threshold = Math.max(1, Math.min(sampleBase, percent * sampleBase));
	const bucket = hashRolloutSeed(seed) % sampleBase;
	return bucket < threshold;
}
