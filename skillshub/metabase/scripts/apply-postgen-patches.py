#!/usr/bin/env python3
"""
Apply hand-maintained patches to the auto-generated `.pp-build/` tree.

Why:
  `make regen-metabase-cli` wipes `.pp-build/` and re-generates from
  `spec.json` via Printing Press. Any local hand-patches we apply to
  the generated Go code would be lost. This script reapplies them
  after every regen.

The Makefile target invokes this between `pp generate` and `go build`.

What it patches (2026-05-23):
  1. `internal/cli/helpers.go` — add `compactQueryResult` helper that
     strips column metadata (fingerprints, field_ref, lib/* aliases),
     drops `insights` / `results_metadata` / `pivot-export-options` /
     `json_query` / execution bookkeeping, and keeps SQL + columns
     (name + type + semantic_type + description) + rows + row_count.
  2. `internal/cli/card_query_card-run.go` — call `compactQueryResult`
     instead of `compactFields` when `--compact` is on.
  3. `internal/cli/dataset_query.go` — same.

The reason: Metabase's REST envelope for query responses is ~24 KB of
column metadata + UI bookkeeping per card BEFORE any rows. With the
agent's 24 KB tool-result cap, rows get chopped entirely. The slim
takes a 12-col card response from ~24 KB → ~5 KB (and the actual rows
finally fit in the budget).

Each patch is idempotent: it checks for an "ALREADY PATCHED" marker
before applying, so re-running is safe.
"""
from __future__ import annotations
import sys
from pathlib import Path

PP_BUILD = Path(__file__).resolve().parent.parent / ".pp-build" / "internal" / "cli"

MARKER = "2026-05-23 query-aware slim applied"
COMPACT_QUERY_RESULT_FUNC = """
// """ + MARKER + """
//
// compactQueryResult slims a Metabase card/dataset query response for agent
// consumption. The raw response is a nested envelope (`data.data.{cols,
// rows, insights, native_form, results_metadata, ...}`) where the bulk of
// the bytes are UI rendering bookkeeping (column fingerprints, sparkline
// insights, `field_ref` MBQL clauses, `lib/*` alias plumbing,
// `results_metadata` duplicate of `cols`, pivot-export options). The
// useful payload for an LLM agent is just the SQL Metabase ran, the
// column schema (name + type + semantic_type + description), and the
// rows themselves.
//
// On a typical 12-column response this drops ~20-22 KB of envelope and
// leaves room for the actual rows under a 24 KB per-tool-result cap.
//
// If the response doesn't have the query-result shape (e.g. caller passed
// us a list endpoint payload), we fall back to `compactFields` so the
// helper is safe to use universally.
func compactQueryResult(data json.RawMessage) json.RawMessage {
	var outer map[string]any
	if err := json.Unmarshal(data, &outer); err != nil {
		return data
	}
	inner, ok := outer["data"].(map[string]any)
	if !ok {
		return compactFields(data)
	}
	colsAny, hasCols := inner["cols"]
	_, hasRows := inner["rows"]
	if !hasCols && !hasRows {
		return compactFields(data)
	}

	colsArr, _ := colsAny.([]any)
	slimCols := make([]map[string]any, 0, len(colsArr))
	for _, c := range colsArr {
		col, ok := c.(map[string]any)
		if !ok {
			continue
		}
		out := map[string]any{}
		if v, ok := col["name"]; ok && v != nil {
			out["name"] = v
		}
		if v, ok := col["display_name"]; ok && v != nil && v != out["name"] {
			out["display_name"] = v
		}
		switch {
		case col["effective_type"] != nil:
			out["type"] = col["effective_type"]
		case col["base_type"] != nil:
			out["type"] = col["base_type"]
		case col["database_type"] != nil:
			out["type"] = col["database_type"]
		}
		if v, ok := col["semantic_type"]; ok && v != nil {
			out["semantic_type"] = v
		}
		if v, ok := col["description"]; ok && v != nil && v != "" {
			out["description"] = v
		}
		slimCols = append(slimCols, out)
	}

	slimInner := map[string]any{
		"columns": slimCols,
	}
	if rows, ok := inner["rows"]; ok {
		slimInner["rows"] = rows
	}
	if rc, ok := inner["row_count"]; ok && rc != nil {
		slimInner["row_count"] = rc
	}
	if nf, ok := inner["native_form"].(map[string]any); ok {
		if q, ok := nf["query"]; ok && q != nil {
			slimInner["sql"] = q
		}
	}
	if tz, ok := inner["requested_timezone"]; ok && tz != nil {
		slimInner["requested_timezone"] = tz
	}
	if tz, ok := inner["results_timezone"]; ok && tz != nil && tz != slimInner["requested_timezone"] {
		slimInner["results_timezone"] = tz
	}

	slim := map[string]any{
		"data": slimInner,
	}
	if dbID, ok := outer["database_id"]; ok && dbID != nil {
		slim["database_id"] = dbID
	}
	if st, ok := outer["status"]; ok && st != nil {
		slim["status"] = st
	}
	result, err := json.Marshal(slim)
	if err != nil {
		return data
	}
	return result
}
"""


