//! Retrieval containment — which stored context an execution is allowed to see.
//!
//! OPC Workstream B, §5A.2 of
//! `docs/plans/2026-08-07-opc-engagements-contextual-authority.md`.
//!
//! The authority carrier (`magician::magician_v2::engagements`) answers *what
//! an execution may do*. It says nothing about *what an execution may read*,
//! and agent-scoped memory does not either: `memory_tiers: scope: agent`
//! separates one agent from another, never one of an agent's own engagements
//! from the next. A single outward actor serving investors, candidates,
//! vendors and press would therefore retrieve one counterparty's material
//! while answering another — a leak between outsiders even though no owner
//! data moved. This module is the boundary that stops it.
//!
//! # Two states, and the third that is not a state
//!
//! An execution is either [`RetrievalScope::Unbound`] (it carries no
//! engagement, so retrieval is unchanged) or [`RetrievalScope::Bound`] (it
//! carries one, and retrieval is confined to it). There is no "partially
//! bound": the caller either has an engagement id or does not.
//!
//! A stored item is either labelled to one engagement, labelled explicitly
//! neutral, or **unlabelled** — and unlabelled is the important case.
//!
//! # Unlabelled is not neutral
//!
//! The tempting rule is "filter out items labelled with a *different*
//! engagement". That rule is vacuously true on a corpus where nothing is
//! labelled: every item passes, the filter reports success, and the leak is
//! exactly as wide as it was before. Unlabelled context is context we cannot
//! prove is neutral, so under a bound execution it is **not retrievable**.
//! Neutrality has to be asserted by whoever wrote the item, never inferred
//! from the absence of an assertion.
//!
//! # A second dimension: one occasion, not one relationship
//!
//! An engagement is a standing relationship. An **occasion** is a single
//! bounded gathering — a meeting — and the two partition differently: two
//! meetings with the same counterparty are one engagement and two occasions,
//! and a room must not read the earlier one while it is in the later one.
//! [`RetrievalScope::Meeting`] is that second dimension, added in the same
//! shape rather than beside it, because a stored item carries exactly one
//! label key and two parallel filters would be two chances to disagree about
//! the unlabelled case.
//!
//! A meeting-bound retrieval is **narrower than a bound engagement, not
//! wider**: it admits only items labelled to that same meeting. It does not
//! admit [`ContextLabel::Neutral`], which an engagement-bound retrieval does.
//! Neutral asserts "any engagement may read this", which is a claim about
//! relationships and says nothing about occasions; honouring it in a room
//! would let anything an author marked neutral — owner material included —
//! into a gathering of outsiders. So the room reads its own occasion and
//! nothing else.
//!
//! # Generic on purpose
//!
//! Nothing here knows what an engagement is *for*. Fundraising is the first
//! consumer, not the definition — support threads, recruiting, vendors, press
//! and partnerships partition identically. Nothing here knows what a meeting
//! is for either: it is any bounded occasion whose identity the caller can
//! name. The only vocabulary is "engagement", "meeting", "neutral" and
//! "unlabelled".

use serde_json::Value;

/// Canonical metadata / record key carrying an item's engagement label.
///
/// One key, one string value, so a label survives every JSON round-trip this
/// codebase performs on memory records without a schema migration.
pub const ENGAGEMENT_SCOPE_KEY: &str = "engagement_scope";

/// The label value that asserts an item may be retrieved under **any**
/// engagement. Deliberately a word an author has to type.
pub const NEUTRAL_TOKEN: &str = "neutral";

/// Prefix of the label value that binds an item to exactly one engagement.
/// The remainder is the engagement id.
pub const ENGAGEMENT_TOKEN_PREFIX: &str = "engagement:";

/// Prefix of the label value that binds an item to exactly one **occasion** —
/// one meeting. The remainder is the meeting's binding id.
///
/// A distinct prefix rather than a reused engagement token: the two axes have
/// different admission rules (an occasion does not honour `neutral`), so a
/// reader that could not tell them apart would have to guess which rule to
/// apply, and the guess that reads as harmless — treat it as an engagement —
/// is the one that lets a later meeting read an earlier one.
pub const MEETING_TOKEN_PREFIX: &str = "meeting:";

