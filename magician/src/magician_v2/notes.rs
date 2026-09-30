use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::magician_v2::artifact_v2::{
    models::{
        ExecutionIndexEntry, ExecutionScheduleRecord, ExecutionScheduleStep, ExecutionState,
        OutputRef, TaskRecord,
    },
    workspace::ArtifactV2Workspace,
    ArtifactV2Error,
};
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

// The notes projection seam (plan 2.3) owns the product logic behind
// selection capture and task pages; this provider module keeps the registry,
// settings, and write orchestration, and re-imports the moved helpers under
// the same names this file — and its tests — already used.
use super::notes_projection::{
    already_captured_note_ref, bound_timeline_executions, bound_timeline_steps,
    capture_append_plan, capture_marker, default_daily_page_path, markdown_code_block,
    output_media_is_embeddable, previous_daily_page_path, render_task_note_markdown,
    task_note_asset_file_name, task_note_date, task_note_path, task_note_sort_instant,
    task_note_tags, TaskNoteTimelineEntry, TASK_NOTE_SCHEMA,
};
// The audio-notes UX seam (plan 3.5): the media-adjacent archive policy
// (dated-page layout, companion-page contract, listing order, receipt
// projection) moved out of this provider under the same names.
use super::audio_notes_seam::{
    audio_note_layout, audio_note_newest_first, audio_note_ref_from_index,
    render_audio_note_markdown,
};

const NOTES_DIR: &str = "notes";
const NOTES_SETTINGS_FILE: &str = "settings.json";
const AUDIO_NOTE_INDEX_DIR: &str = "audio-index";
const TASK_NOTE_INDEX_DIR: &str = "task-note-index";
const DEFAULT_PROVIDER_LOCAL: &str = "local_markdown";
const DEFAULT_PROVIDER_SILVERBULLET: &str = "silverbullet";
/// Per-workspace SilverBullet Spaces live under `{forest}/spaces/{principal}/{workspace}`.
const SILVERBULLET_SPACES_DIR: &str = "spaces";
const MAX_TASK_NOTE_EMBED_BYTES: u64 = 128 * 1024;
const MAX_TASK_NOTE_ASSET_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TASK_NOTE_ASSET_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_OBSERVATION_NOTE_BYTES: u64 = 256 * 1024;
const MAX_OBSERVATION_NOTE_CHARS: usize = 32 * 1024;
const MAX_OBSERVATION_SCAN_ENTRIES: usize = 20_000;
/// Search is bounded on every axis that grows with the space: how many terms a
/// query may carry, and how much of a matching line is returned.
const MAX_NOTE_SEARCH_TERMS: usize = 12;
const MAX_NOTE_SEARCH_LIMIT: usize = 50;
const DEFAULT_NOTE_SEARCH_LIMIT: usize = 10;
const MAX_NOTE_SEARCH_MATCHES_PER_NOTE: usize = 3;
const MAX_NOTE_SEARCH_SNIPPET_CHARS: usize = 240;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotesSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_provider")]
    pub default_provider: String,
    #[serde(default = "default_fallback_provider")]
    pub fallback_provider: String,
    #[serde(default)]
    pub local_markdown: LocalMarkdownNotesSettings,
    #[serde(default)]
    pub silverbullet: SilverBulletNotesSettings,
    #[serde(default)]
    pub task_publishing: TaskPublishingSettings,
}

impl Default for NotesSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_provider: default_provider(),
            fallback_provider: default_fallback_provider(),
            local_markdown: LocalMarkdownNotesSettings::default(),
            silverbullet: SilverBulletNotesSettings::default(),
            task_publishing: TaskPublishingSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskNotePublishMode {
    Compact,
    #[default]
    Standard,
    Diagnostic,
}

impl TaskNotePublishMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Standard => "standard",
            Self::Diagnostic => "diagnostic",
        }
    }

    fn asset_limit(self) -> usize {
        match self {
            Self::Compact => 4,
            Self::Standard => 12,
            Self::Diagnostic => 32,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskPublishingSettings {
    #[serde(default)]
    pub auto_publish_completed: bool,
    #[serde(default)]
    pub default_mode: TaskNotePublishMode,
    #[serde(default = "default_true")]
    pub include_assets: bool,
}

impl Default for TaskPublishingSettings {
    fn default() -> Self {
        Self {
            auto_publish_completed: false,
            default_mode: TaskNotePublishMode::Standard,
            include_assets: true,
        }
    }
}

struct NotesProviderRegistry {
    providers: Vec<Box<dyn NotesProvider>>,
}

fn notes_provider_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl NotesProviderRegistry {
    fn from_envelope(envelope: &NotesSettingsEnvelope) -> Self {
        let silverbullet_active = silverbullet_space_is_active(&envelope.settings)
            && envelope.resolved.silverbullet_write_safe;
        Self {
            providers: vec![
                Box::new(FilesystemNotesProvider::new(
                    DEFAULT_PROVIDER_LOCAL,
                    "Local Markdown",
                    PathBuf::from(&envelope.resolved.local_markdown_root),
                    true,
                    true,
                )),
                Box::new(
                    FilesystemNotesProvider::new(
                        DEFAULT_PROVIDER_SILVERBULLET,
                        "SilverBullet Space",
                        PathBuf::from(&envelope.resolved.silverbullet_space_path),
                        silverbullet_active,
                        silverbullet_active,
                    )
                    .served_from(&envelope.resolved.silverbullet_server_url),
                ),
            ],
        }
    }

    /// Providers in the order a lookup should try them: the configured default
    /// first, so a path that exists in both resolves to the space the owner
    /// actually works in rather than whichever was constructed first.
    fn ordered_for_read(&self, default_provider: &str) -> Vec<&dyn NotesProvider> {
        let mut ordered: Vec<&dyn NotesProvider> =
            self.providers.iter().map(|provider| &**provider).collect();
        ordered.sort_by_key(|provider| provider.id() != default_provider);
        ordered
    }

    async fn provider_statuses(&self) -> Vec<NotesProviderStatus> {
        let mut statuses = Vec::with_capacity(self.providers.len());
        for provider in &self.providers {
            let mut status = provider.status().await;
            status.health = provider.health().await;
            statuses.push(status);
        }
        statuses
    }

    async fn select_for_write<'a>(
        &'a self,
        requested: Option<&str>,
        settings: &NotesSettings,
    ) -> std::io::Result<SelectedNotesProvider<'a>> {
        if !settings.enabled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "notes provider is disabled",
            ));
        }

        let requested_provider = requested_provider_id(requested, settings);
        let fallback_provider = normalize_provider_id(&settings.fallback_provider)
            .filter(|provider| self.provider(provider).is_some())
            .unwrap_or_else(default_fallback_provider);

        let fallback_reason = if let Some(provider) = self.provider(&requested_provider) {
            if let Some(reason) = provider.unavailable_write_reason().await {
                reason
            } else {
                return Ok(SelectedNotesProvider {
                    requested_provider,
                    provider,
                    used_fallback: false,
                    fallback_reason: None,
                });
            }
        } else {
            format!("notes provider `{}` is unknown", requested_provider)
        };

        let Some(fallback) = self.provider(&fallback_provider) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("fallback notes provider `{fallback_provider}` is unavailable"),
            ));
        };
        if fallback.id() == requested_provider {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                fallback_reason,
            ));
        }
        if let Some(fallback_unavailable) = fallback.unavailable_write_reason().await {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{}; fallback `{}` is also unavailable: {}",
                    fallback_reason,
                    fallback.id(),
                    fallback_unavailable
                ),
            ));
        }

        Ok(SelectedNotesProvider {
            requested_provider,
            provider: fallback,
            used_fallback: true,
            fallback_reason: Some(fallback_reason),
        })
    }

    fn provider(&self, id: &str) -> Option<&dyn NotesProvider> {
        self.providers
            .iter()
            .find(|provider| provider.id() == id)
            .map(|provider| provider.as_ref())
    }
}

struct SelectedNotesProvider<'a> {
    requested_provider: String,
    provider: &'a dyn NotesProvider,
    used_fallback: bool,
    fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct ProviderWriteRef {
    relative_path: PathBuf,
    absolute_path: PathBuf,
}

#[async_trait]
trait NotesProvider: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn root(&self) -> &Path;
    fn configured(&self) -> bool;
    fn create_root_on_write(&self) -> bool;

    async fn status(&self) -> NotesProviderStatus {
        status_for_provider(
            self.id(),
            self.label(),
            self.root(),
            self.configured(),
            self.create_root_on_write(),
        )
        .await
    }

    /// Extra liveness a provider needs beyond its directory. `None` by default:
    /// a provider is assumed self-contained unless it says otherwise.
    async fn health(&self) -> Option<NotesProviderHealth> {
        None
    }

    async fn unavailable_write_reason(&self) -> Option<String> {
        if !self.configured() {
            return Some(format!("notes provider `{}` is not configured", self.id()));
        }
        match fs::metadata(self.root()).await {
            Ok(metadata) if metadata.is_dir() => {
                if writable_probe(self.root()).await {
                    None
                } else {
                    Some(format!(
                        "notes provider `{}` root is not writable",
                        self.id()
                    ))
                }
            },
            Ok(_) => Some(format!(
                "notes provider `{}` root is not a directory",
                self.id()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.create_root_on_write() {
                    None
                } else {
                    Some(format!(
                        "notes provider `{}` root does not exist",
                        self.id()
                    ))
                }
            },
            Err(error) => Some(format!(
                "notes provider `{}` root is unavailable: {}",
                self.id(),
                error
            )),
        }
    }

    async fn create_note(&self, request: &CreateNoteRequest) -> std::io::Result<ProviderWriteRef>;

    async fn append_note(&self, request: &AppendNoteRequest) -> std::io::Result<ProviderWriteRef>;

    async fn write_note_markdown(
        &self,
        request: &WriteNoteMarkdownRequest,
    ) -> std::io::Result<ProviderWriteRef>;

    async fn write_asset(
        &self,
        request: &WriteNoteAssetRequest,
    ) -> std::io::Result<ProviderWriteRef>;
}

#[derive(Debug, Clone)]
struct FilesystemNotesProvider {
    id: String,
    label: String,
    root: PathBuf,
    configured: bool,
    create_root_on_write: bool,
    /// Set only for a provider whose pages are served by something. Its
    /// presence is what makes a health probe applicable.
    server_url: Option<String>,
}

impl FilesystemNotesProvider {
    fn new(
        id: &str,
        label: &str,
        root: PathBuf,
        configured: bool,
        create_root_on_write: bool,
    ) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            root,
            configured,
            create_root_on_write,
            server_url: None,
        }
    }

    fn served_from(mut self, server_url: impl Into<String>) -> Self {
        let server_url = server_url.into();
        let trimmed = server_url.trim();
        if !trimmed.is_empty() {
            self.server_url = Some(trimmed.to_string());
        }
        self
    }
}

/// Atomically replace one provider-owned file without exposing a truncated
/// destination to readers or to an idempotent retry. The temporary file lives
/// beside the destination so the final rename stays on one filesystem.
async fn write_provider_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "notes destination has no parent directory",
        )
    })?;
    fs::create_dir_all(parent).await?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("note");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let result = async {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, path).await?;
        if let Ok(directory) = fs::File::open(parent).await {
            let _ = directory.sync_all().await;
        }
        Ok::<(), std::io::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

#[async_trait]
impl NotesProvider for FilesystemNotesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn configured(&self) -> bool {
        self.configured
    }

    fn create_root_on_write(&self) -> bool {
        self.create_root_on_write
    }

    async fn create_note(&self, request: &CreateNoteRequest) -> std::io::Result<ProviderWriteRef> {
        ensure_standard_note_dirs(&self.root).await?;
        let target_dir = request
            .target_dir
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Inbox");
        let slug = slugify(&request.title);
        let file_name = if slug.is_empty() {
            // Untitled notes follow SilverBullet's default Quick Note layout —
            // `Inbox/<YYYY-MM-DD>/<HH-MM-SS>.md` in local time — so notes created by
            // magician and notes created directly in the SB UI land in the same place.
            let now = chrono::Local::now();
            format!("{}/{}.md", now.format("%Y-%m-%d"), now.format("%H-%M-%S"))
        } else {
            format!("{slug}.md")
        };
        let rel_path = safe_relative_path(target_dir)?.join(file_name);
        let (rel_path, abs_path) = unique_note_path(&self.root, &rel_path).await?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let markdown = render_note_markdown(&request.title, &request.body);
        write_provider_file_atomic(&abs_path, markdown.as_bytes()).await?;
        Ok(ProviderWriteRef {
            relative_path: rel_path,
            absolute_path: abs_path,
        })
    }

    async fn health(&self) -> Option<NotesProviderHealth> {
        // Only probe a provider the owner actually set up. The resolved server
        // URL always has a default, so without this an owner who has never
        // configured SilverBullet would pay a two-second timeout on every
        // settings load and be shown "not running" for a provider they do not
        // use — a fault report about nothing.
        if !self.configured() {
            return None;
        }
        let server_url = self.server_url.as_deref()?;
        let (server_reachable, server_message) = probe_notes_server(server_url).await;
        let (cli_available, cli_message) = probe_silverbullet_cli();
        Some(NotesProviderHealth {
            server_url: server_url.to_string(),
            server_reachable,
            server_message,
            cli_available,
            cli_message,
        })
    }

    async fn append_note(&self, request: &AppendNoteRequest) -> std::io::Result<ProviderWriteRef> {
        ensure_standard_note_dirs(&self.root).await?;
        let rel_path = match request
            .target_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(path) => safe_relative_path(path)?,
            None => default_daily_page_path(),
        };
        let abs_path = checked_join(&self.root, &rel_path)?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let title = request
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let content = render_append_markdown(title, &request.body);
        let existing = fs::read_to_string(&abs_path).await.unwrap_or_default();
        let next = if existing.trim().is_empty() {
            content
        } else {
            format!("{}\n{}", existing.trim_end(), content)
        };
        write_provider_file_atomic(&abs_path, next.as_bytes()).await?;
        Ok(ProviderWriteRef {
            relative_path: rel_path,
            absolute_path: abs_path,
        })
    }

    async fn write_note_markdown(
        &self,
        request: &WriteNoteMarkdownRequest,
    ) -> std::io::Result<ProviderWriteRef> {
        ensure_standard_note_dirs(&self.root).await?;
        let rel_path = safe_relative_path(&request.target_path)?;
        let abs_path = checked_join(&self.root, &rel_path)?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        write_provider_file_atomic(&abs_path, request.markdown.as_bytes()).await?;
        Ok(ProviderWriteRef {
            relative_path: rel_path,
            absolute_path: abs_path,
        })
    }

    async fn write_asset(
        &self,
        request: &WriteNoteAssetRequest,
    ) -> std::io::Result<ProviderWriteRef> {
        ensure_standard_note_dirs(&self.root).await?;
        let rel_path =
            safe_relative_path(&request.target_dir)?.join(safe_file_name(&request.file_name)?);
        let abs_path = checked_join(&self.root, &rel_path)?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        write_provider_file_atomic(&abs_path, &request.bytes).await?;
        Ok(ProviderWriteRef {
            relative_path: rel_path,
            absolute_path: abs_path,
        })
    }
}

