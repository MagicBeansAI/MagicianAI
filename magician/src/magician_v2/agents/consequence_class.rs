//! What an act COSTS if it is wrong — OPC phase 3, plan phase 1.
//!
//! `docs/plans/2026-08-07-opc-approval-envelopes.md` §3. This is the
//! classification only: every current `requires_approval` rule and every outward
//! act tagged with its consequence class. **No behaviour change.** The envelope
//! store, the resolver and the shadow mode are the plan's phases 2 onward.
//!
//! # Why a class rather than a judgement
//!
//! Approval today matches on `(tool, action)` — a *mechanism*. What an owner
//! consents to is an *outcome*. "Important" is a judgement and cannot be
//! computed; **consequence class is a property of the act**, which is what makes
//! the model generic rather than a fundraising special case.
//!
//! # What this axis deliberately is NOT
//!
//! Reversibility. A sent email is *correctable*, not reversible: once it has
//! been read the disclosure has happened and no follow-up undoes it. A taxonomy
//! that grouped disclosure with things that can genuinely be taken back would
//! license exactly the acts most worth thinking about.

/// The five classes of §3, ordered by what they cost when wrong.
///
/// Serialises to the same tokens as [`ConsequenceClass::as_str`], so a stored
/// envelope's `covers[]` and a logged class read identically.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConsequenceClass {
    /// A draft written, research done, a deck built but unsent. **No gate.**
    PrivateLocal,
    /// A message to someone already in the engagement. Talking to a person you
    /// already agreed to talk to.
    BoundedCommunication,
    /// Financials, a data room, non-public material. A one-way transfer of
    /// information, however correctable the wording was.
    ConfidentialDisclosure,
    /// An application submitted, a demo published — anything public.
    SubmissionOrPublication,
    /// Terms agreed, money moved, anything binding.
    CommitmentOrTransaction,
}

impl ConsequenceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateLocal => "private_local",
            Self::BoundedCommunication => "bounded_communication",
            Self::ConfidentialDisclosure => "confidential_disclosure",
            Self::SubmissionOrPublication => "submission_or_publication",
            Self::CommitmentOrTransaction => "commitment_or_transaction",
        }
    }

    /// Whether an act of this class needs a gate at all.
    ///
    /// Only private/local work does not. Building a deck is not an outward act
    /// and gating it would train the owner to click through.
    pub fn requires_gate(self) -> bool {
        !matches!(self, Self::PrivateLocal)
    }

    /// Whether a **standing** envelope may cover this class.
    ///
    /// Bounded communication only. A standing envelope is consent to a *class of
    /// future acts* the owner has not seen, and disclosure, submission and
    /// commitment are precisely the acts worth seeing first.
    pub fn standing_envelope_may_cover(self) -> bool {
        matches!(self, Self::BoundedCommunication)
    }

    /// Whether a **reviewed batch** may cover this class (§3.1).
    ///
    /// Wider than a standing envelope, because the owner saw the actual
    /// instances: *"submit to these ten, here is the template, here is the
    /// list"* is not pre-authorising an unknown act. A batch is exhausted by its
    /// own list and nothing can be added to it, which is the only reason it may
    /// carry disclosure and submission at all.
    ///
    /// **Commitment is never covered, batch or otherwise.**
    pub fn reviewed_batch_may_cover(self) -> bool {
        !matches!(self, Self::CommitmentOrTransaction)
    }
}

/// Capability actions that move money or bind the owner to something.
///
/// Listed explicitly rather than pattern-matched on words like "order" or "pay":
/// a capability whose name merely *resembles* a transaction would be gated by
/// accident, and one that does not resemble it at all — `checkout`,
/// `book_table` — would be missed. Both errors are worse than a list somebody
/// has to maintain deliberately.
const COMMITMENT_ACTIONS: &[(&str, &str)] = &[
    // Payments and orders, from the shipped `executive-assistant` grant.
    ("zepto-mcp", "create_order"),
    ("zepto-mcp", "create_online_payment_order"),
    ("zepto-mcp", "create_wallet_order"),
    ("zepto-mcp", "create_upi_reserve_pay_order"),
    ("swiggy-mcp", "place_food_order"),
    ("swiggy-mcp", "checkout"),
    // A reservation is a commitment made in someone's name even though no money
    // moves at the moment of booking.
    ("swiggy-mcp", "book_table"),
];

