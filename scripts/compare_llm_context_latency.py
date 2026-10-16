#!/usr/bin/env python3
"""
LLM call latency + cache behaviour probe.

Pulls model + max_output_tokens + streaming + tool_choice etc. from the
active `magician-config.yaml` so the test stays in lockstep with what
magician sends in production. No hardcoded profile defaults.

Scenarios
=========

`--scenario full`
    One FULL call using a captured chat-prompt dump from
    `magician_data_v3/.../debug/chat_prompts/*.json`. Same system
    prompt, full tool catalogue. As of v0.4.0 the history is emitted
    as a **structured Responses-API `input` array** (proper
    `message` / `function_call` / `function_call_output` items),
    mirroring what magician's `openai_responses.rs` actually sends.
    The trace's last `user_turn` is the prompt — preserved verbatim
    so the model elicits a realistic-size response instead of a
    token-thin "continue the conversation" reply. Pass
    `--flat-history` to fall back to the legacy single-user-blob
    layout (back-compat with pre-v0.4.0 runs).

`--scenario minimal`
    One small call (3-line system + 1-line user, no tools). Baseline.

`--scenario both`  [default]
    Single FULL + single MINIMAL, in that order. NOTE the caching
    caveat: the FULL call primes OpenAI's prefix cache. Running this
    scenario a SECOND time within 5 minutes is no longer a cold-cache
    measurement — the script will print the cached_tokens count so you
    can see this explicitly.

`--scenario burst --runs N`
    Sends N IDENTICAL FULL calls back-to-back. Run 1 may be cold OR
    warm depending on whether the same prompt was sent in the last
    ~5 minutes; runs 2..N should hit OpenAI's prefix cache and report
    a large `cached_tokens` value. The point: directly observe how
    much TTFT drops on cache hits.

`--scenario grow --runs N`
    Starts with the FULL prompt's system + tools as the cacheable
    prefix, then appends a synthetic `[ASSISTANT] ... [TOOL_CALL] ...
    [TOOL_RESULT] ... [USER] ...` block on each subsequent call.
    Total input tokens grow ~linearly; cached_tokens should track the
    unchanged prefix; TTFT should grow with the non-cached tail. This
    is the realistic "chat session getting longer" scenario.

`--scenario cooldown --runs 2 --gap-secs 360`
    Send identical FULL calls separated by `--gap-secs`. Useful for
    measuring cache TTL: if gap < ~5 min, runs 2+ should still cache-
    hit. If gap > ~10 min, cache evicts and run 2 returns to cold.

Profile and overrides
=====================

Loads `--profile chat-gptterra-responses-vision-toolsauto-fast` from
`magician-config.yaml > llm.router.profiles.<name>` by
default. Reads model, max_output_tokens, timeout, supports_reasoning,
reasoning.effort, metadata.streaming, metadata.tool_choice. Use
`--model`, `--max-output-tokens`, `--no-stream`, `--profile other`
to override individual fields.

Cache observation
=================

Every response.completed SSE event carries
`response.usage.input_tokens_details.cached_tokens`. Each per-call
report includes this field; the burst/grow scenarios annotate each
run with `cached=NNNN` so cache hits show up immediately. Hit-rate
across the run is included in the summary.

Usage examples
==============

    OPENAI_API_KEY=sk-... python3 scripts/compare_llm_context_latency.py
    OPENAI_API_KEY=sk-... python3 scripts/compare_llm_context_latency.py \\
        --scenario burst --runs 5
    OPENAI_API_KEY=sk-... python3 scripts/compare_llm_context_latency.py \\
        --scenario grow --runs 10
    OPENAI_API_KEY=sk-... python3 scripts/compare_llm_context_latency.py \\
        --scenario cooldown --runs 2 --gap-secs 360
    OPENAI_API_KEY=sk-... python3 scripts/compare_llm_context_latency.py \\
        --profile chat-gptterra-responses-vision-toolsauto-thinking
    OPENAI_API_KEY=sk-... python3 scripts/compare_llm_context_latency.py \\
        --model gpt-4o --no-stream

Dependencies: stdlib + `requests` + `pyyaml`.
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
import webbrowser
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

import requests
import yaml
import pathlib

# The router's profiles and operation_mapping live in a sibling
# `llm-router.yaml`; reading the config file alone yields neither.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from magician_config_text import read_config_text  # noqa: E402


__version__ = "0.5.1"

OPENAI_RESPONSES_URL = "https://api.openai.com/v1/responses"

# Pricing dict — ($/1M input, $/1M cached input, $/1M output).
# Source: https://openai.com/pricing (verify against current rates;
# override at runtime with `--pricing MODEL=IN:CACHED:OUT`).
#
# The OpenAI Responses API returns token counts in `usage` but no
# dollar amount; this script computes cost = tokens × rates locally.
# When a model isn't in this dict and no override is supplied, cost
# is reported as `None` (rendered as `—`) so we never lie about
# numbers we don't know.
DEFAULT_PRICING: dict[str, tuple[float, float, float]] = {
    # GPT-4o family
    "gpt-4o":          (2.50, 1.25,  10.00),
    "gpt-4o-mini":     (0.15, 0.075,  0.60),
    # GPT-4.1 family
    "gpt-4.1":         (2.00, 0.50,   8.00),
    "gpt-4.1-mini":    (0.40, 0.10,   1.60),
    "gpt-4.1-nano":    (0.10, 0.025,  0.40),
    # GPT-5.x — magician's chat profiles. Standard API rates effective
    # 2026-07-30; override with --pricing for a different billing tier.
    "gpt-5":           (5.00, 0.50,  20.00),
    "gpt-5-mini":      (1.00, 0.10,   4.00),
    "gpt-5.6-sol":      (4.00, 0.40,  20.00),
    "gpt-5.6-terra":    (2.00, 0.20,  12.00),
    "gpt-5.6-luna":     (0.20, 0.02,   1.20),
    "gpt-5.5":         (5.00, 0.50,  20.00),
    # GPT-6 standard short-context rates effective 2026-09-22.
    "gpt-6-astra":     (10.00, 1.00,  50.00),
    "gpt-6-sol":        (2.00, 0.20,  10.00),
    "gpt-6.1-sol":      (2.00, 0.10,  10.00),
    "gpt-6-luna":       (0.10, 0.01,   0.50),
    # o-series reasoning models
    "o1":              (15.00, 7.50, 60.00),
    "o1-mini":         (3.00, 1.50,  12.00),
    "o3-mini":         (1.10, 0.55,   4.40),
}


def parse_pricing_override(specs: list[str] | None) -> dict[str, tuple[float, float, float]]:
    """Parse `--pricing MODEL=IN:CACHED:OUT` CLI overrides."""
    out: dict[str, tuple[float, float, float]] = {}
    if not specs:
        return out
    for spec in specs:
        if "=" not in spec:
            raise ValueError(f"--pricing entry missing `=`: {spec!r}")
        model, _, rates = spec.partition("=")
        parts = rates.split(":")
        if len(parts) != 3:
            raise ValueError(
                f"--pricing rates must be IN:CACHED:OUT (got {rates!r})"
            )
        try:
            in_rate, cached_rate, out_rate = (float(p) for p in parts)
        except ValueError as exc:
            raise ValueError(f"--pricing rates not numeric: {rates!r}") from exc
        out[model.strip()] = (in_rate, cached_rate, out_rate)
    return out


def compute_cost_usd(
    model: str,
    input_tokens: int | None,
    cached_tokens: int | None,
    output_tokens: int | None,
    pricing: dict[str, tuple[float, float, float]],
) -> float | None:
    """Compute incurred cost in USD from token counts and per-1M rates.

    Returns `None` when:
    - model is not in the pricing table
    - input or output tokens are missing (call failed before usage was
      reported)

    `cached_tokens` is treated as a SUBSET of `input_tokens` (OpenAI
    Responses-API semantics): the discounted-cache rate applies to
    cached_tokens, the full rate applies to (input_tokens - cached_tokens).
    """
    rates = pricing.get(model)
    if rates is None:
        return None
    if input_tokens is None or output_tokens is None:
        return None
    in_rate, cached_rate, out_rate = rates
    cached = cached_tokens or 0
    non_cached_input = max(0, input_tokens - cached)
    cost = (
        non_cached_input * in_rate
        + cached * cached_rate
        + output_tokens * out_rate
    ) / 1_000_000.0
    return cost


def fmt_usd(cost: float | None) -> str:
    if cost is None:
        return "—"
    if cost < 0.01:
        return f"${cost:.4f}"
    return f"${cost:.3f}"

# Auto-loaded dotenv files, in priority order. The first one that exists
# gets loaded; subsequent ones are ignored. Existing process env wins
# over file values, so explicit `OPENAI_API_KEY=… python3 …` still works.
DEFAULT_DOTENV_CANDIDATES = (
    ".env.development",
    ".env.local",
    ".env",
)


def load_dotenv(path: Path) -> int:
    """Minimal `.env` parser — no external dep on `python-dotenv`.

    Supports:
      - `KEY=VALUE` and `export KEY=VALUE`
      - Comments (lines starting with `#`) and blank lines
      - Surrounding single/double quotes on values
    Does NOT support:
      - Variable substitution (`${OTHER}`), multiline values, escape
        sequences inside quotes. These are rare in our use cases and
        adding them would bloat a 30-line parser into 200.

    Process env wins over file values — existing variables are never
    overwritten, so a CLI `OPENAI_API_KEY=… python3 …` invocation is
    still authoritative.

    Returns the number of variables loaded.
    """
    loaded = 0
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.lstrip("\ufeff").strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].lstrip()
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        value = value.strip()
        if (value.startswith('"') and value.endswith('"')) or (
            value.startswith("'") and value.endswith("'")
        ):
            value = value[1:-1]
        if not key:
            continue
        if key in os.environ:
            continue  # don't clobber explicit env
        os.environ[key] = value
        loaded += 1
    return loaded

DEFAULT_PROFILE_NAMES = (
    "chat-gptterra-responses-vision-toolsauto-fast",
    "chat-gptterra-responses-vision-toolsauto-thinking",
)
# Backward-compat alias for the help string (single profile).
DEFAULT_PROFILE_NAME = DEFAULT_PROFILE_NAMES[0]

# Suite scenario: runs sub-scenarios in cache-friendly order.
# Order: `both` first to capture a cold-cache FULL baseline before burst
# heats the prefix cache; then `burst` to observe cache hits; then `grow`
# to test cache locality as conversation extends.
SUITE_SCENARIOS = (
    ("both", 3),    # 3 full + 3 minimal calls
    ("burst", 3),   # 3 identical FULL calls
    ("grow", 5),    # 5 growing-tail calls
)
DEFAULT_CONFIG_PATH = "magician-config.yaml"
DEFAULT_TRACE = (
    "magician_data_v3/scopes/anonymous/default/debug/chat_prompts/"
    "chat-turn-eaa69d0f-7634-402e-b7f6-8d4b96dc8604-iter1.json"
)

MINIMAL_INSTRUCTIONS = (
    "You are a helpful assistant.\n"
    "Be concise and direct.\n"
    "Reply in plain text."
)
MINIMAL_USER = "Say hello in one short sentence."

# Default terminal prompt for the FULL scenarios. Captured magician
# traces frequently end on a stub turn (`"hi"`, `"thanks"`) which
# produces a token-thin reply and undervalues real chat-turn latency
# by 3-10×. Substituting this prompt forces a substantive,
# production-realistic response (hundreds-to-thousands of output
# tokens) while keeping system prompt + tools + prior history
# unchanged. Override per-run with `--final-prompt "..."` or
# disable substitution with `--no-default-final-prompt`.
DEFAULT_FINAL_PROMPT = (
    "Given all this information, please share what all you know "
    "and what insights can you derive. We have to mail it eventually."
)


# ----- Insights pass (post-measurement, pre-HTML) -----
# After the measurement loop finishes, we send the collected stats
# back to the thinking profile and ask it to derive insights across
# multiple dimensions. The model's response is saved as
# `insights.md` next to the other artifacts and embedded into the
# HTML report so the dashboard opens with quantitative commentary at
# the top instead of just raw charts.
#
# Names below are stable so the JS renderer in the HTML template can
# resolve `[chart:<id>]` and `[bucket:<profile>/<scenario>]` citations
# into clickable chips that scroll/highlight the referenced element.
INSIGHTS_PROFILE_NAME = "chat-gptterra-responses-vision-toolsauto-thinking"

INSIGHTS_INSTRUCTIONS = """\
You are analyzing benchmark results from an LLM latency / cost / cache
probe. You receive structured stats per (profile, scenario) bucket:
TTFT, total latency, input/output/cached/reasoning tokens, USD cost,
per-call values. Your job is to derive QUANTITATIVE insights across
multiple dimensions and produce a markdown report that a human will
read alongside an HTML dashboard.

Format requirements — follow exactly:

* Each insight is a level-3 markdown header: `### N. Short title`.
* Below each header, write 2-5 sentences of analysis. Be quantitative
  — cite specific numbers from the data. Avoid vague verbs like
  "improves" or "helps"; use concrete values and ratios.
* When you reference a chart in the HTML dashboard, use the tag
  `[chart:<id>]` inline. Valid chart ids are:
  `chart-ttft-mean`, `chart-total-mean`, `chart-input-mean`,
  `chart-cache-mean`, `chart-cost-total`, `chart-out-mean`, and
  per-bucket `chart-bucket-N-perf` / `chart-bucket-N-tok` (N is the
  index of the bucket in REPORT.buckets, 0-based).
* When you reference a bucket, use `[bucket:<profile>/<scenario>]`
  with the profile name (full) and scenario label (FULL / MINIMAL /
  BURST / GROW).
* When you embed a specific data point inline, use
  `[metric:<profile>/<scenario>/<field>=<value>]` so the renderer
  can show a hover tooltip with provenance.
* Use **bold** for the punchline number in each insight. Use
  `inline code` for field names like `ttft_ms`, `cached_tokens`.
* Cover dimensions: latency drivers, cache behaviour (cold vs
  warm), reasoning tax, cost / throughput, variance / tail
  behaviour, profile-selection guidance, surprises that contradict
  intuition. Aim for 8-12 insights total.
* Finish with a `### Heuristics for chat-turn SLOs` section that
  emits 2-3 plain-text budget formulas the reader can apply.

