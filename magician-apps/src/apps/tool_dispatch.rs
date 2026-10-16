//! Catalog-facing notes for app tool dispatch.
//!
//! The bind/contain/receipt kernel lives in [`super::app_tool_bind`]. This
//! module only re-exports so authoring and review do not grow a second table.

pub use magician::magician_v2::apps::app_tool_bind::{
    app_tool_dispatch_note, app_tool_dispatch_note_with_shape, app_tool_is_dispatchable,
    bind_parameters_for_call, plan_app_tool_call, plan_app_tool_call_with_shape,
    tool_needs_workdir, AppToolBindEvidence, AppToolBindPlan, AppToolContainProfile,
    AppToolDeclaredShape, AppToolDispatchNote, AppToolIoKind,
};
