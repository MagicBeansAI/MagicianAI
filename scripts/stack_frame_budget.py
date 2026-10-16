#!/usr/bin/env python3
"""Stack-frame budgets for the deep chat and resume paths (debug build, arm64).

Execution and HTTP workers run on the default 2 MiB stack and nothing grows a
stack. A debug build gives an async function's poll frame one slot per
temporary, never overlapped, so a long function or a wide match can reserve
hundreds of KiB on its own. Two such chains overflowed a worker on 2026-10-11
and 2026-10-12. This reads each listed function's frame from its arm64
prologue (`sub sp, sp, #N[, lsl #12]`, plus the stack-probe loop large frames
use) and fails when one exceeds its budget.

    scripts/stack_frame_budget.py [--binary magician.bin] [--list]

Budgets sit just above the values measured after the off-frame fix; raising one
should come with a reason in the commit. Measuring a function not listed:
`--match REGEX` prints the largest matching frames.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

KIB = 1024

# (symbol regex, budget in KiB). A regex may match several monomorphised or
# nested closures; each must fit.
BUDGETS: list[tuple[str, int]] = [
    # Resume after an answered hold (HTTP worker).
    (r"MagicianV2Api41resume_agentic_execution_with_scope_inner28\{closure\}17", 640),
    (r"MagicianV2Api39take_pause_by_storage_key_with_recovery28\{closure\}17", 160),
    (r"MagicianV2Api22take_pause_for_request28\{closure\}17", 80),
    (r"MagicianV2Api21spawn_detached_resume28\{closure\}17", 32),
    # Chat turn -> tool dispatch (execution worker).
    (r"ChatService30process_chat_inline_turn_inner28\{closure\}28\{closure\}17", 300),
    (r"ChatService30process_chat_inline_turn_inner28\{closure\}17", 160),
    (r"ChatService23dispatch_chat_tool_call28\{closure\}17", 160),
    (r"ChatService24process_chat_inline_turn28\{closure\}", 110),
    (r"ChatService21process_message_inner28\{closure\}17", 90),
    (r"HarnessCapabilityProvider.*CapabilityProvider.*7execute28\{closure\}17", 64),
    # Run task-local layers and plane dispatch: each layer must wrap a boxed
    # future. Unboxed, `scope` reached 1,419 KiB and SIGABRTed every VibeDev
    # engineer run (2026-10-14).
    (r"CapturedRunTaskLocals5scope28\{closure\}17", 48),
    (r"plane8dispatch27plane_tools_call_configured28\{closure\}", 32),
    (r"plane8dispatch30plane_execute_approved_capture28\{closure\}", 32),
    (r"god_mode14with_run_trust28\{closure\}17", 32),
    (r"god_mode22with_session_and_trust28\{closure\}17", 32),
    # The native run chain (execution worker). Together these frames reach
    # ~1.4 MiB of a 2 MiB worker before a tool runs, so one more task-local
    # layer on the run (`with_run_trust`) overflowed a god-mode chat run
    # mid-`shell` (2026-10-15). They may not grow; shrink them instead.
    (r"executor23execute_agentically_run28\{closure\}17", 360),
    (r"phases7resolve3run28\{closure\}17", 268),
    (r"StatelessArm23advance_iteration_inner28\{closure\}17", 164),
    (r"driver_worker19advance_under_lease28\{closure\}17", 156),
]

CLOSURE = re.compile(r"_\$u7b\$\$u7b\$closure\$u7d\$\$u7d\$")


def objdump() -> str:
    for candidate in ("llvm-objdump", "/opt/homebrew/opt/llvm/bin/llvm-objdump"):
        found = shutil.which(candidate) or (candidate if Path(candidate).exists() else None)
        if found:
            return found
    sys.exit("llvm-objdump not found (brew install llvm)")


def text_symbols(binary: Path) -> tuple[list[int], list[str]]:
    out = subprocess.run(["nm", "-n", str(binary)], capture_output=True, text=True, check=True).stdout
    addrs, names = [], []
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[1] in ("T", "t"):
            addrs.append(int(parts[0], 16))
            names.append(parts[2])
    return addrs, names


def frame_size(tool: str, binary: Path, start: int, end: int) -> int:
    stop = min(end, start + 160)
    out = subprocess.run(
        [tool, "-d", "--no-show-raw-insn", f"--start-address={hex(start)}",
         f"--stop-address={hex(stop)}", str(binary)],
        capture_output=True, text=True,
    ).stdout
    total = probe = 0
    for line in out.splitlines():
        m = re.search(r"sub\s+x9,\s*sp,\s*#(0x[0-9a-f]+|\d+),\s*lsl #12", line)
        if m:
            probe = int(m.group(1), 0) * 4096
            continue
        m = re.search(r"sub\s+sp,\s*sp,\s*#(0x[0-9a-f]+|\d+)(,\s*lsl #12)?", line)
        if m:
            value = int(m.group(1), 0) * (4096 if m.group(2) else 1)
            if probe and value == 4096:
                continue  # one step of the probe loop
            total += value
    return total + probe


def frames(tool, binary, addrs, names, pattern):
    regex = re.compile(pattern)
    for i, raw in enumerate(names):
        name = CLOSURE.sub("{closure}", raw)
        if regex.search(name):
            end = addrs[i + 1] if i + 1 < len(addrs) else addrs[i] + 400
            yield frame_size(tool, binary, addrs[i], end), name


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", type=Path, default=Path("magician.bin"))
    parser.add_argument("--match", help="print the largest frames matching REGEX and exit")
    parser.add_argument("--list", action="store_true", help="print every budgeted frame")
    args = parser.parse_args()
    tool = objdump()
    addrs, names = text_symbols(args.binary)

    if args.match:
        for size, name in sorted(frames(tool, args.binary, addrs, names, args.match), reverse=True)[:25]:
            print(f"{size // KIB:>6} KiB  {name[:180]}")
        return 0

    failures = 0
    for pattern, budget in BUDGETS:
        found = list(frames(tool, args.binary, addrs, names, pattern))
        if not found:
            print(f"MISSING  {pattern} (renamed? update the budget list)")
            failures += 1
            continue
        for size, name in found:
            over = size > budget * KIB
            failures += over
            if over or args.list:
                status = "OVER" if over else "ok"
                print(f"{status:<8} {size // KIB:>5} KiB / {budget} KiB  {name[:150]}")
    print("stack frames within budget" if not failures else f"{failures} frame(s) over budget or missing")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
