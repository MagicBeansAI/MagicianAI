//! Pure lifecycle for the macOS notch orb.
//!
//! The desktop renderer is deliberately downstream of this module.  Color and
//! motion are projections of a state the Rust process owns, so a sleeping or
//! temporarily hidden webview can never invent what the microphone is doing.

use serde::Serialize;

pub const ORB_PHASE_EVENT: &str = "orb://phase";
pub const ORB_CAPTION_EVENT: &str = "orb://caption";
pub const ORB_CAPTION_CLEAR_EVENT: &str = "orb://caption-clear";
pub const ORB_ENDED_EVENT: &str = "orb://ended";
pub const ORB_AUDIO_LEVEL_EVENT: &str = "orb://audio-level";

pub const DEFAULT_LEASH_MS: u64 = 2 * 60 * 60 * 1_000;
pub const DEFAULT_COOLDOWN_MS: u64 = 2_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrbTurn {
    Listening,
    Thinking,
    Speaking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrbEndedReason {
    CapReached,
    UserDisarm,
    MicrophoneLost,
    PowerPolicy,
    SessionFailed,
}

impl OrbEndedReason {
    pub fn message(&self) -> &'static str {
        match self {
            Self::CapReached => "Listening window ended.",
            Self::UserDisarm => "Resting",
            Self::MicrophoneLost => "Microphone access was lost.",
            Self::PowerPolicy => "Paused while this Mac is on battery.",
            Self::SessionFailed => "The voice session ended.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrbState {
    Off,
    Arming,
    Armed,
    Heard {
        phrase: String,
    },
    Connecting,
    Conversing(OrbTurn),
    Cooldown {
        until_ms: u64,
    },
    /// A press-to-talk session is parked. The microphone is closed. A Live
    /// socket may still be connected so the next hold does not pay for setup.
    HoldReady,
    VoiceBusy,
    Paused {
        until_ms: u64,
    },
    Disarming {
        reason: OrbEndedReason,
    },
    Ended {
        reason: OrbEndedReason,
    },
    RecoverableError {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrbPhase {
    Armed,
    Heard,
    Listening,
    Thinking,
    Speaking,
    Ended,
}

impl OrbPhase {
    pub fn palette_key(self) -> &'static str {
        match self {
            Self::Armed => "armed_ember",
            Self::Heard => "violet_surge",
            Self::Listening => "calm_aurora",
            Self::Thinking => "amber_thinking",
            Self::Speaking => "teal_speaking",
            Self::Ended => "graphite",
        }
    }

    pub fn status(self) -> &'static str {
        match self {
            Self::Armed => "Ready when you are",
            Self::Heard => "I heard you",
            Self::Listening => "Listening",
            Self::Thinking => "Thinking",
            Self::Speaking => "Speaking",
            Self::Ended => "Resting",
        }
    }
}

impl OrbState {
    pub fn phase(&self) -> Option<OrbPhase> {
        match self {
            Self::Off | Self::Arming => None,
            Self::Armed | Self::HoldReady | Self::RecoverableError { .. } => Some(OrbPhase::Armed),
            Self::Heard { .. } | Self::Connecting => Some(OrbPhase::Heard),
            Self::Conversing(OrbTurn::Listening) | Self::Cooldown { .. } => {
                Some(OrbPhase::Listening)
            },
            Self::Conversing(OrbTurn::Thinking) => Some(OrbPhase::Thinking),
            Self::Conversing(OrbTurn::Speaking) => Some(OrbPhase::Speaking),
            Self::VoiceBusy | Self::Paused { .. } | Self::Disarming { .. } | Self::Ended { .. } => {
                Some(OrbPhase::Ended)
            },
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Arming => "arming",
            Self::Armed => "armed",
            Self::Heard { .. } => "heard",
            Self::Connecting => "connecting",
            Self::Conversing(OrbTurn::Listening) => "listening",
            Self::Conversing(OrbTurn::Thinking) => "thinking",
            Self::Conversing(OrbTurn::Speaking) => "speaking",
            Self::Cooldown { .. } => "cooldown",
            Self::HoldReady => "hold_ready",
            Self::VoiceBusy => "voice_busy",
            Self::Paused { .. } => "paused",
            Self::Disarming { .. } => "disarming",
            Self::Ended { .. } => "ended",
            Self::RecoverableError { .. } => "recoverable_error",
        }
    }

    pub fn window_is_open(&self) -> bool {
        matches!(
            self,
            Self::Armed
                | Self::Heard { .. }
                | Self::Connecting
                | Self::Conversing(_)
                | Self::Cooldown { .. }
                | Self::HoldReady
                | Self::RecoverableError { .. }
        )
    }

    fn status(&self) -> String {
        match self {
            Self::Heard { phrase } if !phrase.trim().is_empty() => {
                format!("Heard “{}”", phrase.trim())
            },
            Self::Connecting => "Making a voice connection…".to_string(),
            Self::Cooldown { .. } => "I’m still listening".to_string(),
            Self::HoldReady => "Hold Left ⌥ to talk".to_string(),
            Self::VoiceBusy => {
                "Waiting while another voice session uses the microphone".to_string()
            },
            Self::Paused { .. } => "Paused for a little while".to_string(),
            Self::Disarming { reason } | Self::Ended { reason } => reason.message().to_string(),
            Self::RecoverableError { message } => message.clone(),
            _ => self
                .phase()
                .map(OrbPhase::status)
                .unwrap_or("Resting")
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrbAction {
    Configure {
        leash_ms: u64,
        cooldown_ms: u64,
        wake_phrase: String,
    },
    Arm,
    WakeHeard {
        phrase: String,
    },
    Connecting,
    Connected,
    Turn(OrbTurn),
    ConversationEnded,
    /// Park a press-to-talk turn. The microphone is closed; a retained Live
    /// socket is allowed to stay up.
    HoldReady,
    RecoverableError {
        message: String,
    },
    ExternalVoiceStarted,
    ExternalVoiceEnded,
    Pause {
        duration_ms: u64,
    },
    Resume,
    Disarm {
        reason: OrbEndedReason,
    },
    CompleteDisarm,
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrbSnapshot {
    pub revision: u64,
    pub state: &'static str,
    pub phase: Option<OrbPhase>,
    pub palette_key: Option<&'static str>,
    pub status: String,
    pub window_open: bool,
    pub paused_until_ms: Option<u64>,
    pub cooldown_until_ms: Option<u64>,
    pub leash_deadline_ms: Option<u64>,
    pub cap_expired_during_wake: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrbCaptionPayload {
    pub role: &'static str,
    pub speaker_name: String,
    pub text: String,
    pub final_caption: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrbAudioLevelPayload {
    pub channel: &'static str,
    pub level: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrbEndedPayload {
    pub reason: OrbEndedReason,
    pub message: &'static str,
}

#[derive(Debug, Clone)]
pub struct OrbMachine {
    state: OrbState,
    revision: u64,
    leash_ms: u64,
    cooldown_ms: u64,
    wake_phrase: String,
    leash_deadline_ms: Option<u64>,
    cap_expired_during_wake: bool,
}

impl Default for OrbMachine {
    fn default() -> Self {
        Self {
            state: OrbState::Off,
            revision: 0,
            leash_ms: DEFAULT_LEASH_MS,
            cooldown_ms: DEFAULT_COOLDOWN_MS,
            wake_phrase: String::new(),
            leash_deadline_ms: None,
            cap_expired_during_wake: false,
        }
    }
}

impl OrbMachine {
    pub fn state(&self) -> &OrbState {
        &self.state
    }

    pub fn snapshot(&self) -> OrbSnapshot {
        let phase = self.state.phase();
        OrbSnapshot {
            revision: self.revision,
            state: self.state.name(),
            phase,
            palette_key: phase.map(OrbPhase::palette_key),
            status: if matches!(self.state, OrbState::Armed) && !self.wake_phrase.is_empty() {
                format!("Say “{}”", self.wake_phrase)
            } else if matches!(self.state, OrbState::Armed) {
                // Wake is off. The resting orb is a press-to-talk control, so
                // it must not invite a phrase the detector is not running.
                "Hold Left ⌥ to talk".to_string()
            } else {
                self.state.status()
            },
            window_open: self.state.window_is_open(),
            paused_until_ms: match self.state {
                OrbState::Paused { until_ms } => Some(until_ms),
                _ => None,
            },
            cooldown_until_ms: match self.state {
                OrbState::Cooldown { until_ms } => Some(until_ms),
                _ => None,
            },
            leash_deadline_ms: self.leash_deadline_ms,
            cap_expired_during_wake: self.cap_expired_during_wake,
        }
    }

    /// Apply a lifecycle action, then project an already-owned external
    /// microphone before publishing the snapshot. This closes arm/resume/timer
    /// races without teaching the pure reducer about process-global atomics.
    pub fn apply_with_external_voice(
        &mut self,
        action: OrbAction,
        now_ms: u64,
        external_voice_active: bool,
    ) -> bool {
        let mut changed = self.apply(action, now_ms);
        if external_voice_active && matches!(self.state, OrbState::Armed | OrbState::HoldReady) {
            changed |= self.apply(OrbAction::ExternalVoiceStarted, now_ms);
        }
        changed
    }

    /// Apply one action at an injected clock value. Returns `true` only when the
    /// externally visible snapshot changed.
    pub fn apply(&mut self, action: OrbAction, now_ms: u64) -> bool {
        let before = self.snapshot();
        match action {
            OrbAction::Configure {
                leash_ms,
                cooldown_ms,
                wake_phrase,
            } => {
                self.leash_ms = leash_ms.max(1_000);
                self.cooldown_ms = cooldown_ms.max(250);
                self.wake_phrase = wake_phrase.trim().to_string();
                // Configuration is not a lifecycle event. In particular, an
                // unrelated settings save must not extend an already-running
                // listening lease. The new duration takes effect on the next
                // explicit arm/resume.
            },
            OrbAction::Arm => {
                if matches!(
                    self.state,
                    OrbState::Off | OrbState::Arming | OrbState::Ended { .. }
                ) {
                    self.state = OrbState::Armed;
                    self.leash_deadline_ms = Some(now_ms.saturating_add(self.leash_ms));
                    self.cap_expired_during_wake = false;
                }
            },
            OrbAction::WakeHeard { phrase } => {
                // A pause is a latch, not just a visual state: wake hits are
                // discarded rather than queued for the pause boundary.
                if matches!(
                    self.state,
                    OrbState::Armed | OrbState::Cooldown { .. } | OrbState::HoldReady
                ) {
                    self.state = OrbState::Heard { phrase };
                }
            },
            OrbAction::Connecting => {
                if matches!(self.state, OrbState::Heard { .. }) {
                    self.state = OrbState::Connecting;
                }
            },
            OrbAction::Connected => {
                if matches!(self.state, OrbState::Heard { .. } | OrbState::Connecting) {
                    self.state = OrbState::Conversing(OrbTurn::Listening);
                }
            },
            OrbAction::Turn(turn) => {
                if matches!(
                    self.state,
                    OrbState::Heard { .. }
                        | OrbState::Connecting
                        | OrbState::Conversing(_)
                        | OrbState::Cooldown { .. }
                        | OrbState::HoldReady
                        | OrbState::RecoverableError { .. }
                ) {
                    self.state = OrbState::Conversing(turn);
                }
            },
            OrbAction::ConversationEnded => {
                if self.cap_expired_during_wake {
                    self.state = OrbState::Disarming {
                        reason: OrbEndedReason::CapReached,
                    };
                } else if matches!(
                    self.state,
                    OrbState::Heard { .. }
                        | OrbState::Connecting
                        | OrbState::Conversing(_)
                        | OrbState::Cooldown { .. }
                        | OrbState::HoldReady
                ) {
                    self.state = OrbState::Cooldown {
                        until_ms: now_ms.saturating_add(self.cooldown_ms),
                    };
                }
            },
            OrbAction::HoldReady => {
                if matches!(
                    self.state,
                    OrbState::Heard { .. }
                        | OrbState::Connecting
                        | OrbState::Conversing(_)
                        | OrbState::Cooldown { .. }
                        | OrbState::RecoverableError { .. }
                ) {
                    self.state = OrbState::HoldReady;
                }
            },
            OrbAction::RecoverableError { message } => {
                if self.state.window_is_open() {
                    self.state = OrbState::RecoverableError { message };
                }
            },
            OrbAction::ExternalVoiceStarted => {
                if matches!(
                    self.state,
                    OrbState::Armed
                        | OrbState::Heard { .. }
                        | OrbState::Connecting
                        | OrbState::Conversing(_)
                        | OrbState::Cooldown { .. }
                        | OrbState::HoldReady
                        | OrbState::RecoverableError { .. }
                ) {
                    self.state = OrbState::VoiceBusy;
                }
            },
            OrbAction::ExternalVoiceEnded => {
                if matches!(self.state, OrbState::VoiceBusy) {
                    self.state = OrbState::Armed;
                }
            },
            OrbAction::Pause { duration_ms } => {
                if self.state.window_is_open() {
                    self.state = OrbState::Paused {
                        until_ms: now_ms.saturating_add(duration_ms.max(1_000)),
                    };
                    self.leash_deadline_ms = None;
                    self.cap_expired_during_wake = false;
                }
            },
            OrbAction::Resume => {
                if matches!(self.state, OrbState::Paused { .. }) {
                    self.state = OrbState::Armed;
                    self.leash_deadline_ms = Some(now_ms.saturating_add(self.leash_ms));
                }
            },
            OrbAction::Disarm { reason } => {
                self.state = OrbState::Disarming { reason };
                self.leash_deadline_ms = None;
            },
            OrbAction::CompleteDisarm => {
                if let OrbState::Disarming { reason } = self.state.clone() {
                    self.state = OrbState::Ended { reason };
                    self.leash_deadline_ms = None;
                    self.cap_expired_during_wake = false;
                }
            },
            OrbAction::Tick => {
                if matches!(self.state, OrbState::Paused { until_ms } if until_ms <= now_ms) {
                    self.state = OrbState::Armed;
                    self.leash_deadline_ms = Some(now_ms.saturating_add(self.leash_ms));
                } else if matches!(self.state, OrbState::Cooldown { until_ms } if until_ms <= now_ms)
                {
                    self.state = OrbState::Armed;
                } else if self
                    .leash_deadline_ms
                    .is_some_and(|deadline| deadline <= now_ms)
                {
                    if matches!(
                        self.state,
                        OrbState::Heard { .. } | OrbState::Connecting | OrbState::Conversing(_)
                    ) {
                        self.cap_expired_during_wake = true;
                        self.leash_deadline_ms = None;
                    } else if self.state.window_is_open()
                        || matches!(self.state, OrbState::VoiceBusy)
                    {
                        self.state = OrbState::Disarming {
                            reason: OrbEndedReason::CapReached,
                        };
                        self.leash_deadline_ms = None;
                    }
                }
            },
        }
        let after = self.snapshot();
        if before != after {
            self.revision = self.revision.saturating_add(1);
            true
        } else {
            false
        }
    }
}

pub fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed(leash_ms: u64) -> OrbMachine {
        let mut machine = OrbMachine::default();
        machine.apply(
            OrbAction::Configure {
                leash_ms,
                cooldown_ms: 2_500,
                wake_phrase: "hey assistant".to_string(),
            },
            0,
        );
        machine.apply(OrbAction::Arm, 10);
        machine
    }

    #[test]
    fn phase_vocabulary_is_honest_and_palette_pinned() {
        let cases = [
            (OrbState::Armed, OrbPhase::Armed, "armed_ember"),
            (
                OrbState::Heard {
                    phrase: "hey magician".into(),
                },
                OrbPhase::Heard,
                "violet_surge",
            ),
            (
                OrbState::Conversing(OrbTurn::Listening),
                OrbPhase::Listening,
                "calm_aurora",
            ),
            (
                OrbState::Conversing(OrbTurn::Thinking),
                OrbPhase::Thinking,
                "amber_thinking",
            ),
            (
                OrbState::Conversing(OrbTurn::Speaking),
                OrbPhase::Speaking,
                "teal_speaking",
            ),
            (
                OrbState::Ended {
                    reason: OrbEndedReason::UserDisarm,
                },
                OrbPhase::Ended,
                "graphite",
            ),
        ];
        for (state, phase, palette) in cases {
            assert_eq!(state.phase(), Some(phase));
            assert_eq!(phase.palette_key(), palette);
        }
    }

    #[test]
    fn armed_status_exposes_the_configured_wake_phrase() {
        let machine = armed(10_000);
        assert_eq!(machine.snapshot().status, "Say “hey assistant”");
    }

    #[test]
    fn press_to_talk_parks_without_claiming_the_microphone_is_open() {
        let mut machine = OrbMachine::default();
        assert!(machine.apply(OrbAction::Arm, 0));
        assert_eq!(machine.snapshot().status, "Hold Left ⌥ to talk");
        assert!(machine.apply(
            OrbAction::WakeHeard {
                phrase: "Ready".to_string(),
            },
            10,
        ));
        assert!(machine.apply(OrbAction::Connected, 20));
        assert_eq!(machine.snapshot().state, "listening");
        assert!(machine.apply(OrbAction::HoldReady, 30));
        let parked = machine.snapshot();
        assert_eq!(parked.state, "hold_ready");
        assert_eq!(parked.phase, Some(OrbPhase::Armed));
        assert!(parked.window_open);
        assert_eq!(parked.status, "Hold Left ⌥ to talk");
        assert!(machine.apply(OrbAction::Turn(OrbTurn::Listening), 40));
        assert_eq!(machine.snapshot().state, "listening");
    }

    #[test]
    fn public_off_copy_uses_plain_resting_language() {
        assert_eq!(OrbState::Off.status(), "Resting");
        assert_eq!(OrbEndedReason::UserDisarm.message(), "Resting");
    }

    #[test]
    fn startup_arm_cannot_enter_conversation_without_an_explicit_wake() {
        let mut machine = OrbMachine::default();
        machine.apply(
            OrbAction::Configure {
                leash_ms: 10_000,
                cooldown_ms: 2_500,
                wake_phrase: "hey assistant".to_string(),
            },
            0,
        );
        machine.apply(OrbAction::Arm, 10);

        assert_eq!(machine.state(), &OrbState::Armed);
        assert_eq!(machine.snapshot().phase, Some(OrbPhase::Armed));

        machine.apply(
            OrbAction::WakeHeard {
                phrase: "hey assistant".to_string(),
            },
            20,
        );
        assert!(matches!(machine.state(), OrbState::Heard { .. }));
    }

    #[test]
    fn pause_discards_wake_hits_and_rearms_at_its_real_deadline() {
        let mut machine = armed(60_000);
        machine.apply(
            OrbAction::Pause {
                duration_ms: 3_600_000,
            },
            1_000,
        );
        machine.apply(
            OrbAction::WakeHeard {
                phrase: "hey magician".into(),
            },
            2_000,
        );
        assert!(matches!(machine.state(), OrbState::Paused { .. }));
        machine.apply(OrbAction::Tick, 3_600_999);
        assert!(matches!(machine.state(), OrbState::Paused { .. }));
        machine.apply(OrbAction::Tick, 3_601_000);
        assert_eq!(machine.state(), &OrbState::Armed);
    }

    #[test]
    fn leash_yields_to_an_in_flight_wake_then_collects_after_the_exchange() {
        let mut machine = armed(1_000);
        machine.apply(
            OrbAction::WakeHeard {
                phrase: "hey magician".into(),
            },
            900,
        );
        machine.apply(OrbAction::Connecting, 950);
        machine.apply(OrbAction::Tick, 1_010);
        assert!(matches!(machine.state(), OrbState::Connecting));
        assert!(machine.snapshot().cap_expired_during_wake);
        machine.apply(OrbAction::Connected, 1_100);
        machine.apply(OrbAction::Turn(OrbTurn::Speaking), 1_500);
        assert!(matches!(
            machine.state(),
            OrbState::Conversing(OrbTurn::Speaking)
        ));
        machine.apply(OrbAction::ConversationEnded, 2_000);
        assert!(matches!(
            machine.state(),
            OrbState::Disarming {
                reason: OrbEndedReason::CapReached
            }
        ));
    }

    #[test]
    fn ordinary_conversation_uses_a_known_cooldown_then_rearms() {
        let mut machine = armed(60_000);
        machine.apply(
            OrbAction::WakeHeard {
                phrase: "magician".into(),
            },
            100,
        );
        machine.apply(OrbAction::Connected, 200);
        machine.apply(OrbAction::ConversationEnded, 300);
        assert_eq!(machine.state().phase(), Some(OrbPhase::Listening));
        machine.apply(OrbAction::Tick, 2_799);
        assert!(matches!(machine.state(), OrbState::Cooldown { .. }));
        machine.apply(OrbAction::Tick, 2_800);
        assert_eq!(machine.state(), &OrbState::Armed);
    }

    #[test]
    fn disarm_is_total_across_every_lifecycle_state() {
        let states = vec![
            OrbState::Off,
            OrbState::Arming,
            OrbState::Armed,
            OrbState::Heard { phrase: "x".into() },
            OrbState::Connecting,
            OrbState::Conversing(OrbTurn::Listening),
            OrbState::Conversing(OrbTurn::Thinking),
            OrbState::Conversing(OrbTurn::Speaking),
            OrbState::Cooldown { until_ms: 10 },
            OrbState::VoiceBusy,
            OrbState::Paused { until_ms: 10 },
            OrbState::RecoverableError {
                message: "retrying".into(),
            },
        ];
        for state in states {
            let mut machine = OrbMachine {
                state,
                ..OrbMachine::default()
            };
            machine.apply(
                OrbAction::Disarm {
                    reason: OrbEndedReason::UserDisarm,
                },
                0,
            );
            assert!(matches!(
                machine.state(),
                OrbState::Disarming {
                    reason: OrbEndedReason::UserDisarm
                }
            ));
        }
    }

    #[test]
    fn snapshots_expose_only_deadlines_the_machine_actually_knows() {
        let mut machine = armed(10_000);
        assert_eq!(machine.snapshot().leash_deadline_ms, Some(10_010));
        assert_eq!(machine.snapshot().cooldown_until_ms, None);
        machine.apply(
            OrbAction::WakeHeard {
                phrase: "magician".into(),
            },
            20,
        );
        machine.apply(OrbAction::Connected, 30);
        machine.apply(OrbAction::Turn(OrbTurn::Speaking), 40);
        let speaking = machine.snapshot();
        assert_eq!(speaking.cooldown_until_ms, None);
        assert_eq!(speaking.paused_until_ms, None);
    }

    #[test]
    fn configuration_preserves_active_paused_and_disarming_lifecycle() {
        let mut active = armed(10_000);
        active.apply(
            OrbAction::WakeHeard {
                phrase: "magician".into(),
            },
            20,
        );
        active.apply(OrbAction::Connected, 30);
        let original_deadline = active.snapshot().leash_deadline_ms;
        active.apply(
            OrbAction::Configure {
                leash_ms: 60_000,
                cooldown_ms: 9_000,
                wake_phrase: "hey assistant".to_string(),
            },
            5_000,
        );
        active.apply(OrbAction::Arm, 5_001);
        assert!(matches!(active.state(), OrbState::Conversing(_)));
        assert_eq!(active.snapshot().leash_deadline_ms, original_deadline);

        let mut paused = armed(10_000);
        paused.apply(OrbAction::Pause { duration_ms: 5_000 }, 100);
        paused.apply(
            OrbAction::Configure {
                leash_ms: 20_000,
                cooldown_ms: 500,
                wake_phrase: "hey assistant".to_string(),
            },
            200,
        );
        paused.apply(OrbAction::Arm, 201);
        assert!(matches!(paused.state(), OrbState::Paused { .. }));

        let mut disarming = armed(10_000);
        disarming.apply(
            OrbAction::Disarm {
                reason: OrbEndedReason::UserDisarm,
            },
            100,
        );
        disarming.apply(OrbAction::Arm, 101);
        assert!(matches!(disarming.state(), OrbState::Disarming { .. }));
    }

    #[test]
    fn pause_cannot_rearm_off_or_ended_states() {
        let mut off = OrbMachine::default();
        assert!(!off.apply(OrbAction::Pause { duration_ms: 5_000 }, 0));
        assert_eq!(off.state(), &OrbState::Off);

        let mut ended = armed(10_000);
        ended.apply(
            OrbAction::Disarm {
                reason: OrbEndedReason::PowerPolicy,
            },
            10,
        );
        ended.apply(OrbAction::CompleteDisarm, 20);
        assert!(!ended.apply(OrbAction::Pause { duration_ms: 5_000 }, 30));
        assert!(!ended.apply(OrbAction::Resume, 40));
        assert!(matches!(
            ended.state(),
            OrbState::Ended {
                reason: OrbEndedReason::PowerPolicy
            }
        ));
    }

    #[test]
    fn ordinary_voice_capture_is_truthfully_projected_without_extending_the_lease() {
        let mut machine = armed(10_000);
        let deadline = machine.snapshot().leash_deadline_ms;
        machine.apply(OrbAction::ExternalVoiceStarted, 100);
        assert_eq!(machine.state(), &OrbState::VoiceBusy);
        assert_eq!(
            machine.snapshot().status,
            "Waiting while another voice session uses the microphone"
        );
        machine.apply(OrbAction::ExternalVoiceEnded, 200);
        assert_eq!(machine.state(), &OrbState::Armed);
        assert_eq!(machine.snapshot().leash_deadline_ms, deadline);

        let mut raced_wake = armed(10_000);
        raced_wake.apply(
            OrbAction::WakeHeard {
                phrase: "magician".into(),
            },
            50,
        );
        raced_wake.apply(OrbAction::Connecting, 60);
        raced_wake.apply(OrbAction::ExternalVoiceStarted, 70);
        assert_eq!(raced_wake.state(), &OrbState::VoiceBusy);

        let mut raced_conversation = armed(10_000);
        raced_conversation.apply(
            OrbAction::WakeHeard {
                phrase: "magician".into(),
            },
            50,
        );
        raced_conversation.apply(OrbAction::Connecting, 60);
        raced_conversation.apply(OrbAction::Connected, 70);
        raced_conversation.apply(OrbAction::ExternalVoiceStarted, 80);
        assert_eq!(raced_conversation.state(), &OrbState::VoiceBusy);
        // An old orb socket completing after the external lease starts cannot
        // overwrite the truthful busy state.
        assert!(!raced_conversation.apply(OrbAction::ConversationEnded, 90));
        assert_eq!(raced_conversation.state(), &OrbState::VoiceBusy);
    }

    #[test]
    fn every_arm_path_yields_to_an_existing_external_microphone_lease() {
        let mut initial_arm = OrbMachine::default();
        initial_arm.apply_with_external_voice(OrbAction::Arm, 10, true);
        assert_eq!(initial_arm.state(), &OrbState::VoiceBusy);
        assert_eq!(
            initial_arm.snapshot().leash_deadline_ms,
            Some(10 + DEFAULT_LEASH_MS)
        );

        let mut paused = armed(10_000);
        paused.apply(OrbAction::Pause { duration_ms: 1_000 }, 100);
        paused.apply_with_external_voice(OrbAction::Resume, 200, true);
        assert_eq!(paused.state(), &OrbState::VoiceBusy);

        let mut pause_expiry = armed(10_000);
        pause_expiry.apply(OrbAction::Pause { duration_ms: 1_000 }, 100);
        pause_expiry.apply_with_external_voice(OrbAction::Tick, 1_100, true);
        assert_eq!(pause_expiry.state(), &OrbState::VoiceBusy);
        pause_expiry.apply_with_external_voice(OrbAction::ExternalVoiceEnded, 1_101, false);
        assert_eq!(pause_expiry.state(), &OrbState::Armed);
    }

    #[test]
    fn sustained_transition_churn_is_safe_on_a_small_stack() {
        std::thread::Builder::new()
            .name("orb-small-stack-regression".to_string())
            .stack_size(64 * 1024)
            .spawn(|| {
                let mut machine = OrbMachine::default();
                machine.apply(
                    OrbAction::Configure {
                        leash_ms: 1_000_000_000,
                        cooldown_ms: 250,
                        wake_phrase: "hey assistant".to_string(),
                    },
                    0,
                );
                machine.apply(OrbAction::Arm, 0);
                for cycle in 0..100_000_u64 {
                    let now = cycle * 1_000;
                    machine.apply(
                        OrbAction::WakeHeard {
                            phrase: "hey magician".to_string(),
                        },
                        now,
                    );
                    machine.apply(OrbAction::Connecting, now);
                    machine.apply(OrbAction::Connected, now);
                    machine.apply(OrbAction::Turn(OrbTurn::Thinking), now);
                    machine.apply(OrbAction::Turn(OrbTurn::Speaking), now);
                    machine.apply(OrbAction::ConversationEnded, now);
                    machine.apply(OrbAction::Tick, now + 250);
                }
                assert_eq!(machine.snapshot().state, "armed");
            })
            .expect("small-stack regression thread must start")
            .join()
            .expect("orb transitions must not grow the call stack");
    }
}
