# Decision models dashboard

Open `/decision-models` from `/llm` or Engine settings. Choose Today (UTC), 7 or 30 days, an operation, model and quality cohort. Applied answers, stale/preserved/failed writes, model/workflow latency, cost and token/cache reporting coverage appear separately. Unknown application receipts are visible; generic operations do not claim a persisted mutation from confidence eligibility.

LLM agreement is not correctness. Recent sampled channel classifications show frozen body-blind inputs and closed question labels for owner review. Save the complete judgment; edits are revision checked. Reviewed needs-reply misses include their positive-case denominator. Confidence intervals do not correct selective sampling bias. Compare one stable cohort over time before judging regression; pooled cohorts may have different models, packs, projections or thresholds.

Engine settings expose existing Off/Shadow/Gated mode, operator activation before qualification, comparison sample rate and hourly count/spending reservation limits. Settings use the engine's existing revision-aware writer and preserve omitted monitoring fields for older clients. Protected outputs retain their qualification rules.

Page entry makes one bounded summary request; recent cases and settings load only when opened. No automatic polling runs in the background. The service uses a bounded nonblocking writer, a 512KiB SQLite page cache and a 5-second response cache of at most 8 dashboards; no startup/page journal scan. Evidence begins when the collector is deployed and is independent of eval folders. Write errors, capture drops and truncated reads prevent a complete-data claim.

Daily summaries retain 90 days; private cases retain 7 days and at most 2,048 comparisons. Capture diagnostics are process-wide and reset on restart. Model cost groups aggregate in one pass.
