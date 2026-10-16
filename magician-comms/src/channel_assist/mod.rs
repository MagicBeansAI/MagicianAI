//! Mail Assist — Gmail metadata data plane + annotation store (Phase 1).
//!
//! Design: `docs/plans/2026-07-05-mail-assist-phase1-design.md` (§2 is the
//! storage + captured-fields contract). Phase 1 persists thread/message
//! METADATA only — never bodies, snippets, attachments, or full recipient
//! lists (to/cc are stored as domains only). Sensitive rows keep their ids
//! but carry a redacted subject. Annotation lifecycle changes append to an
//! audit table and never delete; the per-account sync watermark lives in
//! its own small table beside the four design tables so `sync/status` can
//! report cursors without scanning threads.
//!
//! The store is the channel-adapter boundary: rows are provider-neutral,
//! keyed `(provider, account_alias, thread_id)` with an opaque
//! `provider_cursor` (for gmail it holds the `historyId`). Gmail-specific
//! naming stays inside `gws_client`/`adapters::gmail` — everything above the
//! store is provider-blind (see the design doc's "Channel adapter boundary").
//! This physical directory remains named `mail_assist` until the storage/file
//! migration. Public imports use `magician_comms::channel_assist` and the
//! channel-neutral aliases from [`channel`].
//!
//! Phase 1b (`2026-07-05-channel-assist-phase1b-design.md`) generalizes
//! the plane to multiple channels: accounts live in the `channel_assist`
//! registry (`registry`), pull-shaped providers implement the
//! [`ingest::ChannelIngestor`] trait, and rows carry a `lane`
//! (`user_assist` | `envoy`) plus local-distillation fields
//! (`direction`/`summary`/`intent`/`distill_state`). Gmail body content
//! for distillation is fetched `format=full` and parsed by the pure
//! `content` pipeline — in process memory only, NEVER persisted. The
//! `distill` queue worker derives `{summary, intent}` EXCLUSIVELY on the
//! local (ollama-family) profile bound to `channel_ingest_distill`,
//! enforced by a fail-closed guard with no remote fallback path.
//!
//! Plan workstream 3.1 splits this directory into the product lane and the
//! substrate: the product decisions live behind the [`assist`] seam
//! (classification, reconciliation, distillation, drafting,
//! writing-preference, fixtures/evals, quality budgets), while everything
//! the seam does NOT own — adapters, ingestors, sync, store, provider
//! registry — stays in place as Layer 1 substrate. The pre-3.1 flat module
//! names were re-export shims and were removed by Phase 5 (batch 4 of the
//! 2026-08-28 removal inventory); import the product modules through
//! [`assist`] directly.

pub mod attention_lane_bridge;
pub mod attention_learning;
pub mod canonical_attention;
pub mod governance;
pub mod memory_api;
pub mod resurfacing;

pub mod assist;

pub mod adapter_registry;
pub mod adapters;
pub mod channel;
pub mod channel_observe;
pub mod channel_providers;
pub mod evidence_bridge;
pub mod feedback_bridge;
pub mod gws_client;
pub mod ingest;
pub mod ingest_agentmail;
pub mod ingest_imessage;
pub mod ingest_kapso;
pub mod ingest_telegram;
pub mod ingest_whatsapp;
pub mod live_content;
pub mod pattern_synthesis;
pub mod registry;
pub mod sensitivity;
pub mod store;
pub mod sync;
pub mod telemetry;
pub mod types;
pub mod verification_sources;
