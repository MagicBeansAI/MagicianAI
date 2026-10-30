//! Claiming an owner identity from a channel: `/claim <code>`.
//!
//! The owner should never have to hand-type their WhatsApp number or Telegram
//! chat id. Instead the owner mints a one-time code in Magician (Settings →
//! Owner identities, owner auth only) and sends `/claim <code>` to the
//! Magician bot from the chat they want recognised. The chat API answers the
//! command before any agent turn (`chat_api::owner_claim_command_response`)
//! and records the sender address the transport verified as an owner identity
//! for that channel.
//!
//! # What is trusted
//!
//! - The **address** is the one the channel bot reported for an authenticated
//!   request on a transport that authenticates senders
//!   ([`envoy::channel_is_verified`]) — the same root owner routing uses. Mail
//!   channels never qualify: a `From:` header is not proof.
//! - The **code** proves the owner wanted this: only an owner-authenticated
//!   API call mints one. Codes are 8 characters from an unambiguous 31-letter
//!   alphabet (~8.5e11 values), live 10 minutes, are single use, compared in
//!   constant time, and a sender gets [`MAX_FAILED_ATTEMPTS`] wrong tries per
//!   [`FAILED_ATTEMPT_WINDOW_MS`] before every further try is refused. Enough
//!   failures across all senders burns every live code.
//!
//! # Where the identity goes
//!
//! A claim only APPENDS to the channel's `envoy.owner_identity_envs` variable
//! (e.g. `MAGICIAN_OWNER_KAPSO_IDENTITIES`): in both live env files, then the
//! process env, so the next message routes as the owner with no restart
//! (`owner_identities_for` reads the variable at call time). Hand-set values
//! in that variable and in `envoy.owner_identities` keep working unchanged —
//! existing entries are never reordered or rewritten, and no other env key is
//! touched. A channel with no identity variable cannot be claimed: the claim
//! is refused with a message naming the setting, because the routing config
//! the chat API holds is a startup snapshot and a config-map write would not
//! take effect until restart.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rand::Rng;

use crate::config::{channel_addresses_are_phones, owner_identity_matches, EnvoyConfig};
use crate::magician_v2::chat::envoy;
use crate::magician_v2::runtime_settings::{
    read_env_file_values, update_env_file, RuntimeSettingsPaths,
};

/// Characters in a claim code.
pub const CLAIM_CODE_LEN: usize = 8;
/// How long a minted code stays redeemable.
pub const CLAIM_CODE_TTL_MS: i64 = 10 * 60 * 1000;
/// Wrong codes one sender may try per window before every try is refused.
pub const MAX_FAILED_ATTEMPTS: usize = 5;
/// The failed-attempt window.
pub const FAILED_ATTEMPT_WINDOW_MS: i64 = 10 * 60 * 1000;
/// Wrong codes across all senders per window before every live code is burned.
const MAX_GLOBAL_FAILURES: usize = 50;
/// Live codes kept at once; minting more drops the oldest.
const MAX_LIVE_CODES: usize = 4;
/// No `0/O`, `1/I/L`: a code read off a screen is typed right.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// Longest submitted code considered (anything longer is wrong anyway).
const MAX_SUBMITTED_LEN: usize = 64;

/// `/claim <code>` typed into a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCommand {
    /// The normalized code, `None` when the command came without one.
    pub code: Option<String>,
}

