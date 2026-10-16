//! An external organisation, and the addresses through which we reach it.
//!
//! Every type here is deliberately domain-free. A counterparty is a supplier, a
//! customer, a candidate's employer, a regulator, a school — the machinery is
//! *"an organisation we are not, and the set of addresses that are it"*, and
//! nothing about that is specific to any one programme.
//!
//! # The three things this file is careful about
//!
//! 1. **Normalisation is the identity.** Two spellings of one address must fold
//!    to one string or the same organisation splits in two; two *different*
//!    addresses must never fold to one string or one organisation's authority
//!    lands on another. Every fold this module performs is written down in
//!    [`normalise_identity`], and the ones it refuses to perform are written
//!    down there too.
//! 2. **Provenance is not proof.** [`MintSource`] records *how we learned* an
//!    address. None of its variants imply verification —
//!    [`MintSource::implies_verified`] is `false` for every one of them, and it
//!    exists so that adding a variant makes somebody answer the question.
//! 3. **Verification has a root the caller cannot supply.** [`TrustedSignal`]
//!    delegates to [`crate::magician_v2::chat::envoy::channel_is_verified`]
//!    rather than re-deciding: a caller's own claim may de-escalate and may
//!    never raise, and a channel nobody has classified fails closed.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The separator that keeps a derived id's components from bleeding into each
/// other.
///
/// Any caller-supplied string that feeds a derivation is refused if it carries
/// this character — the same refusal `run_state::store::open` makes, for the
/// same reason: a component carrying the separator can fuse two different things
/// into one id, and for this module "two different things" means two different
/// organisations.
pub(crate) const FIELD_SEP: char = '\u{1f}';

/// A pointer to a counterparty.
///
/// Deliberately just the id. A ref that carried the display name would be a
/// snapshot that goes stale the moment somebody renames the organisation, and
/// callers would compare on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CounterpartyRef {
    pub counterparty_id: String,
}

impl CounterpartyRef {
    pub fn new(counterparty_id: impl Into<String>) -> Self {
        Self {
            counterparty_id: counterparty_id.into(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.counterparty_id
    }
}

/// What kind of address an identity is.
///
/// The kind is part of every derived id and every index key, so a handle and a
/// domain that happen to normalise to the same string are still two different
/// identities — the same discipline [`crate::magician_v2::audience::AudienceRef::as_key`]
/// uses to keep `engagement:acme` and `account:acme` apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Email,
    Phone,
    /// A platform-qualified account name, `platform:handle`. See
    /// [`normalise_identity`] for why the qualifier is mandatory.
    Handle,
    Domain,
}

impl IdentityKind {
    /// Every arm, in declaration order.
    ///
    /// [`Self::parse`] reads this list, so a variant missing from it cannot be
    /// named by a caller that only has a string — a route's query parameter, a
    /// request body, a stored adapter setting. That is the fail-closed
    /// direction, and the test that walks it is exhaustive on the enum so a new
    /// variant cannot be added without somebody deciding whether a caller may
    /// name it.
    pub const ALL: [Self; 4] = [Self::Email, Self::Phone, Self::Handle, Self::Domain];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Handle => "handle",
            Self::Domain => "domain",
        }
    }

    /// The kind a caller named, or `None`.
    ///
    /// **`None` is "that is not a kind", never a default.** A surface that
    /// silently defaulted an unrecognised word to one of these would file an
    /// address under a kind nobody chose — and the kind is part of every derived
    /// id, so the address would then be invisible to every lookup that used the
    /// right one, or worse, land on the row a different kind already holds.
    ///
    /// Case and surrounding whitespace are folded, because a kind typed on two
    /// different days is one kind. Nothing else is: no plural form, no synonym
    /// table, no prefix match — the same refusal
    /// [`normalise_identity`] makes about near-miss addresses.
    pub fn parse(label: &str) -> Option<Self> {
        let wanted = label.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == wanted.as_str())
    }
}

