//! Provenance and kind for a user-knowledge entry.
//!
//! Two independent axes, both required before a memory may act:
//!   * TRUST  — where the entry came from. A security boundary: text that
//!     arrived from outside (email, screen, web) must never become a rule, or
//!     anyone who can write into the owner's inbox can suppress their alerts.
//!   * KIND   — what the entry *is*. A permission: an episodic observation is
//!     evidence and may never act as an instruction.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTrust {
    /// The owner said it, or corrected the system.
    Stated,
    /// The system inferred it from the owner's own activity.
    Inferred,
    /// Derived from content the owner did not author.
    Untrusted,
}

impl MemoryTrust {
    /// Trust a producer DECLARES for itself, from its `source_type` label.
    ///
    /// Normalised before matching, and the reason is asymmetric. Falling through
    /// to `Inferred` is fail-closed for the `Stated` labels — a variant spelling
    /// loses trust. It is fail-**open** for the untrusted ones: `Untrusted` may
    /// not condition salience and `Inferred` may, so `"Meeting_Capture"` or
    /// `" meeting_capture"` would have been *upgraded* into a level that can move
    /// what the owner is shown.
    ///
    /// `source_type` is written by a model summarising conversation content, so
    /// on an untrusted surface the spelling is attacker-influenced — the variant
    /// is not a hypothetical typo, it is a thing that can be asked for.
    ///
    /// Origin clamping ([`clamped_by_origin`](Self::clamped_by_origin)) contains
    /// this on every surface: no surface ceiling is `Stated` any more, so a
    /// declared `explicit_user_statement` can no longer reach the level that
    /// suppresses alerts however it is spelled.
    pub fn from_source_type(source_type: &str) -> Self {
        match source_type.trim().to_ascii_lowercase().as_str() {
            "explicit_user_statement" | "correction" | "owner_confirmed" => Self::Stated,
            "meeting_capture" | "screen_observation" | "screen_capture" => Self::Untrusted,
            // Fail closed: an unrecognized producer never earns Stated.
            _ => Self::Inferred,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::Inferred => "inferred",
            Self::Untrusted => "untrusted",
        }
    }

    /// The **maximum** trust content from this surface may be awarded.
    ///
    /// This is the authority, and [`from_source_type`](Self::from_source_type)
    /// is not. `source_type` is a field a model writes while summarising
    /// conversation content — on an untrusted surface that content is
    /// attacker-influenced, so the model can be induced to stamp
    /// `explicit_user_statement` and launder untrusted text into `Stated`. A
    /// surface is minted by the server from the registered session and cannot
    /// be influenced by anything said on it.
    ///
    /// An untrusted surface caps at `Untrusted`. **An owner surface caps at
    /// `Inferred`** — closed 2026-08-20, having been named and deferred on
    /// 2026-08-18.
    ///
    /// The deferred version read: an owner surface imposes no cap, so
    /// `from_source_type` is the only thing standing between a transform and
    /// `Stated`. That is the longer fuse on the same defect. An owner's session
    /// routinely carries text the owner did not author — pasted email, a
    /// document, a tool result, fetched web content — and the transform that
    /// summarises it is an LLM reading exactly that text. It may declare
    /// `explicit_user_statement`, and `Stated` + `Normative` is the one
    /// combination [`may_suppress`](Self::may_suppress) admits. So the content
    /// an attacker put in front of the owner could become a rule that silences
    /// the owner's alerts, on the owner's own surface, with no room involved.
    ///
    /// Capping at `Inferred` costs exactly one thing, stated plainly: a
    /// preference the owner really did state in chat is consolidated as an
    /// inference and can no longer suppress anything on its own. Suppression
    /// now requires a `Stated` entry from a producer that is not a transform —
    /// an explicit owner confirmation. That is the intended shape: the owner
    /// says it directly, rather than a summariser saying the owner said it.
    ///
    /// Note what this does **not** touch. A ceiling is a bound on what a
    /// TRANSFORM may award its output; it is applied where consolidation
    /// writes ([`MemoryConsolidator::clamp_trust_on_user_items`]). An entry the
    /// owner wrote through a direct memory write is not a transform output and
    /// never passes through here, so it keeps whatever trust its producer
    /// declared.
    pub fn ceiling_for_surface(surface: crate::magician_v2::agents::InvocationSurface) -> Self {
        use crate::magician_v2::agents::SurfaceAudience;
        match surface.audience() {
            SurfaceAudience::Untrusted => Self::Untrusted,
            SurfaceAudience::Owner => Self::Inferred,
        }
    }