impl NotesSettings {
    pub fn normalize(mut self) -> Self {
        self.default_provider =
            normalize_provider_id(&self.default_provider).unwrap_or_else(default_provider);
        self.fallback_provider = normalize_provider_id(&self.fallback_provider)
            .unwrap_or_else(default_fallback_provider);
        self.local_markdown.root = normalize_optional_path(self.local_markdown.root);
        self.silverbullet.space_path = normalize_optional_path(self.silverbullet.space_path);
        self.silverbullet.local_url = normalize_url_or_default(
            self.silverbullet.local_url,
            default_silverbullet_local_url(),
        );
        self.silverbullet.server_url = normalize_browser_origin_or_default(
            self.silverbullet.server_url,
            default_silverbullet_server_url(),
        );
        self.silverbullet.public_origin = normalize_public_origin(self.silverbullet.public_origin);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LocalMarkdownNotesSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SilverBulletNotesSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_path: Option<String>,
    /// Loopback URL used only for sidecar lifecycle and health checks.
    ///
    /// This must stay separate from the browser-facing origin: a public
    /// Cloudflare hostname must never make the launcher bind SilverBullet to a
    /// public interface or use a remote Access round-trip as local health.
    #[serde(default = "default_silverbullet_local_url")]
    pub local_url: String,
    /// Legacy browser URL retained for backwards-compatible settings files.
    /// New deployments should set `public_origin`; when present it wins for
    /// generated note links and user-facing "open notes" actions.
    #[serde(default = "default_silverbullet_server_url")]
    pub server_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_origin: Option<String>,
}

impl Default for SilverBulletNotesSettings {
    fn default() -> Self {
        Self {
            space_path: None,
            local_url: default_silverbullet_local_url(),
            server_url: default_silverbullet_server_url(),
            public_origin: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NotesSettingsEnvelope {
    pub principal: String,
    pub workspace: String,
    pub settings_path: String,
    pub settings: NotesSettings,
    pub resolved: ResolvedNotesProviderSettings,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedNotesProviderSettings {
    pub default_provider: String,
    pub fallback_provider: String,
    pub local_markdown_root: String,
    pub silverbullet_space_path: String,
    /// False for legacy settings that would make the visible Space contain the
    /// canonical runtime root. Such settings remain inspectable but writes
    /// fall back until a dedicated notes-only subtree is selected.
    pub silverbullet_write_safe: bool,
    pub silverbullet_local_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silverbullet_public_origin: Option<String>,
    /// Effective browser-facing URL. Kept for older web clients.
    pub silverbullet_server_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotesProviderStatusResponse {
    pub principal: String,
    pub workspace: String,
    pub enabled: bool,
    pub active_provider: String,
    pub fallback_provider: String,
    pub providers: Vec<NotesProviderStatus>,
    pub warnings: Vec<String>,
}

/// Liveness of the pieces a provider needs beyond its own directory.
///
/// A SilverBullet space is a folder, so the filesystem checks above pass
/// whether or not the SilverBullet server is running — a space can be perfectly
/// writable while the owner cannot open a single page. `open_url` values point
/// at that server, so reporting only the directory made "ready" mean less than
/// it appeared to.
#[derive(Debug, Clone, Serialize)]
pub struct NotesProviderHealth {
    pub server_url: String,
    pub server_reachable: bool,
    pub server_message: String,
    /// The `sb` CLI, used by the optional Phase 7 enhancements. Absent is not a
    /// fault: nothing in the write path needs it.
    pub cli_available: bool,
    pub cli_message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotesProviderStatus {
    pub id: String,
    pub label: String,
    pub configured: bool,
    pub available: bool,
    pub writable: bool,
    pub root: String,
    pub message: String,
    /// Present only for a provider that depends on something beyond its own
    /// directory. `None` on Local Markdown, where the directory is the whole
    /// story.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<NotesProviderHealth>,
}

/// Stable description of the provider roots admitted to the Notes observation
/// target. The revision changes only when the configured roots change, never
/// when an individual note is edited, so a healthy subscription does not turn
/// stale on every write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotesObservationCatalog {
    pub enabled: bool,
    pub source_revision: String,
    pub providers: Vec<String>,
}

/// What to look for, and how much to bring back.
#[derive(Debug, Clone, Deserialize)]
pub struct NoteSearchRequest {
    pub query: String,
    /// Most notes to return. Clamped; absent means a screenful.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Restrict to one provider. Absent searches every configured root, which is
    /// what an owner means by "my notes" when a fallback has been in use.
    #[serde(default)]
    pub provider: Option<String>,
}

/// One line that matched, with where it sits in the note.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoteSearchMatch {
    /// 1-based, so it can be quoted to the owner as a line number.
    pub line: usize,
    pub text: String,
}

/// A note that matched, carrying enough to decide without opening it.
#[derive(Debug, Clone, Serialize)]
pub struct NoteSearchHit {
    pub provider: String,
    pub relative_path: String,
    pub source_ref: String,
    pub title: String,
    pub modified_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    /// A title hit ranks above a body hit: naming a note is a stronger statement
    /// about what it is than mentioning the words inside it.
    pub matched_in_title: bool,
    /// Total matching lines, which may exceed the returned `matches`.
    pub match_count: usize,
    pub matches: Vec<NoteSearchMatch>,
}

/// The result of a search, including what it could not reach.
#[derive(Debug, Clone, Serialize)]
pub struct NoteSearchResults {
    pub hits: Vec<NoteSearchHit>,
    pub scanned_notes: usize,
    /// True when the scan bound stopped traversal before the space ended, so the
    /// absence of a result does not mean the note is not there. Reported rather
    /// than hidden: a silent partial search is indistinguishable from a complete
    /// one that found nothing.
    pub scan_truncated: bool,
    pub query_terms: Vec<String>,
    /// True when more notes matched than `limit` allowed back.
    pub more_available: bool,
}

/// Private Markdown projected by the Notes provider boundary for the Observe
/// runtime. The runtime owns conversion into its common candidate envelope;
/// raw filesystem paths never cross that API boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedMarkdownNote {
    pub provider: String,
    pub relative_path: String,
    pub source_ref: String,
    pub title: String,
    pub markdown: String,
    pub modified_at_ms: i64,
    pub content_hash: String,
    pub open_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedMarkdownNoteBatch {
    pub items: Vec<ObservedMarkdownNote>,
    pub scanned_entries: usize,
    pub response_bytes: u64,
    pub next_offset: Option<usize>,
}

#[derive(Debug, Clone)]
struct NotesObservationRoot {
    provider: String,
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoteTreeEntry {
    pub name: String,
    pub relative_path: String,
    pub kind: String,
    /// A folder with no notes and no subfolders cannot be expanded.
    #[serde(default)]
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoteTree {
    pub provider: String,
    pub path: String,
    pub entries: Vec<NoteTreeEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoteFile {
    pub provider: String,
    pub relative_path: String,
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoteBacklink {
    pub relative_path: String,
    pub title: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NoteBacklinks {
    pub provider: String,
    pub path: String,
    pub backlinks: Vec<NoteBacklink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateNoteRequest {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub target_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendNoteRequest {
    #[serde(default)]
    pub title: Option<String>,
    pub body: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub target_path: Option<String>,
}

/// Where a note that already exists lives, and how to open it.
///
/// Separate from [`NoteRef`], which describes a write that just happened and
/// therefore knows its provider by construction. A lookup has to search, and
/// may find the note under a provider that is not the configured default.
/// A piece of text the owner selected somewhere, with where it came from.
///
/// Source metadata is optional throughout: a selection from a native app may
/// carry no URL, and one from a scratch buffer no title. A capture that loses
/// its provenance is still worth keeping — refusing it would discard the thing
/// the owner asked to save over a field they never had.
#[derive(Debug, Clone, Deserialize)]
pub struct CaptureSelectionRequest {
    pub text: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_title: Option<String>,
    #[serde(default)]
    pub source_app: Option<String>,
    /// Where to file it. Omitted means the dated Inbox page, which is the point
    /// of a capture: somewhere reliable without asking the owner to decide.
    #[serde(default)]
    pub target_path: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    /// Stable id for one capture, so a surface that retries after a failed
    /// response does not file the same selection twice. Mirrors how audio notes
    /// make outbox retries idempotent. A genuine second capture of the same text
    /// carries a new id and is filed again, because repeating a deliberate act
    /// is not a duplicate.
    #[serde(default)]
    pub capture_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteLocation {
    pub provider: String,
    pub path: String,
    pub absolute_path: String,
    pub open_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteRef {
    pub requested_provider: String,
    pub provider: String,
    pub used_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub path: String,
    pub absolute_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WriteNoteAssetRequest {
    pub provider: Option<String>,
    pub target_dir: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct WriteNoteMarkdownRequest {
    pub provider: Option<String>,
    pub target_path: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteAssetRef {
    pub requested_provider: String,
    pub provider: String,
    pub used_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub path: String,
    pub absolute_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    pub bytes: usize,
}

/// One durable voice recording plus its human-readable companion page.
///
/// The caller may explicitly request SilverBullet. Provider selection happens
/// once for both files, so an availability transition cannot split the audio
/// and Markdown page across two note roots.
#[derive(Debug, Clone)]
pub struct SaveAudioNoteRequest {
    pub provider: Option<String>,
    pub note_id: Option<String>,
    pub captured_at: Option<String>,
    pub source_surface: String,
    pub transcript: Option<String>,
    pub original_filename: Option<String>,
    pub mime_type: String,
    pub duration_ms: Option<u64>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioNoteRef {
    pub note_id: String,
    pub requested_provider: String,
    pub provider: String,
    pub used_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub captured_at: String,
    pub note_path: String,
    #[serde(skip_serializing)]
    pub note_absolute_path: String,
    pub audio_path: String,
    #[serde(skip_serializing)]
    pub audio_absolute_path: String,
    #[serde(skip_serializing)]
    pub open_url: Option<String>,
    pub bytes: usize,
    pub content_hash: String,
}

/// Canonical, scoped discovery record for one audio note. Provider files stay
/// human-owned Markdown/audio; this small runtime index gives iOS and agents a
/// stable list/read surface even when the configured provider later changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioNoteIndexEntry {
    pub note_id: String,
    pub requested_provider: String,
    pub provider: String,
    pub used_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub captured_at: String,
    pub source_surface: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub mime_type: String,
    pub note_path: String,
    pub audio_path: String,
    pub bytes: usize,
    /// BLAKE3 of the original recording. Empty only for an index written by a
    /// pre-hash build; new writes always populate it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hash: String,
}

/// On-disk envelope for the public discovery record. The root snapshot keeps a
/// durable note reachable after provider settings change, but is never returned
/// by the Notes API or model-facing internal-data actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAudioNoteIndexEntry {
    #[serde(flatten)]
    note: AudioNoteIndexEntry,
    #[serde(default)]
    provider_root: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AudioNotePage {
    pub items: Vec<AudioNoteIndexEntry>,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone)]
pub struct AudioNoteRecordingRef {
    pub absolute_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PublishTaskNoteRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub mode: Option<TaskNotePublishMode>,
    #[serde(default)]
    pub include_assets: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskNoteAssetRef {
    pub source_output_id: String,
    pub source_relative_path: String,
    pub path: String,
    pub media_type: String,
    pub bytes: usize,
    pub content_hash: String,
}

/// Durable discovery/provenance record for one idempotent task projection.
/// The Markdown page remains user-owned; this scoped runtime entry is the
/// machine-readable link from a canonical task to its current published page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskNoteIndexEntry {
    pub projection_id: String,
    pub schema: String,
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub agent_id: String,
    pub thread_id: String,
    pub mode: TaskNotePublishMode,
    pub requested_provider: String,
    pub provider: String,
    pub used_fallback: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub task_created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_completed_at: Option<String>,
    pub source_updated_at: String,
    pub published_at: String,
    pub date: String,
    pub tags: Vec<String>,
    pub note_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
    #[serde(default)]
    pub assets: Vec<TaskNoteAssetRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTaskNoteIndexEntry {
    #[serde(flatten)]
    note: TaskNoteIndexEntry,
    #[serde(default)]
    provider_root: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskNotePage {
    pub items: Vec<TaskNoteIndexEntry>,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone)]
struct PreparedTaskNoteAsset {
    source_output_id: String,
    source_relative_path: String,
    file_name: String,
    media_type: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct NotesSettingsStore {
    workspace_layout: ArtifactV2Workspace,
}

impl NotesSettingsStore {
    pub fn new<P: AsRef<Path>>(storage_root: P) -> Self {
        Self::with_workspace_layout(ArtifactV2Workspace::new(storage_root))
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// Start one watcher per notes folder already on disk.
    ///
    /// A scope created later is armed the next time its notes are searched.
    pub fn spawn_notes_watches(&self) {
        let store = Self {
            workspace_layout: self.workspace_layout.clone(),
        };
        let run = async move {
            if let Err(error) = store.arm_installed_watches().await {
                tracing::warn!(error = %error, "notes watches did not start");
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(run);
        }
    }

    async fn arm_installed_watches(&self) -> std::io::Result<()> {
        for (principal, workspace) in self.workspace_layout.list_scopes() {
            if let Err(error) = self.arm_scope_watch(&principal, &workspace).await {
                tracing::warn!(error = %error, principal, workspace, "notes watch skipped a scope");
            }
        }
        Ok(())
    }

    async fn arm_scope_watch(&self, principal: &str, workspace: &str) -> std::io::Result<()> {
        let (envelope, roots) = self.observation_roots(principal, workspace).await?;
        if !envelope.settings.enabled || roots.is_empty() {
            return Ok(());
        }
        let index_dir = self
            .workspace_layout
            .scope_root(principal, workspace)
            .join(NOTES_DIR)
            .join("hybrid-index");
        let roots = roots
            .into_iter()
            .map(|root| super::notes_search_index::NotesIndexRoot {
                provider: root.provider,
                root: root.root,
            })
            .collect();
        if super::notes_hybrid::ensure_notes_watch(index_dir, roots) {
            tracing::info!(principal, workspace, "notes watch armed");
        }
        Ok(())
    }

    pub async fn load_envelope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<NotesSettingsEnvelope> {
        let path = self.settings_path(principal, workspace)?;
        let settings = match self.workspace_layout.read_path(&path).await {
            Ok(bytes) => serde_json::from_slice::<NotesSettings>(&bytes)
                .map(NotesSettings::normalize)
                .unwrap_or_default(),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                NotesSettings::default()
            },
            Err(error) => return Err(artifact_v2_error_to_io(error)),
        };
        Ok(self.envelope_for(principal, workspace, path, settings))
    }

    pub async fn save(
        &self,
        principal: &str,
        workspace: &str,
        settings: NotesSettings,
    ) -> std::io::Result<NotesSettingsEnvelope> {
        let path = self.settings_path(principal, workspace)?;
        validate_silverbullet_network_settings(&settings.silverbullet)?;
        validate_silverbullet_space_boundary(
            &settings.silverbullet,
            &self.workspace_layout,
            principal,
            workspace,
        )?;
        let mut settings = settings.normalize();
        if settings.silverbullet.space_path.is_some() {
            let resolved = path_to_string(&resolve_silverbullet_space_root(
                &settings.silverbullet,
                principal,
                workspace,
            ));
            if settings.silverbullet.space_path.as_deref() != Some(resolved.as_str()) {
                settings.silverbullet.space_path = Some(resolved);
            }
        }
        self.workspace_layout
            .write_json_atomic_path(&path, &settings)
            .await
            .map_err(artifact_v2_error_to_io)?;
        Ok(self.envelope_for(principal, workspace, path, settings))
    }

    pub async fn provider_status(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<NotesProviderStatusResponse> {
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let providers = registry.provider_statuses().await;
        Ok(NotesProviderStatusResponse {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            enabled: envelope.settings.enabled,
            active_provider: envelope.resolved.default_provider.clone(),
            fallback_provider: envelope.resolved.fallback_provider.clone(),
            providers,
            warnings: envelope.warnings,
        })
    }

    /// Resolve the exact provider set behind the first-class Notes observation
    /// target. Local Markdown is always part of the logical Notes space; a
    /// configured SilverBullet Space joins it only after passing the same
    /// notes-only boundary used for writes.
    pub async fn observation_catalog(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<NotesObservationCatalog> {
        let (envelope, roots) = self.observation_roots(principal, workspace).await?;
        let providers = roots
            .iter()
            .map(|root| root.provider.clone())
            .collect::<Vec<_>>();
        let revision_input = roots
            .iter()
            .map(|root| {
                (
                    root.provider.as_str(),
                    root.root.to_string_lossy().into_owned(),
                )
            })
            .collect::<Vec<_>>();
        let revision = blake3::hash(
            &serde_json::to_vec(&(
                "magician.notes-observation.v1",
                envelope.settings.enabled,
                revision_input,
                envelope.resolved.silverbullet_server_url,
            ))
            .unwrap_or_default(),
        )
        .to_hex()
        .to_string();
        Ok(NotesObservationCatalog {
            enabled: envelope.settings.enabled,
            source_revision: revision,
            providers,
        })
    }

    /// Discover the newest Markdown revisions across the scoped Notes roots.
    /// Traversal is iterative and symlinks are ignored, which keeps large or
    /// adversarial note trees bounded without recursion or path escape.
    pub async fn discover_observation_notes(
        &self,
        principal: &str,
        workspace: &str,
        offset: usize,
        limit: usize,
    ) -> std::io::Result<ObservedMarkdownNoteBatch> {
        if limit == 0 {
            return Ok(ObservedMarkdownNoteBatch {
                items: Vec::new(),
                scanned_entries: 0,
                response_bytes: 0,
                next_offset: None,
            });
        }
        let (envelope, roots) = self.observation_roots(principal, workspace).await?;
        if !envelope.settings.enabled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "notes provider is disabled",
            ));
        }
        let mut descriptors = Vec::new();
        let mut scanned_entries = 0usize;
        for root in &roots {
            scan_observation_root(root, &mut descriptors, &mut scanned_entries).await?;
            if scanned_entries >= MAX_OBSERVATION_SCAN_ENTRIES {
                break;
            }
        }
        descriptors.sort_by(|left, right| {
            right
                .modified_at_ms
                .cmp(&left.modified_at_ms)
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| left.relative_path.cmp(&right.relative_path))
        });
        let descriptor_count = descriptors.len();
        let descriptors = descriptors
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let next_offset = (offset.saturating_add(limit) < descriptor_count)
            .then_some(offset.saturating_add(limit));

        let mut items = Vec::with_capacity(descriptors.len());
        let mut response_bytes = 0u64;
        for descriptor in descriptors {
            if let Some((note, bytes)) = materialize_observation_note(&envelope, descriptor).await?
            {
                response_bytes = response_bytes.saturating_add(bytes);
                items.push(note);
            }
        }

        Ok(ObservedMarkdownNoteBatch {
            items,
            scanned_entries,
            response_bytes,
            next_offset,
        })
    }

    /// Find notes containing every term of a query.
    ///
    /// The files remain the source of truth. This read is dropped when the
    /// call returns. The query runs against the on-disk LanceDB table and the
    /// full-text index stored with it. Changed notes are merged into that
    /// table in the background. The walk is the same boundary-safe one Observe
    /// uses, so runtime directories that share a notes root stay out of the
    /// results.
    pub async fn search_notes(
        &self,
        principal: &str,
        workspace: &str,
        request: NoteSearchRequest,
    ) -> std::io::Result<NoteSearchResults> {
        #[cfg(not(test))]
        {
            let _ = self.arm_scope_watch(principal, workspace).await;
        }
        let terms = note_search_terms(&request.query);
        if terms.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "search requires at least one non-empty term",
            ));
        }
        let limit = request
            .limit
            .unwrap_or(DEFAULT_NOTE_SEARCH_LIMIT)
            .clamp(1, MAX_NOTE_SEARCH_LIMIT);

        let (envelope, roots) = self.observation_roots(principal, workspace).await?;
        if !envelope.settings.enabled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "notes provider is disabled",
            ));
        }
        let roots = match request.provider.as_deref().map(str::trim) {
            Some(provider) if !provider.is_empty() => {
                let available = roots
                    .iter()
                    .map(|root| root.provider.clone())
                    .collect::<Vec<_>>();
                let filtered = roots
                    .into_iter()
                    .filter(|root| root.provider == provider)
                    .collect::<Vec<_>>();
                // A misspelt or unconfigured provider would otherwise search
                // nothing and report no results, which reads as "you have not
                // written that down" — a different and wrong answer.
                if filtered.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "no notes provider named `{provider}` is configured for this scope; \
                             available: {}",
                            if available.is_empty() {
                                "none".to_string()
                            } else {
                                available.join(", ")
                            }
                        ),
                    ));
                }
                filtered
            },
            _ => roots,
        };

        let snapshot = super::notes_search_index::snapshot_for(
            roots
                .iter()
                .map(|root| super::notes_search_index::NotesIndexRoot {
                    provider: root.provider.clone(),
                    root: root.root.clone(),
                })
                .collect(),
        )
        .await?;
        let scanned_notes = snapshot.scanned_notes;
        let scan_truncated = snapshot.scan_truncated;
        let mut candidates = snapshot
            .notes
            .iter()
            .filter_map(|note| evaluate_indexed_note_fuzzy(note, &terms))
            .collect::<Vec<_>>();
        let index_dir = self
            .workspace_layout
            .scope_root(principal, workspace)
            .join("notes")
            .join("hybrid-index");
        let admissions = super::notes_hybrid::query_stored_index(&index_dir, &request.query, limit)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(error = %error, "notes index query failed");
                Vec::new()
            });
        super::notes_hybrid::schedule_index_refresh(index_dir, snapshot.notes.clone()).await;
        if !admissions.is_empty() {
            let mut seen = candidates
                .iter()
                .map(|candidate| {
                    format!(
                        "{}\n{}",
                        candidate.hit.provider, candidate.hit.relative_path
                    )
                })
                .collect::<std::collections::HashSet<_>>();
            for admission in admissions {
                let key = format!("{}\n{}", admission.provider, admission.relative_path);
                if !seen.insert(key) {
                    continue;
                }
                let Some(note) = snapshot.notes.iter().find(|note| {
                    note.provider == admission.provider
                        && note.relative_path == admission.relative_path
                }) else {
                    continue;
                };
                if let Some(candidate) = evaluate_indexed_note_fuzzy(note, &terms) {
                    candidates.push(candidate);
                } else if admission.semantic {
                    if let Some(candidate) = semantic_note_candidate(note) {
                        candidates.push(candidate);
                    }
                }
            }
        }

        // A title hit outranks a body hit, then weight of evidence, then
        // recency. Path last so equal notes come back in a stable order rather
        // than whichever read finished first.
        candidates.sort_by(|left, right| {
            right
                .hit
                .matched_in_title
                .cmp(&left.hit.matched_in_title)
                .then_with(|| right.hit.match_count.cmp(&left.hit.match_count))
                .then_with(|| right.hit.modified_at_ms.cmp(&left.hit.modified_at_ms))
                .then_with(|| left.hit.relative_path.cmp(&right.hit.relative_path))
        });
        let more_available = candidates.len() > limit;
        candidates.truncate(limit);

        // Open URLs only for what is actually returned — building them for
        // discarded hits would be work the owner never sees.
        let hits = candidates
            .into_iter()
            .map(|candidate| {
                let mut hit = candidate.hit;
                hit.open_url = provider_open_url(
                    &hit.provider,
                    &envelope,
                    &PathBuf::from(&hit.relative_path),
                    &candidate.absolute_path,
                );
                hit
            })
            .collect::<Vec<_>>();

        Ok(NoteSearchResults {
            hits,
            scanned_notes,
            scan_truncated,
            query_terms: terms.clone(),
            more_available,
        })
    }

    /// One level of the notes folder. `relative_path` empty is the root.
    /// Directories expand on demand; this does not walk the whole tree.
    pub async fn list_note_tree(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
    ) -> std::io::Result<NoteTree> {
        let (envelope, root) = self.primary_note_root(principal, workspace).await?;
        let relative = if relative_path.trim().is_empty() {
            PathBuf::new()
        } else {
            safe_relative_path(relative_path)?
        };
        let canonical_root = fs::canonicalize(&root.root).await?;
        let directory = if relative.as_os_str().is_empty() {
            canonical_root.clone()
        } else {
            checked_join(&canonical_root, &relative)?
        };
        let metadata = fs::symlink_metadata(&directory).await?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notes tree path must be a directory inside the notes root",
            ));
        }
        let mut entries = fs::read_dir(&directory).await?;
        let mut listed = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_symlink() || is_observation_internal_directory(&entry.file_name()) {
                continue;
            }
            let name = path_to_string(Path::new(&entry.file_name()));
            if name.is_empty() || name.chars().any(char::is_control) {
                continue;
            }
            let child_relative = if relative.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                relative.join(&name)
            };
            if file_type.is_dir() {
                let child_path = entry.path();
                listed.push(NoteTreeEntry {
                    name,
                    relative_path: path_to_string(&child_relative),
                    kind: "dir".to_string(),
                    has_children: directory_has_listable_child(&child_path).await,
                });
            } else if file_type.is_file() && is_markdown_note_path(&entry.path()) {
                listed.push(NoteTreeEntry {
                    name,
                    relative_path: path_to_string(&child_relative),
                    kind: "file".to_string(),
                    has_children: false,
                });
            }
        }
        listed.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        let _ = envelope;
        Ok(NoteTree {
            provider: root.provider,
            path: path_to_string(&relative),
            entries: listed,
        })
    }