/// Fold an address to the single string identity comparison happens on.
///
/// # What this folds, and what it refuses to fold
///
/// Folding too little splits one organisation in two, which is annoying.
/// Folding too much merges two organisations into one, which hands one
/// counterparty's authority to another. So every fold below is deliberate and
/// every guess is refused:
///
/// - **Email** — case is folded across the *whole* address, local part
///   included. RFC 5321 permits a case-sensitive local part, so this is the one
///   place two genuinely distinct mailboxes could fold into one. It is folded
///   anyway because every provider we can actually reach treats the local part
///   case-insensitively, and not folding it would split a single person across
///   every capitalisation their mail client ever emitted. Nothing else about an
///   email is touched: dots are not stripped, `+tags` are not stripped, and
///   there is no per-provider table — a table of provider quirks is a heuristic
///   that rots silently into a merge.
/// - **Phone** — formatting characters are removed and a leading `+` is kept.
///   A country code is **never** inferred. A bare national number and its E.164
///   form are two different identities and will not resolve to each other. That
///   is the fail-closed direction: guessing `+1` because the owner happens to be
///   in North America would route a stranger's number to a real counterparty.
/// - **Handle** — must be `platform:handle`. An unqualified `@acme` is not an
///   address: it is a different organisation on every platform it exists on, and
///   resolving one platform's `@acme` to another's is exactly the authority
///   hand-off this module exists to prevent. The platform and the handle are
///   both lower-cased and one leading `@` is dropped.
/// - **Domain** — lower-cased, one trailing dot removed. `www.` is **not**
///   stripped and no public-suffix logic is applied: both are guesses, and a
///   domain here is only ever a *candidate* signal anyway (see
///   `candidates_by_domain`). A single-label host is refused, because `com`
///   would match half the world in a candidate list.
///
/// Any value carrying [`FIELD_SEP`] or any other control character is refused
/// outright: the first would fuse id components, the second would shear a log
/// line.
pub fn normalise_identity(kind: IdentityKind, value: &str) -> Result<String> {
    let raw = value.trim();
    if raw.is_empty() {
        anyhow::bail!(
            "an identity with no value is not an address; a blank identity would bind every \
             blank lookup to whichever counterparty claimed it first"
        );
    }
    if raw.contains(FIELD_SEP) {
        anyhow::bail!(
            "an identity value must not contain U+001F: it is the separator that keeps a \
             derived id's components from bleeding into each other, and a value carrying it \
             could fuse two organisations' addresses into one identity"
        );
    }
    if raw.chars().any(char::is_control) {
        anyhow::bail!(
            "an identity value must not contain control characters; a newline would shear the \
             append-only log line this identity is recorded on"
        );
    }

    match kind {
        IdentityKind::Email => normalise_email(raw),
        IdentityKind::Phone => normalise_phone(raw),
        IdentityKind::Handle => normalise_handle(raw),
        IdentityKind::Domain => normalise_domain(raw),
    }
}

fn normalise_email(raw: &str) -> Result<String> {
    let Some((local, domain)) = raw.split_once('@') else {
        anyhow::bail!("`{raw}` is not an email address: it has no `@`");
    };
    if local.is_empty() {
        anyhow::bail!("`{raw}` is not an email address: it has no mailbox before the `@`");
    }
    if domain.contains('@') {
        anyhow::bail!("`{raw}` is not an email address: it has more than one `@`");
    }
    let domain = normalise_domain(domain)?;
    Ok(format!("{}@{domain}", local.to_lowercase()))
}

fn normalise_phone(raw: &str) -> Result<String> {
    let international = raw.starts_with('+');
    let digits: String = raw
        .chars()
        .skip(usize::from(international))
        .filter(|character| !matches!(character, ' ' | '-' | '(' | ')' | '.' | '\u{a0}'))
        .collect();
    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        anyhow::bail!(
            "`{raw}` is not a phone number: after removing formatting it must be digits, with \
             at most a leading `+`. No country code is ever inferred, so a number must arrive \
             in the form it will always be compared in"
        );
    }
    Ok(if international {
        format!("+{digits}")
    } else {
        digits
    })
}

