//! Bot tokens — the in-memory broker for bot daemons the runtime spawns.
//!
//! A bot is a child process the runtime itself launched, from a config that
//! lives at `scopes/<principal>/<workspace>/bots/<bot>/`. The runtime therefore
//! already KNOWS the bot's scope with certainty. The token exists only because
//! the bot calls back over HTTP, and HTTP carries no process identity — so the
//! bearer re-establishes at the API boundary a fact the spawn already had.
//!
//! Because of that, a bot token is deliberately **not** a persisted credential:
//!
//! * minted in-process immediately before the spawn,
//! * injected as `MAGICIAN_BEARER_TOKEN` into the child's environment,
//! * revoked the moment the bot stops,
//! * never written to disk, so there is nothing at rest to leak or rotate, and
//!   consequently no TTL and no refresh endpoint.
//!
//! Its lifetime is exactly the bot process's lifetime. A runtime restart drops
//! every grant, which is correct: the bots are its children and restart with it.
//!
//! Mirrors [`crate::magician_v2::execution::coding_engine::citizen::CitizenTokenRegistry`]
//! and `CodingControlRegistry` — a process-global map whose entries live for
//! exactly one spawn. Unlike a citizen token (which authenticates to the
//! separate Citizen API), a bot token is accepted by the ordinary
//! `/api/magician/v2` middleware and engraves the scope like any other bearer.
//!
//! # Known limitation: scope breadth
//!
//! A bot token grants the **whole API surface of its scope** — the same breadth
//! a `mag_pat_` for that workspace would carry. It is narrower than a PAT only
//! in lifetime, not in authority.
//!
//! A per-bot tool floor (as [`CitizenGrant::allowed_tools`] gives coding runs)
//! is deliberately NOT modelled here yet, because this middleware engraves a
//! scope and does not see tool names — a field stored but never enforced would
//! read as a control while doing nothing, which is worse than its absence.
//! Adding one means an enforcement point that can see the dispatch, and that is
//! its own change.
//!
//! [`CitizenGrant::allowed_tools`]: crate::magician_v2::execution::coding_engine::citizen::CitizenGrant

use std::collections::HashMap;
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use super::sessions::mint_token_value;

/// Bearer prefix for bot tokens.
///
/// MUST be classified before [`super::sessions::SESSION_TOKEN_PREFIX`] (`mag_`),
/// which is a prefix of this one — see `classify_token`.
pub const BOT_TOKEN_PREFIX: &str = "mag_bot_";

/// What a bot token resolves to — the scope the bot's API calls run within.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotGrant {
    pub principal: String,
    pub workspace: String,
    /// Bot name as keyed in `bot_configs.yaml` (`telegram`, `kapso`, …).
    pub bot_name: String,
}

impl BotGrant {
    /// The identity a grant is replaced and revoked by.
    ///
    /// Scope-qualified on purpose: a bot name alone is NOT unique. Two scopes
    /// may each run `telegram`, and keying on the bare name would let one
    /// workspace's restart silently revoke the other's live credential.
    fn slot(&self) -> (&str, &str, &str) {
        (
            self.principal.as_str(),
            self.workspace.as_str(),
            self.bot_name.as_str(),
        )
    }
}

#[derive(Default)]
pub struct BotTokenRegistry {
    grants: Mutex<HashMap<String, BotGrant>>,
}

static REGISTRY: Lazy<Arc<BotTokenRegistry>> = Lazy::new(|| Arc::new(BotTokenRegistry::default()));

/// The process-global bot token registry.
pub fn bot_token_registry() -> Arc<BotTokenRegistry> {
    REGISTRY.clone()
}

impl BotTokenRegistry {
    /// Mint a fresh opaque token for `grant` and register it.
    ///
    /// Any token previously issued to the same `(principal, workspace, bot)`
    /// slot is revoked first, so a crash-restart cannot leave a usable
    /// credential behind for a process that no longer exists.
    pub fn mint(&self, grant: BotGrant) -> String {
        let token = mint_token_value(BOT_TOKEN_PREFIX);
        let mut grants = self.grants.lock();
        let slot = grant.slot();
        grants.retain(|_, existing| existing.slot() != slot);
        grants.insert(token.clone(), grant);
        token
    }

    /// Resolve a token to its grant. `None` for anything unknown or revoked.
    pub fn resolve(&self, token: &str) -> Option<BotGrant> {
        self.grants.lock().get(token.trim()).cloned()
    }

