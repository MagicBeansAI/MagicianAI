#!/usr/bin/env python3
"""Verify recurring App runs and retire explicitly selected legacy ambient tasks.

Uses normal owner APIs; never prints credentials or message bodies. Cleanup is
guarded by two completed recurring occurrences and committed post evidence.
"""
import argparse
import getpass
import json
import os
from pathlib import Path
import re
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["login", "apps", "health", "details", "dispatch", "queue", "inventory", "posts", "retry-blocked", "cleanup"])
    parser.add_argument("--session", type=Path)
    parser.add_argument("--task")
    parser.add_argument("--cursor")
    parser.add_argument("--limit", type=int, default=25)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--verified-recurring-task")
    parser.add_argument("--runtime-root", type=Path, default=Path.home() / "MagicianNotes")
    args = parser.parse_args()
    base = "http://127.0.0.1:3002/api/magician"
    token = None

    def request(path, method="GET", body=None, api_root=base):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        req = urllib.request.Request(api_root + path, method=method, headers=headers,
            data=None if body is None else json.dumps(body).encode())
        try:
            with urllib.request.urlopen(req, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            try:
                reason = json.load(error).get("error", "request_failed")
            except (ValueError, AttributeError):
                reason = "request_failed"
            raise RuntimeError(f"HTTP {error.code}: {reason}") from None

    if args.command == "login":
        response = request("/v2/auth/login", "POST", {
            "username": input("Username: ").strip(),
            "password": getpass.getpass("Password: "), "workspace": "default"})
        if not response.get("token"):
            raise RuntimeError("Login did not return a session token")
        if args.out:
            fd = os.open(args.out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            path = str(args.out)
        else:
            fd, path = tempfile.mkstemp(prefix="magician-apps-session-", suffix=".json")
        with os.fdopen(fd, "w") as output:
            json.dump(response, output)
        print(f"Private session file: {path}")
        return

    if not args.session:
        parser.error("--session is required (create one with the login command)")
    session = json.loads(args.session.read_text())
    token = session.get("token")
    if not isinstance(token, str) or not token:
        raise RuntimeError("The session file has no token")
    if session.get("principal") != "anonymous" or session.get("workspace") != "default":
        raise RuntimeError("This maintenance operation requires an anonymous/default session")
    request("/v2/auth/session")
    scope_root = args.runtime_root / "scopes/anonymous/default"

    def health():
        return request("/v2/apps/background-behaviors")

    def ambient(snapshot):
        rows = [item for item in snapshot["items"] if item["behavior_id"] == "ambient_turn"]
        if len(rows) != 1:
            raise RuntimeError(f"Expected one ambient behavior, found {len(rows)}")
        return rows[0]

    def inventory():
        rows = []
        for category in ["internal_tasks", "tasks"]:
            for path in (scope_root / category).glob("task_app_*/state/app_workflow_binding.json"):
                sealed = json.loads(path.read_text())
                binding = sealed.get("payload", sealed)
                if isinstance(binding, str):
                    binding = json.loads(binding)
                behavior = (binding.get("background_behavior_binding") or {}).get("grant", {})
                if behavior.get("behavior_id") != "ambient_turn":
                    continue
                task_id = path.parents[1].name
                if not re.fullmatch(r"task_app_[0-9a-f]{64}", task_id):
                    raise RuntimeError("Unexpected ambient task identity")
                state = json.loads((path.parent / "task_state.json").read_text())
                rows.append({"task_id": task_id, "status": state["status"],
                    "active_execution": state.get("active_root_execution_id"),
                    "recurring": binding.get("recurring"), "category": category})
        return rows

    def all_task_ids():
        return {f"{category}/{path.name}" for category in ["internal_tasks", "tasks"]
                for path in (scope_root / category).glob("task_*") if path.is_dir()}

    def post_ids(installation):
        # Indexed pages avoid retaining the full corpus or exposing messages.
        body = {"protocol_version": "1", "source_installation_id": installation,
                "entity": "post", "select": ["created_at"], "pagination": "keyset",
                "order": [{"field": "created_at", "direction": "descending"}],
                "limit": 200, "purpose": "owner_maintenance_verification"}
        found, cursors = set(), set()
        while True:
            page = request(f"/v2/apps/installations/{installation}/data/query", "POST", body)
            found.update(row["record_id"] for row in page["envelope"]["value"])
            cursor = page.get("next_cursor")
            if cursor is None:
                return found
            if cursor in cursors:
                raise RuntimeError("Post query repeated a cursor")
            cursors.add(cursor)
            body["cursor"] = cursor

    if args.command == "apps":
        result = request("/v2/apps/directory?section=installed&limit=48")
    elif args.command == "health":
        result = health()
    elif args.command == "inventory":
        result = {"tasks": inventory()}
    elif args.command == "posts":
        result = {"post_ids": sorted(post_ids(ambient(health())["installation_id"]))}
    elif args.command == "retry-blocked":
        item = ambient(health())
        latest = (item.get("recurring") or {}).get("latest") or {}
        settled_failure = (item.get("last_error") == "workflow_execution_failed"
            and latest.get("status") == "failed" and latest.get("terminal") is True
            and latest.get("settled") is True)
        if item["state"] != "blocked" or not (
                item.get("last_error") == "workflow_launch_blocked" or settled_failure):
            raise RuntimeError("Only a blocked ambient launch or settled failed occurrence can be retried here")
        installation = urllib.parse.quote(item["installation_id"], safe="")
        result = request(f"/v2/apps/background-behaviors/{installation}/ambient_turn/retry", "POST", {
            "expected_installation_generation": item["installation_generation"],
            "expected_revision": item["revision"]})
    elif args.command == "details":
        if not args.task:
            parser.error("--task is required")
        query = {"workspace": "default", "limit": args.limit}
        if args.cursor:
            query["cursor"] = args.cursor
        result = request(f"/v3/tasks/{urllib.parse.quote(args.task, safe='')}/details?" + urllib.parse.urlencode(query))
    elif args.command == "queue":
        if not args.task or not re.fullmatch(r"task_app_[0-9a-f]{64}", args.task):
            parser.error("queue requires a canonical App --task")
        snapshot = request("/api/llm/queue/snapshot", api_root="http://127.0.0.1:3002")
        jobs = []
        for lane, entries in snapshot["registry"].items():
            if not isinstance(entries, list):
                continue
            for job in entries:
                task = job.get("task_ref") or {}
                if task.get("task_id") != args.task:
                    continue
                if task.get("scope") != {"principal": "anonymous", "workspace": "default"}:
                    raise RuntimeError("Queue task scope does not match the maintenance scope")
                # Retain correlation and outcome evidence, never prompts or outputs.
                jobs.append({"lane": lane, **{key: job[key] for key in [
                    "job_id", "task_ref", "state", "provider", "model", "origin",
                    "submitted_at_ms", "dispatched_at_ms", "completed_at_ms", "attempts", "profile"
                ] if key in job}})
        result = {"observed_at": time.time(), "task_id": args.task,
            "workers_total": snapshot["workers_total"], "jobs": jobs}
    elif args.command == "dispatch":
        if not args.task or not re.fullmatch(r"task_app_[0-9a-f]{64}", args.task):
            parser.error("dispatch requires a canonical App --task")
        # The governed facts endpoint applies the authenticated scope. Keep
        # prompts and responses out of evidence; correlate roots with queue jobs.
        sql = ("SELECT root_execution_id, provider, model, count(*) AS call_count, "
            "count(dispatch_job_id) AS dispatcher_calls, "
            "count(DISTINCT dispatch_job_id) AS dispatcher_jobs FROM llm_calls "
            f"WHERE task_id = '{args.task}' GROUP BY root_execution_id, provider, model")
        result = request("/v2/analytics/llm/facts/query", "POST", {
            "sql": sql, "from_ms": int((time.time() - 86400) * 1000), "limit": 500})
    else:
        expected = args.verified_recurring_task
        if not expected or not re.fullmatch(r"task_app_[0-9a-f]{64}", expected) or not args.out:
            parser.error("cleanup requires --verified-recurring-task and --out for durable receipts")
        item = ambient(health())
        recurring = item.get("recurring") or {}
        if recurring.get("latest", {}).get("task_id") != expected or recurring.get("completed_count", 0) < 2:
            raise RuntimeError("Two completed occurrences under the expected recurring task must be verified first")
        if recurring.get("published_records", {}).get("post", 0) < 1:
            raise RuntimeError("No committed posts have been observed for the recurring task")
        rows = inventory()
        old = [row for row in rows if row["recurring"] is None]
        if any(row["category"] != "internal_tasks" or row["active_execution"] or row["status"] not in ["completed", "failed", "cancelled", "canceled"] for row in old):
            raise RuntimeError("Some legacy ambient tasks are not settled internal tasks; cleanup has not started")
        old_ids = {"internal_tasks/" + row["task_id"] for row in old}
        unrelated_before = all_task_ids() - old_ids
        posts_before = post_ids(item["installation_id"])
        journal_path = args.out.with_suffix(".receipts.jsonl")
        fd = os.open(journal_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as journal:
            def record(value):
                journal.write(json.dumps(value) + "\n")
                journal.flush()
                os.fsync(journal.fileno())
            record({"started_at": time.time(), "planned_tasks": sorted(old_ids),
                "unrelated_tasks_before": sorted(unrelated_before), "post_ids_before": sorted(posts_before)})
            deleted = []
            for row in old:
                run = "run:app-action:" + row["task_id"]
                receipt = request("/v2/apps/maintenance/action-runs/" + urllib.parse.quote(run, safe=""), "DELETE")
                if receipt.get("deleted_task_id") != row["task_id"]:
                    raise RuntimeError("Cleanup receipt does not match the requested task")
                deleted.append(row["task_id"])
                record(receipt)
                print(f"Deleted {len(deleted)} of {len(old)} old ambient tasks", flush=True)
            remaining = inventory()
            if any(row["recurring"] is None for row in remaining) or not any(row["task_id"] == expected for row in remaining):
                raise RuntimeError("Post-cleanup inventory does not match the authorized scope")
            posts_after = post_ids(item["installation_id"])
            missing_tasks = unrelated_before - all_task_ids()
            missing_posts = posts_before - posts_after
            result = {"deleted_count": len(deleted), "deleted_tasks": deleted, "retained_ambient_tasks": remaining,
                "unrelated_tasks_before": len(unrelated_before), "missing_unrelated_tasks": sorted(missing_tasks),
                "posts_before": len(posts_before), "posts_after": len(posts_after), "missing_posts": sorted(missing_posts)}
            record(result)
            if missing_tasks or missing_posts:
                args.out.write_text(json.dumps(result, indent=2))
                raise RuntimeError("Cleanup preservation verification failed; inspect recorded evidence")

    if args.out:
        args.out.write_text(json.dumps(result, indent=2))
        print(f"Evidence saved: {args.out}")
    else:
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