/// The consequence class of a capability action.
///
/// Three cases, and the middle one is the safety property:
///
/// 1. a known commitment → `CommitmentOrTransaction`;
/// 2. an act known to be **outward** but with no class assigned →
///    `CommitmentOrTransaction`, **fail closed**. Being wrong here costs a
///    prompt; being wrong the other way costs a send nobody approved;
/// 3. anything else → `PrivateLocal`, which needs no gate.
///
/// Case 3 is what keeps this a classification and not a behaviour change: an
/// ordinary local capability is untouched, exactly as today.
pub fn consequence_class_for(capability: &str, action: &str) -> ConsequenceClass {
    consequence_class_from_outward(
        capability,
        action,
        super::outward_actions::outward_action_class(capability, action),
    )
}

fn consequence_class_from_outward(
    capability: &str,
    action: &str,
    outward: Option<super::outward_actions::OutwardClass>,
) -> ConsequenceClass {
    let capability_key = capability.trim().to_ascii_lowercase();
    let action_key = action.trim().to_ascii_lowercase();

    if COMMITMENT_ACTIONS
        .iter()
        .any(|(tool, act)| *tool == capability_key && *act == action_key)
    {
        return ConsequenceClass::CommitmentOrTransaction;
    }
    // A wildcard rule over a commitment capability is a commitment: `'*'` on
    // `swiggy-mcp` covers `checkout`.
    if action_key == "*"
        && COMMITMENT_ACTIONS
            .iter()
            .any(|(tool, _)| *tool == capability_key)
    {
        return ConsequenceClass::CommitmentOrTransaction;
    }

    match outward {
        Some(outward) => outward_consequence(outward),
        // Not outward and not a known commitment: local work, no gate. This is
        // today's behaviour, unchanged, which is what plan phase 1 requires.
        None => ConsequenceClass::PrivateLocal,
    }
}

/// The consequence class of a dispatch, judged from its ARGUMENTS as well as its
/// action token.
///
/// [`consequence_class_for`] asks [`super::outward_actions::outward_action_class`],
/// which matches the action token against a list of sending verbs. That misses
/// the `raw` escape action the shipped `gmail` and `presto-gmail` skills expose:
/// `gmail action=raw args=["+send", …]` sends an email while its token says
/// nothing, so it would classify as `private_local` — **the one class that needs
/// no gate at all**, persisted onto the disclosure record of an act that reached
/// a person.
///
/// This asks [`super::outward_actions::outward_dispatch_class`] instead, which
/// treats an unbounded argv passthrough on an outward-capable capability as
/// outward whatever the token says. Any write point recording what an act cost
/// should use this one; `consequence_class_for` remains correct where only a
/// `(capability, action)` pair is available, such as a static approval rule.
pub fn consequence_class_for_dispatch(
    capability: &str,
    action: &str,
    resolved_params: &std::collections::HashMap<String, serde_json::Value>,
) -> ConsequenceClass {
    let by_pair = consequence_class_for(capability, action);
    if by_pair != ConsequenceClass::PrivateLocal {
        return by_pair;
    }
    match super::outward_actions::outward_dispatch_class(capability, action, resolved_params) {
        Some(outward) => outward_consequence(outward),
        None => ConsequenceClass::PrivateLocal,
    }
}

