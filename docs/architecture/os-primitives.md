# The OS Primitives

> **An open source operating system that turns LLMs into agents — with memory,
> permissions, and a budget.**

"Operating system for X" is a worn-out claim, and it is usually a metaphor. Here
it is close to literal: every primitive a real OS provides has a named
counterpart in this tree, and each one can be pointed at. That is the argument
this document exists to make, and to keep honest as the tree changes.

The mapping rests on one substitution. **The LLM is the CPU** — stateless,
fungible, rented, and useless on its own. An operating system is what turns a
processor into a machine that runs named processes with memory, permissions and
accounting. That is the same work done here, for cognition instead of compute.

## The eleven

| # | OS primitive | Magician | Status |
| --- | --- | --- | --- |
| 1 | CPU | the LLM — 12 providers, local Ollama, routing per operation | **ships** |
| 2 | Process | the agent — 31 shipped, 29 with typed memory | **ships** |
| 3 | Scheduler | `system:scheduler`, `/triggers`, recurring monitors | **ships** |
| 4 | Process isolation | `magic-supervisor`, per-channel daemons, `magicutor` out-of-process, `governed_process_jail` | **ships** |
| 5 | Memory management | typed tiers, consolidation, retention over Parquet + LanceDB | **ships** |
| 6 | Filesystem | storage abstraction, scopes — Track A closed | **ships local**, cloud in flight |
| 7 | Permissions | `trust_level`, `denied_tools`, `denied_tool_params`, three token kinds | **ships**, resolver still `LocalPermissive` |
| 8 | Accounting | double-entry ledger, reserve→commit, budgets in `EMAIL_SENDS` / `WHATSAPP_SENDS` / `GITHUB_PUSHES` | **built**, 3/204 packs wired — and no pack declares whether it *should* be |
| 9 | Device drivers | 6 channel adapters — agentmail, gmail, kapso, telegram, telegram-self, whatsapp | **ships** |
| 10 | Package manager | skillshub — 100 skills, validate + install | **ships** |
| 11 | ✨ **App Platform** ✨ *(soon)* | `magician-app-contract` (9,535 LOC) + `magician-apps` (35,912) + TypeScript SDK, both in the workspace | **contract ships**, catalogue empty |

## Why the process row is the load-bearing one

An agent here is not a prompt with tools, and not a loop that calls them; those
are features of whatever model is pointed at this. A shipped agent is 500–760
lines of declaration — persona, invocation policy, allowed and denied tools,
denied tool *parameters*, trust level, typed memory tiers, consolidation rules,
circuit breaker, notification rules, retention, delegation targets.

`llm_routing` is **nullable**, and 11 of the 31 shipped agents name no model at
all. Route one from a frontier API to a local model and it is the same agent:
same memory, same obligations, same ledger, still answers to its name. Replace
its memory tiers and it is a different agent on the same model.

**The agent is what survives a model swap.** Identity tracks memory, not
cognition. Which is why a process is the right primitive for it, and why the
model belongs one row up as the CPU.

## Where this is honest about itself

Two rows are not finished, and they are the two the headline leans on hardest.

**#8 Accounting** is the "budget" in the sentence above. The ledger is real —
journal entries must balance per commodity (`LedgerError::NotBalanced`), with
reservations, period close, and two-phase reserve→commit. Commodities are not
only money, which is the unusual part: an agent can be budgeted in `EMAIL_SENDS`
and `GITHUB_PUSHES`, in consequences rather than dollars. But three capability packs of
204 declare `spend:` today — `web_answer` in `usd`, `agentmail-send` in
`EMAIL_SENDS`, `kapso-whatsapp-send` in `WHATSAPP_SENDS` — so the guarantee is
narrow. Never state it as "every tool call" or "every spend."

That 204 is every pack, and it flatters the gap in one direction while hiding a
worse one. Most of the surface is `awk`, `sed`, `read_file` — local operations
with nothing to meter, so counting them as unwired debt is unfair. Of the 38
skillshub packs that actually require credentials, 2 declare spend.

The real problem is that neither ratio can be made exact. Embedded pack defs have
no `auth` or `spend` field in their schema at all; `web_answer` declares spend by
adding a block the schema does not otherwise use. For 115 of the 204, spend
eligibility is not undeclared but **inexpressible** — nothing records which
capabilities consume a metered resource. Until something does, any coverage
fraction here is an estimate, which is how the previous figure (1/131) became
unreproducible.

**#11 ✨ App Platform ✨** *(soon)* is the answer to "won't a model vendor ship this." The
contract and SDK exist and compile in the workspace; the installable catalogue on
top of them does not exist yet.

Neither gap is a research problem. #8 is declaring `spend:` on packs that already
exist. #11 is a catalogue on a contract already written.

## Related

- [Resource Authority API](../components/magician/resource-authority-api.md) — the accounting row
- [Storage Abstraction](../components/magician/storage-abstraction.md) — the filesystem row
- [Agent templates](../../magician_data_v3/system/agent_templates/README.md) — the process row
- [Apps contract](../components/magician-apps/README.md) — the ✨ App Platform ✨
