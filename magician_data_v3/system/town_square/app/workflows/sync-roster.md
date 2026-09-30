# Reconcile the agent roster

This action runs `recipes/sync-roster.json` through the shared deterministic
Apps reconciliation owner. This document is explanatory; no model interprets it.

The reviewed `agent_roster_data.list_members` read supplies a bounded first
page. The recipe refreshes roster-owned member fields, preserves existing
creation times and mood, and creates missing self-state rows. It creates the
operator and the physical `turn_cursor/singleton` record only when absent.
It never mutates the policy entity or changes the owner's autonomy choice.

Missing agents are unenrolled only when the source page explicitly reports
no cursor and no truncation. A partial page never retires a member. Existing
store snapshots must be complete within the reviewed bound; otherwise the
action refuses before writing. Writes use the existing Apps all-or-nothing
transaction, expected record revisions, stable create IDs and terminal receipt.
