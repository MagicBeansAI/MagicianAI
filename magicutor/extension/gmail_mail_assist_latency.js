(function installGmailMailAssistLatency(globalObject) {
  'use strict';

  const DEFAULT_BUDGET_MS = 16.7;
  const MAX_SAMPLES = 200;
  const samples = [];

  function clockNow() {
    return globalObject.performance && typeof globalObject.performance.now === 'function'
      ? globalObject.performance.now()
      : Date.now();
  }

  function record(durationMs, budgetMs = DEFAULT_BUDGET_MS, surface = 'gmail_chip') {
    const sample = {
      surface,
      duration_ms: Number(durationMs.toFixed(3)),
      budget_ms: budgetMs,
      within_budget: durationMs <= budgetMs,
      recorded_at: Date.now()
    };
    samples.push(sample);
    if (samples.length > MAX_SAMPLES) samples.splice(0, samples.length - MAX_SAMPLES);
    if (!sample.within_budget && globalObject.console && typeof globalObject.console.warn === 'function') {
      globalObject.console.warn(
        `[Magician] ${surface} render missed ${budgetMs}ms budget (${sample.duration_ms}ms)`
      );
    }
    if (typeof globalObject.dispatchEvent === 'function' && typeof globalObject.CustomEvent === 'function') {
      globalObject.dispatchEvent(
        new globalObject.CustomEvent('magician:gmail-chip-latency', { detail: sample })
      );
    }
    return sample;
  }

  function measureRender(render, options = {}) {
    if (typeof render !== 'function') throw new TypeError('render must be a function');
    const started = clockNow();
    const value = render();
    const durationMs = clockNow() - started;
    return {
      value,
      sample: record(
        durationMs,
        Number.isFinite(options.budgetMs) ? options.budgetMs : DEFAULT_BUDGET_MS,
        options.surface || 'gmail_chip'
      )
    };
  }

  function stats() {
    const ordered = samples.map((sample) => sample.duration_ms).sort((a, b) => a - b);
    const violations = samples.filter((sample) => !sample.within_budget).length;
    const percentile = (p) => {
      if (ordered.length === 0) return null;
      return ordered[Math.min(ordered.length - 1, Math.ceil(ordered.length * p) - 1)];
    };
    return {
      count: samples.length,
      violations,
      p50_ms: percentile(0.5),
      p95_ms: percentile(0.95),
      budget_ms: DEFAULT_BUDGET_MS
    };
  }

  const api = Object.freeze({ DEFAULT_BUDGET_MS, measureRender, record, stats });
  globalObject.MagicianGmailLatency = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof globalThis !== 'undefined' ? globalThis : this);