impl ClaimCommand {
    /// The command the whole message is, if it is one.
    ///
    /// Trimmed and case-insensitive: `/claim ABCD2345`, `/claim abcd-2345`,
    /// `/claim@MagicianBot ABCD2345`, and a control prefix a bot did not strip
    /// (`/magic /claim …`, `@magic /claim …`).
    pub fn parse(text: &str) -> Option<Self> {
        let mut rest = text.trim();
        for prefix in ["/magic", "@magic"] {
            if let Some(after) = strip_prefix_ignore_case(rest, prefix) {
                if after.starts_with(char::is_whitespace) {
                    rest = after.trim_start();
                    break;
                }
            }
        }
        let rest = rest.strip_prefix('/')?;
        let mut words = rest.split_whitespace();
        let first = words.next()?;
        let name = first.split('@').next().unwrap_or(first);
        if !name.eq_ignore_ascii_case("claim") {
            return None;
        }
        let code = normalize_code(&words.collect::<Vec<_>>().concat());
        Some(Self {
            code: (!code.is_empty()).then_some(code),
        })
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

/// Upper-cased alphanumerics only: `abcd-2345` and `ABCD 2345` are `ABCD2345`.
pub fn normalize_code(raw: &str) -> String {
    raw.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(MAX_SUBMITTED_LEN)
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

fn random_code() -> String {
    let mut rng = rand::rngs::OsRng;
    (0..CLAIM_CODE_LEN)
        .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// One minted code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCode {
    pub code: String,
    /// Lower-case channel type the code is restricted to, if any.
    pub channel: Option<String>,
    pub expires_at_ms: i64,
}

/// What redeeming a submitted code did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// The code was live for this channel and is now spent.
    Redeemed(ClaimCode),
    /// Wrong, expired, spent or for another channel. Counted as a failure.
    Invalid,
    /// This sender has used up its tries for the window. Not checked.
    RateLimited,
}

/// Live codes and recent failures. Pure: the caller supplies the time.
#[derive(Debug, Default)]
pub struct ClaimRegistry {
    codes: Vec<ClaimCode>,
    failures: HashMap<String, Vec<i64>>,
}

impl ClaimRegistry {
    /// Mint a fresh random code.
    pub fn mint(&mut self, channel: Option<&str>, now_ms: i64) -> ClaimCode {
        self.mint_code(&random_code(), channel, now_ms)
    }

    /// Mint `code` (tests pick it; [`Self::mint`] draws it at random).
    pub fn mint_code(&mut self, code: &str, channel: Option<&str>, now_ms: i64) -> ClaimCode {
        self.prune(now_ms);
        while self.codes.len() >= MAX_LIVE_CODES {
            self.codes.remove(0);
        }
        let minted = ClaimCode {
            code: normalize_code(code),
            channel: channel
                .map(|channel| channel.trim().to_ascii_lowercase())
                .filter(|channel| !channel.is_empty()),
            expires_at_ms: now_ms + CLAIM_CODE_TTL_MS,
        };
        self.codes.push(minted.clone());
        minted
    }

    /// Redeem `submitted` for a sender. Every live code is compared, each in
    /// constant time, so timing says nothing about which part was right.
    pub fn redeem(
        &mut self,
        channel: &str,
        address: &str,
        submitted: &str,
        now_ms: i64,
    ) -> RedeemOutcome {
        self.prune(now_ms);
        let key = attempt_key(channel, address);
        if self
            .failures
            .get(&key)
            .is_some_and(|failures| failures.len() >= MAX_FAILED_ATTEMPTS)
        {
            return RedeemOutcome::RateLimited;
        }
        let submitted = normalize_code(submitted);
        let channel = channel.trim().to_ascii_lowercase();
        let mut matched = None;
        for (index, code) in self.codes.iter().enumerate() {
            let same = constant_time_eq(code.code.as_bytes(), submitted.as_bytes());
            let channel_ok = code
                .channel
                .as_deref()
                .map_or(true, |restricted| restricted == channel);
            if same && channel_ok && matched.is_none() {
                matched = Some(index);
            }
        }
        match matched {
            Some(index) => {
                self.failures.remove(&key);
                RedeemOutcome::Redeemed(self.codes.remove(index))
            },
            None => {
                self.failures.entry(key).or_default().push(now_ms);
                let total: usize = self.failures.values().map(Vec::len).sum();
                if total >= MAX_GLOBAL_FAILURES {
                    self.codes.clear();
                }
                RedeemOutcome::Invalid
            },
        }
    }

    /// Put back a redeemed code whose claim could not be saved.
    pub fn restore(&mut self, code: ClaimCode) {
        if !self.codes.iter().any(|live| live.code == code.code) {
            self.codes.push(code);
        }
    }

    /// Live codes right now.
    pub fn live_codes(&mut self, now_ms: i64) -> usize {
        self.prune(now_ms);
        self.codes.len()
    }

    fn prune(&mut self, now_ms: i64) {
        self.codes.retain(|code| code.expires_at_ms > now_ms);
        self.failures.retain(|_, failures| {
            failures.retain(|at| now_ms - *at < FAILED_ATTEMPT_WINDOW_MS);
            !failures.is_empty()
        });
    }
}

/// The rate-limit key: channel plus the address, digits-only for a phone.
fn attempt_key(channel: &str, address: &str) -> String {
    let channel = channel.trim().to_ascii_lowercase();
    let address = address.trim();
    let digits: String = address.chars().filter(char::is_ascii_digit).collect();
    let address = if channel_addresses_are_phones(&channel) && !digits.is_empty() {
        digits
    } else {
        address.to_ascii_lowercase()
    };
    format!("{channel}:{address}")
}

/// The process-wide registry the API mints into and chat ingress redeems from.
pub fn registry() -> &'static Mutex<ClaimRegistry> {
    static REGISTRY: OnceLock<Mutex<ClaimRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(ClaimRegistry::default()))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Mint a code in the process registry.
pub fn mint_claim_code(channel: Option<&str>) -> ClaimCode {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .mint(channel, now_ms())
}

/// Whether a channel can be claimed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimChannel {
    /// Verified transport with an identity variable to append to.
    Claimable { env_name: String },
    /// The transport does not authenticate the sender (mail), or it is `web`.
    Unverified,
    /// Verified, but `envoy.owner_identity_envs` names no variable for it.
    NoIdentityVariable,
}