fn normalise_handle(raw: &str) -> Result<String> {
    let Some((platform, handle)) = raw.split_once(':') else {
        anyhow::bail!(
            "`{raw}` is not a usable handle: a handle must be platform-qualified as \
             `platform:handle`. The same name on two platforms is two different \
             organisations, and resolving one to the other would hand one counterparty's \
             authority to another"
        );
    };
    let handle = handle.trim().trim_start_matches('@');
    if platform.trim().is_empty() || handle.is_empty() {
        anyhow::bail!(
            "`{raw}` is not a usable handle: both the platform and the name are required"
        );
    }
    if handle.contains(':') {
        anyhow::bail!(
            "`{raw}` is not a usable handle: more than one `:` leaves the platform qualifier \
             ambiguous"
        );
    }
    Ok(format!(
        "{}:{}",
        platform.trim().to_lowercase(),
        handle.to_lowercase()
    ))
}

fn normalise_domain(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('.').to_lowercase();
    if trimmed.is_empty() {
        anyhow::bail!("`{raw}` is not a domain: it is empty");
    }
    if trimmed.chars().any(|character| {
        matches!(character, '/' | '@' | ':' | '?' | '#') || character.is_whitespace()
    }) {
        anyhow::bail!(
            "`{raw}` is not a bare domain: a scheme, port, path or address wrapper must be \
             removed by the caller rather than guessed at here"
        );
    }
    if !trimmed.contains('.') {
        anyhow::bail!(
            "`{raw}` is a single-label host, not an organisation's domain; accepting one would \
             let a label like `com` gather every counterparty into one candidate list"
        );
    }
    if trimmed.starts_with('.') || trimmed.contains("..") {
        anyhow::bail!("`{raw}` is not a domain: it has an empty label");
    }
    Ok(trimmed)
}

/// The domain part of a normalised email, for candidate review only.
///
/// Returns `None` for anything that is not a normalised email. Used exclusively
/// by `candidates_by_domain`; resolution never touches it.
pub fn email_domain_of(normalised_email: &str) -> Option<&str> {
    normalised_email.split_once('@').map(|(_, domain)| domain)
}

/// Where a counterparty has got to, as the owner names it.
///
/// **An open set on purpose.** A closed enum here would be a sales funnel
/// wearing a generic name, and the first team to use this primitive for support
/// tickets or hiring would have to fork it. The only things a stage may not be
/// are blank or separator-carrying.
///
/// A stage is *not* a terminal state and *not* ordered: this module records the
/// label the owner last set and takes no view on whether `churned` comes after
/// `active`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Stage(String);

impl Stage {
    pub fn new(label: &str) -> Result<Self> {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            anyhow::bail!("a stage with no label says nothing; omit the stage instead");
        }
        if trimmed.contains(FIELD_SEP) {
            anyhow::bail!(
                "a stage label must not contain U+001F: it is the separator that keeps a \
                 derived id's components apart"
            );
        }
        if trimmed.chars().any(char::is_control) {
            anyhow::bail!("a stage label must not contain control characters");
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The label as the owner wrote it — this is what gets displayed.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The comparison form. Case is not a distinction an owner means to draw
    /// between two stages they typed on different days.
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }
}

impl TryFrom<String> for Stage {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(&value)
    }
}

impl From<Stage> for String {
    fn from(stage: Stage) -> Self {
        stage.0
    }
}

/// **How we learned an address.** The point of this module.
///
/// A plain string label cannot tell you whether an address came from the owner's
/// own mouth or from a scraper, and the two are not interchangeable when the
/// next step is sending something to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MintSource {
    /// The owner told us. Trusted as a *statement*, still not verified.
    OwnerStated,
    /// It arrived on an inbound message. Seeing an address is not proof the
    /// sender owns it — SMTP does not authenticate `From:`.
    ObservedOnInbound,
    /// We worked it out — a pattern, a directory, a page. **A guess.** It is
    /// never verified, and it may never be promoted on the strength of the
    /// research that produced it.
    ResearchInferred,
    /// Somebody we already know vouched for it. The voucher is recorded in
    /// `introduced_by`; an introduction with no introducer is not an
    /// introduction.
    Introduced,
}

