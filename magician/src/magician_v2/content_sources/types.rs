use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

pub const CONTENT_SOURCE_SCHEMA_VERSION: u32 = 1;
pub const MAX_DISCOVERY_ITEMS: usize = 200;
pub const MAX_CANDIDATE_TITLE_CHARS: usize = 512;
pub const MAX_CANDIDATE_CHEAP_TEXT_CHARS: usize = 32 * 1024;
pub const MAX_DOCUMENT_TEXT_CHARS: usize = 4 * 1024 * 1024;
pub const MAX_ADAPTER_ID_CHARS: usize = 128;
pub const MAX_SOURCE_ITEM_ID_CHARS: usize = 2048;
pub const MAX_DISCOVERY_TARGETS: usize = 32;
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_DISPLAY_NAME_CHARS: usize = 256;
pub const MAX_PROVENANCE_LABEL_CHARS: usize = 1024;
pub const MAX_PROVENANCE_RETRIEVER_CHARS: usize = 256;
pub const MAX_CONTENT_URL_CHARS: usize = 8 * 1024;
pub const MAX_CONTENT_HASH_CHARS: usize = 256;
pub const MAX_COST_COMMODITY_CHARS: usize = 128;
pub const MAX_SCOPE_COMPONENT_BYTES: usize = 255;

