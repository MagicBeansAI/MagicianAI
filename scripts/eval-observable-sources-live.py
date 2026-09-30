#!/usr/bin/env python3
"""Capped, sanitized live evaluation for Phase 10 observable RSS sources."""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
from pathlib import Path
import socket
import sys
import time
from typing import Any
from urllib.parse import urlencode, urljoin, urlparse
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener
from xml.etree import ElementTree

import yaml


REPO_ROOT = Path(__file__).resolve().parents[1]
SKILLS = (
    REPO_ROOT / "skillshub" / "producthunt-search" / "SKILL.md",
    REPO_ROOT / "skillshub" / "arxiv-search" / "SKILL.md",
)
MAX_SKILL_BYTES = 256 * 1024
MAX_FEEDS = 4
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
MAX_API_BYTES = 512 * 1024
MAX_ENTRIES = 50


class EvalFailure(RuntimeError):
    pass


def public_url(value: str) -> str:
    parsed = urlparse(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise EvalFailure("observable target is not an HTTP(S) URL")
    if parsed.username or parsed.password:
        raise EvalFailure("observable target embeds credentials")
    host = parsed.hostname.rstrip(".").lower()
    if host == "localhost" or host.endswith((".localhost", ".local")):
        raise EvalFailure("observable target names a local host")
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        address = None
    if address is not None and not address.is_global:
        raise EvalFailure("observable target is a non-public IP address")
    return value


def resolve_public(value: str) -> None:
    parsed = urlparse(public_url(value))
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    addresses = {
        item[4][0]
        for item in socket.getaddrinfo(parsed.hostname, port, type=socket.SOCK_STREAM)
    }
    if not addresses:
        raise EvalFailure("observable target did not resolve")
    for raw in addresses:
        if not ipaddress.ip_address(raw).is_global:
            raise EvalFailure("observable target resolved to a non-public address")


class SafeRedirects(HTTPRedirectHandler):
    def redirect_request(self, req: Request, fp: Any, code: int, msg: str, headers: Any, newurl: str) -> Request:
        target = urljoin(req.full_url, newurl)
        resolve_public(target)
        if req.full_url.startswith("https://") and not target.startswith("https://"):
            raise EvalFailure("observable redirect downgraded HTTPS")
        return super().redirect_request(req, fp, code, msg, headers, target)


def read_bounded(response: Any, maximum: int) -> bytes:
    declared = response.headers.get("Content-Length")
    if declared and int(declared) > maximum:
        raise EvalFailure("response exceeded the configured byte cap")
    body = response.read(maximum + 1)
    if len(body) > maximum:
        raise EvalFailure("response exceeded the configured byte cap")
    return body


def fetch(url: str, timeout: float, maximum: int) -> tuple[bytes, dict[str, str]]:
    resolve_public(url)
    opener = build_opener(ProxyHandler({}), SafeRedirects())
    request = Request(url, headers={"User-Agent": "MagicianObservableSourceEval/1"})
    with opener.open(request, timeout=timeout) as response:
        public_url(response.geturl())
        return read_bounded(response, maximum), {
            "content_type": response.headers.get("Content-Type", "").split(";", 1)[0].lower(),
            "etag": response.headers.get("ETag", ""),
            "last_modified": response.headers.get("Last-Modified", ""),
        }


def parse_feed(body: bytes) -> tuple[int, int]:
    stripped = body.lstrip()
    if stripped.startswith(b"{"):
        payload = json.loads(body)
        if not isinstance(payload, dict) or not isinstance(payload.get("items"), list):
            raise EvalFailure("JSON Feed omitted its items array")
        entries = payload["items"][:MAX_ENTRIES]
        links = {
            str(item.get("url") or item.get("external_url"))
            for item in entries
            if isinstance(item, dict) and (item.get("url") or item.get("external_url"))
        }
        return len(entries), len(links)

    root = ElementTree.fromstring(body)
    entries = list(root.findall(".//item"))
    if not entries:
        entries = [element for element in root.iter() if element.tag.rsplit("}", 1)[-1] == "entry"]
    entries = entries[:MAX_ENTRIES]
    links: set[str] = set()
    for entry in entries:
        for child in entry.iter():
            if child.tag.rsplit("}", 1)[-1] != "link":
                continue
            value = child.attrib.get("href") or (child.text or "").strip()
            if value:
                links.add(value)
                break
    return len(entries), len(links)


def load_manifests() -> list[dict[str, Any]]:
    sources: list[dict[str, Any]] = []
    for path in SKILLS:
        with path.open("rb") as handle:
            raw = handle.read(MAX_SKILL_BYTES + 1)
        if len(raw) > MAX_SKILL_BYTES:
            raise EvalFailure(f"{path} exceeds the bounded SKILL.md size")
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise EvalFailure(f"{path} is not UTF-8") from error
        if not text.startswith("---\n"):
            raise EvalFailure(f"{path} omits YAML frontmatter")
        end = text.find("\n---\n", 4)
        if end < 0:
            raise EvalFailure(f"{path} has unterminated YAML frontmatter")
        frontmatter = yaml.safe_load(text[4:end])
        try:
            payload = frontmatter["metadata"]["magician"]["observe_source"]
        except (KeyError, TypeError) as error:
            raise EvalFailure(
                f"{path} omits metadata.magician.observe_source"
            ) from error
        if not isinstance(payload, dict) or set(payload) != {"schema_version", "source", "profiles"}:
            raise EvalFailure(f"{path} has a non-strict Observe extension")
        if payload["schema_version"] != 1:
            raise EvalFailure(f"{path} has an unsupported Observe schema version")
        source = payload.get("source")
        profiles = payload.get("profiles")
        if not isinstance(source, dict) or not isinstance(profiles, list):
            raise EvalFailure(f"{path} omits source/profile data")
        for profile in profiles:
            if not isinstance(profile, dict) or "observe" not in profile.get("surfaces", []):
                continue
            acquisition = profile.get("acquisition", {})
            limits = profile.get("limits", {})
            actions = acquisition.get("allowed_actions")
            targets = acquisition.get("targets")
            if (
                profile.get("operation") != "discover"
                or not profile.get("discoverable")
                or not profile.get("unattended")
                or not profile.get("read_only")
                or actions != ["rss.discover"]
                or acquisition.get("escalation") != "none"
                or acquisition.get("ladder") is not None
                or not isinstance(targets, list)
                or not targets
                or len(targets) > 16
                or not 1 <= int(limits.get("max_selected_per_run", 0)) <= int(limits.get("max_candidates_per_run", 0)) <= 200
            ):
                raise EvalFailure(f"{path} has an unsafe Observe profile")
            sources.append(
                {
                    "source_id": str(source.get("id")),
                    "profile_id": str(profile.get("id")),
                    "targets": [public_url(str(target)) for target in targets],
                }
            )
    if not sources or len(sources) > MAX_FEEDS:
        raise EvalFailure("shipped observable source count is outside evaluation bounds")
    return sources


def api_page(base_url: str, cursor: str | None, timeout: float) -> dict[str, Any]:
    query: dict[str, str | int] = {
        "required_action": "rss.discover",
        "readiness": "eligible",
        "limit": 100,
    }
    if cursor:
        query["cursor"] = cursor
    url = f"{base_url.rstrip('/')}/api/magician/v2/observe/sources?{urlencode(query)}"
    request = Request(
        url,
        headers={
            "Accept": "application/json",
            **(
                {"Authorization": f"Bearer {os.environ['MAGICIAN_BEARER_TOKEN'].strip()}"}
                if os.environ.get("MAGICIAN_BEARER_TOKEN", "").strip()
                else {}
            ),
        },
    )
    opener = build_opener(ProxyHandler({}))
    with opener.open(request, timeout=timeout) as response:
        payload = json.loads(read_bounded(response, MAX_API_BYTES))
    if not isinstance(payload, dict) or not isinstance(payload.get("items"), list):
        raise EvalFailure("Observe source API returned an invalid page")
    return payload


def inspect_api(base_url: str, timeout: float, required: set[str]) -> dict[str, Any]:
    cursor: str | None = None
    offer_ids: set[str] = set()
    source_ids: set[str] = set()
    totals: set[int] = set()
    pages = 0
    while True:
        page = api_page(base_url, cursor, timeout)
        pages += 1
        if pages > 32:
            raise EvalFailure("Observe source API pagination exceeded its page cap")
        totals.add(int(page.get("total", -1)))
        for item in page["items"]:
            if not isinstance(item, dict) or item.get("readiness") != "eligible":
                raise EvalFailure("Observe source API returned a non-eligible row")
            bindings = item.get("action_bindings")
            if not isinstance(bindings, list) or len(bindings) != 1 or bindings[0].get("action_id") != "rss.discover":
                raise EvalFailure("Observe source API widened the exact RSS action")
            offer_id = str(item.get("offer_id"))
            if offer_id in offer_ids:
                raise EvalFailure("Observe source API repeated an offer across pages")
            offer_ids.add(offer_id)
            source_ids.add(str(item.get("source_id")))
        cursor = page.get("next_cursor")
        if not cursor:
            break
    if len(totals) != 1 or totals != {len(offer_ids)}:
        raise EvalFailure("Observe source API total does not match cursor pages")
    missing = sorted(required - source_ids)
    if missing:
        raise EvalFailure(f"Observe source API omitted shipped sources: {', '.join(missing)}")
    return {"pages": pages, "offer_count": len(offer_ids), "total": next(iter(totals))}


def self_test() -> None:
    rss = b"<rss><channel><item><guid>1</guid><link>https://example.com/a</link></item><item><guid>2</guid><link>https://example.com/a</link></item></channel></rss>"
    atom = b"<feed xmlns='http://www.w3.org/2005/Atom'><entry><id>1</id><link href='https://example.com/b'/></entry></feed>"
    assert parse_feed(rss) == (2, 1)
    assert parse_feed(atom) == (1, 1)
    try:
        parse_feed(b"not a feed")
    except Exception:
        pass
    else:
        raise EvalFailure("malformed feed fixture was accepted")
    sources = load_manifests()
    assert {source["source_id"] for source in sources} == {"product-hunt", "arxiv-ai"}
    print("observable-sources live evaluator self-test passed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--allow-network", action="store_true")
    parser.add_argument("--api-base-url", default="http://127.0.0.1:3002")
    parser.add_argument("--timeout-secs", type=float, default=20.0)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0

    sources = load_manifests()
    if args.dry_run:
        report = {
            "schema_version": 1,
            "status": "planned",
            "source_count": len(sources),
            "network_allowed": False,
            "api_base_host": urlparse(args.api_base_url).hostname,
        }
    else:
        if not args.allow_network:
            raise EvalFailure("live observable-source evaluation requires --allow-network")
        feed_results: list[dict[str, Any]] = []
        for source in sources:
            for target in source["targets"]:
                started = time.monotonic()
                body, headers = fetch(target, args.timeout_secs, MAX_RESPONSE_BYTES)
                item_count, unique_links = parse_feed(body)
                if item_count == 0:
                    raise EvalFailure(f"{source['source_id']} returned no feed entries")
                feed_results.append(
                    {
                        "source_id": source["source_id"],
                        "target_host": urlparse(target).hostname,
                        "item_count_capped": item_count,
                        "unique_link_count_capped": unique_links,
                        "latency_ms": round((time.monotonic() - started) * 1000),
                        "content_type": headers["content_type"],
                        "etag_present": bool(headers["etag"]),
                        "last_modified_present": bool(headers["last_modified"]),
                    }
                )
        api_result = inspect_api(
            args.api_base_url,
            args.timeout_secs,
            {source["source_id"] for source in sources},
        )
        report = {
            "schema_version": 1,
            "status": "passed",
            "source_count": len(sources),
            "feed_results": feed_results,
            "observe_api": api_result,
        }

    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (EvalFailure, OSError, ValueError, ElementTree.ParseError, json.JSONDecodeError) as error:
        print(f"observable-sources live evaluation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
