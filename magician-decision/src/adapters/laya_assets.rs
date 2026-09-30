//! What every laya runtime loads besides the network: the tokenizer, its
//! special tokens, and `rl_agent_config.json` (sequence limits and
//! calibration). Shared by `laya-onnx` and `laya-mlx`, whose model
//! directories lay these files out the same way.

use std::path::Path;

use tokenizers::Tokenizer;

use super::laya::{LayaAgentConfig, SpecialTokens};
use crate::error::DecisionError;
use crate::model::ModelCapabilities;

pub fn laya_capabilities(max_state_tokens: Option<u64>) -> ModelCapabilities {
    ModelCapabilities {
        calibrated: false,
        text_only: true,
        max_state_tokens,
        max_choice_options: Some(64),
        max_questions: None,
        supports_score: true,
        remote: false,
    }
}

pub(crate) struct LayaAssets {
    pub tokenizer: Tokenizer,
    pub special: SpecialTokens,
    pub agent: LayaAgentConfig,
}

/// `tokenizer.json`, `tokenizer_config.json`, and `rl_agent_config.json`
/// from a laya model directory.
pub(crate) fn load_assets(dir: &Path) -> Result<LayaAssets, DecisionError> {
    let tokenizer_path = dir.join("tokenizer.json");
    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| asset_error("tokenizer", &tokenizer_path, e))?;
    let special = special_tokens(&tokenizer, &dir.join("tokenizer_config.json"))?;
    let agent_path = dir.join("rl_agent_config.json");
    let agent: LayaAgentConfig = serde_json::from_str(
        &std::fs::read_to_string(&agent_path).map_err(|e| asset_error("read", &agent_path, e))?,
    )
    .map_err(|e| asset_error("parse", &agent_path, e))?;
    Ok(LayaAssets {
        tokenizer,
        special,
        agent,
    })
}

pub(crate) fn asset_error(what: &str, path: &Path, error: impl std::fmt::Display) -> DecisionError {
    DecisionError::Transport(format!("laya: {what} {}: {error}", path.display()))
}

fn special_tokens(
    tokenizer: &Tokenizer,
    config_path: &Path,
) -> Result<SpecialTokens, DecisionError> {
    let text =
        std::fs::read_to_string(config_path).map_err(|e| asset_error("read", config_path, e))?;
    let config: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| asset_error("parse", config_path, e))?;
    let token = |key: &str| -> Result<(String, u32), DecisionError> {
        let name = config
            .get(key)
            .and_then(|v| {
                v.as_str()
                    .or_else(|| v.get("content").and_then(|c| c.as_str()))
            })
            .ok_or_else(|| asset_error(&format!("no {key} in"), config_path, "missing"))?;
        let id = tokenizer.token_to_id(name).ok_or_else(|| {
            asset_error(
                &format!("{key} {name:?} not in the tokenizer for"),
                config_path,
                "unknown",
            )
        })?;
        Ok((name.to_string(), id))
    };
    let (_, cls) = token("cls_token")?;
    let (_, sep) = token("sep_token")?;
    let (mask_text, mask) = token("mask_token")?;
    let (_, pad) = token("pad_token")?;
    Ok(SpecialTokens {
        cls,
        sep,
        mask,
        pad,
        mask_text,
    })
}
