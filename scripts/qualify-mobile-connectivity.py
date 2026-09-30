#!/usr/bin/env python3
"""Serial pairing protocol probes against an already-running isolated backend.

This is NOT physical-device acceptance. Enroll/revoke mutate only uniquely named
test devices. Between enroll and verify, an operator can restart the backend and
its Secret Service to test durable credentials without rebuilding anything.
State contains credentials: keep it private and outside reports/version control.
"""
from __future__ import annotations

import argparse
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import stat
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid

# Reuse the container harness's origin validation and redirect refusal.
spec = importlib.util.spec_from_file_location(
    "container_harness", Path(__file__).with_name("qualify-container-integration.py"))
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)
require = harness.require
SCHEMA = "magician.mobile-protocol.v1"


def request(origin, path, *, method="GET", body=None, headers=None, expected=200):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), harness.NoRedirect())
    req = urllib.request.Request(origin + path, method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "User-Agent": "MagicanConnectivityProbe/1.0",
                 **(headers or {})})
    try:
        response = opener.open(req, timeout=15)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        require(response.code == expected, f"{method} {path}: HTTP {response.code}, expected {expected}")
        raw = response.read(1024 * 1024 + 1)
        require(len(raw) <= 1024 * 1024, "response exceeds 1 MiB")
        if expected != 200:
            return {}
        result = json.loads(raw)
        require(isinstance(result, dict), "expected a JSON object")
        if method == "POST" and path.startswith("/api/magician/v2/devices/"):
            require("no-store" in response.headers.get("Cache-Control", ""),
                    "pairing capability response is cacheable")
        return result


def private_state(path, *, create=False):
    parent = path.parent.stat()
    require(parent.st_uid == os.getuid() and not parent.st_mode & 0o077,
            "state parent must be owned by this user with mode 0700")
    flags = os.O_NOFOLLOW | os.O_NONBLOCK | (os.O_WRONLY | os.O_CREAT | os.O_EXCL if create else os.O_RDONLY)
    fd = os.open(path, flags, 0o600)
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        os.close(fd)
        raise harness.ProbeError("state must be a private, owned regular file")
    return os.fdopen(fd, "w" if create else "r")


def save(state, stream):
    stream.seek(0)
    json.dump(state, stream)
    stream.truncate()
    stream.flush()
    os.fsync(stream.fileno())


def device_headers(device, token=None):
    return {**access_headers(device), "X-Magician-Device-Id": device["device_id"],
            "Authorization": "Bearer " + (device["token"] if token is None else token),
            # Neither an enrolled phone nor an ESP may choose another scope.
            "X-Principal": "forged-mobile-probe", "X-Workspace": "forged-workspace"}


def access_headers(device):
    access = device.get("cloudflare_access")
    if access is None:
        return {}
    require(isinstance(access, dict) and all(isinstance(access.get(k), str) and access[k]
            for k in ("client_id", "client_secret")), "invalid Access credential in grant")
    return {"CF-Access-Client-Id": access["client_id"],
            "CF-Access-Client-Secret": access["client_secret"]}


def public_origin(value):
    result = harness.origin(value)
    require(urllib.parse.urlsplit(result).scheme == "https", "public origin must use HTTPS")
    return result


def verify_device(origin, device, *, revoked=False):
    status = 401 if revoked else 200
    me = request(origin, "/api/magician/v2/devices/me",
                 headers=device_headers(device), expected=status)
    if not revoked:
        for key in ("device_id", "principal", "workspace"):
            require(me.get(key) == device[key], f"{device['kind']}: server-owned {key} changed")
        request(origin, "/api/magician/v2/devices/me",
                headers=device_headers(device, "invalid-probe-token"), expected=401)


