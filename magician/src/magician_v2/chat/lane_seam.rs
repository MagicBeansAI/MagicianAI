//! The conversational lane seam (plan workstream 1.2a — Brainstorm slice;
//! 1.2b — VibeDev hot-tool slice; 1.2c — Tutor/App Copilot slice;
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! A chat lane is a product feature that mints a `FeatureMode`/
//! `InvocationSurface` pairing from an already-authenticated session, plus
//! the initial hot-tool set its turns promote. Brainstorm (Live Thinking
//! Maps) is the first lane behind this seam. VibeDev (1.2b) registered its
//! hot-tool arm here; its admission is invoke-grammar-driven and stays
//! inline in the service, with the decision content owned by the cockpit's
//! seam module since 3.4 (`magician_v2/vibedev/`, which the registered arm
//! delegates to). Tutor/App Copilot (1.2c) registered their
//! admission and hot-tool arms here: both admit on a conjunction of the
//! turn's leading invoke (`FeatureMode`, parsed server-side from the turn
//! text) and a recognized product-source label, and both promote the
//! screen-draw inline adapter's leaves. Until a lane's slice lands, that
//! lane's arms stay inline in `chat/service.rs` — do not widen this trait
//! ahead of a consuming slice.
//!
//! Batch 7 (2026-08-28, phase5 removal inventory) widens the seam from
//! admission + hot sets to the lane-declared *dispatch data* the service
//! still computes inline: per-lane boolean flags
//! ([`ChatLane::keeps_tutor_runtime_tools`],
//! [`ChatLane::injects_personal_tutor_instructions`],
//! [`ChatLane::promotes_tutor_screen_draw`],
//! [`ChatLane::participates_in_narration_approval_sweep`]), the
//! delegation/handover narrowing ([`ChatLane::narrow_delegation_targets`]),
//! the same-lane re-invoke predicates ([`ChatLane::same_lane_invoke`]), the
//! surface→mode preview table ([`feature_mode_for_surface`],
//! [`preview_surfaces`]), the narration approval sweep's feature-mode list
//! ([`narration_approval_sweep_feature_modes`]), and the None-mode
//! surface-dependent hot-set default inside
//! [`hot_chat_tools_for_feature_mode`]. Every default and override
//! replicates the named service arm verbatim (cited per method); the
//! service call sites repoint onto the lookup helpers in the follow-up
//! task. The fail-closed floor is unchanged: an unregistered mode keeps no
//! runtime tools, injects nothing, draws nothing, and answers `false` to
//! every same-lane predicate.
//!
//! What did not move in 1.2a: the brainstorm facilitation model bounding
//! (profile-override rejection and the Terra/Luna fallback) stays in the
//! service — it reads `llm_service` state the seam does not own. The
//! preview surface→mode mapping moved onto the seam in Batch 7
//! ([`feature_mode_for_surface`], [`preview_surfaces`]), derived from the
//! registry; the shared preview plumbing around it stays in the service.
//! What did not move in 1.2c: the Tutor/App Copilot rail internals stay in
//! `tutor.rs` (with the rail/receipt/preemption logic split into
//! `tutor::copilot_rail`), and the service keeps the grant-widening,
//! tutor-quick allowlists, voice takeover, and active-run continuation
//! arms that read state this seam does not own.

use std::sync::OnceLock;

use crate::magician_v2::agents::{FeatureMode, InvocationSourceKind, InvocationSurface};

/// The agent definition that owns every Brainstorm (thinking map) session.
/// Same value `feature_agent_id(FeatureMode::Brainstorm)` returns; one
/// spelling.
pub const BRAINSTORM_FACILITATION_AGENT_ID: &str = "brainstorm-facilitator";
/// The only persisted UI thread a thinking-map product session may live on.
pub const THINKING_MAP_THREAD_ID: &str = "brainstorming";
/// The only origin-channel address that proves a session was created by the
/// iOS thinking-map product surface. Persisted at session creation; a
/// per-turn `source_surface` label can never substitute for it.
pub const THINKING_MAP_PRODUCT_SOURCE_KEY: &str = "app:ios:thinking-map";

/// The `(surface, mode, source)` triple a lane mints when it admits a
/// session — field-for-field what the service's product arms return inline.
pub struct ProductLaneAdmission {
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub invocation_source_kind: InvocationSourceKind,
}

/// Exactly the session and turn fields the registered admission arms
/// read — no more. Borrowed so admission allocates nothing and cannot
/// mutate the session it inspects. The surface label is the turn's
/// normalized `source_surface`; the origin fields come from the persisted
/// `ChatSession::origin_channel`; the leading feature is the turn's
/// leading invoke parsed server-side
/// (`execution::agentic::parse_leading_feature_invocation`) — a whole
/// value the seam compares, never text it re-parses.
pub struct LaneSessionProbe<'a> {
    pub surface_label: &'a str,
    pub agent_id: &'a str,
    pub ui_thread_id: &'a str,
    pub origin_channel_type: &'a str,
    pub origin_channel_address: Option<&'a str>,
    pub leading_feature: FeatureMode,
}

/// Runtime inputs a lane's hot set may read beyond static names — the
/// surface the turn runs on and the leaf names of the capability packs
/// bound to the Tutor screen-draw inline adapter on this turn's capability
/// snapshot. The service owns the snapshot and resolves the leaves once;
/// the Tutor/App Copilot lanes promote them, every other lane ignores them,
/// and an empty slice is the no-adapters answer, not a fallback. The
/// surface is what the None-mode default (see
/// [`hot_chat_tools_for_feature_mode`]) needs: the service's inline None
/// arm is surface-dependent (voice adds `list_tasks`), so the seam's
/// unregistered-mode answer must be too.
pub struct LaneHotToolsProbe<'a> {
    pub surface: InvocationSurface,
    pub tutor_screen_draw_leaves: &'a [String],
}