    /// Read one Markdown note for the in-app reading pane.
    pub async fn read_note_file(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
    ) -> std::io::Result<NoteFile> {
        let (envelope, root) = self.primary_note_root(principal, workspace).await?;
        let relative = safe_relative_path(relative_path)?;
        if !is_markdown_note_path(&relative) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only Markdown notes can be opened",
            ));
        }
        let canonical_root = fs::canonicalize(&root.root).await?;
        let absolute_path = checked_join(&canonical_root, &relative)?;
        let metadata = fs::symlink_metadata(&absolute_path).await?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notes file must be a real file inside the notes root",
            ));
        }
        if metadata.len() > MAX_OBSERVATION_NOTE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "note is too large to open",
            ));
        }
        let canonical = fs::canonicalize(&absolute_path).await?;
        if !canonical.starts_with(&canonical_root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notes file escaped the notes root",
            ));
        }
        let markdown = String::from_utf8(fs::read(&canonical).await?).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "note is not UTF-8 text")
        })?;
        let _ = envelope;
        Ok(NoteFile {
            provider: root.provider,
            relative_path: path_to_string(&relative),
            title: markdown_note_title(&markdown, &relative),
            markdown,
        })
    }

    /// Write a new Markdown note inside a folder of the active notes root.
    pub async fn create_markdown_note(
        &self,
        principal: &str,
        workspace: &str,
        folder: &str,
        name: &str,
    ) -> std::io::Result<NoteFile> {
        let (_envelope, root) = self.primary_note_root(principal, workspace).await?;
        let folder = if folder.trim().is_empty() {
            PathBuf::new()
        } else {
            safe_relative_path(folder)?
        };
        let file_name = note_file_name(name)?;
        let relative = folder.join(&file_name);
        if !is_markdown_note_path(&relative) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only a Markdown note can be created",
            ));
        }
        let canonical_root = fs::canonicalize(&root.root).await?;
        let absolute = checked_join(&canonical_root, &relative)?;
        if fs::try_exists(&absolute).await? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a note with that name is already in this folder",
            ));
        }
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).await?;
        }
        let stem = Path::new(&file_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Note");
        let markdown = format!("# {stem}\n\n");
        write_provider_file_atomic(&absolute, markdown.as_bytes()).await?;
        self.reindex_primary_notes(principal, workspace).await?;
        self.read_note_file(principal, workspace, &path_to_string(&relative))
            .await
    }

    /// Replace one existing note and refresh the search index.
    pub async fn save_note_markdown(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
        markdown: &str,
    ) -> std::io::Result<NoteFile> {
        let (_envelope, root) = self.primary_note_root(principal, workspace).await?;
        let relative = safe_relative_path(relative_path)?;
        if !is_markdown_note_path(&relative) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only a Markdown note can be saved",
            ));
        }
        let canonical_root = fs::canonicalize(&root.root).await?;
        let absolute = checked_join(&canonical_root, &relative)?;
        let metadata = fs::symlink_metadata(&absolute).await?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notes save must name a real file inside the notes root",
            ));
        }
        write_provider_file_atomic(&absolute, markdown.as_bytes()).await?;
        self.reindex_primary_notes(principal, workspace).await?;
        self.read_note_file(principal, workspace, &path_to_string(&relative))
            .await
    }

    /// Create an empty folder inside the active notes root.
    pub async fn create_note_folder(
        &self,
        principal: &str,
        workspace: &str,
        folder: &str,
        name: &str,
    ) -> std::io::Result<String> {
        let (_envelope, root) = self.primary_note_root(principal, workspace).await?;
        let parent = if folder.trim().is_empty() {
            PathBuf::new()
        } else {
            safe_relative_path(folder)?
        };
        let relative = parent.join(folder_name(name)?);
        let canonical_root = fs::canonicalize(&root.root).await?;
        let absolute = checked_join(&canonical_root, &relative)?;
        if fs::try_exists(&absolute).await? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "that folder already exists",
            ));
        }
        fs::create_dir_all(&absolute).await?;
        Ok(path_to_string(&relative))
    }

    /// Delete one Markdown note and drop it from the search index.
    pub async fn delete_markdown_note(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
    ) -> std::io::Result<()> {
        let (_envelope, root) = self.primary_note_root(principal, workspace).await?;
        let relative = safe_relative_path(relative_path)?;
        if !is_markdown_note_path(&relative) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only a Markdown note can be deleted",
            ));
        }
        let canonical_root = fs::canonicalize(&root.root).await?;
        let absolute = checked_join(&canonical_root, &relative)?;
        let metadata = fs::symlink_metadata(&absolute).await?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notes delete must name a real file inside the notes root",
            ));
        }
        fs::remove_file(&absolute).await?;
        self.reindex_primary_notes(principal, workspace).await
    }

    /// Delete a folder of notes and drop those notes from the search index.
    pub async fn delete_note_folder(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
    ) -> std::io::Result<()> {
        let (_envelope, root) = self.primary_note_root(principal, workspace).await?;
        let relative = safe_relative_path(relative_path)?;
        if relative.as_os_str().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the notes root cannot be deleted",
            ));
        }
        let canonical_root = fs::canonicalize(&root.root).await?;
        let absolute = checked_join(&canonical_root, &relative)?;
        let metadata = fs::symlink_metadata(&absolute).await?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "notes folder delete must name a real folder inside the notes root",
            ));
        }
        refuse_symlink_inside(&absolute).await?;
        fs::remove_dir_all(&absolute).await?;
        self.reindex_primary_notes(principal, workspace).await
    }

    async fn reindex_primary_notes(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<()> {
        let (_, root) = self.primary_note_root(principal, workspace).await?;
        let snapshot = super::notes_search_index::snapshot_for(vec![
            super::notes_search_index::NotesIndexRoot {
                provider: root.provider.clone(),
                root: root.root.clone(),
            },
        ])
        .await?;
        let index_dir = self
            .workspace_layout
            .scope_root(principal, workspace)
            .join("notes")
            .join("hybrid-index");
        super::notes_hybrid::refresh_index(&index_dir, &snapshot.notes)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(())
    }

    /// Notes that link to `relative_path`, by a wiki link or a Markdown file link.
    pub async fn list_note_backlinks(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
    ) -> std::io::Result<NoteBacklinks> {
        let relative = safe_relative_path(relative_path)?;
        if !is_markdown_note_path(&relative) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only Markdown notes have backlinks",
            ));
        }
        let (_envelope, root) = self.primary_note_root(principal, workspace).await?;
        let snapshot = super::notes_search_index::snapshot_for(vec![
            super::notes_search_index::NotesIndexRoot {
                provider: root.provider.clone(),
                root: root.root.clone(),
            },
        ])
        .await?;
        let current = path_to_string(&relative);
        let pairs = snapshot
            .notes
            .iter()
            .map(|note| (note.relative_path.as_str(), note.markdown.as_str()))
            .collect::<Vec<_>>();
        let backlinks = super::notes_links::backlinks_to(&pairs, &current)
            .into_iter()
            .filter_map(|found| {
                let note = snapshot
                    .notes
                    .iter()
                    .find(|note| note.relative_path == found.relative_path)?;
                let relative = PathBuf::from(&note.relative_path);
                Some(NoteBacklink {
                    title: markdown_note_title(&note.markdown, &relative),
                    relative_path: found.relative_path,
                    line: found.line,
                    text: found.text,
                })
            })
            .collect::<Vec<_>>();
        Ok(NoteBacklinks {
            provider: root.provider,
            path: current,
            backlinks,
        })
    }

    async fn primary_note_root(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<(NotesSettingsEnvelope, NotesObservationRoot)> {
        let (envelope, roots) = self.observation_roots(principal, workspace).await?;
        if !envelope.settings.enabled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "notes provider is disabled",
            ));
        }
        let preferred = roots
            .iter()
            .find(|root| root.provider == envelope.settings.default_provider)
            .or_else(|| roots.first());
        let Some(root) = preferred else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "notes root is not configured",
            ));
        };
        Ok((envelope, root.clone()))
    }

    /// Resolve one previously emitted provider-relative Notes identity without
    /// scanning the whole Space. This powers live Worth a look detail/open
    /// actions while preserving the same scope and symlink boundary.
    pub async fn read_observation_note(
        &self,
        principal: &str,
        workspace: &str,
        source_ref: &str,
    ) -> std::io::Result<Option<ObservedMarkdownNote>> {
        let Some(rest) = source_ref.strip_prefix("notes:") else {
            return Ok(None);
        };
        let Some((provider, relative_path)) = rest.split_once(':') else {
            return Ok(None);
        };
        let relative = match safe_relative_path(relative_path) {
            Ok(path) if is_markdown_note_path(&path) => path,
            _ => return Ok(None),
        };
        let (envelope, roots) = self.observation_roots(principal, workspace).await?;
        if !envelope.settings.enabled {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "notes provider is disabled",
            ));
        }
        let Some(root) = roots.into_iter().find(|root| root.provider == provider) else {
            return Ok(None);
        };
        let canonical_root = match fs::canonicalize(&root.root).await {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let absolute_path = checked_join(&canonical_root, &relative)?;
        let metadata = match fs::symlink_metadata(&absolute_path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let modified_at_ms = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or_else(|| Utc::now().timestamp_millis())
            .max(1);
        let descriptor = ObservationNoteDescriptor {
            provider: provider.to_string(),
            canonical_root,
            absolute_path,
            relative_path: path_to_string(&relative),
            modified_at_ms,
        };
        Ok(materialize_observation_note(&envelope, descriptor)
            .await?
            .map(|(note, _)| note))
    }

    async fn observation_roots(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<(NotesSettingsEnvelope, Vec<NotesObservationRoot>)> {
        let envelope = self.load_envelope(principal, workspace).await?;
        let mut roots = vec![NotesObservationRoot {
            provider: DEFAULT_PROVIDER_LOCAL.to_string(),
            root: provider_root_snapshot(Path::new(&envelope.resolved.local_markdown_root))?,
        }];
        if silverbullet_space_is_active(&envelope.settings)
            && envelope.resolved.silverbullet_write_safe
        {
            roots.push(NotesObservationRoot {
                provider: DEFAULT_PROVIDER_SILVERBULLET.to_string(),
                root: provider_root_snapshot(Path::new(
                    &envelope.resolved.silverbullet_space_path,
                ))?,
            });
        }
        let mut claimed = BTreeSet::new();
        roots.retain(|root| claimed.insert(root.root.clone()));
        Ok((envelope, roots))
    }

    pub async fn create_note(
        &self,
        principal: &str,
        workspace: &str,
        request: CreateNoteRequest,
    ) -> std::io::Result<NoteRef> {
        let _guard = notes_provider_write_lock().lock().await;
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let selection = registry
            .select_for_write(request.provider.as_deref(), &envelope.settings)
            .await?;
        let write_ref = selection.provider.create_note(&request).await?;
        Ok(NoteRef {
            requested_provider: selection.requested_provider,
            provider: selection.provider.id().to_string(),
            used_fallback: selection.used_fallback,
            fallback_reason: selection.fallback_reason,
            path: path_to_string(&write_ref.relative_path),
            absolute_path: write_ref.absolute_path.display().to_string(),
            open_url: provider_open_url(
                selection.provider.id(),
                &envelope,
                &write_ref.relative_path,
                &write_ref.absolute_path,
            ),
        })
    }

    pub async fn append_note(
        &self,
        principal: &str,
        workspace: &str,
        request: AppendNoteRequest,
    ) -> std::io::Result<NoteRef> {
        let _guard = notes_provider_write_lock().lock().await;
        self.append_note_locked(principal, workspace, request).await
    }

    /// The body of `append_note`, for callers that already hold
    /// `notes_provider_write_lock`.
    ///
    /// `capture_selection` must hold the lock across its dedupe read and the
    /// append — a check outside the lock would let two concurrent sends of one
    /// `capture_id` both see "not captured" and both file — so the locked body
    /// is callable without re-acquiring a non-reentrant lock.
    async fn append_note_locked(
        &self,
        principal: &str,
        workspace: &str,
        request: AppendNoteRequest,
    ) -> std::io::Result<NoteRef> {
        debug_assert!(
            notes_provider_write_lock().try_lock().is_err(),
            "append_note_locked requires notes_provider_write_lock to be held"
        );
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let selection = registry
            .select_for_write(request.provider.as_deref(), &envelope.settings)
            .await?;
        let write_ref = selection.provider.append_note(&request).await?;
        Ok(NoteRef {
            requested_provider: selection.requested_provider,
            provider: selection.provider.id().to_string(),
            used_fallback: selection.used_fallback,
            fallback_reason: selection.fallback_reason,
            path: path_to_string(&write_ref.relative_path),
            absolute_path: write_ref.absolute_path.display().to_string(),
            open_url: provider_open_url(
                selection.provider.id(),
                &envelope,
                &write_ref.relative_path,
                &write_ref.absolute_path,
            ),
        })
    }

    /// File a selection the owner made somewhere into their notes.
    ///
    /// Built on `append_note` rather than beside it: a capture is an append with
    /// provenance, and a second write path would mean two things that must agree
    /// about daily-page naming, provider selection and fallback.
    ///
    /// Idempotent on `capture_id`. A surface that sends a capture, loses the
    /// response and retries would otherwise file the selection twice, and the
    /// owner could not tell that from a real second capture. The marker lives in
    /// the note itself, so the guarantee survives a restart with no separate
    /// store to keep in sync. A deliberate second capture carries a new id and is
    /// filed again — repeating an intentional act is not a duplicate.
    ///
    /// The dedupe read and the append happen under one hold of the process-wide
    /// write lock: a check outside the lock would let two concurrent sends of
    /// one `capture_id` interleave as "both not captured" and both append.
    /// When the request omits `target_path`, the dedupe also reads yesterday's
    /// default page: a capture sent at 23:59:58 and retried at 00:00:01
    /// resolves a different "today", and without that look-back the retry
    /// would miss the marker and duplicate. The look-back is dedupe-only — the
    /// append still lands on the current default page — and spans exactly one
    /// day back, not an arbitrary history.
    pub async fn capture_selection(
        &self,
        principal: &str,
        workspace: &str,
        request: CaptureSelectionRequest,
    ) -> std::io::Result<NoteRef> {
        // The decision half — validation, daily-page naming, rendering —
        // lives behind the notes projection seam (plan 2.3); this method
        // keeps only the orchestration around it.
        let plan = capture_append_plan(&request)?;
        let _guard = notes_provider_write_lock().lock().await;
        if let Some(capture_id) = plan.capture_id.as_deref() {
            // The page the append will land on, plus — only when the caller
            // named no page — yesterday's default, which is where an
            // original capture that crossed midnight wrote to. See the
            // method doc for the window.
            let mut dedupe_paths = vec![plan.target_path.clone()];
            if request.target_path.is_none() {
                dedupe_paths.push(path_to_string(&previous_daily_page_path()));
            }
            for dedupe_path in &dedupe_paths {
                if let Some(existing) = self
                    .already_captured(principal, workspace, dedupe_path, capture_id)
                    .await?
                {
                    // Nothing written. `requested` equals `provider` because no
                    // selection happened, and no fallback could have occurred.
                    return Ok(already_captured_note_ref(existing));
                }
            }
        }

        self.append_note_locked(
            principal,
            workspace,
            AppendNoteRequest {
                title: None,
                body: plan.body,
                provider: request.provider,
                target_path: Some(plan.target_path),
            },
        )
        .await
    }

    /// Whether this capture already landed on the target page, and where.
    ///
    /// A missing page is not an error: the first capture of the day creates it.
    /// A page that exists but cannot be stat'ed or read is: swallowing either
    /// error would read as "not captured" and file the same capture twice, so
    /// any stat or read failure other than the page being gone fails the
    /// capture loudly.
    async fn already_captured(
        &self,
        principal: &str,
        workspace: &str,
        target_path: &str,
        capture_id: &str,
    ) -> std::io::Result<Option<NoteLocation>> {
        // Look for the marker in every provider's copy of this path, not just
        // the first one that has the file. The write is fallback-aware and the
        // read was not: with the default provider holding a same-named page —
        // which the dated Inbox page usually is — a capture that fell back and
        // then retried would check the default's file, miss its own marker, and
        // file the passage a second time.
        let envelope = self.load_envelope(principal, workspace).await?;
        let rel_path = safe_relative_path(target_path)?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let marker = capture_marker(capture_id);
        for provider in registry.ordered_for_read(&envelope.resolved.default_provider) {
            if !provider.configured() {
                continue;
            }
            let abs_path = checked_join(provider.root(), &rel_path)?;
            match fs::metadata(&abs_path).await {
                // The page is not there (yet): the common missing-page case.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                // Any other stat failure fails the capture, mirroring the
                // read treatment directly below: an inaccessible path must
                // not read as "not captured" and file the passage twice.
                Err(error) => return Err(error),
                // Present but not a regular file (a directory named like
                // the page, say): there is no page body to search.
                Ok(meta) if !meta.is_file() => continue,
                Ok(_) => {},
            }
            let content = match fs::read_to_string(&abs_path).await {
                Ok(content) => content,
                // The page vanished between the metadata check and the read;
                // that is the missing-page case, not a failure to read it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !content.contains(&marker) {
                continue;
            }
            return Ok(Some(NoteLocation {
                provider: provider.id().to_string(),
                path: path_to_string(&rel_path),
                absolute_path: abs_path.display().to_string(),
                open_url: provider_open_url(provider.id(), &envelope, &rel_path, &abs_path),
            }));
        }
        Ok(None)
    }

    /// Locate an existing note and produce the URL that opens it.
    ///
    /// `open-root` opens the provider folder; this answers the narrower and more
    /// useful question of where one page is. The search is read-only and takes
    /// no write lock — it neither creates the note nor the directories around
    /// it, so asking about a note that does not exist leaves nothing behind.
    ///
    /// A path that resolves under no configured provider is `NotFound` rather
    /// than an empty success, so a caller cannot mistake "nowhere" for "here
    /// but unopenable".
    pub async fn resolve_note(
        &self,
        principal: &str,
        workspace: &str,
        relative_path: &str,
    ) -> std::io::Result<NoteLocation> {
        let envelope = self.load_envelope(principal, workspace).await?;
        let rel_path = safe_relative_path(relative_path)?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        for provider in registry.ordered_for_read(&envelope.resolved.default_provider) {
            if !provider.configured() {
                continue;
            }
            let abs_path = checked_join(provider.root(), &rel_path)?;
            if !matches!(fs::metadata(&abs_path).await, Ok(meta) if meta.is_file()) {
                continue;
            }
            return Ok(NoteLocation {
                provider: provider.id().to_string(),
                path: path_to_string(&rel_path),
                absolute_path: abs_path.display().to_string(),
                open_url: provider_open_url(provider.id(), &envelope, &rel_path, &abs_path),
            });
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no note at `{}` in any configured provider", relative_path),
        ))
    }

    pub async fn write_note_markdown(
        &self,
        principal: &str,
        workspace: &str,
        request: WriteNoteMarkdownRequest,
    ) -> std::io::Result<NoteRef> {
        let _guard = notes_provider_write_lock().lock().await;
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let selection = registry
            .select_for_write(request.provider.as_deref(), &envelope.settings)
            .await?;
        let write_ref = selection.provider.write_note_markdown(&request).await?;
        Ok(NoteRef {
            requested_provider: selection.requested_provider,
            provider: selection.provider.id().to_string(),
            used_fallback: selection.used_fallback,
            fallback_reason: selection.fallback_reason,
            path: path_to_string(&write_ref.relative_path),
            absolute_path: write_ref.absolute_path.display().to_string(),
            open_url: provider_open_url(
                selection.provider.id(),
                &envelope,
                &write_ref.relative_path,
                &write_ref.absolute_path,
            ),
        })
    }

    pub async fn write_asset(
        &self,
        principal: &str,
        workspace: &str,
        request: WriteNoteAssetRequest,
    ) -> std::io::Result<NoteAssetRef> {
        let _guard = notes_provider_write_lock().lock().await;
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let selection = registry
            .select_for_write(request.provider.as_deref(), &envelope.settings)
            .await?;
        let bytes = request.bytes.len();
        let write_ref = selection.provider.write_asset(&request).await?;
        Ok(NoteAssetRef {
            requested_provider: selection.requested_provider,
            provider: selection.provider.id().to_string(),
            used_fallback: selection.used_fallback,
            fallback_reason: selection.fallback_reason,
            path: path_to_string(&write_ref.relative_path),
            absolute_path: write_ref.absolute_path.display().to_string(),
            open_url: provider_open_url(
                selection.provider.id(),
                &envelope,
                &write_ref.relative_path,
                &write_ref.absolute_path,
            ),
            bytes,
        })
    }

    pub async fn publish_task_note(
        &self,
        principal: &str,
        workspace: &str,
        task: &TaskRecord,
        request: PublishTaskNoteRequest,
    ) -> std::io::Result<TaskNoteIndexEntry> {
        if task.manifest.principal != principal || task.manifest.workspace != workspace {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "task does not belong to the requested notes scope",
            ));
        }
        if !matches!(
            task.state.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "only completed, failed, or cancelled tasks can be published to notes",
            ));
        }
        let _guard = notes_provider_write_lock().lock().await;
        let envelope = self.load_envelope(principal, workspace).await?;
        let mode = request
            .mode
            .unwrap_or(envelope.settings.task_publishing.default_mode);
        let include_assets = request
            .include_assets
            .unwrap_or(envelope.settings.task_publishing.include_assets);
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let selection = registry
            .select_for_write(request.provider.as_deref(), &envelope.settings)
            .await?;
        let completed_at = self.task_completed_at(principal, workspace, task).await;
        let date = task_note_date(completed_at.as_deref(), &task.state.updated_at);
        let existing = self
            .read_task_note_record(principal, workspace, &task.manifest.task_id)
            .await?;
        let note_path = existing
            .as_ref()
            .map(|record| record.note.note_path.clone())
            .unwrap_or_else(|| task_note_path(task, &date));
        let note_parent = Path::new(&note_path)
            .parent()
            .map(path_to_string)
            .unwrap_or_else(|| "Tasks".to_string());
        let assets_dir = format!("{note_parent}/{}.assets", task.manifest.task_id);
        let (primary_body, intermediate_outputs, timeline, prepared_assets) = self
            .prepare_task_note_projection(principal, workspace, task, mode, include_assets)
            .await?;

        let mut asset_refs = Vec::with_capacity(prepared_assets.len());
        for asset in prepared_assets {
            let content_hash = format!("blake3:{}", blake3::hash(&asset.bytes).to_hex());
            let bytes = asset.bytes.len();
            let write = selection
                .provider
                .write_asset(&WriteNoteAssetRequest {
                    provider: Some(selection.provider.id().to_string()),
                    target_dir: assets_dir.clone(),
                    file_name: asset.file_name,
                    bytes: asset.bytes,
                })
                .await?;
            asset_refs.push(TaskNoteAssetRef {
                source_output_id: asset.source_output_id,
                source_relative_path: asset.source_relative_path,
                path: path_to_string(&write.relative_path),
                media_type: asset.media_type,
                bytes,
                content_hash,
            });
        }

        let published_at = Utc::now().to_rfc3339();
        let tags = task_note_tags(task, &date);
        let markdown = render_task_note_markdown(
            task,
            mode,
            &date,
            completed_at.as_deref(),
            &published_at,
            &tags,
            primary_body.as_deref(),
            &intermediate_outputs,
            &timeline,
            &asset_refs,
        );
        let note_write = selection
            .provider
            .write_note_markdown(&WriteNoteMarkdownRequest {
                provider: Some(selection.provider.id().to_string()),
                target_path: note_path,
                markdown,
            })
            .await?;
        let provider_root = provider_root_snapshot(selection.provider.root())?;
        let note = TaskNoteIndexEntry {
            projection_id: format!("task:{}", task.manifest.task_id),
            schema: TASK_NOTE_SCHEMA.to_string(),
            task_id: task.manifest.task_id.clone(),
            title: task.manifest.title.clone(),
            status: task.state.status.clone(),
            agent_id: task.manifest.agent_id.clone(),
            thread_id: task.manifest.ui_thread_id.clone(),
            mode,
            requested_provider: selection.requested_provider,
            provider: selection.provider.id().to_string(),
            used_fallback: selection.used_fallback,
            fallback_reason: selection.fallback_reason,
            task_created_at: task.manifest.created_at.clone(),
            task_due_date: task.manifest.due_date.clone(),
            task_completed_at: completed_at,
            source_updated_at: task.state.updated_at.clone(),
            published_at,
            date,
            tags,
            note_path: path_to_string(&note_write.relative_path),
            open_url: provider_open_url(
                selection.provider.id(),
                &envelope,
                &note_write.relative_path,
                &note_write.absolute_path,
            ),
            assets: asset_refs,
        };
        self.workspace_layout
            .write_json_atomic_path(
                self.task_note_index_path(principal, workspace, &task.manifest.task_id)?,
                &StoredTaskNoteIndexEntry {
                    note: note.clone(),
                    provider_root: provider_root.display().to_string(),
                },
            )
            .await
            .map_err(artifact_v2_error_to_io)?;
        Ok(note)
    }

    pub async fn auto_publish_completed_task(
        &self,
        principal: &str,
        workspace: &str,
        task: &TaskRecord,
    ) -> std::io::Result<Option<TaskNoteIndexEntry>> {
        if task.state.status != "completed"
            || task.state.synthesis_pending()
            || task.state.synthesis_failed_execution_id.is_some()
        {
            return Ok(None);
        }
        let envelope = self.load_envelope(principal, workspace).await?;
        if !envelope.settings.task_publishing.auto_publish_completed {
            return Ok(None);
        }
        self.publish_task_note(
            principal,
            workspace,
            task,
            PublishTaskNoteRequest::default(),
        )
        .await
        .map(Some)
    }

    pub async fn list_task_notes(
        &self,
        principal: &str,
        workspace: &str,
        offset: usize,
        limit: usize,
        query: Option<&str>,
    ) -> std::io::Result<TaskNotePage> {
        let directory = self.task_note_index_dir(principal, workspace)?;
        let entries = self
            .workspace_layout
            .read_dir_path_or_empty(&directory)
            .await
            .map_err(artifact_v2_error_to_io)?;
        let normalized_query = query
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);
        let mut notes = Vec::new();
        for entry in entries.into_iter().filter(|entry| entry.is_file) {
            if !entry.file_name.ends_with(".json") {
                continue;
            }
            let path = directory.join(&entry.file_name);
            let Ok(record) = self
                .workspace_layout
                .read_json_path::<StoredTaskNoteIndexEntry, _>(&path)
                .await
            else {
                continue;
            };
            let note = record.note;
            if let Some(query) = normalized_query.as_deref() {
                let haystack = format!(
                    "{} {} {} {} {} {}",
                    note.title,
                    note.task_id,
                    note.agent_id,
                    note.status,
                    note.date,
                    note.tags.join(" ")
                )
                .to_lowercase();
                if !haystack.contains(query) {
                    continue;
                }
            }
            notes.push(note);
        }
        notes.sort_by(|left, right| {
            task_note_sort_instant(right)
                .cmp(&task_note_sort_instant(left))
                .then_with(|| right.published_at.cmp(&left.published_at))
                .then_with(|| right.task_id.cmp(&left.task_id))
        });
        let total = notes.len();
        let limit = limit.clamp(1, 100);
        let items = notes
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        Ok(TaskNotePage {
            has_more: offset.saturating_add(items.len()) < total,
            items,
            offset,
            limit,
            total,
        })
    }

    pub async fn read_task_note(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> std::io::Result<Option<TaskNoteIndexEntry>> {
        Ok(self
            .read_task_note_record(principal, workspace, task_id)
            .await?
            .map(|record| record.note))
    }

    pub async fn read_task_note_markdown(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        max_bytes: usize,
    ) -> std::io::Result<Option<(TaskNoteIndexEntry, String)>> {
        let Some(record) = self
            .read_task_note_record(principal, workspace, task_id)
            .await?
        else {
            return Ok(None);
        };
        let root = provider_root_snapshot(Path::new(&record.provider_root))?;
        let path = checked_join(&root, &safe_relative_path(&record.note.note_path)?)?;
        let file = fs::File::open(&path).await?;
        let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
        file.take(max_bytes as u64).read_to_end(&mut bytes).await?;
        let body = String::from_utf8_lossy(&bytes).to_string();
        Ok(Some((record.note, body)))
    }

    pub async fn task_note_is_available(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> std::io::Result<bool> {
        let Some(record) = self
            .read_task_note_record(principal, workspace, task_id)
            .await?
        else {
            return Ok(false);
        };
        let root = match provider_root_snapshot(Path::new(&record.provider_root)) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let path = checked_join(&root, &safe_relative_path(&record.note.note_path)?)?;
        match fs::metadata(path).await {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn task_completed_at(
        &self,
        principal: &str,
        workspace: &str,
        task: &TaskRecord,
    ) -> Option<String> {
        let execution_id = task.state.last_completed_root_execution_id.as_deref()?;
        self.workspace_layout
            .read_json_path::<ExecutionState, _>(self.workspace_layout.execution_state_path(
                principal,
                workspace,
                &task.manifest.task_id,
                execution_id,
            ))
            .await
            .ok()
            .and_then(|state| state.completed_at)
    }

    async fn prepare_task_note_projection(
        &self,
        principal: &str,
        workspace: &str,
        task: &TaskRecord,
        mode: TaskNotePublishMode,
        include_assets: bool,
    ) -> std::io::Result<(
        Option<String>,
        Vec<(OutputRef, String)>,
        Vec<TaskNoteTimelineEntry>,
        Vec<PreparedTaskNoteAsset>,
    )> {
        let primary = task
            .refs
            .primary_user_output_id
            .as_ref()
            .and_then(|id| {
                task.refs
                    .outputs
                    .iter()
                    .find(|output| output.output_id == *id)
            })
            .or_else(|| {
                task.refs
                    .outputs
                    .iter()
                    .find(|output| output.audience == "user")
            });
        let primary_body = match primary {
            Some(output) => {
                self.read_embeddable_task_output(principal, workspace, task, output)
                    .await?
            },
            None => None,
        };
        let mut intermediate_outputs = Vec::new();
        if mode != TaskNotePublishMode::Compact {
            for output in &task.refs.outputs {
                if primary.is_some_and(|primary| primary.output_id == output.output_id)
                    || output.audience != "user"
                {
                    continue;
                }
                if let Some(body) = self
                    .read_embeddable_task_output(principal, workspace, task, output)
                    .await?
                {
                    intermediate_outputs.push((output.clone(), body));
                    if mode == TaskNotePublishMode::Standard && intermediate_outputs.len() >= 4 {
                        break;
                    }
                }
            }
        }
        let executions = if mode == TaskNotePublishMode::Compact {
            Vec::new()
        } else {
            // Was `read_jsonl_path(..).unwrap_or_default()`, which is the
            // quietest form of this bug: one unreadable line made the whole
            // read fail, and the `unwrap_or_default` turned that into "this
            // task had no executions" in a note somebody published and read.
            // Now the unreadable records are the only thing missing, and they
            // are counted.
            let index_path = self.workspace_layout.task_executions_index_path(
                principal,
                workspace,
                &task.manifest.task_id,
            );
            match self
                .workspace_layout
                .read_jsonl_path_tolerant::<ExecutionIndexEntry, _>(&index_path)
                .await
            {
                Ok(read) => {
                    if read.lost_committed_records() {
                        tracing::warn!(
                            target: "notes",
                            path = %index_path.display(),
                            corrupt_records = read.corrupt,
                            task_id = %task.manifest.task_id,
                            "published note omits unreadable execution-index records"
                        );
                    }
                    read.records
                },
                Err(error) => {
                    tracing::warn!(
                        target: "notes",
                        path = %index_path.display(),
                        %error,
                        "execution index unreadable; publishing the note without it"
                    );
                    Vec::new()
                },
            }
        };
        // Ordering and the per-mode bounds live behind the projection seam,
        // where the rendering that consumes them lives too.
        let executions = bound_timeline_executions(mode, executions);
        let mut timeline = Vec::with_capacity(executions.len());
        for execution in executions {
            let steps: Vec<ExecutionScheduleStep> = self
                .workspace_layout
                .read_json_path::<ExecutionScheduleRecord, _>(
                    self.workspace_layout.execution_schedule_path(
                        principal,
                        workspace,
                        &task.manifest.task_id,
                        &execution.execution_id,
                    ),
                )
                .await
                .map(|schedule| schedule.steps)
                .unwrap_or_default();
            let (steps, omitted_steps) = bound_timeline_steps(mode, steps);
            timeline.push(TaskNoteTimelineEntry {
                execution,
                steps,
                omitted_steps,
            });
        }

        let mut assets = Vec::new();
        let mut total_bytes = 0_u64;
        if include_assets {
            for output in &task.refs.outputs {
                if assets.len() >= mode.asset_limit() {
                    break;
                }
                let is_embeddable = output_media_is_embeddable(&output.media_type);
                if mode != TaskNotePublishMode::Diagnostic
                    && is_embeddable
                    && output.role != "user_media"
                {
                    continue;
                }
                let path = self
                    .workspace_layout
                    .task_dir(principal, workspace, &task.manifest.task_id)
                    .join(safe_relative_path(&output.relative_path)?);
                let Some(metadata) = self
                    .workspace_layout
                    .metadata_path(&path)
                    .await
                    .map_err(artifact_v2_error_to_io)?
                else {
                    continue;
                };
                if !metadata.is_file()
                    || metadata.len() > MAX_TASK_NOTE_ASSET_BYTES
                    || total_bytes.saturating_add(metadata.len()) > MAX_TASK_NOTE_ASSET_TOTAL_BYTES
                {
                    continue;
                }
                let bytes = self
                    .workspace_layout
                    .read_path(&path)
                    .await
                    .map_err(artifact_v2_error_to_io)?;
                total_bytes = total_bytes.saturating_add(bytes.len() as u64);
                assets.push(PreparedTaskNoteAsset {
                    source_output_id: output.output_id.clone(),
                    source_relative_path: output.relative_path.clone(),
                    file_name: task_note_asset_file_name(output),
                    media_type: output.media_type.clone(),
                    bytes,
                });
            }
        }
        if include_assets
            && mode == TaskNotePublishMode::Diagnostic
            && assets.len() < mode.asset_limit()
        {
            let bytes = serde_json::to_vec_pretty(task).map_err(std::io::Error::other)?;
            assets.push(PreparedTaskNoteAsset {
                source_output_id: "diagnostic:task_record".to_string(),
                source_relative_path: "task_record.json".to_string(),
                file_name: "task-record.json".to_string(),
                media_type: "application/json".to_string(),
                bytes,
            });
        }
        Ok((primary_body, intermediate_outputs, timeline, assets))
    }

    async fn read_embeddable_task_output(
        &self,
        principal: &str,
        workspace: &str,
        task: &TaskRecord,
        output: &OutputRef,
    ) -> std::io::Result<Option<String>> {
        if !output_media_is_embeddable(&output.media_type) {
            return Ok(None);
        }
        let path = self
            .workspace_layout
            .task_dir(principal, workspace, &task.manifest.task_id)
            .join(safe_relative_path(&output.relative_path)?);
        let bytes = match self
            .workspace_layout
            .read_prefix_path(&path, MAX_TASK_NOTE_EMBED_BYTES)
            .await
        {
            Ok(bytes) => bytes,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            },
            Err(error) => return Err(artifact_v2_error_to_io(error)),
        };
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        if text.is_empty() {
            return Ok(None);
        }
        let rendered = match output.media_type.split(';').next().unwrap_or("").trim() {
            "text/markdown" => text,
            "application/json" => markdown_code_block("json", &text),
            "text/html" => markdown_code_block("html", &text),
            "application/xml" | "text/xml" => markdown_code_block("xml", &text),
            _ => markdown_code_block("text", &text),
        };
        Ok(Some(rendered))
    }

    /// Save a voice recording and its transcript as a single Notes-provider
    /// operation. The audio is written first and removed again if its companion
    /// Markdown page cannot be created, avoiding a silent orphan recording.
    pub async fn save_audio_note(
        &self,
        principal: &str,
        workspace: &str,
        request: SaveAudioNoteRequest,
    ) -> std::io::Result<AudioNoteRef> {
        if request.bytes.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "audio note recording is empty",
            ));
        }

        let layout = audio_note_layout(&request)?;
        let bytes = request.bytes.len();
        let content_hash = format!("blake3:{}", blake3::hash(&request.bytes).to_hex());
        let _guard = notes_provider_write_lock().lock().await;

        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let existing_record = self
            .read_audio_note_record(principal, workspace, &layout.note_id)
            .await?;
        if let Some(mut stored) = existing_record {
            let existing = &mut stored.note;
            let same_capture_instant = DateTime::parse_from_rfc3339(&existing.captured_at)
                .is_ok_and(|captured_at| captured_at == layout.captured_at);
            if !same_capture_instant {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "audio note id is already bound to a different capture",
                ));
            }
            if existing.bytes != bytes
                || (!existing.content_hash.is_empty()
                    && existing.content_hash.as_str() != content_hash.as_str())
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "audio note id is already bound to a different recording",
                ));
            }
            let incoming_transcript = request
                .transcript
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if existing.source_surface != request.source_surface.trim()
                || existing.transcript.as_deref() != incoming_transcript
                || existing.duration_ms != request.duration_ms
                || existing.mime_type != request.mime_type.trim()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "audio note id is already bound to different metadata",
                ));
            }
            let provider = registry.provider(&existing.provider).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "audio note provider is no longer available",
                )
            })?;
            let provider_root = if stored.provider_root.trim().is_empty() {
                provider_root_snapshot(provider.root())?
            } else {
                provider_root_snapshot(Path::new(&stored.provider_root))?
            };
            let note_absolute_path =
                checked_join(&provider_root, &safe_relative_path(&existing.note_path)?)?;
            let audio_absolute_path =
                checked_join(&provider_root, &safe_relative_path(&existing.audio_path)?)?;
            let note_exists = fs::try_exists(&note_absolute_path).await?;
            let audio_exists = fs::try_exists(&audio_absolute_path).await?;
            if audio_exists {
                let durable_bytes = fs::read(&audio_absolute_path).await?;
                if blake3::hash(&durable_bytes) != blake3::hash(&request.bytes) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "audio note id is already bound to a different recording",
                    ));
                }
            }
            if note_exists && audio_exists {
                let needs_migration =
                    existing.content_hash.is_empty() || stored.provider_root.trim().is_empty();
                existing.content_hash = content_hash.clone();
                stored.provider_root = provider_root.display().to_string();
                if needs_migration {
                    self.workspace_layout
                        .write_json_atomic_path(
                            self.audio_note_index_path(principal, workspace, &layout.note_id)?,
                            &stored,
                        )
                        .await
                        .map_err(artifact_v2_error_to_io)?;
                }
                return Ok(audio_note_ref_from_index(
                    stored.note,
                    note_absolute_path,
                    audio_absolute_path,
                ));
            }

            let markdown = render_audio_note_markdown(&request, &layout);
            if !audio_exists {
                write_provider_file_atomic(&audio_absolute_path, &request.bytes).await?;
            }
            if !note_exists {
                if let Err(error) =
                    write_provider_file_atomic(&note_absolute_path, markdown.as_bytes()).await
                {
                    if !audio_exists {
                        let _ = fs::remove_file(&audio_absolute_path).await;
                    }
                    return Err(error);
                }
            }
            existing.content_hash = content_hash;
            stored.provider_root = provider_root.display().to_string();
            if let Err(error) = self
                .workspace_layout
                .write_json_atomic_path(
                    self.audio_note_index_path(principal, workspace, &layout.note_id)?,
                    &stored,
                )
                .await
            {
                if !note_exists {
                    let _ = fs::remove_file(&note_absolute_path).await;
                }
                if !audio_exists {
                    let _ = fs::remove_file(&audio_absolute_path).await;
                }
                return Err(artifact_v2_error_to_io(error));
            }
            return Ok(audio_note_ref_from_index(
                stored.note,
                note_absolute_path,
                audio_absolute_path,
            ));
        }
        let selection = registry
            .select_for_write(request.provider.as_deref(), &envelope.settings)
            .await?;
        // Resolve every fallible root-snapshot step before publishing provider
        // files, so even an unusual current-directory failure cannot leave an
        // unindexed pair behind.
        let selected_provider_root = provider_root_snapshot(selection.provider.root())?;

        let markdown = render_audio_note_markdown(&request, &layout);
        let expected_audio_relative =
            safe_relative_path(&layout.target_dir)?.join(safe_file_name(&layout.audio_file_name)?);
        let expected_audio_path =
            checked_join(selection.provider.root(), &expected_audio_relative)?;
        let audio_preexisted = fs::try_exists(&expected_audio_path).await?;
        let expected_note_relative = safe_relative_path(&layout.note_path)?;
        let expected_note_path = checked_join(selection.provider.root(), &expected_note_relative)?;
        let note_preexisted = fs::try_exists(&expected_note_path).await?;

        // A process can stop after publishing one/both provider files but before
        // committing the discovery index. Adopt only exact remnants from that
        // attempt. Never overwrite a human-edited or unrelated file merely
        // because its UUID-shaped path has no runtime index.
        if audio_preexisted && fs::read(&expected_audio_path).await? != request.bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "audio note path already contains a different recording",
            ));
        }
        if note_preexisted && fs::read(&expected_note_path).await? != markdown.as_bytes() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "audio note path already contains different Markdown",
            ));
        }

        let audio_write = if audio_preexisted {
            ProviderWriteRef {
                relative_path: expected_audio_relative,
                absolute_path: expected_audio_path,
            }
        } else {
            selection
                .provider
                .write_asset(&WriteNoteAssetRequest {
                    provider: None,
                    target_dir: layout.target_dir.clone(),
                    file_name: layout.audio_file_name.clone(),
                    bytes: request.bytes,
                })
                .await?
        };

        let note_write = if note_preexisted {
            ProviderWriteRef {
                relative_path: expected_note_relative,
                absolute_path: expected_note_path,
            }
        } else {
            match selection
                .provider
                .write_note_markdown(&WriteNoteMarkdownRequest {
                    provider: None,
                    target_path: layout.note_path.clone(),
                    markdown,
                })
                .await
            {
                Ok(write_ref) => write_ref,
                Err(error) => {
                    if !audio_preexisted {
                        let _ = fs::remove_file(&audio_write.absolute_path).await;
                    }
                    return Err(error);
                },
            }
        };

        let note_ref = AudioNoteRef {
            note_id: layout.note_id.clone(),
            requested_provider: selection.requested_provider.clone(),
            provider: selection.provider.id().to_string(),
            used_fallback: selection.used_fallback,
            fallback_reason: selection.fallback_reason.clone(),
            captured_at: layout.captured_at.to_rfc3339(),
            note_path: path_to_string(&note_write.relative_path),
            note_absolute_path: note_write.absolute_path.display().to_string(),
            audio_path: path_to_string(&audio_write.relative_path),
            audio_absolute_path: audio_write.absolute_path.display().to_string(),
            open_url: provider_open_url(
                selection.provider.id(),
                &envelope,
                &note_write.relative_path,
                &note_write.absolute_path,
            ),
            bytes,
            content_hash: content_hash.clone(),
        };
        let index = StoredAudioNoteIndexEntry {
            note: AudioNoteIndexEntry {
                note_id: note_ref.note_id.clone(),
                requested_provider: note_ref.requested_provider.clone(),
                provider: note_ref.provider.clone(),
                used_fallback: note_ref.used_fallback,
                fallback_reason: note_ref.fallback_reason.clone(),
                captured_at: note_ref.captured_at.clone(),
                source_surface: request.source_surface.trim().to_string(),
                transcript: request
                    .transcript
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                duration_ms: request.duration_ms,
                mime_type: request.mime_type.trim().to_string(),
                note_path: note_ref.note_path.clone(),
                audio_path: note_ref.audio_path.clone(),
                bytes,
                content_hash,
            },
            provider_root: selected_provider_root.display().to_string(),
        };
        if let Err(error) = self
            .workspace_layout
            .write_json_atomic_path(
                self.audio_note_index_path(principal, workspace, &layout.note_id)?,
                &index,
            )
            .await
        {
            if !note_preexisted {
                let _ = fs::remove_file(&note_write.absolute_path).await;
            }
            if !audio_preexisted {
                let _ = fs::remove_file(&audio_write.absolute_path).await;
            }
            return Err(artifact_v2_error_to_io(error));
        }

        Ok(note_ref)
    }

    pub async fn list_audio_notes(
        &self,
        principal: &str,
        workspace: &str,
        offset: usize,
        limit: usize,
        query: Option<&str>,
    ) -> std::io::Result<AudioNotePage> {
        let directory = self.audio_note_index_dir(principal, workspace)?;
        let entries = self
            .workspace_layout
            .read_dir_path_or_empty(&directory)
            .await
            .map_err(artifact_v2_error_to_io)?;
        let normalized_query = query
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);
        let mut notes = Vec::new();
        for entry in entries.into_iter().filter(|entry| entry.is_file) {
            if !entry.file_name.ends_with(".json") {
                continue;
            }
            let path = directory.join(&entry.file_name);
            let Ok(record) = self
                .workspace_layout
                .read_json_path::<StoredAudioNoteIndexEntry, _>(&path)
                .await
            else {
                continue;
            };
            let note = record.note;
            if let Some(query) = normalized_query.as_deref() {
                let haystack = format!(
                    "{} {} {} {}",
                    note.captured_at,
                    note.source_surface,
                    note.provider,
                    note.transcript.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                if !haystack.contains(query) {
                    continue;
                }
            }
            notes.push(note);
        }
        notes.sort_by(|left, right| audio_note_newest_first(left, right));
        let total = notes.len();
        let limit = limit.clamp(1, 100);
        let items = notes
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        Ok(AudioNotePage {
            has_more: offset.saturating_add(items.len()) < total,
            items,
            offset,
            limit,
            total,
        })
    }

    pub async fn read_audio_note(
        &self,
        principal: &str,
        workspace: &str,
        note_id: &str,
    ) -> std::io::Result<Option<AudioNoteIndexEntry>> {
        Ok(self
            .read_audio_note_record(principal, workspace, note_id)
            .await?
            .map(|record| record.note))
    }

    async fn read_audio_note_record(
        &self,
        principal: &str,
        workspace: &str,
        note_id: &str,
    ) -> std::io::Result<Option<StoredAudioNoteIndexEntry>> {
        let path = self.audio_note_index_path(principal, workspace, note_id)?;
        match self
            .workspace_layout
            .read_json_path::<StoredAudioNoteIndexEntry, _>(&path)
            .await
        {
            Ok(record) => Ok(Some(record)),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            },
            Err(error) => Err(artifact_v2_error_to_io(error)),
        }
    }

    pub async fn audio_note_recording(
        &self,
        principal: &str,
        workspace: &str,
        note_id: &str,
    ) -> std::io::Result<Option<AudioNoteRecordingRef>> {
        let Some(record) = self
            .read_audio_note_record(principal, workspace, note_id)
            .await?
        else {
            return Ok(None);
        };
        let note = record.note;
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let Some(provider) = registry.provider(&note.provider) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "audio note provider is no longer available",
            ));
        };
        let provider_root = if record.provider_root.trim().is_empty() {
            provider_root_snapshot(provider.root())?
        } else {
            provider_root_snapshot(Path::new(&record.provider_root))?
        };
        let absolute_path = checked_join(&provider_root, &safe_relative_path(&note.audio_path)?)?;
        if !fs::try_exists(&absolute_path).await? {
            return Ok(None);
        }
        Ok(Some(AudioNoteRecordingRef { absolute_path }))
    }

    pub async fn delete_audio_note(
        &self,
        principal: &str,
        workspace: &str,
        note_id: &str,
    ) -> std::io::Result<bool> {
        let _guard = notes_provider_write_lock().lock().await;
        let Some(record) = self
            .read_audio_note_record(principal, workspace, note_id)
            .await?
        else {
            return Ok(false);
        };
        let note = record.note;
        let envelope = self.load_envelope(principal, workspace).await?;
        let registry = NotesProviderRegistry::from_envelope(&envelope);
        let provider = registry.provider(&note.provider).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "audio note provider is no longer available",
            )
        })?;
        let provider_root = if record.provider_root.trim().is_empty() {
            provider_root_snapshot(provider.root())?
        } else {
            provider_root_snapshot(Path::new(&record.provider_root))?
        };
        for relative in [&note.note_path, &note.audio_path] {
            let path = checked_join(&provider_root, &safe_relative_path(relative)?)?;
            match fs::remove_file(path).await {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        }
        self.workspace_layout
            .remove_file_path(self.audio_note_index_path(principal, workspace, note_id)?)
            .await
            .map_err(artifact_v2_error_to_io)?;
        Ok(true)
    }

    fn envelope_for(
        &self,
        principal: &str,
        workspace: &str,
        settings_path: PathBuf,
        settings: NotesSettings,
    ) -> NotesSettingsEnvelope {
        let local_root = settings
            .local_markdown
            .root
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(|| self.default_local_root(principal, workspace));
        let silverbullet_root =
            resolve_silverbullet_space_root(&settings.silverbullet, principal, workspace);
        let silverbullet_public_origin = settings.silverbullet.public_origin.clone();
        let silverbullet_browser_url = silverbullet_public_origin
            .clone()
            .unwrap_or_else(|| settings.silverbullet.server_url.clone());
        let mut warnings = Vec::new();
        let silverbullet_write_safe = validate_silverbullet_space_boundary(
            &settings.silverbullet,
            &self.workspace_layout,
            principal,
            workspace,
        )
        .is_ok();
        if settings.silverbullet.space_path.is_some() && !silverbullet_write_safe {
            warnings.push(
                "The configured SilverBullet Space would expose canonical runtime storage; SilverBullet note writes will use the configured fallback until a dedicated notes-only folder or strict subfolder is selected."
                    .to_string(),
            );
        }
        if settings.default_provider != DEFAULT_PROVIDER_LOCAL
            && settings.default_provider != DEFAULT_PROVIDER_SILVERBULLET
        {
            warnings.push(format!(
                "Unknown notes provider `{}`; writes will use local_markdown fallback.",
                settings.default_provider
            ));
        }
        NotesSettingsEnvelope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            settings_path: settings_path.display().to_string(),
            resolved: ResolvedNotesProviderSettings {
                default_provider: settings.default_provider.clone(),
                fallback_provider: settings.fallback_provider.clone(),
                local_markdown_root: local_root.display().to_string(),
                silverbullet_space_path: silverbullet_root.display().to_string(),
                silverbullet_write_safe,
                silverbullet_local_url: settings.silverbullet.local_url.clone(),
                silverbullet_public_origin,
                silverbullet_server_url: silverbullet_browser_url,
            },
            settings,
            warnings,
        }
    }

    fn settings_path(&self, principal: &str, workspace: &str) -> std::io::Result<PathBuf> {
        if !is_safe_scope_id(principal) || !is_safe_scope_id(workspace) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsafe principal/workspace scope",
            ));
        }
        Ok(self
            .workspace_layout
            .scope_root(principal, workspace)
            .join(NOTES_DIR)
            .join(NOTES_SETTINGS_FILE))
    }

    fn task_note_index_dir(&self, principal: &str, workspace: &str) -> std::io::Result<PathBuf> {
        Ok(self
            .settings_path(principal, workspace)?
            .parent()
            .expect("notes settings always have a parent")
            .join(TASK_NOTE_INDEX_DIR))
    }

    fn task_note_index_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> std::io::Result<PathBuf> {
        ArtifactV2Workspace::validate_task_id(task_id).map_err(artifact_v2_error_to_io)?;
        Ok(self
            .task_note_index_dir(principal, workspace)?
            .join(format!("{task_id}.json")))
    }

    async fn read_task_note_record(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> std::io::Result<Option<StoredTaskNoteIndexEntry>> {
        let path = self.task_note_index_path(principal, workspace, task_id)?;
        match self
            .workspace_layout
            .read_json_path::<StoredTaskNoteIndexEntry, _>(&path)
            .await
        {
            Ok(record) => Ok(Some(record)),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            },
            Err(error) => Err(artifact_v2_error_to_io(error)),
        }
    }

    fn audio_note_index_dir(&self, principal: &str, workspace: &str) -> std::io::Result<PathBuf> {
        Ok(self
            .settings_path(principal, workspace)?
            .parent()
            .expect("notes settings always have a parent")
            .join(AUDIO_NOTE_INDEX_DIR))
    }

    fn audio_note_index_path(
        &self,
        principal: &str,
        workspace: &str,
        note_id: &str,
    ) -> std::io::Result<PathBuf> {
        let note_id = Uuid::parse_str(note_id.trim()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "note_id must be a UUID")
        })?;
        Ok(self
            .audio_note_index_dir(principal, workspace)?
            .join(format!("{}.json", note_id.hyphenated())))
    }

    fn default_local_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace_layout
            .scope_root(principal, workspace)
            .join(NOTES_DIR)
            .join("space")
    }
}

