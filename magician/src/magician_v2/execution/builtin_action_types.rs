//! Centralized built-in action-lane metadata shared by decision, autonomy, and
//! prompt/tool filtering code.

// `browser` and `duckdb` are intentionally absent — they are pack-dispatched
// inner-loop tools, not built-in lanes.
pub fn canonical_decision_builtin_action_type(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "file" | "files" => Some("file"),
        "http" => Some("http"),
        "bash" | "shell" => Some("bash"),
        _ => None,
    }
}

pub fn builtin_action_type_for_tool_name(name: &str) -> Option<&'static str> {
    canonical_decision_builtin_action_type(name).or_else(|| {
        match name.trim().to_ascii_lowercase().as_str() {
            "api" | "rest" => Some("http"),
            _ => None,
        }
    })
}

pub fn is_reserved_non_pack_capability_name(name: &str) -> bool {
    builtin_action_type_for_tool_name(name).is_some()
}

/// The one spelling of each dispatch policy name that no pack registers.
///
/// # Why these are constants rather than literals in two places
///
/// The executor maps every consequential action onto one of these names before
/// an authority is consulted
/// ([`crate::magician_v2::execution::agentic::executable_action_policy_name`]),
/// and the ceiling validator refuses any entry that is not one of them. Those
/// were two hand-written lists of the same strings, and the failure mode of
/// their drifting apart is silent and bad in one direction: a ceiling entry the
/// executor never emits is compared to nothing, so the act is denied while the
/// owner reading the roster sees the tool listed and concludes it was
/// permitted — **a grant that grants nothing while reading as a grant.**
///
/// Naming each spelling once removes that direction of drift entirely: the
/// executor and the validator now quote the same constant, so they cannot
/// disagree about how a name is spelled. What is still possible is a *new*
/// action variant nobody adds to the lists below — narrower, and visible,
/// because the variant's arm is right beside the constant it fails to use.
pub const POLICY_NAME_DUCKDB: &str = "duckdb";
/// See [`POLICY_NAME_DUCKDB`].
pub const POLICY_NAME_FILES: &str = "files";
/// See [`POLICY_NAME_DUCKDB`].
pub const POLICY_NAME_HTTP: &str = "http";
/// See [`POLICY_NAME_DUCKDB`].
pub const POLICY_NAME_SHELL: &str = "shell";
/// See [`POLICY_NAME_DUCKDB`].
pub const POLICY_NAME_SPAWN_SUB_GOAL: &str = "spawn_sub_goal";
/// See [`POLICY_NAME_DUCKDB`]. Bounded by `team`, not by a ceiling.
pub const POLICY_NAME_DELEGATE_TO_AGENT: &str = "delegate_to_agent";
/// See [`POLICY_NAME_DUCKDB`]. Bounded by `team`, not by a ceiling.
pub const POLICY_NAME_HANDOVER_TO_AGENT: &str = "handover_to_agent";

/// Dispatch policy names that no capability pack registers.
///
/// Every consequential action is mapped onto one policy name before an
/// authority is consulted (`executable_action_policy_name` in the agentic
/// executor). Most of those names are pack names a
/// [`crate::magician_v2::execution::capability::CapabilityRegistry`] holds;
/// these are not, and an authority that could not name them could never permit
/// a sub-goal, a file write, an HTTP call, a shell command or a DuckDB query.
///
/// **Spelled exactly as the executor spells them, and no aliases.** It emits
/// `files` and `shell`, never `file` or `bash` — so an authority that accepted
/// an alias would record a name no dispatch will ever compare equal to. That is
/// a grant that grants nothing while reading as a grant, which is strictly worse
/// than a refusal: the owner believes they permitted something and the run fails
/// somewhere else entirely. [`is_reserved_non_pack_capability_name`] above is
/// deliberately *not* reused here for that reason — it answers a different
/// question ("could this name be a pack?") and accepts the aliases.
///
/// `duckdb` is listed even though the production binary also registers a pack of
/// that name: the action variant exists whether or not the pack was registered,
/// so an authority that named it only when the registry happened to hold it
/// would permit different things in two processes.
pub const CEILING_NAMEABLE_NON_PACK_POLICY_NAMES: [&str; 5] = [
    POLICY_NAME_DUCKDB,
    POLICY_NAME_FILES,
    POLICY_NAME_HTTP,
    POLICY_NAME_SHELL,
    POLICY_NAME_SPAWN_SUB_GOAL,
];

/// Dispatch policy names whose bound is a **target**, not a capability.
///
/// Delegation and handover name another agent, so an authority that bounds what
/// may be *done* is never consulted for them — the bound that applies is the one
/// over who may be delegated to. Naming one of these in a capability ceiling is
/// therefore inert, and inert is indistinguishable from granted when an owner
/// reads it back.
///
/// Kept beside the list above so a caller validating a ceiling can tell the two
/// apart and say *which* bound the name belongs to, rather than reporting a real
/// action name as unknown.
pub const DELEGATION_DISPATCH_POLICY_NAMES: [&str; 2] =
    [POLICY_NAME_DELEGATE_TO_AGENT, POLICY_NAME_HANDOVER_TO_AGENT];

