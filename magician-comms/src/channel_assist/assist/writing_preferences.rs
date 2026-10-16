//! Per-sender/domain writing preference learning.
//!
//! The learner accepts either an exact owner statement or an in-memory
//! before/after draft edit. Raw drafts never enter the store or memory system;
//! edit feedback is reduced deterministically to short, user-visible
//! statements before persistence. Candidates can be shown before promotion,
//! while promoted statements are mirrored to `user.channel_writing_preferences`.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use magician::magician_v2::agents::memory::AgentMemoryResolver;
use magician::magician_v2::chat::service::{
    merge_user_memory_tier_fields, remove_user_memory_tier_field,
};
use magician::magician_v2::process_storage;

use super::store::WritingPreference;
use super::types::sanitize_follow_up_key_detail;

/// The tier promoted statements mirror to — the shared tier-name contract
/// (plan 3.1 prerequisite (b), lib-side
/// `magician_v2::evidence::tier_contracts`), not a local string.
pub const WRITING_PREFERENCE_TIER: &str =
    magician::magician_v2::evidence::tier_contracts::CHANNEL_WRITING_PREFERENCES_TIER;
pub const MAX_WRITING_PREFERENCE_CHARS: usize = 240;

pub fn normalize_statement(raw: &str) -> Result<String> {
    let statement = sanitize_follow_up_key_detail(raw, MAX_WRITING_PREFERENCE_CHARS)
        .context("writing preference statement is empty")?;
    if statement.chars().count() < 4 {
        anyhow::bail!("writing preference statement is too short");
    }
    Ok(if statement.ends_with(['.', '!', '?']) {
        statement
    } else {
        format!("{statement}.")
    })
}

pub fn sender_domain(address: Option<&str>) -> Option<String> {
    address
        .and_then(|address| address.rsplit_once('@'))
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
}

/// Derive style-only statements from a draft edit. This deliberately avoids
/// copying names, facts, or sentences from either draft.
pub fn derive_edit_preferences(original: &str, edited: &str) -> Vec<String> {
    let original_words = word_count(original);
    let edited_words = word_count(edited);
    let mut statements = Vec::new();

    if original_words >= 20 && edited_words.saturating_mul(100) <= original_words.saturating_mul(70)
    {
        statements.push("Keep replies concise.".to_string());
    } else if edited_words >= 30
        && edited_words.saturating_mul(100) >= original_words.max(1).saturating_mul(140)
    {
        statements.push("Include a little more context in replies.".to_string());
    }

    let original_greeting = greeting_style(original);
    let edited_greeting = greeting_style(edited);
    if edited_greeting != original_greeting {
        if let Some(style) = edited_greeting {
            statements.push(format!("Use a {style} greeting."));
        } else if original_greeting.is_some() {
            statements.push("Start replies without a greeting.".to_string());
        }
    }

    let original_signoff = signoff_style(original);
    let edited_signoff = signoff_style(edited);
    if edited_signoff != original_signoff {
        if let Some(style) = edited_signoff {
            statements.push(format!("End replies with a {style} sign-off."));
        } else if original_signoff.is_some() {
            statements.push("End replies without a sign-off.".to_string());
        }
    }

    if original.matches('!').count() > edited.matches('!').count() && original.contains('!') {
        statements.push("Avoid exclamation marks.".to_string());
    }

    statements.sort();
    statements.dedup();
    statements
}

