//! Local-generation kitty pin: the operator-writable
//! `runtime.ollama.local_generation.selected` value.
//!
//! Auto-setup picks the first memory tier the machine satisfies and writes
//! that one line. Settings reuses the same surgical edit the installer uses
//! (`selected: &local_generation_model <id>`) so YAML comments and the
//! cross-file `*local_generation_model` alias stay intact. RAM-tier
//! mismatches are warnings, not refusals: the owner can still switch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

const SELECTED_ANCHOR_PATTERN: &str = r"(?m)^(\s*selected: &local_generation_model ).*$";
const CATALOG_RELATIVE: &str = "data/magician_v2/local_generation_catalog.yaml";
const RUN_OLLAMA_RELATIVE: &str = "scripts/run-ollama.sh";

/// Light view of the installer-owned `runtime.ollama.local_generation` block.
#[derive(Debug, Clone, Deserialize, Default)]
struct ConfigView {
    #[serde(default)]
    runtime: RuntimeView,
    #[serde(default)]
    privacy: PrivacyView,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RuntimeView {
    #[serde(default)]
    ollama: OllamaView,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct OllamaView {
    #[serde(default)]
    local_generation: Option<LocalGenerationBlock>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PrivacyView {
    #[serde(default)]
    processing: PrivacyProcessingView,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PrivacyProcessingView {
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LocalGenerationBlock {
    #[serde(default)]
    pub min_memory_gb: Option<u32>,
    #[serde(default)]
    pub requires_arch: Option<String>,
    #[serde(default)]
    pub requires_os: Option<String>,
    #[serde(default)]
    pub tiers: Vec<LocalGenerationTier>,
    #[serde(default)]
    pub embedding_resident_gb: Option<f64>,
    #[serde(default)]
    pub system_headroom_gb: Option<f64>,
    #[serde(default)]
    pub selected: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocalGenerationTier {
    pub min_memory_gb: u32,
    pub model: String,
    #[serde(default)]
    pub resident_gb: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub ollama: Option<String>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub disk_gb: Option<f64>,
    #[serde(default)]
    pub resident_gb: Option<f64>,
    #[serde(default)]
    pub min_memory_gb: Option<u32>,
    #[serde(default)]
    pub classify_agree_pct: Option<f64>,
    #[serde(default)]
    pub distill_recall_pct: Option<f64>,
    #[serde(default)]
    pub browser_effect: Option<String>,
    #[serde(default)]
    pub browser_protocol: Option<String>,
    #[serde(default)]
    pub tok_s_channel: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostSnapshot {
    pub memory_gb: u32,
    pub free_memory_gb: Option<u32>,
    pub arch: String,
    pub os: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalGenerationModelStatus {
    pub id: String,
    pub label: String,
    pub ollama: String,
    pub install: Option<String>,
    pub min_memory_gb: u32,
    pub resident_gb: Option<f64>,
    pub disk_gb: Option<f64>,
    pub classify_agree_pct: Option<f64>,
    pub distill_recall_pct: Option<f64>,
    pub browser_effect: Option<String>,
    pub browser_protocol: Option<String>,
    pub tok_s_channel: Option<f64>,
    pub notes: Option<String>,
    pub selected: bool,
    pub recommended: bool,
    pub rule_ok: bool,
    pub installed: Option<bool>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalGenerationEnvelope {
    pub settings_path: String,
    pub selected: Option<String>,
    pub recommended: Option<String>,
    pub processing_mode: String,
    pub host: HostSnapshot,
    pub min_memory_gb: u32,
    pub requires_arch: Option<String>,
    pub requires_os: Option<String>,
    pub models: Vec<LocalGenerationModelStatus>,
    pub warnings: Vec<String>,
    pub catalog_path: Option<String>,
}

#[derive(Debug)]
pub enum LocalGenerationError {
    Io(std::io::Error),
    InvalidModel(String),
    UnknownModel {
        requested: String,
        known: Vec<String>,
    },
    Anchor(String),
}

impl std::fmt::Display for LocalGenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidModel(model) => {
                write!(
                    f,
                    "local generation model `{model}` is not a legal Ollama tag"
                )
            },
            Self::UnknownModel { requested, known } => write!(
                f,
                "unknown local generation model `{requested}`; known: {}",
                known.join(", ")
            ),
            Self::Anchor(message) => write!(f, "{message}"),
        }
    }
}

impl From<std::io::Error> for LocalGenerationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn magician_source_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
            candidates.push(dir.join(".."));
            candidates.push(dir.join("../.."));
            candidates.push(dir.join("../../.."));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
    for candidate in candidates {
        let Ok(canon) = candidate.canonicalize() else {
            continue;
        };
        if canon.join(RUN_OLLAMA_RELATIVE).is_file() || canon.join(CATALOG_RELATIVE).is_file() {
            return Some(canon);
        }
    }
    None
}

pub fn catalog_path() -> Option<PathBuf> {
    magician_source_root().map(|root| root.join(CATALOG_RELATIVE))
}

pub fn run_ollama_script_path() -> Option<PathBuf> {
    magician_source_root().map(|root| root.join(RUN_OLLAMA_RELATIVE))
}

pub fn load_catalog(path: &Path) -> Vec<CatalogModel> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_yaml::from_str::<CatalogFile>(&text)
        .map(|file| file.models)
        .unwrap_or_default()
}

pub fn parse_config_view(yaml: &str) -> ConfigSnapshot {
    let view: ConfigView = serde_yaml::from_str(yaml).unwrap_or_default();
    let block = view.runtime.ollama.local_generation.unwrap_or_default();
    let processing_mode = view
        .privacy
        .processing
        .mode
        .unwrap_or_else(|| "local".to_string());
    ConfigSnapshot {
        block,
        processing_mode,
    }
}

#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub block: LocalGenerationBlock,
    pub processing_mode: String,
}

pub fn normalize_arch(arch: &str) -> String {
    match arch {
        "aarch64" | "arm64" => "arm64".to_string(),
        "x86_64" | "amd64" => "x86_64".to_string(),
        other => other.to_string(),
    }
}

pub fn normalize_os(os: &str) -> String {
    match os {
        "darwin" | "macos" => "macos".to_string(),
        other => other.to_string(),
    }
}

pub fn host_snapshot() -> HostSnapshot {
    HostSnapshot {
        memory_gb: physical_memory_gb(),
        free_memory_gb: free_memory_gb(),
        arch: normalize_arch(std::env::consts::ARCH),
        os: normalize_os(std::env::consts::OS),
    }
}

fn physical_memory_gb() -> u32 {
    let bytes: u64 = if cfg!(target_os = "macos") {
        sysctl_u64("hw.memsize").unwrap_or(0)
    } else {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("MemTotal:"))
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|kb| kb.parse::<u64>().ok())
            })
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    (bytes / 1024 / 1024 / 1024) as u32
}