fn artifact_v2_error_to_io(error: ArtifactV2Error) -> std::io::Error {
    match error {
        ArtifactV2Error::Io(error) => error,
        other => std::io::Error::other(other),
    }
}

/// How long a status request will wait on the notes server.
///
/// Bounded because `provider_status` answers a page load: an unreachable server
/// that accepts the connection and never replies would otherwise hang the
/// settings screen, turning a diagnostic into the fault it is meant to report.
const NOTES_SERVER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Probe the notes server for liveness, not for correctness.
///
/// Any HTTP response means the server is up and serving, so a 401 or a 404
/// counts as reachable — the question here is whether the owner's `open_url`
/// links will resolve at all, not whether a particular page exists or the
/// caller is authorized.
async fn probe_notes_server(server_url: &str) -> (bool, String) {
    let client = match reqwest::Client::builder()
        .timeout(NOTES_SERVER_PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => return (false, format!("probe unavailable: {error}")),
    };
    match client.get(server_url).send().await {
        Ok(response) => (
            true,
            format!("reachable (HTTP {})", response.status().as_u16()),
        ),
        Err(error) if error.is_timeout() => (
            false,
            format!(
                "no response within {}s",
                NOTES_SERVER_PROBE_TIMEOUT.as_secs()
            ),
        ),
        Err(error) if error.is_connect() => (
            false,
            "not running or not accepting connections".to_string(),
        ),
        Err(error) => (false, format!("unreachable: {error}")),
    }
}

/// Whether the `sb` CLI is on PATH.
///
/// Reported, not enforced: nothing in the write path shells out to it, so its
/// absence is information for the optional CLI enhancements rather than a fault
/// that should make a healthy space look broken.
///
/// Resolved once per process. `which` walks every PATH entry with a `stat` per
/// candidate, and this runs on the async runtime behind a status GET that the
/// command palette calls; the process's PATH does not change underneath it.
fn probe_silverbullet_cli() -> (bool, String) {
    static SILVERBULLET_CLI: OnceLock<(bool, String)> = OnceLock::new();

    SILVERBULLET_CLI
        .get_or_init(|| match which::which("sb") {
            Ok(path) => (true, format!("found at {}", path.display())),
            Err(_) => (
                false,
                "not on PATH; only the optional `sb` CLI enhancements need it".to_string(),
            ),
        })
        .clone()
}

async fn status_for_provider(
    id: &str,
    label: &str,
    root: &Path,
    configured: bool,
    create_root_on_write: bool,
) -> NotesProviderStatus {
    let (available, writable, message) = if !configured {
        (false, false, "not configured".to_string())
    } else {
        match fs::metadata(root).await {
            Ok(metadata) if metadata.is_dir() => {
                // Deliberately NOT `writable_probe`: this is a GET
                // (`/notes/providers/status`) that sits in front of the command
                // palette, and the probe creates `.magician/provider-state`,
                // writes a file, then removes all three — five mutations per
                // provider in the owner's Space every time the palette asks how
                // things look. A status read reports; the write path
                // (`unavailable_write_reason`) still does the real probe, where
                // a write is about to happen anyway.
                let writable = directory_is_writable(root).await;
                if writable {
                    (true, true, "ready".to_string())
                } else {
                    (
                        true,
                        false,
                        "directory exists but is not writable".to_string(),
                    )
                }
            },
            Ok(_) => (
                true,
                false,
                "path exists but is not a directory".to_string(),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if create_root_on_write {
                    (
                        false,
                        true,
                        "will be created on first note write".to_string(),
                    )
                } else {
                    (
                        false,
                        false,
                        "directory has not been created yet".to_string(),
                    )
                }
            },
            Err(error) => (false, false, format!("status unavailable: {error}")),
        }
    };
    NotesProviderStatus {
        id: id.to_string(),
        label: label.to_string(),
        configured,
        available,
        writable,
        root: root.display().to_string(),
        message,
        // Filesystem facts only. A provider that needs more than its directory
        // fills this in from `NotesProvider::health`, which knows what that is.
        health: None,
    }
}

/// Whether the process could write into `root`, decided without writing.
///
/// Unix asks the kernel the same question the write itself would ask
/// (`access(2)` with `W_OK`), which accounts for ownership, group and ACLs —
/// unlike inspecting the mode bits, which says nothing about *who* is asking.
/// Elsewhere it falls back to the read-only permission flag.
async fn directory_is_writable(root: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let Ok(path) = std::ffi::CString::new(root.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a valid NUL-terminated C string that outlives the
        // call, and `access` only reads it.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        match fs::metadata(root).await {
            Ok(metadata) => !metadata.permissions().readonly(),
            Err(_) => false,
        }
    }
}

/// Writability by actually writing. Reserved for the write path, which is about
/// to mutate the Space regardless; [`directory_is_writable`] answers the
/// read-only status question.
async fn writable_probe(root: &Path) -> bool {
    let state_dir = root.join(".magician").join("provider-state");
    if fs::create_dir_all(&state_dir).await.is_err() {
        return false;
    }
    let probe = state_dir.join(".write-probe");
    let ok = fs::write(&probe, b"ok").await.is_ok();
    let _ = fs::remove_file(&probe).await;
    // Purely a writability check — don't leave an empty scaffold dir behind in
    // the user's Space. `remove_dir` is a no-op unless the dir is already empty.
    let _ = fs::remove_dir(&state_dir).await;
    let _ = fs::remove_dir(root.join(".magician")).await;
    ok
}

async fn ensure_standard_note_dirs(root: &Path) -> std::io::Result<()> {
    // Only `Inbox` is a real note folder (daily notes land there).
    // `.magician/provider-state` used to be scaffolded here too, but nothing ever
    // persists state in it — it just left an empty hidden dir in the Space root.
    fs::create_dir_all(root.join("Inbox")).await?;
    Ok(())
}

fn render_note_markdown(title: &str, body: &str) -> String {
    let title = title.trim();
    let heading = if title.is_empty() { "Untitled" } else { title };
    format!(
        "---\nmagician_kind: note\ncreated_at: {}\n---\n\n# {}\n\n{}\n",
        Utc::now().to_rfc3339(),
        heading,
        body.trim()
    )
}

fn render_append_markdown(title: Option<&str>, body: &str) -> String {
    let timestamp = Utc::now().to_rfc3339();
    match title {
        Some(title) => format!("\n## {title}\n\n_{}_\n\n{}\n", timestamp, body.trim()),
        None => format!("\n## {}\n\n{}\n", timestamp, body.trim()),
    }
}

fn requested_provider_id(requested: Option<&str>, settings: &NotesSettings) -> String {
    requested
        .and_then(normalize_provider_id)
        .unwrap_or_else(|| settings.default_provider.clone())
}

fn normalize_provider_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match value {
        "local" | "local_markdown" => Some(DEFAULT_PROVIDER_LOCAL.to_string()),
        "sb" | "silverbullet" | "silverbullet_space" => {
            Some(DEFAULT_PROVIDER_SILVERBULLET.to_string())
        },
        _ => Some(value.to_string()),
    }
}

fn normalize_optional_path(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_public_origin(value: Option<String>) -> Option<String> {
    let value = normalize_optional_string(value)?;
    let mut url = url::Url::parse(&value).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    url.set_path("");
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn validate_silverbullet_network_settings(
    settings: &SilverBulletNotesSettings,
) -> std::io::Result<()> {
    let local = url::Url::parse(settings.local_url.trim()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SilverBullet local_url must be an HTTP loopback origin",
        )
    })?;
    let loopback = local
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback())
        || local.host_str() == Some("localhost");
    if local.scheme() != "http"
        || !loopback
        || !local.username().is_empty()
        || local.password().is_some()
        || local.query().is_some()
        || local.fragment().is_some()
        || !matches!(local.path(), "" | "/")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SilverBullet local_url must be an HTTP loopback origin",
        ));
    }
    if settings
        .public_origin
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
        && normalize_public_origin(settings.public_origin.clone()).is_none()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SilverBullet public_origin must be an HTTPS origin without credentials, path, query, or fragment",
        ));
    }
    if normalize_browser_origin_or_default(settings.server_url.clone(), String::new()).is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SilverBullet server_url must be an HTTP(S) origin without credentials, path, query, or fragment",
        ));
    }
    Ok(())
}

fn silverbullet_space_is_active(settings: &NotesSettings) -> bool {
    settings
        .silverbullet
        .space_path
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty())
}

fn path_is_workspace_space(path: &Path, principal: &str, workspace: &str) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(workspace)
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some(principal)
        && path
            .parent()
            .and_then(|parent| parent.parent())
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some(SILVERBULLET_SPACES_DIR)
}

fn forest_prefix_relative_is_workspace_space(
    configured: &Path,
    relative: &Path,
    principal: &str,
    workspace: &str,
) -> bool {
    let comps: Vec<&str> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    match comps.as_slice() {
        [] => true,
        [w] if *w == workspace
            && configured.file_name().and_then(|name| name.to_str()) == Some(principal)
            && configured
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                == Some(SILVERBULLET_SPACES_DIR) =>
        {
            true
        },
        [p, w]
            if *p == principal
                && *w == workspace
                && configured.file_name().and_then(|name| name.to_str())
                    == Some(SILVERBULLET_SPACES_DIR) =>
        {
            true
        },
        [dir] if *dir == SILVERBULLET_SPACES_DIR => true,
        [dir, p] if *dir == SILVERBULLET_SPACES_DIR && *p == principal => true,
        [dir, p, w] if *dir == SILVERBULLET_SPACES_DIR && *p == principal && *w == workspace => {
            true
        },
        _ => false,
    }
}

fn pin_has_notes_besides_spaces(configured: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(configured) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        name.to_str()
            .is_none_or(|name| name != ".DS_Store" && name != SILVERBULLET_SPACES_DIR)
    })
}

