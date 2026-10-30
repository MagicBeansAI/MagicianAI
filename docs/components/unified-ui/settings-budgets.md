# Settings: spending

Server seam ([magician-api: run budgets](../magician-api/README.md#run-budgets)):
`GET`/`PUT /api/magician/v2/budgets/defaults`, `GET`/`PUT
/api/magician/v2/budgets/envelope`, `POST /api/magician/v2/budgets/envelope/grant`
and `GET`/`PUT`/`DELETE /api/magician/v2/budgets/limits`. Owner only: a login
session or a verified paired owner phone; errors are `{error, message}` and the
panel shows `message`. Design:
model spend in resource authority.

Settings hosts `$lib/settings/BudgetsPanel.svelte` (requests in
`$lib/settings/budgets.ts`, through the `/budget` slash command's
`budgetRequest`) as **Spending** in the **Agents & safety** category. The card
id and anchor stay `#budgets` (also `#model-spend`, `#action-limits`), so old
deep links still land; search still matches "budget", "limit" and "spend". The
iOS and Android apps show it through their embedded web Settings ("Magican
settings" row), not a native copy.

## Model spend

The limits live in `resource_authority.model_spend`. A muted line says model
spend is posted to the resource-authority ledger every 15 minutes.

- **Per run limit.** One dollar field plus **Unlimited**; saving writes the
  same value to `native_max_cost_usd` and `harness_max_cost_usd`. When they
  differ, **Advanced: separate limits** opens with one field each.
  `native_env_override` notes that `MAGICIAN_AGENTIC_MAX_COST_USD` overrides
  the Magician run limit.
- **Daily spend limit.** On/off (`enabled`), dollars per window (`daily_usd`,
  or Unlimited) and **Warn me at** (`warn_at_pct`, 1–100). The label names the
  window (`UTC day`, or `day (rolls at 07:00 UTC)` for a
  `reset_offset_minutes` of 420). `total_usd`, `window` and `unpriced` stay
  config-only.
- **Today.** Spent, remaining, today's limit (with top-ups), reset time, a
  progress bar and an optional per-class breakdown. Absent when no envelope
  runs in the process (`installed: false`).
- **Add to today.** Amount and optional note (200 chars) →
  `/budgets/envelope/grant`; current window only.

Today's status rides one shared poll (`spendEnvelopePoll`: 15 s while the
panel is open, 60 s otherwise, jittered backoff); a save or grant applies its
own response and calls `pollNow()`. Unsaved daily-limit edits survive a
refresh.

## Action limits

Rows of `resource_authority.budgets` from `GET /budgets/limits`
(`{enabled, limits: [{scope, id, commodity, ceiling, period}], applies}`).
Each row shows a readable label (`EMAIL_SENDS` on tool `agentmail-send` →
"Emails (agentmail-send)", `WHATSAPP_SENDS` → "WhatsApp sends", `INR`/`USD` →
"Spend in ₹/$"; otherwise the commodity name), the ceiling with its unit
("50 emails", "₹2,000") and the period ("per day"). Ceiling and period edit
inline with **Save** (`PUT`, upsert, carryover kept); **Remove** sends
`DELETE ?scope=&id=&commodity=` (each part URL-encoded). **Add limit** takes
scope (tool, agent, principal), id, commodity (uppercased), ceiling and period.
`applies: on_restart` shows "Changes apply when Magician restarts." (the
dispatch gate reads its rows at boot); `enabled: false` notes that nothing is
enforced; a server without the route (bare 404) hides the block.

**Validation.** Dollar amounts are plain positive dollars (`100`, `$12.50`,
`1,000`; at most $1,000,000); the percentage is a whole number 1–100; an
action-limit ceiling is a positive plain number (`$`, `₹` and commas are
dropped) sent as a decimal string. Invalid input never reaches the server.

The card ends with **Open the spend ledger** → `/budget` (the resource-authority
Spend ledger page).

Tests: `src/lib/settings/budgets.test.ts`,
`src/lib/settings/BudgetsPanel.component.test.ts`.
