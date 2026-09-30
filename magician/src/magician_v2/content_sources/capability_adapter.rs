#[cfg(any(test, feature = "test-fixtures"))]
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};
use tool_runtime_core::manifest_parser::parse_skill_runtime_package;
use url::Url;

#[cfg(any(test, feature = "test-fixtures"))]
use super::registry::ContentSourceRegistry;
use super::{
    traits::DiscoveryAdapter,
    types::{
        canonicalize_http_url, AdapterAuth, AdapterCost, AdapterExecution, ContentCandidate,
        ContentInvocationSource, ContentPrivacy, ContentProvenance, ContentSourceCapabilities,
        ContentSourceClass, ContentSourceDescriptor, DiscoveryPage, DiscoveryRequest,
        RemoteDataPolicy, RetrievalActionMetadata, RetrievalAuthority, RetrievalRung,
        SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION, MAX_CANDIDATE_CHEAP_TEXT_CHARS,
        MAX_CANDIDATE_TITLE_CHARS, MAX_DISCOVERY_ITEMS,
    },
};
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::skills::embedded_extensions::discover_skill_markdown_paths;
use crate::magician_v2::{
    execution::{
        capability::NativeActionSchemaDef,
        compiled_providers::project_runtime_package_to_pack,
        primitive_dispatch::{
            DeterministicCapabilityInvocation, DeterministicCapabilityInvocationSource,
            DeterministicCapabilityInvoker,
        },
        CapabilityPackDefinition, ImplementationType,
    },
    skills::embedded_extensions::{
        load_skill_magician_extension, owning_skill_name, read_bounded_skill_markdown,
    },
};

pub const CAPABILITY_DISCOVERY_EXTENSION: &str = "content_source";
pub const CAPABILITY_DISCOVERY_MANIFEST_SCHEMA_VERSION: u32 = 1;
const DEFAULT_MAX_QUERY_CHARS: usize = 8 * 1024;
const MAX_CONFIGURED_QUERY_CHARS: usize = 64 * 1024;
const MAX_CURSOR_CHARS: usize = 8 * 1024;
const MAX_OPTION_STRING_CHARS: usize = 4 * 1024;
const MAX_OPTION_LIST_ITEMS: usize = 128;
const MAX_OPTION_LIST_ITEM_CHARS: usize = 2 * 1024;