/// Content acquisition scope values become workspace path segments in the
/// capability and cache layers. Reject values that the workspace sanitizer
/// would rewrite so two distinct identities can never resolve to one scope.
pub fn validate_content_scope_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value != value.trim() {
        bail!("{label} must be non-empty and have no surrounding whitespace");
    }
    if value.len() > MAX_SCOPE_COMPONENT_BYTES {
        bail!("{label} exceeds {MAX_SCOPE_COMPONENT_BYTES} UTF-8 bytes");
    }
    if value == "."
        || value == ".."
        || value.contains("..")
        || value.chars().any(|ch| {
            ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        bail!("{label} contains characters that are unsafe for scoped storage");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSourceClass {
    OwnedChannel,
    Syndication,
    WebSearch,
    WebPage,
    Community,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterExecution {
    LocalProcess,
    RemoteEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterAuth {
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalAuthority {
    LocalOnly,
    PublicRemoteRead,
    PublicBrowserRead,
    PublicBrowserInteract,
    AuthenticatedRead,
    AuthenticatedInteract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalOperation {
    Discover,
    Read,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalRung {
    Inline,
    SourceNative,
    PublicSearch,
    PublicStatic,
    VerifiedReplay,
    PublicRendered,
    PublicBrowserHandoff,
    OwnerAssisted,
    Authenticated,
    InteractionHandoff,
}

impl RetrievalRung {
    pub fn supports(self, operation: RetrievalOperation) -> bool {
        match operation {
            RetrievalOperation::Discover => matches!(
                self,
                Self::Inline
                    | Self::SourceNative
                    | Self::PublicSearch
                    | Self::PublicBrowserHandoff
                    | Self::Authenticated
            ),
            RetrievalOperation::Read => matches!(
                self,
                Self::Inline
                    | Self::PublicStatic
                    | Self::VerifiedReplay
                    | Self::PublicRendered
                    | Self::OwnerAssisted
                    | Self::Authenticated
                    | Self::InteractionHandoff
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalOutputKind {
    Candidates,
    Gist,
    FullText,
    Structured,
    Handoff,
}

/// Projection metadata used by the Phase 7 retrieval controller. Native,
/// manifest-backed, and compiled actions all expose this same contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalActionMetadata {
    pub action_id: String,
    pub operation: RetrievalOperation,
    pub rung: RetrievalRung,
    pub outputs: Vec<RetrievalOutputKind>,
    pub authority: RetrievalAuthority,
    /// Stable within-rung ordering for auto-discovered actions. Lower values
    /// run first; ties are broken by action_id. Central ladder configuration
    /// can still place explicit actions ahead as an operator override.
    #[serde(default = "default_retrieval_priority")]
    pub priority: u16,
    #[serde(default)]
    pub parallel_safe: bool,
    /// Request option names this action accepts. The controller projects the
    /// shared need down to this list so source-specific options cannot make a
    /// different ladder action fail as an invalid request.
    #[serde(default)]
    pub accepted_options: Vec<String>,
    /// MIME types accepted by a read action. An empty list means the media
    /// type cannot be determined before dispatch.
    #[serde(default)]
    pub accepted_media_types: Vec<String>,
    #[serde(default)]
    pub accepts_targets: bool,
    #[serde(default)]
    pub requires_targets: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<AdapterCost>,
}

impl RetrievalActionMetadata {
    pub fn discovery(
        action_id: impl Into<String>,
        rung: RetrievalRung,
        authority: RetrievalAuthority,
        parallel_safe: bool,
    ) -> Self {
        Self {
            action_id: action_id.into(),
            operation: RetrievalOperation::Discover,
            rung,
            outputs: vec![RetrievalOutputKind::Candidates],
            authority,
            priority: default_retrieval_priority(),
            parallel_safe,
            accepted_options: Vec::new(),
            accepted_media_types: Vec::new(),
            accepts_targets: false,
            requires_targets: false,
            estimated_cost: None,
        }
    }

    pub fn reader(
        action_id: impl Into<String>,
        rung: RetrievalRung,
        authority: RetrievalAuthority,
        outputs: Vec<RetrievalOutputKind>,
    ) -> Self {
        Self {
            action_id: action_id.into(),
            operation: RetrievalOperation::Read,
            rung,
            outputs,
            authority,
            priority: default_retrieval_priority(),
            parallel_safe: false,
            accepted_options: Vec::new(),
            accepted_media_types: Vec::new(),
            accepts_targets: false,
            requires_targets: false,
            estimated_cost: None,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_bounded_label(&self.action_id, "retrieval action_id", MAX_ADAPTER_ID_CHARS)?;
        if !self.action_id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.')
        }) {
            bail!("retrieval action_id contains unsupported characters");
        }
        if !self.rung.supports(self.operation) {
            bail!(
                "retrieval rung `{:?}` is incompatible with operation `{:?}`",
                self.rung,
                self.operation
            );
        }
        if self.outputs.is_empty() {
            bail!("retrieval action requires at least one output capability");
        }
        if self.requires_targets && !self.accepts_targets {
            bail!("retrieval action cannot require targets it does not accept");
        }
        let options = self
            .accepted_options
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        if options.len() != self.accepted_options.len()
            || self.accepted_options.iter().any(|option| {
                option.trim().is_empty()
                    || option.chars().count() > MAX_ADAPTER_ID_CHARS
                    || !option.chars().all(|character| {
                        character.is_ascii_lowercase()
                            || character.is_ascii_digit()
                            || matches!(character, '-' | '_')
                    })
            })
        {
            bail!("retrieval action accepted_options contains invalid or duplicate names");
        }
        let media_types = self
            .accepted_media_types
            .iter()
            .map(|media_type| media_type.trim().to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        if media_types.len() != self.accepted_media_types.len()
            || self.accepted_media_types.iter().any(|media_type| {
                let media_type = media_type.trim();
                media_type.is_empty()
                    || media_type.chars().count() > MAX_ADAPTER_ID_CHARS
                    || !media_type.contains('/')
                    || media_type.chars().any(char::is_whitespace)
            })
        {
            bail!("retrieval action accepted_media_types contains invalid or duplicate values");
        }
        if self.operation == RetrievalOperation::Discover && !self.accepted_media_types.is_empty() {
            bail!("discovery retrieval actions cannot declare accepted media types");
        }
        let mut outputs = self.outputs.clone();
        outputs.sort();
        outputs.dedup();
        if outputs.len() != self.outputs.len() {
            bail!("retrieval action output capabilities contain duplicates");
        }
        match self.operation {
            RetrievalOperation::Discover
                if !self.outputs.contains(&RetrievalOutputKind::Candidates) =>
            {
                bail!("discovery retrieval actions must produce candidates")
            },
            RetrievalOperation::Read
                if !self.outputs.iter().any(|output| {
                    matches!(
                        output,
                        RetrievalOutputKind::Gist
                            | RetrievalOutputKind::FullText
                            | RetrievalOutputKind::Structured
                            | RetrievalOutputKind::Handoff
                    )
                }) =>
            {
                bail!("read retrieval actions must produce readable content or a handoff")
            },
            _ => {},
        }
        if let Some(cost) = &self.estimated_cost {
            cost.validate()?;
        }
        Ok(())
    }
}

fn default_retrieval_priority() -> u16 {
    1_000
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentSourceCapabilities {
    pub discovery: bool,
    pub full_content: bool,
    pub cursor: bool,
    pub conditional_fetch: bool,
    pub execution: AdapterExecution,
    pub auth: AdapterAuth,
    /// True only when the adapter sends the user's intent or derived query to
    /// a remote service. RSS fetches a remote URL but leaves this false.
    pub sends_user_intent: bool,
    pub metered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentSourceDescriptor {
    pub adapter_id: String,
    pub display_name: String,
    pub class: ContentSourceClass,
    pub capabilities: ContentSourceCapabilities,
    pub retrieval: RetrievalActionMetadata,
}

impl ContentSourceDescriptor {
    pub fn validate(&self) -> Result<()> {
        if self.adapter_id.trim().is_empty() {
            bail!("content source adapter_id must not be empty");
        }
        if self.adapter_id.chars().count() > MAX_ADAPTER_ID_CHARS {
            bail!("content source adapter_id exceeds {MAX_ADAPTER_ID_CHARS} characters");
        }
        if self.adapter_id.chars().any(char::is_control) {
            bail!("content source adapter_id must not contain control characters");
        }
        if self.display_name.trim().is_empty() {
            bail!("content source display_name must not be empty");
        }
        if self.display_name.chars().count() > MAX_DISPLAY_NAME_CHARS
            || self.display_name.chars().any(char::is_control)
        {
            bail!(
                "content source display_name must be at most {MAX_DISPLAY_NAME_CHARS} characters \
                 and contain no controls"
            );
        }
        self.retrieval.validate()?;
        match self.retrieval.operation {
            RetrievalOperation::Discover if !self.capabilities.discovery => {
                bail!("discovery retrieval metadata requires discovery capability")
            },
            RetrievalOperation::Read if !self.capabilities.full_content => {
                bail!("read retrieval metadata requires full-content capability")
            },
            _ => {},
        }
        if self.retrieval.estimated_cost.is_some() && !self.capabilities.metered {
            bail!("only metered content actions may declare estimated cost")
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentPrivacy {
    Public,
    Private,
    Restricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDataPolicy {
    Deny,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessPolicy {
    CachedOk,
    Fresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentInvocationSource {
    UserFeed,
    RecurringMonitor,
    ObservedSource,
    InteractiveRead,
    InternalSystem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub adapter_id: String,
    pub item_id: String,
}

impl SourceIdentity {
    pub fn new(adapter_id: impl Into<String>, item_id: impl Into<String>) -> Result<Self> {
        let identity = Self {
            adapter_id: adapter_id.into(),
            item_id: item_id.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<()> {
        if self.adapter_id.trim().is_empty() || self.item_id.trim().is_empty() {
            bail!("source identity requires non-empty adapter_id and item_id");
        }
        if self.adapter_id.chars().count() > MAX_ADAPTER_ID_CHARS {
            bail!("source adapter_id exceeds {MAX_ADAPTER_ID_CHARS} characters");
        }
        if self.item_id.chars().count() > MAX_SOURCE_ITEM_ID_CHARS {
            bail!("source item_id exceeds {MAX_SOURCE_ITEM_ID_CHARS} characters");
        }
        if self.adapter_id.chars().any(char::is_control)
            || self.item_id.chars().any(char::is_control)
        {
            bail!("source identity must not contain control characters");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentProvenance {
    pub source_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub retrieved_by: String,
}

impl ContentProvenance {
    fn validate(&self) -> Result<()> {
        validate_bounded_label(
            &self.source_label,
            "content provenance source_label",
            MAX_PROVENANCE_LABEL_CHARS,
        )?;
        validate_bounded_label(
            &self.retrieved_by,
            "content provenance retrieved_by",
            MAX_PROVENANCE_RETRIEVER_CHARS,
        )?;
        if let Some(source_url) = self.source_url.as_deref() {
            validate_content_url(source_url, "content provenance source_url")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentCandidate {
    pub schema_version: u32,
    pub identity: SourceIdentity,
    pub title: String,
    /// The cheapest text available for selection: a distilled comms summary,
    /// RSS summary, or search snippet. It is never an owned-channel raw body.
    pub cheap_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at_ms: Option<i64>,
    pub observed_at_ms: i64,
    pub privacy: ContentPrivacy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub provenance: ContentProvenance,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl ContentCandidate {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONTENT_SOURCE_SCHEMA_VERSION {
            bail!(
                "unsupported content candidate schema version {}",
                self.schema_version
            );
        }
        self.identity.validate()?;
        if self.title.trim().is_empty() {
            bail!("content candidate title must not be empty");
        }
        if self.title.chars().count() > MAX_CANDIDATE_TITLE_CHARS {
            bail!("content candidate title exceeds {MAX_CANDIDATE_TITLE_CHARS} characters");
        }
        if self.cheap_text.trim().is_empty() {
            bail!("content candidate cheap_text must not be empty");
        }
        if self.cheap_text.chars().count() > MAX_CANDIDATE_CHEAP_TEXT_CHARS {
            bail!(
                "content candidate cheap_text exceeds {MAX_CANDIDATE_CHEAP_TEXT_CHARS} characters"
            );
        }
        if self.observed_at_ms <= 0 {
            bail!("content candidate requires a positive observed_at_ms");
        }
        if self.published_at_ms.is_some_and(|value| value <= 0) {
            bail!("content candidate published_at_ms must be positive when present");
        }
        self.provenance.validate()?;
        validate_optional_content_hash(self.content_hash.as_deref())?;
        validate_metadata_size(&self.metadata)?;
        if let Some(url) = &self.canonical_url {
            validate_content_url(url, "content candidate canonical_url")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDocument {
    pub schema_version: u32,
    pub identity: SourceIdentity,
    pub title: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub fetched_at_ms: i64,
    pub privacy: ContentPrivacy,
    pub content_hash: String,
    pub provenance: ContentProvenance,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl ContentDocument {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONTENT_SOURCE_SCHEMA_VERSION {
            bail!(
                "unsupported content document schema version {}",
                self.schema_version
            );
        }
        self.identity.validate()?;
        if self.title.trim().is_empty() || self.text.trim().is_empty() {
            bail!("content document requires non-empty title and text");
        }
        if self.title.chars().count() > MAX_CANDIDATE_TITLE_CHARS {
            bail!("content document title exceeds {MAX_CANDIDATE_TITLE_CHARS} characters");
        }
        if self.text.chars().count() > MAX_DOCUMENT_TEXT_CHARS {
            bail!("content document text exceeds {MAX_DOCUMENT_TEXT_CHARS} characters");
        }
        if self.content_hash.trim().is_empty() {
            bail!("content document requires a content hash");
        }
        if self.fetched_at_ms <= 0 {
            bail!("content document requires a positive fetched_at_ms");
        }
        validate_optional_content_hash(Some(&self.content_hash))?;
        self.provenance.validate()?;
        validate_metadata_size(&self.metadata)?;
        if let Some(url) = &self.canonical_url {
            validate_content_url(url, "content document canonical_url")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterCost {
    pub commodity: String,
    /// Integer micros keep the transport deterministic. For USD this is
    /// millionths of one dollar; counted providers may use their own named
    /// commodity and integer unit.
    pub amount_microunits: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryValidator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl DiscoveryValidator {
    pub fn validate(&self) -> Result<()> {
        for value in [self.etag.as_deref(), self.last_modified.as_deref()]
            .into_iter()
            .flatten()
        {
            if value.trim().is_empty()
                || value.chars().count() > 1_024
                || value
                    .chars()
                    .any(|character| character == '\r' || character == '\n')
            {
                bail!("discovery validator is empty, oversized, or contains a line break");
            }
        }
        Ok(())
    }
}

impl AdapterCost {
    pub fn validate(&self) -> Result<()> {
        validate_bounded_label(
            &self.commodity,
            "adapter cost commodity",
            MAX_COST_COMMODITY_CHARS,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryRequest {
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub validators: BTreeMap<String, DiscoveryValidator>,
    pub limit: usize,
    pub freshness: FreshnessPolicy,
    pub remote_query_policy: RemoteDataPolicy,
    pub invocation_source: ContentInvocationSource,
    #[serde(default)]
    pub options: BTreeMap<String, Value>,
}

impl DiscoveryRequest {
    pub fn validate(&self) -> Result<()> {
        validate_content_scope_component(&self.principal, "discovery principal")?;
        validate_content_scope_component(&self.workspace, "discovery workspace")?;
        if self.limit == 0 || self.limit > MAX_DISCOVERY_ITEMS {
            bail!(
                "discovery limit must be between 1 and {MAX_DISCOVERY_ITEMS}, got {}",
                self.limit
            );
        }
        if self.targets.len() > MAX_DISCOVERY_TARGETS {
            bail!(
                "discovery request has {} targets above limit {MAX_DISCOVERY_TARGETS}",
                self.targets.len()
            );
        }
        if self.validators.len() > self.targets.len()
            || self.validators.iter().any(|(target, validator)| {
                !self.targets.iter().any(|candidate| candidate == target)
                    || validator.validate().is_err()
            })
        {
            bail!("discovery validators must be bounded and belong to request targets");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryTransportStats {
    #[serde(default)]
    pub modified_targets: u64,
    #[serde(default)]
    pub not_modified_targets: u64,
    #[serde(default)]
    pub response_bytes: u64,
}

impl Default for DiscoveryTransportStats {
    fn default() -> Self {
        Self {
            modified_targets: 0,
            not_modified_targets: 0,
            response_bytes: 0,
        }
    }
}

impl DiscoveryTransportStats {
    fn is_empty(&self) -> bool {
        self.modified_targets == 0 && self.not_modified_targets == 0 && self.response_bytes == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryPage {
    pub items: Vec<ContentCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub validators: BTreeMap<String, DiscoveryValidator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AdapterCost>,
    #[serde(default, skip_serializing_if = "DiscoveryTransportStats::is_empty")]
    pub transport: DiscoveryTransportStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadDepth {
    Gist,
    FullText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSelectionReason {
    FeedMatch,
    MonitorMatch,
    ObserveMatch,
    UserRequested,
    InternalPolicy,
}

/// Receipt proving that a complete external candidate passed a consumer-owned
/// selection stage before a potentially expensive full-content read. The
/// receipt also protects the candidate's inline selection text from mutation.
/// Direct user reads are explicit intent, but their caller-supplied text is not
/// authoritative evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadSelectionEvidence {
    pub selected_at_ms: i64,
    pub reason: ReadSelectionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance_score: Option<f64>,
}

impl ReadSelectionEvidence {
    pub fn validate(&self) -> Result<()> {
        if self.selected_at_ms <= 0 {
            bail!("read selection evidence requires a positive selected_at_ms");
        }
        if let Some(score) = self.relevance_score {
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                bail!("read selection relevance_score must be finite and between 0 and 1");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadRequest {
    pub principal: String,
    pub workspace: String,
    pub candidate: ContentCandidate,
    pub depth: ReadDepth,
    pub freshness: FreshnessPolicy,
    pub remote_content_policy: RemoteDataPolicy,
    pub invocation_source: ContentInvocationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ReadSelectionEvidence>,
    /// Opaque process-issued grant consumed only by identity-bearing readers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_grant_id: Option<String>,
}

impl ReadRequest {
    pub fn validate(&self) -> Result<()> {
        validate_content_scope_component(&self.principal, "read principal")?;
        validate_content_scope_component(&self.workspace, "read workspace")?;
        self.candidate.validate()?;
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        if self
            .authority_grant_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty() || id.len() > 128)
        {
            bail!("read authority grant id is invalid");
        }
        if matches!(
            self.invocation_source,
            ContentInvocationSource::UserFeed
                | ContentInvocationSource::RecurringMonitor
                | ContentInvocationSource::ObservedSource
        ) && self.selection.is_none()
        {
            bail!("feed, monitor, and observed-source reads require post-selection evidence");
        }
        match (
            self.invocation_source,
            self.selection.as_ref().map(|selection| selection.reason),
        ) {
            (ContentInvocationSource::UserFeed, Some(ReadSelectionReason::FeedMatch))
            | (
                ContentInvocationSource::RecurringMonitor,
                Some(ReadSelectionReason::MonitorMatch),
            )
            | (ContentInvocationSource::ObservedSource, Some(ReadSelectionReason::ObserveMatch))
            | (
                ContentInvocationSource::InteractiveRead,
                None | Some(ReadSelectionReason::UserRequested),
            )
            | (ContentInvocationSource::InternalSystem, _) => {},
            (ContentInvocationSource::UserFeed, Some(_)) => {
                bail!("user-feed reads require feed_match selection evidence")
            },
            (ContentInvocationSource::RecurringMonitor, Some(_)) => {
                bail!("recurring-monitor reads require monitor_match selection evidence")
            },
            (ContentInvocationSource::ObservedSource, Some(_)) => {
                bail!("observed-source reads require observe_match selection evidence")
            },
            (ContentInvocationSource::InteractiveRead, Some(_)) => {
                bail!("interactive reads accept only user_requested selection evidence")
            },
            // Missing post-selection evidence is handled above.
            (
                ContentInvocationSource::UserFeed
                | ContentInvocationSource::RecurringMonitor
                | ContentInvocationSource::ObservedSource,
                None,
            ) => {},
        }
        Ok(())
    }
}

/// Canonicalize an HTTP(S) URL for exact duplicate suppression. Tracking-only
/// parameters and fragments are removed; semantic query parameters are kept.
pub fn canonicalize_http_url(raw: &str) -> Result<String> {
    if raw.chars().count() > MAX_CONTENT_URL_CHARS {
        bail!("content URL exceeds {MAX_CONTENT_URL_CHARS} characters");
    }
    let mut url = Url::parse(raw).with_context(|| format!("parsing URL `{raw}`"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("content URL must use http or https");
    }
    if url.host_str().is_none() {
        bail!("content URL requires a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("content URL must not contain embedded credentials");
    }
    url.set_fragment(None);

    let retained = url
        .query_pairs()
        .filter(|(key, _)| !is_tracking_parameter(key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !retained.is_empty() {
        url.query_pairs_mut().extend_pairs(retained);
    }

    Ok(url.to_string())
}

fn is_tracking_parameter(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "fbclid" | "gclid" | "dclid" | "msclkid" | "mc_cid" | "mc_eid"
        )
}

fn validate_metadata_size(metadata: &BTreeMap<String, Value>) -> Result<()> {
    let bytes = serde_json::to_vec(metadata).context("serializing content metadata")?;
    if bytes.len() > MAX_METADATA_BYTES {
        bail!("content metadata exceeds {MAX_METADATA_BYTES} bytes");
    }
    Ok(())
}

fn validate_bounded_label(value: &str, label: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        bail!("{label} must be at most {max_chars} characters and contain no controls");
    }
    Ok(())
}

fn validate_content_url(value: &str, label: &str) -> Result<()> {
    canonicalize_http_url(value).with_context(|| format!("validating {label}"))?;
    Ok(())
}

fn validate_optional_content_hash(value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.trim().is_empty()
        || value.chars().count() > MAX_CONTENT_HASH_CHARS
        || value.chars().any(char::is_control)
    {
        bail!(
            "content hash must be non-empty, at most {MAX_CONTENT_HASH_CHARS} characters, and \
             contain no controls"
        );
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn scope_components_reject_values_that_workspace_paths_would_alias() {
        for value in [
            "alice/admin",
            "alice_admin..",
            " alice",
            "..",
            "a:b",
            "a\\b",
        ] {
            assert!(validate_content_scope_component(value, "principal").is_err());
        }
        assert!(validate_content_scope_component("alice_admin@example.com", "principal").is_ok());
        assert!(validate_content_scope_component(&"é".repeat(128), "principal").is_err());
        assert!(validate_content_scope_component(&"a".repeat(255), "principal").is_ok());
    }

    #[test]
    fn canonical_url_removes_fragment_and_tracking_without_losing_semantics() {
        let canonical =
            canonicalize_http_url("https://example.com/story?id=42&utm_source=newsletter#section")
                .unwrap();
        assert_eq!(canonical, "https://example.com/story?id=42");
    }

    #[test]
    fn canonical_url_rejects_embedded_credentials() {
        assert!(canonicalize_http_url("https://user:secret@example.com/story").is_err());
    }

    #[test]
    fn discovery_request_rejects_unbounded_limits() {
        let request = DiscoveryRequest {
            principal: "p".into(),
            workspace: "w".into(),
            intent: None,
            query: None,
            targets: Vec::new(),
            cursor: None,
            validators: BTreeMap::new(),
            limit: MAX_DISCOVERY_ITEMS + 1,
            freshness: FreshnessPolicy::CachedOk,
            remote_query_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::InternalSystem,
            options: BTreeMap::new(),
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn deserialized_empty_identity_is_rejected() {
        let candidate = ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity {
                adapter_id: String::new(),
                item_id: "item".into(),
            },
            title: "Title".into(),
            cheap_text: "Summary".into(),
            canonical_url: None,
            published_at_ms: None,
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: ContentProvenance {
                source_label: "Source".into(),
                source_url: None,
                retrieved_by: "adapter".into(),
            },
            metadata: BTreeMap::new(),
        };
        assert!(candidate.validate().is_err());
    }

    #[test]
    fn feed_reads_require_valid_selection_evidence() {
        let candidate = ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new("search", "item").unwrap(),
            title: "Title".into(),
            cheap_text: "Summary".into(),
            canonical_url: Some("https://example.com/item".into()),
            published_at_ms: None,
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: ContentProvenance {
                source_label: "Example".into(),
                source_url: Some("https://example.com/item".into()),
                retrieved_by: "search".into(),
            },
            metadata: BTreeMap::new(),
        };
        let mut request = ReadRequest {
            principal: "p".into(),
            workspace: "w".into(),
            candidate,
            depth: ReadDepth::FullText,
            freshness: FreshnessPolicy::CachedOk,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::UserFeed,
            selection: None,
            authority_grant_id: None,
        };
        assert!(request.validate().is_err());
        request.selection = Some(ReadSelectionEvidence {
            selected_at_ms: 1,
            reason: ReadSelectionReason::FeedMatch,
            relevance_score: Some(0.8),
        });
        assert!(request.validate().is_ok());
        request.selection.as_mut().unwrap().relevance_score = Some(f64::NAN);
        assert!(request.validate().is_err());
    }

    #[test]
    fn read_request_requires_an_explicit_invocation_source_on_deserialization() {
        let payload = serde_json::json!({
            "principal": "p",
            "workspace": "w",
            "candidate": {
                "schema_version": CONTENT_SOURCE_SCHEMA_VERSION,
                "identity": {"adapter_id": "search", "item_id": "item"},
                "title": "Title",
                "cheap_text": "Summary",
                "observed_at_ms": 1,
                "privacy": "public",
                "provenance": {"source_label": "Source", "retrieved_by": "search"}
            },
            "depth": "full_text",
            "freshness": "cached_ok",
            "remote_content_policy": "deny"
        });
        assert!(serde_json::from_value::<ReadRequest>(payload).is_err());
    }

    #[test]
    fn candidate_rejects_unbounded_provenance_and_invalid_observation_time() {
        let mut candidate = ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new("search", "item").unwrap(),
            title: "Title".into(),
            cheap_text: "Summary".into(),
            canonical_url: Some("https://example.com/item".into()),
            published_at_ms: None,
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: Some("hash".into()),
            provenance: ContentProvenance {
                source_label: "Source".into(),
                source_url: Some("https://example.com/feed".into()),
                retrieved_by: "search".into(),
            },
            metadata: BTreeMap::new(),
        };
        candidate.provenance.source_label = "x".repeat(MAX_PROVENANCE_LABEL_CHARS + 1);
        assert!(candidate.validate().is_err());

        candidate.provenance.source_label = "Source".into();
        candidate.observed_at_ms = 0;
        assert!(candidate.validate().is_err());

        candidate.observed_at_ms = 1;
        candidate.published_at_ms = Some(0);
        assert!(candidate.validate().is_err());
    }

    #[test]
    fn content_url_and_cost_commodity_are_bounded() {
        let oversized_url = format!("https://example.com/{}", "x".repeat(MAX_CONTENT_URL_CHARS));
        assert!(canonicalize_http_url(&oversized_url).is_err());
        assert!(AdapterCost {
            commodity: " ".into(),
            amount_microunits: 0,
        }
        .validate()
        .is_err());
    }
}
