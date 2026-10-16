//! Capability that flows from the work, not only from the actor — §2.1.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` §2.1.
//! Doc: `docs/components/magician/work-context.md`.
//!
//! # The thing that was blocking composition
//!
//! Capability is granted **per-agent, statically**, and there is no
//! skill-discovery tool — an agent cannot find a capability it was not given. So
//! *"compose for a specific purpose"* meant editing agent YAML, which does not
//! scale to purposes nobody has thought of yet.
//!
//! # The generalisation
//!
//! The engagements plan defines `effective = agent ∩ engagement ∩ channel`.
//! Widen the middle term from *engagement* to **work context** — a program or an
//! engagement — and grants flow from the work as well as from the actor.
//!
//! # The rule that makes it safe
//!
//! **A work context identifies capabilities; it never attaches them.**
//! Intersection can only *narrow*. A capability the work names and the agent
//! does not hold is not granted by naming it — it becomes a pointer to delegate
//! to someone who does, and the work context travels with the delegation.
//!
//! `resolve` therefore has exactly one arm that returns "usable", and it
//! requires the agent to already hold the capability. Everything else is either
//! a delegation pointer or nothing. That asymmetry is the entire safety
//! property, and there is a test that walks every combination to prove no other
//! path reaches usable.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// The field separator derived ids are joined with.
///
/// A work id that carried it could address a record it did not name, so it is
/// refused at the boundary rather than escaped downstream.
const FIELD_SEP: char = '\u{1f}';

/// The work an agent is acting inside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum WorkContextKind {
    Program(String),
    Engagement(String),
}

impl WorkContextKind {
    pub fn as_key(&self) -> String {
        match self {
            Self::Program(id) => format!("program:{id}"),
            Self::Engagement(id) => format!("engagement:{id}"),
        }
    }

    /// The id, whichever kind of work this is.
    ///
    /// Exists so a consumer that must **guard** the id before deriving anything
    /// from it — refuse a blank one, refuse one carrying the field separator
    /// derived ids are joined with — does not have to match on the arms, and
    /// therefore does not have to be edited when a third kind of work is added.
    /// A guard written as a `match` is a guard that silently stops covering the
    /// next variant somebody adds.
    ///
    /// Not [`Self::as_key`]: that answer is prefixed, so it is never blank and a
    /// blank-id check against it would pass for `program:`.
    pub fn id(&self) -> &str {
        match self {
            Self::Program(id) | Self::Engagement(id) => id,
        }
    }

    /// The wire token for [`Self::Program`].
    pub const PROGRAM_TOKEN: &'static str = "program";
    /// The wire token for [`Self::Engagement`].
    pub const ENGAGEMENT_TOKEN: &'static str = "engagement";

    /// Every wire token, in declaration order.
    ///
    /// Exists so a consumer that must **enumerate over every kind of work** —
    /// a reverse index filed one directory per kind, a sweep that has to look
    /// under all of them — reads this list rather than writing its own literal.
    /// A hardcoded axis is how one kind of work becomes the only kind that can
    /// be found: the enumeration returns nothing for every other kind, and an
    /// index nobody wrote to reads exactly like a relationship with no acts.
    ///
    /// [`Self::from_token`] is checked against this list by test, so a variant
    /// missing from it cannot be named by a caller that only holds a string.
    pub const KIND_TOKENS: [&'static str; 2] = [Self::PROGRAM_TOKEN, Self::ENGAGEMENT_TOKEN];

    /// The wire token for this kind — the half of [`Self::as_key`] before the
    /// colon.
    ///
    /// Exists so a carrier can cross a crate boundary that cannot depend on
    /// this module (the storage trait in `runtime-core`) without that boundary
    /// learning what kinds of work exist.
    pub fn kind_token(&self) -> &'static str {
        match self {
            Self::Program(_) => Self::PROGRAM_TOKEN,
            Self::Engagement(_) => Self::ENGAGEMENT_TOKEN,
        }
    }

    /// Rebuild a kind from a wire token and an id, refusing anything else.
    ///
    /// **Fails closed on an unknown token.** A token this build does not know
    /// is a record written by something this build does not understand, and
    /// coercing it into the nearest known arm would silently re-scope the
    /// authority it carries. The id is guarded here too, so a value that
    /// travelled as a string cannot re-enter as an unguarded one.
    pub fn from_token(kind_token: &str, id: &str) -> Result<Self, String> {
        let token = kind_token.trim();
        // An if-chain rather than a `match` on the associated constants: the
        // tokens are the single source of truth for the axis a reverse index is
        // filed under, and a literal repeated here is how the two drift apart.
        let kind = if token == Self::PROGRAM_TOKEN {
            Self::Program(id.to_string())
        } else if token == Self::ENGAGEMENT_TOKEN {
            Self::Engagement(id.to_string())
        } else {
            return Err(format!(
                "unknown work kind `{token}`: refusing rather than guessing which kind of \
                 work granted this authority, because guessing wrong re-scopes it"
            ));
        };
        kind.guard_id()?;
        Ok(kind)
    }

