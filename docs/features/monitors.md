# Monitors

A **Monitor** is an explicit, user-owned recurring watch over pages, sites,
searches, or signed-in sources — "watch the Acme pricing page weekly and
tell me only when something material changes." Underneath, a monitor is an
ordinary persistent scheduled task with a typed monitor contract attached:
there is no second scheduler, queue, store, or runtime, and every client
manages the same canonical task.

Component docs (design + implementation detail):
[backend](../components/magician/recurring-monitors.md) ·
[web/Tauri](../components/unified-ui/monitors.md) ·
[iOS](../components/magios/monitors.md) · plan:
Recurring Monitors productization

> The product noun is **Monitor** everywhere. It is deliberately NOT called
> Scout — `vc-researcher` is already a specialized VC Worker named Scout,
> and the monitors feature neither uses nor changes that agent.

## Creating a monitor

Every creation path ends in the same **review-before-activate** step: you
see the interpreted contract (objective, sources, include/exclude rules,
match strictness, notification policy) and the EXACT schedule before
anything recurring starts.

| Path | How |
| --- | --- |
| Chat | Ask the assistant to watch something ("watch this page daily…"). It calls `preview_monitor` and shows a review card; your confirmation calls `create_monitor`. The tools refuse to create anything that wasn't previewed, so review is enforced in data, not convention. |
| Web / Tauri | `/tasks?type=monitors` → **New monitor** — one fixed-size composer (objective, URLs, cadence, notify policy; sources/rules/strictness behind "More options"). |
| iOS (Magios) | Tasks → **Monitors** lane → **+** — the same composer as a native sheet. |
| Convert an existing task | On an eligible task row (persistent, not already a monitor): web Tasks tab → row `⋯` menu → **Convert to monitor**; iOS → task card → Actions → **Convert to monitor**. The composer opens prefilled (objective from the task description, title kept), you add sources and review, and `POST /monitors/{id}/convert` attaches the contract. The task keeps its id, schedule, run history, executions, and outputs — a task without a schedule simply becomes a run-on-demand monitor. Conversion is always user-explicit; monitors are never inferred from task titles or prose. |

The first run after activation is a **baseline**: it records what exists
today and never notifies (unless you explicitly enable "Notify me about the
initial baseline").

## Managing monitors

Manage from `/tasks?type=monitors` (web) or the Monitors lane (iOS); chat
edits go through the `update_monitor` tool. All clients drive the same
`/api/magician/v3/monitors` routes:

- **Pause / Resume** — flips the schedule's paused flag; history is kept.
  (A monitor without a schedule has nothing to pause — run it on demand.)
- **Run now** — starts a run immediately through the ordinary task
  execution path.
- **Edit** — title, contract fields, or schedule; contract edits bump the
  server-owned monitor revision.
- **Delete** — archives the underlying task (soft by default; it disappears
  from every monitor surface, but the record is restorable and its
  notification-dedupe history survives).
- **Detail** — Latest / Updates / Runs / Settings: every accepted run
  (including quiet and degraded ones) with per-source outcomes, the durable
  update history with the notified/suppressed decision, and the typed
  contract. "Open task view" jumps to the shared task detail for
  execution-level inspection.

## Source access and auth recovery

A source that needs login, hits a CAPTCHA, times out, or rate-limits is a
**source failure, never a change**: an unreadable source can never make
previously seen items look deleted (removal needs two consecutive complete
scans). One transient blip stays quiet; the **second consecutive failure**
of the same source raises a "needs access" item in Attention / Today
**Needs you** that deep-links to the problem. When a later run reads the
source cleanly again, the item resolves itself. Dismissing an ongoing
problem sticks; a NEW failure streak resurfaces it.

## Today placement and the quiet-runs rule

- Material changes appear as cards in Today's **Changed** section, each
  opening the exact update on the monitor detail.
- **Unchanged runs never surface** — no Today card, no Attention item, no
  notification. They remain visible in the monitor's Runs history.
- Dismissing a Changed card suppresses exactly that change; the next
  DIFFERENT change surfaces again.
- Monitor results never enter Followups by importance alone, and access
  problems go to **Needs you**, not Changed.

## Notifications and feedback

- Notification policy per monitor: **material changes only** (default),
  **every run** (quiet receipts stay in the Updates history — they still
  never hit Today), or **never** (changes are still recorded in Updates).
- Duplicate protection is durable: the same change never notifies twice,
  across retries and restarts.
- Every update card offers **Useful / Not relevant** feedback. Feedback is
  evidence for later tuning — it never silently rewrites the monitor's
  contract (any rule change goes back through the previewed edit path).

## Limits and known behavior

- Contract bounds: objective ≤ 2000 chars; ≤ 100 source URLs (http/https
  only); ≤ 50 entries per list (domains, search phrases, rules,
  signed-in sources); at least one URL, domain, or search phrase required.
- Update and feedback histories are bounded (newest 500 records per
  monitor); run artifacts follow the normal execution retention.
- Monitor lists, run history, and update history are server-paginated.
- No high-frequency polling, no anti-bot evasion, and no unattended
  credential entry — signed-in sources use existing authenticated access
  and degrade to a "needs access" escalation when it expires.
- iOS has no OS push yet; monitor updates land in Today/Attention and the
  in-app surfaces (the deep-link landing path for push already exists).
