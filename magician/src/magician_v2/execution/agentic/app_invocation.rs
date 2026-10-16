//! Stable App invocation identity for one attributed tool call.

/// The caller validates the runtime effect ID before entering this function.
/// Iteration numbers cannot identify a call: a model turn may dispatch several
/// tools, and a recovered call may run at a different local iteration number.
pub(super) fn for_effect(session_id: &str, effect_id: &str) -> String {
    format!("app-action:{}:{session_id}:{effect_id}", session_id.len())
}
