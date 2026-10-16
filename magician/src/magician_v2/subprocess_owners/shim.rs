//! Reviewed compatibility shim for batches that still resolve the old root.
//!
//! The shim cannot expose arbitrary engine roots into a closed child. It only
//! records owner-specific deprecation metrics when a compatibility path is used.

use super::owners::SubprocessOwner;

pub fn emit_compat_metric(owner: SubprocessOwner) {
    tracing::warn!(
        owner = owner.id(),
        metric = "magician.storage.subprocess.compat",
        "compatibility shim resolved a legacy runtime-root path"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_names_the_owner() {
        emit_compat_metric(SubprocessOwner::WorkdirsScratch);
        emit_compat_metric(SubprocessOwner::SkillWorking);
    }
}