pub async fn promote_to_memory(
    principal: &str,
    workspace: &str,
    preference: &WritingPreference,
) -> Result<()> {
    let resolver = AgentMemoryResolver::with_workspace_layout(process_storage::workspace());
    let field_key = memory_field_key(preference);
    let mut fields = Map::new();
    fields.insert(
        field_key,
        json!({
            "statement": preference.statement,
            "scope": preference.scope_kind.as_db_str(),
            "scope_value": preference.scope_value,
            "provider": preference.provider,
            "account": preference.account_alias,
            "evidence_count": preference.evidence_count,
            "promoted_at": preference.updated_at,
            "source_annotation_id": preference.source_annotation_id,
        }),
    );
    let outcome = merge_user_memory_tier_fields(
        &resolver,
        principal,
        workspace,
        WRITING_PREFERENCE_TIER,
        &fields,
    )
    .await;
    if outcome.get("status").and_then(Value::as_str) == Some("error") {
        anyhow::bail!(
            "writing preference memory promotion failed: {}",
            outcome
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    Ok(())
}

pub async fn remove_from_memory(
    principal: &str,
    workspace: &str,
    preference: &WritingPreference,
) -> Result<()> {
    let resolver = AgentMemoryResolver::with_workspace_layout(process_storage::workspace());
    let outcome = remove_user_memory_tier_field(
        &resolver,
        principal,
        workspace,
        WRITING_PREFERENCE_TIER,
        &memory_field_key(preference),
    )
    .await;
    if outcome.get("status").and_then(Value::as_str) == Some("error") {
        anyhow::bail!(
            "writing preference memory removal failed: {}",
            outcome
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    Ok(())
}

fn memory_field_key(preference: &WritingPreference) -> String {
    format!(
        "writing:{}:{}:{}:{}",
        preference.provider,
        preference.account_alias,
        preference.scope_kind.as_db_str(),
        preference.id
    )
}

fn word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

fn greeting_style(value: &str) -> Option<&'static str> {
    let first = value
        .trim_start()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|ch: char| !ch.is_alphabetic())
        .to_ascii_lowercase();
    match first.as_str() {
        "dear" => Some("formal"),
        "hi" | "hello" => Some("friendly"),
        "hey" => Some("casual"),
        _ => None,
    }
}

fn signoff_style(value: &str) -> Option<&'static str> {
    let tail = value
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if tail.contains("kind regards") || tail.contains("sincerely") {
        Some("formal")
    } else if tail.contains("best,") || tail.contains("thanks,") || tail.contains("thank you,") {
        Some("friendly")
    } else if tail.contains("cheers,") {
        Some("casual")
    } else {
        None
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn edit_learning_emits_only_style_statements() {
        let original = "Hello Jamie! Here is a very long explanation with many extra words about the project, its history, its schedule, and several details that are not necessary. Best, Alex";
        let edited = "Hi Jamie,\n\nThe project is on schedule.\n\nThanks,\nAlex";
        let statements = derive_edit_preferences(original, edited);
        assert!(statements.contains(&"Keep replies concise.".to_string()));
        assert!(statements.contains(&"Avoid exclamation marks.".to_string()));
        let joined = statements.join(" ");
        assert!(!joined.contains("project is on schedule"));
        assert!(!joined.contains("Jamie"));
    }

    #[test]
    fn exact_statement_is_bounded_and_redacted() {
        let normalized = normalize_statement(
            "Use a warm tone and do not include secret=abcd1234 or me@example.com",
        )
        .unwrap();
        assert!(normalized.contains("[secret omitted]"));
        assert!(normalized.contains("[email omitted]"));
        assert!(normalized.ends_with('.'));
    }

    #[test]
    fn memory_field_key_is_stable_and_scoped() {
        let preference = WritingPreference {
            id: "preference-1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "personal".to_string(),
            scope_kind: super::super::store::WritingPreferenceScopeKind::Sender,
            scope_value: "person@example.com".to_string(),
            statement: "Keep replies concise.".to_string(),
            status: super::super::store::WritingPreferenceStatus::Candidate,
            source_annotation_id: None,
            evidence_count: 1,
            created_at: 1,
            updated_at: 1,
            schema_version: super::super::types::MAIL_ASSIST_SCHEMA_VERSION,
        };
        assert_eq!(
            memory_field_key(&preference),
            "writing:gmail:personal:sender:preference-1"
        );
    }
}