Be falsifiable. If a claim can't be backed by a number in the data,
don't make it.
"""


# ---------------------------------------------------------------- config


@dataclass
class ProfileConfig:
    name: str
    provider: str
    model: str
    max_output_tokens: int
    timeout_secs: int
    streaming: bool
    tool_choice: str
    supports_vision: bool
    supports_reasoning: bool
    reasoning_effort: str | None = None
    reasoning_max_tokens: int | None = None
    api_key_env: str = "OPENAI_API_KEY"
    openai_api_mode: str = "responses"

    @classmethod
    def from_yaml(cls, yaml_path: Path, profile_name: str) -> "ProfileConfig":
        cfg = yaml.safe_load(read_config_text(yaml_path))
        profiles = cfg.get("llm", {}).get("router", {}).get("profiles", {})
        if profile_name not in profiles:
            available = sorted(profiles.keys())
            raise KeyError(
                f"Profile {profile_name!r} not found in {yaml_path}. "
                f"Available: {available[:8]}{'...' if len(available) > 8 else ''}"
            )
        p = profiles[profile_name]
        metadata = p.get("metadata", {}) or {}
        reasoning = p.get("reasoning", {}) or {}
        return cls(
            name=profile_name,
            provider=p.get("provider", "openai"),
            model=p["model"],
            max_output_tokens=int(p.get("max_output_tokens", 32768)),
            timeout_secs=int(p.get("timeout_secs", 600)),
            streaming=bool(metadata.get("streaming", True)),
            tool_choice=str(metadata.get("tool_choice", "auto")),
            supports_vision=bool(p.get("supports_vision", False)),
            supports_reasoning=bool(p.get("supports_reasoning", False)),
            reasoning_effort=reasoning.get("effort"),
            reasoning_max_tokens=reasoning.get("max_reasoning_tokens"),
            api_key_env=p.get("api_key_env", "OPENAI_API_KEY"),
            openai_api_mode=str(metadata.get("openai_api_mode", "responses")),
        )

    def banner(self) -> str:
        bits = [
            f"profile={self.name}",
            f"model={self.model}",
            f"max_output_tokens={self.max_output_tokens}",
            f"streaming={self.streaming}",
            f"tool_choice={self.tool_choice}",
            f"supports_vision={self.supports_vision}",
            f"supports_reasoning={self.supports_reasoning}",
        ]
        if self.supports_reasoning and self.reasoning_effort:
            bits.append(f"reasoning.effort={self.reasoning_effort}")
        return "  ".join(bits)


# ---------------------------------------------------------------- payload


def load_trace(path: Path) -> dict:
    with path.open() as f:
        return json.load(f)


def flatten_history(history: list[dict]) -> str:
    """Flatten magician's internal history shape into one text block.

    See notes in the script docstring: this preserves input token count
    while sidestepping the Responses-API multi-turn / function_call /
    function_call_output mapping. Token count is what matters for the
    latency comparison.
    """
    parts: list[str] = []
    for item in history:
        t = item.get("type", "")
        if t == "user_turn":
            content = item.get("content", [])
            if isinstance(content, list):
                for p in content:
                    if isinstance(p, dict):
                        if p.get("type") == "text":
                            parts.append(f"[USER] {p.get('text', '')}")
                        elif p.get("type") == "image":
                            parts.append("[USER] <image>")
                        else:
                            parts.append(f"[USER] {json.dumps(p)[:400]}")
            elif isinstance(content, str):
                parts.append(f"[USER] {content}")
        elif t == "assistant_turn":
            parts.append(f"[ASSISTANT] {item.get('text') or ''}")
        elif t == "assistant_tool_call":
            name = item.get("tool_name", "?")
            args = item.get("arguments")
            if isinstance(args, (dict, list)):
                args = json.dumps(args)
            parts.append(f"[TOOL_CALL {name}] {args}")
        elif t == "tool_result":
            name = item.get("tool_name", "?")
            content = item.get("content", "")
            if not isinstance(content, str):
                content = json.dumps(content)
            parts.append(f"[TOOL_RESULT {name}] {content[:8000]}")
        else:
            parts.append(f"[{t.upper()}] {json.dumps(item)[:400]}")
    return "\n\n".join(parts)


def convert_tool_specs(tool_specs: list[dict]) -> list[dict]:
    out: list[dict] = []
    for spec in tool_specs:
        out.append({
            "type": "function",
            "name": spec["name"],
            "description": spec.get("description", "") or "",
            "parameters": spec.get("parameters") or {"type": "object", "properties": {}},
        })
    return out


def history_to_responses_input(history: list[dict]) -> list[dict]:
    """Map a captured magician chat history to a Responses-API `input` array.

    Magician's captured history schema:

    * `user_turn`        — `{content: [{type:text|image, ...}, ...]}`
    * `assistant_turn`   — `{text?: str, tool_calls?: [{id, name, arguments}],
                             provider_state?: {response_id, ...}}`
                           (text and tool_calls can both be present; either
                           can be absent.)
    * `tool_result`      — `{tool_call_id, tool_name, content}`

    These map onto the Responses-API `input` array as:

    * `user_turn`        → one `message{role:user, content:[input_text|input_image]}`
    * `assistant_turn`   → up to N+1 items, in order:
        * one `message{role:assistant, content:[output_text]}` when `text` is non-empty
        * one `function_call{call_id, name, arguments}` per entry in `tool_calls`
          (every `function_call` MUST be followed somewhere downstream by a
          matching `function_call_output` with the same `call_id`, or OpenAI
          400s with `No tool call found for function call output with
          call_id ...`).
    * `tool_result`      → one `function_call_output{call_id, output}`,
                           using `tool_call_id` as the link key.

    This is the same shape `openai_responses.rs::map_messages` produces for
    production chat calls. Preserving it gives the model the same
    prefix-cache anchor + the same in-context demonstrations + the same
    role-tagged conversational expectation it sees in production, so
    output-token decisions (and therefore streaming latency) are realistic.

    Unmatched `function_call` / `function_call_output` pairs are dropped:
    OpenAI rejects them with a 400 at request validation. The drop is
    safer than synthesizing a stub — a synthetic output would silently
    change the conversation.
    """
    out: list[dict] = []
    pending_call_ids: set[str] = set()

    def _user_blocks(content) -> list[dict]:
        blocks: list[dict] = []
        if isinstance(content, list):
            for p in content:
                if not isinstance(p, dict):
                    continue
                if p.get("type") == "text":
                    blocks.append({"type": "input_text",
                                   "text": p.get("text", "")})
                elif p.get("type") == "image":
                    url = p.get("url") or p.get("data_url")
                    if url:
                        blocks.append({"type": "input_image", "image_url": url})
                    else:
                        blocks.append({"type": "input_text",
                                       "text": "[image omitted]"})
                else:
                    blocks.append({"type": "input_text",
                                   "text": json.dumps(p)[:2000]})
        elif isinstance(content, str):
            blocks.append({"type": "input_text", "text": content})
        return blocks

    for item in history:
        t = item.get("type", "")
        if t == "user_turn":
            blocks = _user_blocks(item.get("content", []))
            if blocks:
                out.append({"role": "user", "content": blocks})
        elif t == "assistant_turn":
            # 1) optional visible text
            text = item.get("text") or ""
            if text:
                out.append({
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                })
            # 2) zero or more tool_calls nested on the same assistant turn
            for tc in item.get("tool_calls") or []:
                if not isinstance(tc, dict):
                    continue
                call_id = tc.get("id") or tc.get("call_id") or tc.get("tool_call_id")
                if not call_id:
                    # Unidentifiable tool call — skip; emitting it without an
                    # id would 400 on the matching tool_result lookup.
                    continue
                args = tc.get("arguments")
                if isinstance(args, (dict, list)):
                    args = json.dumps(args)
                elif args is None:
                    args = "{}"
                elif not isinstance(args, str):
                    args = str(args)
                out.append({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": tc.get("name") or tc.get("tool_name", "?"),
                    "arguments": args,
                })
                pending_call_ids.add(call_id)
        elif t == "tool_result":
            call_id = item.get("tool_call_id") or item.get("call_id") or item.get("id")
            if not call_id or call_id not in pending_call_ids:
                # Orphan tool result — OpenAI rejects function_call_output
                # without a preceding function_call. Drop silently rather
                # than 400 the whole request.
                continue
            pending_call_ids.discard(call_id)
            content = item.get("content", "")
            if not isinstance(content, str):
                content = json.dumps(content)
            out.append({
                "type": "function_call_output",
                "call_id": call_id,
                "output": content[:32_000],
            })
        else:
            # Unknown shape — preserve token budget without breaking the
            # array; fold into a user-role text block.
            out.append({
                "role": "user",
                "content": [{"type": "input_text",
                             "text": f"[{t.upper()}] {json.dumps(item)[:2000]}"}],
            })

    # Trailing orphan `function_call` items (tool_calls that were never
    # answered in the captured trace) would also 400 — strip them.
    if pending_call_ids:
        out = [
            it for it in out
            if not (it.get("type") == "function_call"
                    and it.get("call_id") in pending_call_ids)
        ]
    return out


@dataclass
class PromptParts:
    """The variable bits of a Responses-API request.

    Two layouts are supported:
    * `user_text` — single-user-message flattened layout. The historical
      default (pre-2026-05-20); kept for back-compat with cached runs.
    * `input_items` — structured Responses-API `input` array with proper
      `message` / `function_call` / `function_call_output` items, one
      per history turn. This mirrors what magician's
      `openai_responses.rs` actually sends and gives the API the same
      multi-turn prefix shape (and therefore the same prefix-cache
      anchoring + same output-token decisions) it sees in production.

    When `input_items` is set it wins; `user_text` is ignored.
    """
    instructions: str
    user_text: str = ""
    input_items: list[dict] | None = None
    tools: list[dict] = field(default_factory=list)


def parts_from_trace_full(
    trace: dict,
    *,
    structured: bool = True,
    final_prompt: str | None = None,
) -> PromptParts:
    """Build the FULL prompt from a captured magician chat-prompt trace.

    Default `structured=True` mirrors production: emits a Responses-API
    `input` array with proper `message` (user/assistant) and
    `function_call` / `function_call_output` items. The last history
    turn is the user's real prompt — preserved verbatim rather than
    replaced with a vague "continue" line, so the model elicits a
    realistic-size response (hundreds-to-thousands of output tokens)
    instead of a token-thin continuation.

    `final_prompt` (when set) replaces the trace's terminal user turn
    with the supplied text. Use this to measure realistic chat-turn
    latency when the captured trace's last user message is a stub
    (`"hi"`, `"thanks"`, etc.) — the structural fidelity stays the
    same (system + tools + 18-turn history are still there), only
    the question being asked changes. Without this, output volume is
    bounded by what the captured prompt actually demanded.

    `structured=False` falls back to the legacy single-user-blob
    layout for direct comparison with older runs.
    """
    if structured:
        items = history_to_responses_input(trace["history"])
        if final_prompt:
            # Replace any trailing user message; if the array ends on
            # an assistant/function item, append the override instead.
            while items and items[-1].get("role") == "user":
                items.pop()
            items.append({
                "role": "user",
                "content": [{"type": "input_text", "text": final_prompt}],
            })
        elif not items or items[-1].get("role") != "user":
            # Trace doesn't end on a user turn — synthesize a brief
            # continuation so the request still completes.
            items.append({
                "role": "user",
                "content": [{"type": "input_text",
                             "text": "Continue from where the last turn left off."}],
            })
        return PromptParts(
            instructions=trace["system_prompt"],
            input_items=items,
            tools=convert_tool_specs(trace["tool_specs"]),
        )
    tail = final_prompt or "(continue the conversation)"
    flat = flatten_history(trace["history"]) + f"\n\n[USER] {tail}"
    return PromptParts(
        instructions=trace["system_prompt"],
        user_text=flat,
        tools=convert_tool_specs(trace["tool_specs"]),
    )


def parts_minimal() -> PromptParts:
    return PromptParts(
        instructions=MINIMAL_INSTRUCTIONS,
        user_text=MINIMAL_USER,
        tools=[],
    )


def openai_model_supports_reasoning_none(model: str) -> bool:
    """Mirror of `magicllm/src/providers/mod.rs::openai_model_supports_reasoning_none`.

    Returns True when the model is a GPT-5.x family model (other than
    `gpt-5-pro*`) that accepts `reasoning.effort: "none"` to opt out of
    reasoning explicitly. magicllm sends this for the fast profile —
    omitting the field would be silently different from production.
    """
    m = model.lower()
    if m.startswith("gpt-5-pro"):
        return False
    if not m.startswith("gpt-5."):
        return False
    rest = m[len("gpt-5."):]
    digits = ""
    for ch in rest:
        if ch.isdigit():
            digits += ch
        else:
            break
    try:
        return int(digits) >= 1
    except ValueError:
        return False


def build_payload(parts: PromptParts, prof: ProfileConfig, *, stream: bool | None = None) -> dict:
    # Prefer the structured multi-turn `input` array when the caller
    # supplied one — this mirrors what magician's openai_responses.rs
    # sends and gives the API the same prefix-cache anchor and the
    # same in-context conversational demonstration the production path
    # relies on. Fall back to the legacy single-user-blob layout when
    # only `user_text` was provided (kept for back-compat with the
    # minimal-prompt path and with cached older runs).
    if parts.input_items is not None:
        input_payload = parts.input_items
    else:
        input_payload = [
            {
                "role": "user",
                "content": [{"type": "input_text", "text": parts.user_text}],
            }
        ]
    payload: dict = {
        "model": prof.model,
        "instructions": parts.instructions,
        "input": input_payload,
        "max_output_tokens": prof.max_output_tokens,
        "stream": stream if stream is not None else prof.streaming,
    }
    if parts.tools:
        payload["tools"] = parts.tools
        payload["tool_choice"] = prof.tool_choice

    # Reasoning payload — mirrors `OpenAIResponsesProvider::reasoning_payload_for_request`
    # in `magicllm/src/providers/openai_responses.rs:560-575` minus
    # `reasoning.max_tokens`:
    #
    # * supports_reasoning + effort set → send `{effort, summary: "auto"}`.
    #   The `summary: "auto"` is auto-added by magicllm when reasoning is
    #   enabled (see `map_reasoning` lines 552-555); without it the API
    #   bills reasoning tokens but returns no reasoning summary, which
    #   alters latency observability.
    # * supports_reasoning = false AND model supports the "none" effort
    #   (gpt-5.1+ except gpt-5-pro*) → send `{effort: "none"}` explicitly.
    #   Omitting this would be silently different from what magician
    #   sends in production.
    # * otherwise → omit the `reasoning` field entirely.
    #
    # NOTE — `reasoning.max_tokens` is deliberately NOT sent. The
    # current OpenAI Responses API rejects this field with
    # `Unknown parameter: 'reasoning.max_tokens'` (status 400). magicllm
    # still inserts it from the YAML's `reasoning.max_reasoning_tokens`
    # but in production the API may be silently treating it as a no-op
    # or magicllm's calls may now be failing too — either way, this
    # script omits it so the test calls actually complete. Overall
    # output is still capped by the top-level `max_output_tokens`.
    if prof.supports_reasoning and prof.reasoning_effort:
        payload["reasoning"] = {
            "effort": prof.reasoning_effort,
            "summary": "auto",
        }
    elif openai_model_supports_reasoning_none(prof.model):
        payload["reasoning"] = {"effort": "none"}
    return payload


# ---------------------------------------------------------------- runner


# ---------------------------------------------------------------- terminal


class Term:
    """ANSI helpers + live-progress one-liner.

    Skips colors when stdout isn't a TTY (e.g., piped to a file or
    less). `live(...)` writes a CR-terminated status string that
    subsequent live() calls overwrite; `live_done(...)` finalises with
    a trailing newline so the next print resumes from a clean line.
    """

    _enabled: bool = True
    _live_active: bool = False

    @classmethod
    def configure(cls, *, force_no_color: bool) -> None:
        cls._enabled = (not force_no_color) and sys.stdout.isatty()

    @classmethod
    def _wrap(cls, code: str, text: str) -> str:
        if not cls._enabled:
            return text
        return f"\033[{code}m{text}\033[0m"

    @classmethod
    def green(cls, text: str) -> str: return cls._wrap("32", text)
    @classmethod
    def red(cls, text: str) -> str: return cls._wrap("31", text)
    @classmethod
    def yellow(cls, text: str) -> str: return cls._wrap("33", text)
    @classmethod
    def cyan(cls, text: str) -> str: return cls._wrap("36", text)
    @classmethod
    def magenta(cls, text: str) -> str: return cls._wrap("35", text)
    @classmethod
    def dim(cls, text: str) -> str: return cls._wrap("2", text)
    @classmethod
    def bold(cls, text: str) -> str: return cls._wrap("1", text)

    SPINNER_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

    @classmethod
    def live(cls, text: str) -> None:
        """Write a single-line status that the next call overwrites."""
        if not cls._enabled:
            # Non-TTY: don't spam with overwrites; only emit on
            # live_done() so piped output stays clean.
            return
        line = f"\r{text}\033[K"  # \033[K clears to end of line
        sys.stdout.write(line)
        sys.stdout.flush()
        cls._live_active = True

    @classmethod
    def live_done(cls, final_text: str) -> None:
        """Replace any in-progress live() line with `final_text` + newline."""
        if cls._live_active and cls._enabled:
            sys.stdout.write("\r\033[K")
        print(final_text)
        cls._live_active = False

    @classmethod
    def spinner_char(cls, chunks: int) -> str:
        return cls.SPINNER_FRAMES[chunks % len(cls.SPINNER_FRAMES)]

    @classmethod
    def truecolor(cls, r: int, g: int, b: int, text: str) -> str:
        """24-bit RGB ANSI. Used for the hero banner gradient."""
        if not cls._enabled:
            return text
        return f"\033[38;2;{r};{g};{b}m{text}\033[0m"


def print_hero_banner(version: str) -> None:
    """Print a colorful welcome banner with version + tagline.

    Skipped when stdout isn't a TTY or --no-color is set; falls back to a
    single-line title so piped output stays clean.
    """
    if not Term._enabled:
        print(f"Magican Perf  v{version}  ·  Context Latency Probe")
        print()
        return

    # Compact block font for "MAGICAN". Seven four-cell glyphs plus the six
    # inter-letter spaces keep each row exactly 34 cells wide.
    # "Perf" goes in the right tagline panel — keeps the box readable
    # at the standard 80-col width.
    glyphs = {
        "M": ("█  █", "████", "█ ██", "█  █", "█  █", "█  █"),
        "A": (" ██ ", "█  █", "████", "█  █", "█  █", "█  █"),
        "G": (" ██ ", "█   ", "█ ██", "█  █", "█  █", " ██ "),
        "I": ("████", " ██ ", " ██ ", " ██ ", " ██ ", "████"),
        "C": (" ██ ", "█  █", "█   ", "█   ", "█  █", " ██ "),
        "N": ("█  █", "██ █", "████", "█ ██", "█  █", "█  █"),
    }
    art = [" ".join(glyphs[letter][row] for letter in "MAGICAN") for row in range(6)]
    # Right-aligned panel of tagline lines. Padded to 33 cells.
    right = [
        "Magican Perf",
        f"v{version}",
        "",
        "OpenAI Responses API",
        "TTFT · cache · cost",
        "",
    ]
    # Gradient on the ASCII art: deep blue → cyan-purple → magenta → pink.
    art_grad = [
        (88, 166, 255),    # bright blue
        (110, 175, 255),
        (140, 150, 255),
        (170, 130, 255),
        (200, 110, 240),
        (220, 100, 200),
    ]
    # Subtle gradient on the right-side text (lighter, less saturated).
    text_grad = [
        (220, 220, 235),
        (180, 180, 210),
        (140, 140, 180),
        (200, 200, 220),
        (160, 160, 195),
        (140, 140, 180),
    ]
    border = (60, 100, 180)

    def tc(c: tuple[int, int, int], s: str) -> str:
        return Term.truecolor(c[0], c[1], c[2], s)

    # Box widths: 78 outer; 76 inner.
    # 2 left + 34 art + 5 gutter + 33 text + 2 trail = 76 ✓
    inner_fill = " " * 76
    print(tc(border, "╔" + "═" * 76 + "╗"))
    print(tc(border, "║") + inner_fill + tc(border, "║"))
    for art_row, text_row, art_color, text_color in zip(art, right, art_grad, text_grad):
        art_padded = art_row.ljust(34)
        right_padded = text_row.ljust(33)
        line = (
            tc(border, "║")
            + "  "
            + tc(art_color, art_padded)
            + "     "
            + tc(text_color, right_padded)
            + "  "
            + tc(border, "║")
        )
        print(line)
    print(tc(border, "║") + inner_fill + tc(border, "║"))
    print(tc(border, "╚" + "═" * 76 + "╝"))
    print()


def _sleep_with_spinner(secs: float, label: str) -> None:
    """Sleep `secs` seconds while updating a single-line spinner.

    Used by the cooldown scenario to make a 6-minute wait visible
    instead of a frozen terminal. The spinner rotates ~4× per second
    and shows seconds remaining as a countdown.
    """
    if secs <= 0:
        return
    start = time.monotonic()
    tick = 0
    while True:
        elapsed = time.monotonic() - start
        remaining = secs - elapsed
        if remaining <= 0:
            break
        spinner = Term.spinner_char(tick)
        tick += 1
        mins, sec = divmod(int(remaining), 60)
        time_str = f"{mins}:{sec:02d}" if mins else f"{int(remaining)}s"
        Term.live(
            f"  {Term.cyan(spinner)} {Term.dim(label)}  "
            f"{Term.bold(time_str)} remaining"
        )
        # 250ms updates — snappy spinner without burning CPU.
        time.sleep(min(0.25, remaining))
    Term.live_done(Term.dim(f"  cooldown elapsed ({int(secs)}s) — resuming"))


def fmt_ms(ms: int | None) -> str:
    if ms is None:
        return "—"
    if ms < 1000:
        return f"{ms} ms"
    return f"{ms / 1000:.2f} s"


def fmt_tokens(n: int | None) -> str:
    if n is None:
        return "—"
    if n >= 1000:
        return f"{n / 1000:.1f}k"
    return str(n)


def fmt_pct(num: int | None, den: int | None) -> str:
    if not num or not den or den == 0:
        return "  0%"
    pct = round(100 * num / den)
    return f"{pct:>3}%"


def status_pill(status: int, error: str | None) -> str:
    if error or status >= 400:
        return Term.red(f"[{status or 'ERR'}]")
    if 200 <= status < 300:
        return Term.green(f"[{status}]")
    return Term.yellow(f"[{status or '???'}]")


@dataclass
class CallResult:
    label: str
    status: int
    ttft_ms: int | None
    total_ms: int
    input_tokens: int | None
    output_tokens: int | None
    cached_tokens: int | None
    reasoning_tokens: int | None  # subset of output_tokens; populated for reasoning models
    chunks: int
    payload_bytes: int
    model: str                     # model used (for downstream cost lookup)
    cost_usd: float | None = None  # computed, None when model isn't in pricing dict
    error: str | None = None
    # Captured response bodies — populated by `run_call_streaming` when
    # the SSE stream emits `response.output_text.delta` /
    # `response.reasoning_summary_text.delta` events. Used by the
    # post-run quality-review artifact (`responses.jsonl`) and shown
    # inline in the report. Kept off the slim stats path so report
    # rendering doesn't blow up memory on multi-K-token bodies.
    output_text: str | None = None
    reasoning_summary: str | None = None


def _finalise_result(
    *, label: str, model: str, status_code: int, ttft_s: float | None,
    start: float, input_tokens: int | None, output_tokens: int | None,
    cached_tokens: int | None, reasoning_tokens: int | None,
    chunks: int, payload_bytes: int, error_text: str | None,
    pricing: dict[str, tuple[float, float, float]],
    output_text: str | None = None,
    reasoning_summary: str | None = None,
) -> CallResult:
    """Build a CallResult including computed cost (None if model unknown)."""
    return CallResult(
        label=label,
        status=status_code,
        ttft_ms=int(ttft_s * 1000) if ttft_s is not None else None,
        total_ms=int((time.monotonic() - start) * 1000),
        input_tokens=input_tokens,
        output_tokens=output_tokens,
        cached_tokens=cached_tokens,
        reasoning_tokens=reasoning_tokens,
        chunks=chunks,
        payload_bytes=payload_bytes,
        model=model,
        cost_usd=compute_cost_usd(model, input_tokens, cached_tokens, output_tokens, pricing),
        error=error_text,
        output_text=output_text,
        reasoning_summary=reasoning_summary,
    )


def run_call_streaming(
    api_key: str, payload: dict, label: str, *, timeout: int,
    pricing: dict[str, tuple[float, float, float]], verbose: bool = False,
) -> CallResult:
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
    }
    model = payload.get("model", "unknown")
    payload_bytes = len(json.dumps(payload).encode("utf-8"))
    start = time.monotonic()
    Term.live(f"  {label}  {Term.dim('connecting…')}")
    ttft_s: float | None = None
    output_tokens: int | None = None
    input_tokens: int | None = None
    cached: int | None = None
    reasoning_tokens: int | None = None
    chunks = 0
    error_text: str | None = None
    status_code = 0
    # Accumulate response body so the post-run quality-review artifact
    # (`responses.jsonl`) can capture what the model actually said.
    # Lightweight: the delta strings are small per chunk and joined
    # only once at the end.
    body_parts: list[str] = []
    reasoning_parts: list[str] = []

    try:
        with requests.post(
            OPENAI_RESPONSES_URL,
            headers=headers,
            json=payload,
            stream=True,
            timeout=timeout,
        ) as resp:
            status_code = resp.status_code
            if resp.status_code != 200:
                error_text = resp.text[:2000]
                return _finalise_result(
                    label=label, model=model, status_code=status_code,
                    ttft_s=None, start=start,
                    input_tokens=None, output_tokens=None,
                    cached_tokens=None, reasoning_tokens=None,
                    chunks=0, payload_bytes=payload_bytes,
                    error_text=error_text, pricing=pricing,
                )
            for raw in resp.iter_lines(decode_unicode=True):
                if not raw or raw.startswith(":"):
                    continue
                if not raw.startswith("data:"):
                    continue
                data = raw[5:].lstrip()
                if data == "[DONE]":
                    break
                try:
                    evt = json.loads(data)
                except json.JSONDecodeError:
                    continue
                chunks += 1
                etype = evt.get("type", "")
                if etype == "response.output_text.delta":
                    if ttft_s is None:
                        ttft_s = time.monotonic() - start
                    body_parts.append(evt.get("delta") or "")
                elif etype == "response.reasoning_summary_text.delta":
                    reasoning_parts.append(evt.get("delta") or "")
                if etype == "response.completed":
                    usage = (evt.get("response") or {}).get("usage") or {}
                    output_tokens = usage.get("output_tokens")
                    input_tokens = usage.get("input_tokens")
                    in_details = usage.get("input_tokens_details") or {}
                    cached = in_details.get("cached_tokens")
                    out_details = usage.get("output_tokens_details") or {}
                    reasoning_tokens = out_details.get("reasoning_tokens")
                if verbose:
                    print(f"    SSE {etype} ({len(data)} bytes)")
                else:
                    if chunks % 2 == 0 or etype == "response.output_text.delta":
                        elapsed = time.monotonic() - start
                        spinner = Term.spinner_char(chunks)
                        ttft_hint = (
                            f"  ttft={int(ttft_s * 1000)}ms"
                            if ttft_s is not None else ""
                        )
                        Term.live(
                            f"  {label}  {Term.cyan(spinner)} "
                            f"chunks={chunks:>3}  elapsed={elapsed:5.1f}s{ttft_hint}"
                        )
    except requests.RequestException as exc:
        error_text = f"{type(exc).__name__}: {exc}"

    output_text = "".join(body_parts) or None
    reasoning_summary = "".join(reasoning_parts) or None
    return _finalise_result(
        label=label, model=model, status_code=status_code,
        ttft_s=ttft_s, start=start,
        input_tokens=input_tokens, output_tokens=output_tokens,
        cached_tokens=cached, reasoning_tokens=reasoning_tokens,
        chunks=chunks, payload_bytes=payload_bytes,
        error_text=error_text, pricing=pricing,
        output_text=output_text, reasoning_summary=reasoning_summary,
    )


def run_call_non_streaming(
    api_key: str, payload: dict, label: str, *, timeout: int,
    pricing: dict[str, tuple[float, float, float]],
) -> CallResult:
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    model = payload.get("model", "unknown")
    payload_bytes = len(json.dumps(payload).encode("utf-8"))
    start = time.monotonic()
    error_text: str | None = None
    status_code = 0
    body: dict | None = None
    try:
        resp = requests.post(OPENAI_RESPONSES_URL, headers=headers, json=payload, timeout=timeout)
        status_code = resp.status_code
        if resp.status_code != 200:
            error_text = resp.text[:2000]
        else:
            body = resp.json()
    except requests.RequestException as exc:
        error_text = f"{type(exc).__name__}: {exc}"
    usage = (body or {}).get("usage") or {}
    in_details = usage.get("input_tokens_details") or {}
    out_details = usage.get("output_tokens_details") or {}
    # Recover the response text from the non-streaming body so the
    # quality-review artifact still gets populated when the run was
    # forced non-streaming (e.g. --no-stream).
    output_text_parts: list[str] = []
    reasoning_summary_parts: list[str] = []
    for item in (body or {}).get("output") or []:
        if not isinstance(item, dict):
            continue
        itype = item.get("type")
        if itype == "message":
            for c in item.get("content") or []:
                if isinstance(c, dict) and c.get("type") == "output_text":
                    output_text_parts.append(c.get("text") or "")
        elif itype == "reasoning":
            for s in item.get("summary") or []:
                if isinstance(s, dict) and s.get("type") == "summary_text":
                    reasoning_summary_parts.append(s.get("text") or "")
    output_text = "".join(output_text_parts) or None
    reasoning_summary = "".join(reasoning_summary_parts) or None
    return _finalise_result(
        label=label, model=model, status_code=status_code,
        ttft_s=None, start=start,
        input_tokens=usage.get("input_tokens"),
        output_tokens=usage.get("output_tokens"),
        cached_tokens=in_details.get("cached_tokens"),
        reasoning_tokens=out_details.get("reasoning_tokens"),
        chunks=0, payload_bytes=payload_bytes,
        output_text=output_text, reasoning_summary=reasoning_summary,
        error_text=error_text, pricing=pricing,
    )


def run_call(
    api_key: str, payload: dict, label: str, *, timeout: int,
    pricing: dict[str, tuple[float, float, float]], verbose: bool = False,
) -> CallResult:
    if payload.get("stream", False):
        return run_call_streaming(
            api_key, payload, label, timeout=timeout,
            pricing=pricing, verbose=verbose,
        )
    return run_call_non_streaming(
        api_key, payload, label, timeout=timeout, pricing=pricing,
    )


def print_result(r: CallResult) -> None:
    """Finalise any in-flight `Term.live` line with a formatted result."""
    if r.error:
        Term.live_done(
            f"  {Term.red('✗')} {r.label}  {status_pill(r.status, r.error)}  "
            f"{Term.dim('ERROR')}  total={fmt_ms(r.total_ms)}  "
            f"payload={r.payload_bytes:,}b"
        )
        print(f"    {Term.dim(r.error[:400])}")
        return

    pct_str = fmt_pct(r.cached_tokens, r.input_tokens)
    if r.cached_tokens and r.input_tokens and (r.cached_tokens / r.input_tokens) > 0.5:
        pct_str = Term.green(pct_str)
    elif r.cached_tokens and r.input_tokens and (r.cached_tokens / r.input_tokens) > 0.05:
        pct_str = Term.yellow(pct_str)
    else:
        pct_str = Term.dim(pct_str)

    ttft_str = (
        f"ttft={Term.bold(fmt_ms(r.ttft_ms))}"
        if r.ttft_ms is not None else Term.dim("ttft=—")
    )

    cost_str = (
        Term.yellow(fmt_usd(r.cost_usd))
        if r.cost_usd is not None and r.cost_usd >= 0.01
        else fmt_usd(r.cost_usd)
    )

    reasoning_str = (
        f" reason={fmt_tokens(r.reasoning_tokens)}"
        if r.reasoning_tokens else ""
    )

    Term.live_done(
        f"  {Term.green('✓')} {r.label}  {status_pill(r.status, None)}  "
        f"{ttft_str}  total={Term.bold(fmt_ms(r.total_ms))}  "
        f"in={fmt_tokens(r.input_tokens)}  "
        f"cached={fmt_tokens(r.cached_tokens)}({pct_str})  "
        f"out={fmt_tokens(r.output_tokens)}{reasoning_str}  "
        f"cost={cost_str}  "
        f"{Term.dim(f'chunks={r.chunks} payload={r.payload_bytes:,}b')}"
    )


# ---------------------------------------------------------------- scenarios


def scenario_one(
    api_key: str, parts: PromptParts, prof: ProfileConfig, label: str, *,
    stream: bool | None, pricing: dict[str, tuple[float, float, float]],
) -> CallResult:
    payload = build_payload(parts, prof, stream=stream)
    return run_call(api_key, payload, label, timeout=prof.timeout_secs, pricing=pricing)


def scenario_burst(
    api_key: str, parts: PromptParts, prof: ProfileConfig, runs: int, *,
    stream: bool | None, pricing: dict[str, tuple[float, float, float]],
) -> list[CallResult]:
    """N back-to-back identical calls. Runs 2..N expected to hit cache."""
    payload = build_payload(parts, prof, stream=stream)
    results: list[CallResult] = []
    for i in range(runs):
        r = run_call(
            api_key, payload, f"burst[{i + 1}/{runs}]",
            timeout=prof.timeout_secs, pricing=pricing,
        )
        print_result(r)
        results.append(r)
    return results


def scenario_grow(
    api_key: str,
    base_parts: PromptParts,
    prof: ProfileConfig,
    runs: int,
    *,
    stream: bool | None,
    pricing: dict[str, tuple[float, float, float]],
) -> list[CallResult]:
    """Append synthetic turns each call. Prefix should cache; tail grows.

    The grow scenario is intentionally a *flat-text* growth pattern —
    structured `function_call` items would require synthetic call_id
    bookkeeping that adds no information vs. simply extending a flat
    history. The base prompt is therefore re-rendered with
    `structured=False` regardless of how `base_parts` was originally
    built, so the cacheable prefix is the full system + tools + flat
    history (matching pre-v0.4.0 input-token counts and keeping
    cross-version comparisons valid).
    """
    results: list[CallResult] = []
    # Re-flatten the trace history into a single user-message base so
    # we can grow it by string append. Without this, when `base_parts`
    # was built with structured=True (the v0.4.0+ default), its
    # `user_text` is empty and we'd send only the synthetic tail —
    # dropping the captured history out of the request and reducing
    # input tokens from ~24K to ~16K (which masks any real cache
    # behaviour we're trying to observe).
    if base_parts.input_items is not None and not base_parts.user_text:
        # Re-derive the flat text from the structured items. The
        # token count comes out comparable to the legacy flatten.
        flat_parts: list[str] = []
        for it in base_parts.input_items:
            role = it.get("role")
            if role == "user":
                for c in it.get("content", []):
                    if isinstance(c, dict) and c.get("type") == "input_text":
                        flat_parts.append(f"[USER] {c.get('text', '')}")
            elif role == "assistant":
                for c in it.get("content", []):
                    if isinstance(c, dict) and c.get("type") == "output_text":
                        flat_parts.append(f"[ASSISTANT] {c.get('text', '')}")
            elif it.get("type") == "function_call":
                flat_parts.append(f"[TOOL_CALL {it.get('name', '?')}] {it.get('arguments', '{}')}")
            elif it.get("type") == "function_call_output":
                flat_parts.append(f"[TOOL_RESULT] {it.get('output', '')[:8000]}")
        base_text = "\n\n".join(flat_parts)
    else:
        base_text = base_parts.user_text

    accumulated = ""
    for step in range(runs):
        # Add ~250 chars / ~60 tokens of new "tail" per step. Realistic
        # turn shape: assistant text + tool_call + tool_result + new user
        # question.
        accumulated += (
            f"\n\n[ASSISTANT] (step {step}) Acknowledged. Calling tool to fetch data.\n"
            f"[TOOL_CALL fetch_step_{step}] {{\"query\": \"step {step} probe\"}}\n"
            f"[TOOL_RESULT fetch_step_{step}] "
            f"{json.dumps({'step': step, 'rows': list(range(step * 5, step * 5 + 10))})}\n"
            f"[USER] (step {step}) Continue with the next step please."
        )
        # The instructions + tools stay IDENTICAL → that prefix is what
        # caches. The user_text grows. Note that `input_items` is
        # explicitly left unset so build_payload uses the flat layout
        # — the scenario's whole point is to grow ONE user blob.
        parts = PromptParts(
            instructions=base_parts.instructions,
            user_text=base_text + accumulated,
            tools=base_parts.tools,
        )
        payload = build_payload(parts, prof, stream=stream)
        r = run_call(
            api_key, payload, f"grow[{step + 1}/{runs}]",
            timeout=prof.timeout_secs, pricing=pricing,
        )
        print_result(r)
        results.append(r)
    return results


def _run_sub_scenario(
    scenario: str,
    runs: int,
    api_key: str,
    full_parts: PromptParts,
    min_parts: PromptParts,
    prof: ProfileConfig,
    bucket: "dict[tuple[str, str], list[CallResult]]",
    stream: bool | None,
    gap_secs: int,
    pricing: dict[str, tuple[float, float, float]],
) -> None:
    """Dispatch one scenario's worth of calls into the bucket dict."""
    if scenario in ("full", "both"):
        results: list[CallResult] = []
        for i in range(runs):
            r = scenario_one(
                api_key, full_parts, prof,
                f"full[{i + 1}/{runs}]", stream=stream, pricing=pricing,
            )
            print_result(r)
            results.append(r)
        bucket[(prof.name, "FULL")] = results
    if scenario in ("minimal", "both"):
        results = []
        for i in range(runs):
            r = scenario_one(
                api_key, min_parts, prof,
                f"min[{i + 1}/{runs}]", stream=stream, pricing=pricing,
            )
            print_result(r)
            results.append(r)
        bucket[(prof.name, "MINIMAL")] = results
    if scenario == "burst":
        bucket[(prof.name, "BURST")] = scenario_burst(
            api_key, full_parts, prof, runs, stream=stream, pricing=pricing,
        )
    if scenario == "grow":
        bucket[(prof.name, "GROW")] = scenario_grow(
            api_key, full_parts, prof, runs, stream=stream, pricing=pricing,
        )
    if scenario == "cooldown":
        bucket[(prof.name, "COOLDOWN")] = scenario_cooldown(
            api_key, full_parts, prof, runs, gap_secs, stream=stream, pricing=pricing,
        )


