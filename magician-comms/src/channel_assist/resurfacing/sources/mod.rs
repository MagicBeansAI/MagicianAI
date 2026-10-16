//! Comms corpus sources for the lib attention service's source trait (plan
//! workstream 3.0). The `ResurfacingSource` trait and the memory/
//! task-episode adapters live lib-side at
//! `magician::magician_v2::attention::resurfacing::sources`; the comms
//! adapter below reads `ChannelAssistStore` and implements the trait from
//! this crate, keeping the lib free of comms types.

pub mod comms;