/// A conversational product lane registered behind this seam.
pub trait ChatLane: Send + Sync {
    /// The wire enum value this lane owns. `FeatureMode` variants stay for
    /// wire compat; a new lane registers here instead of editing the
    /// service.
    fn feature_mode(&self) -> FeatureMode;
    /// The surface an admitted session runs on.
    fn admission_surface(&self) -> InvocationSurface;
    /// Exact-session admission. `false` is the fail-closed answer: a
    /// caller-supplied surface label alone mints nothing.
    fn matches_session(&self, probe: &LaneSessionProbe<'_>) -> bool;
    /// Tool names the service promotes into the lane's initial hot set,
    /// with the turn's runtime inputs (`LaneHotToolsProbe`). The service
    /// sorts and dedups the result; order is free.
    fn hot_chat_tools(&self, probe: &LaneHotToolsProbe<'_>) -> Vec<String>;
    /// Whether this lane's turns keep the Tutor chat runtime tools loaded.
    /// Replaces `turn_allows_tutor_runtime_tools` (chat/service.rs:2096-2102),
    /// whose whole body is `matches!(feature_mode, Tutor | AppCopilot)`:
    /// Tutor and App Copilot override `true`; every other lane — and every
    /// unregistered mode through the lookup helpers — keeps the default
    /// `false`, which drops the runtime tools exactly as the service's
    /// non-matching arms do.
    fn keeps_tutor_runtime_tools(&self) -> bool {
        false
    }
    /// Whether the turn injects the personal-tutor instruction block.
    /// Replaces `should_inject_personal_tutor_instructions`
    /// (chat/service.rs:32626-32634), body `matches!(feature_mode, Tutor |
    /// AppCopilot)` — co-extensive with [`ChatLane::keeps_tutor_runtime_tools`]
    /// today, but deliberately a separate method: one decision is about the
    /// tool catalog and the other about the prompt, and the two may diverge
    /// without re-folding the seam.
    fn injects_personal_tutor_instructions(&self) -> bool {
        false
    }
    /// Narrows the turn's structural delegation/handover target lists in
    /// place. Replaces `narrow_feature_delegation_targets`
    /// (chat/service.rs:5233-5249) arm-for-arm: the default no-op is the
    /// service's `_ => {}` arm (chat/service.rs:5247); Tutor clears both
    /// lists (the service's Tutor arm, chat/service.rs:5239-5242); App
    /// Copilot keeps only its allowed delegation target and clears
    /// handovers (the service's AppCopilot arm, chat/service.rs:5243-5246).
    fn narrow_delegation_targets(
        &self,
        delegation_targets: &mut Vec<String>,
        handover_targets: &mut Vec<String>,
    ) {
        let _ = (delegation_targets, handover_targets);
    }
    /// Whether a turn's text is an explicit invoke of this same lane — the
    /// re-invoke half of `explicit_tutor_invocation_replaces_active_run`
    /// (chat/service.rs:6243-6247): `Tutor => is_tutor_prompt(text)`,
    /// `AppCopilot => is_app_copilot_prompt(text)`, `_ => false`. The
    /// predicates themselves stay in `tutor.rs` — the seam calls them, it
    /// does not restate their marker tables.
    fn same_lane_invoke(&self, _text: &str) -> bool {
        false
    }
    /// Whether this lane's turns promote the Tutor screen-draw inline
    /// adapter. Replaces the two `matches!(invocation.feature_mode, Tutor |
    /// AppCopilot)` guards in `build_surface_projection` — the direct-grant
    /// pack promotion (chat/service.rs:28984-28997) and the
    /// `tutor_screen_draw_leaves` resolution (chat/service.rs:29081-29091):
    /// both gate the same adapter on the same two modes.
    fn promotes_tutor_screen_draw(&self) -> bool {
        false
    }
    /// Whether this lane's policy projection joins the narration approval
    /// sweep (the mode list at chat/service.rs:23878-23883:
    /// `[None, Tutor, AppCopilot, Brainstorm]` — VibeDev absent). The
    /// default `true` matches that list's membership for every lane that
    /// is not VibeDev; VibeDev overrides `false`. See
    /// [`narration_approval_sweep_feature_modes`] for why the order is
    /// pinned rather than registry-derived.
    fn participates_in_narration_approval_sweep(&self) -> bool {
        true
    }
}

/// The registered lanes, in registration order. Later slices append their
/// lane here — and to the two hand-pinned candidate lists in this file
/// (`narration_approval_sweep_feature_modes`'s historical order and
/// `preview_surfaces`'s wire-visible order), which is the full extent of
/// in-file registration. The order is the admission precedence the
/// service's old inline arm chain had: Brainstorm before Tutor/App
/// Copilot, and VibeDev fail-closed (its text-driven arm stays inline
/// after the seam arm).
pub fn registered_lanes() -> &'static [Box<dyn ChatLane>] {
    static LANES: OnceLock<Vec<Box<dyn ChatLane>>> = OnceLock::new();
    LANES
        .get_or_init(|| {
            vec![
                Box::new(BrainstormLane),
                Box::new(VibeDevLane),
                Box::new(TutorLane),
                Box::new(AppCopilotLane),
            ]
        })
        .as_slice()
}

/// Admission for a turn a registered lane claims. `None` is the fail-closed
/// answer; the caller falls through to the ordinary arms below the product
/// arms. What counts as proof is each lane's `matches_session`: Brainstorm's
/// exact persisted session shape, Tutor/App Copilot's leading-invoke +
/// product-source-label conjunction.
pub fn authenticated_product_lane(probe: &LaneSessionProbe<'_>) -> Option<ProductLaneAdmission> {
    let lane = registered_lanes()
        .iter()
        .find(|lane| lane.matches_session(probe))?;
    Some(ProductLaneAdmission {
        surface: lane.admission_surface(),
        feature_mode: lane.feature_mode(),
        // Every registered admission is a proof of product origin a client
        // cannot mint alone (persisted session shape, or an exact leading
        // invoke on a recognized product surface), so an admitted turn is a
        // product feature, not a direct one.
        invocation_source_kind: InvocationSourceKind::ProductFeature,
    })
}

/// Initial hot tools for a turn's feature mode, with the turn's runtime
/// inputs. Registered modes answer from their lane; the unregistered
/// default — which is exactly where `FeatureMode::None` lands, None having
/// no registered lane — is the service's former inline None arm: voice adds
/// the task hands beside baseline recall, every other surface keeps
/// baseline recall alone. (`FeatureMode` has no other unregistered variant
/// today, so no realizable (mode, surface) input changes answer.)
///
/// `create_task` is hot on voice because "create a task titled …" is the
/// most common actionable thing said to a voice session, and deferred it
/// costs every mouth a `tool_search` first — a hop the mouths often decline
/// to take ("the task-creation tool isn't available in this session's
/// toolset") and one that on Gemini Live forces the session reconnect a
/// catalog change needs there, which drops the turn. The delegated chat turn
/// a GPT Live 1 call runs is on this same surface and gains it too.
pub fn hot_chat_tools_for_feature_mode(
    mode: FeatureMode,
    probe: &LaneHotToolsProbe<'_>,
) -> Vec<String> {
    match lane_for_mode(mode) {
        Some(lane) => lane.hot_chat_tools(probe),
        None => match probe.surface {
            InvocationSurface::RealtimeVoice => vec![
                "search_memory".to_string(),
                "list_tasks".to_string(),
                "create_task".to_string(),
            ],
            _ => vec!["search_memory".to_string()],
        },
    }
}

/// Control tools a chat turn on this surface is never offered. `yield` is a
/// task-loop control, never a chat tool. `need_user_input` is a real chat
/// pause — the person sees the question and answers it — except on the
/// realtime voice surface, where a delegated turn's question reaches nobody:
/// the caller hears nothing, the delegation waits for an answer that cannot
/// come, and the call ends with the turn cancelled. A voice brain asks by
/// speaking, in its reply, and the next utterance answers it.
pub fn control_tools_withheld_for_surface(surface: InvocationSurface) -> &'static [&'static str] {
    match surface {
        InvocationSurface::RealtimeVoice => &["yield", "need_user_input"],
        _ => &["yield"],
    }
}