/// How `channel_type` can be claimed under `cfg`.
pub fn claim_channel(cfg: &EnvoyConfig, channel_type: &str) -> ClaimChannel {
    let channel = channel_type.trim().to_ascii_lowercase();
    if channel.is_empty() || channel == "web" || !envoy::channel_is_verified(&channel, true, None) {
        return ClaimChannel::Unverified;
    }
    let env_name = cfg
        .owner_identity_envs
        .iter()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(&channel))
        .map(|(_, env)| env.trim().to_string())
        .filter(|env| is_env_key(env));
    match env_name {
        Some(env_name) => ClaimChannel::Claimable { env_name },
        None => ClaimChannel::NoIdentityVariable,
    }
}

/// Channels a code can be redeemed from under `cfg`, sorted.
pub fn claimable_channels(cfg: &EnvoyConfig) -> Vec<String> {
    let mut channels: Vec<String> = cfg
        .owner_identity_envs
        .keys()
        .map(|channel| channel.trim().to_ascii_lowercase())
        .filter(|channel| matches!(claim_channel(cfg, channel), ClaimChannel::Claimable { .. }))
        .collect();
    channels.sort();
    channels.dedup();
    channels
}

fn is_env_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Every channel with an identity source (config map or variable), sorted.
pub fn identity_channels(cfg: &EnvoyConfig) -> Vec<String> {
    let mut channels: Vec<String> = cfg
        .owner_identities
        .keys()
        .chain(cfg.owner_identity_envs.keys())
        .map(|channel| channel.trim().to_ascii_lowercase())
        .filter(|channel| !channel.is_empty())
        .collect();
    channels.sort();
    channels.dedup();
    channels
}

fn split_identities(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', ';', '\n'])
        .map(str::trim)
        .filter(|identity| !identity.is_empty())
}

/// `existing` with `address` appended, or `None` when it already names the
/// address. Existing text is kept as written — separators, order, spacing —
/// and the new entry goes after it.
pub fn with_identity(existing: &str, channel: &str, address: &str) -> Option<String> {
    let address = address.trim();
    if address.is_empty()
        || split_identities(existing)
            .any(|identity| owner_identity_matches(channel, identity, address))
    {
        return None;
    }
    let kept = existing.trim_end_matches(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';');
    Some(if kept.is_empty() {
        address.to_string()
    } else {
        format!("{kept},{address}")
    })
}

