//! Owner notices: the advisory nudges a check produced while letting an act
//! through, with any "remember this as my preference" the judge offered
//! (plan §8, §10.2; P7.1 for nudges).
//!
//! A nudge does not stop the act, so there is no pause to carry it. It is
//! recorded here, owner-private, and shown on the owner's private surfaces
//! (`GET /preference-alignment/notices`). A remember offer on a notice goes
//! through the same rule as one on a hold: the surface shows the exact
//! wording, and only a confirmation carrying the notice's token writes,
//! under a revision check (`lasting_write`). Nothing is promoted implicitly.
//!
//! The file is `memory/users/preference_alignment_notices.json`
//! (`alignment_notices.v1`), read and written under an exclusive flock and
//! bounded to the newest [`MAX_NOTICES`].

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::ask::{AskIntention, LastingWrite};
use super::lasting_write::{apply_lasting_write, WriteOutcome};
use crate::magician_v2::agents::memory_lifecycle::digest;
use crate::magician_v2::agents::storage::{AgentStorage, AgentStorageError};
use crate::magician_v2::agents::AgentMemoryResolver;

pub const ALIGNMENT_NOTICES_FILE: &str = "preference_alignment_notices.json";
pub const ALIGNMENT_NOTICES_SCHEMA_VERSION: &str = "alignment_notices.v1";
pub const MAX_NOTICES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeState {
    Open,
    /// The owner confirmed the remember offer and it was saved.
    Remembered,
    Dismissed,
    /// The preference changed before the owner answered; nothing was saved.
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberOffer {
    pub intention: AskIntention,
    pub write: LastingWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// The act the nudge was about (it was not held).
    pub act_summary: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intentions: Vec<AskIntention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<RememberOffer>,
    pub state: NoticeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_ms: Option<i64>,
}

impl Notice {
    /// A notice for one nudge. The id is stable for the same run, act and
    /// explanation, so a re-proposed act does not notify twice.
    pub fn for_nudge(
        execution_id: Option<String>,
        binding_id: &str,
        act_summary: String,
        explanation: String,
        intentions: Vec<AskIntention>,
        remember: Option<AskIntention>,
        now_ms: i64,
    ) -> Self {
        let id = digest(&json!(["alignment_notice.v1", execution_id, binding_id, explanation]))
            [..24]
            .to_string();
        // Only an inferred intention is "remembered"; an explicit one already is.
        let remember = remember
            .filter(|intention| {
                intention.assertion_status
                    == super::intention::AssertionStatus::Inferred
            })
            .and_then(|intention| {
            LastingWrite::remember(&id, &intention).map(|write| RememberOffer { intention, write })
        });
        Self {
            id,
            created_at_ms: now_ms,
            execution_id,
            act_summary,
            explanation,
            intentions,
            remember,
            state: NoticeState::Open,
            resolved_at_ms: None,
        }
    }
}

impl Notice {
    /// A hold nobody answered before its deadline: the act was not run
    /// (P5.4). Owner-private like every notice; the reason is the hold's own.
    pub fn for_expired_hold(
        execution_id: Option<String>,
        ask: &super::ask::AlignmentAsk,
        now_ms: i64,
    ) -> Self {
        let detail = ask.detail.clone().unwrap_or_default();
        let explanation = if detail.explanation.is_empty() {
            "An action waited for your review until its deadline and was not run.".to_string()
        } else {
            format!(
                "An action waited for your review until its deadline and was not run. \
                 {}",
                detail.explanation
            )
        };
        Self {
            id: digest(&json!(["alignment_notice.expired.v1", ask.ask_id, ask.binding_id]))[..24]
                .to_string(),
            created_at_ms: now_ms,
            execution_id,
            act_summary: detail.act_summary,
            explanation,
            intentions: detail.intentions,
            remember: None,
            state: NoticeState::Open,
            resolved_at_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NoticesFile {
    schema_version: String,
    #[serde(default)]
    notices: Vec<Notice>,
}

impl Default for NoticesFile {
    fn default() -> Self {
        Self {
            schema_version: ALIGNMENT_NOTICES_SCHEMA_VERSION.to_string(),
            notices: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NoticeError {
    #[error("notices scope unresolved: {0}")]
    Scope(String),
    #[error("notices lock failed: {0}")]
    Lock(String),
    #[error("notices unreadable: {0}")]
    Read(String),
    #[error("notices corrupt: {0}")]
    Corrupt(String),
    #[error("notices write failed: {0}")]
    Write(String),
    #[error("lasting write failed: {0}")]
    LastingWrite(String),
}

/// What answering a notice did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeOutcome {
    Resolved(Notice),
    NotFound,
    /// Already answered; the notice as it stands.
    AlreadyResolved(Notice),
    /// A remember confirmation without this notice's token.
    WrongToken,
    /// The notice offers nothing to remember.
    NothingToRemember,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeAnswer<'a> {
    Remember { token: &'a str },
    Dismiss,
}

pub fn alignment_notices_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(ALIGNMENT_NOTICES_FILE)
}

fn storage_for(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<AgentStorage, NoticeError> {
    resolver
        .resolve_for_scope(principal, workspace)
        .map(|memory| memory.storage().clone())
        .map_err(|error| NoticeError::Scope(error.to_string()))
}

async fn read_file(storage: &AgentStorage, path: &PathBuf) -> Result<NoticesFile, NoticeError> {
    match storage.read_bytes(path).await {
        Ok(bytes) => {
            let file: NoticesFile = serde_json::from_slice(&bytes)
                .map_err(|error| NoticeError::Corrupt(error.to_string()))?;
            if file.schema_version != ALIGNMENT_NOTICES_SCHEMA_VERSION {
                return Err(NoticeError::Corrupt(format!(
                    "unsupported schema version `{}`",
                    file.schema_version
                )));
            }
            Ok(file)
        },
        Err(AgentStorageError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
            Ok(NoticesFile::default())
        },
        Err(error) => Err(NoticeError::Read(error.to_string())),
    }
}

async fn locked_path(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<(AgentStorage, PathBuf), NoticeError> {
    let storage = storage_for(resolver, principal, workspace)?;
    let path = alignment_notices_path(&storage);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| NoticeError::Write(error.to_string()))?;
    }
    Ok((storage, path))
}

/// Record a notice. A notice with the same id is kept as it is.
pub async fn record_notice(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    notice: Notice,
) -> Result<bool, NoticeError> {
    let (storage, path) = locked_path(resolver, principal, workspace).await?;
    let _flock = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| NoticeError::Lock(error.to_string()))?;
    let mut file = match read_file(&storage, &path).await {
        Ok(file) => file,
        // A corrupt notices file is replaced: notices are advisory, and a
        // file that refused every write would hide every later nudge.
        Err(NoticeError::Corrupt(_)) => NoticesFile::default(),
        Err(error) => return Err(error),
    };
    if file.notices.iter().any(|existing| existing.id == notice.id) {
        return Ok(false);
    }
    file.notices.push(notice);
    if file.notices.len() > MAX_NOTICES {
        // Answered notices go first, then the oldest.
        while file.notices.len() > MAX_NOTICES {
            let index = file
                .notices
                .iter()
                .position(|existing| existing.state != NoticeState::Open)
                .unwrap_or(0);
            file.notices.remove(index);
        }
    }
    storage
        .write_json_atomic(&path, &file)
        .await
        .map_err(|error| NoticeError::Write(error.to_string()))?;
    Ok(true)
}

/// The owner's notices, newest first.
pub async fn list_notices(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<Vec<Notice>, NoticeError> {
    let storage = storage_for(resolver, principal, workspace)?;
    let path = alignment_notices_path(&storage);
    let mut notices = read_file(&storage, &path).await?.notices;
    notices.reverse();
    Ok(notices)
}

/// Answer one notice. A remember answer must carry the notice's token; the
/// write then goes through `lasting_write` and its revision check.
pub async fn answer_notice(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    id: &str,
    answer: NoticeAnswer<'_>,
    now: DateTime<Utc>,
) -> Result<NoticeOutcome, NoticeError> {
    let (storage, path) = locked_path(resolver, principal, workspace).await?;
    // Lock order: notices, then knowledge (inside `apply_lasting_write`).
    let _flock = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| NoticeError::Lock(error.to_string()))?;
    let mut file = read_file(&storage, &path).await?;
    let Some(index) = file.notices.iter().position(|notice| notice.id == id) else {
        return Ok(NoticeOutcome::NotFound);
    };
    if file.notices[index].state != NoticeState::Open {
        return Ok(NoticeOutcome::AlreadyResolved(file.notices[index].clone()));
    }
    let state = match answer {
        NoticeAnswer::Dismiss => NoticeState::Dismissed,
        NoticeAnswer::Remember { token } => {
            let Some(offer) = file.notices[index].remember.clone() else {
                return Ok(NoticeOutcome::NothingToRemember);
            };
            if token.trim() != offer.write.token {
                return Ok(NoticeOutcome::WrongToken);
            }
            match apply_lasting_write(resolver, principal, workspace, &offer.write, id, now)
                .await
                .map_err(|error| NoticeError::LastingWrite(error.to_string()))?
            {
                WriteOutcome::Applied | WriteOutcome::AlreadyApplied => NoticeState::Remembered,
                WriteOutcome::Stale | WriteOutcome::NotFound => NoticeState::Stale,
            }
        },
    };
    let notice = &mut file.notices[index];
    notice.state = state;
    notice.resolved_at_ms = Some(now.timestamp_millis());
    let resolved = notice.clone();
    storage
        .write_json_atomic(&path, &file)
        .await
        .map_err(|error| NoticeError::Write(error.to_string()))?;
    Ok(NoticeOutcome::Resolved(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::intention::{
        AssertionStatus, IntentionKind, IntentionSource,
    };
    use crate::magician_v2::preference_alignment::sources::native::assertion_status;

    fn knowledge() -> serde_json::Value {
        json!({"preferences": [
            {"memory_record_id": "inf-1", "key": "spice", "value": "mild", "source_type": "inferred"}
        ]})
    }

    fn inferred(doc: &serde_json::Value) -> AskIntention {
        AskIntention {
            id: "inf-1".into(),
            revision: digest(&doc["preferences"][0]),
            source: IntentionSource::NativeTier {
                tier: "preferences".into(),
            },
            kind: IntentionKind::Taste,
            assertion_status: AssertionStatus::Inferred,
            statement: "spice: mild".into(),
        }
    }

    #[tokio::test]
    async fn a_nudges_remember_offer_saves_only_with_its_token() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let memory = resolver.resolve_for_scope("owner", "ws").unwrap();
        memory.save_user_knowledge(&knowledge()).await.unwrap();

        let notice = Notice::for_nudge(
            Some("exec-1".into()),
            "alignment_binding.v1:x",
            "order".into(),
            "You usually order mild food.".into(),
            vec![inferred(&knowledge())],
            Some(inferred(&knowledge())),
            1,
        );
        let token = notice.remember.as_ref().expect("offer").write.token.clone();
        assert!(record_notice(&resolver, "owner", "ws", notice.clone()).await.unwrap());
        // The same nudge again is not a second notice.
        assert!(!record_notice(&resolver, "owner", "ws", notice.clone()).await.unwrap());
        assert_eq!(list_notices(&resolver, "owner", "ws").await.unwrap().len(), 1);

        assert_eq!(
            answer_notice(
                &resolver,
                "owner",
                "ws",
                &notice.id,
                NoticeAnswer::Remember { token: "guess" },
                Utc::now()
            )
            .await
            .unwrap(),
            NoticeOutcome::WrongToken
        );
        let NoticeOutcome::Resolved(resolved) = answer_notice(
            &resolver,
            "owner",
            "ws",
            &notice.id,
            NoticeAnswer::Remember { token: &token },
            Utc::now(),
        )
        .await
        .unwrap() else {
            panic!("the confirmed offer is saved");
        };
        assert_eq!(resolved.state, NoticeState::Remembered);
        let saved = memory.load_user_knowledge().await.unwrap();
        assert_eq!(assertion_status(&saved["preferences"][0]), AssertionStatus::Confirmed);
        // Answered once.
        assert!(matches!(
            answer_notice(&resolver, "owner", "ws", &notice.id, NoticeAnswer::Dismiss, Utc::now())
                .await
                .unwrap(),
            NoticeOutcome::AlreadyResolved(_)
        ));
    }

    #[tokio::test]
    async fn a_preference_that_changed_since_the_nudge_is_not_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let memory = resolver.resolve_for_scope("owner", "ws").unwrap();
        let notice = Notice::for_nudge(
            None,
            "b",
            "order".into(),
            "x".into(),
            vec![],
            Some(inferred(&knowledge())),
            1,
        );
        let token = notice.remember.as_ref().unwrap().write.token.clone();
        record_notice(&resolver, "owner", "ws", notice.clone()).await.unwrap();
        let mut changed = knowledge();
        changed["preferences"][0]["value"] = json!("medium");
        memory.save_user_knowledge(&changed).await.unwrap();
        let NoticeOutcome::Resolved(resolved) = answer_notice(
            &resolver,
            "owner",
            "ws",
            &notice.id,
            NoticeAnswer::Remember { token: &token },
            Utc::now(),
        )
        .await
        .unwrap() else {
            panic!()
        };
        assert_eq!(resolved.state, NoticeState::Stale);
        let saved = memory.load_user_knowledge().await.unwrap();
        assert_eq!(saved["preferences"][0]["source_type"], json!("inferred"));
    }
}