def scenario_cooldown(
    api_key: str,
    parts: PromptParts,
    prof: ProfileConfig,
    runs: int,
    gap_secs: int,
    *,
    stream: bool | None,
    pricing: dict[str, tuple[float, float, float]],
) -> list[CallResult]:
    """Send, sleep, send. Useful for TTL measurement."""
    payload = build_payload(parts, prof, stream=stream)
    results: list[CallResult] = []
    for i in range(runs):
        if i > 0:
            _sleep_with_spinner(
                gap_secs,
                f"cooldown gap {i}/{runs - 1} — waiting for cache TTL eviction",
            )
        r = run_call(
            api_key, payload, f"cooldown[{i + 1}/{runs}]",
            timeout=prof.timeout_secs, pricing=pricing,
        )
        print_result(r)
        results.append(r)
    return results


# ---------------------------------------------------------------- summary


def summarise_lines(label: str, results: list[CallResult]) -> list[str]:
    """Build the per-bucket summary as a list of lines (no I/O)."""
    if not results:
        return []
    lines = [f"\n[{label}]"]
    lines.append(f"  HTTP statuses: {[r.status for r in results]}")
    ttfts = [r.ttft_ms for r in results if r.ttft_ms is not None]
    totals = [r.total_ms for r in results if r.total_ms is not None]
    if ttfts:
        lines.append(
            f"  TTFT   avg={sum(ttfts) // len(ttfts)} ms  "
            f"min={min(ttfts)} max={max(ttfts)} ms"
        )
    if totals:
        lines.append(
            f"  Total  avg={sum(totals) // len(totals)} ms  "
            f"min={min(totals)} max={max(totals)} ms"
        )
    ins = [r.input_tokens for r in results if r.input_tokens is not None]
    outs = [r.output_tokens for r in results if r.output_tokens is not None]
    cached = [r.cached_tokens for r in results if r.cached_tokens is not None]
    if ins:
        lines.append(f"  Input tokens   per call: {ins}")
    if outs:
        lines.append(f"  Output tokens  per call: {outs}")
    if cached:
        hits = []
        for r in results:
            if r.input_tokens and r.cached_tokens is not None and r.input_tokens > 0:
                hits.append(round(100 * r.cached_tokens / r.input_tokens))
            else:
                hits.append(None)
        lines.append(f"  Cached tokens  per call: {cached}")
        lines.append(f"  Cache hit %    per call: {hits}")
    reasoning = [r.reasoning_tokens for r in results if r.reasoning_tokens is not None]
    if reasoning:
        lines.append(f"  Reasoning tok  per call: {reasoning}")
        lines.append(
            f"  Reasoning tok  total: {sum(reasoning)}   "
            f"avg: {sum(reasoning) // len(reasoning)}"
        )
    costs = [r.cost_usd for r in results if r.cost_usd is not None]
    if costs:
        total = sum(costs)
        lines.append(
            f"  Cost USD       per call: {[fmt_usd(c) for c in costs]}"
        )
        lines.append(
            f"  Cost USD       total: {fmt_usd(total)}   "
            f"avg: {fmt_usd(total / len(costs))}"
        )
    lines.append(f"  Payload bytes  per call: {[r.payload_bytes for r in results]}")
    return lines