/// `existing` without the entry exactly equal to `identity`, or `None` when
/// it has no such entry. The other entries keep their order.
pub fn without_identity(existing: &str, identity: &str) -> Option<String> {
    let identity = identity.trim();
    let entries: Vec<&str> = split_identities(existing).collect();
    if !entries.contains(&identity) {
        return None;
    }
    Some(
        entries
            .into_iter()
            .filter(|entry| *entry != identity)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// The env files a claim writes: both live files that exist, else the
/// active one.
fn env_files(paths: &RuntimeSettingsPaths) -> Vec<&Path> {
    let mut files: Vec<&Path> = [
        paths.env_development_path.as_path(),
        paths.env_path.as_path(),
    ]
    .into_iter()
    .filter(|path| path.is_file())
    .collect();
    if files.is_empty() {
        files.push(paths.env_target_path.as_path());
    }
    files
}

/// Append `address` to `env_name` in both live env files and the process env.
///
/// Only that one key's line changes. A file that lacks the key gets the
/// process value plus the address, so a file loaded with precedence never
/// shadows identities the other file already held.
pub fn persist_env_identity(
    paths: &RuntimeSettingsPaths,
    env_name: &str,
    channel: &str,
    address: &str,
) -> std::io::Result<()> {
    let process_value = std::env::var(env_name).unwrap_or_default();
    for file in env_files(paths) {
        let values = read_env_file_values(file);
        let in_file = values.get(env_name);
        let base = in_file.cloned().unwrap_or_else(|| process_value.clone());
        let next = with_identity(&base, channel, address).unwrap_or(base);
        if in_file != Some(&next) {
            update_env_file(file, &[(env_name, Some(next))])?;
        }
    }
    if let Some(next) = with_identity(&process_value, channel, address) {
        std::env::set_var(env_name, next);
    }
    Ok(())
}

/// Remove the entry exactly equal to `identity` from `env_name` in both live
/// env files and the process env. `Ok(false)` when no source held it.
pub fn remove_env_identity(
    paths: &RuntimeSettingsPaths,
    env_name: &str,
    identity: &str,
) -> std::io::Result<bool> {
    let mut removed = false;
    for file in env_files(paths) {
        let values = read_env_file_values(file);
        if let Some(next) = values
            .get(env_name)
            .and_then(|existing| without_identity(existing, identity))
        {
            let value = (!next.is_empty()).then_some(next);
            update_env_file(file, &[(env_name, value)])?;
            removed = true;
        }
    }
    if let Some(next) = std::env::var(env_name)
        .ok()
        .and_then(|existing| without_identity(&existing, identity))
    {
        if next.is_empty() {
            std::env::remove_var(env_name);
        } else {
            std::env::set_var(env_name, next);
        }
        removed = true;
    }
    Ok(removed)
}

/// One `/claim` from a chat, with only server-derived facts.
#[derive(Debug, Clone, Copy)]
pub struct ClaimRequest<'a> {
    pub channel_type: &'a str,
    pub address: &'a str,
    /// [`envoy::channel_is_verified`], never the caller's claim.
    pub verified: bool,
    pub origin_channel_type: &'a str,
    pub origin_address: Option<&'a str>,
    pub code: Option<&'a str>,
}

/// What the chat is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimReply {
    Claimed,
    AlreadyOwner,
    InvalidCode,
    RateLimited,
    MissingCode,
    UnverifiedChannel,
    NotThisChat,
    NoIdentityVariable,
    WebChat,
    SaveFailed,
}

impl ClaimReply {
    /// The reply text. Never contains the code.
    pub fn text(self, channel_type: &str) -> String {
        let what = match channel_type.trim().to_ascii_lowercase().as_str() {
            "kapso" | "whatsapp" => "WhatsApp number",
            "telegram" => "Telegram chat",
            "sms" => "phone number",
            "imessage" => "iMessage address",
            _ => "address",
        };
        match self {
            Self::Claimed | Self::AlreadyOwner => format!(
                "Done — this {what} is now your owner identity. Messages from it reach \
                 Magician directly; /god turns on god mode for a chat."
            ),
            Self::InvalidCode => "That code isn't valid or has expired.".to_string(),
            Self::RateLimited => {
                "Too many wrong codes. Wait a few minutes, then get a new code from Magician."
                    .to_string()
            },
            Self::MissingCode => "Send /claim followed by the code from Magician \
                                  (Settings → Owner identities)."
                .to_string(),
            Self::UnverifiedChannel => "Ownership can't be claimed from this channel.".to_string(),
            Self::NotThisChat => "Ownership can't be claimed from this chat.".to_string(),
            Self::NoIdentityVariable => format!(
                "Ownership can't be claimed from this channel yet: Magician has no owner \
                 identity variable for `{}` (envoy.owner_identity_envs). Set one, or add \
                 the address to envoy.owner_identities by hand.",
                channel_type.trim()
            ),
            Self::WebChat => "Send /claim with your code from the WhatsApp or Telegram chat \
                              you want Magician to recognise as you."
                .to_string(),
            Self::SaveFailed => "Couldn't save that. Try the code again in a moment.".to_string(),
        }
    }
}