/// The registered lane owning a feature mode, if any — the find-by-mode
/// pattern every service-facing lookup helper shares. `None` is the
/// unregistered answer (`FeatureMode::None` today).
fn lane_for_mode(mode: FeatureMode) -> Option<&'static Box<dyn ChatLane>> {
    registered_lanes()
        .iter()
        .find(|lane| lane.feature_mode() == mode)
}

/// Whether the lane owning `mode` keeps the Tutor chat runtime tools — the
/// service-facing one-liner for `turn_allows_tutor_runtime_tools`
/// (chat/service.rs:2096-2102). Fail-closed `false` for unregistered modes,
/// which is also the service arm's answer for `FeatureMode::None`.
pub fn lane_keeps_tutor_runtime_tools(mode: FeatureMode) -> bool {
    lane_for_mode(mode).is_some_and(|lane| lane.keeps_tutor_runtime_tools())
}

/// Whether the lane owning `mode` promotes the Tutor screen-draw inline
/// adapter — the service-facing one-liner for the two Tutor|AppCopilot
/// guards in `build_surface_projection` (chat/service.rs:28984-28997 and
/// :29081-29091). Fail-closed `false` for unregistered modes.
pub fn lane_promotes_tutor_screen_draw(mode: FeatureMode) -> bool {
    lane_for_mode(mode).is_some_and(|lane| lane.promotes_tutor_screen_draw())
}

/// Whether `text` is an explicit same-lane invoke for the lane owning
/// `mode` — the service-facing one-liner for the match at
/// chat/service.rs:6243-6247. Fail-closed `false` for unregistered modes.
pub fn lane_same_lane_invoke(mode: FeatureMode, text: &str) -> bool {
    lane_for_mode(mode).is_some_and(|lane| lane.same_lane_invoke(text))
}

/// Whether the lane owning `mode` joins the narration approval sweep.
/// Private: callers use [`narration_approval_sweep_feature_modes`]. The
/// fallback for an unregistered mode is the trait default (`true`) so the
/// helper never invents a stricter answer than the trait declares; today
/// only `FeatureMode::None` is unregistered, and it leads the sweep list
/// unconditionally, so the fallback decides nothing for current inputs.
fn lane_participates_in_narration_approval_sweep(mode: FeatureMode) -> bool {
    lane_for_mode(mode).is_some_and(|lane| lane.participates_in_narration_approval_sweep())
}

/// The feature modes whose policy projections join the narration approval
/// sweep — replaces the hardcoded list at chat/service.rs:23878-23883:
/// `[None, Tutor, AppCopilot, Brainstorm]` (VibeDev absent).
///
/// `FeatureMode::None` leads unconditionally: it is the unregistered
/// default mode and the sweep must include the ordinary no-feature Chat
/// projection. The lane candidates then follow in the service's historical
/// order — NOT registry order (the registry is Brainstorm-first and no
/// filter of it reproduces the service order) — each admitted by its
/// [`ChatLane::participates_in_narration_approval_sweep`] flag, with
/// VibeDev answering `false` today. The order is pinned rather than
/// derived because the sweep loop's final outputs (a `HashSet` union and a
/// saturation count) are commutative, but the loop body is not provably
/// order-blind: each `build_surface_projection` call stamps shared
/// working-set-cache access sequences and emits `warn!` lines whose order a
/// reordering would change. A new lane joins by appending its mode to the
/// candidate list — still without editing the service.
pub fn narration_approval_sweep_feature_modes() -> Vec<FeatureMode> {
    let mut modes = vec![FeatureMode::None];
    for mode in [
        FeatureMode::Tutor,
        FeatureMode::AppCopilot,
        FeatureMode::Brainstorm,
        FeatureMode::Vibedev,
    ] {
        if lane_participates_in_narration_approval_sweep(mode) {
            modes.push(mode);
        }
    }
    modes
}

