#!/usr/bin/env python3
"""Guard the Android device bridge's bounded MCP contract.

The wire is intentionally asymmetric: Kotlin hand-rolls a tools-only server and
Rust uses the governed rmcp client. Comparing serializers would test nothing,
because only one side has local protocol types. This guard instead pins the
small cross-language contract and the deletion boundary that keeps a second
transport from growing back unnoticed.
"""

from __future__ import annotations

import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
KOTLIN_SERVER = ROOT / (
    "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/"
    "mcp/MagdroidMcpServer.kt"
)
KOTLIN_PROTOCOL = ROOT / (
    "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/"
    "mcp/McpProtocol.kt"
)
RUST_CLIENT = ROOT / "magician-mcp-client/src/client.rs"
RUST_CONFIG = ROOT / "magician-mcp-client/src/config.rs"
RUST_HUB = ROOT / "magician/src/magician_v2/device_bridge.rs"

LEGACY_PATHS = (
    ROOT
    / "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/bridge/BridgeProtocol.kt",
    ROOT
    / "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/mcp/McpHttpServer.kt",
    ROOT
    / "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/mcp/McpAuthManager.kt",
    ROOT
    / "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/mcp/McpNetworkUtils.kt",
    ROOT / "magician/src/magician_v2/execution/embedded_pack_defs/android_tools.yaml",
)


def read(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(f"missing required file: {path.relative_to(ROOT)}")
    return path.read_text(encoding="utf-8")


def require_all(
    failures: list[str], path: Path, text: str, needles: tuple[str, ...]
) -> None:
    missing = [needle for needle in needles if needle not in text]
    if missing:
        failures.append(
            f"{path.relative_to(ROOT)} is missing contract markers: {missing}"
        )


def run() -> list[str]:
    failures: list[str] = []
    server = read(KOTLIN_SERVER)
    protocol = read(KOTLIN_PROTOCOL)
    client = read(RUST_CLIENT)
    config = read(RUST_CONFIG)
    hub = read(RUST_HUB)

    require_all(
        failures,
        KOTLIN_SERVER,
        server,
        (
            'PROTOCOL_VERSION: String = "2026-07-28"',
            '"server/discover"',
            '"tools/list"',
            '"tools/call"',
            '"subscriptions/listen"',
            '"notifications/subscriptions/acknowledged"',
            '"notifications/tools/list_changed"',
            'put("listChanged", true)',
            "MAX_REQUEST_BYTES",
        ),
    )
    require_all(
        failures,
        KOTLIN_PROTOCOL,
        protocol,
        (
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ),
    )
    require_all(
        failures,
        RUST_CONFIG,
        config,
        ("DuplexJsonTransportConfig", "DuplexJson(DuplexJsonTransportConfig)"),
    )
    require_all(
        failures,
        RUST_CLIENT,
        client,
        (
            "McpTransportConfig::DuplexJson",
            "ClientLifecycleMode::Discover",
            "ProtocolVersion::V_2026_07_28",
        ),
    )
    require_all(
        failures,
        RUST_HUB,
        hub,
        (
            "open_notification_subscription",
            "enable_tools_list_changed",
            "with_tools_list_changed",
            "DeviceConnectionId",
            "refresh_tools_if_invalidated",
        ),
    )

    present = [str(path.relative_to(ROOT)) for path in LEGACY_PATHS if path.exists()]
    if present:
        failures.append(f"legacy device transports or discovery packs returned: {present}")

    return failures


def main() -> int:
    failures = run()
    if failures:
        print("Android MCP device bridge drift:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "Android MCP device bridge guard passed: discover-only rmcp client, "
        "bounded Kotlin tools server, annotations, subscription, and legacy deletions agree"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