fn looks_like_on_disk_notes_forest(configured: &Path) -> bool {
    if pin_has_notes_besides_spaces(configured) {
        return false;
    }
    let spaces = configured.join(SILVERBULLET_SPACES_DIR);
    let Ok(principals) = std::fs::read_dir(&spaces) else {
        return false;
    };
    for principal in principals.flatten() {
        if !principal
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        let principal_name = principal.file_name();
        let Some(principal_name) = principal_name.to_str() else {
            continue;
        };
        if !is_safe_scope_id(principal_name) {
            continue;
        }
        let Ok(workspaces) = std::fs::read_dir(principal.path()) else {
            continue;
        };
        for workspace in workspaces.flatten() {
            if !workspace
                .file_type()
                .map(|kind| kind.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let workspace_name = workspace.file_name();
            let Some(workspace_name) = workspace_name.to_str() else {
                continue;
            };
            if is_safe_scope_id(workspace_name) {
                return true;
            }
        }
    }
    false
}

fn coerce_forest_pin_to_workspace_space(
    configured: PathBuf,
    principal: &str,
    workspace: &str,
    scoped_default: PathBuf,
) -> PathBuf {
    let principal = silverbullet_scope_segment(principal);
    let workspace = silverbullet_scope_segment(workspace);
    if path_is_workspace_space(&configured, principal, workspace) {
        return configured;
    }
    if let Ok(relative) = scoped_default.strip_prefix(&configured) {
        if forest_prefix_relative_is_workspace_space(&configured, &relative, principal, workspace) {
            return scoped_default;
        }
    }
    let from_forest = configured
        .join(SILVERBULLET_SPACES_DIR)
        .join(principal)
        .join(workspace);
    // A missing workspace child must still resolve to that child. Searching the
    // forest or `spaces/<principal>` would walk sibling Spaces.
    if from_forest.is_dir() || looks_like_on_disk_notes_forest(&configured) {
        return from_forest;
    }
    let from_spaces = configured.join(principal).join(workspace);
    if configured.file_name().and_then(|name| name.to_str()) == Some(SILVERBULLET_SPACES_DIR)
        && (from_spaces.is_dir() || !pin_has_notes_besides_spaces(&configured))
    {
        return from_spaces;
    }
    if configured
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some(SILVERBULLET_SPACES_DIR)
        && configured.file_name().and_then(|name| name.to_str()) == Some(principal)
    {
        let child = configured.join(workspace);
        if child.is_dir() || !pin_has_notes_besides_spaces(&configured) {
            return child;
        }
    }
    configured
}

fn resolve_silverbullet_space_root(
    settings: &SilverBulletNotesSettings,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    let scoped_default = default_silverbullet_space_path(principal, workspace);
    match settings.space_path.as_deref() {
        Some(path) if !path.trim().is_empty() => coerce_forest_pin_to_workspace_space(
            expand_tilde(path),
            principal,
            workspace,
            scoped_default,
        ),
        _ => scoped_default,
    }
}

fn silverbullet_scope_segment(id: &str) -> &str {
    if is_safe_scope_id(id) {
        id
    } else {
        "default"
    }
}

fn validate_silverbullet_space_boundary(
    settings: &SilverBulletNotesSettings,
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    let space = resolve_silverbullet_space_root(settings, principal, workspace_id);
    let space = normalized_absolute_path(&space)?;
    if silverbullet_space_is_too_broad(&space) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SilverBullet space_path must be a notes-only folder, not HOME or /",
        ));
    }
    for runtime_root in [workspace.base_root(), workspace.visible_root()] {
        let runtime_root = normalized_absolute_path(runtime_root)?;
        if runtime_root == space || runtime_root.starts_with(&space) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SilverBullet space_path must be a notes-only folder, not a parent of canonical runtime storage",
            ));
        }
    }
    Ok(())
}

fn silverbullet_space_is_too_broad(space: &Path) -> bool {
    if space == Path::new("/") {
        return true;
    }
    std::env::var_os("HOME").is_some_and(|home| {
        normalized_absolute_path(Path::new(&home)).is_ok_and(|home| home == space)
    })
}

fn normalized_absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {},
            Component::ParentDir => {
                // Resolve an existing symlink before applying `..`. Lexically
                // collapsing first can produce a different path than the OS
                // (for example `link-to-child/..`) and would weaken the
                // notes-only Space boundary.
                match std::fs::canonicalize(&resolved) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error),
                }
                resolved.pop();
            },
            Component::Normal(part) => {
                resolved.push(part);
                match std::fs::canonicalize(&resolved) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error),
                }
            },
        }
    }
    Ok(resolved)
}

fn normalize_url_or_default(value: String, default: String) -> String {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        default
    } else {
        value.to_string()
    }
}

fn normalize_browser_origin_or_default(value: String, default: String) -> String {
    let value = value.trim();
    let Ok(mut url) = url::Url::parse(value) else {
        return default;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return default;
    }
    url.set_path("");
    url.to_string().trim_end_matches('/').to_string()
}

fn expand_tilde(value: &str) -> PathBuf {
    let value = value.trim();
    if value == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(value)
}

fn default_silverbullet_space_path(principal: &str, workspace: &str) -> PathBuf {
    resolve_default_silverbullet_space_path(
        std::env::var_os("MAGICIAN_NOTES_SPACE"),
        Some(crate::magician_v2::process_storage::runtime_root().into_os_string()),
        None,
        std::env::var_os("HOME"),
        principal,
        workspace,
    )
}

fn resolve_default_silverbullet_space_path(
    explicit_forest: Option<OsString>,
    runtime_root: Option<OsString>,
    legacy_storage_root: Option<OsString>,
    home: Option<OsString>,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    let forest = if let Some(explicit_forest) = explicit_forest.filter(|value| !value.is_empty()) {
        PathBuf::from(explicit_forest)
    } else {
        runtime_root
            .filter(|value| !value.is_empty())
            .or(legacy_storage_root)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                home.filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join("MagicianNotes"))
            })
            .unwrap_or_else(|| PathBuf::from("MagicianNotes"))
            .join("MagicanNotes")
    };
    forest
        .join(SILVERBULLET_SPACES_DIR)
        .join(silverbullet_scope_segment(principal))
        .join(silverbullet_scope_segment(workspace))
}

fn default_silverbullet_local_url() -> String {
    "http://127.0.0.1:3021".to_string()
}

fn default_silverbullet_server_url() -> String {
    "http://127.0.0.1:3021".to_string()
}

fn default_provider() -> String {
    DEFAULT_PROVIDER_LOCAL.to_string()
}

fn default_fallback_provider() -> String {
    DEFAULT_PROVIDER_LOCAL.to_string()
}

fn default_true() -> bool {
    true
}

/// Whether a provider-relative path is one [`NotesSettingsStore::read_observation_note`]
/// could resolve: relative, free of parent/root components, and Markdown.
///
/// Exactly the gate that read path applies, exposed so a caller holding a
/// configured note path can refuse it up front. Without this, a mistyped path
/// is indistinguishable from a note that was never written.
pub fn is_readable_note_path(value: &str) -> bool {
    safe_relative_path(value).is_ok_and(|path| is_markdown_note_path(&path))
}

fn safe_relative_path(value: &str) -> std::io::Result<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "notes paths must be relative to the provider root",
        ));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {},
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "notes paths may not contain parent/root/prefix components",
                ));
            },
        }
    }
    if out.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "notes path cannot be empty",
        ));
    }
    Ok(out)
}

fn safe_file_name(value: &str) -> std::io::Result<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "asset file name cannot be empty",
        ));
    }
    let path = Path::new(value);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) => Ok(PathBuf::from(part)),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "asset file name must not contain path components",
        )),
    }
}

fn checked_join(root: &Path, relative: &Path) -> std::io::Result<PathBuf> {
    let candidate = root.join(relative);
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let relative_parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let canonical_parent = canonical_parent_for_relative(root, relative_parent, &canonical_root)?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "notes path escapes provider root",
        ));
    }
    Ok(candidate)
}

fn provider_root_snapshot(root: &Path) -> std::io::Result<PathBuf> {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    // Canonicalize when the provider root exists so symlinked configuration is
    // bound to the same durable directory. Missing legacy roots retain their
    // absolute spelling and can become reachable again without CWD dependence.
    Ok(std::fs::canonicalize(&absolute).unwrap_or(absolute))
}

fn canonical_parent_for_relative(
    root: &Path,
    relative_parent: &Path,
    canonical_root: &Path,
) -> std::io::Result<PathBuf> {
    let parts: Vec<OsString> = relative_parent
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect();

    for existing_len in (0..=parts.len()).rev() {
        let mut existing = PathBuf::new();
        for part in &parts[..existing_len] {
            existing.push(part);
        }
        let existing_abs = if existing.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&existing)
        };
        match std::fs::canonicalize(&existing_abs) {
            Ok(mut canonical) => {
                for part in &parts[existing_len..] {
                    canonical.push(part);
                }
                return Ok(canonical);
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }

    let mut canonical = canonical_root.to_path_buf();
    for part in parts {
        canonical.push(part);
    }
    Ok(canonical)
}

async fn unique_note_path(root: &Path, preferred: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let first_abs = checked_join(root, preferred)?;
    match fs::metadata(&first_abs).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((preferred.to_path_buf(), first_abs));
        },
        Err(error) => return Err(error),
        Ok(_) => {},
    }

    let parent = preferred.parent().unwrap_or_else(|| Path::new(""));
    let file_name = preferred
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("note.md");
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            (stem.to_string(), Some(extension.to_string()))
        },
        _ => (file_name.to_string(), None),
    };

    for index in 2..10_000 {
        let candidate_name = match &extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate_rel = if parent.as_os_str().is_empty() {
            PathBuf::from(candidate_name)
        } else {
            parent.join(candidate_name)
        };
        let candidate_abs = checked_join(root, &candidate_rel)?;
        match fs::metadata(&candidate_abs).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((candidate_rel, candidate_abs));
            },
            Err(error) => return Err(error),
            Ok(_) => {},
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique note file name",
    ))
}

/// Slugify a title into a note-file-safe stem. Shared with the notes
/// projection seam, whose page and asset names are built from the same
/// slugs this provider layer files them under.
pub(crate) fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Render a path with forward slashes on every platform. Shared with the
/// notes projection seam, whose default daily page is named in the same
/// form the provider writes it.
pub(crate) fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[derive(Debug)]
struct ObservationNoteDescriptor {
    provider: String,
    canonical_root: PathBuf,
    absolute_path: PathBuf,
    relative_path: String,
    modified_at_ms: i64,
}

/// A hit plus the path needed to build its open URL, which the hit itself must
/// not carry: a filesystem path is not something a search result should hand out.
struct NoteSearchCandidate {
    hit: NoteSearchHit,
    absolute_path: PathBuf,
}

/// Split a query into the terms a note must contain.
///
/// Lowercased for case-insensitive matching, and bounded so a pathological query
/// cannot turn every note read into a long scan of the same body.
fn note_search_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| !term.is_empty())
        .take(MAX_NOTE_SEARCH_TERMS)
        .collect()
}

/// Decide whether one already-read note answers the query.
///
/// Every term must appear somewhere in the note. Requiring all of them is what
/// makes a two-word query narrow the results instead of widening them, which is
/// the opposite of what matching any term would do.
fn evaluate_indexed_note_fuzzy(
    note: &super::notes_search_index::IndexedNote,
    terms: &[String],
) -> Option<NoteSearchCandidate> {
    evaluate_indexed_note_matching(note, terms, true)
}

fn semantic_note_candidate(
    note: &super::notes_search_index::IndexedNote,
) -> Option<NoteSearchCandidate> {
    let relative = PathBuf::from(&note.relative_path);
    let title = markdown_note_title(&note.markdown, &relative);
    let text = note
        .markdown
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(title.as_str())
        .to_string();
    Some(NoteSearchCandidate {
        hit: NoteSearchHit {
            provider: note.provider.clone(),
            relative_path: note.relative_path.clone(),
            source_ref: format!("notes:{}:{}", note.provider, note.relative_path),
            title,
            modified_at_ms: note.modified_at_ms,
            open_url: None,
            matched_in_title: false,
            match_count: 0,
            matches: vec![NoteSearchMatch {
                line: 1,
                text: bounded_search_snippet(&text),
            }],
        },
        absolute_path: note.absolute_path.clone(),
    })
}

fn term_in_text(haystack: &str, term: &str, fuzzy: bool) -> bool {
    if haystack.contains(term) {
        return true;
    }
    if !fuzzy {
        return false;
    }
    let max_distance = match term.chars().count() {
        0..=2 => 0,
        3..=5 => 1,
        _ => 2,
    };
    if max_distance == 0 {
        return false;
    }
    haystack
        .split(|ch: char| !ch.is_alphanumeric())
        .any(|token| !token.is_empty() && levenshtein_within(token, term, max_distance))
}

fn levenshtein_within(left: &str, right: &str, max_distance: usize) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > max_distance {
        return false;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        let mut row_best = current[0];
        for (j, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
            row_best = row_best.min(current[j + 1]);
        }
        if row_best > max_distance {
            return false;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()] <= max_distance
}

fn evaluate_indexed_note_matching(
    note: &super::notes_search_index::IndexedNote,
    terms: &[String],
    fuzzy: bool,
) -> Option<NoteSearchCandidate> {
    let relative = PathBuf::from(&note.relative_path);
    let title = markdown_note_title(&note.markdown, &relative);
    let haystack = note.markdown.to_lowercase();
    let title_lower = title.to_lowercase();
    // Match the path without its extension. Every note ends in `.md`, so leaving
    // it in makes a search for `md` match the whole space and call each one a
    // title hit — the folder and the file name are the owner's words, the
    // extension is not.
    // `with_extension("")` drops only the file's own extension, so a directory
    // carrying a dot — `Projects/v1.2/` — keeps its name. Splitting the whole
    // path on its last dot happens to work only while every note ends in `.md`,
    // and would silently start eating folder names if that ever changed.
    let path_lower = Path::new(&note.relative_path)
        .with_extension("")
        .to_string_lossy()
        .to_lowercase();

    // Where a note is filed counts as part of it, alongside its name: a page
    // called "Invoices", and a page under `Clients/Acme/`, both answer a search
    // for that word whether or not the body repeats it. Path has to be admitted
    // here and not only in `matched_in_title` below — checking it in one place
    // and not the other is what made a path-only hit unreachable.
    if !terms.iter().all(|term| {
        term_in_text(&haystack, term, fuzzy)
            || term_in_text(&title_lower, term, fuzzy)
            || term_in_text(&path_lower, term, fuzzy)
    }) {
        return None;
    }
    let matched_in_title = terms.iter().all(|term| {
        term_in_text(&title_lower, term, fuzzy) || term_in_text(&path_lower, term, fuzzy)
    });

    // Rank lines by how many distinct terms they carry, so the line shown first
    // is the one closest to the whole question rather than the earliest mention.
    let mut scored = Vec::new();
    for (index, line) in note.markdown.lines().enumerate() {
        let lowered = line.to_lowercase();
        let carried = terms
            .iter()
            .filter(|term| term_in_text(&lowered, term, fuzzy))
            .count();
        if carried > 0 {
            scored.push((carried, index, line));
        }
    }
    let match_count = scored.len();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let matches = scored
        .into_iter()
        .take(MAX_NOTE_SEARCH_MATCHES_PER_NOTE)
        .map(|(_, index, line)| NoteSearchMatch {
            line: index.saturating_add(1),
            text: bounded_search_snippet(line),
        })
        .collect::<Vec<_>>();

    // Only the title matched, with nothing in the body — still a hit, and the
    // title is the evidence.
    if matches.is_empty() && !matched_in_title {
        return None;
    }

    Some(NoteSearchCandidate {
        hit: NoteSearchHit {
            provider: note.provider.clone(),
            relative_path: note.relative_path.clone(),
            source_ref: format!("notes:{}:{}", note.provider, note.relative_path),
            title,
            modified_at_ms: note.modified_at_ms,
            open_url: None,
            matched_in_title,
            match_count,
            matches,
        },
        absolute_path: note.absolute_path.clone(),
    })
}

/// Trim a matching line to something quotable.
///
/// Bounded on characters rather than bytes so a multi-byte line cannot be cut
/// mid-character, which would make the snippet invalid UTF-8 to render.
fn bounded_search_snippet(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= MAX_NOTE_SEARCH_SNIPPET_CHARS {
        return trimmed.to_string();
    }
    let kept = trimmed
        .chars()
        .take(MAX_NOTE_SEARCH_SNIPPET_CHARS)
        .collect::<String>();
    format!("{kept}…")
}

async fn materialize_observation_note(
    envelope: &NotesSettingsEnvelope,
    descriptor: ObservationNoteDescriptor,
) -> std::io::Result<Option<(ObservedMarkdownNote, u64)>> {
    let metadata = match fs::symlink_metadata(&descriptor.absolute_path).await {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_OBSERVATION_NOTE_BYTES => {
            metadata
        },
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let canonical_path = match fs::canonicalize(&descriptor.absolute_path).await {
        Ok(path) if path.starts_with(&descriptor.canonical_root) => path,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let bytes = fs::read(&canonical_path).await?;
    if bytes.len() as u64 > MAX_OBSERVATION_NOTE_BYTES {
        return Ok(None);
    }
    let content_hash = blake3::hash(&bytes).to_hex().to_string();
    let markdown = match String::from_utf8(bytes) {
        Ok(markdown) => bounded_note_markdown(markdown),
        Err(_) => return Ok(None),
    };
    if markdown.trim().is_empty() {
        return Ok(None);
    }
    let relative = PathBuf::from(&descriptor.relative_path);
    let open_url = provider_open_url(&descriptor.provider, envelope, &relative, &canonical_path);
    Ok(Some((
        ObservedMarkdownNote {
            provider: descriptor.provider.clone(),
            relative_path: descriptor.relative_path.clone(),
            source_ref: format!("notes:{}:{}", descriptor.provider, descriptor.relative_path),
            title: markdown_note_title(&markdown, &relative),
            markdown,
            modified_at_ms: descriptor.modified_at_ms,
            content_hash,
            open_url,
        },
        metadata.len(),
    )))
}

async fn scan_observation_root(
    source: &NotesObservationRoot,
    descriptors: &mut Vec<ObservationNoteDescriptor>,
    scanned_entries: &mut usize,
) -> std::io::Result<()> {
    let canonical_root = match fs::canonicalize(&source.root).await {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut pending = VecDeque::from([canonical_root.clone()]);
    while let Some(directory) = pending.pop_front() {
        if *scanned_entries >= MAX_OBSERVATION_SCAN_ENTRIES {
            break;
        }
        let mut reader = fs::read_dir(&directory).await?;
        let mut entries = Vec::new();
        while let Some(entry) = reader.next_entry().await? {
            entries.push(entry);
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if *scanned_entries >= MAX_OBSERVATION_SCAN_ENTRIES {
                break;
            }
            *scanned_entries = (*scanned_entries).saturating_add(1);
            let file_type = entry.file_type().await?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !is_observation_internal_directory(&entry.file_name()) {
                    pending.push_back(entry.path());
                }
                continue;
            }
            if !file_type.is_file() || !is_markdown_note_path(&entry.path()) {
                continue;
            }
            let metadata = entry.metadata().await?;
            if metadata.len() > MAX_OBSERVATION_NOTE_BYTES {
                continue;
            }
            let relative = match entry.path().strip_prefix(&canonical_root) {
                Ok(path) => path_to_string(path),
                Err(_) => continue,
            };
            if relative.is_empty() || relative.chars().any(char::is_control) {
                continue;
            }
            let modified_at_ms = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or_else(|| Utc::now().timestamp_millis())
                .max(1);
            descriptors.push(ObservationNoteDescriptor {
                provider: source.provider.clone(),
                canonical_root: canonical_root.clone(),
                absolute_path: entry.path(),
                relative_path: relative,
                modified_at_ms,
            });
        }
    }
    Ok(())
}

pub(crate) fn is_observation_internal_directory(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with('.') || matches!(name, "_plug" | "_trash"))
}

async fn directory_has_listable_child(directory: &Path) -> bool {
    let Ok(mut entries) = fs::read_dir(directory).await else {
        return false;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if file_type.is_symlink() || is_observation_internal_directory(&entry.file_name()) {
            continue;
        }
        if file_type.is_dir() || (file_type.is_file() && is_markdown_note_path(&entry.path())) {
            return true;
        }
    }
    false
}

fn folder_name(name: &str) -> std::io::Result<String> {
    let trimmed = name.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a folder name cannot include another folder",
        ));
    }
    Ok(trimmed.to_string())
}

fn note_file_name(name: &str) -> std::io::Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a note name cannot include a folder",
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}.md"))
    }
}

async fn refuse_symlink_inside(directory: &Path) -> std::io::Result<()> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        let mut entries = fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            if file_type.is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a notes folder that contains a symlink cannot be deleted",
                ));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

pub(crate) fn is_markdown_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
        })
}

fn bounded_note_markdown(markdown: String) -> String {
    if markdown.chars().count() <= MAX_OBSERVATION_NOTE_CHARS {
        markdown
    } else {
        markdown.chars().take(MAX_OBSERVATION_NOTE_CHARS).collect()
    }
}

fn markdown_note_title(markdown: &str, relative_path: &Path) -> String {
    let heading = markdown.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|title| !title.is_empty())
    });
    let fallback = relative_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("Untitled note");
    let title = heading.unwrap_or(fallback);
    let sanitized = title
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(240)
        .collect::<String>();
    if sanitized.trim().is_empty() {
        "Untitled note".to_string()
    } else {
        sanitized.trim().to_string()
    }
}

fn provider_open_url(
    provider_id: &str,
    envelope: &NotesSettingsEnvelope,
    relative_path: &Path,
    absolute_path: &Path,
) -> Option<String> {
    match provider_id {
        DEFAULT_PROVIDER_LOCAL => file_open_url(absolute_path),
        DEFAULT_PROVIDER_SILVERBULLET => {
            silverbullet_open_url(&envelope.resolved.silverbullet_server_url, relative_path)
        },
        _ => None,
    }
}

fn file_open_url(path: &Path) -> Option<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    url::Url::from_file_path(absolute)
        .ok()
        .map(|url| url.to_string())
}