/// The feature mode a preview surface projects onto — replaces
/// `feature_mode_for_preview_surface` (chat/service.rs:31470-31481):
/// Tutor→Tutor, AppCopilot→AppCopilot, ThinkingMap→Brainstorm, everything
/// else→None. Derived from the registry's `admission_surface()`↔
/// `feature_mode()` pairs (BrainstormLane: ThinkingMap↔Brainstorm;
/// TutorLane: Tutor↔Tutor; AppCopilotLane: AppCopilot↔AppCopilot — each
/// matching the service table exactly). The VibeDev lane is skipped: its
/// `admission_surface` (`Chat`) is nominal and, by that lane's own
/// invariant, may never be consumed — the service table maps `Chat` to
/// `FeatureMode::None`, and consuming the nominal pair would answer
/// `Vibedev` instead. `None` is the fallback, as in the service's `_` arm.
pub fn feature_mode_for_surface(surface: InvocationSurface) -> FeatureMode {
    registered_lanes()
        .iter()
        .filter(|lane| lane.feature_mode() != FeatureMode::Vibedev)
        .find(|lane| lane.admission_surface() == surface)
        .map_or(FeatureMode::None, |lane| lane.feature_mode())
}

/// The surfaces the effective-tool-policy preview walks — replaces the
/// `PREVIEW_SURFACES` const (chat/service.rs:27778-27784: `[Chat,
/// RealtimeVoice, Tutor, AppCopilot, ThinkingMap]`). The head entries are
/// not product lanes: `Chat` is the ordinary no-feature surface and
/// `RealtimeVoice` is the voice transport, which is why they lead and why
/// no registry derivation produces them. The product entries come from the
/// lanes' `admission_surface()`, in the const's historical order (Tutor,
/// App Copilot, Brainstorm→ThinkingMap) so the Vec is byte-identical to
/// today's const — the preview's surface list is wire-visible in that
/// order. VibeDev contributes nothing: its `admission_surface` is nominal
/// (`Chat`) and never matched.
pub fn preview_surfaces() -> Vec<InvocationSurface> {
    let mut surfaces = vec![InvocationSurface::Chat, InvocationSurface::RealtimeVoice];
    for mode in [
        FeatureMode::Tutor,
        FeatureMode::AppCopilot,
        FeatureMode::Brainstorm,
    ] {
        if let Some(lane) = lane_for_mode(mode) {
            surfaces.push(lane.admission_surface());
        }
    }
    surfaces
}

struct BrainstormLane;

impl ChatLane for BrainstormLane {
    fn feature_mode(&self) -> FeatureMode {
        FeatureMode::Brainstorm
    }

    fn admission_surface(&self) -> InvocationSurface {
        InvocationSurface::ThinkingMap
    }

    /// Exactly one session shape answers true: the persisted
    /// brainstorm-facilitator thread on the iOS thinking-map origin (see
    /// the constants above). The channel-type comparison ignores case
    /// while the label and address comparisons do not; that asymmetry is
    /// the pinned behavior, not an oversight.
    fn matches_session(&self, probe: &LaneSessionProbe<'_>) -> bool {
        probe.surface_label == "thinking_map"
            && probe.agent_id == BRAINSTORM_FACILITATION_AGENT_ID
            && probe.ui_thread_id == THINKING_MAP_THREAD_ID
            && probe
                .origin_channel_type
                .trim()
                .eq_ignore_ascii_case("thinking_map")
            && probe
                .origin_channel_address
                .is_some_and(|address| address.trim() == THINKING_MAP_PRODUCT_SOURCE_KEY)
    }

    fn hot_chat_tools(&self, _probe: &LaneHotToolsProbe<'_>) -> Vec<String> {
        vec!["search_memory".to_string()]
    }
}

struct VibeDevLane;

impl ChatLane for VibeDevLane {
    fn feature_mode(&self) -> FeatureMode {
        FeatureMode::Vibedev
    }

    /// Nominal — and unreachable through admission. Because
    /// `matches_session` below is hard `false`,
    /// `authenticated_product_lane` can never return this lane, so no
    /// consumer can legitimately read this surface: the rail rides the
    /// surface the turn arrived on (`Chat` or `RealtimeVoice`) rather than
    /// minting one. The trait requires the method; the invariant it
    /// serves is that a lane's `admission_surface` must only be consumed
    /// for a lane that matched.
    fn admission_surface(&self) -> InvocationSurface {
        InvocationSurface::Chat
    }

    /// Always `false` — the fail-closed answer — by design: the rail is
    /// admitted by its invoke grammar
    /// (`invoke_grammar::parse_vibedev_rail_invocation` on the turn text),
    /// not by the session the turn rides. Since plan 3.4 that text-driven
    /// admission lives behind the cockpit's seam module
    /// (`vibedev::rail::rail_turn_lane` mints the lane triple;
    /// `rail_admits_turn` is the divert guard), called from the service's
    /// inline arm — this lane still registers for the hot-tool set only,
    /// because session-shape admission is not the rail's shape.
    fn matches_session(&self, _probe: &LaneSessionProbe<'_>) -> bool {
        false
    }

    /// The rail hands its work to a task instead of answering inline, so its
    /// turns need nothing beyond baseline recall. Since 3.4 the answer is
    /// the cockpit module's own (`vibedev::rail::hot_chat_tools`) — the
    /// registered arm delegates rather than restating it.
    fn hot_chat_tools(&self, _probe: &LaneHotToolsProbe<'_>) -> Vec<String> {
        crate::magician_v2::vibedev::rail::hot_chat_tools()
    }

    /// Replaces the VibeDev *absence* from the narration approval sweep's
    /// mode list (chat/service.rs:23878-23883 lists `[None, Tutor,
    /// AppCopilot, Brainstorm]` — no Vibedev): the rail hands its work to a
    /// task instead of projecting a Chat-surface policy, so it contributes
    /// no narration-approval authority.
    fn participates_in_narration_approval_sweep(&self) -> bool {
        false
    }
}

