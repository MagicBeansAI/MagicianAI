//! The engine that started the current flow, for routing its background
//! operations. Explicit on the flow's routing overrides where a flow threads
//! them (runs, plane grants); ambient through a task-local where a flow is
//! one future (a chat turn, an MCP call, a run phase). Never a process value.
//!
//! The value is an engine name from the harness roster. `magician` is the
//! service's own native loop, so it never names a parent: a flow it starts
//! leaves every operation on that operation's own configured profile.

/// The native engine name. A flow this engine starts has no parent.
const NATIVE_ENGINE: &str = "magician";

tokio::task_local! {
    /// The parent engine of the flow this task belongs to. Absent (or `None`)
    /// outside any flow: boot paths, cron sweeps, tests.
    static PARENT_ENGINE: Option<String>;
}

/// Normalise an engine name at the flow boundary: trimmed, lowercased, and
/// `None` when it is empty or names the native engine.
pub fn normalize_parent_engine(engine: Option<&str>) -> Option<String> {
    let engine = engine?.trim().to_ascii_lowercase();
    (!engine.is_empty() && engine != NATIVE_ENGINE).then_some(engine)
}

/// Run `future` with `engine` as the flow's parent. `magician` and empty
/// clear the parent rather than naming one, so a nested native flow does not
/// inherit an outer harness by accident. Not `async`: the engine is read
/// eagerly, so the returned future borrows nothing from the caller and can be
/// spawned or scheduled on another task as-is. That relies on the edition-2021
/// rule that a return-position `impl Trait` captures the type parameters in
/// scope but not the elided lifetime of `engine` (edition 2024 captures every
/// in-scope lifetime, and would need `+ use<F>` to say the same).
pub fn with_parent_engine<F: std::future::Future>(
    engine: Option<&str>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    PARENT_ENGINE.scope(normalize_parent_engine(engine), future)
}

/// The ambient parent, if this task runs inside a flow that named one.
pub fn current_parent_engine() -> Option<String> {
    PARENT_ENGINE
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|engine| !engine.is_empty() && engine != NATIVE_ENGINE)
}

/// A CLI client's engine family from an MCP `clientInfo.name`. The name is
/// split on every non-alphanumeric character, case-insensitively, and
/// matched on whole tokens, so vendor spellings and version suffixes map
/// alike while a vendor's other products do not: a name that carries the
/// vendor alone — its desktop or web app — is not that vendor's CLI. An
/// unrecognised client has no parent.
pub fn engine_family_from_client_name(name: &str) -> Option<&'static str> {
    let lowered = name.to_ascii_lowercase();
    let tokens: Vec<&str> = lowered
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    let has = |token: &str| tokens.iter().any(|candidate| *candidate == token);
    if has("claude") && has("code") {
        Some("claude_code")
    } else if has("codex") {
        Some("codex")
    } else if has("grok") {
        Some("grok")
    } else if has("antigravity") || has("agy") {
        Some("agy")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_parent_is_scoped_to_the_future() {
        assert_eq!(current_parent_engine(), None);

        let (outer_before, inner, outer_after) = with_parent_engine(Some("codex"), async {
            let outer_before = current_parent_engine();
            let inner =
                with_parent_engine(Some("claude_code"), async { current_parent_engine() }).await;
            let outer_after = current_parent_engine();
            (outer_before, inner, outer_after)
        })
        .await;

        assert_eq!(outer_before.as_deref(), Some("codex"));
        assert_eq!(inner.as_deref(), Some("claude_code"));
        assert_eq!(outer_after.as_deref(), Some("codex"));
        assert_eq!(current_parent_engine(), None);
    }

    /// The scoped future owns its engine name, so a flow entry that has to
    /// hand the future to another task can — the borrow ends at the call.
    #[tokio::test]
    async fn the_scoped_future_can_be_spawned() {
        let engine = String::from("codex");
        let scoped = with_parent_engine(Some(engine.as_str()), async { current_parent_engine() });
        drop(engine);
        let seen = tokio::spawn(scoped).await.expect("scoped task joins");
        assert_eq!(seen.as_deref(), Some("codex"));
    }

    #[tokio::test]
    async fn the_parent_is_normalised_at_the_boundary() {
        let seen = with_parent_engine(Some("  Grok \n"), async { current_parent_engine() }).await;
        assert_eq!(seen.as_deref(), Some("grok"));
    }

    #[tokio::test]
    async fn magician_is_not_a_parent() {
        let native = with_parent_engine(Some("magician"), async { current_parent_engine() }).await;
        assert_eq!(native, None);

        let empty = with_parent_engine(Some(""), async { current_parent_engine() }).await;
        assert_eq!(empty, None);

        let absent = with_parent_engine(None, async { current_parent_engine() }).await;
        assert_eq!(absent, None);

        // A native flow nested in a harness flow clears the parent rather
        // than inheriting the outer engine.
        let nested = with_parent_engine(Some("codex"), async {
            with_parent_engine(Some("Magician"), async { current_parent_engine() }).await
        })
        .await;
        assert_eq!(nested, None);
    }

    #[test]
    fn client_names_map_to_roster_families() {
        let table: &[(&str, Option<&str>)] = &[
            ("claude-code", Some("claude_code")),
            ("Claude Code", Some("claude_code")),
            ("claude_code/2.1.0", Some("claude_code")),
            // The vendor's other products are not its CLI.
            ("claude", None),
            ("claude-ai", None),
            ("claude-desktop", None),
            ("codex", Some("codex")),
            ("codex-cli", Some("codex")),
            ("Codex MCP client", Some("codex")),
            ("grok", Some("grok")),
            ("Grok CLI", Some("grok")),
            ("antigravity", Some("agy")),
            ("Antigravity", Some("agy")),
            ("agy", Some("agy")),
            // Whole tokens only: a family name buried in another word is
            // not that family.
            ("decodex", None),
            ("cursor", None),
            ("", None),
            ("   ", None),
        ];
        for (name, family) in table {
            assert_eq!(
                engine_family_from_client_name(name),
                *family,
                "client name {name:?}"
            );
        }
    }
}
