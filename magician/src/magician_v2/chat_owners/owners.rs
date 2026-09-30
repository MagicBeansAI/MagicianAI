//! Cataloged Task 13 chat and progress owners and the relative paths they may hold.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChatOwner {
    ChatSessions,
    ChatMessages,
    ChatTranscripts,
    ChatTurnEvents,
    ChatEnrollments,
    ProgressEvents,
    ProgressSubscriptions,
    ProgressLineage,
    UiPreferences,
    MediaPreferences,
}

impl ChatOwner {
    pub const ALL: [ChatOwner; 10] = [
        Self::ChatSessions,
        Self::ChatMessages,
        Self::ChatTranscripts,
        Self::ChatTurnEvents,
        Self::ChatEnrollments,
        Self::ProgressEvents,
        Self::ProgressSubscriptions,
        Self::ProgressLineage,
        Self::UiPreferences,
        Self::MediaPreferences,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::ChatSessions => "chat_sessions",
            Self::ChatMessages => "chat_messages",
            Self::ChatTranscripts => "chat_transcripts",
            Self::ChatTurnEvents => "chat_turn_events",
            Self::ChatEnrollments => "chat_enrollments",
            Self::ProgressEvents => "progress_channels",
            Self::ProgressSubscriptions => "progress_subscriptions",
            Self::ProgressLineage => "progress_lineage",
            Self::UiPreferences => "ui_preferences",
            Self::MediaPreferences => "media_preferences",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::ChatSessions => "ui/chat_sessions/sess-1/session.json",
            Self::ChatMessages => "ui/chat_sessions/sess-1/messages/000000.jsonl",
            Self::ChatTranscripts => "ui/chat_sessions/sess-1/llm_history/manifest.json",
            Self::ChatTurnEvents => "ui/chat_turn_events/turn-1.jsonl",
            Self::ChatEnrollments => "chat/enrollments.json",
            Self::ProgressEvents => "progress_channels/events/exec-1.jsonl",
            Self::ProgressSubscriptions => "progress_channels/subscriptions.json",
            Self::ProgressLineage => "progress_channels/lineage_index.json",
            Self::UiPreferences => "ui/preferences.json",
            Self::MediaPreferences => "media/preferences.json",
        }
    }

    pub fn allows(self, rel: &str) -> bool {
        if rel.starts_with('/') || std::path::Path::new(rel).is_absolute() {
            return false;
        }
        let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
        if parts.is_empty()
            || parts
                .iter()
                .any(|part| *part == "." || *part == ".." || part.contains('\0'))
        {
            return false;
        }
        match self {
            Self::ChatSessions => session_record(&parts),
            Self::ChatMessages => {
                prefix(&parts, &["ui", "chat_sessions"], 5) && parts[3] == "messages"
            },
            Self::ChatTranscripts => {
                prefix(&parts, &["ui", "chat_sessions"], 5) && parts[3] == "llm_history"
            },
            Self::ChatTurnEvents => prefix(&parts, &["ui", "chat_turn_events"], 3),
            Self::ChatEnrollments => parts == ["chat", "enrollments.json"],
            Self::ProgressEvents => prefix(&parts, &["progress_channels", "events"], 3),
            Self::ProgressSubscriptions => parts == ["progress_channels", "subscriptions.json"],
            Self::ProgressLineage => parts == ["progress_channels", "lineage_index.json"],
            Self::UiPreferences => parts == ["ui", "preferences.json"],
            Self::MediaPreferences => parts == ["media", "preferences.json"],
        }
    }

    pub fn claiming(rel: &str) -> Option<ChatOwner> {
        Self::ALL.into_iter().find(|owner| owner.allows(rel))
    }

    pub fn root(
        self,
        workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
    ) -> PathBuf {
        workspace.scope_root(principal, workspace_name)
    }
}

fn prefix(parts: &[&str], head: &[&str], min_len: usize) -> bool {
    parts.len() >= min_len && parts.get(..head.len()) == Some(head)
}

fn session_excluded(name: &str) -> bool {
    matches!(name, "messages" | "llm_history" | "outputs")
}

fn session_record(parts: &[&str]) -> bool {
    if !prefix(parts, &["ui", "chat_sessions"], 3) {
        return false;
    }
    if parts[2] == ".lifecycle" {
        return parts.len() >= 4;
    }
    if parts.len() == 3 {
        return parts[2].ends_with(".json");
    }
    parts.len() >= 4 && !session_excluded(parts[3])
}

pub fn walk_files(scope_root: &Path, owner: ChatOwner) -> Vec<String> {
    let mut out = Vec::new();
    let _ = collect(scope_root, scope_root, owner, &mut out);
    out.sort();
    out.dedup();
    out.into_iter().filter(|rel| owner.allows(rel)).collect()
}

fn collect(
    scope_root: &Path,
    dir: &Path,
    owner: ChatOwner,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                continue;
            }
        }
        if path.is_dir() {
            collect(scope_root, &path, owner, out)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(scope_root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if owner.allows(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    Ok(())
}