/// The Tutor/App Copilot hot set is one shape: baseline recall plus every
/// leaf of the capability packs bound to the Tutor screen-draw inline
/// adapter, so the overlay draw transport is hot from the first turn.
/// App Copilot is always screen-bound and Tutor keeps the same transport,
/// which is why the two lanes share the builder instead of diverging.
fn tutor_copilot_hot_chat_tools(probe: &LaneHotToolsProbe<'_>) -> Vec<String> {
    let mut names = vec!["search_memory".to_string()];
    names.extend(probe.tutor_screen_draw_leaves.iter().cloned());
    names
}

struct TutorLane;

impl ChatLane for TutorLane {
    fn feature_mode(&self) -> FeatureMode {
        FeatureMode::Tutor
    }

    fn admission_surface(&self) -> InvocationSurface {
        InvocationSurface::Tutor
    }

    /// Exactly the conjunction the service's inline arm held: a leading
    /// Tutor invoke on a recognized Tutor product-source label. Either
    /// half alone — a bare label a client asserts, or an `@tutor` on an
    /// ordinary chat thread — mints nothing.
    fn matches_session(&self, probe: &LaneSessionProbe<'_>) -> bool {
        probe.leading_feature == FeatureMode::Tutor
            && matches!(
                probe.surface_label,
                "ios_tutor_overlay" | "personal_tutor" | "personal_tutor_background" | "tutor"
            )
    }

    fn hot_chat_tools(&self, probe: &LaneHotToolsProbe<'_>) -> Vec<String> {
        tutor_copilot_hot_chat_tools(probe)
    }

    /// Replaces the Tutor arm of `turn_allows_tutor_runtime_tools`
    /// (chat/service.rs:2096-2102).
    fn keeps_tutor_runtime_tools(&self) -> bool {
        true
    }

    /// Replaces the Tutor arm of `should_inject_personal_tutor_instructions`
    /// (chat/service.rs:32626-32634).
    fn injects_personal_tutor_instructions(&self) -> bool {
        true
    }

    /// Replaces the Tutor arm of `narrow_feature_delegation_targets`
    /// (chat/service.rs:5239-5242): a Tutor turn delegates and hands over to
    /// nothing, so both lists clear.
    fn narrow_delegation_targets(
        &self,
        delegation_targets: &mut Vec<String>,
        handover_targets: &mut Vec<String>,
    ) {
        delegation_targets.clear();
        handover_targets.clear();
    }

    /// Replaces the Tutor arm of the same-lane match at
    /// chat/service.rs:6244: `is_tutor_prompt(text)`.
    fn same_lane_invoke(&self, text: &str) -> bool {
        crate::magician_v2::tutor::is_tutor_prompt(text)
    }

    /// Replaces the Tutor half of the two screen-draw guards
    /// (chat/service.rs:28984-28997 and :29081-29091).
    fn promotes_tutor_screen_draw(&self) -> bool {
        true
    }
}

struct AppCopilotLane;

impl AppCopilotLane {
    /// The single delegation target an App Copilot turn may keep — the
    /// `"mac-operator"` literal in the AppCopilot arm of
    /// `narrow_feature_delegation_targets` (chat/service.rs:5244), named so
    /// the allowed target is lane data, not a magic string.
    pub const ALLOWED_DELEGATION_TARGET: &'static str = "mac-operator";
}

impl ChatLane for AppCopilotLane {
    fn feature_mode(&self) -> FeatureMode {
        FeatureMode::AppCopilot
    }

    fn admission_surface(&self) -> InvocationSurface {
        InvocationSurface::AppCopilot
    }

    /// Same conjunction as [`TutorLane`] with the App Copilot spellings;
    /// clause order kept from the service arm it replaces. Note the
    /// label set is closed — `ios_app_copilot` is deliberately absent,
    /// and the untested-label fail-through stays the pinned behavior.
    fn matches_session(&self, probe: &LaneSessionProbe<'_>) -> bool {
        matches!(
            probe.surface_label,
            "app_copilot" | "app_copilot_background"
        ) && probe.leading_feature == FeatureMode::AppCopilot
    }

    fn hot_chat_tools(&self, probe: &LaneHotToolsProbe<'_>) -> Vec<String> {
        tutor_copilot_hot_chat_tools(probe)
    }

    /// Replaces the AppCopilot arm of `turn_allows_tutor_runtime_tools`
    /// (chat/service.rs:2096-2102).
    fn keeps_tutor_runtime_tools(&self) -> bool {
        true
    }

    /// Replaces the AppCopilot arm of
    /// `should_inject_personal_tutor_instructions`
    /// (chat/service.rs:32626-32634).
    fn injects_personal_tutor_instructions(&self) -> bool {
        true
    }

    /// Replaces the AppCopilot arm of `narrow_feature_delegation_targets`
    /// (chat/service.rs:5243-5246): only the macOS operator may receive App
    /// Copilot's delegated work, and handovers clear entirely.
    fn narrow_delegation_targets(
        &self,
        delegation_targets: &mut Vec<String>,
        handover_targets: &mut Vec<String>,
    ) {
        delegation_targets.retain(|target| target == Self::ALLOWED_DELEGATION_TARGET);
        handover_targets.clear();
    }

    /// Replaces the AppCopilot arm of the same-lane match at
    /// chat/service.rs:6245: `is_app_copilot_prompt(text)`.
    fn same_lane_invoke(&self, text: &str) -> bool {
        crate::magician_v2::tutor::is_app_copilot_prompt(text)
    }

    /// Replaces the App Copilot half of the two screen-draw guards
    /// (chat/service.rs:28984-28997 and :29081-29091).
    fn promotes_tutor_screen_draw(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(surface_label: &str, leading_feature: FeatureMode) -> LaneSessionProbe<'_> {
        LaneSessionProbe {
            surface_label,
            agent_id: "personal-assistant",
            ui_thread_id: "thread-1",
            origin_channel_type: "chat",
            origin_channel_address: None,
            leading_feature,
        }
    }

