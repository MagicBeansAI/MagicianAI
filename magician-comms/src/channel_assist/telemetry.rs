//! Channel-assist LLM telemetry emission.
//!
//! The `llm_calls` Parquet lakehouse (what `/llm` + Today's Pulse read) is fed
//! by `LLMResponseReceived` runtime-transport events, which the agentic
//! executor / chat / coding layers emit. The distill + classify workers call
//! the operation router DIRECTLY (they're lightweight background loops, not
//! agent runs), so they skip those layers and would never be recorded. This
//! emits an operation-tagged `LLMResponseReceived` after each channel-op router
//! call so `channel_ingest_distill` + `channel_classify` land in the lakehouse
//! with real tokens + priced cost — `$0` on local ollama, real cost the moment
//! the classifier is bound to a remote profile.

pub use magician::magician_v2::llm_dispatch_seam::emit_mail_llm_call;
