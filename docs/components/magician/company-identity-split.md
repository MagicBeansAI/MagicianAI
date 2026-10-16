# Owner and company identity are separate agents

The company harness must never reach the owner's personal accounts.
`executive-assistant` is **the owner's** transactional agent (inbox, calendar,
contacts, WhatsApp, iMessage, Telegram); company officers run autonomously and
must not hold a delegation edge to it, direct or transitive. This is a
precondition for unpausing the company loop.

## The split

The officers (`ceo`, `cmo`, `cpo`, `cro`) delegate to `company-assistant` (Aide), a company-scoped worker:

| Property | Value | Why |
|---|---|---|
| `kind` | `worker` | Not `personal` — see the memory note below |
| `user_memory_isolation` | `fully_isolated` | The field that actually stops it reading the owner |
| `delegation_targets` | `[web-researcher-opc]` | One hop to an isolated leaf (owner decision 2026-10-01). Re-delegating any further would be a route back to the owner's agents |
| `memory_tiers` | `[]` | Genuinely none, because workers receive no injected defaults |
| `denied_tools` | `gmail`, `calendar`, `sheets`, `imessage`, `imessage_send`, `whatsapp`, `telegram`, `zepto-mcp`, `swiggy-mcp`, `search_memory`, `treasurer`, `internal_data` | Denied, not merely ungranted |

## The identity half

Swapping the delegation target is not enough if the company assistant holds the
owner's Google. The same job runs on company identities:

| | owner's assistant | company's assistant |
|---|---|---|
| mail | `gmail` | `presto-gmail` (reach.magican@gmail.com), `agentmail-read` / `agentmail-send` (magican@agentmail.to) |
| calendar | `calendar` | `presto-calendar` |
| sheets | `sheets` | `presto-sheets` |
| messaging | `whatsapp`, `telegram`, `imessage` | `kapso-whatsapp-read` / `kapso-whatsapp-send` — Presto's own number |
| documents | `office-word` / `-excel` / `-powerpoint` | the same — these touch no identity |
| commerce | `zepto-mcp`, `swiggy-mcp` | **none**, and both are denied |

Each `presto-*` skill is a hard-pinned fork of the shared gws skill with the
`account` parameter **removed**, so the company assistant cannot select the
owner's mailbox even by mistake. This is a structural rail, not prompt guidance.

### Approval rules name real action ids

`approval::rule_matches` compares the action token exactly, so a rule naming an
action the skill does not expose gates nothing. The company assistant's rules use
the ids the skills declare in `SKILL.md` frontmatter (for `calendar`:
`events_insert`, `events_patch`, `events_delete`) and also cover `raw`, the
unbounded argv passthrough whose token says nothing (readiness review §9A).

## No wildcard delegation on non-system agents

`delegation_targets: ['*']` expands to every enabled non-system agent,
`executive-assistant` included, so any agent with a wildcard reachable from an
officer reopens the route. A test for the literal string `executive-assistant`
cannot see a wildcard, so the wildcard ban is asserted directly.

- **`creative-mind`** (Muse; reachable from `cpo` via `cto` and from `ceo` via
  `vc-researcher`) names one target, `web-researcher` (owner decision
  2026-10-01), which is itself a leaf (`delegation_targets: []`), so no onward
  route opens.
- **`personal-assistant`** (Presto) is the owner's own router, so its edge to
  `executive-assistant` is legitimate. A wildcard would also give it the whole
  company roster (a two-hop path via `ceo` to `ambassador`, the agent that mails
  strangers) and inject every agent into its prompt (a 23-target injection fed an
  agentic-loop stack overflow). Its list is **ten** ids in three evidence bands
  the definition keeps apart:

| band | targets | what establishes the edge |
|---|---|---|
| proven by a shipped artifact | `mac-operator`, `web-researcher`, `creative-mind`, `executive-assistant` | `narrow_feature_delegation_targets` retains `mac-operator` for App Copilot and `prepare_tutor_mac_operator_delegation` keys the tutor envelope on that id; the `chat.delegate.status_changed` fixture pins `personal-assistant -> web-researcher`; `test_spine_reconcile_resumes_parent_with_comic_child` parks a PA-owned root on a `creative-mind` child; Presto holds only the `presto-*` identities, so the owner's mail, calendar and messaging are reachable only through Vera |
| declared by the definition and nothing else | `android-operator`, `simple-data-analyst` | the tools block says the Android verbs are Pilot's, reached by delegation, and Presto holds no android tool; the persona names "data analysis, SQL results" among delegated results, and no other agent owns that |
| confirmed by the owner | `writing-assistant`, `internal-system-analyst`, `harness-sre`, `wealth-manager` | the four the repo could not prove; the owner states all four are in typical use. `wealth-manager` holds `gmail` and `imessage`: the owner's router reaching the owner's finance tool, not the §5A.1 shape. `harness-sre` holds `delegate_to_agent` (today `internal-system-analyst` and `cto`); if that list widens, this edge widens with it |

Never restore `'*'`, and never add the company harness.

The list is enforced. `resolve_effective_delegation_target_ids_for_surface`
(`agents/types.rs`) is the shared resolver for chat and autonomous execution;
delegation admission in `agents/runtime.rs` refuses any target outside it; and
`find_agents_for_capability` filters discovery through the same set, so a
missing id is a capability Presto reports as unavailable, not one it reaches
under another name.

`executive-assistant` is unchanged and stays the owner's; it is simply not
reachable from the company harness.

## Two details that look like style and are not

**`kind: worker` matters for the memory line.** On a *personal* agent, an empty
`memory_tiers` list **triggers** the standard six-tier default injection (§4.5a
of the outward agent boundary plan). Workers get no defaults, so an empty list
means none. A test fails if workers ever start receiving defaults.

**Tools are denied, not merely absent.** An ungranted tool becomes reachable the
day someone widens a default; a denied one does not. The test asserts each is
absent from `tools` *and* present in `denied_tools`.

## What is asserted

`no_company_officer_may_delegate_to_the_owners_assistant` walks **every shipped
definition**, so a new officer that copies an old delegation target fails.
`the_company_assistant_is_sealed_from_the_owner` pins the isolation, the
single `web-researcher-opc` target (and that it stays a fully isolated leaf),
the empty tier list and each denied tool.

## Outward capabilities are still gated

`company-assistant` holds `presto-gmail`, `presto-calendar`, `agentmail-send`
and `kapso-whatsapp-send`, outward capabilities governed by the capture gate
(`docs/components/magician/outward-actions.md`) and, separately, by its own
`requires_approval` rules. Both apply. Aide and her research leaf
`web-researcher-opc` also hold `find-details-by-username` for public-profile
lookup.

`presto-calendar` is in `outward_actions::OUTWARD_CAPABILITIES`
(`CalendarInvite`) and `events_insert` / `events_patch` / `events_delete` are in
`SENDING_ACTIONS`, so the static `(capability, action)` classifier reads a
calendar write as outward. `outward_dispatch_class` also classifies any dispatch
on an outward capability that carries an unbounded argument passthrough (such
as `raw`), whatever its action token.
