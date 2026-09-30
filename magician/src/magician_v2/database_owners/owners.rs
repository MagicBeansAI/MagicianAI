//! Cataloged Task 15 SQLite/DuckDB lifecycle owners and their files.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DatabaseOwner {
    AnalyticsDuckdb,
    ChannelAssistDuckdb,
    UiThreadsDuckdb,
    FeedDuckdb,
    BrowserEngineUsage,
    AppStoreSqlite,
    SocialSqlite,
    ApiMining,
    AttentionLearning,
    AttentionFunnel,
    Resurfacing,
    HitlLifecycle,
}

impl DatabaseOwner {
    pub const ALL: [DatabaseOwner; 12] = [
        Self::AnalyticsDuckdb,
        Self::ChannelAssistDuckdb,
        Self::UiThreadsDuckdb,
        Self::FeedDuckdb,
        Self::BrowserEngineUsage,
        Self::AppStoreSqlite,
        Self::SocialSqlite,
        Self::ApiMining,
        Self::AttentionLearning,
        Self::AttentionFunnel,
        Self::Resurfacing,
        Self::HitlLifecycle,
    ];

    pub const TENANT: [DatabaseOwner; 8] = [
        Self::AnalyticsDuckdb,
        Self::ChannelAssistDuckdb,
        Self::UiThreadsDuckdb,
        Self::FeedDuckdb,
        Self::BrowserEngineUsage,
        Self::AppStoreSqlite,
        Self::SocialSqlite,
        Self::ApiMining,
    ];

    pub const HOST: [DatabaseOwner; 4] = [
        Self::AttentionLearning,
        Self::AttentionFunnel,
        Self::Resurfacing,
        Self::HitlLifecycle,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::AnalyticsDuckdb => "analytics_duckdb",
            Self::ChannelAssistDuckdb => "channel_assist_duckdb",
            Self::UiThreadsDuckdb => "ui_threads_duckdb",
            Self::FeedDuckdb => "feed_duckdb",
            Self::BrowserEngineUsage => "browser_engine_usage_sqlite",
            Self::AppStoreSqlite => "app_store_sqlite",
            Self::SocialSqlite => "social_sqlite",
            Self::ApiMining => "api_mining",
            Self::AttentionLearning => "attention_learning_sqlite",
            Self::AttentionFunnel => "attention_funnel_sqlite",
            Self::Resurfacing => "resurfacing_sqlite",
            Self::HitlLifecycle => "hitl_lifecycle_sqlite",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::AnalyticsDuckdb => "analytics/analytics.duckdb",
            Self::ChannelAssistDuckdb => "mail_assist/mail_assist.duckdb",
            Self::UiThreadsDuckdb => "ui/threads/ui_threads.duckdb",
            Self::FeedDuckdb => "ui/feed/feed.duckdb",
            Self::BrowserEngineUsage => "analytics/browser_engine_usage.sqlite3",
            Self::AppStoreSqlite => "apps/app_store.sqlite3",
            Self::SocialSqlite => "social/social.db",
            Self::ApiMining => "api_mining/projections/_rows.db",
            Self::AttentionLearning => "attention_learning.db",
            Self::AttentionFunnel => "attention_funnel.db",
            Self::Resurfacing => "resurfacing.db",
            Self::HitlLifecycle => "pending_hitl_lifecycle.v3.sqlite3",
        }
    }

    pub fn is_host(self) -> bool {
        matches!(
            self,
            Self::AttentionLearning
                | Self::AttentionFunnel
                | Self::Resurfacing
                | Self::HitlLifecycle
        )
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
        if self == Self::ApiMining {
            return prefix(&parts, &["api_mining"], 2);
        }
        db_file(&parts, self.sample_rel())
    }

    pub fn claiming(rel: &str) -> Option<DatabaseOwner> {
        Self::ALL.into_iter().find(|owner| owner.allows(rel))
    }

    pub fn root(
        self,
        workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
    ) -> PathBuf {
        if self.is_host() {
            workspace.base_root().to_path_buf()
        } else {
            workspace.scope_root(principal, workspace_name)
        }
    }
}

fn prefix(parts: &[&str], head: &[&str], min_len: usize) -> bool {
    parts.len() >= min_len && parts.get(..head.len()) == Some(head)
}

pub(crate) fn is_live_engine_sidecar(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with(".wal")
}

fn db_file(parts: &[&str], sample: &str) -> bool {
    let sample_parts: Vec<&str> = sample.split('/').filter(|part| !part.is_empty()).collect();
    if parts == sample_parts {
        return true;
    }
    if parts.len() != sample_parts.len() || parts.is_empty() {
        return false;
    }
    if parts[..parts.len() - 1] != sample_parts[..sample_parts.len() - 1] {
        return false;
    }
    let last = parts[parts.len() - 1];
    let name = sample_parts[sample_parts.len() - 1];
    last == format!("{name}-wal") || last == format!("{name}-shm") || last == format!("{name}.wal")
}

pub fn walk_files(scope_root: &Path, owner: DatabaseOwner) -> Vec<String> {
    let mut out = Vec::new();
    let _ = collect(scope_root, scope_root, owner, &mut out);
    out.sort();
    out.dedup();
    out.into_iter().filter(|rel| owner.allows(rel)).collect()
}

fn collect(
    scope_root: &Path,
    dir: &Path,
    owner: DatabaseOwner,
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
