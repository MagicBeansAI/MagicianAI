#!/usr/bin/env python3
"""Measure one decision model through the real decision-engine binary.

    scripts/bench-decision-engine.py --adapter kev-onnx --model kev-4b-q8f32 \
        --model-dir kev-4b [--root DIR] [--requests 40]

Starts the engine on a temp socket with a one-model `decision-engine.yaml`
(one model on the shared action operation), sends distinct action requests
(no state-cache hits), and reports:

- load: engine start to its first action response (model load + first call);
- p50 / p95 / max latency of the rest, as the engine reports it;
- RSS after the first step, at the end, and at peak (`/usr/bin/time -l`).

Observations and authorized catalogs are synthetic Browser/Android-shaped
fixtures, each with its own goal. Returned calls are never executed. `--root` is the runtime root; a relative
`--model-dir` is a folder under its `models/decision` (the engine's
`models_dir` default) and ONNX Runtime is its `lib/onnxruntime`; hosted
models read their key from the environment (`--api-key-env`).
"""

import argparse
import http.client
import json
import os
import random
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time


class UnixConnection(http.client.HTTPConnection):
    def __init__(self, path, timeout):
        super().__init__("engine", timeout=timeout)
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def call(sock, method, path, body=None, timeout=120):
    conn = UnixConnection(sock, timeout)
    headers = {"content-type": "application/json"} if body is not None else {}
    conn.request(method, path, body=json.dumps(body) if body is not None else None, headers=headers)
    response = conn.getresponse()
    data = response.read()
    conn.close()
    return json.loads(data)


APPS = ["com.whatsapp", "com.android.settings", "com.google.android.gm", "com.spotify.music"]
WORDS = ["Search", "Alice", "Bob", "Settings", "Wi-Fi", "Bluetooth", "Inbox", "Compose",
         "Play", "Pause", "Library", "Profile", "Notifications", "Archive", "Send", "Back"]


def action_request(rng, index, locality, contract_version):
    android = index % 2 == 0
    elements = [{"target": str(i) if android else f"@e{i}",
                 "label": f"{rng.choice(WORDS)} {i}", "role": "button"}
                for i in range(rng.randint(6, 60))]
    selected = rng.choice(elements)
    return {
        "contract_version": contract_version, "snapshot": f"fixture:{index}", "locality": locality,
        "context": {"goal": f"Click {selected['label']} (task {index})",
                    "observation": {"elements": elements},
                    "evidence": [{"id": f"snapshot:{index}", "value": {"elements": elements}, "succeeded": True}]},
        "tools": [{"name": "android_fixture__tap" if android else "browser_fixture__click",
                   "description": "Click a button identified by its target in the current snapshot.",
                   "parameters": {"type": "object", "properties": {"target": {"type": "string"}},
                                  "required": ["target"], "additionalProperties": False}}],
    }