fn silverbullet_open_url(server_url: &str, relative_path: &Path) -> Option<String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }

    let mut path = path_to_string(relative_path);
    if let Some(stripped) = path.strip_suffix(".md") {
        path = stripped.to_string();
    } else if let Some(stripped) = path.strip_suffix(".markdown") {
        path = stripped.to_string();
    }
    let encoded = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(urlencoding::encode)
        .collect::<Vec<_>>()
        .join("/");

    if encoded.is_empty() {
        Some(format!("{base}/"))
    } else {
        Some(format!("{base}/{encoded}"))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::{
        TaskLifecycle, TaskManifest, TaskOutputMode, TaskRefs, TaskState, TaskSyncMode,
        TaskTagRecord,
    };

    /// Write the notes a search test needs, and return the store.
    async fn store_with_notes(
        temp: &tempfile::TempDir,
        notes: &[(&str, &str)],
    ) -> NotesSettingsStore {
        let store = NotesSettingsStore::new(temp.path());
        for (path, body) in notes {
            store
                .write_note_markdown(
                    "owner",
                    "default",
                    WriteNoteMarkdownRequest {
                        provider: Some("local_markdown".into()),
                        target_path: (*path).into(),
                        markdown: (*body).to_string(),
                    },
                )
                .await
                .unwrap();
        }
        store
    }

    fn search(query: &str) -> NoteSearchRequest {
        NoteSearchRequest {
            query: query.into(),
            limit: None,
            provider: None,
        }
    }

    /// Every term must match, so adding a word narrows rather than widens.
    #[tokio::test]
    async fn search_requires_every_term_not_any_of_them() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[
                (
                    "Inbox/both.md",
                    "# Both\n\nthe budget meeting is on Tuesday",
                ),
                ("Inbox/one.md", "# One\n\nthe budget is approved"),
                ("Inbox/other.md", "# Other\n\nthe meeting was moved"),
            ],
        )
        .await;

        let results = store
            .search_notes("owner", "default", search("budget meeting"))
            .await
            .unwrap();

        let paths = results
            .hits
            .iter()
            .map(|hit| hit.relative_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["Inbox/both.md"]);
        assert_eq!(results.query_terms, vec!["budget", "meeting"]);
    }

    /// A note named for the thing answers a search for it, and outranks a note
    /// that merely mentions it.
    #[tokio::test]
    async fn a_title_match_outranks_a_body_match() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[
                (
                    "Inbox/mentions.md",
                    "# Scratch\n\ninvoices came up twice\ninvoices again",
                ),
                (
                    "Inbox/invoices.md",
                    "# Invoices\n\nnothing else written yet",
                ),
            ],
        )
        .await;

        let results = store
            .search_notes("owner", "default", search("invoices"))
            .await
            .unwrap();

        assert_eq!(results.hits.len(), 2);
        assert_eq!(results.hits[0].relative_path, "Inbox/invoices.md");
        assert!(results.hits[0].matched_in_title);
        assert!(!results.hits[1].matched_in_title);
    }

    /// Case-insensitive and substring, which is what makes an owner's own words
    /// work without them guessing the exact form they typed.
    #[tokio::test]
    async fn search_ignores_case_and_matches_inside_a_word() {
        let temp = tempfile::tempdir().unwrap();
        let store =
            store_with_notes(&temp, &[("Inbox/a.md", "# A\n\nThe INVOICES were filed")]).await;

        let results = store
            .search_notes("owner", "default", search("invoice"))
            .await
            .unwrap();

        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].matches[0].text, "The INVOICES were filed");
        assert_eq!(results.hits[0].matches[0].line, 3);
    }

    /// The catalog is reused for an unchanged folder and dropped when a file changes.
    #[tokio::test]
    async fn search_sees_an_edit_instead_of_a_stale_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(&temp, &[("Inbox/a.md", "# A\n\nfirst draft")]).await;

        let first = store
            .search_notes("owner", "default", search("first"))
            .await
            .unwrap();
        assert_eq!(first.hits.len(), 1);

        let second = store
            .search_notes("owner", "default", search("first"))
            .await
            .unwrap();
        assert_eq!(second.hits.len(), 1);

        store
            .write_note_markdown(
                "owner",
                "default",
                WriteNoteMarkdownRequest {
                    provider: Some("local_markdown".into()),
                    target_path: "Inbox/a.md".into(),
                    markdown: "# A\n\nsecond draft".into(),
                },
            )
            .await
            .unwrap();

        let after = store
            .search_notes("owner", "default", search("first"))
            .await
            .unwrap();
        assert!(after.hits.is_empty());
        let renamed = store
            .search_notes("owner", "default", search("second"))
            .await
            .unwrap();
        assert_eq!(renamed.hits.len(), 1);
        assert_eq!(renamed.hits[0].matches[0].text, "second draft");
    }

    /// A one-character typo still finds the note through the BM25 fuzzy index.
    #[tokio::test]
    async fn search_finds_a_note_when_one_character_is_wrong() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[("Inbox/invoice.md", "# Invoice\n\npayment is due Friday")],
        )
        .await;

        let results = store
            .search_notes("owner", "default", search("invioce"))
            .await
            .unwrap();

        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].relative_path, "Inbox/invoice.md");
        assert!(results.hits[0].matched_in_title);
    }

    #[tokio::test]
    async fn note_tree_lists_one_folder_level_and_reads_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[
                ("Inbox/a.md", "# Hello\n\nbody text"),
                ("Projects/nested.md", "# Nested\n\ninside"),
            ],
        )
        .await;

        let root = store.list_note_tree("owner", "default", "").await.unwrap();
        let names = root
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Inbox", "Projects"]);
        assert!(root.entries.iter().all(|entry| entry.kind == "dir"));
        assert!(root.entries.iter().all(|entry| entry.has_children));

        let inbox = store
            .list_note_tree("owner", "default", "Inbox")
            .await
            .unwrap();
        assert_eq!(inbox.entries.len(), 1);
        assert_eq!(inbox.entries[0].kind, "file");
        assert_eq!(inbox.entries[0].relative_path, "Inbox/a.md");

        let file = store
            .read_note_file("owner", "default", "Inbox/a.md")
            .await
            .unwrap();
        assert_eq!(file.title, "Hello");
        assert!(file.markdown.contains("body text"));
        assert!(store
            .read_note_file("owner", "default", "../secret.md")
            .await
            .is_err());

        let created = store
            .create_markdown_note("owner", "default", "Inbox", "Fresh page")
            .await
            .unwrap();
        assert_eq!(created.relative_path, "Inbox/Fresh page.md");
        let found = store
            .search_notes("owner", "default", search("Fresh"))
            .await
            .unwrap();
        assert!(found
            .hits
            .iter()
            .any(|hit| hit.relative_path == "Inbox/Fresh page.md"));
        store
            .delete_markdown_note("owner", "default", "Inbox/Fresh page.md")
            .await
            .unwrap();
        let after = store
            .search_notes("owner", "default", search("Fresh"))
            .await
            .unwrap();
        assert!(after.hits.is_empty());
        store
            .create_markdown_note("owner", "default", "Empty", "temp")
            .await
            .unwrap();
        store
            .delete_markdown_note("owner", "default", "Empty/temp.md")
            .await
            .unwrap();
        let with_empty = store.list_note_tree("owner", "default", "").await.unwrap();
        assert!(!with_empty
            .entries
            .iter()
            .find(|entry| entry.name == "Empty")
            .unwrap()
            .has_children);
        store
            .delete_note_folder("owner", "default", "Projects")
            .await
            .unwrap();
        let remaining = store.list_note_tree("owner", "default", "").await.unwrap();
        assert!(remaining
            .entries
            .iter()
            .all(|entry| entry.name != "Projects"));
    }

    /// The line carrying the most of the question is shown first, not the
    /// earliest mention.
    #[tokio::test]
    async fn the_strongest_line_is_the_one_returned_first() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[(
                "Inbox/a.md",
                "# A\n\nbudget alone here\nmeeting alone here\nthe budget meeting decision",
            )],
        )
        .await;

        let results = store
            .search_notes("owner", "default", search("budget meeting"))
            .await
            .unwrap();

        assert_eq!(
            results.hits[0].matches[0].text,
            "the budget meeting decision"
        );
        assert_eq!(results.hits[0].match_count, 3);
    }

    /// Where a note is filed is part of what it is. This was documented in the
    /// tool guide before it was true: the admission filter checked body and
    /// title only, so a path-only hit was dropped before ranking could see it.
    #[tokio::test]
    async fn a_note_is_found_by_the_folder_it_is_filed_in() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[(
                "Clients/Acme/kickoff.md",
                "# Kickoff\n\nnothing else written yet",
            )],
        )
        .await;

        let results = store
            .search_notes("owner", "default", search("acme"))
            .await
            .unwrap();

        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].relative_path, "Clients/Acme/kickoff.md");
        assert!(results.hits[0].matched_in_title);
    }

    /// The extension is not something the owner wrote. Leaving it in the matched
    /// path made a search for `md` return the entire space, every hit ranked as
    /// a title match.
    #[tokio::test]
    async fn the_file_extension_is_not_searchable_text() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(
            &temp,
            &[
                ("Inbox/a.md", "# A\n\nfirst body"),
                ("Inbox/b.md", "# B\n\nsecond body"),
            ],
        )
        .await;

        let results = store
            .search_notes("owner", "default", search("md"))
            .await
            .unwrap();
        assert!(
            results.hits.is_empty(),
            "the extension must not match every note"
        );

        // The folder and file name still match, because those the owner chose.
        let by_folder = store
            .search_notes("owner", "default", search("inbox"))
            .await
            .unwrap();
        assert_eq!(by_folder.hits.len(), 2);
    }

    /// A provider that is misspelt or unconfigured must not read as an empty
    /// notes space.
    #[tokio::test]
    async fn an_unknown_provider_is_refused_rather_than_searched_as_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(&temp, &[("Inbox/a.md", "# A\n\nbudget")]).await;

        let error = store
            .search_notes(
                "owner",
                "default",
                NoteSearchRequest {
                    query: "budget".into(),
                    limit: None,
                    provider: Some("silverbulet".into()),
                },
            )
            .await
            .expect_err("a provider that matches no root must not silently find nothing");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("local_markdown"));
    }

    /// A capped result set must say so — presenting it as everything there is
    /// would be a wrong answer, not a short one.
    #[tokio::test]
    async fn a_capped_result_set_reports_that_more_exist() {
        let temp = tempfile::tempdir().unwrap();
        let notes = (0..5)
            .map(|index| {
                (
                    format!("Inbox/n{index}.md"),
                    "# N\n\nshared term".to_string(),
                )
            })
            .collect::<Vec<_>>();
        let borrowed = notes
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect::<Vec<_>>();
        let store = store_with_notes(&temp, &borrowed).await;

        let results = store
            .search_notes(
                "owner",
                "default",
                NoteSearchRequest {
                    query: "shared".into(),
                    limit: Some(2),
                    provider: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(results.hits.len(), 2);
        assert!(results.more_available);
        assert!(!results.scan_truncated);
    }

    /// An empty query is refused rather than returning the whole space.
    #[tokio::test]
    async fn search_refuses_a_query_with_no_terms() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(&temp, &[("Inbox/a.md", "# A\n\nbody")]).await;

        let error = store
            .search_notes("owner", "default", search("   "))
            .await
            .expect_err("an empty query must not scan the space");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// Search sees only what the Observe boundary admits, so the runtime
    /// directories sharing a notes root stay out of an owner's results.
    #[tokio::test]
    async fn search_does_not_reach_outside_the_admitted_roots() {
        let temp = tempfile::tempdir().unwrap();
        let store = store_with_notes(&temp, &[("Inbox/real.md", "# Real\n\nsecret sauce")]).await;

        let root = temp.path().join("scopes/owner/default/notes/space");
        fs::create_dir_all(root.join(".silverbullet"))
            .await
            .unwrap();
        fs::write(
            root.join(".silverbullet/private.md"),
            b"# Hidden\n\nsecret sauce",
        )
        .await
        .unwrap();
        fs::write(root.join("Inbox/notes.txt"), b"secret sauce")
            .await
            .unwrap();

        let results = store
            .search_notes("owner", "default", search("secret sauce"))
            .await
            .unwrap();

        let paths = results
            .hits
            .iter()
            .map(|hit| hit.relative_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["Inbox/real.md"]);
    }

    #[tokio::test]
    async fn observation_catalog_and_discovery_are_scoped_revisioned_and_paged() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        for (path, body) in [
            ("Inbox/alpha.md", "# Alpha\n\nFirst observation body."),
            (
                "Projects/beta.markdown",
                "# Beta\n\nSecond observation body.",
            ),
        ] {
            store
                .write_note_markdown(
                    "owner",
                    "default",
                    WriteNoteMarkdownRequest {
                        provider: Some("local_markdown".into()),
                        target_path: path.into(),
                        markdown: body.into(),
                    },
                )
                .await
                .unwrap();
        }
        let root = temp.path().join("scopes/owner/default/notes/space");
        fs::write(root.join("Inbox/ignored.txt"), b"not a note")
            .await
            .unwrap();
        fs::create_dir_all(root.join(".silverbullet"))
            .await
            .unwrap();
        fs::write(root.join(".silverbullet/private.md"), b"# Internal")
            .await
            .unwrap();

        let catalog = store.observation_catalog("owner", "default").await.unwrap();
        assert!(catalog.enabled);
        assert_eq!(catalog.providers, vec!["local_markdown"]);
        assert_eq!(catalog.source_revision.len(), 64);

        let first = store
            .discover_observation_notes("owner", "default", 0, 1)
            .await
            .unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.next_offset, Some(1));
        let second = store
            .discover_observation_notes("owner", "default", first.next_offset.unwrap(), 1)
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.next_offset, None);
        let paths = [
            first.items[0].relative_path.as_str(),
            second.items[0].relative_path.as_str(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(
            paths,
            BTreeSet::from(["Inbox/alpha.md", "Projects/beta.markdown"])
        );
        assert!(first.items[0]
            .source_ref
            .starts_with("notes:local_markdown:"));
        assert!(first.items[0]
            .open_url
            .as_deref()
            .is_some_and(|url| url.starts_with("file://")));
    }

    #[tokio::test]
    async fn observation_discovery_tracks_content_revision_without_changing_source_revision() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let request = |markdown: &str| WriteNoteMarkdownRequest {
            provider: Some("local_markdown".into()),
            target_path: "Inbox/changing.md".into(),
            markdown: markdown.into(),
        };
        store
            .write_note_markdown("owner", "default", request("# Changing\n\nVersion one"))
            .await
            .unwrap();
        let catalog_before = store.observation_catalog("owner", "default").await.unwrap();
        let before = store
            .discover_observation_notes("owner", "default", 0, 10)
            .await
            .unwrap()
            .items
            .remove(0);

        store
            .write_note_markdown("owner", "default", request("# Changing\n\nVersion two"))
            .await
            .unwrap();
        let catalog_after = store.observation_catalog("owner", "default").await.unwrap();
        let after = store
            .discover_observation_notes("owner", "default", 0, 10)
            .await
            .unwrap()
            .items
            .remove(0);

        assert_eq!(before.source_ref, after.source_ref);
        assert_ne!(before.content_hash, after.content_hash);
        assert_eq!(
            catalog_before.source_revision,
            catalog_after.source_revision
        );
    }

    #[tokio::test]
    async fn disabled_notes_remain_visible_but_fail_closed_for_observation_reads() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let mut settings = NotesSettings::default();
        settings.enabled = false;
        store.save("owner", "default", settings).await.unwrap();

        let catalog = store.observation_catalog("owner", "default").await.unwrap();
        assert!(!catalog.enabled);
        let error = store
            .discover_observation_notes("owner", "default", 0, 10)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    fn sample_publishable_task() -> TaskRecord {
        let primary = OutputRef {
            output_id: "out-user-final".to_string(),
            scope: "task".to_string(),
            audience: "user".to_string(),
            role: "primary_task_user".to_string(),
            relative_path: "outputs/final.md".to_string(),
            media_type: "text/markdown".to_string(),
            created_at: "2026-08-01T10:15:00Z".to_string(),
            source_execution_id: Some("exec-1".to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let image = OutputRef {
            output_id: "out-user-image".to_string(),
            scope: "task".to_string(),
            audience: "user".to_string(),
            role: "user_media".to_string(),
            relative_path: "outputs/result.png".to_string(),
            media_type: "image/png".to_string(),
            created_at: "2026-08-01T10:16:00Z".to_string(),
            source_execution_id: Some("exec-1".to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        TaskRecord {
            manifest: TaskManifest {
                task_id: "task-note-1".to_string(),
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
                title: "Research launch plan".to_string(),
                description: "Build the launch brief.".to_string(),
                agent_id: "research-agent".to_string(),
                goal_id: None,
                ui_thread_id: "thread-9".to_string(),
                priority: Some("p2".to_string()),
                due_date: Some("2026-08-03T17:30:00+05:30".to_string()),
                tags: vec![TaskTagRecord {
                    id: "tag-1".to_string(),
                    name: "Launch Plan".to_string(),
                    color: None,
                }],
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: TaskLifecycle::Persistent,
                sync_mode: TaskSyncMode::Deferred,
                monitor_spec: None,
                monitor_revision: 0,
                created_at: "2026-08-01T09:00:00Z".to_string(),
                updated_at: "2026-08-01T10:17:00Z".to_string(),
            },
            state: TaskState {
                task_id: "task-note-1".to_string(),
                status: "completed".to_string(),
                completion_kind: None,
                open_items: Vec::new(),
                active_root_execution_id: None,
                latest_root_execution_id: Some("exec-1".to_string()),
                last_completed_root_execution_id: Some("exec-1".to_string()),
                default_task_agent_output_id: None,
                primary_user_output_id: Some(primary.output_id.clone()),
                schedule_fire_count: 0,
                synthesis_pending_executions: Vec::new(),
                synthesis_failed_execution_id: None,
                monitor_cursor: None,
                last_progress_at: Some("2026-08-01T10:16:30Z".to_string()),
                updated_at: "2026-08-01T10:17:00Z".to_string(),
            },
            refs: TaskRefs {
                task_id: "task-note-1".to_string(),
                outputs: vec![primary.clone(), image],
                default_task_agent_output_id: None,
                primary_user_output_id: Some(primary.output_id),
                updated_at: Some("2026-08-01T10:17:00Z".to_string()),
            },
        }
    }

    async fn write_sample_task_outputs(store: &NotesSettingsStore, task: &TaskRecord) {
        let task_dir = store.workspace_layout.task_dir(
            &task.manifest.principal,
            &task.manifest.workspace,
            &task.manifest.task_id,
        );
        fs::create_dir_all(task_dir.join("outputs")).await.unwrap();
        fs::write(
            task_dir.join("outputs/final.md"),
            b"The launch brief is ready with three validated channels.",
        )
        .await
        .unwrap();
        fs::write(task_dir.join("outputs/result.png"), b"png-test-bytes")
            .await
            .unwrap();

        let execution = ExecutionIndexEntry {
            execution_id: "exec-1".to_string(),
            task_id: task.manifest.task_id.clone(),
            root_execution_id: Some("exec-1".to_string()),
            parent_execution_id: None,
            agent_id: task.manifest.agent_id.clone(),
            relationship_type: "root".to_string(),
            status: "completed".to_string(),
            completion_kind: None,
            open_items: Vec::new(),
            plan_id: Some("plan-1".to_string()),
            primary_execution_output_id: None,
            active_child_execution_ids: Vec::new(),
            started_at: "2026-08-01T10:00:00Z".to_string(),
            completed_at: Some("2026-08-01T10:16:59Z".to_string()),
            updated_at: "2026-08-01T10:16:59Z".to_string(),
        };
        store
            .workspace_layout
            .write_jsonl_records_atomic_path(
                store.workspace_layout.task_executions_index_path(
                    &task.manifest.principal,
                    &task.manifest.workspace,
                    &task.manifest.task_id,
                ),
                std::slice::from_ref(&execution),
            )
            .await
            .unwrap();
        store
            .workspace_layout
            .write_json_atomic_path(
                store.workspace_layout.execution_state_path(
                    &task.manifest.principal,
                    &task.manifest.workspace,
                    &task.manifest.task_id,
                    &execution.execution_id,
                ),
                &ExecutionState {
                    execution_id: execution.execution_id.clone(),
                    task_id: execution.task_id.clone(),
                    root_execution_id: execution.root_execution_id.clone(),
                    parent_execution_id: None,
                    agent_id: execution.agent_id.clone(),
                    relationship_type: execution.relationship_type.clone(),
                    status: execution.status.clone(),
                    completion_kind: None,
                    open_items: Vec::new(),
                    plan_id: execution.plan_id.clone(),
                    primary_execution_output_id: None,
                    active_child_execution_ids: Vec::new(),
                    started_at: execution.started_at.clone(),
                    completed_at: execution.completed_at.clone(),
                    updated_at: execution.updated_at.clone(),
                    completed_step_ids: vec!["step-research".to_string()],
                    failed_step_ids: Vec::new(),
                    current_step_id: None,
                    task_output_mode: TaskOutputMode::Accumulate,
                    refinement: None,
                    synthesis_pending: false,
                    synthesis_failed: None,
                },
            )
            .await
            .unwrap();
        store
            .workspace_layout
            .write_json_atomic_path(
                store.workspace_layout.execution_schedule_path(
                    &task.manifest.principal,
                    &task.manifest.workspace,
                    &task.manifest.task_id,
                    &execution.execution_id,
                ),
                &ExecutionScheduleRecord {
                    execution_id: execution.execution_id,
                    task_id: execution.task_id,
                    plan_id: execution.plan_id,
                    source_plan_relative_path: Some("plans/plan-1.json".to_string()),
                    source_kind: "runtime_context".to_string(),
                    updated_at: "2026-08-01T10:16:59Z".to_string(),
                    steps: vec![ExecutionScheduleStep {
                        step_id: "step-research".to_string(),
                        title: "Validate launch channels".to_string(),
                        order: 0,
                        depends_on_step_ids: Vec::new(),
                        capability: Some("research".to_string()),
                        delegate_agent_id: None,
                        taskplan_status: "completed".to_string(),
                        taskplan_progress: "3 channels validated".to_string(),
                        sub_step_labels: Vec::new(),
                        recipe: None,
                    }],
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn task_projection_is_dated_tagged_idempotent_and_asset_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let task = sample_publishable_task();
        write_sample_task_outputs(&store, &task).await;

        let first = store
            .publish_task_note(
                "anonymous",
                "default",
                &task,
                PublishTaskNoteRequest::default(),
            )
            .await
            .unwrap();
        let second = store
            .publish_task_note(
                "anonymous",
                "default",
                &task,
                PublishTaskNoteRequest::default(),
            )
            .await
            .unwrap();

        assert_eq!(first.note_path, second.note_path);
        assert_eq!(first.date, "2026-08-01");
        assert!(first.tags.contains(&"magician/task".to_string()));
        assert!(first.tags.contains(&"task-tag/launch-plan".to_string()));
        assert!(first.tags.contains(&"agent/research-agent".to_string()));
        assert!(first.tags.contains(&"task/completed".to_string()));
        assert!(first.tags.contains(&"lifecycle/persistent".to_string()));
        assert!(first.tags.contains(&"priority/p2".to_string()));
        assert!(first.tags.contains(&"due/2026-08-03".to_string()));
        assert_eq!(first.assets.len(), 1);
        assert_eq!(
            first.task_due_date.as_deref(),
            Some("2026-08-03T17:30:00+05:30")
        );
        assert_eq!(
            first.task_completed_at.as_deref(),
            Some("2026-08-01T10:16:59Z")
        );
        let root = PathBuf::from(
            store
                .load_envelope("anonymous", "default")
                .await
                .unwrap()
                .resolved
                .local_markdown_root,
        );
        let markdown = fs::read_to_string(root.join(&first.note_path))
            .await
            .unwrap();
        assert!(markdown.contains("magician_schema: \"magician.task-note.v1\""));
        assert!(markdown.contains("principal: \"anonymous\""));
        assert!(markdown.contains("workspace: \"default\""));
        assert!(markdown.contains("lifecycle: \"persistent\""));
        assert!(markdown.contains("date: \"2026-08-01\""));
        assert!(markdown.contains("due_date: \"2026-08-03T17:30:00+05:30\""));
        assert!(markdown.contains("completed_at: \"2026-08-01T10:16:59Z\""));
        assert!(markdown.contains("## Final answer"));
        assert!(markdown.contains("three validated channels"));
        assert!(markdown.contains("Validate launch channels"));
        assert!(markdown.contains("3 channels validated"));
        assert!(markdown.contains("## Assets"));
        let page = store
            .list_task_notes("anonymous", "default", 0, 10, Some("launch-plan"))
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].task_id, task.manifest.task_id);

        let mut older_task = task.clone();
        older_task.manifest.task_id = "task-note-older".to_string();
        older_task.manifest.title = "Older completed launch task".to_string();
        older_task.state.task_id = older_task.manifest.task_id.clone();
        older_task.state.last_completed_root_execution_id = None;
        older_task.state.updated_at = "2026-07-01T10:17:00Z".to_string();
        write_sample_task_outputs(&store, &older_task).await;
        store
            .publish_task_note(
                "anonymous",
                "default",
                &older_task,
                PublishTaskNoteRequest::default(),
            )
            .await
            .unwrap();
        let ordered = store
            .list_task_notes("anonymous", "default", 0, 10, None)
            .await
            .unwrap();
        assert_eq!(ordered.items[0].task_id, task.manifest.task_id);
        assert_eq!(ordered.items[1].task_id, older_task.manifest.task_id);

        fs::remove_file(root.join(&first.note_path)).await.unwrap();
        assert!(!store
            .task_note_is_available("anonymous", "default", &task.manifest.task_id)
            .await
            .unwrap());
        let repaired = store
            .publish_task_note(
                "anonymous",
                "default",
                &task,
                PublishTaskNoteRequest::default(),
            )
            .await
            .unwrap();
        assert_eq!(repaired.note_path, first.note_path);
        assert!(store
            .task_note_is_available("anonymous", "default", &task.manifest.task_id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn compact_and_diagnostic_modes_keep_their_disclosure_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let task = sample_publishable_task();
        write_sample_task_outputs(&store, &task).await;

        let compact = store
            .publish_task_note(
                "anonymous",
                "default",
                &task,
                PublishTaskNoteRequest {
                    mode: Some(TaskNotePublishMode::Compact),
                    include_assets: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let root = PathBuf::from(
            store
                .load_envelope("anonymous", "default")
                .await
                .unwrap()
                .resolved
                .local_markdown_root,
        );
        let compact_body = fs::read_to_string(root.join(&compact.note_path))
            .await
            .unwrap();
        assert!(!compact_body.contains("## Important outputs"));
        assert!(!compact_body.contains("Diagnostic projection explicitly requested"));

        let diagnostic = store
            .publish_task_note(
                "anonymous",
                "default",
                &task,
                PublishTaskNoteRequest {
                    mode: Some(TaskNotePublishMode::Diagnostic),
                    include_assets: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(diagnostic
            .assets
            .iter()
            .any(|asset| asset.source_output_id == "diagnostic:task_record"));
        let diagnostic_body = fs::read_to_string(root.join(&diagnostic.note_path))
            .await
            .unwrap();
        assert!(diagnostic_body.contains("Diagnostic projection explicitly requested"));
    }

    #[tokio::test]
    async fn automatic_task_projection_is_opt_in_and_waits_for_settled_completion() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let task = sample_publishable_task();
        write_sample_task_outputs(&store, &task).await;

        assert!(store
            .auto_publish_completed_task("anonymous", "default", &task)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .read_task_note("anonymous", "default", &task.manifest.task_id)
            .await
            .unwrap()
            .is_none());

        store
            .save(
                "anonymous",
                "default",
                NotesSettings {
                    task_publishing: TaskPublishingSettings {
                        auto_publish_completed: true,
                        ..TaskPublishingSettings::default()
                    },
                    ..NotesSettings::default()
                },
            )
            .await
            .unwrap();

        let mut pending = task.clone();
        pending
            .state
            .synthesis_pending_executions
            .push("exec-1".to_string());
        assert!(store
            .auto_publish_completed_task("anonymous", "default", &pending)
            .await
            .unwrap()
            .is_none());

        let mut synthesis_failed = task.clone();
        synthesis_failed.state.synthesis_failed_execution_id = Some("exec-1".to_string());
        assert!(store
            .auto_publish_completed_task("anonymous", "default", &synthesis_failed)
            .await
            .unwrap()
            .is_none());

        let published = store
            .auto_publish_completed_task("anonymous", "default", &task)
            .await
            .unwrap()
            .expect("settled completed task should publish when enabled");
        assert_eq!(published.task_id, task.manifest.task_id);
    }

    #[tokio::test]
    async fn task_projection_rejects_non_terminal_task_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let mut task = sample_publishable_task();
        task.state.status = "running".to_string();

        let error = store
            .publish_task_note(
                "anonymous",
                "default",
                &task,
                PublishTaskNoteRequest::default(),
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("only completed, failed, or cancelled"));
    }

    #[test]
    fn silverbullet_default_space_is_a_strict_child_of_the_magician_root() {
        assert_eq!(
            resolve_default_silverbullet_space_path(
                None,
                Some(OsString::from("/runtime-root")),
                Some(OsString::from("/legacy-root")),
                Some(OsString::from("/home")),
                "alice",
                "work",
            ),
            PathBuf::from("/runtime-root/MagicanNotes/spaces/alice/work")
        );
        assert_eq!(
            resolve_default_silverbullet_space_path(
                None,
                None,
                Some(OsString::from("/legacy-root")),
                Some(OsString::from("/home")),
                "alice",
                "work",
            ),
            PathBuf::from("/legacy-root/MagicanNotes/spaces/alice/work")
        );
        assert_eq!(
            resolve_default_silverbullet_space_path(
                Some(OsString::from("/explicit-forest")),
                Some(OsString::from("/runtime-root")),
                None,
                None,
                "alice",
                "work",
            ),
            PathBuf::from("/explicit-forest/spaces/alice/work")
        );
        assert_eq!(
            resolve_default_silverbullet_space_path(
                Some(OsString::new()),
                Some(OsString::from("/runtime-root")),
                None,
                None,
                "alice",
                "work",
            ),
            PathBuf::from("/runtime-root/MagicanNotes/spaces/alice/work")
        );
    }

    #[test]
    fn silverbullet_default_space_is_per_workspace() {
        let home = resolve_default_silverbullet_space_path(
            None,
            Some(OsString::from("/runtime-root")),
            None,
            None,
            "alice",
            "home",
        );
        let work = resolve_default_silverbullet_space_path(
            None,
            Some(OsString::from("/runtime-root")),
            None,
            None,
            "alice",
            "work",
        );
        let bob = resolve_default_silverbullet_space_path(
            None,
            Some(OsString::from("/runtime-root")),
            None,
            None,
            "bob",
            "home",
        );
        assert_eq!(
            home,
            PathBuf::from("/runtime-root/MagicanNotes/spaces/alice/home")
        );
        assert_ne!(home, work);
        assert_ne!(home, bob);
    }

    #[tokio::test]
    async fn default_silverbullet_roots_do_not_cross_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let alice = store
            .load_envelope("alice", "home")
            .await
            .expect("alice envelope");
        let bob = store
            .load_envelope("bob", "home")
            .await
            .expect("bob envelope");
        assert!(alice
            .resolved
            .silverbullet_space_path
            .ends_with("spaces/alice/home"));
        assert!(bob
            .resolved
            .silverbullet_space_path
            .ends_with("spaces/bob/home"));
        assert_ne!(
            alice.resolved.silverbullet_space_path,
            bob.resolved.silverbullet_space_path
        );
    }

    #[tokio::test]
    async fn forest_space_path_resolves_to_the_workspace_child() {
        let temp = tempfile::tempdir().unwrap();
        let forest = temp.path().join("MagicanNotes");
        fs::create_dir_all(forest.join("spaces/alice/work"))
            .await
            .unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let envelope = store
            .save(
                "alice",
                "work",
                NotesSettings {
                    enabled: true,
                    default_provider: "silverbullet".to_string(),
                    fallback_provider: "local_markdown".to_string(),
                    local_markdown: LocalMarkdownNotesSettings::default(),
                    silverbullet: SilverBulletNotesSettings {
                        space_path: Some(forest.display().to_string()),
                        local_url: default_silverbullet_local_url(),
                        server_url: default_silverbullet_server_url(),
                        public_origin: None,
                    },
                    task_publishing: TaskPublishingSettings::default(),
                },
            )
            .await
            .unwrap();
        let expected = forest.join("spaces/alice/work");
        assert_eq!(
            PathBuf::from(&envelope.resolved.silverbullet_space_path),
            expected
        );
        assert_eq!(
            envelope.settings.silverbullet.space_path.as_deref(),
            Some(expected.to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn silverbullet_creates_a_missing_workspace_root_on_write() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let space = temp.path().join("spaces/alice/work");
        store
            .save(
                "alice",
                "work",
                NotesSettings {
                    enabled: true,
                    default_provider: "silverbullet".to_string(),
                    fallback_provider: "local_markdown".to_string(),
                    local_markdown: LocalMarkdownNotesSettings::default(),
                    silverbullet: SilverBulletNotesSettings {
                        space_path: Some(space.display().to_string()),
                        local_url: default_silverbullet_local_url(),
                        server_url: default_silverbullet_server_url(),
                        public_origin: None,
                    },
                    task_publishing: TaskPublishingSettings::default(),
                },
            )
            .await
            .unwrap();

        assert!(!space.exists());
        let note_ref = store
            .create_note(
                "alice",
                "work",
                CreateNoteRequest {
                    title: "Scoped".to_string(),
                    body: "only this workspace".to_string(),
                    provider: Some("silverbullet".to_string()),
                    target_dir: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(note_ref.provider, "silverbullet");
        assert!(!note_ref.used_fallback);
        assert!(PathBuf::from(&note_ref.absolute_path).starts_with(&space));
        assert!(PathBuf::from(note_ref.absolute_path).is_file());
    }

    #[test]
    fn forest_pin_coerce_only_follows_the_spaces_layout() {
        let forest = PathBuf::from("/runtime/MagicanNotes");
        let scoped = forest.join("spaces/alice/work");
        assert_eq!(
            coerce_forest_pin_to_workspace_space(forest.clone(), "alice", "work", scoped.clone(),),
            scoped
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                forest.join("spaces"),
                "alice",
                "work",
                scoped.clone(),
            ),
            scoped
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                forest.join("spaces/alice"),
                "alice",
                "work",
                scoped.clone(),
            ),
            scoped
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(scoped.clone(), "alice", "work", scoped.clone()),
            scoped
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                PathBuf::from("/runtime"),
                "alice",
                "work",
                scoped.clone(),
            ),
            PathBuf::from("/runtime")
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                PathBuf::from("/"),
                "alice",
                "work",
                scoped.clone(),
            ),
            PathBuf::from("/")
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                PathBuf::from("/tmp/my-notes"),
                "alice",
                "work",
                scoped,
            ),
            PathBuf::from("/tmp/my-notes")
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                PathBuf::from("/other/spaces"),
                "alice",
                "work",
                PathBuf::from("/runtime/MagicanNotes/spaces/alice/work"),
            ),
            PathBuf::from("/other/spaces/alice/work")
        );
        assert_eq!(
            coerce_forest_pin_to_workspace_space(
                PathBuf::from("/other/spaces/alice"),
                "alice",
                "work",
                PathBuf::from("/runtime/MagicanNotes/spaces/alice/work"),
            ),
            PathBuf::from("/other/spaces/alice/work")
        );
    }

    fn silverbullet_settings_for_space(space: &Path) -> NotesSettings {
        NotesSettings {
            enabled: true,
            default_provider: "silverbullet".to_string(),
            fallback_provider: "local_markdown".to_string(),
            local_markdown: LocalMarkdownNotesSettings::default(),
            silverbullet: SilverBulletNotesSettings {
                space_path: Some(space.display().to_string()),
                local_url: default_silverbullet_local_url(),
                server_url: default_silverbullet_server_url(),
                public_origin: None,
            },
            task_publishing: TaskPublishingSettings::default(),
        }
    }

    #[tokio::test]
    async fn forest_pin_search_does_not_cross_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        let forest = temp.path().join("MagicanNotes");
        let alice_space = forest.join("spaces/alice/work");
        let bob_space = forest.join("spaces/bob/home");
        fs::create_dir_all(&alice_space).await.unwrap();
        fs::create_dir_all(&bob_space).await.unwrap();
        fs::write(
            alice_space.join("alice-only.md"),
            "# Alice\nalice-unique-token\n",
        )
        .await
        .unwrap();
        fs::write(bob_space.join("bob-only.md"), "# Bob\nbob-unique-token\n")
            .await
            .unwrap();

        let store = NotesSettingsStore::new(temp.path());
        store
            .save("alice", "work", silverbullet_settings_for_space(&forest))
            .await
            .unwrap();
        store
            .save("bob", "home", silverbullet_settings_for_space(&forest))
            .await
            .unwrap();

        let leaked = store
            .search_notes("alice", "work", search("bob-unique-token"))
            .await
            .unwrap();
        assert!(
            leaked.hits.is_empty(),
            "alice search must not see bob's notes: {:?}",
            leaked
                .hits
                .iter()
                .map(|hit| hit.relative_path.as_str())
                .collect::<Vec<_>>()
        );
        let own = store
            .search_notes("alice", "work", search("alice-unique-token"))
            .await
            .unwrap();
        assert_eq!(own.hits.len(), 1);
        assert_eq!(own.hits[0].relative_path, "alice-only.md");
        let bob_leaked = store
            .search_notes("bob", "home", search("alice-unique-token"))
            .await
            .unwrap();
        assert!(bob_leaked.hits.is_empty());
    }

    #[tokio::test]
    async fn forest_pin_search_does_not_see_a_sibling_when_this_workspace_dir_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let forest = temp.path().join("MagicanNotes");
        let alice_space = forest.join("spaces/alice/work");
        fs::create_dir_all(&alice_space).await.unwrap();
        fs::write(
            alice_space.join("alice-only.md"),
            "# Alice\nalice-unique-token\n",
        )
        .await
        .unwrap();

        let store = NotesSettingsStore::new(temp.path());
        let settings_path = temp.path().join("scopes/bob/home/notes/settings.json");
        fs::create_dir_all(settings_path.parent().unwrap())
            .await
            .unwrap();
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&silverbullet_settings_for_space(&forest)).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            !forest.join("spaces/bob/home").exists(),
            "bob's workspace Space must stay uncreated so search cannot rely on an empty folder"
        );
        let leaked = store
            .search_notes("bob", "home", search("alice-unique-token"))
            .await
            .unwrap();
        assert!(
            leaked.hits.is_empty(),
            "bob must not see alice's notes through a forest pin: {:?}",
            leaked
                .hits
                .iter()
                .map(|hit| hit.relative_path.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn custom_space_with_inbox_layout_is_not_nested_under_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("custom-space");
        fs::create_dir_all(space.join("Inbox/2026-08-31"))
            .await
            .unwrap();
        fs::write(
            space.join("Inbox/2026-08-31/note.md"),
            "# Keep\nvisible-here\n",
        )
        .await
        .unwrap();
        let store = NotesSettingsStore::new(temp.path());
        store
            .save("alice", "work", silverbullet_settings_for_space(&space))
            .await
            .unwrap();
        let envelope = store.load_envelope("alice", "work").await.unwrap();
        assert_eq!(
            PathBuf::from(&envelope.resolved.silverbullet_space_path),
            space
        );
        let results = store
            .search_notes("alice", "work", search("visible-here"))
            .await
            .unwrap();
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].relative_path, "Inbox/2026-08-31/note.md");
    }

    #[tokio::test]
    async fn custom_space_with_a_spaces_folder_is_not_treated_as_a_forest() {
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("my-notes");
        fs::create_dir_all(space.join("spaces")).await.unwrap();
        fs::write(space.join("keep.md"), "# Keep\nstill-visible\n")
            .await
            .unwrap();
        let store = NotesSettingsStore::new(temp.path());
        store
            .save("alice", "work", silverbullet_settings_for_space(&space))
            .await
            .unwrap();
        let envelope = store.load_envelope("alice", "work").await.unwrap();
        assert_eq!(
            PathBuf::from(&envelope.resolved.silverbullet_space_path),
            space
        );
        let results = store
            .search_notes("alice", "work", search("still-visible"))
            .await
            .unwrap();
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].relative_path, "keep.md");
    }

    #[tokio::test]
    async fn custom_space_with_nested_spaces_layout_keeps_root_notes() {
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("my-notes");
        fs::create_dir_all(space.join("spaces/projects/work"))
            .await
            .unwrap();
        fs::write(space.join("keep.md"), "# Keep\nstill-visible\n")
            .await
            .unwrap();
        let store = NotesSettingsStore::new(temp.path());
        store
            .save("alice", "work", silverbullet_settings_for_space(&space))
            .await
            .unwrap();
        let envelope = store.load_envelope("alice", "work").await.unwrap();
        assert_eq!(
            PathBuf::from(&envelope.resolved.silverbullet_space_path),
            space
        );
        let results = store
            .search_notes("alice", "work", search("still-visible"))
            .await
            .unwrap();
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].relative_path, "keep.md");
    }

    #[tokio::test]
    async fn root_space_path_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let error = store
            .save(
                "alice",
                "work",
                silverbullet_settings_for_space(Path::new("/")),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("HOME or /"));
    }

    #[test]
    fn non_markdown_projection_content_uses_a_non_colliding_code_fence() {
        let rendered = markdown_code_block("html", "<script>x()</script>\n```nested```");
        assert!(rendered.starts_with("````html\n"));
        assert!(rendered.ends_with("\n````"));
    }

    #[test]
    fn safe_relative_path_rejects_parent_escape() {
        assert!(safe_relative_path("../x.md").is_err());
        assert!(safe_relative_path("/tmp/x.md").is_err());
        assert!(safe_relative_path("Inbox/x.md").is_ok());
    }

    #[test]
    fn provider_aliases_normalize() {
        assert_eq!(
            normalize_provider_id("local").as_deref(),
            Some("local_markdown")
        );
        assert_eq!(normalize_provider_id("sb").as_deref(), Some("silverbullet"));
    }

    #[test]
    fn audio_note_sort_uses_absolute_capture_time_across_time_zones() {
        let entry = |note_id: &str, captured_at: &str| AudioNoteIndexEntry {
            note_id: note_id.to_string(),
            requested_provider: "local_markdown".to_string(),
            provider: "local_markdown".to_string(),
            used_fallback: false,
            fallback_reason: None,
            captured_at: captured_at.to_string(),
            source_surface: "ios_voice_note".to_string(),
            transcript: None,
            duration_ms: None,
            mime_type: "audio/m4a".to_string(),
            note_path: "Audio Notes/note.md".to_string(),
            audio_path: "Audio Notes/note.m4a".to_string(),
            bytes: 1,
            content_hash: "blake3:test".to_string(),
        };
        let actually_newer = entry(
            "11111111-1111-4111-8111-111111111111",
            "2026-08-01T23:30:00-07:00",
        );
        let lexically_newer = entry(
            "22222222-2222-4222-8222-222222222222",
            "2026-08-02T05:00:00+00:00",
        );

        assert_eq!(
            audio_note_newest_first(&actually_newer, &lexically_newer),
            std::cmp::Ordering::Less
        );
    }

    #[tokio::test]
    async fn create_note_uses_scoped_default_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let note_ref = store
            .create_note(
                "anonymous",
                "default",
                CreateNoteRequest {
                    title: "Hello Notes".to_string(),
                    body: "Saved body".to_string(),
                    provider: None,
                    target_dir: None,
                },
            )
            .await
            .unwrap();

        let expected = temp
            .path()
            .join("scopes/anonymous/default/notes/space/Inbox/hello-notes.md");
        assert_eq!(note_ref.requested_provider, "local_markdown");
        assert_eq!(note_ref.provider, "local_markdown");
        assert!(!note_ref.used_fallback);
        assert!(note_ref.fallback_reason.is_none());
        assert_eq!(note_ref.path, "Inbox/hello-notes.md");
        assert_eq!(PathBuf::from(note_ref.absolute_path), expected);
        assert!(note_ref
            .open_url
            .as_deref()
            .unwrap_or_default()
            .starts_with("file://"));
        let content = fs::read_to_string(&expected).await.unwrap();
        assert!(content.contains("# Hello Notes"));
        assert!(content.contains("Saved body"));
        assert!(!temp
            .path()
            .join("scopes/anonymous/default/notes/space/.magician/provider-state")
            .exists());
    }

    #[tokio::test]
    async fn writable_probe_leaves_no_provider_state_scaffold() {
        let temp = tempfile::tempdir().unwrap();

        assert!(writable_probe(temp.path()).await);
        assert!(!temp.path().join(".magician/provider-state").exists());
        assert!(!temp.path().join(".magician").exists());
    }

    #[tokio::test]
    async fn provider_status_decides_writability_without_touching_the_space() {
        let temp = tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(temp.path()).unwrap().count();

        let status =
            status_for_provider("local_markdown", "Local Markdown", temp.path(), true, false).await;

        assert!(status.available);
        assert!(status.writable);
        assert_eq!(status.message, "ready");

        // The whole point: a GET status must not mutate the owner's Space.
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            before,
            "reading provider status created something"
        );
        assert!(!temp.path().join(".magician").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_writable_directory_still_reads_as_not_writable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        // Root ignores the permission bits, so skip rather than assert a
        // falsehood when the suite happens to run privileged.
        if unsafe { libc::geteuid() } != 0 {
            assert!(!directory_is_writable(&locked).await);
        }

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(directory_is_writable(&locked).await);
    }

    #[test]
    fn silverbullet_open_url_uses_page_paths_for_markdown() {
        assert_eq!(
            silverbullet_open_url(
                "http://127.0.0.1:3021/",
                Path::new("Tasks/2026-06-20/task_projection - price-check.md")
            )
            .as_deref(),
            Some("http://127.0.0.1:3021/Tasks/2026-06-20/task_projection%20-%20price-check")
        );
        assert_eq!(
            silverbullet_open_url(
                "http://127.0.0.1:3021",
                Path::new("Tasks/2026-06-20/task_projection.assets/result.json")
            )
            .as_deref(),
            Some("http://127.0.0.1:3021/Tasks/2026-06-20/task_projection.assets/result.json")
        );
    }

    #[tokio::test]
    async fn public_origin_wins_for_links_while_local_url_remains_loopback() {
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("visible-notes");
        fs::create_dir_all(&space).await.unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let envelope = store
            .save(
                "anonymous",
                "default",
                NotesSettings {
                    enabled: true,
                    default_provider: "silverbullet".to_string(),
                    fallback_provider: "local_markdown".to_string(),
                    local_markdown: LocalMarkdownNotesSettings::default(),
                    silverbullet: SilverBulletNotesSettings {
                        space_path: Some(space.display().to_string()),
                        local_url: "http://127.0.0.1:3021/".to_string(),
                        server_url: "http://127.0.0.1:3021".to_string(),
                        public_origin: Some("https://notes.example.com/".to_string()),
                    },
                    task_publishing: TaskPublishingSettings::default(),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            envelope.resolved.silverbullet_local_url,
            "http://127.0.0.1:3021"
        );
        assert_eq!(
            envelope.resolved.silverbullet_public_origin.as_deref(),
            Some("https://notes.example.com")
        );
        assert_eq!(
            envelope.resolved.silverbullet_server_url,
            "https://notes.example.com"
        );
    }

    #[tokio::test]
    async fn silverbullet_space_can_be_a_dedicated_runtime_subfolder_but_not_serve_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let mut settings = NotesSettings::default();

        settings.silverbullet.space_path = Some(temp.path().display().to_string());
        let equal_error = store
            .save("anonymous", "default", settings.clone())
            .await
            .unwrap_err();
        assert_eq!(equal_error.kind(), std::io::ErrorKind::InvalidInput);

        settings.silverbullet.space_path =
            temp.path().parent().map(|path| path.display().to_string());
        let parent_error = store
            .save("anonymous", "default", settings.clone())
            .await
            .unwrap_err();
        assert_eq!(parent_error.kind(), std::io::ErrorKind::InvalidInput);

        let nested_space = temp
            .path()
            .join("Notes")
            .join("..")
            .join("Notes")
            .display()
            .to_string();
        settings.silverbullet.space_path = Some(nested_space.clone());
        let saved = store.save("anonymous", "default", settings).await.unwrap();
        assert!(saved.resolved.silverbullet_write_safe);
        assert_eq!(
            saved.settings.silverbullet.space_path.as_deref(),
            Some(nested_space.as_str()),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn silverbullet_space_boundary_resolves_symlinks_before_parent_components() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let links = temp.path().join("links");
        let child = temp.path().join("child");
        fs::create_dir_all(&links).await.unwrap();
        fs::create_dir_all(&child).await.unwrap();
        symlink(&child, links.join("alias")).unwrap();

        let store = NotesSettingsStore::new(temp.path());
        let mut settings = NotesSettings::default();
        // Lexically this resembles <runtime>/links. The OS resolves `alias`
        // first, making alias/.. exactly the canonical runtime root.
        settings.silverbullet.space_path =
            Some(links.join("alias").join("..").display().to_string());
        let error = store
            .save("anonymous", "default", settings)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn legacy_unsafe_silverbullet_space_falls_back_without_rewriting_settings() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let settings_path = store.settings_path("anonymous", "default").unwrap();
        fs::create_dir_all(settings_path.parent().unwrap())
            .await
            .unwrap();
        let settings = NotesSettings {
            default_provider: "silverbullet".to_string(),
            fallback_provider: "local_markdown".to_string(),
            silverbullet: SilverBulletNotesSettings {
                space_path: Some(temp.path().display().to_string()),
                ..SilverBulletNotesSettings::default()
            },
            ..NotesSettings::default()
        };
        fs::write(&settings_path, serde_json::to_vec(&settings).unwrap())
            .await
            .unwrap();

        let envelope = store.load_envelope("anonymous", "default").await.unwrap();
        assert!(!envelope.resolved.silverbullet_write_safe);
        assert!(envelope
            .warnings
            .iter()
            .any(|warning| warning.contains("would expose canonical runtime storage")));
        let original_space_path = temp.path().to_string_lossy().to_string();
        assert_eq!(
            envelope.settings.silverbullet.space_path.as_deref(),
            Some(original_space_path.as_str()),
        );

        let note = store
            .create_note(
                "anonymous",
                "default",
                CreateNoteRequest {
                    title: "Safe fallback".to_string(),
                    body: "legacy roots stay inspectable".to_string(),
                    provider: None,
                    target_dir: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(note.requested_provider, "silverbullet");
        assert_eq!(note.provider, "local_markdown");
        assert!(note.used_fallback);
    }

    #[tokio::test]
    async fn settings_reject_public_or_non_loopback_listener_origins() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        for (local_url, public_origin) in [
            ("http://0.0.0.0:3021", Some("https://notes.example.com")),
            ("http://127.0.0.1:3021", Some("http://notes.example.com")),
            (
                "http://127.0.0.1:3021",
                Some("https://notes.example.com/subpath"),
            ),
        ] {
            let error = store
                .save(
                    "anonymous",
                    "default",
                    NotesSettings {
                        silverbullet: SilverBulletNotesSettings {
                            local_url: local_url.to_string(),
                            public_origin: public_origin.map(str::to_string),
                            ..SilverBulletNotesSettings::default()
                        },
                        ..NotesSettings::default()
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }

        let error = store
            .save(
                "anonymous",
                "default",
                NotesSettings {
                    silverbullet: SilverBulletNotesSettings {
                        server_url: "javascript:alert(1)".to_string(),
                        ..SilverBulletNotesSettings::default()
                    },
                    ..NotesSettings::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn append_note_rejects_path_escape() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let error = store
            .append_note(
                "anonymous",
                "default",
                AppendNoteRequest {
                    title: None,
                    body: "nope".to_string(),
                    provider: None,
                    target_path: Some("../escape.md".to_string()),
                },
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn concurrent_appends_do_not_lose_either_update() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let first = store.append_note(
            "anonymous",
            "default",
            AppendNoteRequest {
                title: None,
                body: "first concurrent append".to_string(),
                provider: None,
                target_path: Some("Inbox/concurrent.md".to_string()),
            },
        );
        let second = store.append_note(
            "anonymous",
            "default",
            AppendNoteRequest {
                title: None,
                body: "second concurrent append".to_string(),
                provider: None,
                target_path: Some("Inbox/concurrent.md".to_string()),
            },
        );

        let (first_result, second_result) = tokio::join!(first, second);
        first_result.unwrap();
        second_result.unwrap();
        let content = fs::read_to_string(
            temp.path()
                .join("scopes/anonymous/default/notes/space/Inbox/concurrent.md"),
        )
        .await
        .unwrap();
        assert!(content.contains("first concurrent append"));
        assert!(content.contains("second concurrent append"));
    }

    #[tokio::test]
    async fn silverbullet_default_falls_back_to_local_when_not_configured() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        store
            .save(
                "anonymous",
                "default",
                NotesSettings {
                    enabled: true,
                    default_provider: "silverbullet".to_string(),
                    fallback_provider: "local_markdown".to_string(),
                    local_markdown: LocalMarkdownNotesSettings::default(),
                    silverbullet: SilverBulletNotesSettings {
                        space_path: None,
                        local_url: default_silverbullet_local_url(),
                        server_url: default_silverbullet_server_url(),
                        public_origin: None,
                    },
                    task_publishing: TaskPublishingSettings::default(),
                },
            )
            .await
            .unwrap();

        let note_ref = store
            .create_note(
                "anonymous",
                "default",
                CreateNoteRequest {
                    title: "Fallback Note".to_string(),
                    body: "from fallback".to_string(),
                    provider: None,
                    target_dir: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(note_ref.requested_provider, "silverbullet");
        assert_eq!(note_ref.provider, "local_markdown");
        assert!(note_ref.used_fallback);
        assert!(note_ref
            .fallback_reason
            .as_deref()
            .unwrap_or_default()
            .contains("not configured"));
        assert!(PathBuf::from(note_ref.absolute_path)
            .starts_with(temp.path().join("scopes/anonymous/default/notes/space")));
    }

    #[tokio::test]
    async fn disabled_notes_reject_writes_without_touching_note_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        store
            .save(
                "anonymous",
                "default",
                NotesSettings {
                    enabled: false,
                    ..NotesSettings::default()
                },
            )
            .await
            .unwrap();

        let error = store
            .create_note(
                "anonymous",
                "default",
                CreateNoteRequest {
                    title: "No Write".to_string(),
                    body: "blocked".to_string(),
                    provider: None,
                    target_dir: None,
                },
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!temp
            .path()
            .join("scopes/anonymous/default/notes/space")
            .exists());
    }

    #[tokio::test]
    async fn create_note_allocates_unique_paths_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let first = store
            .create_note(
                "anonymous",
                "default",
                CreateNoteRequest {
                    title: "Same Title".to_string(),
                    body: "first body".to_string(),
                    provider: None,
                    target_dir: None,
                },
            )
            .await
            .unwrap();
        let second = store
            .create_note(
                "anonymous",
                "default",
                CreateNoteRequest {
                    title: "Same Title".to_string(),
                    body: "second body".to_string(),
                    provider: None,
                    target_dir: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(first.path, "Inbox/same-title.md");
        assert_eq!(second.path, "Inbox/same-title-2.md");
        assert!(fs::read_to_string(first.absolute_path)
            .await
            .unwrap()
            .contains("first body"));
        assert!(fs::read_to_string(second.absolute_path)
            .await
            .unwrap()
            .contains("second body"));
    }

    /// A URL with a closing paren — every Wikipedia disambiguation link — must
    /// not terminate the Markdown link at the first one.
    #[tokio::test]
    async fn a_url_containing_parentheses_still_links() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let note = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    source_url: Some(
                        "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string(),
                    ),
                    source_title: Some("Rust [lang]".to_string()),
                    ..capture("a passage")
                },
            )
            .await
            .unwrap();

        let body = fs::read_to_string(&note.absolute_path).await.unwrap();
        assert!(body.contains("(<https://en.wikipedia.org/wiki/Rust_(programming_language)>)"));
        // The title keeps its brackets rather than having them rewritten.
        assert!(body.contains("Rust \\[lang\\]"));
    }

    /// The marker is an HTML comment, so an id that can close it early would let
    /// a caller write content into the owner's note.
    #[tokio::test]
    async fn a_capture_id_that_could_escape_the_marker_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        for hostile in ["a --> <img src=x>", "id with spaces", "<!--nested"] {
            let error = store
                .capture_selection(
                    "anonymous",
                    "default",
                    CaptureSelectionRequest {
                        capture_id: Some(hostile.to_string()),
                        ..capture("a passage")
                    },
                )
                .await
                .expect_err("an unsafe capture id must not reach the note");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }

        // A UUID, which is what the surfaces actually send, is accepted.
        store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string()),
                    ..capture("a passage")
                },
            )
            .await
            .unwrap();
    }

    fn capture(text: &str) -> CaptureSelectionRequest {
        CaptureSelectionRequest {
            text: text.to_string(),
            source_url: None,
            source_title: None,
            source_app: None,
            target_path: Some("Inbox/captures.md".to_string()),
            provider: None,
            capture_id: None,
        }
    }

    /// The passage is quoted so it stays visibly the source's words, and the
    /// provenance is written where the owner can follow it back.
    #[tokio::test]
    async fn capture_keeps_the_passage_and_where_it_came_from() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let note = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    source_url: Some("https://example.test/post".to_string()),
                    source_title: Some("A Post".to_string()),
                    source_app: Some("Google Chrome".to_string()),
                    ..capture("first line\nsecond line")
                },
            )
            .await
            .unwrap();

        let body = fs::read_to_string(&note.absolute_path).await.unwrap();
        assert!(body.contains("> first line"));
        assert!(body.contains("> second line"));
        // Angle-bracket destination, so a URL carrying `)` cannot end the link
        // early. See `a_url_containing_parentheses_still_links`.
        assert!(body.contains("[A Post](<https://example.test/post>)"));
        assert!(body.contains("(Google Chrome)"));
    }

    /// A selection from a native app has no URL and often no title. Refusing it
    /// would discard the thing the owner asked to keep over a field they never
    /// had, so provenance degrades instead of failing.
    #[tokio::test]
    async fn capture_without_provenance_is_still_filed() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let note = store
            .capture_selection("anonymous", "default", capture("orphan passage"))
            .await
            .unwrap();

        let body = fs::read_to_string(&note.absolute_path).await.unwrap();
        assert!(body.contains("> orphan passage"));
        // No source line invented, and no empty one left behind.
        assert!(!body.contains("—"));
    }

    /// A surface that resends after a lost response must not file twice, and
    /// the owner could not tell a duplicate from a real second capture.
    #[tokio::test]
    async fn a_resent_capture_is_filed_once_but_a_new_one_is_filed_again() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let retried = CaptureSelectionRequest {
            capture_id: Some("capture-1".to_string()),
            ..capture("same passage")
        };

        let first = store
            .capture_selection("anonymous", "default", retried.clone())
            .await
            .unwrap();
        let second = store
            .capture_selection("anonymous", "default", retried)
            .await
            .unwrap();

        assert_eq!(first.path, second.path);
        let body = fs::read_to_string(&second.absolute_path).await.unwrap();
        assert_eq!(body.matches("> same passage").count(), 1);

        // A deliberate second capture carries a new id: repeating an intentional
        // act is not a duplicate.
        store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("capture-2".to_string()),
                    ..capture("same passage")
                },
            )
            .await
            .unwrap();
        let body = fs::read_to_string(&second.absolute_path).await.unwrap();
        assert_eq!(body.matches("> same passage").count(), 2);
    }

    #[tokio::test]
    async fn capture_refuses_empty_selection() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let error = store
            .capture_selection("anonymous", "default", capture("   \n  "))
            .await
            .expect_err("whitespace is not a selection");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// The retry check must read the same file the write targets, or a resend
    /// to the default page would never match and would file twice.
    #[tokio::test]
    async fn the_default_capture_page_is_the_one_the_retry_check_reads() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let request = CaptureSelectionRequest {
            target_path: None,
            capture_id: Some("capture-default".to_string()),
            ..capture("default page passage")
        };

        let first = store
            .capture_selection("anonymous", "default", request.clone())
            .await
            .unwrap();
        store
            .capture_selection("anonymous", "default", request)
            .await
            .unwrap();

        assert_eq!(first.path, path_to_string(&default_daily_page_path()));
        let body = fs::read_to_string(&first.absolute_path).await.unwrap();
        assert_eq!(body.matches("> default page passage").count(), 1);
    }

    /// Write yesterday's default page directly, with a marker on it — the
    /// stand-in for a capture that landed just before midnight. Returns the
    /// page's relative path.
    async fn write_yesterdays_default_page(
        store: &NotesSettingsStore,
        capture_id: &str,
        passage: &str,
    ) -> String {
        let yesterday = path_to_string(&PathBuf::from("Inbox").join(format!(
            "{}.md",
            (Utc::now() - chrono::Duration::days(1)).format("%Y-%m-%d")
        )));
        store
            .write_note_markdown(
                "anonymous",
                "default",
                WriteNoteMarkdownRequest {
                    provider: Some("local_markdown".into()),
                    target_path: yesterday.clone(),
                    markdown: format!("> {passage}\n{}", capture_marker(capture_id)),
                },
            )
            .await
            .unwrap();
        yesterday
    }

    /// The dedupe read and the append hold the same write lock, so two
    /// concurrent sends of one `capture_id` cannot both observe "not captured"
    /// and both file.
    #[tokio::test]
    async fn concurrent_sends_of_one_capture_id_file_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let resent = CaptureSelectionRequest {
            capture_id: Some("capture-race".to_string()),
            ..capture("raced passage")
        };

        let (first, second) = tokio::join!(
            store.capture_selection("anonymous", "default", resent.clone()),
            store.capture_selection("anonymous", "default", resent)
        );
        first.unwrap();
        second.unwrap();

        let body = fs::read_to_string(
            temp.path()
                .join("scopes/anonymous/default/notes/space/Inbox/captures.md"),
        )
        .await
        .unwrap();
        assert_eq!(body.matches("> raced passage").count(), 1);
    }

    /// A page that exists but cannot be read must not read as "not captured":
    /// a transient read error on the very file holding the marker would
    /// otherwise file the same capture twice. The capture fails loudly instead.
    #[tokio::test]
    async fn an_unreadable_target_page_fails_the_capture_instead_of_duplicating() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let page = temp
            .path()
            .join("scopes/anonymous/default/notes/space/Inbox/captures.md");
        fs::create_dir_all(page.parent().unwrap()).await.unwrap();
        let mut bytes = capture_marker("capture-lost").into_bytes();
        bytes.extend_from_slice(&[0xff, 0xfe]); // not valid UTF-8, so reads fail
        fs::write(&page, &bytes).await.unwrap();

        let error = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("capture-lost".to_string()),
                    ..capture("lost passage")
                },
            )
            .await
            .expect_err("an unreadable page must fail the capture, not read as uncaptured");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        // The capture failed before anything was appended.
        assert_eq!(fs::read(&page).await.unwrap(), bytes);
    }

    /// The existence half of the dedupe check fails closed too: a metadata
    /// error that is not "page gone" must fail the capture rather than read
    /// as "not captured". The harness makes one deterministically — a plain
    /// file sitting where the `Inbox` directory belongs, so stat'ing
    /// `Inbox/captures.md` under it fails with ENOTDIR — without permission
    /// or root dependence; that shape is only creatable through `std::fs` on
    /// Unix, so the test is Unix-gated.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unstatable_target_page_fails_the_capture_instead_of_duplicating() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let inbox = temp
            .path()
            .join("scopes/anonymous/default/notes/space/Inbox");
        fs::create_dir_all(inbox.parent().unwrap()).await.unwrap();
        fs::write(&inbox, b"not a directory").await.unwrap();

        let error = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("capture-notdir".to_string()),
                    ..capture("blocked passage")
                },
            )
            .await
            .expect_err("an unstatable page must fail the capture, not read as uncaptured");
        // The propagated stat failure, not the missing-page case.
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    }

    /// A capture sent at 23:59:58 and retried at 00:00:01 resolves a different
    /// "today", so the retry must also check yesterday's default page before
    /// concluding the marker is missing.
    #[tokio::test]
    async fn a_retry_after_midnight_still_finds_the_marker_on_yesterdays_default_page() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let yesterday =
            write_yesterdays_default_page(&store, "capture-midnight", "yesterday passage").await;

        // The retry omits `target_path`, so it resolves today's default page —
        // not the one the original wrote to.
        let retried = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("capture-midnight".to_string()),
                    target_path: None,
                    ..capture("yesterday passage")
                },
            )
            .await
            .unwrap();

        // The resend is answered from yesterday's page, not written again.
        assert_eq!(retried.path, yesterday);
        let body = fs::read_to_string(&retried.absolute_path).await.unwrap();
        assert_eq!(body.matches("> yesterday passage").count(), 1);
        // The look-back is dedupe-only: nothing landed on today's page.
        assert!(!temp
            .path()
            .join("scopes/anonymous/default/notes/space")
            .join(default_daily_page_path())
            .exists());
    }

    /// The look-back must not suppress a genuine capture: a new id after
    /// midnight still files, on the current default page.
    #[tokio::test]
    async fn a_new_capture_after_midnight_still_writes_to_todays_default_page() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        write_yesterdays_default_page(&store, "capture-old", "yesterday passage").await;

        let note = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("capture-new".to_string()),
                    target_path: None,
                    ..capture("today passage")
                },
            )
            .await
            .unwrap();

        assert_eq!(note.path, path_to_string(&default_daily_page_path()));
        let body = fs::read_to_string(&note.absolute_path).await.unwrap();
        assert_eq!(body.matches("> today passage").count(), 1);
    }

    /// The yesterday look-back applies only to the default page: a caller who
    /// names a page gets exactly that page checked, so a marker sitting on
    /// yesterday's default page cannot suppress an explicit-page capture.
    #[tokio::test]
    async fn an_explicit_target_path_does_not_look_back_at_yesterdays_default_page() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        write_yesterdays_default_page(&store, "capture-explicit", "yesterday passage").await;

        let note = store
            .capture_selection(
                "anonymous",
                "default",
                CaptureSelectionRequest {
                    capture_id: Some("capture-explicit".to_string()),
                    ..capture("explicit passage")
                },
            )
            .await
            .unwrap();

        assert_eq!(note.path, "Inbox/captures.md");
        let body = fs::read_to_string(&note.absolute_path).await.unwrap();
        assert_eq!(body.matches("> explicit passage").count(), 1);
    }

    #[tokio::test]
    async fn append_note_appends_to_existing_target() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let first = store
            .append_note(
                "anonymous",
                "default",
                AppendNoteRequest {
                    title: Some("First".to_string()),
                    body: "alpha".to_string(),
                    provider: None,
                    target_path: Some("Inbox/daily.md".to_string()),
                },
            )
            .await
            .unwrap();
        let second = store
            .append_note(
                "anonymous",
                "default",
                AppendNoteRequest {
                    title: Some("Second".to_string()),
                    body: "beta".to_string(),
                    provider: None,
                    target_path: Some("Inbox/daily.md".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(first.path, "Inbox/daily.md");
        assert_eq!(second.path, "Inbox/daily.md");
        let content = fs::read_to_string(second.absolute_path).await.unwrap();
        assert!(content.contains("## First"));
        assert!(content.contains("alpha"));
        assert!(content.contains("## Second"));
        assert!(content.contains("beta"));
    }

    #[tokio::test]
    async fn write_asset_uses_same_provider_and_path_safety() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let asset = store
            .write_asset(
                "anonymous",
                "default",
                WriteNoteAssetRequest {
                    provider: None,
                    target_dir: "Artifacts/task_123.assets".to_string(),
                    file_name: "result.json".to_string(),
                    bytes: br#"{"ok":true}"#.to_vec(),
                },
            )
            .await
            .unwrap();

        assert_eq!(asset.provider, "local_markdown");
        assert_eq!(asset.path, "Artifacts/task_123.assets/result.json");
        assert_eq!(asset.bytes, 11);
        assert_eq!(
            fs::read_to_string(&asset.absolute_path).await.unwrap(),
            r#"{"ok":true}"#
        );

        let error = store
            .write_asset(
                "anonymous",
                "default",
                WriteNoteAssetRequest {
                    provider: None,
                    target_dir: "Artifacts".to_string(),
                    file_name: "../escape.json".to_string(),
                    bytes: b"nope".to_vec(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn audio_note_keeps_recording_and_page_together_with_dated_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let note = store
            .save_audio_note(
                "anonymous",
                "default",
                SaveAudioNoteRequest {
                    // Exercise the explicit SilverBullet preference and durable
                    // local fallback used by mobile capture on a fresh setup.
                    provider: Some("silverbullet".to_string()),
                    note_id: Some("a1b2c3d4-1111-4222-8333-123456789abc".to_string()),
                    captured_at: Some("2026-08-01T20:56:24.147+05:30".to_string()),
                    source_surface: "ios_voice_note".to_string(),
                    transcript: Some("Remember the launch idea.".to_string()),
                    original_filename: Some("voice-note.m4a".to_string()),
                    mime_type: "audio/m4a".to_string(),
                    duration_ms: Some(4_250),
                    bytes: b"recorded-audio".to_vec(),
                },
            )
            .await
            .unwrap();

        assert_eq!(note.requested_provider, "silverbullet");
        assert_eq!(note.provider, "local_markdown");
        assert!(note.used_fallback);
        assert_eq!(note.note_id, "a1b2c3d4-1111-4222-8333-123456789abc");
        assert_eq!(note.captured_at, "2026-08-01T20:56:24.147+05:30");
        assert_eq!(
            note.note_path,
            "Audio Notes/2026-08-01/20-56-24-147-a1b2c3d4111142228333123456789abc.md"
        );
        assert_eq!(
            note.audio_path,
            "Audio Notes/2026-08-01/20-56-24-147-a1b2c3d4111142228333123456789abc.m4a"
        );
        assert_eq!(
            fs::read(&note.audio_absolute_path).await.unwrap(),
            b"recorded-audio"
        );

        let markdown = fs::read_to_string(&note.note_absolute_path).await.unwrap();
        assert!(markdown.contains("magician_kind: audio_note"));
        assert!(markdown.contains("recorded_date: 2026-08-01"));
        assert!(markdown.contains("recorded_time: \"20:56:24 +05:30\""));
        assert!(markdown.contains("\"audio-note/date/2026-08-01\""));
        assert!(markdown.contains("\"audio-note/time/20-56\""));
        assert!(markdown.contains("source_surface: \"ios_voice_note\""));
        assert!(markdown.contains("duration_ms: 4250"));
        assert!(markdown.contains("Remember the launch idea."));
        assert!(markdown.contains("<audio controls"));

        let receipt = serde_json::to_value(&note).unwrap();
        assert_eq!(
            receipt.get("note_id").and_then(serde_json::Value::as_str),
            Some("a1b2c3d4-1111-4222-8333-123456789abc")
        );
        assert!(receipt.get("note_absolute_path").is_none());
        assert!(receipt.get("audio_absolute_path").is_none());
        assert!(receipt.get("open_url").is_none());
        let expected_hash = format!("blake3:{}", blake3::hash(b"recorded-audio").to_hex());
        assert_eq!(
            receipt
                .get("content_hash")
                .and_then(serde_json::Value::as_str),
            Some(expected_hash.as_str())
        );

        let retry = store
            .save_audio_note(
                "anonymous",
                "default",
                SaveAudioNoteRequest {
                    provider: Some("silverbullet".to_string()),
                    note_id: Some("a1b2c3d4-1111-4222-8333-123456789abc".to_string()),
                    captured_at: Some("2026-08-01T20:56:24.147+05:30".to_string()),
                    source_surface: "ios_voice_note".to_string(),
                    transcript: Some("Remember the launch idea.".to_string()),
                    original_filename: Some("voice-note.m4a".to_string()),
                    mime_type: "audio/m4a".to_string(),
                    duration_ms: Some(4_250),
                    bytes: b"recorded-audio".to_vec(),
                },
            )
            .await
            .unwrap();
        assert_eq!(retry.note_path, note.note_path);
        assert_eq!(retry.audio_path, note.audio_path);

        let equivalent_timestamp_retry = store
            .save_audio_note(
                "anonymous",
                "default",
                SaveAudioNoteRequest {
                    provider: Some("silverbullet".to_string()),
                    note_id: Some("a1b2c3d4-1111-4222-8333-123456789abc".to_string()),
                    captured_at: Some("2026-08-01T15:26:24.147000Z".to_string()),
                    source_surface: "ios_voice_note".to_string(),
                    transcript: Some("Remember the launch idea.".to_string()),
                    original_filename: Some("voice-note.m4a".to_string()),
                    mime_type: "audio/m4a".to_string(),
                    duration_ms: Some(4_250),
                    bytes: b"recorded-audio".to_vec(),
                },
            )
            .await
            .unwrap();
        assert_eq!(equivalent_timestamp_retry.note_path, note.note_path);
        assert_eq!(equivalent_timestamp_retry.audio_path, note.audio_path);

        let collision = store
            .save_audio_note(
                "anonymous",
                "default",
                SaveAudioNoteRequest {
                    provider: None,
                    note_id: Some("a1b2c3d4-1111-4222-8333-123456789abc".to_string()),
                    captured_at: Some("2026-08-01T20:56:24.147+05:30".to_string()),
                    source_surface: "ios_voice_note".to_string(),
                    transcript: None,
                    original_filename: Some("voice-note.m4a".to_string()),
                    mime_type: "audio/m4a".to_string(),
                    duration_ms: None,
                    bytes: b"different-audio".to_vec(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[tokio::test]
    async fn audio_note_index_supports_scoped_search_pagination_recording_and_delete() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        for (id, captured_at, transcript) in [
            (
                "11111111-1111-4111-8111-111111111111",
                "2026-08-01T10:00:00.000+05:30",
                "launch alpha",
            ),
            (
                "22222222-2222-4222-8222-222222222222",
                "2026-08-02T10:00:00.000+05:30",
                "design beta",
            ),
            (
                "33333333-3333-4333-8333-333333333333",
                "2026-08-03T10:00:00.000+05:30",
                "launch gamma",
            ),
        ] {
            store
                .save_audio_note(
                    "anonymous",
                    "default",
                    SaveAudioNoteRequest {
                        provider: None,
                        note_id: Some(id.to_string()),
                        captured_at: Some(captured_at.to_string()),
                        source_surface: "ios_voice_note".to_string(),
                        transcript: Some(transcript.to_string()),
                        original_filename: Some("voice-note.m4a".to_string()),
                        mime_type: "audio/m4a".to_string(),
                        duration_ms: Some(1_000),
                        bytes: format!("audio-{id}").into_bytes(),
                    },
                )
                .await
                .unwrap();
        }

        let page = store
            .list_audio_notes("anonymous", "default", 1, 1, None)
            .await
            .unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.items[0].note_id,
            "22222222-2222-4222-8222-222222222222"
        );
        assert!(page.has_more);

        let search = store
            .list_audio_notes("anonymous", "default", 0, 20, Some("LAUNCH"))
            .await
            .unwrap();
        assert_eq!(search.total, 2);
        assert_eq!(
            search.items[0].note_id,
            "33333333-3333-4333-8333-333333333333"
        );

        let recording = store
            .audio_note_recording(
                "anonymous",
                "default",
                "22222222-2222-4222-8222-222222222222",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read(recording.absolute_path).await.unwrap(),
            b"audio-22222222-2222-4222-8222-222222222222"
        );

        assert!(store
            .delete_audio_note(
                "anonymous",
                "default",
                "22222222-2222-4222-8222-222222222222",
            )
            .await
            .unwrap());
        assert!(store
            .read_audio_note(
                "anonymous",
                "default",
                "22222222-2222-4222-8222-222222222222",
            )
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .list_audio_notes("anonymous", "default", 0, 20, None)
                .await
                .unwrap()
                .total,
            2
        );
    }

    #[tokio::test]
    async fn audio_note_retry_adopts_only_exact_unindexed_provider_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let request = SaveAudioNoteRequest {
            provider: None,
            note_id: Some("55555555-5555-4555-8555-555555555555".to_string()),
            captured_at: Some("2026-08-05T10:00:00.000+05:30".to_string()),
            source_surface: "ios_voice_note".to_string(),
            transcript: Some("recover exact remnants".to_string()),
            original_filename: Some("voice-note.m4a".to_string()),
            mime_type: "audio/m4a".to_string(),
            duration_ms: Some(1_000),
            bytes: b"recoverable-audio".to_vec(),
        };
        let layout = audio_note_layout(&request).unwrap();
        let root = PathBuf::from(
            store
                .load_envelope("anonymous", "default")
                .await
                .unwrap()
                .resolved
                .local_markdown_root,
        );
        let audio_path = root.join(&layout.target_dir).join(&layout.audio_file_name);
        let note_path = root.join(&layout.note_path);
        fs::create_dir_all(audio_path.parent().unwrap())
            .await
            .unwrap();
        fs::write(&audio_path, &request.bytes).await.unwrap();
        fs::write(&note_path, render_audio_note_markdown(&request, &layout))
            .await
            .unwrap();

        let recovered = store
            .save_audio_note("anonymous", "default", request)
            .await
            .unwrap();
        assert_eq!(
            recovered.audio_absolute_path,
            audio_path.display().to_string()
        );
        assert!(store
            .read_audio_note(
                "anonymous",
                "default",
                "55555555-5555-4555-8555-555555555555",
            )
            .await
            .unwrap()
            .is_some());

        let conflicting = SaveAudioNoteRequest {
            provider: None,
            note_id: Some("66666666-6666-4666-8666-666666666666".to_string()),
            captured_at: Some("2026-08-06T10:00:00.000+05:30".to_string()),
            source_surface: "ios_voice_note".to_string(),
            transcript: None,
            original_filename: Some("voice-note.m4a".to_string()),
            mime_type: "audio/m4a".to_string(),
            duration_ms: None,
            bytes: b"incoming-audio".to_vec(),
        };
        let collision_layout = audio_note_layout(&conflicting).unwrap();
        let collision_path = root
            .join(&collision_layout.target_dir)
            .join(&collision_layout.audio_file_name);
        fs::create_dir_all(collision_path.parent().unwrap())
            .await
            .unwrap();
        fs::write(&collision_path, b"human-owned-audio")
            .await
            .unwrap();

        let error = store
            .save_audio_note("anonymous", "default", conflicting)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(collision_path).await.unwrap(),
            b"human-owned-audio"
        );
    }

    #[tokio::test]
    async fn audio_note_retry_repairs_only_the_missing_indexed_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let request = SaveAudioNoteRequest {
            provider: None,
            note_id: Some("77777777-7777-4777-8777-777777777777".to_string()),
            captured_at: Some("2026-08-07T10:00:00.000+05:30".to_string()),
            source_surface: "ios_voice_note".to_string(),
            transcript: Some("original transcript".to_string()),
            original_filename: Some("voice-note.m4a".to_string()),
            mime_type: "audio/m4a".to_string(),
            duration_ms: Some(1_000),
            bytes: b"repair-audio".to_vec(),
        };
        let saved = store
            .save_audio_note("anonymous", "default", request.clone())
            .await
            .unwrap();
        fs::write(&saved.note_absolute_path, b"# User-edited Audio Note\n")
            .await
            .unwrap();
        fs::remove_file(&saved.audio_absolute_path).await.unwrap();

        store
            .save_audio_note("anonymous", "default", request)
            .await
            .unwrap();

        assert_eq!(
            fs::read_to_string(&saved.note_absolute_path).await.unwrap(),
            "# User-edited Audio Note\n"
        );
        assert_eq!(
            fs::read(&saved.audio_absolute_path).await.unwrap(),
            b"repair-audio"
        );
    }

    #[tokio::test]
    async fn audio_note_keeps_its_internal_provider_root_after_settings_change() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());
        let note_id = "44444444-4444-4444-8444-444444444444";
        let saved = store
            .save_audio_note(
                "anonymous",
                "default",
                SaveAudioNoteRequest {
                    provider: None,
                    note_id: Some(note_id.to_string()),
                    captured_at: Some("2026-08-04T10:00:00.000+05:30".to_string()),
                    source_surface: "ios_voice_note".to_string(),
                    transcript: Some("root snapshot".to_string()),
                    original_filename: Some("voice-note.m4a".to_string()),
                    mime_type: "audio/m4a".to_string(),
                    duration_ms: Some(1_000),
                    bytes: b"root-snapshot-audio".to_vec(),
                },
            )
            .await
            .unwrap();
        let original_audio = PathBuf::from(&saved.audio_absolute_path);
        let canonical_original_audio = fs::canonicalize(&original_audio).await.unwrap();

        let replacement_root = temp.path().join("replacement-notes-root");
        store
            .save(
                "anonymous",
                "default",
                NotesSettings {
                    local_markdown: LocalMarkdownNotesSettings {
                        root: Some(replacement_root.display().to_string()),
                    },
                    ..NotesSettings::default()
                },
            )
            .await
            .unwrap();

        let recording = store
            .audio_note_recording("anonymous", "default", note_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recording.absolute_path, canonical_original_audio);
        assert_eq!(
            fs::read(&recording.absolute_path).await.unwrap(),
            b"root-snapshot-audio"
        );
        let public = serde_json::to_value(
            store
                .read_audio_note("anonymous", "default", note_id)
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(public.get("provider_root").is_none());
        let private_index = store
            .read_audio_note_record("anonymous", "default", note_id)
            .await
            .unwrap()
            .unwrap();
        assert!(Path::new(&private_index.provider_root).is_absolute());

        assert!(store
            .delete_audio_note("anonymous", "default", note_id)
            .await
            .unwrap());
        assert!(!original_audio.exists());
        assert!(!replacement_root.join(&saved.audio_path).exists());
    }

    #[tokio::test]
    async fn audio_note_rejects_invalid_capture_timestamp_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let store = NotesSettingsStore::new(temp.path());

        let error = store
            .save_audio_note(
                "anonymous",
                "default",
                SaveAudioNoteRequest {
                    provider: None,
                    note_id: None,
                    captured_at: Some("tomorrow-ish".to_string()),
                    source_surface: "ios_voice_note".to_string(),
                    transcript: None,
                    original_filename: None,
                    mime_type: "audio/m4a".to_string(),
                    duration_ms: None,
                    bytes: b"recorded-audio".to_vec(),
                },
            )
            .await
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!temp
            .path()
            .join("scopes/anonymous/default/notes/space/Audio Notes")
            .exists());
    }
}
