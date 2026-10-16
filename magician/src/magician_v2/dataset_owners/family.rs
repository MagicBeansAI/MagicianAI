//! Cataloged Task 11 Parquet families and their current layouts.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DatasetFamily {
    Events,
    MemoryEvents,
    ActivityRows,
    ActivityRollups,
    LlmCalls,
    LlmEmbeddings,
    LlmProviderAttempts,
    LlmToolCalls,
    LlmCaptureGaps,
    LlmDispatch,
    LlmCallIo,
    LlmContextBlocks,
    LlmContentTombstones,
    LlmContentAccessAudit,
}

impl DatasetFamily {
    pub const ALL: [DatasetFamily; 14] = [
        Self::Events,
        Self::MemoryEvents,
        Self::ActivityRows,
        Self::ActivityRollups,
        Self::LlmCalls,
        Self::LlmEmbeddings,
        Self::LlmProviderAttempts,
        Self::LlmToolCalls,
        Self::LlmCaptureGaps,
        Self::LlmDispatch,
        Self::LlmCallIo,
        Self::LlmContextBlocks,
        Self::LlmContentTombstones,
        Self::LlmContentAccessAudit,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::MemoryEvents => "memory_events",
            Self::ActivityRows => "activity_rows",
            Self::ActivityRollups => "activity_rollups",
            Self::LlmCalls => "llm_calls",
            Self::LlmEmbeddings => "llm_embeddings",
            Self::LlmProviderAttempts => "llm_provider_attempts",
            Self::LlmToolCalls => "llm_tool_calls",
            Self::LlmCaptureGaps => "llm_capture_gaps",
            Self::LlmDispatch => "llm_dispatch",
            Self::LlmCallIo => "llm_call_io",
            Self::LlmContextBlocks => "llm_context_blocks",
            Self::LlmContentTombstones => "llm_content_tombstones",
            Self::LlmContentAccessAudit => "llm_content_access_audit",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::ActivityRows => "dt=2026-08-31/hour=00/batch_test.parquet",
            _ => "dt=2026-08-31/batch_test.parquet",
        }
    }

    pub fn glob_suffix(self) -> &'static str {
        match self {
            Self::ActivityRows => "dt=*/hour=*/*.parquet",
            _ => "dt=*/*.parquet",
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
        let last = *parts.last().unwrap();
        if !last.ends_with(".parquet") {
            return false;
        }
        match self {
            Self::ActivityRows => {
                (parts.len() == 3 && parts[0].starts_with("dt=") && parts[1].starts_with("hour="))
                    || (parts.len() == 2 && parts[0].starts_with("dt="))
            },
            _ => {
                (parts.len() == 2 && parts[0].starts_with("dt="))
                    || (parts.len() == 3 && parts[0].starts_with("dt=") && parts[1] == "_compact")
            },
        }
    }

    pub fn root(
        self,
        workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
    ) -> PathBuf {
        match self {
            Self::Events => workspace
                .analytics_root(principal, workspace_name)
                .join("events"),
            Self::MemoryEvents => workspace.analytics_memory_events_root(principal, workspace_name),
            Self::ActivityRows => workspace.analytics_activity_rows_root(principal, workspace_name),
            Self::ActivityRollups => {
                workspace.analytics_activity_rollups_root(principal, workspace_name)
            },
            Self::LlmCalls => workspace.analytics_llm_calls_root(principal, workspace_name),
            Self::LlmEmbeddings => {
                workspace.analytics_llm_embeddings_root(principal, workspace_name)
            },
            Self::LlmProviderAttempts => {
                workspace.analytics_llm_provider_attempts_root(principal, workspace_name)
            },
            Self::LlmToolCalls => {
                workspace.analytics_llm_tool_calls_root(principal, workspace_name)
            },
            Self::LlmCaptureGaps => {
                workspace.analytics_llm_capture_gaps_root(principal, workspace_name)
            },
            Self::LlmDispatch => workspace
                .analytics_root(principal, workspace_name)
                .join("llm_dispatch"),
            Self::LlmCallIo => workspace.analytics_llm_call_io_root(principal, workspace_name),
            Self::LlmContextBlocks => {
                workspace.analytics_llm_context_blocks_root(principal, workspace_name)
            },
            Self::LlmContentTombstones => {
                workspace.analytics_llm_content_tombstones_root(principal, workspace_name)
            },
            Self::LlmContentAccessAudit => {
                workspace.analytics_llm_content_access_audit_root(principal, workspace_name)
            },
        }
    }
}

/// Compatibility DuckDB glob. Same files as today's `dt=*/*.parquet` readers.
pub fn family_parquet_glob(root: impl AsRef<Path>, family: DatasetFamily) -> PathBuf {
    root.as_ref().join(family.glob_suffix())
}

pub fn family_read_glob(root: impl AsRef<Path>, family: DatasetFamily) -> String {
    family_parquet_glob(root, family)
        .display()
        .to_string()
        .replace('\'', "''")
}