fn free_memory_gb() -> Option<u32> {
    if cfg!(target_os = "macos") {
        let pages = sysctl_u64("vm.page_free_count")?;
        let page_size = sysctl_u64("hw.pagesize")?;
        Some(((pages.saturating_mul(page_size)) / 1024 / 1024 / 1024) as u32)
    } else {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("MemAvailable:"))
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|kb| kb.parse::<u64>().ok())
            })
            .map(|kb| (kb / 1024 / 1024) as u32)
    }
}

fn sysctl_u64(key: &str) -> Option<u64> {
    let sysctl = runtime_core::process::resolve_program_str("sysctl", None);
    let output = std::process::Command::new(sysctl)
        .args(["-n", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

pub fn recommended_model(block: &LocalGenerationBlock, memory_gb: u32) -> Option<String> {
    let gate = block.min_memory_gb.unwrap_or(0);
    if memory_gb == 0 || memory_gb < gate {
        return None;
    }
    block
        .tiers
        .iter()
        .find(|tier| memory_gb >= tier.min_memory_gb)
        .map(|tier| tier.model.clone())
}

pub fn is_legal_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && model
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ':'))
}

pub fn replace_selected_anchor(text: &str, model: &str) -> Result<String, LocalGenerationError> {
    if !is_legal_model_id(model) {
        return Err(LocalGenerationError::InvalidModel(model.to_string()));
    }
    let regex = Regex::new(SELECTED_ANCHOR_PATTERN).expect("static selected-anchor pattern");
    let matches = regex.find_iter(text).count();
    if matches != 1 {
        return Err(LocalGenerationError::Anchor(format!(
            "expected exactly one `selected: &local_generation_model` line, found {matches}"
        )));
    }
    Ok(regex
        .replace(text, |caps: &regex::Captures| {
            format!("{}{model}", &caps[1])
        })
        .into_owned())
}