    /// Refuse an id that cannot safely name a record.
    ///
    /// Reads [`Self::id`] rather than matching on the arms, so a third kind of
    /// work is guarded the day it is added rather than the day somebody
    /// notices it was not.
    pub fn guard_id(&self) -> Result<(), String> {
        let id = self.id();
        if id.trim().is_empty() {
            return Err(
                "a work context must name something: a blank id addresses every record \
                        and none of them"
                    .to_string(),
            );
        }
        if id.contains(FIELD_SEP) {
            return Err(
                "a work id must not contain U+001F: it is the separator that keeps a derived \
                 id's components from bleeding into each other"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// **The authority a durable execution record carries** — the generic carrier.
///
/// # What it replaced, and why
///
/// The durable record used to carry an OPC-specific engagement reference, so
/// the *generic* record named one flow's type and no other flow had a slot:
/// a support-triage or recruiting run would have had to fabricate an
/// engagement id to be confined at all, and a program-scoped root was refused
/// outright because the field could not hold it. This carries every arm of
/// [`WorkContextKind`], so a new kind of work needs a roster to validate it —
/// not an edit to the execution record.
///
/// # Non-forgeable in exactly the way the engagement reference was (§4.2c)
///
/// Two fields, and neither is caller-settable:
///
/// - a child inherits the whole value **verbatim** from the parent's durable
///   record — row 5: a child that could name its own work could grant itself
///   authority;
/// - a root gets it from the roster that owns the work, which supplies the
///   revision — naming a work context is never the same as being granted it;
/// - the revision pins *which state of the grant* the run was admitted under,
///   so a later revoke or narrow is detectable as staleness at dispatch
///   (row 9). It is a freshness marker, never the thing enforced: the ceiling
///   actually enforced is always read live.
///
/// Absent means **no authority**, never "unrestricted": the field is
/// `#[serde(default)]` on the record, and every consumer treats `None` as an
/// unbound run rather than an unchecked one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkAuthorityRef {
    /// Which work granted the authority.
    pub work: WorkContextKind,
    /// The revision of that work's grant this run was admitted under, as read
    /// from the roster that owns it at admission time.
    pub authority_revision: u64,
}

impl WorkAuthorityRef {
    /// Build a carrier, refusing an id that cannot safely name a record.
    ///
    /// Constructing through a guarded function rather than a struct literal is
    /// what keeps the U+001F rule from depending on each caller remembering it.
    pub fn new(work: WorkContextKind, authority_revision: u64) -> Result<Self, String> {
        work.guard_id()?;
        Ok(Self {
            work,
            authority_revision,
        })
    }

    /// `kind:id` — the reviewable name of the work this run is confined to.
    pub fn as_key(&self) -> String {
        self.work.as_key()
    }

    /// The id of the work, whichever kind it is.
    pub fn id(&self) -> &str {
        self.work.id()
    }

    /// The wire token of the work's kind.
    pub fn kind_token(&self) -> &'static str {
        self.work.kind_token()
    }
}

/// What a piece of work needs.
///
/// Authored by a human in a program, or derived from an engagement. It is a
/// **statement of need**, never a grant — see the module note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkContext {
    pub kind: WorkContextKind,
    /// Capabilities the work needs. Naming one does not confer it.
    pub needs_capabilities: Vec<String>,
    /// Procedure skills the work needs.
    pub needs_playbooks: Vec<String>,
}

impl WorkContext {
    pub fn new(kind: WorkContextKind) -> Self {
        Self {
            kind,
            needs_capabilities: Vec::new(),
            needs_playbooks: Vec::new(),
        }
    }

    pub fn needing(mut self, capabilities: &[&str]) -> Self {
        self.needs_capabilities
            .extend(capabilities.iter().map(|name| normalise(name)));
        self
    }

    pub fn with_playbooks(mut self, playbooks: &[&str]) -> Self {
        self.needs_playbooks
            .extend(playbooks.iter().map(|name| normalise(name)));
        self
    }

    fn names(&self, capability: &str) -> bool {
        let wanted = normalise(capability);
        self.needs_capabilities.iter().any(|held| *held == wanted)
            || self.needs_playbooks.iter().any(|held| *held == wanted)
    }
}

/// What an agent may do with one capability, inside one work context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "standing", rename_all = "snake_case")]
pub enum CapabilityStanding {
    /// The agent holds it **and** the work needs it. Usable, narrowed to this
    /// work. The only arm that permits anything.
    Usable,
    /// The work needs it and the agent does not hold it.
    ///
    /// **Not a grant.** The agent can see that the work needs this and that it
    /// cannot do it, which is what lets it delegate instead of failing silently
    /// or inventing an approach.
    RequiresDelegation,
    /// The agent holds it, but this work does not need it.
    ///
    /// Held capabilities are not revoked by a work context — this says only that
    /// using it here is outside what the work is for. A caller deciding what to
    /// *offer* an agent should exclude it; a caller deciding what to *forbid*
    /// should not, because the agent's own grant still stands outside this work.
    OutsideThisWork,
    /// Neither the agent nor the work has anything to do with it.
    Unavailable,
}

impl CapabilityStanding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Usable => "usable",
            Self::RequiresDelegation => "requires_delegation",
            Self::OutsideThisWork => "outside_this_work",
            Self::Unavailable => "unavailable",
        }
    }

    /// Whether the acting agent may perform this itself.
    ///
    /// True for exactly one variant. Intersection can only narrow, so no amount
    /// of naming a capability in a work context makes this true for an agent
    /// that does not hold it.
    pub fn permits_direct_use(&self) -> bool {
        matches!(self, Self::Usable)
    }

    /// Whether the agent should look for someone else to do it.
    pub fn should_delegate(&self) -> bool {
        matches!(self, Self::RequiresDelegation)
    }
}