/// What a stored item claims about which engagements may retrieve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextLabel {
    /// Explicitly asserted retrievable under any engagement. The only label
    /// that crosses an engagement boundary.
    Neutral,
    /// Bound to exactly this engagement id.
    Engagement(String),
    /// Bound to exactly this meeting — one occasion, identified by its
    /// binding id. Produced in that room and readable only from it, which is
    /// what lets a rejoin of the SAME meeting keep its own material while a
    /// later meeting reads none of it.
    Meeting(String),
    /// No label, an unreadable label, or a label we cannot parse. Never
    /// treated as neutral — see the module note.
    Unlabelled,
}

impl ContextLabel {
    /// Render back to the canonical token, so a writer and a reader of a
    /// label always agree on its spelling.
    ///
    /// [`ContextLabel::Unlabelled`] has no token: the absence of a label is
    /// not something a writer can assert.
    pub fn to_token(&self) -> Option<String> {
        match self {
            Self::Neutral => Some(NEUTRAL_TOKEN.to_string()),
            Self::Engagement(id) => Some(format!("{ENGAGEMENT_TOKEN_PREFIX}{id}")),
            Self::Meeting(id) => Some(format!("{MEETING_TOKEN_PREFIX}{id}")),
            Self::Unlabelled => None,
        }
    }
}

/// The containment an execution retrieves under.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RetrievalScope {
    /// The execution carries no engagement. Retrieval is unchanged: this is
    /// the owner's own chat, an ordinary autonomous task, an operator
    /// diagnostic. Nothing here widens anything — an unbound execution was
    /// never narrowed.
    #[default]
    Unbound,
    /// The execution carries an engagement. Retrieval is confined to items
    /// labelled with this id plus items explicitly labelled neutral.
    Bound { engagement_id: String },
    /// The execution is happening inside one meeting. Retrieval is confined
    /// to items labelled to THAT meeting — and to nothing else, `neutral`
    /// included.
    ///
    /// Narrower than [`Self::Bound`] on purpose. `neutral` asserts "any
    /// engagement may read this", a claim about relationships that nobody made
    /// about occasions; a room full of outsiders is not the place to honour a
    /// claim that was never evaluated for it.
    Meeting { meeting_id: String },
}

/// Why one item was, or was not, admitted. More specific than a bool because
/// the two denials are operationally different: one says "this belongs to
/// somebody else", the other says "nobody ever said who this belongs to", and
/// only the second is fixed by labelling the corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDecision {
    /// Retrievable.
    Admitted,
    /// The item is labelled to a different engagement.
    DeniedOtherEngagement {
        /// The engagement the execution is bound to.
        bound_to: String,
        /// The engagement the item belongs to.
        item_engagement: String,
    },
    /// The item belongs to a different occasion than the one being retrieved
    /// under — including the case where it belongs to no occasion at all
    /// (an engagement label, or an explicit `neutral`).
    ///
    /// Separate from [`Self::DeniedOtherEngagement`] because the operational
    /// answer differs: that one is fixed by binding the execution correctly,
    /// this one is never "fixed" — it is the boundary doing its job, and a
    /// diagnostic that reported them the same way would invite somebody to
    /// widen the room to make the denials stop.
    DeniedOtherMeeting {
        /// The meeting the execution is confined to.
        bound_to: String,
        /// What the item claims instead, as its canonical token. `None` when
        /// the item is unlabelled.
        item_label: Option<String>,
    },
    /// The item carries no provable label. Denied because unlabelled is not
    /// neutral.
    DeniedUnlabelled {
        /// The binding the execution is confined to — an engagement id under
        /// [`RetrievalScope::Bound`], a meeting id under
        /// [`RetrievalScope::Meeting`].
        bound_to: String,
    },
}

impl ScopeDecision {
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted)
    }
}

impl RetrievalScope {
    /// Bind retrieval to one engagement. An empty or whitespace-only id is
    /// not an engagement, and binding to it would silently produce an
    /// [`RetrievalScope::Unbound`]-shaped hole; it is rejected instead.
    pub fn bound(engagement_id: impl AsRef<str>) -> Option<Self> {
        let engagement_id = engagement_id.as_ref().trim();
        if engagement_id.is_empty() {
            return None;
        }
        Some(Self::Bound {
            engagement_id: engagement_id.to_string(),
        })
    }

