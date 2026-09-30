//! Runtime services owned by the magician binary process.
//!
//! Modules here manage long-lived external processes / health-checked
//! resources whose lifecycle is tied to magician's own boot/shutdown.

pub mod device_transport;
pub mod edge_sessions;
pub mod ollama_lifecycle;
pub mod startup;