pub fn write_selected(path: &Path, model: &str) -> Result<(), LocalGenerationError> {
    if !is_legal_model_id(model) {
        return Err(LocalGenerationError::InvalidModel(model.to_string()));
    }
    let lock = crate::magician_v2::runtime_settings::config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let next = replace_selected_anchor(&original, model)?;
    if next == original {
        return Ok(());
    }
    write_bytes_durably_sync(path, next.as_bytes())?;
    Ok(())
}

fn tier_for<'a>(block: &'a LocalGenerationBlock, model: &str) -> Option<&'a LocalGenerationTier> {
    block.tiers.iter().find(|tier| tier.model == model)
}

fn host_gate_warnings(block: &LocalGenerationBlock, host: &HostSnapshot) -> Vec<String> {
    let mut warnings = Vec::new();
    if host.memory_gb == 0 {
        warnings.push(
            "could not read physical memory; auto-setup would refuse rather than guess".to_string(),
        );
    }
    if let Some(min) = block.min_memory_gb {
        if host.memory_gb > 0 && host.memory_gb < min {
            warnings.push(format!(
                "this machine has {} GB; local generation is not offered below {min} GB",
                host.memory_gb
            ));
        }
    }
    if let Some(required) = block.requires_arch.as_deref() {
        if normalize_arch(required) != host.arch {
            warnings.push(format!(
                "local generation is specified for {required}; this host is {}",
                host.arch
            ));
        }
    }
    if let Some(required) = block.requires_os.as_deref() {
        if normalize_os(required) != host.os {
            warnings.push(format!(
                "local generation is specified for {required}; this host is {}",
                host.os
            ));
        }
    }
    warnings
}

fn model_warnings(
    block: &LocalGenerationBlock,
    host: &HostSnapshot,
    model: &LocalGenerationModelStatus,
    recommended: Option<&str>,
) -> (bool, Vec<String>) {
    let mut warnings = Vec::new();
    let mut rule_ok = true;
    let gate = block.min_memory_gb.unwrap_or(0);
    if host.memory_gb > 0 && host.memory_gb < gate {
        rule_ok = false;
        warnings.push(format!(
            "{} needs the local-generation floor of {gate} GB; this machine has {} GB",
            model.label, host.memory_gb
        ));
    }
    if let Some(required) = block.requires_arch.as_deref() {
        if normalize_arch(required) != host.arch {
            rule_ok = false;
        }
    }
    if let Some(required) = block.requires_os.as_deref() {
        if normalize_os(required) != host.os {
            rule_ok = false;
        }
    }
    if host.memory_gb > 0 && host.memory_gb < model.min_memory_gb {
        rule_ok = false;
        warnings.push(format!(
            "{} is auto-picked at {} GB+; this machine has {} GB",
            model.label, model.min_memory_gb, host.memory_gb
        ));
    }
    if let (Some(recommended), true) = (recommended, rule_ok) {
        if recommended != model.id {
            warnings.push(format!(
                "auto-setup would pick {recommended} on this {} GB machine; this is an override",
                host.memory_gb
            ));
        }
    }
    if let (Some(free), Some(resident)) = (host.free_memory_gb, model.resident_gb) {
        let claimed = resident
            + block.embedding_resident_gb.unwrap_or(0.0)
            + block.system_headroom_gb.unwrap_or(0.0);
        if (free as f64) + 0.5 < claimed {
            warnings.push(format!(
                "only {free} GB free now; {} wants ~{resident:.0} GB resident plus embedder and headroom (~{claimed:.0} GB)",
                model.label
            ));
        }
    }
    (rule_ok, warnings)
}