    /// Same, from the stored `origin_surface` string on an episode. An unknown
    /// or absent surface yields `None` — "we do not know" — which callers must
    /// treat as unknown rather than as owner.
    pub fn ceiling_for_origin_surface(origin_surface: Option<&str>) -> Option<Self> {
        let raw = origin_surface.map(str::trim).filter(|s| !s.is_empty())?;
        crate::magician_v2::agents::InvocationSurface::ALL
            .into_iter()
            .find(|surface| surface.as_str().eq_ignore_ascii_case(raw))
            .map(Self::ceiling_for_surface)
    }

    /// How much this trust level permits, for ordering. Higher permits more.
    fn rank(self) -> u8 {
        match self {
            Self::Untrusted => 0,
            Self::Inferred => 1,
            Self::Stated => 2,
        }
    }

    /// Take the weaker of a **declared** trust and the trust **derived** from
    /// origin.
    ///
    /// A producer may lower its own claim — an inference drawn during an owner
    /// conversation is still only an inference. It may never raise it above what
    /// its origin permits, which is the whole point: content that arrived from a
    /// room cannot become a stated owner rule by asserting that it is one.
    pub fn clamped_by_origin(self, origin: Self) -> Self {
        if origin.rank() < self.rank() {
            origin
        } else {
            self
        }
    }

    pub fn may_suppress(self, kind: MemoryKind) -> bool {
        matches!(self, Self::Stated) && matches!(kind, MemoryKind::Normative)
    }