/// Decide and carry out one `/claim`. `persist(env_name, channel, address)`
/// appends the identity; the process path passes [`persist_env_identity`].
/// The code is spent only when the identity was saved.
pub fn handle_claim(
    cfg: &EnvoyConfig,
    request: &ClaimRequest<'_>,
    registry: &Mutex<ClaimRegistry>,
    now_ms: i64,
    persist: impl FnOnce(&str, &str, &str) -> std::io::Result<()>,
) -> ClaimReply {
    let channel = request.channel_type.trim().to_ascii_lowercase();
    let address = request.address.trim();
    if channel.is_empty() || channel == "web" {
        return ClaimReply::WebChat;
    }
    let env_name = match claim_channel(cfg, &channel) {
        _ if !request.verified => return ClaimReply::UnverifiedChannel,
        ClaimChannel::Unverified => return ClaimReply::UnverifiedChannel,
        ClaimChannel::NoIdentityVariable => return ClaimReply::NoIdentityVariable,
        ClaimChannel::Claimable { env_name } => env_name,
    };
    let same_chat = !address.is_empty()
        && request
            .origin_channel_type
            .trim()
            .eq_ignore_ascii_case(&channel)
        && request
            .origin_address
            .is_some_and(|origin| owner_identity_matches(&channel, origin, address));
    if !same_chat {
        return ClaimReply::NotThisChat;
    }
    if cfg.has_owner_identity(&channel, address) {
        return ClaimReply::AlreadyOwner;
    }
    let Some(code) = request.code.filter(|code| !code.trim().is_empty()) else {
        return ClaimReply::MissingCode;
    };
    let redeemed = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .redeem(&channel, address, code, now_ms);
    let claim = match redeemed {
        RedeemOutcome::Redeemed(claim) => claim,
        RedeemOutcome::Invalid => return ClaimReply::InvalidCode,
        RedeemOutcome::RateLimited => return ClaimReply::RateLimited,
    };
    match persist(&env_name, &channel, address) {
        Ok(()) => ClaimReply::Claimed,
        Err(error) => {
            tracing::error!(%error, channel = %channel, "[OWNER-CLAIM] could not save the claimed owner identity");
            registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .restore(claim);
            ClaimReply::SaveFailed
        },
    }
}