impl MintSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnerStated => "owner_stated",
            Self::ObservedOnInbound => "observed_on_inbound",
            Self::ResearchInferred => "research_inferred",
            Self::Introduced => "introduced",
        }
    }

    /// Whether this address was **guessed** rather than told to us, seen, or
    /// vouched for.
    pub fn is_inferred(self) -> bool {
        matches!(self, Self::ResearchInferred)
    }

    /// Whether provenance alone makes an identity verified. **Never.**
    ///
    /// This looks like a constant because it is one, and that is the point:
    /// verification has exactly one root ([`TrustedSignal`]), and a new
    /// `MintSource` variant has to come through here and be told no rather than
    /// quietly inheriting a default of "well, the owner said so".
    pub fn implies_verified(self) -> bool {
        false
    }

    /// Whether this source requires an introducer to be named.
    pub fn requires_introducer(self) -> bool {
        matches!(self, Self::Introduced)
    }
}

/// How an identity came to be recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Minting {
    pub source: MintSource,
    /// What to read to check the claim — a message ref, a transcript ref, a
    /// research note ref. Required: provenance with no evidence is a rumour with
    /// a category label.
    pub evidence_ref: String,
    /// Who or what recorded it.
    pub recorded_by: String,
    pub at: DateTime<Utc>,
}

/// A verification signal, and whether it is one the caller could have forged.
///
/// The shape follows [`crate::magician_v2::chat::envoy::channel_is_verified`]
/// exactly, and delegates to it rather than re-deciding, so the two cannot
/// drift. Two inputs the caller does not control decide it:
///
/// - `request_authenticated` — whether *our* outer boundary proved the request.
/// - the channel's own transport — a phone network establishes who sent a
///   WhatsApp message; SMTP does not establish a `From:` header, and a channel
///   nobody has classified fails closed.
///
/// `caller_claim` may only **de-escalate**: `Some(false)` is honoured because an
/// adapter sees things the server cannot, and `Some(true)` never raises. A
/// verification signal the subject supplies about itself is not a verification
/// signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedSignal {
    pub channel: String,
    pub request_authenticated: bool,
    pub caller_claim: Option<bool>,
    /// What proves it — the delivery receipt, the confirmed click, the signed
    /// callback.
    pub evidence_ref: String,
}

impl TrustedSignal {
    pub fn new(channel: impl Into<String>, evidence_ref: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            request_authenticated: false,
            caller_claim: None,
            evidence_ref: evidence_ref.into(),
        }
    }

    pub fn authenticated(mut self, request_authenticated: bool) -> Self {
        self.request_authenticated = request_authenticated;
        self
    }

    pub fn with_caller_claim(mut self, caller_claim: Option<bool>) -> Self {
        self.caller_claim = caller_claim;
        self
    }

    /// Whether this signal is one a server would stand behind.
    ///
    /// Evidence is part of it: a trusted transport with nothing to point at
    /// leaves an owner unable to check the one decision that turns an address
    /// into a reachable identity.
    pub fn is_trusted(&self) -> bool {
        !self.evidence_ref.trim().is_empty()
            && crate::magician_v2::chat::envoy::channel_is_verified(
                &self.channel,
                self.request_authenticated,
                self.caller_claim,
            )
    }
}

/// Whether an address has been proved to reach who we think it reaches.
///
/// `Unverified` is the default and the only state any automatic path can
/// produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verification", rename_all = "snake_case")]
pub enum Verification {
    Unverified,
    Verified {
        /// The channel whose transport carried the signal.
        channel: String,
        /// What proves it.
        evidence_ref: String,
        /// The owner decision. A promotion with no named decider is not a
        /// decision.
        decided_by: String,
        at: DateTime<Utc>,
    },
}

impl Default for Verification {
    fn default() -> Self {
        Self::Unverified
    }
}

