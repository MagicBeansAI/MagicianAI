//! Hosted Jev is the `typesafe` profile of the System One adapter. These
//! names predate the generic adapter and stay as aliases for existing
//! callers; [`TypesafeConfig::new`] builds the keyed, calibrated profile.

pub use super::systemone::{
    typesafe_capabilities, SystemOneConfig as TypesafeConfig,
    SystemOneDecisionModel as TypesafeDecisionModel,
};