    /// Untrusted provenance may never move salience in either direction.
    pub fn may_condition_salience(self) -> bool {
        !matches!(self, Self::Untrusted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Normative,
    Procedural,
    Factual,
    Episodic,
}

impl MemoryKind {
    pub fn for_tier(tier: &str) -> Self {
        match tier {
            "preferences" => Self::Normative,
            "workflows" => Self::Procedural,
            "organization" | "identity" | "accounts" | "contacts" | "channels" | "skills"
            | "knowledge" | "research_findings" => Self::Factual,
            // Unknown tiers are evidence, not rules.
            _ => Self::Episodic,
        }
    }

    /// Stated owner text may be cited as "why now" even when it is factual
    /// (the live store's only explicit statements sit in research_findings).
    pub fn may_explain(self) -> bool {
        !matches!(self, Self::Episodic)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normative => "normative",
            Self::Procedural => "procedural",
            Self::Factual => "factual",
            Self::Episodic => "episodic",
        }
    }

    pub fn may_condition_salience(self) -> bool {
        matches!(self, Self::Normative | Self::Procedural)
    }

    pub fn may_propose_action(self) -> bool {
        matches!(self, Self::Procedural)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    /// The asymmetry that made a variant spelling an upgrade.
    ///
    /// Falling through to `Inferred` costs trust for the `Stated` labels, which
    /// is the safe direction. For the untrusted producers it GAINS trust —
    /// `Untrusted` may not condition salience and `Inferred` may — and
    /// `source_type` is model-written, so the variant is something that can be
    /// asked for rather than a typo waiting to happen.
    #[test]
    fn an_untrusted_producer_stays_untrusted_however_it_is_spelled() {
        for spelling in [
            "meeting_capture",
            "Meeting_Capture",
            " meeting_capture ",
            "MEETING_CAPTURE",
            "Screen_Observation",
            " screen_capture",
        ] {
            let trust = MemoryTrust::from_source_type(spelling);
            assert_eq!(
                trust,
                MemoryTrust::Untrusted,
                "`{spelling}` names an untrusted producer and must not be upgraded"
            );
            assert!(
                !trust.may_condition_salience(),
                "`{spelling}` must not be able to move what the owner is shown"
            );
        }
    }

    /// The other direction stays fail-closed: a variant of a trusted label loses
    /// trust rather than keeping it.
    #[test]
    fn a_stated_producer_is_recognised_but_an_unknown_one_is_not() {
        for spelling in ["explicit_user_statement", "Correction", " OWNER_CONFIRMED "] {
            assert_eq!(MemoryTrust::from_source_type(spelling), MemoryTrust::Stated);
        }
        assert_eq!(
            MemoryTrust::from_source_type("explicit_user_statement_v2"),
            MemoryTrust::Inferred,
            "an unrecognised producer never earns Stated"
        );
    }

    /// The owner ceiling stops at `Inferred`, and `from_source_type` is still
    /// load-bearing UNDER it: a mis-spelled untrusted producer must land on
    /// `Untrusted`, not on the ceiling. The clamp only ever lowers, so a
    /// ceiling of `Inferred` would leave an `Inferred` claim alone — the
    /// spelling is what has to carry the drop to `Untrusted` here.
    #[test]
    fn a_mis_spelled_meeting_capture_cannot_condition_salience_on_an_owner_surface() {
        use crate::magician_v2::agents::InvocationSurface;

        let ceiling = MemoryTrust::ceiling_for_surface(InvocationSurface::Chat);
        let declared = MemoryTrust::from_source_type("Meeting_Capture");
        let clamped = declared.clamped_by_origin(ceiling);
        assert_eq!(clamped, MemoryTrust::Untrusted);
        assert!(!clamped.may_condition_salience());
    }

    /// The owner-surface trust ceiling (closed 2026-08-20).
    ///
    /// The failure this pins: an owner's session carries text the owner did
    /// not author — pasted email, a document, a fetched page — and the
    /// transform summarising it declares `explicit_user_statement`. Before the
    /// cap that reached `Stated`, and `Stated` + `Normative` is the one
    /// combination that may suppress the owner's alerts. So attacker-supplied
    /// text could become a rule that silences the owner, with no room
    /// anywhere in the chain.
    ///
    /// Asserted over `InvocationSurface::ALL` rather than over a chosen few,
    /// because a new surface variant must inherit a ceiling rather than a
    /// hole, and asserted through `may_suppress` as well as through the value
    /// so that renaming the levels cannot make it pass while the consequence
    /// is gone.
    #[test]
    fn no_surface_lets_a_transform_claim_a_trust_that_may_suppress() {
        use crate::magician_v2::agents::{InvocationSurface, SurfaceAudience};

        for surface in InvocationSurface::ALL {
            let ceiling = MemoryTrust::ceiling_for_surface(surface);
            let expected = match surface.audience() {
                SurfaceAudience::Untrusted => MemoryTrust::Untrusted,
                SurfaceAudience::Owner => MemoryTrust::Inferred,
            };
            assert_eq!(
                ceiling,
                expected,
                "{} derived the wrong ceiling",
                surface.as_str()
            );
            assert!(
                !ceiling.may_suppress(MemoryKind::Normative),
                "{} lets a transform's output suppress the owner's alerts",
                surface.as_str()
            );
            assert_eq!(
                MemoryTrust::from_source_type("explicit_user_statement").clamped_by_origin(ceiling),
                expected,
                "{} let a declared owner statement survive its ceiling",
                surface.as_str()
            );
        }

        // The cost, asserted so it is a decision rather than a surprise: an
        // owner-origin consolidated preference is now an inference, which may
        // still condition what the owner is shown but may no longer silence it.
        let owner_ceiling = MemoryTrust::ceiling_for_surface(InvocationSurface::Chat);
        assert!(owner_ceiling.may_condition_salience());
        assert!(!owner_ceiling.may_suppress(MemoryKind::Normative));
    }

    use super::*;

    #[test]
    fn every_live_source_type_maps_to_a_trust_tier() {
        for (source_type, want) in [
            ("explicit_user_statement", MemoryTrust::Stated),
            ("correction", MemoryTrust::Stated),
            ("insight", MemoryTrust::Inferred),
            ("memory_candidate", MemoryTrust::Inferred),
            ("entity", MemoryTrust::Inferred),
            ("inferred_pattern", MemoryTrust::Inferred),
            ("learning_candidate", MemoryTrust::Inferred),
            ("meeting_capture", MemoryTrust::Untrusted),
            ("screen_observation", MemoryTrust::Untrusted),
            ("screen_capture", MemoryTrust::Untrusted),
            ("something_new_we_have_not_seen", MemoryTrust::Inferred),
        ] {
            assert_eq!(
                MemoryTrust::from_source_type(source_type),
                want,
                "{source_type}"
            );
        }
    }

    #[test]
    fn only_stated_normative_entries_may_suppress() {
        assert!(MemoryTrust::Stated.may_suppress(MemoryKind::Normative));
        assert!(!MemoryTrust::Stated.may_suppress(MemoryKind::Factual));
        assert!(!MemoryTrust::Inferred.may_suppress(MemoryKind::Normative));
        assert!(!MemoryTrust::Untrusted.may_suppress(MemoryKind::Normative));
    }

    #[test]
    fn tier_determines_what_a_memory_may_influence() {
        assert_eq!(MemoryKind::for_tier("preferences"), MemoryKind::Normative);
        assert_eq!(MemoryKind::for_tier("workflows"), MemoryKind::Procedural);
        assert_eq!(MemoryKind::for_tier("organization"), MemoryKind::Factual);
        assert_eq!(
            MemoryKind::for_tier("research_findings"),
            MemoryKind::Factual
        );
        assert!(MemoryKind::Factual.may_explain());
        assert!(!MemoryKind::Episodic.may_explain());
        assert_eq!(
            MemoryKind::for_tier("screen_observations"),
            MemoryKind::Episodic
        );
        assert_eq!(
            MemoryKind::for_tier("some_future_tier"),
            MemoryKind::Episodic
        );

        assert!(MemoryKind::Normative.may_condition_salience());
        assert!(!MemoryKind::Episodic.may_condition_salience());
        assert!(MemoryKind::Procedural.may_propose_action());
        assert!(!MemoryKind::Normative.may_propose_action());
    }
}
