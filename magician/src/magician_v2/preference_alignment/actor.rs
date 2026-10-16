//! Who is writing the owner's knowledge, for the preference history. Each
//! entry point sets it around its write ([`with_actor`]); the history reads
//! it inside `persist_user_knowledge`. A write with no actor set is recorded
//! as `unknown`, never dropped.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteActor {
    /// `owner_surface`, `chat_tool`, `consolidator`, `alignment`,
    /// `memory_api`, `lifecycle`, `learning`, `analyst` or `unknown`.
    pub via: String,
    /// `web` or `ios` for an owner surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// The request, turn, ask or execution that caused the write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl WriteActor {
    pub fn unknown() -> Self {
        Self::via("unknown")
    }

    pub fn via(via: &str) -> Self {
        Self {
            via: via.to_string(),
            surface: None,
            cause: None,
        }
    }

    /// The owner, on one of their private surfaces.
    pub fn surface(surface: &str) -> Self {
        Self {
            via: "owner_surface".to_string(),
            surface: Some(surface.to_string()),
            cause: None,
        }
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }
}

tokio::task_local! {
    static ACTOR: WriteActor;
}

/// Run `future` with `actor` as the writer of any knowledge it persists.
pub async fn with_actor<F: std::future::Future>(actor: WriteActor, future: F) -> F::Output {
    ACTOR.scope(actor, future).await
}

/// Run `future` with `actor` as the writer unless an outer caller already
/// named one: the entry point that knows who is writing wins over the
/// shared helper it calls.
pub async fn with_default_actor<F: std::future::Future>(actor: WriteActor, future: F) -> F::Output {
    match ACTOR.try_with(|current| current.via != "unknown") {
        Ok(true) => future.await,
        _ => ACTOR.scope(actor, future).await,
    }
}

/// The writer set by the nearest [`with_actor`], or `unknown`.
pub fn current_actor() -> WriteActor {
    ACTOR
        .try_with(Clone::clone)
        .unwrap_or_else(|_| WriteActor::unknown())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_actor_is_scoped_to_the_future_and_defaults_to_unknown() {
        assert_eq!(current_actor(), WriteActor::unknown());
        let seen = with_actor(WriteActor::surface("web"), async { current_actor() }).await;
        assert_eq!(seen.surface.as_deref(), Some("web"));
        assert_eq!(seen.via, "owner_surface");
        assert_eq!(current_actor(), WriteActor::unknown());
    }
}