/// The consequence class of an act somebody **explicitly put behind approval**.
///
/// Same as [`consequence_class_for`] with one difference that matters: it can
/// never return [`ConsequenceClass::PrivateLocal`], because that class needs no
/// gate and the act in front of us already has one.
///
/// A capability nobody gated classifying as private/local is correct — that is
/// today's behaviour and the reason plan phase 1 changes nothing. The same
/// answer for a rule an author *deliberately wrote* is not conservative, it is a
/// gate quietly deleted: the moment the envelope resolver decides from the
/// class, an approval the author asked for stops being asked.
///
/// So an unclassified gated act **fails closed** to
/// [`ConsequenceClass::CommitmentOrTransaction`] — the same direction rule 2 of
/// [`consequence_class_for`] already takes for an unclassified outward act. The
/// cost of being wrong is a prompt. The cost of the other answer is a gate the
/// owner believes is there and is not.
///
/// The intended response to an over-strict answer here is to classify the action
/// deliberately, not to soften this fallback.
pub fn consequence_class_for_approval_rule(capability: &str, action: &str) -> ConsequenceClass {
    match consequence_class_for(capability, action) {
        ConsequenceClass::PrivateLocal => ConsequenceClass::CommitmentOrTransaction,
        classified => classified,
    }
}

/// The consequence class of an outward act.
///
/// Every outward capability the runtime currently classifies is a **message to
/// a person** — mail, chat, a calendar invitation — which §3 places in bounded
/// communication. Disclosure and submission classes exist and are unreachable
/// today because the acts that would carry them do not: a data room (OPC phase
/// 7) and a form-submission action (the `browser` gap) are both unbuilt.
///
/// A new outward class with no arm here fails closed to commitment rather than
/// defaulting into bounded communication, so adding a way to publish cannot
/// quietly inherit a standing envelope.
fn outward_consequence(outward: super::outward_actions::OutwardClass) -> ConsequenceClass {
    use super::outward_actions::OutwardClass;
    match outward {
        OutwardClass::Mail | OutwardClass::Message | OutwardClass::CalendarInvite => {
            ConsequenceClass::BoundedCommunication
        },
        // §3's own words for this class: *"an application submitted, a demo
        // published — anything public."* Not bounded communication, and the
        // difference is load-bearing rather than cosmetic: bounded
        // communication is the ONE class a standing envelope may cover, so
        // filing a submission there would let a blanket "you may talk to people
        // in this engagement" consent cover pressing Submit on a form the owner
        // never saw.
        OutwardClass::FormSubmission => ConsequenceClass::SubmissionOrPublication,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §3's table, as behaviour rather than prose. These three predicates are
    /// what every later phase will resolve against, so they are pinned before
    /// anything is built on them.
    #[test]
    fn the_table_in_section_three_holds() {
        // Only private/local escapes a gate.
        assert!(!ConsequenceClass::PrivateLocal.requires_gate());
        for class in [
            ConsequenceClass::BoundedCommunication,
            ConsequenceClass::ConfidentialDisclosure,
            ConsequenceClass::SubmissionOrPublication,
            ConsequenceClass::CommitmentOrTransaction,
        ] {
            assert!(class.requires_gate(), "{class:?} must be gated");
        }

        // A STANDING envelope covers bounded communication and nothing else.
        assert!(ConsequenceClass::BoundedCommunication.standing_envelope_may_cover());
        for class in [
            ConsequenceClass::ConfidentialDisclosure,
            ConsequenceClass::SubmissionOrPublication,
            ConsequenceClass::CommitmentOrTransaction,
        ] {
            assert!(
                !class.standing_envelope_may_cover(),
                "{class:?} must never be covered by a standing envelope — these are \
                 exactly the acts worth seeing before they happen"
            );
        }

        // A REVIEWED BATCH goes wider, because the owner saw the instances.
        assert!(ConsequenceClass::ConfidentialDisclosure.reviewed_batch_may_cover());
        assert!(ConsequenceClass::SubmissionOrPublication.reviewed_batch_may_cover());
    }

    /// The one rule with no exception anywhere in the plan: commitment is never
    /// covered, batch or otherwise.
    #[test]
    fn commitment_is_never_covered_by_anything() {
        let commitment = ConsequenceClass::CommitmentOrTransaction;
        assert!(!commitment.standing_envelope_may_cover());
        assert!(
            !commitment.reviewed_batch_may_cover(),
            "a reviewed batch may carry disclosure and submission; it may NEVER \
             carry a commitment"
        );
    }

    /// The shipped approval inventory, classified. This is what plan phase 1
    /// exists to produce: does the taxonomy hold against what is actually there?
    ///
    /// It does, and it turns up something worth knowing — the owner's assistant
    /// gates four payment-order actions, which are commitments, so no envelope
    /// of any kind may ever cover them.
    #[test]
    fn the_shipped_approval_inventory_classifies() {
        for (capability, action) in [
            ("zepto-mcp", "create_order"),
            ("zepto-mcp", "create_online_payment_order"),
            ("zepto-mcp", "create_wallet_order"),
            ("zepto-mcp", "create_upi_reserve_pay_order"),
            ("swiggy-mcp", "place_food_order"),
            ("swiggy-mcp", "checkout"),
            ("swiggy-mcp", "book_table"),
        ] {
            assert_eq!(
                consequence_class_for(capability, action),
                ConsequenceClass::CommitmentOrTransaction,
                "{capability}/{action} moves money or binds the owner"
            );
        }

        // The messaging half of the same inventory.
        for (capability, action) in [
            ("imessage_send", "*"),
            ("gmail", "send"),
            ("gmail", "reply_all"),
            ("calendar", "create_event"),
        ] {
            assert_eq!(
                consequence_class_for(capability, action),
                ConsequenceClass::BoundedCommunication,
                "{capability}/{action} is a message to a person"
            );
        }
    }

    /// A chat send is the SAME class as WhatsApp through Kapso, and that is
    /// asserted against `kapso-whatsapp-send` rather than restated from §3.
    ///
    /// `whatsapp`, `telegram` and `telegram-self` are person-to-person
    /// messaging: one named recipient, reached immediately, not retractable.
    /// That is bounded communication — the same answer mail and the existing
    /// WhatsApp transport already get — and **not** submission. The difference
    /// is load-bearing rather than tidy: bounded communication is the one class
    /// a standing envelope may cover, so filing these anywhere else would mean
    /// *"you may talk to people in this engagement"* stopped covering the
    /// message it most obviously means.
    ///
    /// Written as an equality against the transport that already exists so the
    /// two cannot drift: reclassifying one without the other fails here.
    #[test]
    fn a_chat_send_is_bounded_communication_like_whatsapp_through_kapso() {
        let kapso = consequence_class_for("kapso-whatsapp-send", "send");
        assert_eq!(kapso, ConsequenceClass::BoundedCommunication);

        for capability in ["whatsapp", "telegram", "telegram-self"] {
            assert_eq!(
                consequence_class_for(capability, "send"),
                kapso,
                "{capability}/send is a message to a person, exactly as kapso's is"
            );
        }
        // And mail's, since §3 files both under bounded communication.
        assert_eq!(consequence_class_for("gmail", "send"), kapso);

        // The two consequences of that class, stated rather than implied.
        assert!(kapso.requires_gate());
        assert!(
            kapso.standing_envelope_may_cover(),
            "a standing envelope covers bounded communication and nothing else"
        );
        assert_ne!(
            kapso,
            ConsequenceClass::SubmissionOrPublication,
            "a message to a named person is not a submission"
        );
    }

    /// A send composed inside a generic `run` action must retain the same
    /// bounded-communication classification as a direct send. The command
    /// wrapper cannot turn outward work into private-local work.
    #[test]
    fn a_send_composed_inside_run_remains_bounded_communication() {
        let run = std::collections::HashMap::from([
            (
                "command".to_string(),
                serde_json::json!("messages send 919876543210@s.whatsapp.net 'hi' --json"),
            ),
            ("action".to_string(), serde_json::json!("run")),
        ]);
        let class = consequence_class_for_dispatch("whatsapp", "run", &run);
        assert_eq!(
            class,
            ConsequenceClass::BoundedCommunication,
            "a send through `run` still reaches a person and must retain its outward gate"
        );
        assert!(class.requires_gate());
    }

    /// Plan phase 1 is explicitly "no behaviour change". An ordinary local
    /// capability must classify as needing no gate, or tagging the inventory
    /// would quietly become gating it.
    #[test]
    fn local_work_is_not_gated_by_classification() {
        for (capability, action) in [
            ("websearch", "search"),
            ("jq", "execute"),
            ("office-word", "write"),
            ("gmail", "list_messages"),
            ("duckdb", "query"),
        ] {
            let class = consequence_class_for(capability, action);
            assert_eq!(
                class,
                ConsequenceClass::PrivateLocal,
                "{capability}/{action} is local work and must stay ungated"
            );
            assert!(!class.requires_gate());
        }
    }

    /// The gap the shipped-inventory test found: `ceo · propose_program_missions`
    /// is behind `requires_approval`, is not outward, and is not a known
    /// commitment — so the ordinary classifier calls it private/local, which
    /// needs no gate.
    ///
    /// Answering that for a rule an author deliberately wrote is not
    /// conservative; it deletes the gate the moment the envelope resolver starts
    /// deciding from the class.
    #[test]
    fn an_explicitly_gated_act_never_classifies_as_needing_no_gate() {
        // The ordinary classifier is unchanged, and must stay that way: a
        // capability nobody gated is local work.
        assert_eq!(
            consequence_class_for("propose_program_missions", "*"),
            ConsequenceClass::PrivateLocal
        );

        // Reached through an approval rule, the same act fails closed instead.
        let gated = consequence_class_for_approval_rule("propose_program_missions", "*");
        assert!(gated.requires_gate());
        assert_eq!(gated, ConsequenceClass::CommitmentOrTransaction);
        assert!(!gated.standing_envelope_may_cover());
    }

    /// The approval-rule classifier must not INVENT strictness where a class
    /// already exists — an already-classified act keeps its own class, or the
    /// owner gets asked about every message for no reason.
    #[test]
    fn an_already_classified_gated_act_keeps_its_own_class() {
        assert_eq!(
            consequence_class_for_approval_rule("gmail", "send"),
            ConsequenceClass::BoundedCommunication
        );
        assert_eq!(
            consequence_class_for_approval_rule("zepto-mcp", "create_order"),
            ConsequenceClass::CommitmentOrTransaction
        );
    }

    /// Two spellings of every class exist — `as_str()` and serde's
    /// `rename_all = "snake_case"` — and the doc claims they agree. Nothing
    /// tested that.
    ///
    /// They must, because both are live at once: an envelope stores `covers[]`
    /// through serde while a disclosure record stores the class through
    /// `as_str()`, and the outward assertions store compares the two AS STRINGS.
    /// A renamed variant would make them diverge silently, and an envelope would
    /// stop covering a class it was granted for — with nothing failing to say so.
    #[test]
    fn the_serde_name_and_as_str_never_diverge() {
        for class in [
            ConsequenceClass::PrivateLocal,
            ConsequenceClass::BoundedCommunication,
            ConsequenceClass::ConfidentialDisclosure,
            ConsequenceClass::SubmissionOrPublication,
            ConsequenceClass::CommitmentOrTransaction,
        ] {
            let encoded = serde_json::to_string(&class).expect("serialise");
            assert_eq!(
                encoded,
                format!("\"{}\"", class.as_str()),
                "{class:?} serialises differently from how it is written to a record"
            );

            let decoded: ConsequenceClass = serde_json::from_str(&encoded).expect("round trip");
            assert_eq!(decoded, class);

            // And the string a record carries must parse back the same way.
            let from_record: ConsequenceClass =
                serde_json::from_str(&format!("\"{}\"", class.as_str()))
                    .expect("a stored `as_str` value must decode");
            assert_eq!(from_record, class);
        }
    }

    /// A wildcard rule over a commitment capability is a commitment. `'*'` on
    /// `swiggy-mcp` covers `checkout`, and reading the wildcard as "unclassified
    /// therefore harmless" would be the expensive way to be wrong.
    #[test]
    fn a_wildcard_over_a_commitment_capability_is_a_commitment() {
        assert_eq!(
            consequence_class_for("swiggy-mcp", "*"),
            ConsequenceClass::CommitmentOrTransaction
        );
        assert_eq!(
            consequence_class_for("zepto-mcp", "*"),
            ConsequenceClass::CommitmentOrTransaction
        );
    }
}
