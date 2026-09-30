//! Stable compatibility identities for centrally owned App runtime semantics.
//!
//! These exact identities anchor the reviewed 2026-09-09 deployed baseline.
//! They are deliberately its former source digests so existing immutable
//! locks do not need rewriting or an inferred permission migration. Source
//! evidence is reported separately by the authoring diagnostics. A breaking
//! change to authority, schemas, effects, receipts or recovery MUST introduce
//! a new contract identity and explicit migration, not keep these constants.
//! See docs/components/magician/app-runtime-compatibility.md.

pub(crate) const RECIPE_CONTRACT_V1: &str =
    "blake3:b341b9bd7a597cb9a5bdc3c869eda92c603a930603a33148f0b62dba136feccd";
pub(crate) const COMPILED_OWNER_CONTRACT_V1: &str =
    "blake3:db0575b27747c5ce58b43de307f8a6aecc3a1b5b65f110d44b3f2dfc2ba4640f";