def summarise(label: str, results: list[CallResult]) -> None:
    """Print the per-bucket summary to stdout."""
    for line in summarise_lines(label, results):
        print(line)


# ---------------------------------------------------------------- stats + HTML


@dataclass
class Stats:
    count: int
    mean: float
    median: float
    p95: float
    minimum: float
    maximum: float
    stddev: float

    @classmethod
    def of(cls, values: list[float]) -> "Stats | None":
        if not values:
            return None
        s = sorted(values)
        n = len(s)
        mean = statistics.fmean(s)
        median = statistics.median(s)
        idx95 = max(0, min(n - 1, int(round(0.95 * (n - 1)))))
        p95 = s[idx95]
        stddev = statistics.pstdev(s) if n > 1 else 0.0
        return cls(
            count=n,
            mean=mean,
            median=median,
            p95=p95,
            minimum=s[0],
            maximum=s[-1],
            stddev=stddev,
        )


def bucket_stats(results: list[CallResult]) -> dict:
    return {
        "ttft_ms": Stats.of([r.ttft_ms for r in results if r.ttft_ms is not None]),
        "total_ms": Stats.of([r.total_ms for r in results if r.total_ms is not None]),
        "input_tokens": Stats.of(
            [r.input_tokens for r in results if r.input_tokens is not None]
        ),
        "output_tokens": Stats.of(
            [r.output_tokens for r in results if r.output_tokens is not None]
        ),
        "cached_tokens": Stats.of(
            [r.cached_tokens for r in results if r.cached_tokens is not None]
        ),
        "cache_pct": Stats.of(
            [
                100 * r.cached_tokens / r.input_tokens
                for r in results
                if r.input_tokens and r.cached_tokens is not None and r.input_tokens > 0
            ]
        ),
        "reasoning_tokens": Stats.of(
            [r.reasoning_tokens for r in results if r.reasoning_tokens is not None]
        ),
        "cost_usd": Stats.of(
            [r.cost_usd for r in results if r.cost_usd is not None]
        ),
    }


def stats_to_dict(s: "Stats | None") -> dict | None:
    return asdict(s) if s is not None else None


# Self-contained HTML report. All dynamic content is injected via
# textContent + DOM construction (`el()` helper); the static structural
# template never substitutes user data into innerHTML.
HTML_TEMPLATE = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Magican Perf · LLM Context Latency Report</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;500;600;700&family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;500;600&display=swap">
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
<style>
:root {
  --bg: #07090f;
  --bg-2: #0c1018;
  --panel: rgba(20, 24, 36, 0.72);
  --panel-solid: #141824;
  --panel-2: rgba(28, 33, 48, 0.65);
  --line: rgba(120, 130, 165, 0.16);
  --line-strong: rgba(120, 130, 165, 0.28);
  --text: #e7ecf6;
  --text-dim: #98a2bd;
  --text-mute: #6e7793;
  --accent: #7dd3fc;
  --accent-2: #c4b5fd;
  --accent-3: #f0abfc;
  --ok: #6ee7b7;
  --warn: #fcd34d;
  --err: #fda4af;
  --cache: #c4b5fd;
  --grid: rgba(120, 130, 165, 0.12);
  --shadow: 0 12px 40px -16px rgba(0, 0, 0, 0.55);
  --glow-cyan: 0 0 40px -10px rgba(125, 211, 252, 0.35);
  --glow-violet: 0 0 40px -10px rgba(196, 181, 253, 0.35);
}
* { box-sizing: border-box; }
html, body { background: var(--bg); }
body {
  margin: 0;
  padding: 0;
  font-family: 'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
  color: var(--text);
  line-height: 1.55;
  font-feature-settings: 'cv02','cv03','cv04','cv11';
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  background:
    radial-gradient(1100px 600px at 18% -10%, rgba(125, 211, 252, 0.14), transparent 60%),
    radial-gradient(900px 500px at 92% 10%, rgba(240, 171, 252, 0.10), transparent 65%),
    radial-gradient(1200px 700px at 50% 110%, rgba(196, 181, 253, 0.08), transparent 70%),
    var(--bg);
  background-attachment: fixed;
  min-height: 100vh;
}
.shell { max-width: 1320px; margin: 0 auto; padding: 56px 40px 80px; }
@media (max-width: 720px) {
  .shell { padding: 32px 20px 60px; }
}

.hero {
  position: relative;
  padding: 36px 36px 32px;
  border-radius: 22px;
  margin-bottom: 36px;
  background:
    linear-gradient(135deg, rgba(125, 211, 252, 0.12), rgba(196, 181, 253, 0.10) 45%, rgba(240, 171, 252, 0.10));
  border: 1px solid var(--line);
  box-shadow: var(--shadow), inset 0 1px 0 rgba(255, 255, 255, 0.04);
  overflow: hidden;
}
.hero::before {
  content: "";
  position: absolute; inset: 0;
  background: radial-gradient(600px 200px at 8% 0%, rgba(125, 211, 252, 0.22), transparent 65%);
  pointer-events: none;
}
.hero::after {
  content: "";
  position: absolute; inset: 0;
  background: radial-gradient(500px 200px at 100% 100%, rgba(240, 171, 252, 0.18), transparent 65%);
  pointer-events: none;
}
.hero-inner { position: relative; z-index: 1; }
.hero-eyebrow {
  display: inline-flex; align-items: center; gap: 8px;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  font-size: 0.72rem;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--accent);
  padding: 5px 11px;
  border-radius: 999px;
  background: rgba(125, 211, 252, 0.10);
  border: 1px solid rgba(125, 211, 252, 0.22);
  margin-bottom: 18px;
}
.hero-eyebrow .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 12px var(--accent); }
.hero h1 {
  margin: 0 0 8px;
  font-family: 'Space Grotesk', sans-serif;
  font-size: clamp(2rem, 4.2vw, 3rem);
  font-weight: 600;
  letter-spacing: -0.025em;
  background: linear-gradient(120deg, #ffffff 0%, #b0d9ff 35%, #d8c9ff 65%, #f7c4f4 100%);
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  line-height: 1.05;
}
.hero h1 .accent { color: var(--accent-3); -webkit-text-fill-color: var(--accent-3); }
.hero-sub { color: var(--text-dim); font-size: 1rem; max-width: 780px; margin: 0; }
.meta-bar {
  display: flex; flex-wrap: wrap; gap: 18px 24px;
  color: var(--text-dim); font-size: 0.88rem;
  margin-top: 22px;
  padding-top: 18px;
  border-top: 1px solid rgba(255, 255, 255, 0.06);
  font-family: 'JetBrains Mono', ui-monospace, monospace;
}
.meta-bar code { background: rgba(255,255,255,0.04); padding: 2px 8px; border-radius: 6px; color: var(--text); border: 1px solid var(--line); }
.meta-bar strong { color: var(--text); font-weight: 500; }
.meta-bar .meta-grand { color: var(--accent-2); }
.meta-bar .meta-grand strong { color: var(--accent-3); font-weight: 600; }

h2 {
  margin: 44px 0 8px;
  font-family: 'Space Grotesk', sans-serif;
  font-size: 1.35rem;
  font-weight: 600;
  letter-spacing: -0.015em;
  color: var(--text);
  display: flex; align-items: baseline; gap: 12px;
}
h2::before {
  content: "";
  width: 8px; height: 8px; border-radius: 2px;
  background: linear-gradient(135deg, var(--accent), var(--accent-3));
  box-shadow: 0 0 16px rgba(125, 211, 252, 0.45);
  transform: translateY(-1px);
}
h3 {
  margin: 22px 0 10px;
  font-family: 'Space Grotesk', sans-serif;
  font-size: 1.0rem;
  font-weight: 600;
  color: var(--text);
  letter-spacing: -0.005em;
}

.section-note {
  color: var(--text-dim);
  font-size: 0.92rem;
  margin: 0 0 18px;
  max-width: 880px;
}

.grid { display: grid; gap: 18px; }
.cols-2 { grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); }
.cols-3 { grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); }

.card {
  position: relative;
  background: var(--panel);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 20px;
  box-shadow: var(--shadow);
  transition: border-color 0.18s ease, transform 0.18s ease;
}
.card:hover { border-color: var(--line-strong); }
.card h3 { margin-top: 0; }

.kv { display: grid; grid-template-columns: max-content 1fr; gap: 6px 18px; font-size: 0.88rem; }
.kv dt { color: var(--text-dim); font-family: 'JetBrains Mono', ui-monospace, monospace; font-size: 0.78rem; align-self: center; }
.kv dd { margin: 0; color: var(--text); font-variant-numeric: tabular-nums; word-break: break-all; font-family: 'JetBrains Mono', ui-monospace, monospace; font-size: 0.85rem; }

.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(118px, 1fr)); gap: 10px; margin: 14px 0 6px; }
.stat {
  background: linear-gradient(180deg, rgba(255,255,255,0.025), rgba(255,255,255,0));
  padding: 10px 14px;
  border-radius: 10px;
  border: 1px solid var(--line);
  transition: border-color 0.18s ease;
}
.stat:hover { border-color: var(--line-strong); }
.stat-label {
  color: var(--text-mute);
  font-size: 0.66rem;
  text-transform: uppercase;
  letter-spacing: 0.10em;
  font-weight: 500;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
}
.stat-value {
  margin-top: 4px;
  font-size: 1.18rem;
  font-weight: 600;
  color: var(--text);
  font-variant-numeric: tabular-nums;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  letter-spacing: -0.01em;
}
.stat-value .dim { color: var(--text-mute); font-size: 0.82rem; font-weight: 400; margin-left: 2px; }

table {
  width: 100%;
  border-collapse: separate;
  border-spacing: 0;
  font-variant-numeric: tabular-nums;
  font-size: 0.85rem;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
}
th, td { padding: 10px 12px; text-align: right; border-bottom: 1px solid var(--line); }
th {
  color: var(--text-mute);
  font-weight: 500;
  text-transform: uppercase;
  font-size: 0.66rem;
  letter-spacing: 0.10em;
  background: rgba(255,255,255,0.015);
}
td.label, th.label { text-align: left; }
tbody tr { transition: background 0.12s ease; }
tbody tr:hover td { background: rgba(125, 211, 252, 0.05); }
tbody tr:last-child td { border-bottom: none; }

.pill {
  display: inline-block;
  padding: 3px 11px;
  border-radius: 999px;
  font-size: 0.7rem;
  font-weight: 600;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  letter-spacing: 0.04em;
}
.pill-ok   { background: rgba(110, 231, 183, 0.12); color: var(--ok);   border: 1px solid rgba(110, 231, 183, 0.22); }
.pill-err  { background: rgba(253, 164, 175, 0.12); color: var(--err);  border: 1px solid rgba(253, 164, 175, 0.22); }
.pill-mute { background: rgba(139, 148, 158, 0.10); color: var(--text-dim); border: 1px solid var(--line); }

.chart-wrap {
  background: var(--panel);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 18px 20px 14px;
  box-shadow: var(--shadow);
}
.chart-wrap canvas { height: 280px !important; max-height: 320px; }
.chart-wrap h3 {
  margin: 0 0 12px;
  font-size: 0.92rem;
  color: var(--text-dim);
  font-weight: 500;
  letter-spacing: 0.005em;
  font-family: 'Inter', sans-serif;
}

