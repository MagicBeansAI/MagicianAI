#!/usr/bin/env python3
"""Expose local inference servers as Ollama /api/generate.

This bridge is intended for focused local model comparisons where Magician's
real Ollama provider and logical-chunk eval runner must stay unchanged while
candidate models execute through llama-server, Ollama, or an OpenAI-compatible
chat server such as ``mlx_lm.server``.
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Iterable
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=11434)
    parser.add_argument(
        "--upstream",
        default="http://127.0.0.1:18080/completion",
        help="llama-server /completion or Ollama /api/generate endpoint",
    )
    parser.add_argument(
        "--upstream-kind",
        choices=("llama-server", "ollama", "openai-chat"),
        default="llama-server",
        help=(
            "translate llama-server/OpenAI-chat responses or transparently "
            "observe Ollama"
        ),
    )
    parser.add_argument(
        "--metrics-jsonl",
        type=Path,
        help="append timing/token metrics without prompts or response bodies",
    )
    parser.add_argument(
        "--served-model",
        default="gemma4:12b",
        help="model identity exposed by synthetic llama-server Ollama metadata",
    )
    parser.add_argument(
        "--upstream-model",
        help=(
            "model name/path sent to an OpenAI-compatible upstream; defaults "
            "to --served-model"
        ),
    )
    parser.add_argument(
        "--served-model-size-bytes",
        type=int,
        default=0,
        help="resident model size exposed by synthetic llama-server /api/ps",
    )
    parser.add_argument(
        "--default-temperature",
        type=float,
        default=0.0,
        help="deterministic temperature used when Ollama options omit one",
    )
    parser.add_argument(
        "--cache-prompt",
        action=argparse.BooleanOptionalAction,
        default=False,
        help="allow llama-server to reuse common prompt prefixes between requests",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run provider-free translation and OpenAI-stream regression checks",
    )
    return parser.parse_args()


def as_int(value: Any, default: int) -> int:
    if isinstance(value, bool):
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def as_float(value: Any, default: float) -> float:
    if isinstance(value, bool):
        return default
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def completion_payload(
    body: dict[str, Any], default_temperature: float, cache_prompt: bool
) -> dict[str, Any]:
    options = body.get("options") if isinstance(body.get("options"), dict) else {}
    payload: dict[str, Any] = {
        "prompt": str(body.get("prompt") or ""),
        "n_predict": as_int(options.get("num_predict"), 4096),
        "temperature": as_float(options.get("temperature"), default_temperature),
        "stream": False,
        "cache_prompt": cache_prompt,
        "seed": 0,
    }
    output_format = body.get("format")
    if output_format == "json":
        payload["json_schema"] = {"type": "object"}
    elif isinstance(output_format, dict):
        payload["json_schema"] = output_format
    return payload


def openai_chat_payload(
    body: dict[str, Any], default_temperature: float, upstream_model: str
) -> dict[str, Any]:
    """Translate the subset of Ollama generate options used by Magician.

    The logical-chunk prompts already contain their structured-output
    instructions and schemas. ``mlx_lm.server`` does not expose Ollama's
    ``format`` grammar field, so forwarding the prompt verbatim preserves the
    evaluator's semantic input without inventing a backend-specific schema
    mechanism.
    """

    options = body.get("options") if isinstance(body.get("options"), dict) else {}
    payload: dict[str, Any] = {
        "model": upstream_model,
        "messages": [{"role": "user", "content": str(body.get("prompt") or "")}],
        "max_tokens": as_int(options.get("num_predict"), 4096),
        "temperature": as_float(options.get("temperature"), default_temperature),
        # Magician still receives one non-streaming Ollama response. Streaming
        # only across this internal hop lets OpenAI-compatible local servers
        # expose an observed time-to-first-token and decode window.
        "stream": True,
        "stream_options": {"include_usage": True},
        "seed": as_int(options.get("seed"), 0),
    }
    for option in ("top_p", "top_k", "min_p"):
        if option in options:
            payload[option] = options[option]
    if stop := options.get("stop"):
        payload["stop"] = stop
    if isinstance(body.get("think"), bool):
        # Qwen-family MLX chat templates commonly expose this argument. It is
        # best-effort because OpenAI-compatible servers cannot guarantee every
        # model template supports an Ollama-style reasoning switch.
        payload["chat_template_kwargs"] = {
            "enable_thinking": body["think"],
        }
    return payload


def text_fragment(value: Any) -> str:
    """Extract text from common OpenAI delta content representations."""

    if isinstance(value, str):
        return value
    if not isinstance(value, list):
        return ""
    parts = []
    for item in value:
        if isinstance(item, str):
            parts.append(item)
        elif isinstance(item, dict) and isinstance(item.get("text"), str):
            parts.append(item["text"])
    return "".join(parts)


def parse_openai_sse(
    lines: Iterable[bytes | str],
    started: float,
    clock: Callable[[], float] = time.monotonic,
) -> tuple[dict[str, Any], float]:
    """Collapse an OpenAI-compatible SSE response and retain observed timing.

    Stock ``mlx_lm.server`` reports aggregate usage but not native prefill and
    decode durations. The first non-empty content/reasoning delta therefore
    gives us an observed TTFT boundary. This is deliberately labelled as an
    estimate in metrics instead of being presented as backend-native timing.
    """

    content_parts: list[str] = []
    reasoning_parts: list[str] = []
    usage: dict[str, Any] = {}
    finish_reason: str | None = None
    response_model: str | None = None
    first_output_ms: float | None = None

    for raw_line in lines:
        line = (
            raw_line.decode("utf-8", errors="replace")
            if isinstance(raw_line, bytes)
            else raw_line
        )
        line = line.strip()
        if not line or not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if data == "[DONE]":
            break
        event = json.loads(data)
        if not isinstance(event, dict):
            continue
        if isinstance(event.get("model"), str):
            response_model = event["model"]
        if isinstance(event.get("usage"), dict):
            usage = event["usage"]
        choices = event.get("choices")
        if not isinstance(choices, list):
            continue
        for choice in choices:
            if not isinstance(choice, dict):
                continue
            if choice.get("finish_reason") is not None:
                finish_reason = str(choice["finish_reason"])
            delta = choice.get("delta")
            if not isinstance(delta, dict):
                continue
            content = text_fragment(delta.get("content"))
            reasoning = text_fragment(
                delta.get("reasoning_content", delta.get("reasoning"))
            )
            if first_output_ms is None and (content or reasoning):
                first_output_ms = max((clock() - started) * 1000, 0.0)
            if content:
                content_parts.append(content)
            if reasoning:
                reasoning_parts.append(reasoning)

    wall_duration_ms = max((clock() - started) * 1000, 0.0)
    ttft_ms = first_output_ms if first_output_ms is not None else wall_duration_ms
    decode_duration_ms = max(wall_duration_ms - ttft_ms, 0.0)
    collapsed: dict[str, Any] = {
        "model": response_model,
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": "".join(content_parts),
                    "reasoning_content": "".join(reasoning_parts),
                },
                "finish_reason": finish_reason or "stop",
            }
        ],
        "usage": usage,
        "_bridge_timing": {
            "ttft_ms": ttft_ms,
            "decode_duration_ms": decode_duration_ms,
        },
    }
    return collapsed, wall_duration_ms


def read_openai_chat_response(
    response: Any, started: float
) -> tuple[dict[str, Any], float]:
    content_type = str(response.headers.get("Content-Type") or "").lower()
    if "text/event-stream" in content_type:
        return parse_openai_sse(response, started)
    upstream = json.loads(response.read())
    if not isinstance(upstream, dict):
        raise ValueError("upstream response body must be an object")
    wall_duration_ms = max((time.monotonic() - started) * 1000, 0.0)
    return upstream, wall_duration_ms


def ollama_payload(request_body: dict[str, Any], upstream: dict[str, Any]) -> dict[str, Any]:
    timings = upstream.get("timings") if isinstance(upstream.get("timings"), dict) else {}
    prompt_ms = as_float(timings.get("prompt_ms"), 0.0)
    predicted_ms = as_float(timings.get("predicted_ms"), 0.0)
    return {
        "model": request_body.get("model") or upstream.get("model") or "llama-server",
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "response": str(upstream.get("content") or ""),
        "done": True,
        "done_reason": upstream.get("stop_type") or "stop",
        "context": [],
        "total_duration": int((prompt_ms + predicted_ms) * 1_000_000),
        "load_duration": 0,
        "prompt_eval_count": as_int(upstream.get("tokens_evaluated"), 0),
        "prompt_eval_duration": int(prompt_ms * 1_000_000),
        "eval_count": as_int(upstream.get("tokens_predicted"), 0),
        "eval_duration": int(predicted_ms * 1_000_000),
    }


def openai_ollama_payload(
    request_body: dict[str, Any], upstream: dict[str, Any], wall_duration_ms: float
) -> dict[str, Any]:
    choices = upstream.get("choices")
    choice = choices[0] if isinstance(choices, list) and choices else {}
    message = choice.get("message") if isinstance(choice, dict) else {}
    content = message.get("content") if isinstance(message, dict) else ""
    usage = upstream.get("usage") if isinstance(upstream.get("usage"), dict) else {}
    prompt_tokens = as_int(usage.get("prompt_tokens"), 0)
    completion_tokens = as_int(usage.get("completion_tokens"), 0)
    timing = (
        upstream.get("_bridge_timing")
        if isinstance(upstream.get("_bridge_timing"), dict)
        else {}
    )
    prompt_duration_ms = as_float(timing.get("ttft_ms"), 0.0)
    completion_duration_ms = as_float(timing.get("decode_duration_ms"), 0.0)
    return {
        "model": request_body.get("model") or upstream.get("model") or "mlx-lm",
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "response": str(content or ""),
        "done": True,
        "done_reason": choice.get("finish_reason") or "stop",
        "context": [],
        "total_duration": int(wall_duration_ms * 1_000_000),
        "load_duration": 0,
        "prompt_eval_count": prompt_tokens,
        "prompt_eval_duration": int(round(prompt_duration_ms * 1_000_000)),
        "eval_count": completion_tokens,
        "eval_duration": int(round(completion_duration_ms * 1_000_000)),
    }


def throughput(tokens: int, duration_ns: int) -> float | None:
    if tokens <= 0 or duration_ns <= 0:
        return None
    return tokens / (duration_ns / 1_000_000_000)


def capability_metadata(
    backend: str, request_body: dict[str, Any]
) -> dict[str, Any]:
    options = (
        request_body.get("options")
        if isinstance(request_body.get("options"), dict)
        else {}
    )
    structured_output_requested = request_body.get("format") is not None
    if not structured_output_requested:
        structured_output_mode = "not_requested"
    elif backend == "openai-chat":
        structured_output_mode = "prompt_constrained"
    else:
        structured_output_mode = "backend_grammar_constrained"
    if backend == "openai-chat":
        timing_source = "observed_openai_stream"
        prompt_semantics = "ttft_including_queue_load_prefill_and_first_token"
        completion_semantics = "post_first_token_stream_window"
    else:
        timing_source = "backend_native"
        prompt_semantics = "native_prompt_evaluation"
        completion_semantics = "native_token_generation"
    if backend == "ollama":
        context_control = "per_request_num_ctx"
        model_lifecycle = "ollama_keep_alive"
        reasoning_control = "native_think_flag"
    elif backend == "llama-server":
        context_control = "server_startup_context"
        model_lifecycle = "server_process_owned"
        reasoning_control = "prompt_only"
    else:
        context_control = "model_or_server_startup_context"
        model_lifecycle = "server_process_owned"
        reasoning_control = (
            "chat_template_enable_thinking_best_effort"
            if isinstance(request_body.get("think"), bool)
            else "not_requested"
        )
    return {
        "structured_output_requested": structured_output_requested,
        "structured_output_mode": structured_output_mode,
        "timing_source": timing_source,
        "prompt_timing_semantics": prompt_semantics,
        "completion_timing_semantics": completion_semantics,
        "requested_context_tokens": options.get("num_ctx"),
        "context_control": context_control,
        "model_lifecycle": model_lifecycle,
        "reasoning_control": reasoning_control,
    }


def metric_record(
    backend: str,
    response: dict[str, Any],
    wall_duration_ms: float,
    request_body: dict[str, Any],
) -> dict[str, Any]:
    prompt_tokens = as_int(response.get("prompt_eval_count"), 0)
    prompt_duration_ns = as_int(response.get("prompt_eval_duration"), 0)
    completion_tokens = as_int(response.get("eval_count"), 0)
    completion_duration_ns = as_int(response.get("eval_duration"), 0)
    completion_throughput_tokens = (
        max(completion_tokens - 1, 0)
        if backend == "openai-chat" and completion_duration_ns > 0
        else completion_tokens
    )
    return {
        "recorded_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "backend": backend,
        "model": response.get("model"),
        "wall_duration_ms": round(wall_duration_ms, 3),
        "total_duration_ms": round(as_int(response.get("total_duration"), 0) / 1_000_000, 3),
        "load_duration_ms": round(as_int(response.get("load_duration"), 0) / 1_000_000, 3),
        "prompt_tokens": prompt_tokens,
        "prompt_duration_ms": round(prompt_duration_ns / 1_000_000, 3),
        "prompt_tokens_per_second": throughput(prompt_tokens, prompt_duration_ns),
        "completion_tokens": completion_tokens,
        "completion_throughput_tokens": completion_throughput_tokens,
        "completion_duration_ms": round(completion_duration_ns / 1_000_000, 3),
        "completion_tokens_per_second": throughput(
            completion_throughput_tokens, completion_duration_ns
        ),
        **capability_metadata(backend, request_body),
    }


def build_handler(
    upstream_url: str,
    upstream_kind: str,
    default_temperature: float,
    cache_prompt: bool,
    metrics_path: Path | None,
    served_model: str,
    upstream_model: str,
    served_model_size_bytes: int,
) -> type[BaseHTTPRequestHandler]:
    metrics_lock = threading.Lock()
    ollama_base_url = (
        upstream_url.rsplit("/api/", 1)[0]
        if upstream_kind == "ollama" and "/api/" in upstream_url
        else upstream_url.rstrip("/")
    )

    class Handler(BaseHTTPRequestHandler):
        server_version = "MagicianLlamaBridge/1.0"

        def log_message(self, format_string: str, *args: Any) -> None:
            print(
                f"[ollama-llama-bridge] {self.address_string()} "
                f"{format_string % args}",
                file=sys.stderr,
                flush=True,
            )

        def send_json(self, status: int, payload: dict[str, Any]) -> None:
            encoded = json.dumps(payload, separators=(",", ":")).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_HEAD(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract
            self.send_response(200)
            self.end_headers()

        def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract
            if self.path in ("/", "/api/version"):
                self.send_json(200, {"version": "llama-server-bridge"})
            elif self.path in ("/api/tags", "/api/ps"):
                if upstream_kind == "ollama":
                    try:
                        with urllib.request.urlopen(
                            f"{ollama_base_url}{self.path}", timeout=30
                        ) as response:
                            payload = json.loads(response.read())
                        if not isinstance(payload, dict):
                            raise ValueError("upstream metadata must be an object")
                        self.send_json(200, payload)
                    except (OSError, ValueError, json.JSONDecodeError) as error:
                        self.send_json(502, {"error": str(error)})
                else:
                    model = {
                        "name": served_model,
                        "model": served_model,
                        "size": served_model_size_bytes,
                        "size_vram": served_model_size_bytes,
                    }
                    self.send_json(200, {"models": [model]})
            else:
                self.send_json(404, {"error": "not found"})

        def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract
            if self.path != "/api/generate":
                self.send_json(404, {"error": "not found"})
                return
            try:
                length = as_int(self.headers.get("Content-Length"), 0)
                request_body = json.loads(self.rfile.read(length))
                if not isinstance(request_body, dict):
                    raise ValueError("request body must be an object")
                if request_body.get("stream") not in (None, False):
                    raise ValueError("streaming is not supported")
                is_unload = (
                    request_body.get("keep_alive") == 0
                    and not str(request_body.get("prompt") or "")
                )
                if is_unload and upstream_kind != "ollama":
                    self.send_json(
                        200,
                        {
                            "model": request_body.get("model") or served_model,
                            "response": "",
                            "done": True,
                            "done_reason": "unload_not_applicable",
                            "total_duration": 0,
                            "load_duration": 0,
                            "prompt_eval_count": 0,
                            "prompt_eval_duration": 0,
                            "eval_count": 0,
                            "eval_duration": 0,
                        },
                    )
                    return
                if upstream_kind == "ollama":
                    outbound_body = request_body
                elif upstream_kind == "openai-chat":
                    outbound_body = openai_chat_payload(
                        request_body, default_temperature, upstream_model
                    )
                else:
                    outbound_body = completion_payload(
                        request_body, default_temperature, cache_prompt
                    )
                upstream_request = urllib.request.Request(
                    upstream_url,
                    data=json.dumps(outbound_body).encode("utf-8"),
                    headers={"Content-Type": "application/json"},
                    method="POST",
                )
                started = time.monotonic()
                with urllib.request.urlopen(upstream_request, timeout=600) as response:
                    if upstream_kind == "openai-chat":
                        upstream_body, wall_duration_ms = read_openai_chat_response(
                            response, started
                        )
                    else:
                        upstream_body = json.loads(response.read())
                        wall_duration_ms = (time.monotonic() - started) * 1000
                if not isinstance(upstream_body, dict):
                    raise ValueError("upstream response body must be an object")
                if upstream_kind == "ollama":
                    response_body = upstream_body
                elif upstream_kind == "openai-chat":
                    response_body = openai_ollama_payload(
                        request_body, upstream_body, wall_duration_ms
                    )
                else:
                    response_body = ollama_payload(request_body, upstream_body)
                if metrics_path is not None and not is_unload:
                    record = metric_record(
                        upstream_kind,
                        response_body,
                        wall_duration_ms,
                        request_body,
                    )
                    with metrics_lock, metrics_path.open("a", encoding="utf-8") as handle:
                        handle.write(json.dumps(record, separators=(",", ":")) + "\n")
                self.send_json(200, response_body)
            except urllib.error.HTTPError as error:
                detail = error.read().decode("utf-8", errors="replace")
                self.send_json(error.code, {"error": detail})
            except (OSError, ValueError, json.JSONDecodeError) as error:
                self.send_json(502, {"error": str(error)})

    return Handler


def self_test() -> int:
    request_body = {
        "model": "eval-alias",
        "prompt": "Return the required JSON object.",
        "format": {"type": "object", "required": ["summary"]},
        "think": False,
        "options": {"num_ctx": 32768, "num_predict": 64, "temperature": 0},
    }
    outbound = openai_chat_payload(request_body, 0.7, "local/mlx-model")
    assert outbound["model"] == "local/mlx-model"
    assert outbound["stream"] is True
    assert outbound["stream_options"] == {"include_usage": True}
    assert outbound["chat_template_kwargs"] == {"enable_thinking": False}
    assert "num_ctx" not in outbound
    assert "response_format" not in outbound

    events = [
        'data: {"model":"local/mlx-model","choices":[{"delta":{"role":"assistant"}}]}\n',
        'data: {"choices":[{"delta":{"content":"{\\\"summary\\\":"}}]}\n',
        'data: {"choices":[{"delta":{"content":"\\\"ok\\\"}"},"finish_reason":"stop"}]}\n',
        'data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}\n',
        "data: [DONE]\n",
    ]
    clock_values = iter((100.050, 100.200))
    collapsed, wall_ms = parse_openai_sse(
        events, 100.0, clock=lambda: next(clock_values)
    )
    assert collapsed["choices"][0]["message"]["content"] == '{"summary":"ok"}'
    assert collapsed["usage"]["prompt_tokens"] == 10
    assert round(wall_ms) == 200
    response = openai_ollama_payload(request_body, collapsed, wall_ms)
    assert response["prompt_eval_duration"] == 50_000_000
    assert response["eval_duration"] == 150_000_000
    record = metric_record("openai-chat", response, wall_ms, request_body)
    assert record["structured_output_mode"] == "prompt_constrained"
    assert record["timing_source"] == "observed_openai_stream"
    assert record["requested_context_tokens"] == 32768
    assert record["context_control"] == "model_or_server_startup_context"
    assert record["model_lifecycle"] == "server_process_owned"
    assert record["reasoning_control"] == "chat_template_enable_thinking_best_effort"
    assert round(record["prompt_tokens_per_second"]) == 200
    assert record["completion_throughput_tokens"] == 3
    assert round(record["completion_tokens_per_second"], 3) == 20.0

    native = capability_metadata("ollama", request_body)
    assert native["structured_output_mode"] == "backend_grammar_constrained"
    assert native["timing_source"] == "backend_native"
    print("local inference bridge self-test passed")
    return 0


def main() -> int:
    args = parse_args()
    if args.self_test:
        return self_test()
    metrics_path = args.metrics_jsonl.resolve() if args.metrics_jsonl else None
    if metrics_path is not None:
        metrics_path.parent.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(
        (args.host, args.port),
        build_handler(
            args.upstream,
            args.upstream_kind,
            args.default_temperature,
            args.cache_prompt,
            metrics_path,
            args.served_model,
            args.upstream_model or args.served_model,
            args.served_model_size_bytes,
        ),
    )
    print(
        f"[ollama-llama-bridge] listening on http://{args.host}:{args.port}; "
        f"upstream={args.upstream}; kind={args.upstream_kind}; "
        f"default_temperature={args.default_temperature}; "
        f"cache_prompt={args.cache_prompt}; metrics={metrics_path}",
        file=sys.stderr,
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
