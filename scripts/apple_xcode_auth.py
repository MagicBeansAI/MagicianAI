#!/usr/bin/env python3
"""Use an installed App Store Connect key only for its configured Xcode team."""

import json
from pathlib import Path
import re
import sys

REPOSITORY = Path(__file__).resolve().parents[1]
METADATA = Path.home() / "Library/Application Support/Magican/release-signing/apple/app-store-connect/metadata.json"


def authentication_arguments(arguments, *, repository=REPOSITORY, metadata_path=METADATA):
    if any(argument.startswith("-authenticationKey") for argument in arguments):
        return []
    team = next((argument.split("=", 1)[1] for argument in reversed(arguments)
                 if argument.startswith("DEVELOPMENT_TEAM=")), None)
    try:
        if team is None:
            config = (repository / "magios/Signing.local.xcconfig").read_text()
            match = re.search(r"^\s*DEVELOPMENT_TEAM\s*=\s*([A-Za-z0-9]+)\s*(?://.*)?$", config, re.M)
            team = match.group(1) if match else None
        metadata = json.loads(metadata_path.read_text())
        if not isinstance(metadata, dict) or not team or metadata.get("team_id") != team:
            return []
        key_id, issuer = metadata["key_id"], metadata["issuer_id"]
        if not isinstance(key_id, str) or not re.fullmatch(r"[A-Za-z0-9]{1,64}", key_id):
            return []
        if not isinstance(issuer, str) or not re.fullmatch(r"[A-Za-z0-9-]{1,64}", issuer):
            return []
        key = Path(metadata.get("private_key_path") or metadata_path.parent / f"AuthKey_{key_id}.p8").expanduser().resolve()
        if key.is_relative_to(repository.resolve()) or not key.is_file():
            return []
        return ["-authenticationKeyPath", str(key), "-authenticationKeyID", key_id,
                "-authenticationKeyIssuerID", issuer]
    except (OSError, ValueError, TypeError, KeyError):
        # A machine without imported signing assets keeps Xcode's normal account flow.
        return []


if __name__ == "__main__":
    for argument in authentication_arguments(sys.argv[1:]):
        print(argument)