/// Whether a capability ceiling may name this without the entry being inert.
pub fn is_ceiling_nameable_non_pack_policy_name(name: &str) -> bool {
    CEILING_NAMEABLE_NON_PACK_POLICY_NAMES.contains(&name)
}

/// Whether this policy name is bounded by a delegation target rather than by a
/// capability ceiling.
pub fn is_delegation_dispatch_policy_name(name: &str) -> bool {
    DELEGATION_DISPATCH_POLICY_NAMES.contains(&name)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        builtin_action_type_for_tool_name, canonical_decision_builtin_action_type,
        is_ceiling_nameable_non_pack_policy_name, is_delegation_dispatch_policy_name,
        is_reserved_non_pack_capability_name, CEILING_NAMEABLE_NON_PACK_POLICY_NAMES,
        DELEGATION_DISPATCH_POLICY_NAMES,
    };

    #[test]
    fn decision_builtin_action_type_aliases_are_canonicalized() {
        assert_eq!(
            canonical_decision_builtin_action_type("files"),
            Some("file")
        );
        assert_eq!(
            canonical_decision_builtin_action_type("shell"),
            Some("bash")
        );
        assert_eq!(canonical_decision_builtin_action_type("duckdb"), None);
    }

    #[test]
    fn browser_is_not_a_builtin_lane_anymore() {
        // Browser is a pack-defined inner-loop tool, not a built-in lane.
        assert_eq!(canonical_decision_builtin_action_type("browser"), None);
        assert!(!is_reserved_non_pack_capability_name("browser"));
    }

    #[test]
    fn builtin_tool_name_mapping_keeps_http_aliases() {
        assert_eq!(builtin_action_type_for_tool_name("api"), Some("http"));
        assert_eq!(builtin_action_type_for_tool_name("rest"), Some("http"));
        assert_eq!(builtin_action_type_for_tool_name("websearch"), None);
    }

    /// The ceiling-nameable list holds the executor's exact spellings and no
    /// aliases.
    ///
    /// Pins the grant that grants nothing: `executable_action_policy_name` emits
    /// `files` and `shell`, so a ceiling entry of `file` or `bash` is compared
    /// against nothing for the whole life of the engagement. Accepting an alias
    /// here is how that entry gets written.
    #[test]
    fn ceiling_nameable_policy_names_are_the_executors_spellings_not_the_aliases() {
        assert_eq!(
            CEILING_NAMEABLE_NON_PACK_POLICY_NAMES,
            ["duckdb", "files", "http", "shell", "spawn_sub_goal"]
        );
        for exact in ["files", "http", "shell", "spawn_sub_goal", "duckdb"] {
            assert!(
                is_ceiling_nameable_non_pack_policy_name(exact),
                "`{exact}` is a name a dispatch really compares against"
            );
        }
        for alias in ["file", "bash", "api", "rest", "FILES", "Shell"] {
            assert!(
                !is_ceiling_nameable_non_pack_policy_name(alias),
                "`{alias}` is never emitted as a policy name, so a ceiling holding it \
                 authorises nothing while reading as a grant"
            );
        }
    }

    /// Delegation names are a separate list, and neither list contains the
    /// other's members.
    ///
    /// Pins two failures. Folding `delegate_to_agent` into the ceiling list
    /// would let an owner write it into a capability ceiling that is never
    /// consulted for delegation, so the entry reads as a grant and bounds
    /// nothing. Dropping the list entirely would make the same name report as an
    /// unknown capability, sending the owner to fix a spelling that was right.
    #[test]
    fn delegation_policy_names_are_named_apart_from_the_ceiling_nameable_ones() {
        assert_eq!(
            DELEGATION_DISPATCH_POLICY_NAMES,
            ["delegate_to_agent", "handover_to_agent"]
        );
        for target_bounded in DELEGATION_DISPATCH_POLICY_NAMES {
            assert!(is_delegation_dispatch_policy_name(target_bounded));
            assert!(
                !is_ceiling_nameable_non_pack_policy_name(target_bounded),
                "`{target_bounded}` is bounded by the delegation target list, never by a \
                 capability ceiling"
            );
        }
        for capability_bounded in CEILING_NAMEABLE_NON_PACK_POLICY_NAMES {
            assert!(!is_delegation_dispatch_policy_name(capability_bounded));
        }
    }

    #[test]
    fn reserved_non_pack_names_exclude_primitive_pack_names() {
        assert!(!is_reserved_non_pack_capability_name("duckdb"));
        assert!(!is_reserved_non_pack_capability_name("browser"));
        assert!(!is_reserved_non_pack_capability_name("websearch"));
    }
}
