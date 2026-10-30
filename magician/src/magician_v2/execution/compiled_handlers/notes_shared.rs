//! Shared argument reading and result shaping for the Notes tools.
//!
//! The HTTP surface already classifies a notes failure by `io::ErrorKind` —
//! a bad argument is a 400, a missing note a 404, a provider outage a 503. A
//! tool answers a model rather than a browser, so the same distinction is
//! carried as a `retryable` flag and a stable `reason`: without it every
//! failure reads alike and the model's only recovery is to try the identical
//! call again, which is right for an outage and wrong for a bad path.

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ArtifactV2Error;
use crate::magician_v2::notes::NoteRef;

/// Read a string argument, treating blank as absent so a model passing `""`
/// for "no preference" does not become an empty provider name.
///
/// Required-ness is the caller's to enforce, which is why this returns an
/// `Option` under one name rather than a `required_`/`optional_` pair that
/// differ only in what they promise.
pub fn text_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// A caller-fixable problem: the arguments were wrong, so retrying them
/// unchanged cannot succeed.
pub fn argument_error(reason: &str) -> Value {
    json!({
        "status": "error",
        "reason": reason,
        "retryable": false,
    })
}

/// The HTTP surface's own reading of which failures are transient.
/// Classify a notes IO failure.
///
/// `fallback` names what failed when the kind says nothing useful, so a
/// read-only tool never reports that a write failed. Everything the caller can
/// act on — bad argument, denied, missing, provider down — is decided here and
/// is the same whichever direction the call was going.
fn io_classification(error: &std::io::Error, fallback: &'static str) -> (&'static str, bool) {
    match error.kind() {
        std::io::ErrorKind::InvalidInput => ("invalid_note_request", false),
        std::io::ErrorKind::PermissionDenied => ("notes_permission_denied", false),
        std::io::ErrorKind::NotFound => ("note_not_found", false),
        std::io::ErrorKind::AlreadyExists => ("note_already_exists", false),
        std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::NotConnected => ("notes_provider_unavailable", true),
        _ => (fallback, false),
    }
}

/// Shape a notes write failure, preserving that reading.
pub fn write_error(tool: &str, error: std::io::Error) -> Value {
    let (reason, retryable) = io_classification(&error, "notes_write_failed");
    json!({
        "status": "error",
        "tool": tool,
        "reason": reason,
        "retryable": retryable,
        "detail": error.to_string(),
    })
}

/// Shape a task lookup failure.
///
/// A missing task and a filesystem hiccup are not the same answer: reporting
/// both as `task_not_found` tells the model to give up on work that would have
/// succeeded on a second call. Only the not-found variants are permanent.
pub fn task_lookup_error(tool: &str, error: ArtifactV2Error) -> Value {
    let (reason, retryable) = match &error {
        ArtifactV2Error::TaskNotFound(_) => ("task_not_found", false),
        ArtifactV2Error::Io(io) => io_classification(io, "task_lookup_failed"),
        _ => ("task_lookup_failed", false),
    };
    json!({
        "status": "error",
        "tool": tool,
        "reason": reason,
        "retryable": retryable,
        "detail": error.to_string(),
    })
}

/// Shape a notes read failure.
///
/// `open_note` and `search_notes` never write, so reporting `notes_write_failed`
/// from them tells the model something untrue about what the tool did — and a
/// model that believes a write was attempted may go looking for a half-written
/// note that cannot exist.
pub fn read_error(tool: &str, error: std::io::Error) -> Value {
    let (reason, retryable) = io_classification(&error, "notes_read_failed");
    json!({
        "status": "error",
        "tool": tool,
        "reason": reason,
        "retryable": retryable,
        "detail": error.to_string(),
    })
}

/// Shape a successful write.
///
/// `used_fallback` is surfaced rather than smoothed over: the note landed
/// somewhere other than the requested provider, and an agent that reports "saved
/// to your notes folder" when it went to the fallback folder has told the owner
/// something untrue.
pub fn note_result(note: NoteRef) -> Value {
    // A note written into a goal workspace may change who may read it.
    crate::magician_v2::execution::goal_workspace_access::invalidate(std::path::Path::new(
        &note.absolute_path,
    ));
    json!({
        "status": "ok",
        "path": note.path,
        "absolute_path": note.absolute_path,
        "provider": note.provider,
        "requested_provider": note.requested_provider,
        "used_fallback": note.used_fallback,
        "fallback_reason": note.fallback_reason,
        "open_url": note.open_url,
    })
}

