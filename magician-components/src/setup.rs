//! Declarative setup drivers shared by the component graph, Skillshub, the
//! engine API, and Magican Desktop.
//!
//! Provider names belong in `setup_catalog.yaml`. Product code implements only
//! the bounded interaction primitives in [`SetupDriver`], so adding another
//! provider that uses an existing primitive does not require a Rust or Svelte
//! branch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const SETUP_CATALOG_YAML: &str = include_str!("setup_catalog.yaml");
const SETUP_CATALOG_SCHEMA: &str = "magician.setup-catalog.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupBinding {
    pub definition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSetup {
    pub definition: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub driver: SetupDriver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum SetupDriver {
    ManagedBot {
        /// Bot name or bounded template using `{profile}`.
        bot: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        start: Vec<ManagedBotStart>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<SetupField>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        fixed_env: BTreeMap<String, String>,
        /// HTTPS hosts whose login URLs may be opened from worker output.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        login_url_hosts: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<SetupInput>,
    },
    /// OAuth whose resource, issuer, scopes, state, PKCE, and token custody are
    /// already governed by the installed skill runtime contract.
    GovernedOauth,
    /// A bounded setup file stored on the engine, with a code-owned validator.
    ConfigurationFile {
        field_label: String,
        accept: String,
        destination: String,
        validator: ConfigurationFileValidator,
        #[serde(default = "default_configuration_file_limit")]
        max_bytes: usize,
    },
    /// One explicit processing-locality choice backed by the engine's privacy
    /// settings plus its data-derived local-generation model catalog.
    ModelRuntime {
        privacy_path: String,
        generation_path: String,
        local_feature: String,
        modes: Vec<SetupModeChoice>,
    },
    /// A Chromium extension shipped by Desktop and verified against the
    /// desktop-local Magicutor bridge rather than the selected remote engine.
    BrowserExtension {
        bundle_resource: String,
        management_url: String,
        probe_path: String,
        probe_needle: String,
    },
    /// The cross-platform CuaDriver installed and verified on the computer
    /// running Magican Desktop, independently of the selected backend host.
    /// Exactly one release: every installer script comes from that release's
    /// tag and is checked by hash before it runs.
    ///
    /// Desktop reads the engine's whole catalog in one pass and the engine may
    /// be an older remote build, so this variant also reads the pre-pin shape
    /// (a bare installer URL per platform, no version). Without that, one
    /// unreadable driver failed every onboarding setup flow, not just this one.
    /// Desktop installs its own compiled pin either way; the built-in catalog
    /// must carry the pinned fields and no legacy ones (`validate_driver`).
    CuaDriver {
        #[serde(default)]
        version: String,
        /// macOS and Linux; the first file is the entry script.
        #[serde(default)]
        unix_installer: Vec<CuaDriverInstallerFile>,
        /// Windows; the first file is the entry script.
        #[serde(default)]
        windows_installer: Vec<CuaDriverInstallerFile>,
        /// Read from an older engine's catalog and never written.
        #[serde(default, skip_serializing)]
        unix_installer_url: Option<String>,
        /// Read from an older engine's catalog and never written.
        #[serde(default, skip_serializing)]
        windows_installer_url: Option<String>,
    },
}

/// One official CuaDriver installer script pinned to a release tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuaDriverInstallerFile {
    /// File name the installer expects beside its entry script.
    pub name: String,
    pub url: String,
    /// Lowercase hex SHA-256 of the exact bytes.
    pub sha256: String,
}

/// The reviewed CuaDriver pin, read from the built-in catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuaDriverRelease {
    pub version: String,
    pub unix_installer: Vec<CuaDriverInstallerFile>,
    pub windows_installer: Vec<CuaDriverInstallerFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupModeChoice {
    pub id: String,
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub local_generation: bool,
}