details {
  background: var(--panel);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 14px 20px;
  margin: 20px 0;
  box-shadow: var(--shadow);
}
details summary {
  cursor: pointer;
  color: var(--text);
  font-weight: 500;
  font-family: 'Space Grotesk', sans-serif;
  list-style: none;
  display: flex; align-items: center; gap: 8px;
}
details summary::-webkit-details-marker { display: none; }
details summary::before {
  content: "+";
  width: 18px; height: 18px;
  display: inline-flex; align-items: center; justify-content: center;
  border-radius: 4px;
  background: rgba(125, 211, 252, 0.12);
  color: var(--accent);
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.85rem;
  font-weight: 600;
  transition: transform 0.18s ease;
}
details[open] summary::before { content: "−"; }

pre {
  background: var(--bg-2);
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 14px 16px;
  margin-top: 12px;
  overflow: auto;
  font-size: 0.78rem;
  font-family: 'JetBrains Mono', ui-monospace, monospace;
  color: var(--text-dim);
  max-height: 420px;
  line-height: 1.5;
}

.matrix-table th, .matrix-table td { text-align: center; }
.matrix-cell-ran {
  background: linear-gradient(180deg, rgba(110, 231, 183, 0.10), rgba(110, 231, 183, 0.05));
  color: var(--ok);
}
.matrix-cell-skip { color: var(--text-mute); }

.bucket-card { margin-bottom: 22px; }
.bucket-card h3 {
  display: flex; align-items: center; gap: 10px;
  font-size: 1.05rem;
}
.bucket-card .bucket-title-name { color: var(--text); }
.bucket-card .bucket-title-sep { color: var(--text-mute); font-weight: 400; }
.bucket-card .bucket-title-scenario { color: var(--accent-2); }

/* AI Insights panel — populated post-measurement by the thinking model */
.insights-panel {
  margin-bottom: 24px;
  padding: 20px 22px 4px;
  background:
    linear-gradient(135deg, rgba(196, 181, 253, 0.10), rgba(125, 211, 252, 0.06));
  border: 1px solid var(--line-strong);
  border-radius: 14px;
  box-shadow: var(--shadow);
}
.insights-panel .insights-eyebrow {
  display: inline-flex; align-items: center; gap: 8px;
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.7rem; letter-spacing: 0.18em; text-transform: uppercase;
  color: var(--accent-3); margin-bottom: 6px;
}
.insights-panel .insights-eyebrow .dot {
  width: 6px; height: 6px; border-radius: 50%;
  background: var(--accent-3); box-shadow: 0 0 12px var(--accent-3);
}
.insights-panel > h2 {
  margin: 4px 0 8px;
  font-family: 'Space Grotesk', sans-serif;
  font-size: 1.45rem; font-weight: 600; letter-spacing: -0.02em;
  background: linear-gradient(120deg, #ffffff 0%, #d8c9ff 60%, #f7c4f4 100%);
  -webkit-background-clip: text; background-clip: text; color: transparent;
}
.insights-panel > h2::before { display: none; }
.insights-panel .insights-sub {
  color: var(--text-dim); font-size: 0.9rem; margin: 0 0 18px;
}
.insights-body h3 {
  margin: 22px 0 8px;
  font-family: 'Space Grotesk', sans-serif;
  font-size: 1.02rem; font-weight: 600;
  color: var(--text); letter-spacing: -0.005em;
}
.insights-body p { margin: 6px 0 10px; line-height: 1.65; }
.insights-body strong { color: #fff; font-weight: 600; }
.insights-body em { color: var(--text-dim); }
.insights-body code {
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.82em;
  padding: 1px 6px; border-radius: 4px;
  background: rgba(125, 211, 252, 0.10);
  color: var(--accent);
  border: 1px solid rgba(125, 211, 252, 0.18);
}
.insights-body ul { margin: 6px 0 12px 0; padding-left: 22px; }
.insights-body ul li { margin: 4px 0; line-height: 1.55; }
.insights-body .chip {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 2px 9px; margin: 0 2px;
  border-radius: 999px;
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.74rem; font-weight: 500;
  border: 1px solid var(--line-strong);
  cursor: pointer;
  vertical-align: baseline;
  transition: background 0.14s ease, border-color 0.14s ease, transform 0.12s ease;
  text-decoration: none;
}
.insights-body .chip:hover { transform: translateY(-1px); }
.insights-body .chip-chart {
  background: rgba(125, 211, 252, 0.12);
  border-color: rgba(125, 211, 252, 0.30);
  color: var(--accent);
}
.insights-body .chip-chart:hover { background: rgba(125, 211, 252, 0.22); }
.insights-body .chip-bucket {
  background: rgba(196, 181, 253, 0.12);
  border-color: rgba(196, 181, 253, 0.30);
  color: var(--accent-2);
}
.insights-body .chip-bucket:hover { background: rgba(196, 181, 253, 0.22); }
.insights-body .chip-metric {
  background: rgba(110, 231, 183, 0.10);
  border-color: rgba(110, 231, 183, 0.26);
  color: var(--ok);
}
.insights-body .chip-metric:hover { background: rgba(110, 231, 183, 0.20); }
.insights-body .chip-icon {
  font-size: 0.78em; opacity: 0.75; line-height: 1;
}
/* Flash effect when a chip scrolls to its target */
@keyframes insights-target-flash {
  0%   { box-shadow: 0 0 0 0 rgba(125, 211, 252, 0.0); }
  20%  { box-shadow: 0 0 0 4px rgba(125, 211, 252, 0.45); }
  100% { box-shadow: 0 0 0 0 rgba(125, 211, 252, 0.0); }
}
.insights-target-flash {
  animation: insights-target-flash 1.6s ease-out;
  border-color: rgba(125, 211, 252, 0.55) !important;
}
</style>
</head>
<body>
<div class="shell">

<header class="hero">
  <div class="hero-inner">
    <div class="hero-eyebrow"><span class="dot"></span><span>Magican Perf · LLM Latency</span></div>
    <h1>Context Latency <span class="accent">Report</span></h1>
    <p class="hero-sub">Side-by-side measurements of TTFT, total latency, prefix-cache behavior, token usage, and cost across profiles and traffic scenarios.</p>
    <p class="meta-bar" id="meta-bar"></p>
  </div>
</header>

<section class="insights-panel" id="insights-panel" hidden>
  <div class="insights-eyebrow"><span class="dot"></span><span>AI Insights · thinking pass</span></div>
  <h2 id="insights-heading">Run analysis</h2>
  <p class="insights-sub" id="insights-sub">Quantitative analysis derived by sending the run's stats back through the thinking profile. Click any chip to jump to the cited chart or bucket.</p>
  <div class="insights-body" id="insights-body"></div>
</section>

<h2>Profiles tested</h2>
<div class="grid cols-2" id="profile-cards"></div>

<h2>Test matrix</h2>
<p class="section-note">Which (profile × scenario) combinations were exercised in this run.</p>
<div class="card"><table class="matrix-table" id="matrix-table"></table></div>

<h2>Per-bucket results</h2>
<p class="section-note">Each bucket = one (profile, scenario). Stats: <strong>median / mean</strong> (smaller stat-value text shows the mean alongside the bold median).</p>
<div id="buckets"></div>

<h2>Cross-bucket comparison</h2>
<p class="section-note">Side-by-side means across every bucket. Lower is better for TTFT / Total; higher is better for cache hit % (under burst/grow).</p>
<div class="grid cols-2">
  <div class="chart-wrap"><h3>Mean TTFT (ms)</h3><canvas id="chart-ttft-mean"></canvas></div>
  <div class="chart-wrap"><h3>Mean total (ms)</h3><canvas id="chart-total-mean"></canvas></div>
  <div class="chart-wrap"><h3>Mean input tokens</h3><canvas id="chart-input-mean"></canvas></div>
  <div class="chart-wrap"><h3>Mean cache hit %</h3><canvas id="chart-cache-mean"></canvas></div>
  <div class="chart-wrap"><h3>Total cost USD per bucket</h3><canvas id="chart-cost-total"></canvas></div>
  <div class="chart-wrap"><h3>Mean output + reasoning tokens</h3><canvas id="chart-out-mean"></canvas></div>
</div>

<details>
  <summary>Raw data (JSON)</summary>
  <pre id="raw-json"></pre>
</details>

</div><!-- /.shell -->

<script>
const REPORT = __REPORT_JSON__;
// Bucket palette — cyan / violet / mint / amber / pink / sky, all
// tuned to read well on the deep-indigo background.
const BUCKET_COLORS = ['#7dd3fc', '#c4b5fd', '#6ee7b7', '#fcd34d', '#f0abfc', '#a5b4fc'];
const CHART_TICK_COLOR = '#98a2bd';
const CHART_GRID_COLOR = 'rgba(120, 130, 165, 0.14)';
const CHART_LABEL_COLOR = '#e7ecf6';
const CHART_FONT = "Inter, -apple-system, sans-serif";

// el(tag, attrs, ...children) — small DOM builder.
// Children are strings (auto-text-noded) or DOM nodes.
function el(tag, attrs, ...children) {
  const node = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null) continue;
      if (k === 'class') node.className = v;
      else if (k === 'style') node.style.cssText = v;
      else node.setAttribute(k, v);
    }
  }
  for (const c of children) {
    if (c == null) continue;
    if (typeof c === 'string' || typeof c === 'number') {
      node.appendChild(document.createTextNode(String(c)));
    } else {
      node.appendChild(c);
    }
  }
  return node;
}
function pill(klass, text) { return el('span', {class: 'pill ' + klass}, text); }
function fmt(n, digits) { return n == null ? '–' : Number(n).toLocaleString(undefined, { maximumFractionDigits: digits ?? 0 }); }
function statBox(label, s, digits) {
  if (!s) return null;
  return el('div', {class: 'stat'},
    el('div', {class: 'stat-label'}, label),
    el('div', {class: 'stat-value'},
      fmt(s.median, digits),
      el('span', {class: 'dim'}, ' / ' + fmt(s.mean, digits)),
    ),
  );
}
function kv(dl, key, value) {
  dl.appendChild(el('dt', null, key));
  if (typeof value === 'string' || value == null) {
    dl.appendChild(el('dd', null, value == null ? '–' : value));
  } else {
    const dd = el('dd');
    dd.appendChild(value);
    dl.appendChild(dd);
  }
}

// ---------- meta bar ----------
// Each meta item is a flex child — labels are dim, values are bright.
const meta = document.getElementById('meta-bar');
function metaItem(label, valueNode) {
  const span = el('span');
  span.appendChild(document.createTextNode(label + ' '));
  if (valueNode == null) {
    span.appendChild(el('strong', null, '–'));
  } else if (typeof valueNode === 'string') {
    span.appendChild(el('strong', null, valueNode));
  } else {
    span.appendChild(valueNode);
  }
  return span;
}
meta.appendChild(metaItem('generated', REPORT.generated_at));
meta.appendChild(metaItem('trace', el('code', null, REPORT.trace_path.split('/').pop())));
meta.appendChild(metaItem('scenario', REPORT.scenario));
meta.appendChild(metaItem('runs/scenario', String(REPORT.runs)));

// Grand total cost across all buckets — most useful single number for "was this run cheap or expensive?"
let GRAND_TOTAL_COST = 0;
let GRAND_TOTAL_CALLS = 0;
Object.values(REPORT.buckets).forEach(b => {
  b.results.forEach(r => {
    if (r.cost_usd != null) GRAND_TOTAL_COST += r.cost_usd;
    GRAND_TOTAL_CALLS += 1;
  });
});
if (GRAND_TOTAL_COST > 0) {
  const grand = el('span', {class: 'meta-grand'});
  grand.appendChild(document.createTextNode('grand total '));
  grand.appendChild(el('strong', null, '$' + GRAND_TOTAL_COST.toFixed(GRAND_TOTAL_COST < 0.01 ? 4 : 3)));
  grand.appendChild(document.createTextNode(' / ' + GRAND_TOTAL_CALLS + ' calls'));
  meta.appendChild(grand);
}

// ---------- profile cards ----------
const profCardsEl = document.getElementById('profile-cards');
REPORT.profiles.forEach(p => {
  const dl = el('dl', {class: 'kv'});
  kv(dl, 'model', p.model);
  kv(dl, 'max_output_tokens', fmt(p.max_output_tokens));
  kv(dl, 'streaming', String(p.streaming));
  kv(dl, 'supports_reasoning', String(p.supports_reasoning));
  kv(dl, 'reasoning.effort',
    p.reasoning_effort ?? pill('pill-mute', 'none (explicit)'));
  if (p.reasoning_max_tokens) {
    kv(dl, 'reasoning.max_tokens', fmt(p.reasoning_max_tokens));
  }
  kv(dl, 'tool_choice', p.tool_choice);
  kv(dl, 'timeout_secs', String(p.timeout_secs));
  profCardsEl.appendChild(el('div', {class: 'card'},
    el('h3', null, p.name),
    dl,
  ));
});

// ---------- matrix table ----------
// Each cell shows: "N runs · $cost" so the matrix doubles as a cost
// breakdown at a glance. Profile row total at the right edge; scenario
// column total at the bottom edge.
const scenarios = REPORT.scenarios_run;
const profileNames = REPORT.profiles.map(p => p.name);
const matrix = document.getElementById('matrix-table');
const thead = el('thead');
const headRow = el('tr', null, el('th', {class: 'label'}, 'profile'));
scenarios.forEach(s => headRow.appendChild(el('th', null, s)));
headRow.appendChild(el('th', null, 'row total'));
thead.appendChild(headRow);
matrix.appendChild(thead);
const tbody = el('tbody');

function bucketCost(b) {
  return b ? b.results.reduce((a, r) => a + (r.cost_usd != null ? r.cost_usd : 0), 0) : 0;
}
function fmtCost(c) {
  if (c <= 0) return '';
  return ' · $' + (c < 0.01 ? c.toFixed(4) : c.toFixed(3));
}

const scenarioColumnTotals = scenarios.map(() => 0);
profileNames.forEach(name => {
  const tr = el('tr', null, el('td', {class: 'label'}, name));
  let rowTotal = 0;
  scenarios.forEach((s, i) => {
    const bucket = REPORT.buckets[name + '__' + s];
    if (bucket) {
      const c = bucketCost(bucket);
      rowTotal += c;
      scenarioColumnTotals[i] += c;
      tr.appendChild(el('td', {class: 'matrix-cell-ran'},
        bucket.results.length + (bucket.results.length === 1 ? ' run' : ' runs') + fmtCost(c)));
    } else {
      tr.appendChild(el('td', {class: 'matrix-cell-skip'}, '—'));
    }
  });
  tr.appendChild(el('td', {class: 'matrix-cell-ran'},
    rowTotal > 0 ? '$' + (rowTotal < 0.01 ? rowTotal.toFixed(4) : rowTotal.toFixed(3)) : '—'));
  tbody.appendChild(tr);
});

// Footer row with column totals
const footRow = el('tr');
footRow.appendChild(el('td', {class: 'label'}, el('strong', null, 'column total')));
let grandFromMatrix = 0;
scenarioColumnTotals.forEach(c => {
  grandFromMatrix += c;
  footRow.appendChild(el('td', {class: 'matrix-cell-ran'},
    c > 0 ? '$' + (c < 0.01 ? c.toFixed(4) : c.toFixed(3)) : '—'));
});
footRow.appendChild(el('td', {class: 'matrix-cell-ran'},
  el('strong', null,
    grandFromMatrix > 0
      ? '$' + (grandFromMatrix < 0.01 ? grandFromMatrix.toFixed(4) : grandFromMatrix.toFixed(3))
      : '—')));
tbody.appendChild(footRow);
matrix.appendChild(tbody);