/// The refusal, as a tool result, when the note at `relative` (a path in the
/// notes space) is somewhere the acting agent may not `op`: a goal workspace
/// it is not a member of, a private goal, a README only the admin agent
/// changes (`goal_workspace_access`). Checked in every provider root the
/// scope has, before any provider reads or writes, and those roots'
/// `Workspaces/` folders are registered on the way. Fails closed: when the
/// roots cannot be read, a path under `Workspaces/` is refused to every agent
/// but the admin.
pub async fn workspace_refusal(
    tool: &str,
    store: &crate::magician_v2::notes::NotesSettingsStore,
    principal: &str,
    workspace: &str,
    relative: &str,
    op: crate::magician_v2::execution::goal_workspace_access::Op,
) -> Option<Value> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    let agent = crate::magician_v2::execution::native_executors::current_acting_agent();
    let refused = |detail: String| {
        json!({
            "status": "error",
            "tool": tool,
            "reason": "notes_permission_denied",
            "retryable": false,
            "detail": detail,
        })
    };
    let relative_path = relative.trim().trim_start_matches(['/', '\\']);
    let roots = match store.note_provider_roots(principal, workspace).await {
        Ok(roots) => roots,
        Err(_) => {
            let admin = agent.as_deref() == Some(access::admin_agent().as_str());
            return (!admin
                && crate::magician_v2::notes::is_goal_workspace_relative_path(relative_path))
            .then(|| refused(format!("`{relative}` is in the goal workspaces folder, which cannot be checked right now")));
        },
    };
    access::register_notes_spaces(roots.iter().cloned());
    // Reads through a delegator count (`refusal_for_current`), as for the
    // file tools.
    roots
        .iter()
        .find_map(|root| access::refusal_for_current(&root.join(relative_path), op).map(refused))
}

/// [`workspace_refusal`] for an absolute path a provider already resolved.
pub fn workspace_refusal_at(
    tool: &str,
    absolute: &std::path::Path,
    op: crate::magician_v2::execution::goal_workspace_access::Op,
) -> Option<Value> {
    crate::magician_v2::execution::goal_workspace_access::refusal_for_current(absolute, op).map(
        |detail| {
            json!({
                "status": "error",
                "tool": tool,
                "reason": "notes_permission_denied",
                "retryable": false,
                "detail": detail,
            })
        },
    )
}

/// Goal workspaces through the notes tools: `search_notes` withholds and
/// counts, `open_note` and the writers refuse, by the acting agent.
#[cfg(test)]
mod goal_workspace_tests {
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::magician_v2::execution::compiled_handlers::{
        append_note, create_note, open_note, save_selection_to_note, search_notes,
    };
    use crate::magician_v2::execution::goal_workspace_access as access;
    use crate::magician_v2::execution::native_executors::ACTING_AGENT;
    use crate::magician_v2::notes::{NotesSettingsStore, WriteNoteMarkdownRequest};

    async fn setup() -> (
        tempfile::TempDir,
        Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let resources = access::test_resources(temp.path());
        let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
        for (path, markdown) in [
            ("Inbox/open.md", "# Open\n\nneedle\n"),
            ("Workspaces/Magican/README.md", "---\nmembers: [cmo]\n---\n# Magican\n"),
            (
                "Workspaces/Magican/Goals/X/x.md",
                "---\nmagician_kind: goal\nowner: cmo\nvisibility: internal\n---\n# x\n\nneedle\n",
            ),
            (
                "Workspaces/Magican/Goals/X/secret.md",
                "---\nmagician_kind: goal\nowner: user\nvisibility: private\n---\n# secret\n\nneedle\n",
            ),
        ] {
            store
                .write_note_markdown(
                    "owner",
                    "default",
                    WriteNoteMarkdownRequest {
                        provider: Some("local_markdown".into()),
                        target_path: path.into(),
                        markdown: markdown.into(),
                    },
                )
                .await
                .unwrap();
        }
        (temp, resources)
    }

    fn args(extra: Value) -> Value {
        let mut args = json!({"__principal": "owner", "__workspace": "default"});
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    }