/// Resolve one capability for one agent inside one work context.
///
/// `agent_capabilities` is the agent's **own** grant, resolved however the
/// caller already resolves it. Passing it in rather than reading it keeps this
/// free of the agent-definition layer, which is what lets the same function
/// serve a dispatch gate, a discovery surface and a test.
pub fn resolve(
    agent_capabilities: &[String],
    work: &WorkContext,
    capability: &str,
) -> CapabilityStanding {
    let wanted = normalise(capability);
    let held = agent_capabilities
        .iter()
        .any(|granted| normalise(granted) == wanted);
    let needed = work.names(&wanted);

    match (held, needed) {
        (true, true) => CapabilityStanding::Usable,
        (false, true) => CapabilityStanding::RequiresDelegation,
        (true, false) => CapabilityStanding::OutsideThisWork,
        (false, false) => CapabilityStanding::Unavailable,
    }
}

/// The capabilities an agent may actually use inside this work.
///
/// The intersection, and nothing else. Sorted and deduplicated so two callers
/// asking the same question get the same answer in the same order.
pub fn effective_capabilities(agent_capabilities: &[String], work: &WorkContext) -> Vec<String> {
    let held: BTreeSet<String> = agent_capabilities
        .iter()
        .map(|name| normalise(name))
        .collect();
    work.needs_capabilities
        .iter()
        .chain(work.needs_playbooks.iter())
        .map(|name| normalise(name))
        .filter(|name| held.contains(name))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What the agent needs but cannot do — the delegation list.
///
/// This is the discovery half of §2.1: *"the acting agent sees those capability
/// summaries"*. An agent that cannot see what the work needs cannot know to
/// delegate, and would instead improvise with what it has — which is how a
/// narrow grant produces a wrong answer rather than a handoff.
pub fn delegation_needs(agent_capabilities: &[String], work: &WorkContext) -> Vec<String> {
    let held: BTreeSet<String> = agent_capabilities
        .iter()
        .map(|name| normalise(name))
        .collect();
    work.needs_capabilities
        .iter()
        .chain(work.needs_playbooks.iter())
        .map(|name| normalise(name))
        .filter(|name| !held.contains(name))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What the work needs, as the acting agent should see it BEFORE it plans.
///
/// # Why this exists as well as the refusal
///
/// §2.1 step 2 is *"the acting agent sees those capability summaries"*, and the
/// only place that happened was step 3's refusal — the agent learned what the
/// work needed **after** reaching for something and being told no. That is the
/// wrong moment twice over: an agent that only discovers a need by tripping over
/// it has already committed to a plan built from what it happens to hold, and a
/// narrow grant that improvises produces a wrong answer instead of a handoff.
///
/// # It surfaces what the agent does NOT hold, deliberately
///
/// The delegation list is the interesting half. What the agent holds it can
/// already see in its own catalogue; what the work needs and the agent lacks is
/// invisible from every other surface, and it is exactly the thing that should
/// change the plan. Both are rendered, because a summary that named only the
/// gaps would read as a list of failures rather than as the shape of the work.
///
/// Returns an empty string when there is no work context or the work names
/// nothing — no header, no "(none)". A section that says nothing costs prompt
/// budget on every turn of every execution that has no work context, which is
/// most of them.
pub fn capability_summary(agent_capabilities: &[String], work: &WorkContext) -> String {
    let usable = effective_capabilities(agent_capabilities, work);
    let needs_delegation = delegation_needs(agent_capabilities, work);
    if usable.is_empty() && needs_delegation.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n## THIS WORK\n");
    section.push_str(&format!(
        "You are acting inside {} `{}`. Naming a capability here does NOT grant it: \
         what you may use is the intersection of your own grant and what the work needs.\n",
        work.kind.kind_token(),
        work.kind.id()
    ));

    if !usable.is_empty() {
        section.push_str("\nYours to use inside this work:\n");
        for name in &usable {
            section.push_str(&format!("- `{name}`\n"));
        }
    }

    if !needs_delegation.is_empty() {
        section.push_str(
            "\nThe work needs these and you do NOT hold them. Delegate to an authorised \
             worker rather than improvising with what you have — the work context travels \
             with the delegation, so the child acts inside this same work:\n",
        );
        for name in &needs_delegation {
            section.push_str(&format!("- `{name}`\n"));
        }
    }
    section
}

/// Capability names are compared case- and whitespace-insensitively.
///
/// A program is authored by a human in YAML and a grant is authored elsewhere;
/// requiring them to agree on capitalisation would make composition fail for a
/// reason nobody could see.
fn normalise(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}