    /// Revoke one token. Idempotent.
    pub fn revoke(&self, token: &str) {
        self.grants.lock().remove(token.trim());
    }

    /// Revoke the token held by one bot in one scope, whatever its value. Used
    /// on stop, where the caller may no longer hold the minted string.
    ///
    /// Scope-qualified for the same reason as [`BotGrant::slot`]: stopping one
    /// workspace's `telegram` must not sign out another's.
    pub fn revoke_bot(&self, principal: &str, workspace: &str, bot_name: &str) {
        self.grants
            .lock()
            .retain(|_, grant| grant.slot() != (principal, workspace, bot_name));
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn len(&self) -> usize {
        self.grants.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant_in(principal: &str, workspace: &str, bot: &str) -> BotGrant {
        BotGrant {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            bot_name: bot.to_string(),
        }
    }

    fn grant(bot: &str) -> BotGrant {
        grant_in("anonymous", "default", bot)
    }

    #[test]
    fn a_minted_token_resolves_to_its_scope() {
        let reg = BotTokenRegistry::default();
        let token = reg.mint(grant("telegram"));
        assert!(token.starts_with(BOT_TOKEN_PREFIX));
        assert_eq!(reg.resolve(&token), Some(grant("telegram")));
    }

    /// The prefix is a superset of the session prefix, so a bot token must not
    /// be mistaken for one. Guards the ordering requirement in `classify_token`.
    #[test]
    fn the_bot_prefix_extends_the_session_prefix() {
        assert!(BOT_TOKEN_PREFIX.starts_with(super::super::sessions::SESSION_TOKEN_PREFIX));
    }

    #[test]
    fn a_revoked_token_stops_resolving() {
        let reg = BotTokenRegistry::default();
        let token = reg.mint(grant("kapso"));
        reg.revoke(&token);
        assert_eq!(reg.resolve(&token), None);
    }

    /// A crash-restart mints again; the dead process's credential must not
    /// outlive it.
    #[test]
    fn re_minting_for_a_bot_invalidates_its_previous_token() {
        let reg = BotTokenRegistry::default();
        let first = reg.mint(grant("telegram"));
        let second = reg.mint(grant("telegram"));
        assert_ne!(first, second);
        assert_eq!(reg.resolve(&first), None);
        assert_eq!(reg.resolve(&second), Some(grant("telegram")));
    }

    /// **The cross-scope bug this keying exists to prevent.** Two workspaces
    /// each run `telegram`; restarting one must not revoke the other's token.
    #[test]
    fn re_minting_one_scopes_bot_leaves_the_same_bot_in_another_scope_alone() {
        let reg = BotTokenRegistry::default();
        let other = reg.mint(grant_in("owner", "company", "telegram"));
        let _mine = reg.mint(grant_in("anonymous", "default", "telegram"));
        let _mine_again = reg.mint(grant_in("anonymous", "default", "telegram"));

        assert_eq!(
            reg.resolve(&other),
            Some(grant_in("owner", "company", "telegram")),
            "another scope's telegram token must survive this scope's restart"
        );
    }

    #[test]
    fn revoking_by_bot_is_scope_qualified() {
        let reg = BotTokenRegistry::default();
        let other = reg.mint(grant_in("owner", "company", "whatsapp"));
        let mine = reg.mint(grant_in("anonymous", "default", "whatsapp"));

        reg.revoke_bot("anonymous", "default", "whatsapp");

        assert_eq!(reg.resolve(&mine), None);
        assert_eq!(
            reg.resolve(&other),
            Some(grant_in("owner", "company", "whatsapp")),
            "stopping one scope's bot must not sign out another scope's"
        );
    }

    /// Re-minting is per-slot, not global: one bot restarting must not sign the
    /// other bots in its own scope out either.
    #[test]
    fn re_minting_one_bot_leaves_its_siblings_alone() {
        let reg = BotTokenRegistry::default();
        let kapso = reg.mint(grant("kapso"));
        let _telegram = reg.mint(grant("telegram"));
        let _telegram_again = reg.mint(grant("telegram"));
        assert_eq!(reg.resolve(&kapso), Some(grant("kapso")));
    }

    #[test]
    fn an_unknown_token_resolves_to_nothing() {
        let reg = BotTokenRegistry::default();
        assert_eq!(reg.resolve("mag_bot_nope"), None);
    }
}