/// [`handle_claim`] against the process registry, the live env files and the
/// process env.
pub fn handle_claim_live(cfg: &EnvoyConfig, request: &ClaimRequest<'_>) -> ClaimReply {
    let paths = crate::magician_v2::runtime_settings::runtime_settings_paths();
    handle_claim(
        cfg,
        request,
        registry(),
        now_ms(),
        |env_name, channel, address| persist_env_identity(&paths, env_name, channel, address),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn cfg(env_name: &str) -> EnvoyConfig {
        let mut envs = HashMap::new();
        envs.insert("kapso".to_string(), env_name.to_string());
        envs.insert("telegram".to_string(), format!("{env_name}_TG"));
        envs.insert("agentmail".to_string(), format!("{env_name}_MAIL"));
        EnvoyConfig {
            owner_identity_envs: envs,
            ..EnvoyConfig::default()
        }
    }

    fn request<'a>(channel: &'a str, address: &'a str, code: Option<&'a str>) -> ClaimRequest<'a> {
        ClaimRequest {
            channel_type: channel,
            address,
            verified: true,
            origin_channel_type: channel,
            origin_address: Some(address),
            code,
        }
    }

    #[test]
    fn the_command_parses_in_every_form_and_nothing_else_does() {
        for (text, code) in [
            ("/claim ABCD2345", Some("ABCD2345")),
            ("  /CLAIM abcd-2345 ", Some("ABCD2345")),
            ("/claim abcd 2345", Some("ABCD2345")),
            ("/claim@MagicianBot ABCD2345", Some("ABCD2345")),
            ("/magic /claim ABCD2345", Some("ABCD2345")),
            ("@Magic /claim ABCD2345", Some("ABCD2345")),
            ("/claim", None),
        ] {
            assert_eq!(
                ClaimCommand::parse(text),
                Some(ClaimCommand {
                    code: code.map(str::to_string)
                }),
                "{text}"
            );
        }
        for text in [
            "",
            "claim ABCD2345",
            "/claims ABCD2345",
            "please /claim X",
            "/god",
            "/magicclaim X",
        ] {
            assert_eq!(ClaimCommand::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn minted_codes_are_unambiguous_and_redeem_once() {
        let mut registry = ClaimRegistry::default();
        let code = registry.mint(None, NOW);
        assert_eq!(code.code.len(), CLAIM_CODE_LEN);
        assert!(code.code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(code.expires_at_ms, NOW + CLAIM_CODE_TTL_MS);
        let lower = code.code.to_ascii_lowercase();
        assert_eq!(
            registry.redeem("kapso", "15551234567", &lower, NOW + 1),
            RedeemOutcome::Redeemed(code.clone())
        );
        assert_eq!(
            registry.redeem("kapso", "15551234567", &code.code, NOW + 2),
            RedeemOutcome::Invalid
        );
    }

    #[test]
    fn an_expired_or_other_channel_code_is_invalid() {
        let mut registry = ClaimRegistry::default();
        registry.mint_code("ABCD2345", None, NOW);
        assert_eq!(
            registry.redeem("kapso", "1555", "ABCD2345", NOW + CLAIM_CODE_TTL_MS),
            RedeemOutcome::Invalid
        );
        registry.mint_code("WXYZ6789", Some("Telegram"), NOW);
        assert_eq!(
            registry.redeem("kapso", "1555", "WXYZ6789", NOW + 1),
            RedeemOutcome::Invalid
        );
        assert!(matches!(
            registry.redeem("telegram", "42", "WXYZ6789", NOW + 2),
            RedeemOutcome::Redeemed(_)
        ));
    }

    #[test]
    fn a_sender_is_refused_after_five_wrong_codes_even_with_the_right_one() {
        let mut registry = ClaimRegistry::default();
        registry.mint_code("ABCD2345", None, NOW);
        for attempt in 0..MAX_FAILED_ATTEMPTS as i64 {
            assert_eq!(
                registry.redeem("kapso", "+1 555 123 4567", "WRONG000", NOW + attempt),
                RedeemOutcome::Invalid
            );
        }
        // Same number written differently is the same sender.
        assert_eq!(
            registry.redeem("kapso", "15551234567", "ABCD2345", NOW + 10),
            RedeemOutcome::RateLimited
        );
        // Another sender is unaffected.
        assert!(matches!(
            registry.redeem("kapso", "15550000000", "ABCD2345", NOW + 11),
            RedeemOutcome::Redeemed(_)
        ));
        // The window passes.
        registry.mint_code("EFGH2345", None, NOW + FAILED_ATTEMPT_WINDOW_MS);
        assert!(matches!(
            registry.redeem(
                "kapso",
                "15551234567",
                "EFGH2345",
                NOW + FAILED_ATTEMPT_WINDOW_MS + 10
            ),
            RedeemOutcome::Redeemed(_)
        ));
    }

    #[test]
    fn many_failures_across_senders_burn_every_live_code() {
        let mut registry = ClaimRegistry::default();
        registry.mint_code("ABCD2345", None, NOW);
        for sender in 0..MAX_GLOBAL_FAILURES {
            registry.redeem("telegram", &sender.to_string(), "WRONG000", NOW + 1);
        }
        assert_eq!(registry.live_codes(NOW + 2), 0);
    }

    #[test]
    fn minting_keeps_a_bounded_number_of_live_codes() {
        let mut registry = ClaimRegistry::default();
        for index in 0..(MAX_LIVE_CODES + 3) {
            registry.mint(None, NOW + index as i64);
        }
        assert_eq!(registry.live_codes(NOW + 10), MAX_LIVE_CODES);
    }

    #[test]
    fn constant_time_eq_is_plain_equality() {
        assert!(constant_time_eq(b"ABCD2345", b"ABCD2345"));
        assert!(!constant_time_eq(b"ABCD2345", b"ABCD2346"));
        assert!(!constant_time_eq(b"ABCD2345", b"ABCD234"));
    }

    #[test]
    fn appending_keeps_hand_set_entries_as_written_and_dedups() {
        assert_eq!(
            with_identity("", "kapso", "919876543210").as_deref(),
            Some("919876543210")
        );
        assert_eq!(
            with_identity("+44 7700 900123; 15551234567\n", "kapso", "919876543210").as_deref(),
            Some("+44 7700 900123; 15551234567,919876543210")
        );
        assert_eq!(
            with_identity("+91 98765 43210", "kapso", "919876543210"),
            None
        );
        assert_eq!(with_identity("12345", "telegram", "12345"), None);
        assert_eq!(without_identity("a,b;c", "b").as_deref(), Some("a,c"));
        assert_eq!(without_identity("a", "a").as_deref(), Some(""));
        assert_eq!(without_identity("a,b", "z"), None);
    }

    #[test]
    fn only_verified_channels_with_an_identity_variable_are_claimable() {
        let cfg = cfg("X");
        assert_eq!(
            claim_channel(&cfg, "Kapso"),
            ClaimChannel::Claimable {
                env_name: "X".to_string()
            }
        );
        assert_eq!(claim_channel(&cfg, "agentmail"), ClaimChannel::Unverified);
        assert_eq!(claim_channel(&cfg, "web"), ClaimChannel::Unverified);
        assert_eq!(
            claim_channel(&cfg, "whatsapp"),
            ClaimChannel::NoIdentityVariable
        );
        assert_eq!(
            claimable_channels(&cfg),
            vec!["kapso".to_string(), "telegram".to_string()]
        );
    }

    #[test]
    fn a_claim_appends_the_verified_sender_and_spends_the_code() {
        let env_name = "MAGICIAN_TEST_OWNER_CLAIM_APPEND";
        std::env::set_var(env_name, "+44 7700 900123");
        let cfg = cfg(env_name);
        let registry = Mutex::new(ClaimRegistry::default());
        registry.lock().unwrap().mint_code("ABCD2345", None, NOW);

        let reply = handle_claim(
            &cfg,
            &request("kapso", "919876543210", Some("abcd2345")),
            &registry,
            NOW + 1,
            |env, channel, address| {
                let existing = std::env::var(env).unwrap_or_default();
                if let Some(next) = with_identity(&existing, channel, address) {
                    std::env::set_var(env, next);
                }
                Ok(())
            },
        );
        assert_eq!(reply, ClaimReply::Claimed);
        // The hand-set identity still matches after the claim appended another.
        assert!(cfg.has_owner_identity("kapso", "447700900123"));
        assert!(cfg.has_owner_identity("kapso", "919876543210"));
        assert!(envoy::is_owner(&cfg, "kapso", "919876543210", true));
        assert_eq!(registry.lock().unwrap().live_codes(NOW + 2), 0);

        // Claiming again is an idempotent success that needs no code.
        let again = handle_claim(
            &cfg,
            &request("kapso", "+91 98765 43210", None),
            &registry,
            NOW + 3,
            |_, _, _| panic!("already the owner: nothing to save"),
        );
        assert_eq!(again, ClaimReply::AlreadyOwner);
        std::env::remove_var(env_name);
    }

    #[test]
    fn a_hand_set_config_identity_still_matches_after_a_claim() {
        let env_name = "MAGICIAN_TEST_OWNER_CLAIM_CONFIG_MAP";
        std::env::remove_var(env_name);
        let mut cfg = cfg(env_name);
        cfg.owner_identities
            .insert("kapso".to_string(), vec!["+1 555 123 4567".to_string()]);
        let registry = Mutex::new(ClaimRegistry::default());
        registry.lock().unwrap().mint_code("ABCD2345", None, NOW);
        let reply = handle_claim(
            &cfg,
            &request("kapso", "919876543210", Some("ABCD2345")),
            &registry,
            NOW + 1,
            |env, channel, address| {
                std::env::set_var(env, with_identity("", channel, address).unwrap());
                Ok(())
            },
        );
        assert_eq!(reply, ClaimReply::Claimed);
        assert!(cfg.has_owner_identity("kapso", "15551234567"));
        assert!(cfg.has_owner_identity("kapso", "919876543210"));
        std::env::remove_var(env_name);
    }

    #[test]
    fn refusals_never_touch_the_code_or_the_identity() {
        let cfg = cfg("MAGICIAN_TEST_OWNER_CLAIM_REFUSALS");
        let registry = Mutex::new(ClaimRegistry::default());
        registry.lock().unwrap().mint_code("ABCD2345", None, NOW);
        let never = |_: &str, _: &str, _: &str| -> std::io::Result<()> { panic!("must not save") };

        let mut unverified = request("kapso", "15551234567", Some("ABCD2345"));
        unverified.verified = false;
        assert_eq!(
            handle_claim(&cfg, &unverified, &registry, NOW, never),
            ClaimReply::UnverifiedChannel
        );
        assert_eq!(
            handle_claim(
                &cfg,
                &request("agentmail", "me@example.com", Some("ABCD2345")),
                &registry,
                NOW,
                never
            ),
            ClaimReply::UnverifiedChannel
        );
        assert_eq!(
            handle_claim(
                &cfg,
                &request("web", "", Some("ABCD2345")),
                &registry,
                NOW,
                never
            ),
            ClaimReply::WebChat
        );
        assert_eq!(
            handle_claim(
                &cfg,
                &request("whatsapp", "1555", Some("ABCD2345")),
                &registry,
                NOW,
                never
            ),
            ClaimReply::NoIdentityVariable
        );
        let other_chat = ClaimRequest {
            origin_address: Some("19998887777"),
            ..request("kapso", "15551234567", Some("ABCD2345"))
        };
        assert_eq!(
            handle_claim(&cfg, &other_chat, &registry, NOW, never),
            ClaimReply::NotThisChat
        );
        assert_eq!(
            handle_claim(
                &cfg,
                &request("kapso", "15551234567", None),
                &registry,
                NOW,
                never
            ),
            ClaimReply::MissingCode
        );
        assert_eq!(
            handle_claim(
                &cfg,
                &request("kapso", "15551234567", Some("WRONG000")),
                &registry,
                NOW,
                never
            ),
            ClaimReply::InvalidCode
        );
        assert_eq!(registry.lock().unwrap().live_codes(NOW), 1);
    }

    #[test]
    fn a_failed_save_puts_the_code_back() {
        let cfg = cfg("MAGICIAN_TEST_OWNER_CLAIM_SAVE_FAIL");
        let registry = Mutex::new(ClaimRegistry::default());
        registry.lock().unwrap().mint_code("ABCD2345", None, NOW);
        let reply = handle_claim(
            &cfg,
            &request("telegram", "42", Some("ABCD2345")),
            &registry,
            NOW,
            |_, _, _| Err(std::io::Error::other("disk full")),
        );
        assert_eq!(reply, ClaimReply::SaveFailed);
        assert_eq!(registry.lock().unwrap().live_codes(NOW), 1);
    }

    #[test]
    fn replies_never_carry_the_code() {
        for reply in [
            ClaimReply::Claimed,
            ClaimReply::InvalidCode,
            ClaimReply::RateLimited,
            ClaimReply::MissingCode,
            ClaimReply::UnverifiedChannel,
            ClaimReply::NoIdentityVariable,
            ClaimReply::SaveFailed,
        ] {
            assert!(!reply.text("kapso").contains("ABCD2345"));
        }
        assert!(ClaimReply::Claimed
            .text("kapso")
            .contains("WhatsApp number"));
        assert!(ClaimReply::Claimed
            .text("telegram")
            .contains("Telegram chat"));
    }

    #[test]
    fn persisting_appends_only_the_one_key_in_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let dev = dir.path().join(".env.development");
        let prod = dir.path().join(".env");
        std::fs::write(
            &dev,
            "OTHER=keepme\nMAGICIAN_TEST_CLAIM_FILES=\"+44 7700 900123;15550001111\"\n",
        )
        .unwrap();
        std::fs::write(&prod, "# comment\nOTHER2=x\n").unwrap();
        let env_name = "MAGICIAN_TEST_CLAIM_FILES";
        std::env::set_var(env_name, "+44 7700 900123;15550001111");
        let paths = RuntimeSettingsPaths {
            runtime_root: dir.path().to_path_buf(),
            config_path: dir.path().join("magician-config.yaml"),
            env_target_path: dev.clone(),
            env_target_mode: "development",
            env_development_path: dev.clone(),
            env_path: prod.clone(),
        };
        persist_env_identity(&paths, env_name, "kapso", "919876543210").unwrap();
        let dev_text = std::fs::read_to_string(&dev).unwrap();
        assert!(dev_text.starts_with("OTHER=keepme\n"));
        assert_eq!(
            read_env_file_values(&dev).get(env_name).map(String::as_str),
            Some("+44 7700 900123;15550001111,919876543210")
        );
        assert_eq!(
            read_env_file_values(&prod)
                .get(env_name)
                .map(String::as_str),
            Some("+44 7700 900123;15550001111,919876543210")
        );
        assert!(std::fs::read_to_string(&prod)
            .unwrap()
            .starts_with("# comment\nOTHER2=x\n"));
        assert_eq!(
            std::env::var(env_name).unwrap(),
            "+44 7700 900123;15550001111,919876543210"
        );
        // Again is a no-op.
        persist_env_identity(&paths, env_name, "kapso", "+919876543210").unwrap();
        assert_eq!(
            std::env::var(env_name).unwrap(),
            "+44 7700 900123;15550001111,919876543210"
        );
        assert!(remove_env_identity(&paths, env_name, "919876543210").unwrap());
        assert_eq!(
            read_env_file_values(&dev).get(env_name).map(String::as_str),
            Some("+44 7700 900123,15550001111")
        );
        std::env::remove_var(env_name);
    }
}
