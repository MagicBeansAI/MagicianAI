"""Shared preflight for the channel-assist model evals (eval-channel-llm.py,
golden-eval-channel.py).

These evals must OWN ollama exclusively: the magician runtime stack uses the same
local ollama daemon, so running an eval alongside it competes for RAM/GPU and can
OOM-crash ollama (which then takes the live distill/classify pipeline down too).

`preflight()` enforces that: the magician stack must be DOWN. It bails by default
if the stack is up, or (with stop_stack=True) stops it via `make stop-supervisor`
— which also stops the Magician-owned ollama — and then (re)starts ollama via
`make run-ollama` so the eval has a clean, exclusive daemon.
"""

from __future__ import annotations

import os
import socket
import subprocess
import sys
import time
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# magician, magicutor, supervisor-control — if any is listening, the stack is up.
STACK_PORTS = {3002: "magician", 3003: "magicutor", 8081: "supervisor"}


def _port_open(port: int, host: str = "127.0.0.1", timeout: float = 1.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def stack_running() -> list:
    return [name for port, name in STACK_PORTS.items() if _port_open(port)]


def ollama_up(base_url: str) -> bool:
    try:
        urllib.request.urlopen(f"{base_url.rstrip('/')}/api/tags", timeout=3)
        return True
    except Exception:
        return False


def preflight(base_url: str, stop_stack: bool, require_ollama: bool = True) -> bool:
    """Guarantee the eval owns its runner. Returns True iff it stopped the stack (so
    the caller can remind the user to restart it).

    require_ollama=False is for candidates that do not run on ollama at all (Apple's
    on-device model via apfel). The stack must still be DOWN so the machine is as
    quiet as it was for the recorded ollama baseline, but starting ollama would only
    add an idle 11GB resident model competing for memory bandwidth."""
    stopped = False
    up = stack_running()
    if up and not require_ollama and not stop_stack:
        # An apple:* candidate runs on the Neural Engine and never touches ollama, so
        # the exclusivity rationale above does not apply: there is no shared daemon to
        # OOM. A live stack only adds CPU noise, which skews latency and nothing else.
        print(f"WARNING: the magician stack is up ({', '.join(up)}). Harmless for an "
              f"apple-only run (no shared ollama), but wall-clock latency will be noisy "
              f"— agreement scores are unaffected. Use --stop-stack for a quiet machine.\n",
              flush=True)
        return stopped
    if up:
        who = ", ".join(up)
        if not stop_stack:
            sys.exit(
                f"Refusing to run: the magician stack is up ({who}). This eval needs exclusive "
                f"ollama access — running it alongside the stack competes for RAM/GPU and can "
                f"OOM-crash ollama. Stop the stack first (`make stop-supervisor`) or re-run with "
                f"--stop-stack."
            )
        print(f"stack is up ({who}); stopping it via `make stop-supervisor` ...", flush=True)
        subprocess.run(["make", "stop-supervisor"], cwd=REPO, check=False)
        for _ in range(60):
            if not stack_running():
                break
            time.sleep(1)
        else:
            sys.exit("stack did not stop within 60s; aborting.")
        stopped = True
        print("stack stopped.", flush=True)
    if not require_ollama:
        print("apple-only run: ollama not required; the magician stack is down. Proceeding.\n", flush=True)
        return stopped
    # `make stop-supervisor` also stops the Magician-owned ollama; (re)start it.
    if not ollama_up(base_url):
        print("ollama is not reachable; starting it via `make run-ollama` ...", flush=True)
        subprocess.run(["make", "run-ollama"], cwd=REPO, check=False)
        for _ in range(30):
            if ollama_up(base_url):
                break
            time.sleep(2)
        else:
            sys.exit("ollama did not come up; check `make run-ollama` output / logs.")
    print("ollama is up; the magician stack is down. Proceeding.\n", flush=True)
    return stopped