CALL_SITE_OLD = "\t\t\t\t} else if flags.compact {\n\t\t\t\t\tfiltered = compactFields(filtered)\n\t\t\t\t}"
CALL_SITE_NEW = "\t\t\t\t} else if flags.compact {\n\t\t\t\t\t// " + MARKER + "\n\t\t\t\t\tfiltered = compactQueryResult(filtered)\n\t\t\t\t}"


def patch_call_site(p: Path) -> tuple[bool, str]:
    """Replace the compactFields call with compactQueryResult.

    Returns (changed, status) where status is one of:
      "patched"          — applied this run
      "already-patched"  — `compactQueryResult` reference already present
      "no-call-site"     — neither the unpatched pattern nor the patched
                           reference found; Printing Press output may
                           have changed shape (warn).
    """
    text = p.read_text()
    if "compactQueryResult(filtered)" in text:
        return False, "already-patched"
    if CALL_SITE_OLD not in text:
        return False, "no-call-site"
    new_text = text.replace(CALL_SITE_OLD, CALL_SITE_NEW, 1)
    p.write_text(new_text)
    return True, "patched"


def patch_helpers_with_status(p: Path) -> tuple[bool, str]:
    text = p.read_text()
    if MARKER in text or "func compactQueryResult(" in text:
        return False, "already-patched"
    new_text = text.rstrip() + "\n" + COMPACT_QUERY_RESULT_FUNC + "\n"
    p.write_text(new_text)
    return True, "patched"


def main() -> int:
    targets = [
        (PP_BUILD / "helpers.go", patch_helpers_with_status),
        (PP_BUILD / "card_query_card-run.go", patch_call_site),
        (PP_BUILD / "dataset_query.go", patch_call_site),
    ]
    if not PP_BUILD.is_dir():
        sys.stderr.write(
            f"error: {PP_BUILD} doesn't exist — run `make regen-metabase-cli` first\n"
        )
        return 1
    changed_any = False
    for path, fn in targets:
        if not path.is_file():
            sys.stderr.write(f"warning: target file missing: {path}\n")
            continue
        changed, status = fn(path)
        rel = path.relative_to(PP_BUILD.parent.parent.parent)
        if status == "patched":
            print(f"  patched {rel}")
            changed_any = True
        elif status == "already-patched":
            print(f"  skipped {path.name} (already patched)")
        elif status == "no-call-site":
            sys.stderr.write(
                f"warning: expected call-site pattern not found in {path.name} — "
                "Printing Press output may have changed.\n"
            )
    if not changed_any:
        print("  nothing to do — all patches already applied")
    return 0


if __name__ == "__main__":
    sys.exit(main())
