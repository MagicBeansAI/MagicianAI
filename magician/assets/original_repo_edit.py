#!/usr/bin/env python3
"""Submit exact file preimages and replacements to Magician for owner review.

Read JSON from stdin: {"rationale":"...","files":[{"path":"relative/path",
"before":"full current text (null for create)","after":"full new text (null for delete)"}]}.
The original files are only changed by Magician after approval.
"""
import json
import os
import sys
import urllib.error
import urllib.request

def main():
    payload = json.load(sys.stdin)
    endpoint = os.environ["MAGICIAN_CITIZEN_URL"].rstrip("/") + "/citizen/edit_original"
    token = os.environ["MAGICIAN_CITIZEN_TOKEN"]
    request = urllib.request.Request(endpoint, data=json.dumps(payload).encode(),
        headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
    # The channel is local and authenticated; do not inherit a shell's HTTP proxy.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=330) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        result = json.load(error)
    print(json.dumps(result))
    return 0 if result.get("ok") else 1

if __name__ == "__main__":
    sys.exit(main())
