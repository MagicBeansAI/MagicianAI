//! Decision-model adapters.
//!
//! An adapter owns the wire format, auth, retries, and model id — nothing
//! else. Lane policy, shadow decisions, and thresholds live in the host's
//! composition. `systemone` serves every model speaking `/v1/systemone` —
//! hosted Jev (the `typesafe` profile) and self-hosted laya / Kev alike;
//! `generative` (same IR answered through a chat model) follows per the plan.

pub mod kev;
#[cfg(feature = "mlx")]
pub mod kev_mlx;
#[cfg(feature = "onnx")]
pub mod kev_onnx;
pub mod laya;
#[cfg(any(feature = "onnx", feature = "mlx"))]
pub(crate) mod laya_assets;
#[cfg(feature = "mlx")]
pub mod laya_mlx;
#[cfg(feature = "onnx")]
pub mod laya_onnx;
pub mod memory;
#[cfg(feature = "mlx")]
pub(crate) mod mlx_common;
pub mod systemone;
pub mod typesafe;

pub use memory::MemoryDecisionModel;
pub use systemone::{SystemOneConfig, SystemOneDecisionModel};
pub use typesafe::{TypesafeConfig, TypesafeDecisionModel};