/// Versioned declaration that turns any deterministic CLI-template capability
/// into a discovery adapter without adding provider-specific Rust code.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDiscoveryManifest {
    pub schema_version: u32,
    pub adapter: CapabilityAdapterManifest,
    pub capability: CapabilityBindingManifest,
    #[serde(default)]
    pub input: CapabilityInputManifest,
    pub output: CapabilityOutputManifest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityAdapterManifest {
    pub id: String,
    pub display_name: String,
    pub class: ContentSourceClass,
    pub execution: AdapterExecution,
    pub auth: AdapterAuth,
    #[serde(default)]
    pub sends_user_intent: bool,
    #[serde(default)]
    pub metered: bool,
    #[serde(default)]
    pub cursor: bool,
    #[serde(default = "default_public_privacy")]
    pub privacy: ContentPrivacy,
    pub max_results: usize,
    /// Optional Phase 7 controller projection. Older source manifests retain
    /// deterministic defaults based on their declared source class.
    #[serde(default)]
    pub retrieval: Option<RetrievalActionMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBindingManifest {
    pub name: String,
    pub action: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInputManifest {
    #[serde(default = "default_query_argument")]
    pub query_argument: Option<String>,
    #[serde(default)]
    pub limit_argument: Option<String>,
    #[serde(default)]
    pub cursor_argument: Option<String>,
    #[serde(default)]
    pub targets_argument: Option<String>,
    #[serde(default)]
    pub targets_join: Option<String>,
    #[serde(default = "default_max_query_chars")]
    pub max_query_chars: usize,
    #[serde(default)]
    pub fixed_arguments: BTreeMap<String, Value>,
    #[serde(default)]
    pub options: BTreeMap<String, CapabilityOptionManifest>,
}

impl Default for CapabilityInputManifest {
    fn default() -> Self {
        Self {
            query_argument: default_query_argument(),
            limit_argument: None,
            cursor_argument: None,
            targets_argument: None,
            targets_join: None,
            max_query_chars: default_max_query_chars(),
            fixed_arguments: BTreeMap::new(),
            options: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityOptionManifest {
    pub argument: String,
    pub value_type: CapabilityOptionType,
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub join: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityOptionType {
    String,
    Boolean,
    PositiveInteger,
    StringList,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityOutputManifest {
    pub mode: CapabilityOutputMode,
    #[serde(default)]
    pub page_pointer: Option<String>,
    #[serde(default)]
    pub error_pointer: Option<String>,
    #[serde(default)]
    pub items_pointer: Option<String>,
    #[serde(default)]
    pub next_cursor_pointer: Option<String>,
    #[serde(default)]
    pub item: Option<MappedItemManifest>,
    #[serde(default)]
    pub response_metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub cost: Option<CapabilityCostManifest>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityOutputMode {
    Mapped,
    CanonicalV1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedItemManifest {
    #[serde(default)]
    pub source_item_id_pointer: Option<String>,
    #[serde(default)]
    pub title_pointer: Option<String>,
    pub url_pointer: String,
    #[serde(default)]
    pub cheap_text_pointers: Vec<String>,
    #[serde(default)]
    pub published_at_pointer: Option<String>,
    #[serde(default)]
    pub source_label_pointer: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCostManifest {
    pub pointer: String,
    pub commodity: String,
    pub encoding: CapabilityCostEncoding,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityCostEncoding {
    DecimalMajorUnits,
    Microunits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalCapabilityPage {
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    cost: Option<AdapterCost>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalCapabilityItem {
    #[serde(default)]
    source_item_id: Option<String>,
    title: String,
    cheap_text: String,
    canonical_url: String,
    #[serde(default)]
    published_at_ms: Option<i64>,
    #[serde(default)]
    source_label: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, Value>,
}

pub struct CapabilityDiscoveryAdapter {
    descriptor: ContentSourceDescriptor,
    manifest: CapabilityDiscoveryManifest,
    invoker: Arc<dyn DeterministicCapabilityInvoker>,
}

impl CapabilityDiscoveryManifest {
    pub fn from_yaml_str(value: &str) -> Result<Self> {
        let manifest: Self =
            serde_yaml::from_str(value).context("decoding capability discovery manifest YAML")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn descriptor(&self) -> ContentSourceDescriptor {
        let mut retrieval = self.adapter.retrieval.clone().unwrap_or_else(|| {
            let rung = match self.adapter.class {
                ContentSourceClass::WebSearch => RetrievalRung::PublicSearch,
                _ => RetrievalRung::SourceNative,
            };
            let authority = match self.adapter.auth {
                AdapterAuth::Required => RetrievalAuthority::AuthenticatedRead,
                AdapterAuth::None | AdapterAuth::Optional => {
                    if self.adapter.execution == AdapterExecution::LocalProcess {
                        RetrievalAuthority::LocalOnly
                    } else {
                        RetrievalAuthority::PublicRemoteRead
                    }
                },
            };
            RetrievalActionMetadata::discovery(
                format!("{}.discover", self.adapter.id),
                rung,
                authority,
                true,
            )
        });
        if retrieval.accepted_options.is_empty() {
            retrieval.accepted_options = self.input.options.keys().cloned().collect();
        }
        ContentSourceDescriptor {
            adapter_id: self.adapter.id.clone(),
            display_name: self.adapter.display_name.clone(),
            class: self.adapter.class,
            capabilities: ContentSourceCapabilities {
                discovery: true,
                full_content: false,
                cursor: self.adapter.cursor,
                conditional_fetch: false,
                execution: self.adapter.execution,
                auth: self.adapter.auth,
                sends_user_intent: self.adapter.sends_user_intent,
                metered: self.adapter.metered,
            },
            retrieval,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CAPABILITY_DISCOVERY_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported capability discovery manifest schema version {}",
                self.schema_version
            );
        }
        self.descriptor().validate()?;
        if !self.adapter.id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.')
        }) {
            bail!("capability discovery adapter id contains unsupported characters");
        }
        validate_name(&self.adapter.display_name, "adapter display_name")?;
        validate_name(&self.capability.name, "capability name")?;
        validate_name(&self.capability.action, "capability action")?;
        if self.adapter.execution == AdapterExecution::RemoteEndpoint
            && self.input.query_argument.is_some()
            && !self.adapter.sends_user_intent
        {
            bail!("remote query adapters must declare sends_user_intent: true");
        }
        if self.adapter.max_results == 0 || self.adapter.max_results > MAX_DISCOVERY_ITEMS {
            bail!("capability discovery max_results must be between 1 and {MAX_DISCOVERY_ITEMS}");
        }
        if self.input.max_query_chars == 0
            || self.input.max_query_chars > MAX_CONFIGURED_QUERY_CHARS
        {
            bail!(
                "capability discovery max_query_chars must be between 1 and \
                 {MAX_CONFIGURED_QUERY_CHARS}"
            );
        }
        validate_optional_name(&self.input.query_argument, "query_argument")?;
        validate_optional_name(&self.input.limit_argument, "limit_argument")?;
        validate_optional_name(&self.input.cursor_argument, "cursor_argument")?;
        validate_optional_name(&self.input.targets_argument, "targets_argument")?;
        if self.adapter.cursor != self.input.cursor_argument.is_some() {
            bail!(
                "cursor capability and cursor_argument must either both be set or both be absent"
            );
        }
        if self.input.targets_join.is_some() && self.input.targets_argument.is_none() {
            bail!("targets_join requires targets_argument");
        }
        for key in self.input.fixed_arguments.keys() {
            validate_name(key, "fixed argument")?;
        }
        let mut dynamic_arguments = BTreeSet::new();
        for argument in [
            self.input.query_argument.as_deref(),
            self.input.limit_argument.as_deref(),
            self.input.cursor_argument.as_deref(),
            self.input.targets_argument.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !dynamic_arguments.insert(argument) {
                bail!("capability discovery input arguments must be unique");
            }
        }
        let mut option_arguments = BTreeSet::new();
        for (option_name, option) in &self.input.options {
            validate_name(option_name, "option")?;
            validate_name(&option.argument, "option argument")?;
            if dynamic_arguments.contains(option.argument.as_str())
                || !option_arguments.insert(option.argument.as_str())
            {
                bail!("option `{option_name}` maps to a duplicate input argument");
            }
            if !option.allowed.is_empty() && option.value_type != CapabilityOptionType::String {
                bail!("option `{option_name}` may declare allowed values only for string input");
            }
            if option.join.is_some() && option.value_type != CapabilityOptionType::StringList {
                bail!("option `{option_name}` may declare join only for string_list input");
            }
        }

        validate_optional_pointer(&self.output.page_pointer, "page_pointer")?;
        validate_optional_pointer(&self.output.error_pointer, "error_pointer")?;
        validate_optional_pointer(&self.output.next_cursor_pointer, "next_cursor_pointer")?;
        for (key, pointer) in &self.output.response_metadata {
            validate_name(key, "response metadata key")?;
            validate_pointer(pointer, "response metadata pointer")?;
        }
        if let Some(cost) = &self.output.cost {
            validate_pointer(&cost.pointer, "cost pointer")?;
            validate_name(&cost.commodity, "cost commodity")?;
        }

        match self.output.mode {
            CapabilityOutputMode::Mapped => {
                if self.adapter.metered && self.output.cost.is_none() {
                    bail!("metered mapped output requires a cost mapping");
                }
                validate_pointer(
                    self.output
                        .items_pointer
                        .as_deref()
                        .ok_or_else(|| anyhow!("mapped output requires items_pointer"))?,
                    "items_pointer",
                )?;
                let item = self
                    .output
                    .item
                    .as_ref()
                    .ok_or_else(|| anyhow!("mapped output requires item mappings"))?;
                validate_pointer(&item.url_pointer, "item url_pointer")?;
                validate_optional_pointer(&item.source_item_id_pointer, "source_item_id_pointer")?;
                validate_optional_pointer(&item.title_pointer, "title_pointer")?;
                validate_optional_pointer(&item.published_at_pointer, "published_at_pointer")?;
                validate_optional_pointer(&item.source_label_pointer, "source_label_pointer")?;
                for pointer in &item.cheap_text_pointers {
                    validate_pointer(pointer, "cheap_text pointer")?;
                }
                for (key, pointer) in &item.metadata {
                    validate_name(key, "item metadata key")?;
                    validate_pointer(pointer, "item metadata pointer")?;
                }
            },
            CapabilityOutputMode::CanonicalV1 => {
                if self.output.items_pointer.is_some()
                    || self.output.next_cursor_pointer.is_some()
                    || self.output.item.is_some()
                    || !self.output.response_metadata.is_empty()
                    || self.output.cost.is_some()
                {
                    bail!("canonical_v1 output must not declare mapped item fields");
                }
            },
        }
        Ok(())
    }
}

impl CapabilityDiscoveryAdapter {
    pub fn new(
        manifest: CapabilityDiscoveryManifest,
        invoker: Arc<dyn DeterministicCapabilityInvoker>,
    ) -> Result<Self> {
        manifest.validate()?;
        Ok(Self {
            descriptor: manifest.descriptor(),
            manifest,
            invoker,
        })
    }
}

#[async_trait]
impl DiscoveryAdapter for CapabilityDiscoveryAdapter {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage> {
        request.validate()?;
        if request.limit > self.manifest.adapter.max_results {
            bail!(
                "discovery adapter `{}` limit must not exceed {}",
                self.descriptor.adapter_id,
                self.manifest.adapter.max_results
            );
        }
        if request.cursor.is_some() && !self.descriptor.capabilities.cursor {
            bail!(
                "discovery adapter `{}` does not support cursors",
                self.descriptor.adapter_id
            );
        }
        if self.descriptor.capabilities.sends_user_intent
            && request.remote_query_policy != RemoteDataPolicy::Allow
        {
            bail!(
                "discovery adapter `{}` requires permission to send the query remotely",
                self.descriptor.adapter_id
            );
        }

        let arguments = build_capability_arguments(&self.manifest, request)?;
        let invocation = DeterministicCapabilityInvocation::new(
            &self.manifest.capability.name,
            &self.manifest.capability.action,
            Value::Object(arguments),
            invocation_source(request.invocation_source),
        )
        .with_scope(&request.principal, &request.workspace)
        .with_execution_id(format!(
            "content-source:{}:{}",
            self.descriptor.adapter_id,
            ulid::Ulid::new()
        ));
        let result = self.invoker.invoke(invocation).await.map_err(|error| {
            anyhow!(
                "invoking capability `{}` for discovery adapter `{}`: {error}",
                self.manifest.capability.name,
                self.descriptor.adapter_id
            )
        })?;
        let payload = result.parsed_json().with_context(|| {
            format!(
                "capability `{}` returned invalid JSON",
                self.manifest.capability.name
            )
        })?;
        let page = normalize_capability_output(&self.manifest, payload, request.limit)?;
        if page.next_cursor.is_some() && !self.descriptor.capabilities.cursor {
            bail!(
                "discovery adapter `{}` returned a cursor without declaring cursor capability",
                self.descriptor.adapter_id
            );
        }
        Ok(page)
    }
}

pub fn load_capability_discovery_manifest(path: &Path) -> Result<CapabilityDiscoveryManifest> {
    load_optional_capability_discovery_manifest(path)?.ok_or_else(|| {
        anyhow!(
            "governed skill `{}` does not declare \
             metadata.magician.{CAPABILITY_DISCOVERY_EXTENSION}",
            path.display()
        )
    })
}

pub fn load_optional_capability_discovery_manifest(
    path: &Path,
) -> Result<Option<CapabilityDiscoveryManifest>> {
    let Some(manifest) = load_skill_magician_extension::<CapabilityDiscoveryManifest>(
        path,
        CAPABILITY_DISCOVERY_EXTENSION,
    )?
    else {
        return Ok(None);
    };
    manifest.validate().with_context(|| {
        format!(
            "validating metadata.magician.{CAPABILITY_DISCOVERY_EXTENSION} in `{}`",
            path.display()
        )
    })?;
    Ok(Some(manifest))
}

/// Discover and register every embedded `metadata.magician.content_source`
/// declaration below the supplied skill roots. A root may also be one concrete
/// skill directory. Roots are ordered from highest to lowest priority; the
/// first occurrence of a skill name wins, matching normal scoped-over-system
/// skill shadowing. Scanning is deterministic and does not follow symlinked
/// skill directories. Manifest file symlinks are supported because the standard
/// skill installer uses file-level links back to the trusted source tree.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn register_capability_discovery_manifests(
    registry: &mut ContentSourceRegistry,
    invoker: Arc<dyn DeterministicCapabilityInvoker>,
    roots: &[PathBuf],
) -> Result<Vec<String>> {
    let paths = capability_discovery_manifest_paths(roots)?;
    let existing = registry
        .discovery_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.adapter_id)
        .collect::<BTreeSet<_>>();
    let mut manifests = Vec::new();
    let mut discovered = BTreeSet::new();
    for path in paths {
        let manifest = validated_capability_discovery_manifest(&path)?;
        let id = manifest.adapter.id.clone();
        if existing.contains(&id) || !discovered.insert(id.clone()) {
            bail!("duplicate capability discovery adapter `{id}`");
        }
        manifests.push(manifest);
    }

    let ids = manifests
        .iter()
        .map(|manifest| manifest.adapter.id.clone())
        .collect::<Vec<_>>();
    for manifest in manifests {
        registry.register_discovery(Arc::new(CapabilityDiscoveryAdapter::new(
            manifest,
            invoker.clone(),
        )?))?;
    }
    Ok(ids)
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn validated_capability_discovery_manifest(path: &Path) -> Result<CapabilityDiscoveryManifest> {
    optional_validated_capability_discovery_manifest(path)?.ok_or_else(|| {
        anyhow!(
            "governed skill `{}` does not declare \
             metadata.magician.{CAPABILITY_DISCOVERY_EXTENSION}",
            path.display()
        )
    })
}

pub fn optional_validated_capability_discovery_manifest(
    path: &Path,
) -> Result<Option<CapabilityDiscoveryManifest>> {
    let Some(manifest) = load_optional_capability_discovery_manifest(path)? else {
        return Ok(None);
    };
    let owning_skill = owning_skill_name(path)?;
    if owning_skill != manifest.capability.name.as_str() {
        bail!(
            "skill `{}` binds content source capability `{}` but is owned by `{owning_skill}`",
            path.display(),
            manifest.capability.name
        );
    }
    let skill_dir = path.parent().expect("owning skill checked above");
    validate_skill_owner(skill_dir, &owning_skill, &manifest)?;
    Ok(Some(manifest))
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn capability_discovery_manifest_paths(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    discover_skill_markdown_paths(roots)?
        .into_iter()
        .filter_map(|path| {
            match load_skill_magician_extension::<CapabilityDiscoveryManifest>(
                &path,
                CAPABILITY_DISCOVERY_EXTENSION,
            ) {
                Ok(Some(_)) => Some(Ok(path)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn validate_skill_owner(
    skill_dir: &Path,
    expected_name: &str,
    manifest: &CapabilityDiscoveryManifest,
) -> Result<()> {
    let skill_path = skill_dir.join("SKILL.md");
    let source = read_bounded_skill_markdown(&skill_path)?;
    let resolved_skill_path =
        crate::magician_v2::skills::path_rewrite::resolve_skill_path(&skill_path);
    let skill_manifest = crate::magician_v2::skills::loader::parse_manifest(&source, skill_dir)
        .with_context(|| format!("validating governed skill `{}`", skill_path.display()))?;
    let runtime_package = parse_skill_runtime_package(&source)
        .with_context(|| format!("decoding governed skill `{}`", skill_path.display()))?
        .ok_or_else(|| {
            anyhow!(
                "content source skill `{expected_name}` requires runtime_contract and \
                 runtime_actions in the same SKILL.md"
            )
        })?;

    let pack = project_runtime_package_to_pack(
        &skill_manifest.name,
        &skill_manifest.description,
        None,
        skill_dir,
        resolved_skill_path.parent().unwrap_or(skill_dir),
        runtime_package,
    )
    .map_err(|error| anyhow!("invalid governed runtime contract for `{expected_name}`: {error}"))?;
    validate_governed_pack_owner(&pack, expected_name, manifest, &skill_path)
}

fn validate_governed_pack_owner(
    pack: &CapabilityPackDefinition,
    expected_name: &str,
    manifest: &CapabilityDiscoveryManifest,
    skill_path: &Path,
) -> Result<()> {
    let source = format!("governed skill `{}`", skill_path.display());
    let expected_action = manifest.capability.action.as_str();
    let action_schema: &NativeActionSchemaDef = pack
        .native_action_schemas
        .get(expected_action)
        .ok_or_else(|| anyhow!("{source} does not declare native action `{expected_action}`"))?;
    let deterministic_cli = matches!(
        &pack.implementation,
        ImplementationType::Primitive {
            provider_name: None,
            runtime_package: Some(_),
            ..
        }
    );
    validate_owner_action_contract(
        &source,
        expected_name,
        &pack.name,
        deterministic_cli,
        &action_schema.parameters,
        &action_schema.required,
        manifest,
    )
}

fn validate_owner_action_contract(
    source: &str,
    expected_name: &str,
    actual_name: &str,
    deterministic_cli: bool,
    parameters: &[String],
    required_parameters: &[String],
    manifest: &CapabilityDiscoveryManifest,
) -> Result<()> {
    let expected_action = manifest.capability.action.as_str();
    if actual_name != expected_name {
        bail!("{source} declares `{actual_name}` instead of owning skill `{expected_name}`");
    }
    if !deterministic_cli {
        bail!("{source} must use a deterministic CLI primitive implementation");
    }
    let declared = parameters.iter().cloned().collect::<BTreeSet<_>>();
    if declared.len() != parameters.len() {
        bail!("capability discovery action `{expected_action}` declares duplicate parameters");
    }
    let required = required_parameters.iter().cloned().collect::<BTreeSet<_>>();
    if required.len() != required_parameters.len() {
        bail!(
            "capability discovery action `{expected_action}` declares duplicate required \
             parameters"
        );
    }
    if let Some(unknown) = required.difference(&declared).next() {
        bail!(
            "capability discovery action `{expected_action}` requires undeclared parameter \
             `{unknown}`"
        );
    }
    let mut supplied = manifest
        .input
        .fixed_arguments
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    supplied.extend(
        [
            manifest.input.query_argument.as_deref(),
            manifest.input.limit_argument.as_deref(),
            manifest.input.cursor_argument.as_deref(),
            manifest.input.targets_argument.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::to_string),
    );
    supplied.extend(
        manifest
            .input
            .options
            .values()
            .map(|option| option.argument.clone()),
    );
    if let Some(unknown) = supplied.difference(&declared).next() {
        bail!(
            "capability discovery supplies undeclared action parameter `{unknown}` for \
             `{expected_action}`"
        );
    }
    if let Some(missing) = required
        .iter()
        .find(|argument| !supplied.contains(argument.as_str()))
    {
        bail!(
            "capability discovery omits required action parameter `{missing}` for \
             `{expected_action}`"
        );
    }
    Ok(())
}

fn build_capability_arguments(
    manifest: &CapabilityDiscoveryManifest,
    request: &DiscoveryRequest,
) -> Result<Map<String, Value>> {
    let mut arguments = manifest
        .input
        .fixed_arguments
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Map<_, _>>();

    if let Some(argument) = &manifest.input.query_argument {
        let query = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                request
                    .intent
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .ok_or_else(|| anyhow!("discovery adapter requires a query or intent"))?;
        if query.chars().count() > manifest.input.max_query_chars {
            bail!(
                "discovery query exceeds {} characters",
                manifest.input.max_query_chars
            );
        }
        arguments.insert(argument.clone(), Value::String(query.to_string()));
    }
    if let Some(argument) = &manifest.input.limit_argument {
        arguments.insert(argument.clone(), Value::from(request.limit as u64));
    }
    if let Some(cursor) = &request.cursor {
        let cursor = cursor.trim();
        if cursor.is_empty() || cursor.chars().count() > MAX_CURSOR_CHARS {
            bail!("discovery cursor must be non-empty and at most {MAX_CURSOR_CHARS} characters");
        }
        let argument = manifest
            .input
            .cursor_argument
            .as_ref()
            .ok_or_else(|| anyhow!("discovery adapter does not support cursors"))?;
        arguments.insert(argument.clone(), Value::String(cursor.to_string()));
    }
    if !request.targets.is_empty() {
        let targets = request
            .targets
            .iter()
            .map(|target| target.trim().to_string())
            .collect::<Vec<_>>();
        if targets
            .iter()
            .any(|target| target.is_empty() || target.chars().count() > MAX_OPTION_LIST_ITEM_CHARS)
        {
            bail!(
                "discovery targets must be non-empty and at most {MAX_OPTION_LIST_ITEM_CHARS} \
                 characters each"
            );
        }
        let argument = manifest
            .input
            .targets_argument
            .as_ref()
            .ok_or_else(|| anyhow!("discovery adapter does not accept targets"))?;
        arguments.insert(
            argument.clone(),
            encode_string_list(&targets, manifest.input.targets_join.as_deref()),
        );
    }

    for key in request.options.keys() {
        if !manifest.input.options.contains_key(key) {
            bail!("unsupported discovery option `{key}`");
        }
    }
    for (key, mapping) in &manifest.input.options {
        let Some(value) = request.options.get(key) else {
            continue;
        };
        arguments.insert(
            mapping.argument.clone(),
            normalize_option_value(key, value, mapping)?,
        );
    }
    Ok(arguments)
}

fn normalize_option_value(
    key: &str,
    value: &Value,
    mapping: &CapabilityOptionManifest,
) -> Result<Value> {
    match mapping.value_type {
        CapabilityOptionType::String => {
            let value = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("discovery option `{key}` must be a non-empty string"))?;
            if value.chars().count() > MAX_OPTION_STRING_CHARS {
                bail!("discovery option `{key}` exceeds {MAX_OPTION_STRING_CHARS} characters");
            }
            if !mapping.allowed.is_empty() && !mapping.allowed.iter().any(|item| item == value) {
                bail!("unsupported value `{value}` for discovery option `{key}`");
            }
            Ok(Value::String(value.to_string()))
        },
        CapabilityOptionType::Boolean => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| anyhow!("discovery option `{key}` must be a boolean")),
        CapabilityOptionType::PositiveInteger => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(Value::from)
            .ok_or_else(|| anyhow!("discovery option `{key}` must be a positive integer")),
        CapabilityOptionType::StringList => {
            let values = value
                .as_array()
                .ok_or_else(|| anyhow!("discovery option `{key}` must be an array of strings"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| {
                            anyhow!("discovery option `{key}` contains an invalid value")
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            if values.is_empty() || values.len() > MAX_OPTION_LIST_ITEMS {
                bail!("discovery option `{key}` must contain 1 to {MAX_OPTION_LIST_ITEMS} values");
            }
            if values
                .iter()
                .any(|value| value.chars().count() > MAX_OPTION_LIST_ITEM_CHARS)
            {
                bail!(
                    "discovery option `{key}` values must not exceed {MAX_OPTION_LIST_ITEM_CHARS} \
                     characters"
                );
            }
            Ok(encode_string_list(&values, mapping.join.as_deref()))
        },
    }
}

fn encode_string_list(values: &[String], join: Option<&str>) -> Value {
    match join {
        Some(separator) => Value::String(values.join(separator)),
        None => Value::Array(values.iter().cloned().map(Value::String).collect()),
    }
}

fn normalize_capability_output(
    manifest: &CapabilityDiscoveryManifest,
    payload: Value,
    limit: usize,
) -> Result<DiscoveryPage> {
    let page = select_optional_pointer(&payload, manifest.output.page_pointer.as_deref())
        .ok_or_else(|| anyhow!("capability output omitted configured page_pointer"))?;
    if let Some(pointer) = &manifest.output.error_pointer {
        if let Some(error) = page
            .pointer(pointer)
            .filter(|value| meaningful_error(value))
        {
            bail!("capability discovery failed: {}", bounded_error(error));
        }
    }

    match manifest.output.mode {
        CapabilityOutputMode::Mapped => normalize_mapped_page(manifest, page, limit),
        CapabilityOutputMode::CanonicalV1 => normalize_canonical_page(manifest, page, limit),
    }
}

fn normalize_mapped_page(
    manifest: &CapabilityDiscoveryManifest,
    page: &Value,
    limit: usize,
) -> Result<DiscoveryPage> {
    let items_pointer = manifest
        .output
        .items_pointer
        .as_deref()
        .ok_or_else(|| anyhow!("mapped output omitted items_pointer"))?;
    let rows = page
        .pointer(items_pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("capability output items_pointer did not resolve to an array"))?;
    let mapping = manifest
        .output
        .item
        .as_ref()
        .ok_or_else(|| anyhow!("mapped output omitted item mappings"))?;
    let response_metadata = mapped_metadata(page, &manifest.output.response_metadata);
    let observed_at_ms = Utc::now().timestamp_millis();
    let mut seen_urls = BTreeSet::new();
    let mut items = Vec::new();
    for row in rows {
        let candidate =
            match mapped_candidate(manifest, mapping, row, &response_metadata, observed_at_ms) {
                Ok(candidate) => candidate,
                Err(error) => {
                    tracing::debug!(
                        adapter_id = %manifest.adapter.id,
                        %error,
                        "skipping malformed capability discovery result"
                    );
                    continue;
                },
            };
        let Some(url) = candidate.canonical_url.as_ref() else {
            continue;
        };
        if seen_urls.insert(url.clone()) {
            items.push(candidate);
            if items.len() == limit {
                break;
            }
        }
    }

    Ok(DiscoveryPage {
        items,
        next_cursor: mapped_next_cursor(page, manifest.output.next_cursor_pointer.as_deref())?,
        validators: BTreeMap::new(),
        cost: mapped_cost(
            page,
            manifest.output.cost.as_ref(),
            manifest.adapter.metered,
        )?,
        transport: Default::default(),
    })
}

fn mapped_candidate(
    manifest: &CapabilityDiscoveryManifest,
    mapping: &MappedItemManifest,
    row: &Value,
    response_metadata: &BTreeMap<String, Value>,
    observed_at_ms: i64,
) -> Result<ContentCandidate> {
    let raw_url = pointer_string(row, &mapping.url_pointer)
        .ok_or_else(|| anyhow!("capability result omitted its URL"))?;
    let canonical_url = canonicalize_http_url(&raw_url)
        .context("canonicalizing capability discovery result URL")?;
    let title = mapping
        .title_pointer
        .as_deref()
        .and_then(|pointer| pointer_string(row, pointer))
        .unwrap_or_else(|| canonical_url.clone());
    let title = bounded_chars(&title, MAX_CANDIDATE_TITLE_CHARS);
    let cheap_text = mapping
        .cheap_text_pointers
        .iter()
        .find_map(|pointer| pointer_text(row, pointer))
        .unwrap_or_else(|| title.clone());
    let cheap_text = bounded_chars(&cheap_text, MAX_CANDIDATE_CHEAP_TEXT_CHARS);
    let source_item_id = mapping
        .source_item_id_pointer
        .as_deref()
        .and_then(|pointer| pointer_string(row, pointer))
        .map(|value| blake3::hash(value.as_bytes()).to_hex().to_string())
        .unwrap_or_else(|| blake3::hash(canonical_url.as_bytes()).to_hex().to_string());
    let source_label = mapping
        .source_label_pointer
        .as_deref()
        .and_then(|pointer| pointer_string(row, pointer))
        .unwrap_or_else(|| source_label_from_url(&canonical_url));
    let mut metadata = response_metadata.clone();
    metadata.extend(mapped_metadata(row, &mapping.metadata));
    metadata.insert(
        "provider".to_string(),
        Value::String(manifest.adapter.id.clone()),
    );
    let published_at_ms = mapping
        .published_at_pointer
        .as_deref()
        .and_then(|pointer| row.pointer(pointer))
        .and_then(parse_timestamp_ms);

    let candidate = ContentCandidate {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new(&manifest.adapter.id, source_item_id)?,
        title: title.clone(),
        cheap_text: cheap_text.clone(),
        canonical_url: Some(canonical_url.clone()),
        published_at_ms,
        observed_at_ms,
        privacy: manifest.adapter.privacy,
        content_hash: Some(
            blake3::hash(format!("{canonical_url}\n{title}\n{cheap_text}").as_bytes())
                .to_hex()
                .to_string(),
        ),
        provenance: ContentProvenance {
            source_label,
            source_url: Some(canonical_url),
            retrieved_by: manifest.capability.name.clone(),
        },
        metadata,
    };
    candidate.validate()?;
    Ok(candidate)
}

fn normalize_canonical_page(
    manifest: &CapabilityDiscoveryManifest,
    page: &Value,
    limit: usize,
) -> Result<DiscoveryPage> {
    let canonical: CanonicalCapabilityPage = serde_json::from_value(page.clone())
        .context("decoding canonical_v1 capability discovery output")?;
    let observed_at_ms = Utc::now().timestamp_millis();
    let mut seen_urls = BTreeSet::new();
    let mut items = Vec::new();
    for row in canonical.items {
        let candidate = match serde_json::from_value::<CanonicalCapabilityItem>(row)
            .context("decoding canonical_v1 capability discovery item")
            .and_then(|item| canonical_candidate(manifest, item, observed_at_ms))
        {
            Ok(candidate) => candidate,
            Err(error) => {
                tracing::debug!(
                    adapter_id = %manifest.adapter.id,
                    %error,
                    "skipping malformed canonical capability discovery result"
                );
                continue;
            },
        };
        let canonical_url = candidate
            .canonical_url
            .as_ref()
            .expect("canonical capability candidates always carry a URL");
        if !seen_urls.insert(canonical_url.clone()) {
            continue;
        }
        items.push(candidate);
        if items.len() == limit {
            break;
        }
    }
    if let Some(cost) = &canonical.cost {
        validate_name(&cost.commodity, "canonical cost commodity")?;
    } else if manifest.adapter.metered {
        bail!("metered canonical_v1 output omitted cost");
    }
    Ok(DiscoveryPage {
        items,
        next_cursor: normalize_cursor(canonical.next_cursor.as_deref())?,
        validators: BTreeMap::new(),
        cost: canonical.cost,
        transport: Default::default(),
    })
}

fn canonical_candidate(
    manifest: &CapabilityDiscoveryManifest,
    mut item: CanonicalCapabilityItem,
    observed_at_ms: i64,
) -> Result<ContentCandidate> {
    let canonical_url = canonicalize_http_url(&item.canonical_url)
        .context("canonicalizing canonical_v1 result URL")?;
    let title = bounded_chars(item.title.trim(), MAX_CANDIDATE_TITLE_CHARS);
    let cheap_text = bounded_chars(item.cheap_text.trim(), MAX_CANDIDATE_CHEAP_TEXT_CHARS);
    let source_item_id = item
        .source_item_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| blake3::hash(value.as_bytes()).to_hex().to_string())
        .unwrap_or_else(|| blake3::hash(canonical_url.as_bytes()).to_hex().to_string());
    item.metadata.insert(
        "provider".to_string(),
        Value::String(manifest.adapter.id.clone()),
    );
    let candidate = ContentCandidate {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new(&manifest.adapter.id, source_item_id)?,
        title: title.clone(),
        cheap_text: cheap_text.clone(),
        canonical_url: Some(canonical_url.clone()),
        published_at_ms: item.published_at_ms,
        observed_at_ms,
        privacy: manifest.adapter.privacy,
        content_hash: Some(
            blake3::hash(format!("{canonical_url}\n{title}\n{cheap_text}").as_bytes())
                .to_hex()
                .to_string(),
        ),
        provenance: ContentProvenance {
            source_label: item
                .source_label
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| source_label_from_url(&canonical_url)),
            source_url: Some(canonical_url),
            retrieved_by: manifest.capability.name.clone(),
        },
        metadata: item.metadata,
    };
    candidate.validate()?;
    Ok(candidate)
}

fn mapped_metadata(value: &Value, mappings: &BTreeMap<String, String>) -> BTreeMap<String, Value> {
    mappings
        .iter()
        .filter_map(|(key, pointer)| {
            value
                .pointer(pointer)
                .and_then(safe_metadata_value)
                .map(|value| (key.clone(), value))
        })
        .collect()
}

fn mapped_next_cursor(page: &Value, pointer: Option<&str>) -> Result<Option<String>> {
    let Some(pointer) = pointer else {
        return Ok(None);
    };
    match page.pointer(pointer) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => normalize_cursor(Some(value)),
        Some(_) => bail!("capability next cursor must be a string or null"),
    }
}

fn normalize_cursor(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_CURSOR_CHARS {
        bail!("capability next cursor exceeds {MAX_CURSOR_CHARS} characters");
    }
    Ok(Some(value.to_string()))
}

fn mapped_cost(
    page: &Value,
    mapping: Option<&CapabilityCostManifest>,
    required: bool,
) -> Result<Option<AdapterCost>> {
    let Some(mapping) = mapping else {
        if required {
            bail!("metered capability output has no cost mapping");
        }
        return Ok(None);
    };
    let Some(value) = page.pointer(&mapping.pointer) else {
        if required {
            bail!("metered capability output omitted its cost field");
        }
        return Ok(None);
    };
    let amount_microunits = match mapping.encoding {
        CapabilityCostEncoding::DecimalMajorUnits => decimal_major_units_to_micros(value)
            .ok_or_else(|| anyhow!("capability cost is not valid decimal major units"))?,
        CapabilityCostEncoding::Microunits => value
            .as_u64()
            .ok_or_else(|| anyhow!("capability cost is not valid integer microunits"))?,
    };
    Ok(Some(AdapterCost {
        commodity: mapping.commodity.clone(),
        amount_microunits,
    }))
}

fn safe_metadata_value(value: &Value) -> Option<Value> {
    match value {
        Value::Null | Value::Object(_) => None,
        Value::String(value) => Some(Value::String(bounded_chars(value, 4 * 1024))),
        Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::Array(values) if values.len() <= 64 => values
            .iter()
            .map(|value| match value {
                Value::String(value) => Some(Value::String(bounded_chars(value, 4 * 1024))),
                Value::Bool(_) | Value::Number(_) => Some(value.clone()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        Value::Array(_) => None,
    }
}

fn decimal_major_units_to_micros(value: &Value) -> Option<u64> {
    let major_units = match value {
        Value::Number(number) => number.as_f64(),
        Value::Object(object) => object
            .get("total")
            .or_else(|| object.get("total_cost"))
            .and_then(Value::as_f64)
            .or_else(|| {
                let values = object
                    .values()
                    .filter_map(Value::as_f64)
                    .collect::<Vec<_>>();
                (!values.is_empty()).then(|| values.into_iter().sum())
            }),
        _ => None,
    }?;
    if !major_units.is_finite() || major_units < 0.0 {
        return None;
    }
    Some((major_units * 1_000_000.0).round() as u64)
}

fn pointer_string(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn pointer_text(value: &Value, pointer: &str) -> Option<String> {
    match value.pointer(pointer)? {
        Value::String(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        },
        Value::Array(values) => {
            let text = values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        },
        _ => None,
    }
}

fn parse_timestamp_ms(value: &Value) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value);
    }
    let value = value.as_str()?;
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.timestamp_millis())
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|date| date.and_utc().timestamp_millis())
        })
}

fn source_label_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "Web source".to_string())
}

fn meaningful_error(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(false) => false,
        Value::String(value) => !value.trim().is_empty(),
        _ => true,
    }
}

fn select_optional_pointer<'a>(value: &'a Value, pointer: Option<&str>) -> Option<&'a Value> {
    match pointer {
        Some(pointer) => value.pointer(pointer),
        None => Some(value),
    }
}

fn invocation_source(source: ContentInvocationSource) -> DeterministicCapabilityInvocationSource {
    match source {
        ContentInvocationSource::UserFeed => DeterministicCapabilityInvocationSource::UserFeed,
        ContentInvocationSource::RecurringMonitor => {
            DeterministicCapabilityInvocationSource::RecurringMonitor
        },
        ContentInvocationSource::ObservedSource => {
            DeterministicCapabilityInvocationSource::ObservedSource
        },
        ContentInvocationSource::InteractiveRead => {
            DeterministicCapabilityInvocationSource::InteractiveRead
        },
        ContentInvocationSource::InternalSystem => {
            DeterministicCapabilityInvocationSource::InternalSystem
        },
    }
}

fn validate_name(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 256 || value.chars().any(char::is_control)
    {
        bail!("{label} must be non-empty, at most 256 characters, and contain no controls");
    }
    Ok(())
}

fn validate_optional_name(value: &Option<String>, label: &str) -> Result<()> {
    if let Some(value) = value {
        validate_name(value, label)?;
    }
    Ok(())
}

fn validate_pointer(value: &str, label: &str) -> Result<()> {
    if !value.is_empty() && !value.starts_with('/') {
        bail!("{label} must be an RFC 6901 JSON pointer");
    }
    Ok(())
}

fn validate_optional_pointer(value: &Option<String>, label: &str) -> Result<()> {
    if let Some(value) = value {
        validate_pointer(value, label)?;
    }
    Ok(())
}

fn bounded_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn bounded_error(value: &Value) -> String {
    bounded_chars(&value.to_string(), 512)
}

fn default_query_argument() -> Option<String> {
    Some("query".to_string())
}

fn default_max_query_chars() -> usize {
    DEFAULT_MAX_QUERY_CHARS
}

fn default_public_privacy() -> ContentPrivacy {
    ContentPrivacy::Public
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::magician_v2::{
        content_sources::{FreshnessPolicy, RetrievalOperation},
        execution::{
            actions::ActionResult, primitive_dispatch::DeterministicCapabilityInvocationResult,
            ExecutionError,
        },
    };

    struct RecordingInvoker {
        requests: Mutex<Vec<DeterministicCapabilityInvocation>>,
        response: Value,
    }

    #[async_trait]
    impl DeterministicCapabilityInvoker for RecordingInvoker {
        async fn invoke(
            &self,
            invocation: DeterministicCapabilityInvocation,
        ) -> std::result::Result<DeterministicCapabilityInvocationResult, ExecutionError> {
            self.requests.lock().unwrap().push(invocation);
            Ok(DeterministicCapabilityInvocationResult {
                output: ActionResult::text(self.response.to_string()),
                duration_ms: 3,
            })
        }
    }

    fn exa_manifest() -> CapabilityDiscoveryManifest {
        embedded_discovery(include_str!(
            "../../../../skillshub/semantic-websearch-via-exa/SKILL.md"
        ))
    }

    fn embedded_discovery(source: &str) -> CapabilityDiscoveryManifest {
        tool_runtime_core::manifest_parser::parse_skill_magician_extension(
            source,
            CAPABILITY_DISCOVERY_EXTENSION,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn shipped_discovery_skills_have_valid_unique_retrieval_contracts() {
        let skill_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../skillshub");
        let paths = capability_discovery_manifest_paths(&[skill_root]).unwrap();
        let mut adapters = BTreeSet::new();
        let mut actions = BTreeSet::new();
        let mut priorities = BTreeMap::new();

        for path in paths {
            let manifest = validated_capability_discovery_manifest(&path).unwrap();
            let descriptor = manifest.descriptor();
            assert_eq!(descriptor.retrieval.operation, RetrievalOperation::Discover);
            assert!(adapters.insert(descriptor.adapter_id.clone()));
            assert!(actions.insert(descriptor.retrieval.action_id.clone()));
            priorities.insert(
                descriptor.retrieval.action_id,
                descriptor.retrieval.priority,
            );
        }

        assert_eq!(adapters.len(), 8);
        assert_eq!(priorities.get("tinyfish.discover"), Some(&25));
        assert!(actions.contains("exa.discover"));
        assert!(actions.contains("arxiv.discover"));
    }

    #[test]
    fn shipped_discovery_manifests_normalize_provider_free_fixtures() {
        let fixtures = [
            (
                embedded_discovery(include_str!(
                    "../../../../skillshub/news-search-via-tavily/SKILL.md"
                )),
                json!({
                    "results": [{
                        "title": "Tavily result",
                        "url": "https://tavily.test/result",
                        "content": "Bounded Tavily evidence",
                        "published_date": "2026-07-22T10:00:00Z",
                        "score": 0.8
                    }],
                    "cost_microunits": 1_000_000
                }),
                "tavily",
            ),
            (
                embedded_discovery(include_str!("../../../../skillshub/reddit-search/SKILL.md")),
                community_fixture("Reddit result", "https://reddit.test/item"),
                "reddit",
            ),
            (
                embedded_discovery(include_str!("../../../../skillshub/github-search/SKILL.md")),
                community_fixture("GitHub result", "https://github.test/item"),
                "github",
            ),
            (
                embedded_discovery(include_str!(
                    "../../../../skillshub/producthunt-search/SKILL.md"
                )),
                community_fixture("Product Hunt result", "https://producthunt.test/item"),
                "product-hunt",
            ),
            (
                embedded_discovery(include_str!(
                    "../../../../skillshub/hackernews-search/SKILL.md"
                )),
                community_fixture("Hacker News result", "https://hackernews.test/item"),
                "hacker-news",
            ),
            (
                embedded_discovery(include_str!("../../../../skillshub/arxiv-search/SKILL.md")),
                json!({
                    "items": [{
                        "source_item_id": "arxiv-1",
                        "title": "arXiv result",
                        "cheap_text": "Provider-neutral arXiv evidence",
                        "canonical_url": "https://arxiv.org/abs/2607.00001",
                        "published_at_ms": 1_784_707_200_000_i64,
                        "source_label": "arxiv.org",
                        "metadata": {"category": "cs.AI"}
                    }]
                }),
                "arxiv",
            ),
        ];

        for (manifest, fixture, expected_adapter) in fixtures {
            let page = normalize_capability_output(&manifest, fixture, 10).unwrap();
            assert_eq!(page.items.len(), 1, "{expected_adapter}");
            assert_eq!(page.items[0].identity.adapter_id, expected_adapter);
            assert!(page.items[0].canonical_url.is_some());
            assert!(!page.items[0].provenance.source_label.trim().is_empty());
        }
    }

    fn community_fixture(title: &str, url: &str) -> Value {
        json!({
            "items": [{
                "source_native_id": "item-1",
                "title": title,
                "url": url,
                "snippet": "Bounded source-native evidence",
                "published_at": 1_784_707_200_000_i64,
                "source": "fixture.test",
                "engagement": 12,
                "author": "author",
                "container": "community"
            }]
        })
    }

    fn request(policy: RemoteDataPolicy) -> DiscoveryRequest {
        DiscoveryRequest {
            principal: "p".to_string(),
            workspace: "w".to_string(),
            intent: Some("useful AI research".to_string()),
            query: Some("contextual retrieval research".to_string()),
            targets: Vec::new(),
            cursor: None,
            validators: BTreeMap::new(),
            limit: 10,
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: policy,
            invocation_source: ContentInvocationSource::UserFeed,
            options: BTreeMap::new(),
        }
    }

    fn exa_response() -> Value {
        json!({
            "search_type": "auto",
            "cost": {"search": 0.002, "contents": 0.001},
            "results": [{
                "title": "A useful paper",
                "url": "https://example.com/paper?utm_source=exa&id=4",
                "published_date": "2026-07-22T10:00:00Z",
                "score": 0.91,
                "highlights": ["A bounded relevant passage."]
            }]
        })
    }

    #[tokio::test]
    async fn exa_manifest_reuses_capability_and_normalizes_results() {
        let invoker = Arc::new(RecordingInvoker {
            requests: Mutex::new(Vec::new()),
            response: exa_response(),
        });
        let adapter = CapabilityDiscoveryAdapter::new(exa_manifest(), invoker.clone()).unwrap();
        let page = adapter
            .discover(&request(RemoteDataPolicy::Allow))
            .await
            .unwrap();

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].identity.adapter_id, "exa");
        assert_eq!(page.items[0].cheap_text, "A bounded relevant passage.");
        assert_eq!(
            page.items[0].canonical_url.as_deref(),
            Some("https://example.com/paper?id=4")
        );
        assert_eq!(page.items[0].metadata["provider_score"], json!(0.91));
        assert_eq!(page.items[0].metadata["search_type"], json!("auto"));
        assert_eq!(page.cost.unwrap().amount_microunits, 3_000);

        let requests = invoker.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].capability_name, "semantic-websearch-via-exa");
        assert_eq!(
            requests[0].source,
            DeterministicCapabilityInvocationSource::UserFeed
        );
        assert_eq!(requests[0].principal.as_deref(), Some("p"));
        assert_eq!(requests[0].workspace.as_deref(), Some("w"));
        assert_eq!(
            requests[0].arguments["query"],
            json!("contextual retrieval research")
        );
        assert_eq!(requests[0].arguments["contents"], json!(false));
        assert_eq!(requests[0].arguments["highlights"], json!(true));
        assert_eq!(requests[0].arguments["type"], json!("auto"));
    }

    #[tokio::test]
    async fn remote_policy_denies_before_capability_invocation() {
        let invoker = Arc::new(RecordingInvoker {
            requests: Mutex::new(Vec::new()),
            response: exa_response(),
        });
        let adapter = CapabilityDiscoveryAdapter::new(exa_manifest(), invoker.clone()).unwrap();
        assert!(adapter
            .discover(&request(RemoteDataPolicy::Deny))
            .await
            .is_err());
        assert!(invoker.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn manifest_maps_typed_options_without_provider_code() {
        let mut request = request(RemoteDataPolicy::Allow);
        request.options.insert("search_type".into(), json!("deep"));
        request.options.insert(
            "include_domains".into(),
            json!(["example.com", "openai.com"]),
        );
        request.options.insert("max_age_hours".into(), json!(24));

        let arguments = build_capability_arguments(&exa_manifest(), &request).unwrap();
        assert_eq!(arguments["type"], json!("deep"));
        assert_eq!(
            arguments["include_domains"],
            json!("example.com,openai.com")
        );
        assert_eq!(arguments["max_age_hours"], json!(24));

        request
            .options
            .insert("search_type".into(), json!("invalid"));
        assert!(build_capability_arguments(&exa_manifest(), &request).is_err());
    }

    #[test]
    fn mapped_output_skips_bad_rows_and_deduplicates_canonical_urls() {
        let page = normalize_capability_output(
            &exa_manifest(),
            json!({
                "results": [
                    {"title":"Missing URL"},
                    {"title":"One", "url":"https://example.com/a?utm_source=x"},
                    {"title":"Duplicate", "url":"https://example.com/a"}
                ],
                "cost": 0
            }),
            10,
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "One");
    }

    #[test]
    fn mapped_output_rejects_provider_error_envelope() {
        assert!(
            normalize_capability_output(&exa_manifest(), json!({"error":"rate limited"}), 10)
                .is_err()
        );
    }

    #[test]
    fn mapped_output_rejects_malformed_metered_cost() {
        assert!(normalize_capability_output(
            &exa_manifest(),
            json!({"results":[], "cost":{"search":"unknown"}}),
            10
        )
        .is_err());
    }

    #[test]
    fn canonical_v1_output_is_provider_neutral() {
        let manifest = CapabilityDiscoveryManifest::from_yaml_str(
            r#"
schema_version: 1
adapter:
  id: canonical-fixture
  display_name: Canonical fixture
  class: web_search
  execution: local_process
  auth: none
  max_results: 5
capability:
  name: fixture-skill
  action: run
output:
  mode: canonical_v1
"#,
        )
        .unwrap();
        let page = normalize_capability_output(
            &manifest,
            json!({
                "items": [{
                    "source_item_id":"item-1",
                    "title":"Canonical result",
                    "cheap_text":"Provider script normalized this.",
                    "canonical_url":"https://example.com/item-1",
                    "metadata":{"rank":1}
                }],
                "cost":{"commodity":"credits", "amount_microunits":2}
            }),
            5,
        )
        .unwrap();
        assert_eq!(page.items[0].identity.adapter_id, "canonical-fixture");
        assert_eq!(page.items[0].provenance.retrieved_by, "fixture-skill");
        assert_eq!(page.cost.unwrap().commodity, "credits");
    }

    #[test]
    fn canonical_v1_skips_malformed_rows_without_losing_valid_results() {
        let manifest = CapabilityDiscoveryManifest::from_yaml_str(
            r#"
schema_version: 1
adapter:
  id: canonical-fixture
  display_name: Canonical fixture
  class: web_search
  execution: local_process
  auth: none
  max_results: 5
capability:
  name: fixture-skill
  action: run
output:
  mode: canonical_v1
"#,
        )
        .unwrap();
        let page = normalize_capability_output(
            &manifest,
            json!({
                "items": [
                    {"title":"Missing fields"},
                    {
                        "title":"Valid result",
                        "cheap_text":"Useful summary",
                        "canonical_url":"https://example.com/valid"
                    },
                    {
                        "title":"Bad URL",
                        "cheap_text":"Useful summary",
                        "canonical_url":"file:///tmp/private"
                    }
                ]
            }),
            5,
        )
        .unwrap();

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "Valid result");
    }

    #[test]
    fn manifest_rejects_invalid_pointer() {
        let invalid = r#"
schema_version: 1
adapter:
  id: invalid
  display_name: Invalid
  class: web_search
  execution: local_process
  auth: none
  max_results: 5
capability:
  name: fixture
  action: run
output:
  mode: mapped
  items_pointer: results
  item:
    url_pointer: /url
"#;
        assert!(CapabilityDiscoveryManifest::from_yaml_str(invalid).is_err());
    }

    #[test]
    fn manifest_rejects_unknown_fields() {
        let invalid = r#"
schema_version: 1
unexpected: true
adapter:
  id: invalid
  display_name: Invalid
  class: web_search
  execution: local_process
  auth: none
  max_results: 5
capability:
  name: fixture
  action: run
output:
  mode: canonical_v1
"#;
        assert!(CapabilityDiscoveryManifest::from_yaml_str(invalid).is_err());
    }

    #[test]
    fn loader_discovers_embedded_contract_without_following_unrelated_files() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
        )
        .unwrap();
        std::fs::write(temp.path().join("ignored.yaml"), "not: a manifest").unwrap();

        let paths = capability_discovery_manifest_paths(&[temp.path().to_path_buf()]).unwrap();
        assert_eq!(paths, vec![skill.join("SKILL.md")]);
        assert_eq!(
            load_capability_discovery_manifest(&paths[0])
                .unwrap()
                .adapter
                .id,
            "exa"
        );
    }

    #[test]
    fn registration_discovers_skill_owned_contract_without_provider_rust() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
        )
        .unwrap();
        let invoker = Arc::new(RecordingInvoker {
            requests: Mutex::new(Vec::new()),
            response: exa_response(),
        });
        let mut registry = ContentSourceRegistry::new();

        let ids = register_capability_discovery_manifests(
            &mut registry,
            invoker,
            &[temp.path().to_path_buf()],
        )
        .unwrap();

        assert_eq!(ids, vec!["exa"]);
        assert_eq!(registry.discovery_descriptors()[0].adapter_id, "exa");
    }

    #[test]
    fn registration_rejects_governed_skill_name_that_differs_from_owning_directory() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md").replacen(
                "name: \"semantic-websearch-via-exa\"",
                "name: \"wrong-owner\"",
                1,
            ),
        )
        .unwrap();

        let error = validated_capability_discovery_manifest(&skill.join("SKILL.md")).unwrap_err();

        assert!(error.to_string().contains("validating governed skill"));
        assert!(format!("{error:#}").contains("does not match parent dir"));
    }

    #[test]
    fn discovery_contract_rejects_undeclared_or_missing_action_parameters() {
        let manifest = exa_manifest();
        assert!(validate_owner_action_contract(
            "fixture",
            "semantic-websearch-via-exa",
            "semantic-websearch-via-exa",
            true,
            &["query".into()],
            &["query".into()],
            &manifest,
        )
        .unwrap_err()
        .to_string()
        .contains("undeclared action parameter"));

        let parameters = vec![
            "query",
            "num_results",
            "contents",
            "highlights",
            "type",
            "category",
            "max_age_hours",
            "start_published_date",
            "include_domains",
            "exclude_domains",
            "missing_binding",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        assert!(validate_owner_action_contract(
            "fixture",
            "semantic-websearch-via-exa",
            "semantic-websearch-via-exa",
            true,
            &parameters,
            &["query".into(), "missing_binding".into()],
            &manifest,
        )
        .unwrap_err()
        .to_string()
        .contains("omits required action parameter"));
        assert!(validate_owner_action_contract(
            "fixture",
            "semantic-websearch-via-exa",
            "semantic-websearch-via-exa",
            true,
            &parameters,
            &["query".into(), "undeclared".into()],
            &manifest,
        )
        .unwrap_err()
        .to_string()
        .contains("requires undeclared parameter"));
    }

    #[test]
    fn manifest_discovery_honors_whole_skill_shadowing() {
        let temp = tempfile::tempdir().unwrap();
        let scoped_root = temp.path().join("scoped");
        let system_root = temp.path().join("system");
        let scoped_skill = scoped_root.join("semantic-websearch-via-exa");
        let system_skill = system_root.join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&scoped_skill).unwrap();
        std::fs::create_dir_all(&system_skill).unwrap();
        let shipped = include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md");
        std::fs::write(scoped_skill.join("SKILL.md"), shipped).unwrap();
        std::fs::write(system_skill.join("SKILL.md"), shipped).unwrap();

        let paths = capability_discovery_manifest_paths(&[scoped_root, system_root]).unwrap();
        assert_eq!(paths, vec![scoped_skill.join("SKILL.md")]);
    }

    #[test]
    fn scoped_skill_without_extension_suppresses_system_extension() {
        let temp = tempfile::tempdir().unwrap();
        let scoped_root = temp.path().join("scoped");
        let system_root = temp.path().join("system");
        let scoped_skill = scoped_root.join("semantic-websearch-via-exa");
        let system_skill = system_root.join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&scoped_skill).unwrap();
        std::fs::create_dir_all(&system_skill).unwrap();
        std::fs::write(
            scoped_skill.join("SKILL.md"),
            "---\nname: semantic-websearch-via-exa\ndescription: override\n---\n",
        )
        .unwrap();
        std::fs::write(
            system_skill.join("SKILL.md"),
            include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
        )
        .unwrap();

        let paths = capability_discovery_manifest_paths(&[scoped_root, system_root]).unwrap();
        assert!(paths.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn loader_accepts_standard_skill_file_install_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source-SKILL.md");
        let installed_skill = temp.path().join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&installed_skill).unwrap();
        std::fs::write(
            &source,
            include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
        )
        .unwrap();
        let installed = installed_skill.join("SKILL.md");
        std::os::unix::fs::symlink(source, &installed).unwrap();

        assert_eq!(
            load_capability_discovery_manifest(&installed)
                .unwrap()
                .adapter
                .id,
            "exa"
        );
    }

    #[cfg(unix)]
    #[test]
    fn manifest_discovery_rejects_broken_skill_install_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("semantic-websearch-via-exa");
        std::fs::create_dir_all(&skill).unwrap();
        std::os::unix::fs::symlink(temp.path().join("missing-SKILL.md"), skill.join("SKILL.md"))
            .unwrap();

        assert!(capability_discovery_manifest_paths(&[temp.path().to_path_buf()]).is_err());
    }
}