    /// Confine retrieval to one meeting. Same refusal of a blank id as
    /// [`Self::bound`], and for the same reason: an empty meeting id would
    /// confine retrieval to a binding no item can ever carry, which reads as a
    /// working boundary while producing a permanently empty room.
    pub fn for_meeting(meeting_id: impl AsRef<str>) -> Option<Self> {
        let meeting_id = meeting_id.as_ref().trim();
        if meeting_id.is_empty() {
            return None;
        }
        Some(Self::Meeting {
            meeting_id: meeting_id.to_string(),
        })
    }

    /// Whether this scope narrows retrieval at all. True for BOTH bound
    /// forms — a meeting confines just as an engagement does, and a caller
    /// asking "is anything being filtered here" must get `true` for both or
    /// it will skip the filter for rooms.
    pub fn is_bound(&self) -> bool {
        !matches!(self, Self::Unbound)
    }

    pub fn bound_engagement_id(&self) -> Option<&str> {
        match self {
            Self::Bound { engagement_id } => Some(engagement_id.as_str()),
            Self::Unbound | Self::Meeting { .. } => None,
        }
    }

    /// The meeting this retrieval is confined to, if any.
    pub fn bound_meeting_id(&self) -> Option<&str> {
        match self {
            Self::Meeting { meeting_id } => Some(meeting_id.as_str()),
            Self::Unbound | Self::Bound { .. } => None,
        }
    }

    /// The whole rule, in one place.
    pub fn decide(&self, label: &ContextLabel) -> ScopeDecision {
        match self {
            Self::Unbound => ScopeDecision::Admitted,
            Self::Bound { engagement_id } => match label {
                ContextLabel::Neutral => ScopeDecision::Admitted,
                ContextLabel::Engagement(item_engagement) if item_engagement == engagement_id => {
                    ScopeDecision::Admitted
                },
                ContextLabel::Engagement(item_engagement) => ScopeDecision::DeniedOtherEngagement {
                    bound_to: engagement_id.clone(),
                    item_engagement: item_engagement.clone(),
                },
                // An occasion inside this engagement is still not this
                // engagement's shared material: a meeting label says "produced
                // in one room", and the engagement never asserted that the room
                // was neutral. Denied as an unproven label rather than admitted
                // as a narrower one.
                ContextLabel::Meeting(_) | ContextLabel::Unlabelled => {
                    ScopeDecision::DeniedUnlabelled {
                        bound_to: engagement_id.clone(),
                    }
                },
            },
            Self::Meeting { meeting_id } => match label {
                ContextLabel::Meeting(item_meeting) if item_meeting == meeting_id => {
                    ScopeDecision::Admitted
                },
                other => ScopeDecision::DeniedOtherMeeting {
                    bound_to: meeting_id.clone(),
                    item_label: other.to_token(),
                },
            },
        }
    }

    pub fn admits(&self, label: &ContextLabel) -> bool {
        self.decide(label).is_admitted()
    }

    /// Convenience for the common shape: an item whose label lives in a JSON
    /// metadata object.
    pub fn admits_metadata(&self, metadata: &Value) -> bool {
        self.admits(&label_from_metadata(metadata))
    }
}

/// Parse a canonical label token.
///
/// Every unrecognised spelling resolves to [`ContextLabel::Unlabelled`] rather
/// than to an error the caller might discard: a label we cannot read is a
/// label we cannot honour, and the fail-closed reading of "cannot honour" is
/// "not retrievable".
pub fn label_from_token(token: &str) -> ContextLabel {
    let token = token.trim();
    if token.eq_ignore_ascii_case(NEUTRAL_TOKEN) {
        return ContextLabel::Neutral;
    }
    if let Some(meeting_id) = token.strip_prefix(MEETING_TOKEN_PREFIX) {
        let meeting_id = meeting_id.trim();
        if meeting_id.is_empty() {
            // `meeting:` with nothing after it names no occasion, and an
            // occasion nobody named is one every room could claim to be.
            return ContextLabel::Unlabelled;
        }
        return ContextLabel::Meeting(meeting_id.to_string());
    }
    let Some(engagement_id) = token.strip_prefix(ENGAGEMENT_TOKEN_PREFIX) else {
        return ContextLabel::Unlabelled;
    };
    let engagement_id = engagement_id.trim();
    if engagement_id.is_empty() {
        // `engagement:` with nothing after it names no engagement. Admitting
        // it under every engagement is precisely the vacuous reading.
        return ContextLabel::Unlabelled;
    }
    ContextLabel::Engagement(engagement_id.to_string())
}

