//! Task 19 Track B operator activation.
//!
//! Default Magician startup does not inventory, plan, or cut over. These
//! surfaces run only from `magician storage …` or explicit `/storage/activation`
//! requests. Cutover stays fail-closed until Decision Gate 3 is accepted.

mod cli;
mod preconditions;
mod report;
mod workflow;

pub use cli::{run_storage_command, StorageCommand};
pub use preconditions::{
    evaluate_preconditions, ActivationContext, GateStatus, PreconditionReport, GATE3_CLOSED,
};
pub use report::{ActivationOperation, ActivationReport, QualifiedOperation};
pub use workflow::{OperatorWorkflow, CUTOVER_CONFIRM, ROLLBACK_CONFIRM};

#[cfg(test)]
mod tests;