impl Verification {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

/// An address we can reach a counterparty through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub identity_id: String,
    /// The counterparty as recorded at mint time. Reads follow merge edges from
    /// here rather than rewriting it, because the log is append-only and *"this
    /// address was filed under Acme Ltd before we knew Acme Ltd and Acme GmbH
    /// were one company"* is a fact worth keeping.
    pub counterparty_id: String,
    pub kind: IdentityKind,
    /// Exactly as supplied, for display and for an owner to read back.
    pub value: String,
    /// The comparison form. **This is the identity** — see [`normalise_identity`].
    pub normalised: String,
    pub minted_by: Minting,
    /// The identity that vouched, when this was an introduction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introduced_by: Option<String>,
    #[serde(default)]
    pub verification: Verification,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl Identity {
    pub fn is_verified(&self) -> bool {
        self.verification.is_verified()
    }

    /// Whether this address was guessed. A caller about to send something
    /// somewhere should be able to ask this in one call.
    pub fn is_inferred(&self) -> bool {
        self.minted_by.source.is_inferred()
    }
}

/// An external organisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counterparty {
    pub counterparty_id: String,
    pub display_name: String,
    /// Normalised if present. A hint for candidate review, never a resolution
    /// key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    /// Set once, by a merge. **Terminal**: a merged record never resurrects and
    /// never merges again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<String>,
}

impl Counterparty {
    /// Whether this record is still the organisation's own row, rather than a
    /// name that has since been folded into another.
    pub fn is_live(&self) -> bool {
        self.merged_into.is_none()
    }

    pub fn as_ref(&self) -> CounterpartyRef {
        CounterpartyRef::new(self.counterparty_id.clone())
    }
}

/// What a caller supplies to create a counterparty.
#[derive(Debug, Clone)]
pub struct CreateCounterparty {
    pub display_name: String,
    pub domain: Option<String>,
    /// The stage it starts in. **Initial only** — a replay that carries a
    /// different stage is an error naming `set_stage`, not a silent no-op.
    pub stage: Option<Stage>,
    pub created_by: String,
}

/// What a caller supplies to record an address.
#[derive(Debug, Clone)]
pub struct AddIdentity {
    pub counterparty_id: String,
    pub kind: IdentityKind,
    pub value: String,
    pub source: MintSource,
    pub evidence_ref: String,
    pub recorded_by: String,
    /// Required when `source` is [`MintSource::Introduced`], refused otherwise:
    /// an introduction is a provenance, not a decoration.
    pub introduced_by: Option<String>,
}

/// The owner decision that turns an address into a verified one.
#[derive(Debug, Clone)]
pub struct Promotion {
    /// The person deciding. Required and non-blank — an unnamed promotion is how
    /// an automatic path would grant itself the one control this module has.
    pub decided_by: String,
    /// What the owner looked at. For a [`MintSource::ResearchInferred`] identity
    /// this must **differ** from the evidence that minted it: a research note
    /// cannot be its own proof.
    pub evidence_ref: String,
    pub signal: TrustedSignal,
}

/// The owner decision that folds two records into one.
#[derive(Debug, Clone)]
pub struct MergeDecision {
    pub decided_by: String,
    pub evidence_ref: String,
}

/// Counts for one counterparty.
///
/// **Counts, never rates or scores.** "3 of 5 identities verified" is a fact an
/// owner can act on; "identity confidence 0.6" is a number that gets compared
/// against a threshold nobody chose and then used as if it were certainty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterpartySummary {
    pub counterparty_id: String,
    pub display_name: String,
    pub stage: Option<Stage>,
    /// Every address on file, whatever its provenance.
    pub identity_count: usize,
    /// Addresses a server-trusted signal has proved.
    pub verified_identity_count: usize,
    /// Addresses that were guessed. Called out separately because an owner
    /// reviewing a record should see at a glance how much of it is inference.
    pub inferred_identity_count: usize,
    /// Records folded into this one by a merge.
    pub merged_in_count: usize,
}