    fn hot_probe(surface: InvocationSurface, leaves: &[String]) -> LaneHotToolsProbe<'_> {
        LaneHotToolsProbe {
            surface,
            tutor_screen_draw_leaves: leaves,
        }
    }

    #[test]
    fn tutor_and_copilot_admission_require_both_conjuncts() {
        // Invoke + recognized label admits and mints the product triple.
        let admitted = authenticated_product_lane(&probe("tutor", FeatureMode::Tutor))
            .expect("tutor admission");
        assert_eq!(admitted.surface, InvocationSurface::Tutor);
        assert_eq!(admitted.feature_mode, FeatureMode::Tutor);
        assert_eq!(
            admitted.invocation_source_kind,
            InvocationSourceKind::ProductFeature
        );

        // A leading invoke on an ordinary thread mints nothing…
        assert!(authenticated_product_lane(&probe("web", FeatureMode::Tutor)).is_none());
        // …and a client-asserted product label without the invoke mints
        // nothing either. Either half alone is fail-closed.
        assert!(authenticated_product_lane(&probe("tutor", FeatureMode::None)).is_none());
        assert!(
            authenticated_product_lane(&probe("ios_tutor_overlay", FeatureMode::AppCopilot))
                .is_none()
        );

        let copilot = authenticated_product_lane(&probe("app_copilot", FeatureMode::AppCopilot))
            .expect("copilot admission");
        assert_eq!(copilot.surface, InvocationSurface::AppCopilot);
        assert_eq!(copilot.feature_mode, FeatureMode::AppCopilot);
        // The closed label set from the service arm: the iOS variant and
        // cross-product labels stay unrecognized.
        assert!(
            authenticated_product_lane(&probe("ios_app_copilot", FeatureMode::AppCopilot))
                .is_none()
        );
        assert!(authenticated_product_lane(&probe("tutor", FeatureMode::AppCopilot)).is_none());
    }

    /// The VibeDev lane registers for its hot-tool set only: its
    /// `matches_session` is hard `false`, so no probe — however
    /// constructed — may ever be admitted as the VibeDev lane through
    /// `authenticated_product_lane`, which is what keeps its nominal
    /// `admission_surface` unread. Sweep the recognized labels across
    /// every feature mode, then prove the sweep is not vacuous with the
    /// one session shape that does admit (Brainstorm's, yielding
    /// Brainstorm — never Vibedev).
    #[test]
    fn vibedev_lane_is_never_admitted_through_authenticated_product_lane() {
        for label in [
            "web",
            "chat",
            "tutor",
            "personal_tutor",
            "personal_tutor_background",
            "ios_tutor_overlay",
            "app_copilot",
            "app_copilot_background",
            "ios_app_copilot",
            "thinking_map",
            "vibedev",
            "realtime_voice",
        ] {
            for mode in [
                FeatureMode::None,
                FeatureMode::Tutor,
                FeatureMode::AppCopilot,
                FeatureMode::Brainstorm,
                FeatureMode::Vibedev,
            ] {
                if let Some(admitted) = authenticated_product_lane(&probe(label, mode)) {
                    assert_ne!(
                        admitted.feature_mode,
                        FeatureMode::Vibedev,
                        "probe ({label}, {mode:?}) must never admit the VibeDev lane"
                    );
                }
            }
        }

        // The strongest constructible probe — the exact persisted
        // brainstorm session shape — admits, and as Brainstorm only.
        let brainstorm = LaneSessionProbe {
            surface_label: "thinking_map",
            agent_id: BRAINSTORM_FACILITATION_AGENT_ID,
            ui_thread_id: THINKING_MAP_THREAD_ID,
            origin_channel_type: "thinking_map",
            origin_channel_address: Some(THINKING_MAP_PRODUCT_SOURCE_KEY),
            leading_feature: FeatureMode::Brainstorm,
        };
        assert_eq!(
            authenticated_product_lane(&brainstorm)
                .expect("the brainstorm session shape admits")
                .feature_mode,
            FeatureMode::Brainstorm
        );
    }

    #[test]
    fn tutor_and_copilot_hot_sets_promote_the_draw_leaves() {
        let leaves = vec!["tutor_screen_draw".to_string()];
        // The registered lanes' hot sets are surface-independent; the probe
        // surface exists for the None-mode default, pinned separately below.
        let hot_probe = hot_probe(InvocationSurface::Chat, &leaves);
        for mode in [FeatureMode::Tutor, FeatureMode::AppCopilot] {
            let mut tools = hot_chat_tools_for_feature_mode(mode, &hot_probe);
            tools.sort();
            tools.dedup();
            // The builder's output is exactly baseline + leaves — the
            // service's dedup is downstream — so the post-dedup set pins
            // baseline recall beside the promoted draw leaf.
            assert_eq!(
                tools,
                vec!["search_memory".to_string(), "tutor_screen_draw".to_string()]
            );
        }
        // Lanes that do not read the leaves keep baseline recall only.
        assert_eq!(
            hot_chat_tools_for_feature_mode(FeatureMode::Brainstorm, &hot_probe),
            vec!["search_memory".to_string()]
        );
        assert_eq!(
            hot_chat_tools_for_feature_mode(FeatureMode::Vibedev, &hot_probe),
            vec!["search_memory".to_string()]
        );
    }

    /// `turn_allows_tutor_runtime_tools` (chat/service.rs:2096-2102) answers
    /// true for exactly Tutor and App Copilot; every other mode — including
    /// the unregistered None — drops the runtime tools. Pin the flag for all
    /// five modes through the service-facing lookup.
    #[test]
    fn tutor_runtime_tools_flag_is_tutor_and_copilot_only() {
        for (mode, expected) in [
            (FeatureMode::None, false),
            (FeatureMode::Tutor, true),
            (FeatureMode::AppCopilot, true),
            (FeatureMode::Brainstorm, false),
            (FeatureMode::Vibedev, false),
        ] {
            assert_eq!(
                lane_keeps_tutor_runtime_tools(mode),
                expected,
                "keeps_tutor_runtime_tools({mode:?})"
            );
        }
    }

    /// `should_inject_personal_tutor_instructions`
    /// (chat/service.rs:32626-32634) is co-extensive with the runtime-tools
    /// flag today. Pin the same matrix AND the co-extensivity itself, so a
    /// future divergence between the two methods is a visible decision, not
    /// an unnoticed drift.
    #[test]
    fn personal_tutor_instruction_flag_tracks_the_runtime_tools_flag() {
        for mode in [
            FeatureMode::None,
            FeatureMode::Tutor,
            FeatureMode::AppCopilot,
            FeatureMode::Brainstorm,
            FeatureMode::Vibedev,
        ] {
            let lane_answer = lane_for_mode(mode)
                .map(|lane| lane.injects_personal_tutor_instructions())
                .unwrap_or(false);
            assert_eq!(
                lane_answer,
                lane_keeps_tutor_runtime_tools(mode),
                "inject/keeps co-extensivity broke for {mode:?}"
            );
        }
        assert!(lane_for_mode(FeatureMode::Tutor)
            .expect("tutor lane registered")
            .injects_personal_tutor_instructions());
        assert!(lane_for_mode(FeatureMode::AppCopilot)
            .expect("app copilot lane registered")
            .injects_personal_tutor_instructions());
    }

    /// The two screen-draw guards in `build_surface_projection`
    /// (chat/service.rs:28984-28997 and :29081-29091) gate on exactly Tutor
    /// and App Copilot.
    #[test]
    fn screen_draw_promotion_flag_is_tutor_and_copilot_only() {
        for (mode, expected) in [
            (FeatureMode::None, false),
            (FeatureMode::Tutor, true),
            (FeatureMode::AppCopilot, true),
            (FeatureMode::Brainstorm, false),
            (FeatureMode::Vibedev, false),
        ] {
            assert_eq!(
                lane_promotes_tutor_screen_draw(mode),
                expected,
                "promotes_tutor_screen_draw({mode:?})"
            );
        }
    }

    /// `narrow_feature_delegation_targets` (chat/service.rs:5233-5249):
    /// Tutor clears both lists, App Copilot keeps only `mac-operator` and
    /// clears handovers, and every other lane (the service's `_ => {}` arm)
    /// narrows nothing. Invoked through the find-by-mode dispatch the
    /// service will use.
    #[test]
    fn delegation_narrowing_matches_the_service_arms() {
        let sample = || {
            (
                vec![
                    "mac-operator".to_string(),
                    "web-browser".to_string(),
                    "code-reviewer".to_string(),
                ],
                vec!["handover-agent".to_string()],
            )
        };
        let (mut delegation, mut handover) = sample();
        lane_for_mode(FeatureMode::Tutor)
            .expect("tutor lane registered")
            .narrow_delegation_targets(&mut delegation, &mut handover);
        assert!(delegation.is_empty(), "tutor clears delegation targets");
        assert!(handover.is_empty(), "tutor clears handover targets");

        let (mut delegation, mut handover) = sample();
        lane_for_mode(FeatureMode::AppCopilot)
            .expect("app copilot lane registered")
            .narrow_delegation_targets(&mut delegation, &mut handover);
        assert_eq!(
            delegation,
            vec![AppCopilotLane::ALLOWED_DELEGATION_TARGET.to_string()],
            "app copilot keeps only the allowed delegation target"
        );
        assert_eq!(
            AppCopilotLane::ALLOWED_DELEGATION_TARGET,
            "mac-operator",
            "the allowed target is the service arm's literal"
        );
        assert!(handover.is_empty(), "app copilot clears handover targets");

        for mode in [FeatureMode::Brainstorm, FeatureMode::Vibedev] {
            let (mut delegation, mut handover) = sample();
            lane_for_mode(mode)
                .expect("lane registered")
                .narrow_delegation_targets(&mut delegation, &mut handover);
            let (expected_delegation, expected_handover) = sample();
            assert_eq!(delegation, expected_delegation, "{mode:?} is a no-op");
            assert_eq!(handover, expected_handover, "{mode:?} is a no-op");
        }
    }

    /// The same-lane match at chat/service.rs:6243-6247: Tutor re-invokes on
    /// a tutor prompt, App Copilot on a copilot prompt, everything else
    /// answers false — including each other's prompts and plain text.
    #[test]
    fn same_lane_invoke_matches_the_service_predicates() {
        let tutor_prompt = "@tutor explain recursion";
        let spoken_tutor_prompt = "hey tutor explain recursion";
        let copilot_prompt = "@copilot show me where to click";
        let plain_text = "please summarize this page";

        assert!(lane_same_lane_invoke(FeatureMode::Tutor, tutor_prompt));
        assert!(lane_same_lane_invoke(
            FeatureMode::Tutor,
            spoken_tutor_prompt
        ));
        assert!(!lane_same_lane_invoke(FeatureMode::Tutor, copilot_prompt));
        assert!(!lane_same_lane_invoke(FeatureMode::Tutor, plain_text));

        assert!(lane_same_lane_invoke(
            FeatureMode::AppCopilot,
            copilot_prompt
        ));
        assert!(lane_same_lane_invoke(
            FeatureMode::AppCopilot,
            "@app-copilot show me where to click"
        ));
        assert!(!lane_same_lane_invoke(
            FeatureMode::AppCopilot,
            tutor_prompt
        ));
        assert!(!lane_same_lane_invoke(FeatureMode::AppCopilot, plain_text));

        for mode in [
            FeatureMode::None,
            FeatureMode::Brainstorm,
            FeatureMode::Vibedev,
        ] {
            assert!(
                !lane_same_lane_invoke(mode, tutor_prompt),
                "{mode:?} never claims a same-lane invoke"
            );
            assert!(
                !lane_same_lane_invoke(mode, copilot_prompt),
                "{mode:?} never claims a same-lane invoke"
            );
        }
    }

    /// The unregistered-mode default of `hot_chat_tools_for_feature_mode`
    /// must replicate the service's inline None arm
    /// (chat/service.rs:29109-29114) for every surface: voice gets
    /// `list_tasks` beside baseline recall, every other surface baseline
    /// recall alone. Sweep every `InvocationSurface` variant.
    #[test]
    fn none_mode_hot_set_default_replicates_the_service_arm() {
        let leaves: Vec<String> = Vec::new();
        for surface in [
            InvocationSurface::Chat,
            InvocationSurface::RealtimeVoice,
            InvocationSurface::Task,
            InvocationSurface::Delegation,
            InvocationSurface::Handover,
            InvocationSurface::ThinkingMap,
            InvocationSurface::Tutor,
            InvocationSurface::AppCopilot,
            InvocationSurface::ContextualAssist,
            InvocationSurface::PublicEnvoy,
            InvocationSurface::Meeting,
            InvocationSurface::Plane,
        ] {
            let expected = match surface {
                InvocationSurface::RealtimeVoice => vec![
                    "search_memory".to_string(),
                    "list_tasks".to_string(),
                    "create_task".to_string(),
                ],
                _ => vec!["search_memory".to_string()],
            };
            assert_eq!(
                hot_chat_tools_for_feature_mode(FeatureMode::None, &hot_probe(surface, &leaves)),
                expected,
                "None-mode default for {surface:?}"
            );
        }
    }

    /// "Create a task titled …" is the most common actionable thing said to
    /// a voice session, and a deferred `create_task` costs every mouth a
    /// `tool_search` first: GPT Realtime and Gemini 3.8 Live declined
    /// outright ("the task-creation tool isn't available in this session's
    /// toolset"), Gemini 3.8 Live Thinking loaded it and lost the turn to
    /// the reconnect a catalog change forces on Gemini, and the chat turn a
    /// GPT Live 1 delegation runs — on this same surface — said it had no
    /// such tool. Hot, the hand is simply there.
    #[test]
    fn voice_turns_keep_the_task_creating_hand_hot() {
        let leaves: Vec<String> = Vec::new();
        let hot = hot_chat_tools_for_feature_mode(
            FeatureMode::None,
            &hot_probe(InvocationSurface::RealtimeVoice, &leaves),
        );
        assert!(hot.iter().any(|name| name == "create_task"), "{hot:?}");
        let chat = hot_chat_tools_for_feature_mode(
            FeatureMode::None,
            &hot_probe(InvocationSurface::Chat, &leaves),
        );
        assert!(
            !chat.iter().any(|name| name == "create_task"),
            "typed chat loads it on demand: {chat:?}"
        );
    }

    /// `yield` is a task-loop control and never a chat tool. `need_user_input`
    /// is a real chat pause — the person sees the question and answers it —
    /// except on the realtime voice surface, where the delegated turn's
    /// question reaches nobody: the caller hears nothing, the delegation
    /// waits for an answer that cannot come, and the call ends with the
    /// turn cancelled. A voice brain asks by speaking, in its reply.
    #[test]
    fn voice_turns_withhold_the_unhearable_pause() {
        assert_eq!(
            control_tools_withheld_for_surface(InvocationSurface::Chat),
            &["yield"]
        );
        assert_eq!(
            control_tools_withheld_for_surface(InvocationSurface::Meeting),
            &["yield"]
        );
        assert_eq!(
            control_tools_withheld_for_surface(InvocationSurface::RealtimeVoice),
            &["yield", "need_user_input"]
        );
    }

    /// `feature_mode_for_preview_surface` (chat/service.rs:31470-31481):
    /// Tutor→Tutor, AppCopilot→AppCopilot, ThinkingMap→Brainstorm, and None
    /// for every non-lane surface (the service's `_` arm).
    #[test]
    fn feature_mode_for_surface_matches_the_service_table() {
        for (surface, expected) in [
            (InvocationSurface::Tutor, FeatureMode::Tutor),
            (InvocationSurface::AppCopilot, FeatureMode::AppCopilot),
            (InvocationSurface::ThinkingMap, FeatureMode::Brainstorm),
            (InvocationSurface::Chat, FeatureMode::None),
            (InvocationSurface::RealtimeVoice, FeatureMode::None),
            (InvocationSurface::Task, FeatureMode::None),
            (InvocationSurface::Delegation, FeatureMode::None),
            (InvocationSurface::Handover, FeatureMode::None),
            (InvocationSurface::ContextualAssist, FeatureMode::None),
            (InvocationSurface::PublicEnvoy, FeatureMode::None),
            (InvocationSurface::Meeting, FeatureMode::None),
            (InvocationSurface::Plane, FeatureMode::None),
        ] {
            assert_eq!(feature_mode_for_surface(surface), expected);
        }
    }

    /// `preview_surfaces` must be byte-identical to the service's
    /// `PREVIEW_SURFACES` const (chat/service.rs:27778-27784) — the preview
    /// walks the list in this order and the order is wire-visible.
    #[test]
    fn preview_surfaces_match_the_service_const() {
        // Chat and RealtimeVoice lead as the non-lane surfaces; the three
        // registered product surfaces follow in the const's historical
        // order (Tutor, App Copilot, Brainstorm's ThinkingMap).
        assert_eq!(
            preview_surfaces(),
            vec![
                InvocationSurface::Chat,
                InvocationSurface::RealtimeVoice,
                InvocationSurface::Tutor,
                InvocationSurface::AppCopilot,
                InvocationSurface::ThinkingMap,
            ]
        );
    }

    /// The narration approval sweep's mode list must reproduce the service's
    /// exact order (chat/service.rs:23878-23883): `[None, Tutor, AppCopilot,
    /// Brainstorm]`, VibeDev absent. Pin order, membership, and the
    /// per-lane participation flags behind both.
    #[test]
    fn narration_sweep_modes_match_the_service_list() {
        assert_eq!(
            narration_approval_sweep_feature_modes(),
            vec![
                FeatureMode::None,
                FeatureMode::Tutor,
                FeatureMode::AppCopilot,
                FeatureMode::Brainstorm,
            ]
        );

        for (mode, expected) in [
            (FeatureMode::Brainstorm, true),
            (FeatureMode::Vibedev, false),
            (FeatureMode::Tutor, true),
            (FeatureMode::AppCopilot, true),
        ] {
            assert_eq!(
                lane_for_mode(mode)
                    .expect("lane registered")
                    .participates_in_narration_approval_sweep(),
                expected,
                "participation({mode:?})"
            );
        }
    }
}