    fn refused(value: &Value) -> bool {
        value["reason"] == "notes_permission_denied"
    }

    #[tokio::test]
    async fn search_notes_withholds_and_counts_what_the_agent_may_not_read() {
        let (_temp, resources) = setup().await;
        let search = |agent: &'static str| {
            let resources = Arc::clone(&resources);
            ACTING_AGENT.scope(Some(agent.to_string()), async move {
                search_notes::handle(resources, args(json!({"query": "needle"})))
                    .await
                    .unwrap()
            })
        };
        let paths = |value: &Value| -> Vec<String> {
            value["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|hit| hit["relative_path"].as_str().unwrap().to_string())
                .collect()
        };
        let outsider = search("ambassador").await;
        assert_eq!(
            paths(&outsider),
            vec!["Inbox/open.md".to_string()],
            "{outsider}"
        );
        assert_eq!(outsider["withheld_count"], 2);
        assert!(!outsider.to_string().contains("secret"));
        let member = search("cmo").await;
        assert!(paths(&member).contains(&"Workspaces/Magican/Goals/X/x.md".to_string()));
        assert!(!paths(&member)
            .iter()
            .any(|path| path.ends_with("secret.md")));
        assert_eq!(member["withheld_count"], 1);
        let admin = search(access::DEFAULT_ADMIN_AGENT).await;
        assert_eq!(admin["withheld_count"], 0);
    }

    #[tokio::test]
    async fn open_note_refuses_a_goal_the_agent_may_not_read() {
        let (_temp, resources) = setup().await;
        let open = |agent: &'static str, path: &'static str| {
            let resources = Arc::clone(&resources);
            ACTING_AGENT.scope(Some(agent.to_string()), async move {
                open_note::handle(resources, args(json!({"path": path})))
                    .await
                    .unwrap()
            })
        };
        assert!(refused(
            &open("ambassador", "Workspaces/Magican/Goals/X/x.md").await
        ));
        assert!(refused(
            &open("cmo", "Workspaces/Magican/Goals/X/secret.md").await
        ));
        assert_eq!(
            open("cmo", "Workspaces/Magican/Goals/X/x.md").await["status"],
            "ok"
        );
        assert_eq!(open("ambassador", "Inbox/open.md").await["status"], "ok");
    }

    #[tokio::test]
    async fn the_note_writers_refuse_a_goal_workspace_target_the_agent_may_not_write() {
        let (temp, resources) = setup().await;
        let call = |agent: &'static str, tool: &'static str, extra: Value| {
            let resources = Arc::clone(&resources);
            ACTING_AGENT.scope(Some(agent.to_string()), async move {
                let args = args(extra);
                match tool {
                    "append_note" => append_note::handle(resources, args).await,
                    "create_note" => create_note::handle(resources, args).await,
                    _ => save_selection_to_note::handle(resources, args).await,
                }
                .unwrap()
            })
        };
        let goal = json!({"body": "more", "target_path": "Workspaces/Magican/Goals/X/x.md"});
        assert!(refused(
            &call("ambassador", "append_note", goal.clone()).await
        ));
        assert!(refused(
            &call(
                "cmo",
                "append_note",
                json!({"body": "more", "target_path": "Workspaces/Magican/Goals/X/secret.md"}),
            )
            .await
        ));
        assert!(refused(
            &call(
                "cmo",
                "append_note",
                json!({"body": "members: [ambassador]", "target_path": "Workspaces/Magican/README.md"}),
            )
            .await
        ));
        assert!(refused(
            &call(
                "cmo",
                "create_note",
                json!({"title": "Plan", "body": "x", "target_dir": "Workspaces/Other"}),
            )
            .await
        ));
        assert!(refused(
            &call(
                "ambassador",
                "save_selection_to_note",
                json!({"text": "quote", "target_path": "Workspaces/Magican/Goals/X/x.md"}),
            )
            .await
        ));
        let space = resources
            .artifact_workspace
            .scope_root("owner", "default")
            .join("notes")
            .join("space");
        let secret =
            std::fs::read_to_string(space.join("Workspaces/Magican/Goals/X/secret.md")).unwrap();
        assert!(!secret.contains("more"));
        assert!(!space.join("Workspaces/Other").exists());
        assert_eq!(call("cmo", "append_note", goal).await["status"], "ok");
        drop(temp);
    }
}
