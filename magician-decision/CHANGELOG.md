# Changelog

## [Unreleased]

- Version shared-chunk item binding so old qualifications cannot authorize changed prompts.

- Validate restricted memory outputs and route shared chunks against model-specific question limits and thresholds.

- Fix unbatched confidence fallback, reject ambiguous profile thresholds, and bypass busy queues for providers already in cooldown.

- Route queued classifications and tool decisions through model-specific primary and backup policies.

- Support explicit local/cloud primary and backup mappings, including remote backups with local-mode opt-in.

- Queue model calls by default using the shared fair scheduler; bound MLX handoffs and report admission time separately from inference.

- Add explicit local classification fallbacks with model-owned thresholds, deadline reserves, coherent answer preservation and per-attempt metering.

- Bundle v1.1.0 evidence, connection and lifecycle packs with explicit rubric boundaries and input roles; retain v1.0.0 for reproducible earlier evaluations.
- Add memory decision packs, shared model cooldown/admission, exact qualification identities and an explicit operator gate setting separate from reviewed evidence.

- Share repeated planner schemas losslessly, honor context limits for pinned models, and report actual model health alongside shared action decisions.

- Validate native and harness chat plans with engine-owned prompts and reply schemas.

_Current development version: `0.4.2`._

- Replace surface-specific judges with schema-validated shared action selection, evidence review, and model-specific thresholds.