pub fn known_model_ids(block: &LocalGenerationBlock, catalog: &[CatalogModel]) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for model in catalog {
        if seen.insert(model.id.clone()) {
            ids.push(model.id.clone());
        }
    }
    for tier in &block.tiers {
        if seen.insert(tier.model.clone()) {
            ids.push(tier.model.clone());
        }
    }
    ids
}

pub fn build_envelope(
    settings_path: &Path,
    snapshot: &ConfigSnapshot,
    catalog: &[CatalogModel],
    catalog_path: Option<&Path>,
    host: HostSnapshot,
    installed: &HashMap<String, bool>,
) -> LocalGenerationEnvelope {
    let recommended = recommended_model(&snapshot.block, host.memory_gb);
    let mut host_warnings = host_gate_warnings(&snapshot.block, &host);
    let selected = snapshot.block.selected.clone();
    let catalog_by_id: HashMap<&str, &CatalogModel> = catalog
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect();

    let mut order: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for model in catalog {
        if seen.insert(model.id.clone()) {
            order.push(model.id.clone());
        }
    }
    for tier in &snapshot.block.tiers {
        if seen.insert(tier.model.clone()) {
            order.push(tier.model.clone());
        }
    }
    if let Some(selected) = selected.as_deref() {
        if seen.insert(selected.to_string()) {
            order.push(selected.to_string());
        }
    }

    let mut models = Vec::new();
    for id in order {
        let catalog_row = catalog_by_id.get(id.as_str()).copied();
        let tier = tier_for(&snapshot.block, &id);
        let min_memory_gb = tier
            .map(|tier| tier.min_memory_gb)
            .or_else(|| catalog_row.and_then(|row| row.min_memory_gb))
            .unwrap_or(0);
        let mut status = LocalGenerationModelStatus {
            id: id.clone(),
            label: catalog_row
                .and_then(|row| row.label.clone())
                .unwrap_or_else(|| id.clone()),
            ollama: catalog_row
                .and_then(|row| row.ollama.clone())
                .unwrap_or_else(|| id.clone()),
            install: catalog_row.and_then(|row| row.install.clone()),
            min_memory_gb,
            resident_gb: tier
                .and_then(|tier| tier.resident_gb)
                .or_else(|| catalog_row.and_then(|row| row.resident_gb)),
            disk_gb: catalog_row.and_then(|row| row.disk_gb),
            classify_agree_pct: catalog_row.and_then(|row| row.classify_agree_pct),
            distill_recall_pct: catalog_row.and_then(|row| row.distill_recall_pct),
            browser_effect: catalog_row.and_then(|row| row.browser_effect.clone()),
            browser_protocol: catalog_row.and_then(|row| row.browser_protocol.clone()),
            tok_s_channel: catalog_row.and_then(|row| row.tok_s_channel),
            notes: catalog_row.and_then(|row| row.notes.clone()),
            selected: selected.as_deref() == Some(id.as_str()),
            recommended: recommended.as_deref() == Some(id.as_str()),
            rule_ok: true,
            installed: installed.get(&id).copied().or_else(|| {
                catalog_row.and_then(|row| {
                    row.ollama
                        .as_ref()
                        .and_then(|name| installed.get(name).copied())
                })
            }),
            warnings: Vec::new(),
        };
        let (rule_ok, warnings) =
            model_warnings(&snapshot.block, &host, &status, recommended.as_deref());
        status.rule_ok = rule_ok;
        status.warnings = warnings;
        if status.installed == Some(false) {
            status.warnings.push(format!(
                "{} is not installed in Ollama; pin it anyway, then `make setup-local-generation MODEL={}`",
                status.label, status.id
            ));
        }
        models.push(status);
    }

    if snapshot.processing_mode.eq_ignore_ascii_case("cloud") {
        host_warnings.push(
            "privacy.processing.mode is cloud, so switching the pin will not prewarm a generation model until processing is local"
                .to_string(),
        );
    }

    LocalGenerationEnvelope {
        settings_path: settings_path.display().to_string(),
        selected,
        recommended,
        processing_mode: snapshot.processing_mode.clone(),
        host,
        min_memory_gb: snapshot.block.min_memory_gb.unwrap_or(0),
        requires_arch: snapshot.block.requires_arch.clone(),
        requires_os: snapshot.block.requires_os.clone(),
        models,
        warnings: host_warnings,
        catalog_path: catalog_path.map(|path| path.display().to_string()),
    }
}