// ---------- per-bucket cards ----------
const bucketsEl = document.getElementById('buckets');
Object.entries(REPORT.buckets).forEach(([key, bucket], bIdx) => {
  const stats = bucket.stats;
  const card = el('div', {class: 'card bucket-card'});
  const errs = bucket.results.filter(r => r.error || (r.status && r.status >= 400)).length;
  const statusPill = errs === 0
    ? pill('pill-ok', bucket.results.length + ' OK')
    : pill('pill-err', errs + '/' + bucket.results.length + ' ERROR');

  const header = el('h3');
  header.appendChild(el('span', {class: 'bucket-title-name'}, bucket.profile));
  header.appendChild(el('span', {class: 'bucket-title-sep'}, '·'));
  header.appendChild(el('span', {class: 'bucket-title-scenario'}, bucket.scenario));
  header.appendChild(statusPill);
  card.appendChild(header);

  // Total cost across this bucket (sum of per-call costs).
  const totalCost = bucket.results.reduce(
    (a, r) => a + (r.cost_usd != null ? r.cost_usd : 0), 0,
  );
  const anyCost = bucket.results.some(r => r.cost_usd != null);

  const statsRow = el('div', {class: 'stats'});
  const statBoxes = [
    statBox('TTFT ms', stats.ttft_ms),
    statBox('Total ms', stats.total_ms),
    statBox('Input tok', stats.input_tokens),
    statBox('Cached tok', stats.cached_tokens),
    statBox('Cache %', stats.cache_pct, 1),
    statBox('Output tok', stats.output_tokens),
    statBox('Reasoning tok', stats.reasoning_tokens),
  ];
  if (anyCost) {
    // Bucket total cost stat (sum, not median/mean) — most informative.
    const box = el('div', {class: 'stat'},
      el('div', {class: 'stat-label'}, 'Cost USD (total)'),
      el('div', {class: 'stat-value'},
        '$' + totalCost.toFixed(totalCost < 0.01 ? 4 : 3),
        el('span', {class: 'dim'}, ' / call $' + (totalCost / bucket.results.length).toFixed(4)),
      ),
    );
    statBoxes.push(box);
  }
  statBoxes.forEach(s => { if (s) statsRow.appendChild(s); });
  card.appendChild(statsRow);

  // Per-call table
  const table = el('table');
  const tHead = el('thead');
  const tHeadRow = el('tr');
  ['#', 'HTTP', 'TTFT ms', 'Total ms', 'Input', 'Cached', 'Hit %', 'Output', 'Reason', 'Cost', 'Payload']
    .forEach((h, i) => tHeadRow.appendChild(el('th', {class: i === 0 ? 'label' : ''}, h)));
  tHead.appendChild(tHeadRow);
  table.appendChild(tHead);

  const tBody = el('tbody');
  bucket.results.forEach((r, i) => {
    const pct = (r.input_tokens && r.cached_tokens != null && r.input_tokens > 0)
      ? (100 * r.cached_tokens / r.input_tokens).toFixed(1) : '–';
    const costStr = r.cost_usd != null
      ? '$' + (r.cost_usd < 0.01 ? r.cost_usd.toFixed(4) : r.cost_usd.toFixed(3))
      : '–';
    const sp = r.error
      ? pill('pill-err', String(r.status || 'ERR'))
      : (r.status >= 200 && r.status < 300
        ? pill('pill-ok', String(r.status))
        : pill('pill-err', String(r.status || '–')));
    const tr = el('tr');
    tr.appendChild(el('td', {class: 'label'}, String(i + 1)));
    const tdStatus = el('td');
    tdStatus.appendChild(sp);
    tr.appendChild(tdStatus);
    tr.appendChild(el('td', null, fmt(r.ttft_ms)));
    tr.appendChild(el('td', null, fmt(r.total_ms)));
    tr.appendChild(el('td', null, fmt(r.input_tokens)));
    tr.appendChild(el('td', null, fmt(r.cached_tokens)));
    tr.appendChild(el('td', null, pct));
    tr.appendChild(el('td', null, fmt(r.output_tokens)));
    tr.appendChild(el('td', null, fmt(r.reasoning_tokens)));
    tr.appendChild(el('td', null, costStr));
    tr.appendChild(el('td', null, fmt(r.payload_bytes) + ' B'));
    tBody.appendChild(tr);
  });
  table.appendChild(tBody);
  card.appendChild(table);

  // Per-call charts
  const cv1id = 'chart-bucket-' + bIdx + '-perf';
  const cv2id = 'chart-bucket-' + bIdx + '-tok';
  const cv1Wrap = el('div', {class: 'chart-wrap'}, el('h3', null, 'TTFT & total per call'));
  const cv1 = el('canvas', {id: cv1id});
  cv1Wrap.appendChild(cv1);
  const cv2Wrap = el('div', {class: 'chart-wrap'}, el('h3', null, 'Input tokens & cached per call'));
  const cv2 = el('canvas', {id: cv2id});
  cv2Wrap.appendChild(cv2);
  card.appendChild(el('div', {class: 'grid cols-2', style: 'margin-top: 16px;'}, cv1Wrap, cv2Wrap));

  bucketsEl.appendChild(card);

  const callLabels = bucket.results.map((_, i) => '#' + (i + 1));
  const chartScale = {
    plugins: {
      legend: { labels: { color: CHART_LABEL_COLOR, font: { family: CHART_FONT, size: 11 } } },
      tooltip: { backgroundColor: 'rgba(7, 9, 15, 0.92)', borderColor: 'rgba(125, 211, 252, 0.25)', borderWidth: 1, titleColor: CHART_LABEL_COLOR, bodyColor: CHART_LABEL_COLOR, padding: 10, cornerRadius: 8 },
    },
    scales: {
      x: { ticks: { color: CHART_TICK_COLOR, font: { family: CHART_FONT } }, grid: { color: CHART_GRID_COLOR, drawBorder: false } },
      y: { ticks: { color: CHART_TICK_COLOR, font: { family: CHART_FONT } }, grid: { color: CHART_GRID_COLOR, drawBorder: false }, beginAtZero: true },
    },
    responsive: true,
    maintainAspectRatio: false,
  };
  new Chart(cv1, { type: 'bar', data: { labels: callLabels, datasets: [
    { label: 'TTFT (ms)', data: bucket.results.map(r => r.ttft_ms), backgroundColor: '#7dd3fc', borderRadius: 4 },
    { label: 'Total (ms)', data: bucket.results.map(r => r.total_ms), backgroundColor: '#c4b5fd', borderRadius: 4 },
  ]}, options: chartScale });
  new Chart(cv2, { type: 'bar', data: { labels: callLabels, datasets: [
    { label: 'Input tokens', data: bucket.results.map(r => r.input_tokens), backgroundColor: '#6ee7b7', borderRadius: 4 },
    { label: 'Cached tokens', data: bucket.results.map(r => r.cached_tokens), backgroundColor: '#c4b5fd', borderRadius: 4 },
  ]}, options: chartScale });
});

// ---------- cross-bucket comparison ----------
const bucketKeys = Object.keys(REPORT.buckets);
const bucketLabels = bucketKeys.map(k => REPORT.buckets[k].profile + '\n' + REPORT.buckets[k].scenario);
const colors = bucketKeys.map((_, i) => BUCKET_COLORS[i % BUCKET_COLORS.length]);
function pluckMean(metric) {
  return bucketKeys.map(k => {
    const s = REPORT.buckets[k].stats[metric];
    return s ? s.mean : null;
  });
}
const compareOpts = {
  plugins: {
    legend: { display: false },
    tooltip: { backgroundColor: 'rgba(7, 9, 15, 0.92)', borderColor: 'rgba(125, 211, 252, 0.25)', borderWidth: 1, titleColor: CHART_LABEL_COLOR, bodyColor: CHART_LABEL_COLOR, padding: 10, cornerRadius: 8 },
  },
  scales: {
    x: { ticks: { color: CHART_TICK_COLOR, font: { family: CHART_FONT, size: 10 } }, grid: { color: CHART_GRID_COLOR, drawBorder: false } },
    y: { ticks: { color: CHART_TICK_COLOR, font: { family: CHART_FONT } }, grid: { color: CHART_GRID_COLOR, drawBorder: false }, beginAtZero: true },
  },
  responsive: true,
  maintainAspectRatio: false,
};
const compareOptsLegend = {
  ...compareOpts,
  plugins: {
    ...compareOpts.plugins,
    legend: { display: true, labels: { color: CHART_LABEL_COLOR, font: { family: CHART_FONT, size: 11 } } },
  },
};
new Chart(document.getElementById('chart-ttft-mean'), {
  type: 'bar', data: { labels: bucketLabels, datasets: [{ label: 'Mean TTFT (ms)', data: pluckMean('ttft_ms'), backgroundColor: colors, borderRadius: 6 }] }, options: compareOpts,
});
new Chart(document.getElementById('chart-total-mean'), {
  type: 'bar', data: { labels: bucketLabels, datasets: [{ label: 'Mean total (ms)', data: pluckMean('total_ms'), backgroundColor: colors, borderRadius: 6 }] }, options: compareOpts,
});
new Chart(document.getElementById('chart-input-mean'), {
  type: 'bar', data: { labels: bucketLabels, datasets: [{ label: 'Mean input tokens', data: pluckMean('input_tokens'), backgroundColor: colors, borderRadius: 6 }] }, options: compareOpts,
});
new Chart(document.getElementById('chart-cache-mean'), {
  type: 'bar', data: { labels: bucketLabels, datasets: [{ label: 'Mean cache hit %', data: pluckMean('cache_pct'), backgroundColor: colors, borderRadius: 6 }] }, options: compareOpts,
});
// Total cost per bucket = sum of per-call costs.
const totalCostByBucket = bucketKeys.map(k => REPORT.buckets[k].results
  .reduce((a, r) => a + (r.cost_usd != null ? r.cost_usd : 0), 0));
new Chart(document.getElementById('chart-cost-total'), {
  type: 'bar', data: { labels: bucketLabels, datasets: [{ label: 'Total cost USD', data: totalCostByBucket, backgroundColor: colors, borderRadius: 6 }] }, options: compareOpts,
});
new Chart(document.getElementById('chart-out-mean'), {
  type: 'bar', data: { labels: bucketLabels, datasets: [
    { label: 'Output tokens (mean)', data: pluckMean('output_tokens'), backgroundColor: '#6ee7b7', borderRadius: 6 },
    { label: 'Reasoning tokens (mean)', data: pluckMean('reasoning_tokens'), backgroundColor: '#fcd34d', borderRadius: 6 },
  ]}, options: compareOptsLegend,
});

document.getElementById('raw-json').textContent = JSON.stringify(REPORT, null, 2);

// ---------- AI Insights panel ----------
// Renders the markdown body produced by the post-measurement thinking
// pass into the panel at the top of the report. We intentionally use
// a tiny in-page renderer instead of pulling in a markdown library:
// the citation syntax (`[chart:...]` / `[bucket:...]` / `[metric:...]`)
// needs custom handling anyway, the markdown subset the model emits
// is small (h3, p, strong, em, code, ul/li), and the report stays
// fully self-contained.
function renderInsightsPanel() {
  const md = REPORT.insights_md;
  const panel = document.getElementById('insights-panel');
  if (!md || !panel) return;
  panel.hidden = false;

  // Bucket-name -> bucket-index lookup (for chip target resolution).
  const bucketKeys = Object.keys(REPORT.buckets);
  const bucketIndexByKey = Object.fromEntries(
    bucketKeys.map((k, i) => [k, i])
  );
  function bucketRefToIndex(ref) {
    // ref shape: "<profile>/<scenario>" - match against keys.
    const slash = ref.lastIndexOf('/');
    if (slash <= 0) return null;
    const profile = ref.slice(0, slash);
    const scenario = ref.slice(slash + 1);
    const key = profile + '__' + scenario;
    if (key in bucketIndexByKey) return bucketIndexByKey[key];
    // Fallback: case-insensitive scenario match.
    const lower = scenario.toLowerCase();
    for (const k of bucketKeys) {
      if (k.startsWith(profile + '__') && k.slice(profile.length + 2).toLowerCase() === lower) {
        return bucketIndexByKey[k];
      }
    }
    return null;
  }

  // Tokenise inline markdown into text + chip nodes.
  // Supported chips: [chart:id], [bucket:p/s], [metric:p/s/field=value]
  // Supported inline: **bold**, *italic*, `code`
  const CITATION_RE = /\[(chart|bucket|metric):([^\]]+)\]/g;
  const INLINE_RE = /(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`)/g;

  function flashTarget(el) {
    if (!el) return;
    el.classList.remove('insights-target-flash');
    void el.offsetWidth; // restart animation
    el.classList.add('insights-target-flash');
  }

  function makeChip(kind, value) {
    const a = document.createElement('a');
    a.className = 'chip chip-' + kind;
    a.href = '#';
    let icon = '';
    let label = value;
    let target = null;
    if (kind === 'chart') {
      icon = '📊';
      target = document.getElementById(value);
      a.title = 'chart: ' + value;
    } else if (kind === 'bucket') {
      icon = '🪣';
      const idx = bucketRefToIndex(value);
      if (idx != null) {
        target = document.getElementById('chart-bucket-' + idx + '-perf');
        if (target) target = target.closest('.card');
      }
      a.title = 'bucket: ' + value;
    } else if (kind === 'metric') {
      icon = '·';
      // metric value renders inline; show value verbatim, no scroll.
      const eq = value.indexOf('=');
      label = eq > 0 ? value.slice(eq + 1) : value;
      a.title = 'metric: ' + value;
    }
    if (icon) {
      const ic = document.createElement('span');
      ic.className = 'chip-icon';
      ic.textContent = icon;
      a.appendChild(ic);
    }
    a.appendChild(document.createTextNode(label));
    if (target) {
      a.addEventListener('click', (ev) => {
        ev.preventDefault();
        target.scrollIntoView({behavior: 'smooth', block: 'center'});
        setTimeout(() => flashTarget(target), 380);
      });
    } else {
      a.addEventListener('click', (ev) => ev.preventDefault());
    }
    return a;
  }

  function renderInlineText(line, parent) {
    // Walk the line splitting on citations and inline markers.
    // Two passes: first resolve citation chips, then within the
    // remaining text segments handle **bold** / *italic* / `code`.
    const pieces = [];
    let lastIdx = 0;
    for (const m of line.matchAll(CITATION_RE)) {
      if (m.index > lastIdx) pieces.push({t: 'txt', v: line.slice(lastIdx, m.index)});
      pieces.push({t: 'chip', kind: m[1], v: m[2]});
      lastIdx = m.index + m[0].length;
    }
    if (lastIdx < line.length) pieces.push({t: 'txt', v: line.slice(lastIdx)});

    pieces.forEach(p => {
      if (p.t === 'chip') {
        parent.appendChild(makeChip(p.kind, p.v));
        return;
      }
      let li = 0;
      for (const mm of p.v.matchAll(INLINE_RE)) {
        if (mm.index > li) parent.appendChild(document.createTextNode(p.v.slice(li, mm.index)));
        const tok = mm[0];
        if (tok.startsWith('**')) {
          const s = document.createElement('strong');
          s.textContent = tok.slice(2, -2);
          parent.appendChild(s);
        } else if (tok.startsWith('*')) {
          const e = document.createElement('em');
          e.textContent = tok.slice(1, -1);
          parent.appendChild(e);
        } else {
          const c = document.createElement('code');
          c.textContent = tok.slice(1, -1);
          parent.appendChild(c);
        }
        li = mm.index + tok.length;
      }
      if (li < p.v.length) parent.appendChild(document.createTextNode(p.v.slice(li)));
    });
  }

  const body = document.getElementById('insights-body');
  const lines = md.split('\n');
  let para = null;
  let list = null;
  function flushPara() {
    if (para && para.childNodes.length > 0) body.appendChild(para);
    para = null;
  }
  function flushList() {
    if (list && list.childNodes.length > 0) body.appendChild(list);
    list = null;
  }
  for (const rawLine of lines) {
    const line = rawLine.replace(/\s+$/, '');
    if (!line) { flushPara(); flushList(); continue; }
    if (line.startsWith('### ')) {
      flushPara(); flushList();
      const h = el('h3');
      renderInlineText(line.slice(4), h);
      body.appendChild(h);
      continue;
    }
    if (line.startsWith('## ')) {
      flushPara(); flushList();
      const h = el('h3'); // demote h2 -> h3 inside the panel
      renderInlineText(line.slice(3), h);
      body.appendChild(h);
      continue;
    }
    if (/^[-*]\s+/.test(line)) {
      flushPara();
      if (!list) list = el('ul');
      const li = el('li');
      renderInlineText(line.replace(/^[-*]\s+/, ''), li);
      list.appendChild(li);
      continue;
    }
    flushList();
    if (!para) para = el('p');
    if (para.childNodes.length > 0) para.appendChild(document.createTextNode(' '));
    renderInlineText(line, para);
  }
  flushPara(); flushList();
}
renderInsightsPanel();
</script>
</body>
</html>
"""


def build_insights_summary(
    bucket: dict[tuple[str, str], list[CallResult]],
    profiles: list[ProfileConfig],
) -> dict:
    """Pack the run's measurements into a compact JSON blob for the model.

    The shape is intentionally human-skimmable: per-bucket stats first
    (mean / median / p95 / min / max / stddev for every numeric field),
    then a `per_call` table of raw values for variance analysis. The
    model uses this as the sole source of truth for citations; the
    `INSIGHTS_INSTRUCTIONS` system prompt tells it which keys are
    referenceable via `[bucket:...]` and `[chart:...]` chips.
    """
    bucket_blobs: dict[str, dict] = {}
    bucket_index_by_key: dict[str, int] = {}
    for idx, ((profile_name, scenario), results) in enumerate(bucket.items()):
        key = f"{profile_name}__{scenario}"
        bucket_index_by_key[key] = idx
        stats = bucket_stats(results)
        bucket_blobs[key] = {
            "bucket_index": idx,
            "profile": profile_name,
            "scenario": scenario,
            "n_calls": len(results),
            "stats": {k: stats_to_dict(v) for k, v in stats.items()},
            "per_call": [
                {
                    "ttft_ms": r.ttft_ms,
                    "total_ms": r.total_ms,
                    "input_tokens": r.input_tokens,
                    "output_tokens": r.output_tokens,
                    "cached_tokens": r.cached_tokens,
                    "reasoning_tokens": r.reasoning_tokens,
                    "cost_usd": r.cost_usd,
                    "status": r.status,
                }
                for r in results
            ],
        }
    return {
        "profiles": [
            {
                "name": p.name, "model": p.model,
                "max_output_tokens": p.max_output_tokens,
                "supports_reasoning": p.supports_reasoning,
                "reasoning_effort": p.reasoning_effort,
            }
            for p in profiles
        ],
        "bucket_index_by_key": bucket_index_by_key,
        "buckets": bucket_blobs,
        "chart_ids": [
            "chart-ttft-mean", "chart-total-mean", "chart-input-mean",
            "chart-cache-mean", "chart-cost-total", "chart-out-mean",
        ],
        "per_bucket_chart_id_template": (
            "chart-bucket-{bucket_index}-perf, chart-bucket-{bucket_index}-tok"
        ),
    }


def generate_insights(
    api_key: str,
    profiles: list[ProfileConfig],
    bucket: dict[tuple[str, str], list[CallResult]],
    args: argparse.Namespace,
    pricing: dict[str, tuple[float, float, float]],
) -> str | None:
    """Send the run's stats to the thinking profile and collect a markdown report.

    Returns the model's markdown response, or `None` if the call
    failed (caller falls back to writing artifacts without insights).
    Uses the configured thinking profile when present; otherwise
    falls back to the canonical `INSIGHTS_PROFILE_NAME`.
    """
    insights_profile = next(
        (p for p in profiles if p.name == INSIGHTS_PROFILE_NAME),
        None,
    )
    if insights_profile is None:
        # Build a synthetic profile from the YAML so the script can
        # still produce insights even when the caller restricted
        # `--profile` to non-thinking variants only.
        cfg_path = Path(args.config) if hasattr(args, "config") and args.config else None
        if cfg_path and cfg_path.exists():
            try:
                insights_profile = ProfileConfig.from_yaml(cfg_path, INSIGHTS_PROFILE_NAME)
            except Exception:  # noqa: BLE001
                insights_profile = None

    if insights_profile is None:
        print(Term.dim(
            "  Insights: skipped — couldn't resolve thinking profile "
            f"({INSIGHTS_PROFILE_NAME})"
        ))
        return None

    summary_blob = build_insights_summary(bucket, profiles)
    user_message = (
        "Here are the measurements. Derive insights per the formatting "
        "rules in your instructions. Be quantitative.\n\n"
        "```json\n"
        + json.dumps(summary_blob, indent=2, default=str)
        + "\n```\n"
    )
    payload = {
        "model": insights_profile.model,
        "instructions": INSIGHTS_INSTRUCTIONS,
        "input": [
            {
                "role": "user",
                "content": [{"type": "input_text", "text": user_message}],
            }
        ],
        "max_output_tokens": insights_profile.max_output_tokens,
        "stream": True,
    }
    # Mirror what magicllm's OpenAIResponsesProvider sends: nested
    # `reasoning` with effort + summary mode. Without summary the
    # API bills reasoning tokens but emits no reasoning summary text.
    if insights_profile.supports_reasoning and insights_profile.reasoning_effort:
        payload["reasoning"] = {
            "effort": insights_profile.reasoning_effort,
            "summary": "auto",
        }

    print()
    print(Term.cyan("─" * 78))
    print(
        f"  {Term.bold('INSIGHTS')}  "
        f"deriving insights via {Term.magenta(insights_profile.name)} "
        f"({insights_profile.model})…"
    )
    print(Term.dim(
        "  This is a single thinking-mode call; expect 30-120s plus a few "
        "thousand reasoning tokens."
    ))
    print(Term.cyan("─" * 78))
    md, ok = _stream_insights_text(api_key, payload, insights_profile.timeout_secs)
    if not ok:
        return None
    return md