def settings(args):
    entry = {"adapter": args.adapter, "model": args.model}
    if args.model_dir:
        entry["model_dir"] = args.model_dir
    if args.api_key_env:
        entry["api_key_env"] = args.api_key_env
    if args.threads:
        entry["threads"] = args.threads
    return {"enabled": True, "models": {"m": entry}, "operations": {
        "tool_action_judge": {"model": "m", "pack": "tool_action_judge", "pack_version": "1.0.0",
                              "sees_body": True, "gate": {"enabled": True}, "action_timeout_ms": 30000,
                              "thresholds": {"next_action_confidence": 0.9, "evidence_sufficient": 0.9,
                                             "action_applicable": 0.9}}}}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", default=os.environ.get("DECISION_ENGINE_BIN", "decision-engine.bin"))
    parser.add_argument("--root", default=os.environ.get("MAGICIAN_ROOT_DIR"))
    parser.add_argument("--adapter")
    parser.add_argument("--model")
    parser.add_argument("--config", help="use this decision-engine.yaml instead of a one-model one "
                        "(the report's model name is then the file name)")
    parser.add_argument("--model-dir")
    parser.add_argument("--api-key-env")
    parser.add_argument("--threads", type=int)
    parser.add_argument("--requests", type=int, default=40)
    parser.add_argument("--startup-timeout", type=float, default=600)
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--locality", choices=["local", "cloud"], default="local",
                        help="cloud for a hosted model: local mode keeps page text on loopback models")
    args = parser.parse_args()
    if not args.config and not (args.adapter and args.model):
        parser.error("--adapter and --model, or --config")
    if args.config:
        args.model = os.path.basename(args.config)

    rng = random.Random(args.seed)
    work = tempfile.mkdtemp(prefix="deb.", dir="/tmp")  # socket paths are capped near 104 bytes
    config = args.config or os.path.join(work, "decision-engine.yaml")
    if not args.config:
        with open(config, "w") as out:
            json.dump(settings(args), out)  # JSON is YAML
    sock = os.path.join(work, "s")
    env = dict(os.environ)
    if args.root:
        env["MAGICIAN_ROOT_DIR"] = args.root
    env.setdefault("RUST_LOG", "warn")
    timing = os.path.join(work, "time")
    started = time.monotonic()
    proc = subprocess.Popen(
        ["/usr/bin/time", "-l", "-o", timing, args.engine, "--config", config, "--socket", sock],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,  # its own group, so cleanup reaches the engine too
    )
    try:
        ready_by = time.monotonic() + args.startup_timeout
        while time.monotonic() < ready_by:
            if proc.poll() is not None:
                sys.exit("engine exited during startup")
            if os.path.exists(sock):
                break
            time.sleep(0.05)
        else:
            sys.exit("engine did not open its socket within --startup-timeout")
        contract_version = call(sock, "GET", "/health")["contract_version"]
        latencies, statuses, rss_idle = [], {}, None
        for index in range(args.requests + 1):
            reply = call(sock, "POST", "/v1/action", action_request(rng, index, args.locality, contract_version))
            status = reply["verdict"]["outcome"]
            statuses[status] = statuses.get(status, 0) + 1
            if not reply.get("model"):
                print(f"  request {index}: {status} {reply.get('reason')}", file=sys.stderr)
            if index == 0:
                load_s = time.monotonic() - started
                engine_pid = next(iter(child_pids(proc.pid)), None)
                rss_idle = rss_mb(engine_pid)
            elif reply.get("model"):
                latencies.append(reply["latency_ms"])
        rss_end = rss_mb(engine_pid)
    finally:
        # Stop the engine first — `time` reports the peak when it exits —
        # then the whole group, so an error or ^C never leaves one running.
        for pid in child_pids(proc.pid):
            os.kill(pid, signal.SIGTERM)
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    peak = footprint = None
    if os.path.exists(timing):
        report = open(timing).read()
        match = re.search(r"(\d+)\s+maximum resident set size", report)
        peak = int(match.group(1)) / 1024 / 1024 if match else None
        # macOS: the physical footprint Activity Monitor shows, which counts
        # GPU (Metal) buffers that RSS does not.
        match = re.search(r"(\d+)\s+peak memory footprint", report)
        footprint = int(match.group(1)) / 1024 / 1024 if match else None
    shutil.rmtree(work, ignore_errors=True)
    latencies.sort()
    q = statistics.quantiles(latencies, n=20) if len(latencies) >= 2 else [float("nan")] * 19
    row = {
        "model": args.model, "threads": args.threads or "default", "requests": len(latencies),
        "statuses": statuses, "load_s": round(load_s, 2),
        "p50_ms": round(statistics.median(latencies)) if latencies else None,
        "p95_ms": round(q[18]) if latencies else None, "max_ms": latencies[-1] if latencies else None,
        "rss_first_mb": round(rss_idle) if rss_idle else None,
        "rss_end_mb": round(rss_end) if rss_end else None,
        "rss_peak_mb": round(peak) if peak else None,
        "footprint_peak_mb": round(footprint) if footprint else None,
    }
    print(json.dumps(row))


def child_pids(pid):
    out = subprocess.run(["pgrep", "-P", str(pid)], capture_output=True, text=True).stdout
    return [int(p) for p in out.split()]


def rss_mb(pid):
    if not pid:
        return None
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out) / 1024 if out else None


if __name__ == "__main__":
    main()