pub fn load_envelope_from_path(
    path: &Path,
    installed: &HashMap<String, bool>,
) -> Result<LocalGenerationEnvelope, LocalGenerationError> {
    let yaml = std::fs::read_to_string(path)?;
    let snapshot = parse_config_view(&yaml);
    let catalog_file = catalog_path();
    let catalog = catalog_file
        .as_ref()
        .map(|path| load_catalog(path))
        .unwrap_or_default();
    Ok(build_envelope(
        path,
        &snapshot,
        &catalog,
        catalog_file.as_deref(),
        host_snapshot(),
        installed,
    ))
}

pub fn assert_known_model(
    snapshot: &ConfigSnapshot,
    catalog: &[CatalogModel],
    model: &str,
) -> Result<(), LocalGenerationError> {
    let known = known_model_ids(&snapshot.block, catalog);
    if known.iter().any(|id| id == model) {
        Ok(())
    } else {
        Err(LocalGenerationError::UnknownModel {
            requested: model.to_string(),
            known,
        })
    }
}

pub fn model_is_installed(installed_names: &[String], model: &str) -> bool {
    installed_names.iter().any(|name| {
        name == model
            || name == &format!("{model}:latest")
            || name
                .strip_suffix(":latest")
                .is_some_and(|stripped| stripped == model)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
runtime:
  ollama:
    local_generation:
      min_memory_gb: 16
      requires_arch: arm64
      requires_os: macos
      tiers:
        - min_memory_gb: 36
          model: qwen3.8-ud2-mtp
          resident_gb: 11
        - min_memory_gb: 24
          model: gemma4:12b
          resident_gb: 8
        - min_memory_gb: 16
          model: woof-4b
          resident_gb: 3
      embedding_resident_gb: 4
      system_headroom_gb: 6
      # pin line the installer and Settings both rewrite
      selected: &local_generation_model gemma4:12b
privacy:
  processing:
    mode: local
"#;

    fn catalog() -> Vec<CatalogModel> {
        vec![
            CatalogModel {
                id: "qwen3.8-ud2-mtp".into(),
                label: Some("Qwen 3.8 27B".into()),
                ollama: Some("qwen3.8-ud2-mtp".into()),
                install: Some("create-gguf".into()),
                disk_gb: Some(9.8),
                resident_gb: Some(11.0),
                min_memory_gb: Some(36),
                classify_agree_pct: Some(68.0),
                distill_recall_pct: Some(82.7),
                browser_effect: Some("4/4".into()),
                browser_protocol: Some("3/4".into()),
                tok_s_channel: Some(70.0),
                notes: None,
            },
            CatalogModel {
                id: "gemma4:12b".into(),
                label: Some("Gemma 4 12B".into()),
                ollama: Some("gemma4:12b".into()),
                install: Some("ollama-pull".into()),
                disk_gb: Some(7.6),
                resident_gb: Some(8.0),
                min_memory_gb: Some(24),
                classify_agree_pct: Some(55.0),
                distill_recall_pct: None,
                browser_effect: Some("4/4".into()),
                browser_protocol: Some("2/4".into()),
                tok_s_channel: None,
                notes: None,
            },
            CatalogModel {
                id: "woof-4b".into(),
                label: Some("Underdog Woof 4B".into()),
                ollama: Some("woof-4b".into()),
                install: Some("mlx-safetensors".into()),
                disk_gb: Some(2.4),
                resident_gb: Some(3.0),
                min_memory_gb: Some(16),
                classify_agree_pct: Some(39.0),
                distill_recall_pct: Some(82.7),
                browser_effect: Some("3/4".into()),
                browser_protocol: Some("2/4".into()),
                tok_s_channel: Some(86.0),
                notes: None,
            },
        ]
    }

    fn host(memory_gb: u32) -> HostSnapshot {
        HostSnapshot {
            memory_gb,
            free_memory_gb: Some(20),
            arch: "arm64".into(),
            os: "macos".into(),
        }
    }

    #[test]
    fn ram_tiers_pick_the_first_matching_model() {
        let snapshot = parse_config_view(SAMPLE);
        assert_eq!(
            recommended_model(&snapshot.block, 64).as_deref(),
            Some("qwen3.8-ud2-mtp")
        );
        assert_eq!(
            recommended_model(&snapshot.block, 32).as_deref(),
            Some("gemma4:12b")
        );
        assert_eq!(
            recommended_model(&snapshot.block, 20).as_deref(),
            Some("woof-4b")
        );
        assert_eq!(recommended_model(&snapshot.block, 8), None);
    }

    #[test]
    fn a_32gb_host_may_pin_qwen_with_a_rule_warning() {
        let snapshot = parse_config_view(SAMPLE);
        let envelope = build_envelope(
            Path::new("/tmp/magician-config.yaml"),
            &snapshot,
            &catalog(),
            None,
            host(32),
            &HashMap::new(),
        );
        let qwen = envelope
            .models
            .iter()
            .find(|model| model.id == "qwen3.8-ud2-mtp")
            .expect("qwen");
        assert!(!qwen.rule_ok);
        assert!(qwen
            .warnings
            .iter()
            .any(|warning| warning.contains("36 GB+")));
        let gemma = envelope
            .models
            .iter()
            .find(|model| model.id == "gemma4:12b")
            .expect("gemma");
        assert!(gemma.rule_ok);
        assert!(gemma.recommended);
        let woof = envelope
            .models
            .iter()
            .find(|model| model.id == "woof-4b")
            .expect("woof");
        assert!(woof.rule_ok);
        assert!(!woof.recommended);
        assert!(woof
            .warnings
            .iter()
            .any(|warning| warning.contains("override")));
    }

    #[test]
    fn surgical_replace_keeps_the_anchor_and_surrounding_comments() {
        let updated = replace_selected_anchor(SAMPLE, "woof-4b").expect("replace");
        assert!(updated.contains("selected: &local_generation_model woof-4b"));
        assert!(updated.contains("# pin line the installer and Settings both rewrite"));
        assert!(!updated.contains("selected: &local_generation_model gemma4:12b"));
        let count = updated
            .lines()
            .filter(|line| line.contains("selected: &local_generation_model"))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn surgical_replace_rejects_an_injection() {
        let error = replace_selected_anchor(SAMPLE, "woof-4b\nselected: pwned").unwrap_err();
        assert!(matches!(error, LocalGenerationError::InvalidModel(_)));
    }

    #[test]
    fn write_selected_updates_only_the_anchor_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("magician-config.yaml");
        let source = format!("# header comment\n{SAMPLE}\n# trailing comment stays\n");
        std::fs::write(&path, source).expect("seed");
        write_selected(&path, "qwen3.8-ud2-mtp").expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("# header comment"));
        assert!(text.contains("# pin line the installer and Settings both rewrite"));
        assert!(text.contains("# trailing comment stays"));
        assert!(text.contains("selected: &local_generation_model qwen3.8-ud2-mtp"));
        assert!(!text.contains("selected: &local_generation_model gemma4:12b"));
    }

    #[test]
    fn installed_names_accept_latest_suffix() {
        assert!(model_is_installed(&["woof-4b:latest".into()], "woof-4b"));
        assert!(model_is_installed(&["gemma4:12b".into()], "gemma4:12b"));
        assert!(!model_is_installed(&["gemma4:12b".into()], "woof-4b"));
    }
}