def enroll(origin, path, public=None, owner_headers=None):
    client_origin = public or origin
    with private_state(path, create=True) as stream:
        state = {"schema": SCHEMA, "origin": origin, "run_id": uuid.uuid4().hex,
                 "public_origin": public, "devices": [], "hardware_acceptance": False}
        save(state, stream)
        owner = request(origin, "/api/magician/v2/devices", headers=owner_headers)
        require(owner.get("pairing_available") is not False, "pairing authority is unavailable")
        for kind in ("ios", "android", "esp32"):
            device_id = f"mobile-probe-{state['run_id']}-{kind}"
            payload = {"device_id": device_id, "label": f"Protocol probe ({kind})"}
            access = None
            if kind == "esp32":
                if public:
                    # Reuse the ordinary enrollment's issued outer credential, just
                    # as the provisioned firmware does; never grant loopback trust
                    # to a request arriving through the public edge.
                    access = state["devices"][0].get("cloudflare_access")
                    require(access is not None, "public ESP bootstrap requires issued Access credentials")
                grant = request(client_origin, "/api/magician/v2/devices/pair", method="POST",
                                body=payload, headers=access_headers({"cloudflare_access": access}))
            else:
                ticket = request(origin, "/api/magician/v2/devices/enrollment", method="POST",
                                 body={"client_kind": kind}, headers=owner_headers)
                uri = urllib.parse.urlsplit(ticket["enrollment_uri"])
                params = urllib.parse.parse_qs(uri.query, strict_parsing=True)
                require(uri.scheme == "magican" and uri.netloc == "connect"
                        and params["kind"] == [kind], "invalid enrollment link")
                if public:
                    require(params.get("base") == [public], "enrollment link targets another public origin")
                payload.update(enrollment_id=ticket["enrollment_id"], secret=params["secret"][0])
                grant = request(client_origin, "/api/magician/v2/devices/enrollment/exchange",
                                method="POST", body=payload)
            # Persist the grant before assertions, so a failed probe can be revoked.
            device = {key: grant[key] for key in ("device_id", "principal", "workspace", "token")}
            device["kind"] = kind
            if public:
                device["cloudflare_access"] = grant.get("cloudflare_access") or access
            state["devices"].append(device)
            save(state, stream)
            require(grant["principal"] == owner["principal"] and grant["workspace"] == owner["workspace"],
                    "grant scope differs from the owner")
            if kind != "esp32":
                require(grant["client_kind"] == kind and grant["capabilities"] == ["mobile_client"],
                        "ordinary enrollment granted unexpected capabilities")
                request(client_origin, "/api/magician/v2/devices/enrollment/exchange", method="POST",
                        body=payload, expected=410)
            if public:
                request(client_origin, "/api/magician/v2/devices/me",
                        headers=access_headers(device), expected=401)
            verify_device(client_origin, device)
            print(f"PASS {kind}: enrollment, server-owned scope, credential rejection", flush=True)


def read_state(path, origin, public=None):
    with private_state(path) as stream:
        state = json.load(stream)
    require(state.get("schema") == SCHEMA and state.get("origin") == origin
            and state.get("public_origin") == public,
            "state belongs to another probe or origin")
    require(isinstance(state.get("run_id"), str) and len(state["run_id"]) == 32,
            "invalid probe identity")
    for d in state["devices"]:
        require(d["kind"] in ("ios", "android", "esp32")
                and d["device_id"] == f"mobile-probe-{state['run_id']}-{d['kind']}",
                "refusing a device outside this probe")
    return state


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("enroll", "verify", "revoke", "verify-revoked"))
    parser.add_argument("--origin", required=True, help="loopback origin of an isolated test backend")
    parser.add_argument("--public-origin", help="exact HTTPS origin for client-side requests; owner calls stay on loopback")
    parser.add_argument("--state-file", required=True, type=Path, help="private persistent credential file")
    parser.add_argument("--owner-token-env", help="environment variable holding the owner bearer; used only for loopback owner calls")
    args = parser.parse_args()
    try:
        origin = harness.origin(args.origin)
        host = urllib.parse.urlsplit(origin).hostname
        require(host == "localhost" or ipaddress.ip_address(host).is_loopback,
                "run this owner-side probe on backend loopback; do not publish it")
        public = public_origin(args.public_origin) if args.public_origin else None
        client_origin = public or origin
        owner_headers = None
        if args.owner_token_env:
            token = os.environ.get(args.owner_token_env, "").strip()
            require(bool(token) and len(token) <= 8192 and "\n" not in token and "\r" not in token,
                    "owner bearer environment variable is empty or invalid")
            owner_headers = {"Authorization": "Bearer " + token}
        if args.phase == "enroll":
            enroll(origin, args.state_file, public, owner_headers=owner_headers)
        else:
            state = read_state(args.state_file, origin, public)
            require(bool(state["devices"]), "no enrolled probe devices")
            if args.phase != "revoke":
                require({d["kind"] for d in state["devices"]} == {"ios", "android", "esp32"},
                        "incomplete enrollment; revoke the partial probe and start a new one")
            for d in state["devices"]:
                if args.phase == "revoke":
                    request(origin, "/api/magician/v2/devices/" + d["device_id"], method="DELETE", headers=owner_headers)
                verify_device(client_origin, d, revoked=args.phase in ("revoke", "verify-revoked"))
                print(f"PASS {d['kind']}: {args.phase}", flush=True)
        print("Protocol checks passed" + (" through HTTPS ingress." if public else " on loopback.")
              + " Physical devices, Android automation, push, chat and media remain separate acceptance.")
        return 0
    except Exception as error:
        # URL contents, request/response payloads and credentials are never diagnostics.
        print("FAIL: " + (str(error) if isinstance(error, harness.ProbeError) else type(error).__name__), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