def _stream_insights_text(
    api_key: str, payload: dict, timeout: int,
) -> tuple[str, bool]:
    """Stream the insights call and accumulate `output_text` deltas.

    Returns `(markdown_body, ok)`. On HTTP error or network failure
    `ok` is False and the partial body (if any) is returned alongside.
    """
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
    }
    start = time.monotonic()
    chunks = 0
    body_parts: list[str] = []
    reasoning_parts: list[str] = []
    ttft_s: float | None = None
    usage: dict | None = None
    Term.live(f"  insights  {Term.dim('connecting…')}")
    try:
        with requests.post(
            OPENAI_RESPONSES_URL,
            headers=headers,
            json=payload,
            stream=True,
            timeout=timeout,
        ) as resp:
            if resp.status_code != 200:
                err = resp.text[:2000]
                Term.live_done(
                    f"  insights  {Term.red('✗')} HTTP {resp.status_code}"
                )
                print(Term.red(f"    {err}"))
                return "", False
            for raw in resp.iter_lines(decode_unicode=True):
                if not raw or raw.startswith(":") or not raw.startswith("data:"):
                    continue
                data = raw[5:].lstrip()
                if data == "[DONE]":
                    break
                try:
                    evt = json.loads(data)
                except json.JSONDecodeError:
                    continue
                chunks += 1
                etype = evt.get("type", "")
                if etype == "response.output_text.delta":
                    if ttft_s is None:
                        ttft_s = time.monotonic() - start
                    body_parts.append(evt.get("delta") or "")
                elif etype == "response.reasoning_summary_text.delta":
                    reasoning_parts.append(evt.get("delta") or "")
                elif etype == "response.completed":
                    usage = (evt.get("response") or {}).get("usage") or {}
                if chunks % 2 == 0:
                    elapsed = time.monotonic() - start
                    spinner = Term.spinner_char(chunks)
                    chars_so_far = sum(len(p) for p in body_parts)
                    ttft_hint = (
                        f"  ttft={int(ttft_s * 1000)}ms"
                        if ttft_s is not None else ""
                    )
                    Term.live(
                        f"  insights  {Term.cyan(spinner)} "
                        f"chunks={chunks:>3}  elapsed={elapsed:5.1f}s"
                        f"{ttft_hint}  body={chars_so_far:>6}ch"
                    )
    except requests.RequestException as exc:
        Term.live_done(f"  insights  {Term.red('✗')} {type(exc).__name__}: {exc}")
        return "".join(body_parts), False

    body = "".join(body_parts).strip()
    total_s = time.monotonic() - start
    out_tokens = (usage or {}).get("output_tokens")
    reasoning_tokens = ((usage or {}).get("output_tokens_details") or {}).get("reasoning_tokens")
    Term.live_done(
        f"  insights  {Term.green('✓')} done  total={total_s:0.1f}s  "
        f"body={len(body):,}ch  out={out_tokens}tok  "
        f"reason={reasoning_tokens}tok  chunks={chunks}"
    )
    return body, True


def render_html_report(
    bucket: dict[tuple[str, str], list[CallResult]],
    profiles: list[ProfileConfig],
    trace_path: Path,
    args: argparse.Namespace,
    insights_md: str | None = None,
) -> str:
    """Build a self-contained HTML report (Chart.js via CDN)."""
    scenarios_in_order: list[str] = []
    buckets_out: dict[str, dict] = {}
    for (profile_name, scenario), results in bucket.items():
        if scenario not in scenarios_in_order:
            scenarios_in_order.append(scenario)
        key = f"{profile_name}__{scenario}"
        buckets_out[key] = {
            "profile": profile_name,
            "scenario": scenario,
            "results": [asdict(r) for r in results],
            "stats": {k: stats_to_dict(v) for k, v in bucket_stats(results).items()},
        }

    profile_dicts = []
    for p in profiles:
        profile_dicts.append({
            "name": p.name,
            "provider": p.provider,
            "model": p.model,
            "max_output_tokens": p.max_output_tokens,
            "timeout_secs": p.timeout_secs,
            "streaming": p.streaming,
            "tool_choice": p.tool_choice,
            "supports_vision": p.supports_vision,
            "supports_reasoning": p.supports_reasoning,
            "reasoning_effort": p.reasoning_effort,
            "reasoning_max_tokens": p.reasoning_max_tokens,
        })

    report = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "trace_path": str(trace_path),
        "scenario": args.scenario,
        "runs": args.runs,
        "gap_secs": args.gap_secs if args.scenario == "cooldown" else None,
        "profiles": profile_dicts,
        "scenarios_run": scenarios_in_order,
        "buckets": buckets_out,
        # Markdown body produced by the post-measurement insights pass
        # (`generate_insights`). Rendered inside the HTML report by a
        # minimal client-side markdown converter that also resolves
        # `[chart:...]` / `[bucket:...]` / `[metric:...]` citations
        # into clickable chips. None when insights generation was
        # disabled or failed.
        "insights_md": insights_md,
    }

    # Escape `</` to prevent script-tag breakout if data ever contained
    # the literal `</script>` substring.
    report_json = json.dumps(report, default=str).replace("</", "<\\/")
    return HTML_TEMPLATE.replace("__REPORT_JSON__", report_json)