/// Read the raw label token out of any JSON object that might carry one — a
/// memory item, a tier record's `fields`, an episode record.
///
/// Returns `None` when the value is not an object, has no
/// [`ENGAGEMENT_SCOPE_KEY`], or carries a non-string under it. All three mean
/// the same thing to a reader: nobody asserted a scope here.
pub fn engagement_scope_token(source: &Value) -> Option<String> {
    let token = source.get(ENGAGEMENT_SCOPE_KEY)?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// The label of an item whose metadata is a JSON object.
pub fn label_from_metadata(metadata: &Value) -> ContextLabel {
    match engagement_scope_token(metadata) {
        Some(token) => label_from_token(&token),
        None => ContextLabel::Unlabelled,
    }
}

/// Stamp a resolved label onto a metadata object so downstream readers do not
/// have to re-derive it from wherever it originally lived.
///
/// Writes the key even when the label is [`ContextLabel::Unlabelled`] — as
/// JSON `null` — because a reader must be able to tell "this pipeline
/// considered the label and found none" from "this pipeline never looked".
pub fn stamp_engagement_scope(metadata: &mut Value, label: &ContextLabel) {
    let Some(map) = metadata.as_object_mut() else {
        return;
    };
    match label.to_token() {
        Some(token) => {
            map.insert(ENGAGEMENT_SCOPE_KEY.to_string(), Value::String(token));
        },
        None => {
            map.insert(ENGAGEMENT_SCOPE_KEY.to_string(), Value::Null);
        },
    }
}

/// Resolve an item's label from its own JSON, falling back to a label
/// inherited from the record that contains it.
///
/// Inheritance only ever flows from a container to its items, and an item's
/// own label always wins. A container labelled to an engagement cannot be
/// widened by an item inside it, because an unlabelled item resolves to the
/// container's engagement rather than to [`ContextLabel::Unlabelled`], and an
/// item labelled to a different engagement keeps its own — narrower for that
/// item, never wider for the container.
pub fn label_for_item(item: &Value, inherited: Option<&str>) -> ContextLabel {
    if let Some(token) = engagement_scope_token(item) {
        return label_from_token(&token);
    }
    match inherited {
        Some(token) => label_from_token(token),
        None => ContextLabel::Unlabelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Pins that an unbound execution is not narrowed by this module. If
    /// `decide` ever started denying under `Unbound`, every ordinary chat
    /// turn and autonomous task would lose its memory.
    #[test]
    fn unbound_admits_every_label() {
        let scope = RetrievalScope::Unbound;
        assert_eq!(
            scope.decide(&ContextLabel::Neutral),
            ScopeDecision::Admitted
        );
        assert_eq!(
            scope.decide(&ContextLabel::Engagement("eng-a".to_string())),
            ScopeDecision::Admitted
        );
        assert_eq!(
            scope.decide(&ContextLabel::Unlabelled),
            ScopeDecision::Admitted
        );
    }

    /// The failure this pins: a later meeting reading an earlier meeting's
    /// material. It is the one the surface axis alone could not stop —
    /// `origin_surface` says an entry came from a room, never WHICH room, so
    /// the only containment it could express was "no room reads any of it".
    ///
    /// Both directions are asserted against the same corpus, because either
    /// half alone is worthless: a scope that admitted nothing would pass the
    /// containment half while leaving a rejoining room permanently blank, and
    /// that is precisely the regression this dimension exists to avoid.
    #[test]
    fn a_meeting_reads_its_own_material_and_no_other_meetings() {
        let room = RetrievalScope::for_meeting("meeting-weekly-sync-abc")
            .expect("a named meeting confines retrieval");

        // The rejoin case: same meeting, so its own material is readable.
        assert_eq!(
            room.decide(&ContextLabel::Meeting(
                "meeting-weekly-sync-abc".to_string()
            )),
            ScopeDecision::Admitted,
            "a room could not read what it produced itself, so a rejoin starts blank"
        );

        // The containment case: a different occasion is denied, and the denial
        // names what the item claimed so an audit line can say what it kept out.
        assert_eq!(
            room.decide(&ContextLabel::Meeting(
                "meeting-board-review-xyz".to_string()
            )),
            ScopeDecision::DeniedOtherMeeting {
                bound_to: "meeting-weekly-sync-abc".to_string(),
                item_label: Some("meeting:meeting-board-review-xyz".to_string()),
            }
        );
    }

    /// A meeting is NARROWER than a bound engagement, and the difference is
    /// `neutral`.
    ///
    /// `neutral` asserts "any engagement may read this" — a claim about
    /// relationships that nobody evaluated for occasions. Honouring it in a
    /// room would admit every item an author ever marked neutral, owner
    /// material included, into a gathering of outsiders. Pinned separately so
    /// making the two scopes share one rule is a visible decision rather than
    /// a tidy-up.
    #[test]
    fn a_meeting_does_not_honour_neutral_or_an_engagement_label() {
        let room = RetrievalScope::for_meeting("meeting-1").expect("named meeting");
        let engagement = RetrievalScope::bound("eng-a").expect("named engagement");

        assert_eq!(
            engagement.decide(&ContextLabel::Neutral),
            ScopeDecision::Admitted,
            "the engagement axis must keep honouring neutral — this must not \
             change any existing behaviour"
        );
        assert_eq!(
            room.decide(&ContextLabel::Neutral),
            ScopeDecision::DeniedOtherMeeting {
                bound_to: "meeting-1".to_string(),
                item_label: Some("neutral".to_string()),
            },
            "a room honoured a neutrality claim that was never made about rooms"
        );
        assert_eq!(
            room.decide(&ContextLabel::Engagement("eng-a".to_string())),
            ScopeDecision::DeniedOtherMeeting {
                bound_to: "meeting-1".to_string(),
                item_label: Some("engagement:eng-a".to_string()),
            }
        );
        assert_eq!(
            room.decide(&ContextLabel::Unlabelled),
            ScopeDecision::DeniedOtherMeeting {
                bound_to: "meeting-1".to_string(),
                item_label: None,
            },
            "unlabelled is not this meeting's, for the same reason it is not neutral"
        );
    }

    /// The reverse direction, which is easy to leave open: an engagement-bound
    /// execution must not inherit one of its own meetings' material either. A
    /// room's content is produced in front of whoever was in the room; the
    /// engagement never asserted that was everyone.
    #[test]
    fn an_engagement_does_not_absorb_its_own_meetings_material() {
        let engagement = RetrievalScope::bound("eng-a").expect("named engagement");
        assert_eq!(
            engagement.decide(&ContextLabel::Meeting("meeting-1".to_string())),
            ScopeDecision::DeniedUnlabelled {
                bound_to: "eng-a".to_string(),
            }
        );
    }

    /// Every way of failing to name an occasion resolves to `Unlabelled`, not
    /// to a meeting every room matches. `meeting:` with nothing after it is
    /// the shape that would otherwise be admitted by whichever room asked.
    #[test]
    fn an_unnamed_meeting_binds_nothing() {
        assert_eq!(
            label_from_token("meeting:"),
            ContextLabel::Unlabelled,
            "`meeting:` names no occasion and must not match one"
        );
        assert_eq!(label_from_token("meeting:   "), ContextLabel::Unlabelled);
        assert!(
            RetrievalScope::for_meeting("   ").is_none(),
            "a blank meeting id would confine retrieval to a binding nothing carries"
        );
        assert_eq!(
            label_from_token("meeting:meeting-1"),
            ContextLabel::Meeting("meeting-1".to_string())
        );
        // Round-trips through the same spelling a writer stamps.
        assert_eq!(
            ContextLabel::Meeting("meeting-1".to_string()).to_token(),
            Some("meeting:meeting-1".to_string())
        );
    }

    /// `is_bound` gates whether a caller applies the filter at all, so a
    /// meeting scope reporting `false` there would skip containment entirely
    /// while every rule above still passed its own unit test.
    #[test]
    fn a_meeting_scope_reports_itself_as_narrowing_retrieval() {
        let room = RetrievalScope::for_meeting("meeting-1").expect("named meeting");
        assert!(room.is_bound());
        assert_eq!(room.bound_meeting_id(), Some("meeting-1"));
        assert_eq!(
            room.bound_engagement_id(),
            None,
            "a meeting must not read as an engagement to a caller keying on one"
        );
        assert!(!RetrievalScope::Unbound.is_bound());
    }

    /// Pins the leak this module exists to close: an execution bound to one
    /// engagement must not admit an item belonging to another, and the denial
    /// must name both sides so an audit line can say which pair it separated.
    #[test]
    fn bound_denies_another_engagements_item_and_names_both() {
        let scope = RetrievalScope::bound("eng-a").expect("non-empty id binds");
        assert_eq!(
            scope.decide(&ContextLabel::Engagement("eng-b".to_string())),
            ScopeDecision::DeniedOtherEngagement {
                bound_to: "eng-a".to_string(),
                item_engagement: "eng-b".to_string(),
            }
        );
        assert_eq!(
            scope.decide(&ContextLabel::Engagement("eng-a".to_string())),
            ScopeDecision::Admitted
        );
    }

    /// Pins the vacuous-truth failure: if unlabelled resolved to neutral, a
    /// corpus where nothing is labelled would pass this filter untouched and
    /// the containment would be decorative.
    #[test]
    fn bound_denies_unlabelled_rather_than_treating_it_as_neutral() {
        let scope = RetrievalScope::bound("eng-a").expect("non-empty id binds");
        assert_eq!(
            scope.decide(&ContextLabel::Unlabelled),
            ScopeDecision::DeniedUnlabelled {
                bound_to: "eng-a".to_string(),
            }
        );
        assert_eq!(
            scope.decide(&ContextLabel::Neutral),
            ScopeDecision::Admitted
        );
    }

    /// Pins that only an explicit assertion crosses engagements. A metadata
    /// object with no key, a null value, a non-string value and an empty
    /// string are four different ways of saying nothing, and all four must
    /// read as unlabelled rather than as neutral.
    #[test]
    fn unreadable_labels_all_resolve_to_unlabelled() {
        assert_eq!(label_from_metadata(&json!({})), ContextLabel::Unlabelled);
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": null })),
            ContextLabel::Unlabelled
        );
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": 7 })),
            ContextLabel::Unlabelled
        );
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": "   " })),
            ContextLabel::Unlabelled
        );
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": "engagement:" })),
            ContextLabel::Unlabelled
        );
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": "shared" })),
            ContextLabel::Unlabelled
        );
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": "neutral" })),
            ContextLabel::Neutral
        );
        assert_eq!(
            label_from_metadata(&json!({ "engagement_scope": "engagement:eng-a" })),
            ContextLabel::Engagement("eng-a".to_string())
        );
    }

    /// Pins that binding to an empty id is refused. A `Bound { "" }` would
    /// deny every labelled item while matching nothing, which reads as a
    /// working filter and is actually a broken one.
    #[test]
    fn empty_engagement_id_does_not_bind() {
        assert!(RetrievalScope::bound("").is_none());
        assert!(RetrievalScope::bound("   ").is_none());
        assert_eq!(
            RetrievalScope::bound("eng-a"),
            Some(RetrievalScope::Bound {
                engagement_id: "eng-a".to_string()
            })
        );
    }

    /// Pins the inheritance direction: a container's label reaches an item
    /// that has none, and never overrides one the item declared.
    #[test]
    fn item_label_wins_over_inherited_container_label() {
        assert_eq!(
            label_for_item(&json!({ "text": "x" }), Some("engagement:eng-a")),
            ContextLabel::Engagement("eng-a".to_string())
        );
        assert_eq!(
            label_for_item(
                &json!({ "engagement_scope": "engagement:eng-b" }),
                Some("engagement:eng-a")
            ),
            ContextLabel::Engagement("eng-b".to_string())
        );
        assert_eq!(
            label_for_item(&json!({ "text": "x" }), None),
            ContextLabel::Unlabelled
        );
    }

    /// Pins that a stamped label round-trips, and that "considered, found
    /// none" is written as an explicit null rather than left absent — a
    /// reader must be able to tell it apart from "never looked".
    #[test]
    fn stamped_labels_round_trip_and_record_the_absence() {
        let mut metadata = json!({ "candidate_kind": "collection_item" });
        stamp_engagement_scope(
            &mut metadata,
            &ContextLabel::Engagement("eng-a".to_string()),
        );
        assert_eq!(
            metadata.get("engagement_scope").and_then(Value::as_str),
            Some("engagement:eng-a")
        );
        assert_eq!(
            label_from_metadata(&metadata),
            ContextLabel::Engagement("eng-a".to_string())
        );

        let mut absent = json!({ "candidate_kind": "tier" });
        stamp_engagement_scope(&mut absent, &ContextLabel::Unlabelled);
        assert!(absent.get("engagement_scope").is_some());
        assert!(absent
            .get("engagement_scope")
            .is_some_and(|value| value.is_null()));
        assert_eq!(label_from_metadata(&absent), ContextLabel::Unlabelled);
    }
}