/// A reviewed host program needed before the engine-backed setup catalog can
/// take over. The profile membership and installer parameters live in
/// `setup_catalog.yaml`; Desktop implements only these bounded installer
/// primitives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPrerequisite {
    pub id: String,
    pub order: u16,
    pub label: String,
    pub bins: Vec<String>,
    pub install: HostPrerequisiteInstaller,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum HostPrerequisiteInstaller {
    Homebrew,
    HomebrewFormula { formula: String },
    Manual { install_hint: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedBotStart {
    Bot,
    Auth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupField {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: SetupFieldKind,
    #[serde(default)]
    pub required: bool,
    pub env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupFieldKind {
    #[default]
    Text,
    Email,
    Password,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupInput {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: SetupFieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default = "default_auth_input_limit")]
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationFileValidator {
    GoogleOauthDesktopClient,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupCatalogFile {
    schema_version: String,
    setups: BTreeMap<String, SetupDefinition>,
    #[serde(default)]
    host_prerequisites: BTreeMap<String, HostPrerequisiteDefinition>,
    #[serde(default)]
    host_prerequisite_profiles: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    harnesses: BTreeMap<String, HarnessDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupDefinition {
    label: String,
    driver: SetupDriver,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostPrerequisiteDefinition {
    order: u16,
    label: String,
    bins: Vec<String>,
    install: HostPrerequisiteInstaller,
}

/// Declarative metadata for an external harness that Magician can use after
/// the operator installs and authenticates it independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSetup {
    pub id: String,
    pub order: u16,
    pub label: String,
    pub binary: String,
    pub coding_engine: String,
    pub plane_engine: String,
    pub config_key: String,
    pub refresh_path: String,
    pub install_url: String,
    pub install_hint: String,
    pub surfaces: Vec<HarnessSurface>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HarnessDefinition {
    order: u16,
    label: String,
    binary: String,
    coding_engine: String,
    plane_engine: String,
    config_key: String,
    refresh_path: String,
    install_url: String,
    install_hint: String,
    surfaces: Vec<HarnessSurface>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSurface {
    Vibedev,
    Chat,
    AgenticRuns,
    BackgroundModelCalls,
}

fn default_auth_input_limit() -> usize {
    4096
}

fn default_configuration_file_limit() -> usize {
    1024 * 1024
}

/// Resolve one setup reference through the reviewed built-in setup catalog.
pub fn resolve(binding: &SetupBinding) -> Result<ResolvedSetup, String> {
    static CATALOG: OnceLock<Result<SetupCatalogFile, String>> = OnceLock::new();
    let catalog = CATALOG
        .get_or_init(|| parse_catalog(SETUP_CATALOG_YAML))
        .as_ref()
        .map_err(|error| error.clone())?;
    resolve_from(catalog, binding)
}

fn parse_catalog(source: &str) -> Result<SetupCatalogFile, String> {
    let catalog: SetupCatalogFile =
        serde_yaml::from_str(source).map_err(|error| format!("invalid setup catalog: {error}"))?;
    if catalog.schema_version != SETUP_CATALOG_SCHEMA {
        return Err(format!(
            "unsupported setup catalog schema `{}`",
            catalog.schema_version
        ));
    }
    for (id, definition) in &catalog.setups {
        validate_identifier(id, "setup definition")?;
        if definition.label.trim().is_empty() {
            return Err(format!("setup definition `{id}` has an empty label"));
        }
        validate_driver(id, &definition.driver)?;
    }
    let mut prerequisite_orders = BTreeSet::new();
    for (id, prerequisite) in &catalog.host_prerequisites {
        validate_identifier(id, "host prerequisite")?;
        validate_host_prerequisite(id, prerequisite)?;
        if !prerequisite_orders.insert(prerequisite.order) {
            return Err(format!(
                "host prerequisite `{id}` repeats display order {}",
                prerequisite.order
            ));
        }
    }
    for (profile, prerequisite_ids) in &catalog.host_prerequisite_profiles {
        validate_identifier(profile, "host prerequisite profile")?;
        if prerequisite_ids.is_empty() {
            return Err(format!(
                "host prerequisite profile `{profile}` has no prerequisites"
            ));
        }
        let mut unique = BTreeSet::new();
        for prerequisite_id in prerequisite_ids {
            if !unique.insert(prerequisite_id) {
                return Err(format!(
                    "host prerequisite profile `{profile}` repeats `{prerequisite_id}`"
                ));
            }
            if !catalog.host_prerequisites.contains_key(prerequisite_id) {
                return Err(format!(
                    "host prerequisite profile `{profile}` references unknown prerequisite `{prerequisite_id}`"
                ));
            }
        }
    }
    let mut harness_orders = BTreeSet::new();
    for (id, harness) in &catalog.harnesses {
        validate_identifier(id, "harness")?;
        validate_harness(id, harness)?;
        if !harness_orders.insert(harness.order) {
            return Err(format!(
                "harness `{id}` repeats display order {}",
                harness.order
            ));
        }
    }
    Ok(catalog)
}

/// Return one reviewed host-bootstrap profile in stable display order.
pub fn host_prerequisites(profile: &str) -> Result<Vec<HostPrerequisite>, String> {
    static CATALOG: OnceLock<Result<SetupCatalogFile, String>> = OnceLock::new();
    validate_identifier(profile, "host prerequisite profile")?;
    let catalog = CATALOG
        .get_or_init(|| parse_catalog(SETUP_CATALOG_YAML))
        .as_ref()
        .map_err(|error| error.clone())?;
    let ids = catalog
        .host_prerequisite_profiles
        .get(profile)
        .ok_or_else(|| format!("unknown host prerequisite profile `{profile}`"))?;
    let mut prerequisites = ids
        .iter()
        .filter_map(|id| {
            catalog
                .host_prerequisites
                .get(id)
                .map(|definition| HostPrerequisite {
                    id: id.clone(),
                    order: definition.order,
                    label: definition.label.clone(),
                    bins: definition.bins.clone(),
                    install: definition.install.clone(),
                })
        })
        .collect::<Vec<_>>();
    prerequisites.sort_by_key(|prerequisite| prerequisite.order);
    Ok(prerequisites)
}

fn validate_host_prerequisite(
    id: &str,
    prerequisite: &HostPrerequisiteDefinition,
) -> Result<(), String> {
    if prerequisite.order == 0
        || prerequisite.label.trim().is_empty()
        || prerequisite.bins.is_empty()
    {
        return Err(format!("host prerequisite `{id}` is incomplete"));
    }
    let mut bins = BTreeSet::new();
    for bin in &prerequisite.bins {
        validate_identifier(bin, "host prerequisite binary")?;
        if !bins.insert(bin) {
            return Err(format!("host prerequisite `{id}` repeats binary `{bin}`"));
        }
    }
    match &prerequisite.install {
        HostPrerequisiteInstaller::Homebrew => {
            if prerequisite.bins.len() != 1 || prerequisite.bins[0] != "brew" {
                return Err("the Homebrew bootstrap must probe only `brew`".to_string());
            }
        },
        HostPrerequisiteInstaller::HomebrewFormula { formula } => {
            validate_homebrew_formula(formula)?;
        },
        HostPrerequisiteInstaller::Manual { install_hint } => {
            if install_hint.trim().is_empty() {
                return Err(format!(
                    "manual host prerequisite `{id}` has no install guidance"
                ));
            }
        },
    }
    Ok(())
}

fn validate_homebrew_formula(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('-')
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '@' | '+' | '-' | '_' | '.')
        })
    {
        return Err(format!("invalid Homebrew formula `{value}`"));
    }
    Ok(())
}

/// Return the reviewed external-harness catalog in stable id order.
pub fn harnesses() -> Result<Vec<HarnessSetup>, String> {
    static CATALOG: OnceLock<Result<SetupCatalogFile, String>> = OnceLock::new();
    let catalog = CATALOG
        .get_or_init(|| parse_catalog(SETUP_CATALOG_YAML))
        .as_ref()
        .map_err(|error| error.clone())?;
    let mut harnesses: Vec<_> = catalog
        .harnesses
        .iter()
        .map(|(id, harness)| HarnessSetup {
            id: id.clone(),
            order: harness.order,
            label: harness.label.clone(),
            binary: harness.binary.clone(),
            coding_engine: harness.coding_engine.clone(),
            plane_engine: harness.plane_engine.clone(),
            config_key: harness.config_key.clone(),
            refresh_path: harness.refresh_path.clone(),
            install_url: harness.install_url.clone(),
            install_hint: harness.install_hint.clone(),
            surfaces: harness.surfaces.clone(),
        })
        .collect();
    harnesses.sort_by_key(|harness| harness.order);
    Ok(harnesses)
}

/// The CuaDriver release pinned by the built-in catalog. Desktop installs from
/// this compiled copy, so a remote engine's catalog never chooses what runs on
/// the desktop.
pub fn cua_driver_release() -> Result<CuaDriverRelease, String> {
    static CATALOG: OnceLock<Result<SetupCatalogFile, String>> = OnceLock::new();
    let catalog = CATALOG
        .get_or_init(|| parse_catalog(SETUP_CATALOG_YAML))
        .as_ref()
        .map_err(|error| error.clone())?;
    cua_driver_release_from(catalog)
}

fn cua_driver_release_from(catalog: &SetupCatalogFile) -> Result<CuaDriverRelease, String> {
    let mut releases = catalog
        .setups
        .values()
        .filter_map(|definition| match &definition.driver {
            SetupDriver::CuaDriver {
                version,
                unix_installer,
                windows_installer,
                ..
            } => Some(CuaDriverRelease {
                version: version.clone(),
                unix_installer: unix_installer.clone(),
                windows_installer: windows_installer.clone(),
            }),
            _ => None,
        });
    let release = releases
        .next()
        .ok_or_else(|| "the setup catalog declares no CuaDriver release".to_string())?;
    if releases.any(|other| other != release) {
        return Err("the setup catalog declares conflicting CuaDriver releases".to_string());
    }
    Ok(release)
}

fn validate_cua_driver_version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 9
                && part.chars().all(|character| character.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

/// Each file must be a tag-pinned CuaDriver release asset or source file whose
/// tag is `cua-driver-rs-v<version>`, with a lowercase hex SHA-256.
fn validate_cua_driver_installer(
    id: &str,
    version: &str,
    platform: &str,
    entry: &str,
    files: &[CuaDriverInstallerFile],
) -> Result<(), String> {
    let invalid = |detail: &str| {
        Err(format!(
            "cua-driver setup `{id}` has an invalid {platform} installer: {detail}"
        ))
    };
    if files.first().map(|file| file.name.as_str()) != Some(entry) {
        return invalid(&format!("the first file must be `{entry}`"));
    }
    let tag = format!("cua-driver-rs-v{version}");
    let release_prefix = format!("https://github.com/trycua/cua/releases/download/{tag}/");
    let source_prefix = format!("https://raw.githubusercontent.com/trycua/cua/{tag}/");
    let mut names = BTreeSet::new();
    for file in files {
        if file.name.is_empty()
            || file.name.len() > 128
            || file.name.starts_with('.')
            || !file.name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
            || !names.insert(file.name.as_str())
        {
            return invalid(&format!("file name `{}`", file.name));
        }
        let url = file.url.as_str();
        let pinned = if let Some(rest) = url.strip_prefix(&release_prefix) {
            rest == file.name
        } else if let Some(rest) = url.strip_prefix(&source_prefix) {
            rest.ends_with(&format!("/{}", file.name))
                && rest.split('/').all(|segment| {
                    !segment.is_empty()
                        && segment != "."
                        && segment != ".."
                        && segment.chars().all(|character| {
                            character.is_ascii_alphanumeric()
                                || matches!(character, '-' | '_' | '.')
                        })
                })
        } else {
            false
        };
        if !pinned {
            return invalid(&format!(
                "`{url}` is not pinned to the {tag} release or source tag"
            ));
        }
        if file.sha256.len() != 64
            || !file
                .sha256
                .chars()
                .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
        {
            return invalid(&format!("`{}` needs a lowercase hex SHA-256", file.name));
        }
    }
    Ok(())
}

fn validate_harness(id: &str, harness: &HarnessDefinition) -> Result<(), String> {
    if harness.order == 0 {
        return Err(format!("harness `{id}` has zero display order"));
    }
    if harness.label.trim().is_empty() || harness.install_hint.trim().is_empty() {
        return Err(format!("harness `{id}` has empty operator guidance"));
    }
    validate_identifier(&harness.binary, "harness binary")?;
    validate_identifier(&harness.coding_engine, "harness coding engine")?;
    validate_identifier(&harness.plane_engine, "harness plane engine")?;
    if !harness.config_key.starts_with("coding.")
        || !harness.config_key.ends_with(".enabled")
        || !harness
            .config_key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_'))
        || harness.refresh_path
            != format!(
                "/api/magician/v2/coding/engines/{}/refresh",
                harness.coding_engine
            )
        || !harness.install_url.starts_with("https://")
        || harness.install_url.chars().any(char::is_whitespace)
        || harness.surfaces.is_empty()
    {
        return Err(format!("harness `{id}` has invalid setup metadata"));
    }
    let unique: BTreeSet<_> = harness.surfaces.iter().collect();
    if unique.len() != harness.surfaces.len() {
        return Err(format!("harness `{id}` repeats a supported surface"));
    }
    Ok(())
}

fn resolve_from(
    catalog: &SetupCatalogFile,
    binding: &SetupBinding,
) -> Result<ResolvedSetup, String> {
    validate_identifier(&binding.definition, "setup definition")?;
    if let Some(profile) = binding.profile.as_deref() {
        validate_identifier(profile, "setup profile")?;
    }
    let definition = catalog
        .setups
        .get(&binding.definition)
        .ok_or_else(|| format!("unknown setup definition `{}`", binding.definition))?;
    if driver_needs_profile(&definition.driver) && binding.profile.is_none() {
        return Err(format!(
            "setup definition `{}` requires a profile",
            binding.definition
        ));
    }
    Ok(ResolvedSetup {
        definition: binding.definition.clone(),
        label: definition.label.clone(),
        profile: binding.profile.clone(),
        driver: definition.driver.clone(),
    })
}

fn driver_needs_profile(driver: &SetupDriver) -> bool {
    match driver {
        SetupDriver::ManagedBot { bot, fixed_env, .. } => {
            bot.contains("{profile}")
                || fixed_env
                    .values()
                    .any(|value| value.contains("{profile}") || value.contains("{profile_label}"))
        },
        SetupDriver::GovernedOauth
        | SetupDriver::ConfigurationFile { .. }
        | SetupDriver::ModelRuntime { .. }
        | SetupDriver::BrowserExtension { .. }
        | SetupDriver::CuaDriver { .. } => false,
    }
}

fn validate_driver(id: &str, driver: &SetupDriver) -> Result<(), String> {
    match driver {
        SetupDriver::ManagedBot {
            bot,
            start,
            fields,
            fixed_env,
            login_url_hosts,
            input,
        } => {
            validate_bot_template(bot)?;
            if start.is_empty() {
                return Err(format!("managed-bot setup `{id}` has no start action"));
            }
            let mut field_ids = BTreeSet::new();
            for field in fields {
                validate_identifier(&field.id, "setup field")?;
                validate_env_key(&field.env)?;
                if field.label.trim().is_empty() || !field_ids.insert(&field.id) {
                    return Err(format!("managed-bot setup `{id}` has an invalid field"));
                }
            }
            for (key, value) in fixed_env {
                validate_env_key(key)?;
                validate_template(value, "fixed environment value")?;
            }
            for host in login_url_hosts {
                if host.is_empty()
                    || host.len() > 253
                    || !host.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '-' | '.')
                    })
                    || host.starts_with('.')
                    || host.ends_with('.')
                {
                    return Err(format!(
                        "managed-bot setup `{id}` has an invalid login host"
                    ));
                }
            }
            if let Some(input) = input {
                validate_identifier(&input.id, "setup input")?;
                if input.label.trim().is_empty() || input.max_bytes == 0 || input.max_bytes > 65_536
                {
                    return Err(format!("managed-bot setup `{id}` has invalid auth input"));
                }
            }
        },
        SetupDriver::GovernedOauth => {},
        SetupDriver::ConfigurationFile {
            field_label,
            accept,
            destination,
            max_bytes,
            ..
        } => {
            if field_label.trim().is_empty()
                || accept.trim().is_empty()
                || *max_bytes == 0
                || *max_bytes > 16 * 1024 * 1024
                || !destination.starts_with("{data_root}/")
                || destination.contains("..")
            {
                return Err(format!("configuration-file setup `{id}` is invalid"));
            }
        },
        SetupDriver::ModelRuntime {
            privacy_path,
            generation_path,
            local_feature,
            modes,
        } => {
            validate_settings_path(privacy_path)?;
            validate_settings_path(generation_path)?;
            validate_identifier(local_feature, "local processing feature")?;
            if modes.len() < 2 {
                return Err(format!(
                    "model-runtime setup `{id}` must declare at least two processing modes"
                ));
            }
            let mut mode_ids = BTreeSet::new();
            let mut local_modes = 0;
            for mode in modes {
                validate_identifier(&mode.id, "processing mode")?;
                if mode.label.trim().is_empty()
                    || mode.description.trim().is_empty()
                    || !mode_ids.insert(mode.id.as_str())
                {
                    return Err(format!(
                        "model-runtime setup `{id}` has an invalid processing mode"
                    ));
                }
                local_modes += usize::from(mode.local_generation);
            }
            if local_modes != 1 {
                return Err(format!(
                    "model-runtime setup `{id}` must declare exactly one local-generation mode"
                ));
            }
        },
        SetupDriver::BrowserExtension {
            bundle_resource,
            management_url,
            probe_path,
            probe_needle,
        } => {
            validate_identifier(bundle_resource, "browser extension resource")?;
            if management_url != "chrome://extensions"
                || !probe_path.starts_with('/')
                || probe_path.contains('?')
                || probe_path.contains('#')
                || probe_path.contains("..")
                || probe_needle.trim().is_empty()
                || probe_needle.len() > 256
            {
                return Err(format!(
                    "browser-extension setup `{id}` contains an invalid local target"
                ));
            }
        },
        SetupDriver::CuaDriver {
            version,
            unix_installer,
            windows_installer,
            unix_installer_url,
            windows_installer_url,
        } => {
            if unix_installer_url.is_some() || windows_installer_url.is_some() {
                return Err(format!(
                    "cua-driver setup `{id}` uses the unpinned installer URL shape"
                ));
            }
            if !validate_cua_driver_version(version) {
                return Err(format!(
                    "cua-driver setup `{id}` must pin one exact release version, not `{version}`"
                ));
            }
            validate_cua_driver_installer(id, version, "unix", "install.sh", unix_installer)?;
            validate_cua_driver_installer(
                id,
                version,
                "windows",
                "install.ps1",
                windows_installer,
            )?;
        },
    }
    Ok(())
}

fn validate_settings_path(value: &str) -> Result<(), String> {
    if !value.starts_with("/api/magician/v2/settings/")
        || value.contains('?')
        || value.contains('#')
        || value.contains("..")
    {
        return Err(format!("invalid setup settings path `{value}`"));
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('-')
        || value.starts_with('.')
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(())
}

fn validate_env_key(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase())
        || !value.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return Err(format!("invalid setup environment key `{value}`"));
    }
    Ok(())
}

fn validate_template(value: &str, label: &str) -> Result<(), String> {
    let remainder = value
        .replace("{profile}", "")
        .replace("{profile_label}", "");
    if remainder.contains('{') || remainder.contains('}') {
        return Err(format!("{label} contains an unsupported placeholder"));
    }
    Ok(())
}

fn validate_bot_template(value: &str) -> Result<(), String> {
    validate_template(value, "bot template")?;
    let rendered = value
        .replace("{profile_label}", "profile")
        .replace("{profile}", "profile");
    validate_identifier(&rendered, "bot template")
}

pub fn render_template(template: &str, profile: Option<&str>) -> Result<String, String> {
    validate_template(template, "setup template")?;
    let profile = profile.unwrap_or_default();
    if (template.contains("{profile}") || template.contains("{profile_label}"))
        && profile.is_empty()
    {
        return Err("setup template requires a profile".to_string());
    }
    if !profile.is_empty() {
        validate_identifier(profile, "setup profile")?;
    }
    let profile_label = profile
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ");
    Ok(template
        .replace("{profile_label}", &profile_label)
        .replace("{profile}", profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_catalog_resolves_managed_and_oauth_drivers() {
        let google = resolve(&SetupBinding {
            definition: "google-workspace".to_string(),
            profile: Some("work".to_string()),
        })
        .expect("google setup");
        let SetupDriver::ManagedBot { bot, fields, .. } = google.driver else {
            panic!("expected managed bot");
        };
        assert_eq!(
            render_template(&bot, google.profile.as_deref()).unwrap(),
            "gmail-work"
        );
        assert_eq!(fields[0].env, "GWS_EXPECTED_EMAIL");

        assert!(matches!(
            resolve(&SetupBinding {
                definition: "governed-mcp-oauth".to_string(),
                profile: Some("personal".to_string()),
            })
            .expect("oauth setup")
            .driver,
            SetupDriver::GovernedOauth
        ));

        let runtime = resolve(&SetupBinding {
            definition: "ollama-runtime".to_string(),
            profile: None,
        })
        .expect("model runtime setup");
        let SetupDriver::ModelRuntime { modes, .. } = runtime.driver else {
            panic!("expected model runtime");
        };
        assert_eq!(modes.len(), 2);
        assert_eq!(modes.iter().filter(|mode| mode.local_generation).count(), 1);

        assert!(matches!(
            resolve(&SetupBinding {
                definition: "browser-extension".to_string(),
                profile: None,
            })
            .expect("browser extension setup")
            .driver,
            SetupDriver::BrowserExtension { .. }
        ));

        assert!(matches!(
            resolve(&SetupBinding {
                definition: "cua-driver".to_string(),
                profile: None,
            })
            .expect("CuaDriver setup")
            .driver,
            SetupDriver::CuaDriver { .. }
        ));

        let container = host_prerequisites("macos_container").expect("container prerequisites");
        assert_eq!(
            container
                .iter()
                .map(|prerequisite| prerequisite.id.as_str())
                .collect::<Vec<_>>(),
            vec!["homebrew", "python3"]
        );
        let native = host_prerequisites("macos_native").expect("native prerequisites");
        assert_eq!(
            native
                .iter()
                .map(|prerequisite| prerequisite.id.as_str())
                .collect::<Vec<_>>(),
            vec!["homebrew", "python3", "node", "uv"]
        );
        let linux = host_prerequisites("linux_container").expect("linux prerequisites");
        assert_eq!(linux[0].id, "linux-python3");
        assert!(matches!(
            linux[0].install,
            HostPrerequisiteInstaller::Manual { .. }
        ));
    }

    #[test]
    fn unknown_definitions_and_unbound_templates_fail_closed() {
        assert!(resolve(&SetupBinding {
            definition: "unknown".to_string(),
            profile: None,
        })
        .is_err());
        assert!(resolve(&SetupBinding {
            definition: "google-workspace".to_string(),
            profile: None,
        })
        .is_err());
    }

    #[test]
    fn external_harnesses_are_declarative_and_never_install_managed() {
        let harnesses = harnesses().expect("harness catalog");
        assert_eq!(harnesses.len(), 4);
        assert_eq!(
            harnesses
                .iter()
                .map(|harness| harness.id.as_str())
                .collect::<Vec<_>>(),
            vec!["codex", "claude-code", "grok", "antigravity"]
        );
        for harness in harnesses {
            assert!(harness.install_url.starts_with("https://"));
            assert!(harness.refresh_path.ends_with("/refresh"));
            assert!(harness.config_key.ends_with(".enabled"));
            assert!(harness.surfaces.contains(&HarnessSurface::Vibedev));
            assert!(harness.surfaces.contains(&HarnessSurface::Chat));
            assert!(harness.surfaces.contains(&HarnessSurface::AgenticRuns));
            assert!(harness
                .surfaces
                .contains(&HarnessSurface::BackgroundModelCalls));
        }
    }

    #[test]
    fn cua_driver_is_pinned_to_one_hash_checked_release() {
        let release = cua_driver_release().expect("CuaDriver pin");
        assert_eq!(release.version, "0.28.2");
        let tag = "cua-driver-rs-v0.28.2";
        for (files, entry) in [
            (&release.unix_installer, "install.sh"),
            (&release.windows_installer, "install.ps1"),
        ] {
            assert_eq!(files[0].name, entry);
            for file in files {
                assert!(file.url.contains(&format!("/{tag}/")), "{}", file.url);
                assert!(!file.url.contains("cua.ai"));
                assert_eq!(file.sha256.len(), 64);
            }
        }
        assert_eq!(
            release
                .unix_installer
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            vec!["install.sh", "_install-rust.sh", "_install-common.sh"]
        );
        assert_eq!(
            release
                .windows_installer
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            vec!["install.ps1", "_install-common.psm1"]
        );
    }

    #[test]
    fn an_older_engines_cua_driver_setup_still_reads() {
        // A pre-pin engine serves a bare installer URL per platform. Desktop
        // reads the whole catalog at once, so refusing this shape failed every
        // setup flow, not only CuaDriver's.
        let legacy: SetupDriver = serde_yaml::from_str(
            "kind: cua_driver\n\
             unix_installer_url: https://cua.ai/driver/install.sh\n\
             windows_installer_url: https://cua.ai/driver/install.ps1\n",
        )
        .expect("the pre-pin shape still reads");
        assert!(matches!(legacy, SetupDriver::CuaDriver { ref version, .. } if version.is_empty()));
        // Never trusted as a pin: our own catalog refuses the legacy shape.
        assert!(validate_driver("cua-driver", &legacy).is_err());

        // The legacy fields are read, never written.
        let written = serde_yaml::to_string(&cua_driver_setup_for_test()).expect("serialize");
        assert!(!written.contains("installer_url"), "{written}");
    }

    fn cua_driver_setup_for_test() -> SetupDriver {
        let release = cua_driver_release().expect("CuaDriver pin");
        SetupDriver::CuaDriver {
            version: release.version,
            unix_installer: release.unix_installer,
            windows_installer: release.windows_installer,
            unix_installer_url: None,
            windows_installer_url: None,
        }
    }

    #[test]
    fn cua_driver_pin_rejects_moving_or_unverified_installers() {
        let hash = "a".repeat(64);
        let upper_hash = "A".repeat(64);
        let catalog = |version: &str, unix_url: &str, sha256: &str| {
            format!(
                "schema_version: magician.setup-catalog.v1\n\
                 setups:\n  cua-driver:\n    label: Desktop computer use\n    driver:\n      kind: cua_driver\n      version: \"{version}\"\n      unix_installer:\n        - name: install.sh\n          url: {unix_url}\n          sha256: {sha256}\n      windows_installer:\n        - name: install.ps1\n          url: https://github.com/trycua/cua/releases/download/cua-driver-rs-v{version}/install.ps1\n          sha256: {hash}\n"
            )
        };
        let pinned = "https://github.com/trycua/cua/releases/download/cua-driver-rs-v1.2.3/install.sh";
        let release = cua_driver_release_from(
            &parse_catalog(&catalog("1.2.3", pinned, &hash)).expect("valid pin"),
        )
        .expect("release");
        assert_eq!(release.version, "1.2.3");
        assert!(parse_catalog(&catalog(
            "1.2.3",
            "https://raw.githubusercontent.com/trycua/cua/cua-driver-rs-v1.2.3/libs/cua-driver/scripts/install.sh",
            &hash
        ))
        .is_ok());

        for (version, url, sha256) in [
            // Moving installers and other tags are refused.
            ("1.2.3", "https://cua.ai/driver/install.sh", hash.as_str()),
            (
                "1.2.3",
                "https://github.com/trycua/cua/releases/download/cua-driver-rs-v1.2.4/install.sh",
                hash.as_str(),
            ),
            (
                "1.2.3",
                "https://github.com/trycua/cua/releases/latest/download/install.sh",
                hash.as_str(),
            ),
            (
                "1.2.3",
                "https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.sh",
                hash.as_str(),
            ),
            (
                "1.2.3",
                "https://raw.githubusercontent.com/trycua/cua/cua-driver-rs-v1.2.3/../install.sh",
                hash.as_str(),
            ),
            // A range or prerelease is not one exact release.
            ("1.2", pinned, hash.as_str()),
            ("latest", pinned, hash.as_str()),
            ("1.2.3-nightly", pinned, hash.as_str()),
            // The hash must be a full lowercase SHA-256.
            ("1.2.3", pinned, "abc"),
            ("1.2.3", pinned, upper_hash.as_str()),
        ] {
            assert!(
                parse_catalog(&catalog(version, url, sha256)).is_err(),
                "{version} {url} {sha256}"
            );
        }
    }
}