# ---------------------------------------------------------------- main


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    default_trace = repo_root / DEFAULT_TRACE
    default_cfg = repo_root / DEFAULT_CONFIG_PATH

    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--config", type=Path, default=default_cfg,
                    help=f"Path to magician-config.yaml (default: {DEFAULT_CONFIG_PATH})")
    ap.add_argument(
        "--profile",
        action="append",
        default=None,
        help=(
            "Profile name in llm.router.profiles.* (repeatable — pass "
            "twice to run the matrix against fast AND thinking, etc.). "
            f"Default (when not specified): both {DEFAULT_PROFILE_NAMES[0]} "
            f"AND {DEFAULT_PROFILE_NAMES[1]}, so a bare invocation tests "
            "the whole fast-vs-thinking matrix."
        ),
    )
    ap.add_argument("--trace", type=Path, default=default_trace,
                    help="Path to a captured chat-prompt dump JSON")
    ap.add_argument("--model", default=None,
                    help="Override profile model (e.g. gpt-4o if gpt-5.6-terra is not the API name)")
    ap.add_argument("--max-output-tokens", type=int, default=None,
                    help="Override profile max_output_tokens")
    ap.add_argument("--stream", dest="stream", action="store_true", default=None,
                    help="Force streaming on")
    ap.add_argument("--no-stream", dest="stream", action="store_false", default=None,
                    help="Force streaming off (skips TTFT measurement)")
    ap.add_argument(
        "--scenario",
        choices=("suite", "full", "minimal", "both", "burst", "grow", "cooldown"),
        default="suite",
        help=(
            "Test scenario. Default: `suite` — runs the full matrix "
            "(both + burst + grow) on every requested profile in cache-"
            "friendly order. Individual scenarios narrow the run for "
            "quick probes."
        ),
    )
    ap.add_argument("--runs", type=int, default=1,
                    help=(
                        "Number of runs for individual scenarios. "
                        "Ignored in suite mode (each sub-scenario has "
                        "its own sane default: both=3, burst=3, grow=5)."
                    ))
    ap.add_argument("--gap-secs", type=int, default=360,
                    help="Sleep between cooldown calls (default: 360s = 6min, past the ~5min cache TTL)")
    ap.add_argument(
        "--flat-history",
        action="store_true",
        help=(
            "Use the legacy flattened single-user-blob input layout instead "
            "of the structured Responses-API `input` array. Useful for "
            "comparing against pre-v0.4.0 runs. Default: structured input "
            "(mirrors what magician's openai_responses.rs actually sends)."
        ),
    )
    ap.add_argument(
        "--final-prompt",
        default=None,
        help=(
            "Replace the trace's terminal user message with the given text. "
            "When omitted, the harness substitutes a built-in default that "
            "asks the model to summarize prior context and derive insights "
            "(see DEFAULT_FINAL_PROMPT). This default forces "
            "production-realistic response sizes (hundreds-to-thousands of "
            "output tokens) so latency measurements aren't pinned to "
            "whatever stub the captured trace happened to end on. System "
            "prompt + tools + prior history are unchanged. Pass "
            "--no-default-final-prompt to fall back to the trace's verbatim "
            "terminal prompt."
        ),
    )
    ap.add_argument(
        "--no-default-final-prompt",
        action="store_true",
        help=(
            "Disable the built-in DEFAULT_FINAL_PROMPT substitution and use "
            "the trace's verbatim terminal user_turn as the prompt. Has no "
            "effect when --final-prompt is also passed."
        ),
    )
    ap.add_argument(
        "--no-insights",
        action="store_true",
        help=(
            "Skip the post-measurement insights pass. By default the harness "
            "sends the collected stats back through the thinking profile to "
            "derive a quantitative analysis (saved as `insights.md` and "
            "embedded into `report.html` as an AI Insights panel at the "
            "top). The pass adds ~30-120s and one thinking-mode call's worth "
            "of cost; disable it when you only need raw measurements."
        ),
    )
    ap.add_argument("--verbose", action="store_true")
    ap.add_argument(
        "--no-color",
        action="store_true",
        help="Disable ANSI colors and live progress (auto-disabled when stdout is not a TTY).",
    )
    ap.add_argument(
        "--output-dir",
        type=Path,
        default=None,
        help=(
            "Folder to write all artifacts into "
            "(report.html, report.json, summary.txt, manifest.json). "
            "Default: scripts/llm_latency_runs/<UTC-timestamp>-<scenario>-<N>p/ "
            "next to this script."
        ),
    )
    ap.add_argument(
        "--no-artifacts",
        action="store_true",
        help="Skip writing any artifact files; only print to stdout.",
    )
    ap.add_argument(
        "--no-bodies",
        action="store_true",
        help=(
            "Skip writing per-call response bodies to `responses.jsonl`. "
            "By default the harness captures every call's `output_text` "
            "and `reasoning_summary` so post-run quality review is "
            "possible. Bodies are also stripped from `report.json` "
            "regardless of this flag — they live only in the JSONL."
        ),
    )
    ap.add_argument(
        "--env-file",
        type=Path,
        default=None,
        help=(
            "Path to a `.env`-style file to load before reading "
            "OPENAI_API_KEY. Default: auto-detect "
            f"({' or '.join(DEFAULT_DOTENV_CANDIDATES)}) under the repo "
            "root (parent of scripts/). Process env still wins."
        ),
    )
    ap.add_argument(
        "--no-dotenv",
        action="store_true",
        help="Skip automatic .env loading entirely.",
    )
    ap.add_argument(
        "--no-open-browser",
        action="store_true",
        help=(
            "Skip auto-opening report.html after the run. "
            "By default the script opens the report in a new tab "
            "(new window if no browser session is open) when artifacts "
            "are written. Disable for CI / headless runs."
        ),
    )
    ap.add_argument(
        "--pricing",
        action="append",
        default=None,
        metavar="MODEL=IN:CACHED:OUT",
        help=(
            "Override per-1M-token pricing for a model. Repeatable. "
            "Example: --pricing gpt-5.6-terra=2.00:0.20:12.00. Rates not "
            "overridden fall back to the built-in DEFAULT_PRICING "
            "(verify against https://developers.openai.com/api/docs/pricing — the script "
            "computes cost locally; the API doesn't return dollars)."
        ),
    )
    args = ap.parse_args()

    Term.configure(force_no_color=args.no_color)
    print_hero_banner(__version__)

    # Dotenv resolution: explicit --env-file > auto-detect under repo
    # root > skip. Loaded BEFORE the API-key check so a bare invocation
    # in a repo with `.env.development` works.
    if not args.no_dotenv:
        dotenv_path: Path | None = None
        if args.env_file:
            if not args.env_file.exists():
                print(
                    f"ERROR: --env-file path does not exist: {args.env_file}",
                    file=sys.stderr,
                )
                return 1
            dotenv_path = args.env_file
        else:
            repo_root = Path(__file__).resolve().parent.parent
            for name in DEFAULT_DOTENV_CANDIDATES:
                candidate = repo_root / name
                if candidate.exists():
                    dotenv_path = candidate
                    break
        if dotenv_path:
            try:
                count = load_dotenv(dotenv_path)
                print(Term.dim(
                    f"Loaded {count} env var(s) from {dotenv_path.name} "
                    f"(process env wins)"
                ))
            except OSError as exc:
                print(
                    Term.yellow(f"WARN: could not read {dotenv_path}: {exc}"),
                    file=sys.stderr,
                )

    api_key = os.environ.get("OPENAI_API_KEY")
    if not api_key:
        print(
            Term.red("ERROR: OPENAI_API_KEY not set in environment "
                     "(and no .env.development found at repo root)."),
            file=sys.stderr,
        )
        print(
            Term.dim("Try: OPENAI_API_KEY=sk-... python3 scripts/...py  "
                     "OR  add OPENAI_API_KEY=sk-... to .env.development"),
            file=sys.stderr,
        )
        return 1

    profile_names: list[str] = args.profile or list(DEFAULT_PROFILE_NAMES)

    if not args.trace.exists():
        print(f"ERROR: trace file not found: {args.trace}", file=sys.stderr)
        return 1
    trace = load_trace(args.trace)

    # Load every requested profile up front so any YAML errors surface
    # before we start burning API calls on the first profile.
    profiles: list[ProfileConfig] = []
    for name in profile_names:
        try:
            p = ProfileConfig.from_yaml(args.config, name)
        except (FileNotFoundError, KeyError, yaml.YAMLError) as exc:
            print(f"ERROR loading profile {name!r}: {exc}", file=sys.stderr)
            return 1
        if args.model:
            p.model = args.model
        if args.max_output_tokens is not None:
            p.max_output_tokens = args.max_output_tokens
        profiles.append(p)

    # Resolve pricing: built-in defaults overlaid with --pricing overrides.
    try:
        pricing_overrides = parse_pricing_override(args.pricing)
    except ValueError as exc:
        print(Term.red(f"ERROR: {exc}"), file=sys.stderr)
        return 1
    pricing: dict[str, tuple[float, float, float]] = {**DEFAULT_PRICING, **pricing_overrides}
    # Note any profile model that isn't in the pricing dict — its
    # per-call cost will render as "—" instead of a wrong number.
    unknown_models = sorted({
        p.model for p in profiles if p.model not in pricing
    })

    sizes = trace.get("sizes") or {}
    total_chars = sizes.get("total_chars", 0)
    msg_count = sizes.get("message_count", 0)
    tool_count = sizes.get("tool_count", 0)

    print(Term.cyan("═" * 78))
    print(Term.bold("  LLM Context Latency Probe"))
    print(Term.cyan("═" * 78))
    print(f"  {Term.dim('Config:')}  {args.config}")
    print(f"  {Term.dim('Trace:')}   {args.trace.name}  "
          f"{Term.dim(f'({total_chars:,} chars · {msg_count} msgs · {tool_count} tools)')}")
    # Pricing summary line — show what rates we're using for each
    # profile's model. Honest framing: cost is computed locally.
    pricing_bits = []
    for p in profiles:
        rates = pricing.get(p.model)
        if rates is None:
            pricing_bits.append(f"{p.model}={Term.red('unknown')}")
        else:
            in_r, cached_r, out_r = rates
            override_tag = " (override)" if p.model in pricing_overrides else ""
            pricing_bits.append(
                f"{p.model}=${in_r}/${cached_r}/${out_r}{override_tag}"
            )
    print(f"  {Term.dim('Pricing $/1M (in/cached/out):')} " + "  ".join(pricing_bits))
    if unknown_models:
        print(
            Term.yellow(
                f"  ⚠ No pricing entry for: {', '.join(unknown_models)}. "
                "Cost will show as '—'. Use --pricing MODEL=IN:CACHED:OUT."
            )
        )
    print(f"  {Term.dim('Profiles:')} {len(profiles)}")
    for p in profiles:
        reasoning = (
            f"effort={Term.yellow(p.reasoning_effort)}"
            if p.supports_reasoning and p.reasoning_effort
            else Term.dim("no-reasoning")
        )
        print(
            f"    • {Term.bold(p.name)}\n"
            f"      {Term.dim('model=')}{p.model}  "
            f"{Term.dim('max_out=')}{p.max_output_tokens:,}  "
            f"{Term.dim('stream=')}{p.streaming}  {reasoning}"
        )
    if args.stream is not None:
        print(f"  {Term.yellow('streaming OVERRIDE for all profiles:')} {args.stream}")
    print(f"  {Term.dim('Scenario:')} {Term.bold(args.scenario)}"
          + (f"  ({args.runs} runs)" if args.runs > 1 and args.scenario != "suite" else ""))
    if args.scenario == "suite":
        # Show suite breakdown + estimated call count up-front so the
        # user knows what's about to run before any API key gets burned.
        calls_per_profile = sum(
            (2 * r) if s == "both" else r for s, r in SUITE_SCENARIOS
        )
        total_calls = calls_per_profile * len(profiles)
        sub_list = ", ".join(f"{s}×{r}" for s, r in SUITE_SCENARIOS)
        print(f"  {Term.dim('Suite =')} {sub_list}")
        print(
            f"  {Term.dim('Total calls =')} "
            f"{calls_per_profile} per profile × {len(profiles)} profile(s) = "
            f"{Term.bold(str(total_calls))} calls"
        )
    if args.scenario == "cooldown":
        print(f"  {Term.dim('Cooldown gap:')} {args.gap_secs}s")
    print()
    print(Term.yellow("  ⚠ CACHE CAVEAT — OpenAI's Responses-API prefix cache has a ~5-min TTL."))
    print(Term.dim("    If you ran this script with the same trace less than 5 min ago, the"))
    print(Term.dim("    first call here may already be 'warm'. cached_tokens is reported per"))
    print(Term.dim("    call so you can see hits as they happen."))
    print(Term.cyan("═" * 78))
    print()

    # Resolve the terminal-prompt source:
    # 1. explicit `--final-prompt` overrides everything
    # 2. else `--no-default-final-prompt` keeps the trace verbatim
    # 3. else substitute DEFAULT_FINAL_PROMPT so latency tracks
    #    production-realistic output volume
    if args.final_prompt:
        resolved_final_prompt = args.final_prompt
    elif args.no_default_final_prompt:
        resolved_final_prompt = None
    else:
        resolved_final_prompt = DEFAULT_FINAL_PROMPT

    full_parts = parts_from_trace_full(
        trace,
        structured=not args.flat_history,
        final_prompt=resolved_final_prompt,
    )
    min_parts = parts_minimal()

    # Results bucketed by (profile_name, scenario_label).
    bucket: dict[tuple[str, str], list[CallResult]] = {}

    for prof_idx, prof in enumerate(profiles, start=1):
        print()
        print(Term.cyan("─" * 78))
        print(
            f"  {Term.bold(f'PROFILE {prof_idx}/{len(profiles)}')}  "
            f"{Term.magenta(prof.name)}"
        )
        print(Term.cyan("─" * 78))

        if args.scenario == "suite":
            for sub_scenario, sub_runs in SUITE_SCENARIOS:
                print(Term.dim(f"  ─── {sub_scenario} ({sub_runs} runs) ───"))
                _run_sub_scenario(
                    sub_scenario, sub_runs, api_key, full_parts, min_parts, prof,
                    bucket, args.stream, args.gap_secs, pricing,
                )
        else:
            _run_sub_scenario(
                args.scenario, args.runs, api_key, full_parts, min_parts, prof,
                bucket, args.stream, args.gap_secs, pricing,
            )

    print("\n" + "=" * 30 + " SUMMARY " + "=" * 30)
    for (profile_name, scenario_label), results in bucket.items():
        summarise(f"{profile_name} / {scenario_label}", results)

    # Grand-total cost across all buckets.
    grand_total = sum(
        (r.cost_usd or 0.0)
        for results in bucket.values()
        for r in results
    )
    total_calls = sum(len(results) for results in bucket.values())
    print()
    print(Term.cyan("─" * 78))
    print(
        f"  {Term.bold('GRAND TOTAL')}  "
        f"{Term.bold(fmt_usd(grand_total))} across {total_calls} calls  "
        f"{Term.dim(f'(avg {fmt_usd(grand_total / total_calls) if total_calls else None}/call)')}"
    )
    print(Term.dim("  Cost is computed from token counts × per-1M-token rates;"))
    print(Term.dim("  the OpenAI API does not return a dollar amount. Verify rates"))
    print(Term.dim("  against your account's current pricing or override with --pricing."))
    print(Term.cyan("─" * 78))

    # Post-measurement insights pass — sends the run's stats to the
    # thinking profile and asks for a quantitative analysis. Off-by-
    # default-free (~30-120s + a thinking-mode call), opt out with
    # `--no-insights`. Failure here is non-fatal: we still write the
    # rest of the artifacts.
    insights_md: str | None = None
    if not args.no_insights:
        try:
            insights_md = generate_insights(api_key, profiles, bucket, args, pricing)
        except Exception as exc:  # noqa: BLE001
            print(Term.dim(f"  Insights: skipped ({type(exc).__name__}: {exc})"))

    if not args.no_artifacts:
        try:
            run_dir = _resolve_run_dir(args, profiles)
            _write_run_artifacts(run_dir, bucket, profiles, args, insights_md=insights_md)
            print(f"\nArtifacts written to: {run_dir}")
            print("  • report.html   — open in a browser (needs network for Chart.js CDN)")
            print("  • report.json   — raw data (no response bodies; see responses.jsonl)")
            print("  • summary.txt   — text summary table")
            print("  • manifest.json — run metadata + cost rollup")
            if not args.no_bodies:
                print("  • responses.jsonl — one row per call: output_text + reasoning_summary (for quality review)")
            if insights_md:
                print("  • insights.md   — AI-derived analysis (thinking-profile pass)")

            if not args.no_open_browser:
                report_path = run_dir / "report.html"
                if report_path.exists():
                    _open_report_in_browser(report_path)
        except OSError as exc:
            print(f"\nERROR writing artifacts: {exc}", file=sys.stderr)
            return 1

    return 0


def _open_report_in_browser(report_path: Path) -> None:
    """Open the report in a new tab (or new window if no session exists).

    Prints `Opening report <name>` before invoking the browser so the
    user gets a clear signal that something is about to happen. Failure
    is non-fatal — webbrowser.open() returns False if no browser is
    registered (e.g., headless servers), in which case we just hint
    the user how to open it manually.
    """
    name = report_path.name
    url = report_path.resolve().as_uri()
    print(f"\n{Term.cyan('Opening report ' + name)}  {Term.dim(url)}")
    try:
        # new=2 → new tab if available, else new window (per webbrowser docs).
        # Falls back gracefully on systems where no GUI browser is registered.
        opened = webbrowser.open(url, new=2)
    except Exception as exc:  # noqa: BLE001  — any backend failure is non-fatal
        opened = False
        print(Term.dim(f"  ({type(exc).__name__}: {exc})"))
    if not opened:
        print(Term.dim(
            f"  Couldn't auto-open. Open it manually: {url}"
        ))


def _resolve_run_dir(args: argparse.Namespace, profiles: list[ProfileConfig]) -> Path:
    """Return the resolved output directory for this run.

    If `--output-dir` is set, use it verbatim. Otherwise, auto-generate
    a name like `<UTC-timestamp>-<scenario>-<N-profiles>p` under
    `<script-dir>/llm_latency_runs/`. The auto-name embeds enough
    distinguishing info that two sequential runs never collide and a
    `ls` of the parent directory tells you what was tested when.
    """
    if args.output_dir:
        return args.output_dir.resolve()
    script_dir = Path(__file__).resolve().parent
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    name = f"{timestamp}-{args.scenario}-{len(profiles)}p"
    if args.runs > 1 and args.scenario in ("burst", "grow", "cooldown", "full", "minimal", "both"):
        name += f"-r{args.runs}"
    return (script_dir / "llm_latency_runs" / name).resolve()


def _write_run_artifacts(
    run_dir: Path,
    bucket: dict[tuple[str, str], list[CallResult]],
    profiles: list[ProfileConfig],
    args: argparse.Namespace,
    insights_md: str | None = None,
) -> None:
    run_dir.mkdir(parents=True, exist_ok=True)

    # 0. insights.md — markdown body from the post-measurement
    #    thinking-profile call (when enabled). Written first so
    #    `report.html` can embed it.
    if insights_md:
        (run_dir / "insights.md").write_text(insights_md, encoding="utf-8")

    # 1. report.html — the dashboard.
    html = render_html_report(bucket, profiles, args.trace, args, insights_md=insights_md)
    (run_dir / "report.html").write_text(html, encoding="utf-8")

    # 2. report.json — same data the HTML embeds, but standalone for
    #    grep / jq / spreadsheet ingestion. Heavy response bodies are
    #    stripped here and saved separately as `responses.jsonl` (see
    #    below) so report.json stays small and grep-friendly.
    def _strip_bodies(d: dict) -> dict:
        return {k: v for k, v in d.items()
                if k not in ("output_text", "reasoning_summary")}

    scenarios_in_order: list[str] = []
    buckets_out: dict[str, dict] = {}
    for (profile_name, scenario), results in bucket.items():
        if scenario not in scenarios_in_order:
            scenarios_in_order.append(scenario)
        key = f"{profile_name}__{scenario}"
        buckets_out[key] = {
            "profile": profile_name,
            "scenario": scenario,
            "results": [_strip_bodies(asdict(r)) for r in results],
            "stats": {k: stats_to_dict(v) for k, v in bucket_stats(results).items()},
        }
    report_data = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "trace_path": str(args.trace),
        "scenario": args.scenario,
        "runs": args.runs,
        "gap_secs": args.gap_secs if args.scenario == "cooldown" else None,
        "profiles": [
            {
                "name": p.name, "model": p.model,
                "max_output_tokens": p.max_output_tokens,
                "streaming": p.streaming, "tool_choice": p.tool_choice,
                "supports_reasoning": p.supports_reasoning,
                "reasoning_effort": p.reasoning_effort,
                "reasoning_max_tokens": p.reasoning_max_tokens,
                "timeout_secs": p.timeout_secs,
            }
            for p in profiles
        ],
        "scenarios_run": scenarios_in_order,
        "buckets": buckets_out,
    }
    (run_dir / "report.json").write_text(
        json.dumps(report_data, indent=2, default=str), encoding="utf-8"
    )

    # 2b. responses.jsonl — one JSON line per call carrying the model's
    #     `output_text` + `reasoning_summary` plus enough context to
    #     identify which (profile, scenario, run_index) produced it.
    #     This is the canonical artifact for post-run quality review:
    #     you can `cat responses.jsonl | jq -r '... | select(.profile |
    #     contains("thinking")) | .output_text'` to compare fast vs
    #     thinking replies side-by-side. Skipped under --no-bodies.
    if not getattr(args, "no_bodies", False):
        jsonl_lines: list[str] = []
        for (profile_name, scenario), results in bucket.items():
            for i, r in enumerate(results):
                if r.output_text is None and r.reasoning_summary is None:
                    # Nothing to record (call failed before stream started).
                    continue
                jsonl_lines.append(json.dumps({
                    "profile": profile_name,
                    "scenario": scenario,
                    "run_index": i,
                    "model": r.model,
                    "label": r.label,
                    "status": r.status,
                    "ttft_ms": r.ttft_ms,
                    "total_ms": r.total_ms,
                    "input_tokens": r.input_tokens,
                    "output_tokens": r.output_tokens,
                    "cached_tokens": r.cached_tokens,
                    "reasoning_tokens": r.reasoning_tokens,
                    "cost_usd": r.cost_usd,
                    "output_text": r.output_text,
                    "reasoning_summary": r.reasoning_summary,
                }, default=str, ensure_ascii=False))
        if jsonl_lines:
            (run_dir / "responses.jsonl").write_text(
                "\n".join(jsonl_lines) + "\n", encoding="utf-8"
            )

    # 3. summary.txt — the same per-bucket lines that print() emits.
    summary_lines: list[str] = []
    summary_lines.append("=" * 30 + " SUMMARY " + "=" * 30)
    for (profile_name, scenario_label), results in bucket.items():
        summary_lines.extend(
            summarise_lines(f"{profile_name} / {scenario_label}", results)
        )
    (run_dir / "summary.txt").write_text("\n".join(summary_lines) + "\n", encoding="utf-8")

    # 4. manifest.json — minimal reproducibility metadata + cost rollup.
    grand_total_cost = 0.0
    grand_input_tokens = 0
    grand_cached_tokens = 0
    grand_output_tokens = 0
    grand_reasoning_tokens = 0
    cost_by_bucket: dict[str, float] = {}
    total_calls = 0
    successful_calls = 0
    for (profile_name, scenario), results in bucket.items():
        bucket_cost = sum((r.cost_usd or 0.0) for r in results)
        cost_by_bucket[f"{profile_name}__{scenario}"] = round(bucket_cost, 6)
        grand_total_cost += bucket_cost
        total_calls += len(results)
        successful_calls += sum(1 for r in results if not r.error and 200 <= r.status < 300)
        grand_input_tokens += sum(r.input_tokens or 0 for r in results)
        grand_cached_tokens += sum(r.cached_tokens or 0 for r in results)
        grand_output_tokens += sum(r.output_tokens or 0 for r in results)
        grand_reasoning_tokens += sum(r.reasoning_tokens or 0 for r in results)
    manifest = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "script_path": str(Path(__file__).resolve()),
        "trace_path": str(args.trace.resolve()),
        "config_path": str(args.config.resolve()),
        "scenario": args.scenario,
        "runs": args.runs,
        "gap_secs": args.gap_secs if args.scenario == "cooldown" else None,
        "profiles": [p.name for p in profiles],
        "model_overrides": {
            "model": args.model,
            "max_output_tokens": args.max_output_tokens,
            "stream": args.stream,
        },
        "totals": {
            "calls": total_calls,
            "successful_calls": successful_calls,
            "grand_total_cost_usd": round(grand_total_cost, 6),
            "input_tokens": grand_input_tokens,
            "cached_tokens": grand_cached_tokens,
            "output_tokens": grand_output_tokens,
            "reasoning_tokens": grand_reasoning_tokens,
            "cost_by_bucket": cost_by_bucket,
        },
        "artifacts": ["report.html", "report.json", "summary.txt", "manifest.json"],
    }
    (run_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, default=str), encoding="utf-8"
    )


if __name__ == "__main__":
    sys.exit(main())
