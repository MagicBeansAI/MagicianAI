#!/usr/bin/env python3
"""resolve.py — resolve everything agent-browser needs to drive a
CloakBrowser session, without going through the Playwright SDK launch path.

We use the cloakbrowser pip package only as a thin library for:
  - binary path + version  (cloakbrowser.binary_info)
  - default stealth Chromium flags  (cloakbrowser.get_default_stealth_args)
  - geoip-from-proxy resolution     (cloakbrowser.maybe_resolve_geoip)

We do NOT launch a browser. The Playwright SDK launch path silently
re-introduces JS-layer leaks (navigator.webdriver=true, HeadlessChrome
UA) because Chromium auto-sets webdriver when CDP attaches the way
Playwright connects. agent-browser launches the binary itself with a
flag set that keeps the C++ patches in the binary active.

Usage:
  resolve.py [--proxy URL] [--profile DIR] [--headed]
                   [--humanize] [--user-agent UA] [--no-geoip]
                   [--init-script PATH]... [--extra-arg FLAG]...

Output: a single JSON object on stdout. Shape:
  {
    "binary_path": "/Users/.../Chromium",
    "version":     "150.0.7871.114.3",
    "args":        ["--no-sandbox", "--fingerprint=12345",
                    "--fingerprint-platform=macos",
                    "--fingerprint-webrtc-ip=...",   # if proxy + geoip
                    "--proxy-server=...",            # if proxy
                    "--lang=en-US",                  # if geoip resolved
                    "--user-data-dir=/path"],        # if profile set
    "user_agent": "Mozilla/5.0 ... Chrome/146.0 ..." | null,
    "init_scripts": ["/abs/path/to/stealth-init.js"],
    "headed":     false,
    "geoip": { "timezone": "...", "locale": "...", "exit_ip": "..." } | null,
    "env": { "TZ": "..." }     # only set when timezone resolved
  }

Errors go to stderr with a non-zero exit. The JSON contract is exit-code-0
or nothing; the caller can rely on `if exit_code == 0: parse(stdout)`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import sys
import tempfile
import textwrap
from pathlib import Path
from typing import Any, Optional


def _die(message: str, code: int = 1) -> None:
    sys.stderr.write(f"cloak: {message}\n")
    sys.exit(code)


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Resolve CloakBrowser config for agent-browser.")
    parser.add_argument("--proxy", default=None,
                        help="http(s)://user:pass@host:port or socks5://...")
    parser.add_argument("--profile", default=None,
                        help="persistent profile dir (Chromium --user-data-dir)")
    parser.add_argument("--headed", action="store_true",
                        help="emit headed=true; up to the runtime to translate to AGENT_BROWSER_HEADED")
    parser.add_argument("--humanize", action="store_true",
                        help="(currently a no-op flag — humanize requires SDK-level "
                             "interception of mouse/keyboard calls that external CDP drivers "
                             "bypass; we record the request so future work can detect demand)")
    parser.add_argument("--user-agent", default=None,
                        help="explicit UA override; not normally needed because the binary "
                             "patches HeadlessChrome out when launched directly")
    parser.add_argument("--no-geoip", action="store_true",
                        help="skip geoip resolution even when --proxy is set")
    parser.add_argument("--init-script", action="append", default=[],
                        dest="init_scripts",
                        help="extra init script path (repeatable). The bundled stealth-init.js "
                             "is always included.")
    parser.add_argument("--extra-arg", action="append", default=[],
                        dest="extra_args",
                        help="extra Chromium flag to append (repeatable)")
    return parser.parse_args(argv)


def _resolve_stealth_init_path() -> Path:
    """Path to the JS stealth shim, packaged alongside this script."""
    here = Path(__file__).resolve().parent
    candidate = here / "stealth-init.js"
    if not candidate.is_file():
        _die(f"stealth-init.js missing at {candidate}")
    return candidate


def _resolve_binary_info() -> dict[str, Any]:
    try:
        from cloakbrowser import binary_info
    except ImportError as exc:
        _die(f"cloakbrowser SDK not importable from {sys.executable}: {exc}. "
             f"Run `make -C skillshub setup-python` to install.")
    info = binary_info()
    if not isinstance(info, dict) or not info.get("installed") or not info.get("binary_path"):
        _die("CloakBrowser binary not installed. Run `make -C skillshub setup-cloak-browser`.")
    return info


def _resolve_stealth_args() -> list[str]:
    """Pull the default fingerprint flags Cloak ships. These are pure
    Chromium command-line args and will be passed through agent-browser's
    `--args` mechanism."""
    try:
        from cloakbrowser import get_default_stealth_args
    except ImportError:
        _die("cloakbrowser SDK not importable when fetching default stealth args.")
    args = list(get_default_stealth_args())
    # The SDK re-rolls the fingerprint seed at module import time. Force a
    # fresh seed per session so two consecutive sessions don't share a
    # fingerprint (which would cluster them in detector telemetry).
    args = [a for a in args if not a.startswith("--fingerprint=")]
    args.append(f"--fingerprint={random.randint(1, 1_000_000)}")
    return args


def _maybe_resolve_geoip(proxy: Optional[str]) -> Optional[dict[str, Any]]:
    if not proxy:
        return None
    try:
        from cloakbrowser import maybe_resolve_geoip
    except ImportError:
        _die("cloakbrowser SDK not importable when resolving geoip.")
    timezone, locale, exit_ip = maybe_resolve_geoip(True, proxy, None, None)
    if not (timezone or locale or exit_ip):
        return None
    return {
        "timezone": timezone,
        "locale": locale,
        "exit_ip": exit_ip,
    }


def _build_locale_tz_init_script(timezone: Optional[str], locale: Optional[str]) -> Optional[Path]:
    """Generate a JS init script that overrides navigator.language / languages
    and best-effort spoofs Intl.DateTimeFormat timezone.

    Why this exists: Chromium's `--accept-lang=` and `--lang=` flags don't
    reliably propagate to `navigator.language` in all builds, and there is
    no Chromium command-line flag to set the JS Intl timezone (only the
    UI's clock zone). CDP `Emulation.setTimezoneOverride` is the canonical
    fix for timezone but requires agent-browser to issue it; we substitute
    a JS prototype patch as best effort. Plain navigator.language override
    works fine.

    Cached per (timezone, locale) tuple in tempdir so consecutive sessions
    with the same geoip don't churn the filesystem."""
    if not (timezone or locale):
        return None
    key = hashlib.sha1(f"{timezone}|{locale}".encode()).hexdigest()[:16]
    cache_dir = Path(tempfile.gettempdir()) / "cloak-init-scripts"
    cache_dir.mkdir(parents=True, exist_ok=True)
    path = cache_dir / f"locale-tz-{key}.js"
    if path.is_file():
        return path

    tz_literal = json.dumps(timezone or "")
    locale_literal = json.dumps(locale or "")
    # Build a likely languages array: ["xx-XX", "xx", "en"] without dupes.
    languages_list: list[str] = []
    if locale:
        languages_list.append(locale)
        if "-" in locale:
            languages_list.append(locale.split("-", 1)[0])
    if "en" not in [l.split("-", 1)[0] for l in languages_list]:
        languages_list.append("en")
    langs_literal = json.dumps(languages_list)

    body = textwrap.dedent(f"""\
        // Auto-generated by resolve.py — geoip-derived locale + timezone.
        // Cached at {path}
        (() => {{
          const TZ = {tz_literal};
          const LOCALE = {locale_literal};
          const LANGS = {langs_literal};

          if (LOCALE) {{
            try {{
              Object.defineProperty(Navigator.prototype, 'language', {{
                get: () => LOCALE, configurable: true,
              }});
              Object.defineProperty(Navigator.prototype, 'languages', {{
                get: () => LANGS, configurable: true,
              }});
            }} catch (e) {{}}
          }}

          if (TZ) {{
            // Best-effort Intl override. Chrome may resist overriding the
            // prototype in some builds; the canonical fix is CDP
            // Emulation.setTimezoneOverride from the driver side.
            try {{
              const _resOpts = Intl.DateTimeFormat.prototype.resolvedOptions;
              Intl.DateTimeFormat.prototype.resolvedOptions = function () {{
                const opts = _resOpts.call(this);
                opts.timeZone = TZ;
                return opts;
              }};
            }} catch (e) {{}}
          }}
        }})();
        """)
    path.write_text(body)
    return path


def main(argv: list[str]) -> int:
    args = _parse_args(argv)

    binary = _resolve_binary_info()
    chromium_args = _resolve_stealth_args()
    init_scripts: list[str] = [str(_resolve_stealth_init_path())]
    init_scripts.extend(str(Path(p).resolve()) for p in args.init_scripts)

    geoip: Optional[dict[str, Any]] = None
    if args.proxy and not args.no_geoip:
        try:
            geoip = _maybe_resolve_geoip(args.proxy)
        except Exception as exc:
            # Soft-fail geoip — proxy may still be usable without timezone hints.
            sys.stderr.write(f"cloak: geoip resolution failed (continuing): {exc}\n")

    if args.proxy:
        chromium_args.append(f"--proxy-server={args.proxy}")
    if args.profile:
        profile_path = Path(args.profile).expanduser().resolve()
        profile_path.mkdir(parents=True, exist_ok=True)
        chromium_args.append(f"--user-data-dir={profile_path}")
    if geoip:
        loc = geoip.get("locale")
        if loc:
            # Both --lang (UI) and --accept-lang (Accept-Language header).
            # Chromium changed which flag drives navigator.language across
            # versions; setting both is the safe play.
            chromium_args.append(f"--lang={loc}")
            chromium_args.append(f"--accept-lang={loc}")
        if geoip.get("exit_ip"):
            # Only set --fingerprint-webrtc-ip if not already overridden by user
            if not any(a.startswith("--fingerprint-webrtc-ip") for a in chromium_args + args.extra_args):
                chromium_args.append(f"--fingerprint-webrtc-ip={geoip['exit_ip']}")
        # Geoip-driven JS init script for navigator.language + Intl timezone
        locale_tz_script = _build_locale_tz_init_script(
            geoip.get("timezone"), geoip.get("locale"),
        )
        if locale_tz_script is not None:
            init_scripts.append(str(locale_tz_script))
    # User-supplied flags come last so they win on conflict
    chromium_args.extend(args.extra_args)

    env: dict[str, str] = {}
    if geoip and geoip.get("timezone"):
        # TZ env var doesn't reliably affect Chromium on macOS, but agent-browser
        # itself reads it and Linux Chromium honors it — set anyway for free.
        env["TZ"] = geoip["timezone"]

    out = {
        "binary_path": binary["binary_path"],
        "version": binary.get("version"),
        "platform": binary.get("platform"),
        "args": chromium_args,
        "user_agent": args.user_agent,
        "init_scripts": init_scripts,
        "headed": bool(args.headed),
        "humanize_requested": bool(args.humanize),
        "geoip": geoip,
        "env": env,
    }
    json.dump(out, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
