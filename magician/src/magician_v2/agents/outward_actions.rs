//! Which dispatches send something OUT of the owner's control, and what may be
//! done with them.
//!
//! Readiness review §9 step 2 — *"Restricted outward actions — email, WhatsApp,
//! calendar, form submission. `extra_args`, account overrides and raw actions
//! must be gone before anything autonomous can send"* — and step 4, *"Capture
//! mode and conservative caps — dry-run so the rest can be rehearsed"*. They are
//! one mechanism: you cannot rehearse what you have not first identified, and
//! identifying it is worthless if the identification is advisory.
//!
//! **Classified centrally, not per agent.** Each agent's `requires_approval`
//! list is true only for the agents someone remembered to edit — today
//! `executive-assistant` holds `gmail`, `whatsapp`, `telegram` and `calendar`
//! with no approval rule at all, while `swiggy-mcp` has one. A central
//! classification is true for every agent, including ones that do not exist yet,
//! which is the property that matters for a gate that runs before anything
//! autonomous may send.
//!
//! # Two tables, because "outward" has two shapes
//!
//! [`OUTWARD_CAPABILITIES`] holds capabilities whose *purpose* is to transmit:
//! a short list of sending verbs decides, and the generic escape hatches
//! (`execute`, an unbounded argv passthrough) correctly read as sends on them.
//!
//! [`SUBMITTING_CAPABILITIES`] holds capabilities that drive somebody else's
//! page. Step 2's fourth restricted act is *form submission*, and a browser is
//! how one happens — but a browser mostly reads, so its transmitting tokens are
//! named per-capability and the generic fallbacks deliberately do not apply.
//! One table with one action list cannot serve both: the verbs that identify a
//! send say nothing about a browser, and the fallbacks that are right for a
//! sender would classify every page read as an outward act.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::Value;

/// What kind of outward act a dispatch performs.
///
/// The distinction is not cosmetic: these differ in who receives them, whether
/// they can be retracted, and what a mistaken one costs. Mail and messages reach
/// a named person immediately and cannot be recalled; a calendar invite reaches
/// people and also writes to their schedule; a form submission reaches whoever
/// runs the site and is addressed to nobody by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutwardClass {
    /// Email, to a recipient outside the runtime.
    Mail,
    /// A chat message — WhatsApp, Telegram, iMessage.
    Message,
    /// A calendar invitation, which reaches attendees AND writes their schedule.
    CalendarInvite,
    /// Something typed into somebody else's page and sent to their server.
    ///
    /// The readiness review names *"form submission"* alongside email, WhatsApp
    /// and calendar, and it is the one of the four with no named recipient: an
    /// application lands with whoever runs the site. That is why it is a class
    /// of its own rather than a shape of [`Message`](Self::Message) — the
    /// recipient-screening gate reads the class to decide whether an empty
    /// recipient list is a parse failure or a fact, and a submission's audience
    /// is a site.
    FormSubmission,
}

impl OutwardClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Message => "message",
            Self::CalendarInvite => "calendar_invite",
            Self::FormSubmission => "form_submission",
        }
    }
}

/// Capabilities whose sending actions leave the owner's control, paired with the
/// action tokens that actually send.
///
/// A capability appears here only if it can transmit to a third party. Reading
/// counterparts (`agentmail-read`, `kapso-whatsapp-read`) are deliberately
/// absent: they are not outward acts, and sweeping them in would make the gate
/// so noisy that someone would turn it off.
const OUTWARD_CAPABILITIES: &[(&str, OutwardClass)] = &[
    ("gmail", OutwardClass::Mail),
    ("presto-gmail", OutwardClass::Mail),
    ("agentmail-send", OutwardClass::Mail),
    // ── The three chat channels, and the residual they still carry ──────────
    //
    // `whatsapp`, `telegram` and `telegram-self` transmit through ONE token the
    // list below matches: **`send`**, an explicit action each skill now declares
    // so the capability NAMES what it is doing. Their typed recipients are
    // respectively `jid`, `chat_id`, and `to`; provider message ids are lifted
    // into the result where the adapter can supply one.
    //
    // `run` is not classified wholesale: doing that would refuse reads such as
    // `chats list` and `getUpdates`. Each skill also exposes a `run` action
    // taking `command` (whatsapp, telegram-self) or `method`/`data` (telegram),
    // so `outward_dispatch_class` inspects those capability-specific command
    // heads and classifies only the transmitting forms — which means
    //
    //     whatsapp  run  command="messages send <jid> 'hi' --json"
    //     telegram  run  method=sendMessage  data="{\"chat_id\":…}"
    //
    // now enters the outward block and fails closed at the bindability gate.
    // The typed `send` action is the only live path. Reads remain unclassified,
    // avoiding the browser mistake of treating every opaque interaction as a
    // transmission just because some of them can transmit.
    //
    // `react` is a second, smaller one: `whatsapp react` puts an emoji into
    // somebody's chat, is a declared action, and is in neither list. It is left
    // unclassified deliberately — a reaction carries no message and no
    // recipient the gate could screen — and recorded so it is a decision rather
    // than an oversight.
    ("whatsapp", OutwardClass::Message),
    ("kapso-whatsapp-send", OutwardClass::Message),
    ("telegram", OutwardClass::Message),
    ("telegram-self", OutwardClass::Message),
    ("imessage_send", OutwardClass::Message),
    ("calendar", OutwardClass::CalendarInvite),
    ("presto-calendar", OutwardClass::CalendarInvite),
];

/// Action tokens that transmit. A capability in the table above is outward only
/// when its action is one of these — `gmail` reading an inbox is not an outward
/// act, and gating it would be false precision.
///
/// `execute` is included because a flat compiled tool with no action parameter
/// falls back to that token (see `pack_action_for_approval`); for a capability
/// whose whole purpose is sending — `imessage_send`, `agentmail-send` — the
/// bare invocation IS the send.
const SENDING_ACTIONS: &[&str] = &[
    "send",
    "send_message",
    "sendmessage",
    "send_email",
    "sendemail",
    "send_mail",
    "reply",
    "reply_all",
    "forward",
    "create_event",
    "invite",
    "execute",
    "*",
];

/// Browser tokens that can transmit what has been typed into somebody else's
/// page.
///
/// Read from the pinned `agent-browser` command reference and the inner loop's
/// own vocabularies, not guessed. Three facts shape the list:
///
/// - **There is no `submit` subcommand.** A form is submitted by clicking its
///   submit control, or by pressing Enter in a focused field. `submit` is
///   listed first anyway so a vocabulary that grows one does not arrive
///   ungated.
/// - **Staging input is not transmitting it.** `fill`, `type`, `check`,
///   `uncheck`, `select`, `focus` and `upload` put values into a page and send
///   nothing; they are absent for the same reason `gmail` reading an inbox is.
/// - **Two tokens carry another command inside them.** `find` takes a trailing
///   interaction (`find testid "submit-btn" click`) and `batch` takes a whole
///   command list, so neither can be judged from its own name. They remain an
///   explicitly recorded blind spot until the capability exposes a bindable
///   submission action; classifying either token wholesale would disable
///   ordinary browser use.
///
/// Everything a page is *read* with — `snapshot`, `get`, `is`, `screenshot`,
/// `pdf`, `scroll`, `hover`, `wait` — and everything it is *navigated* with —
/// `open`, `goto`, `navigate`, `back`, `forward`, `reload` — is deliberately
/// absent. Sweeping those in would gate ordinary reading, which is how a gate
/// gets switched off.
///
/// A click cannot tell a link from a submit control, so it is deliberately not
/// classified wholesale. This preserves ordinary navigation but leaves
/// submit-by-click as the honest limitation documented on
/// [`BROWSER_SUBMITTING_ACTIONS`] below.
///
/// # What this costs an autonomous run, stated plainly
///
/// A classified outward dispatch also meets §4A's bindability refusal, and
/// `browser` has **no restricted form** — its whole calling surface is argv, so
/// there is no sender to bind and no recipient to canonicalise. An autonomous
/// agent therefore cannot submit through a browser act this table CLASSIFIES,
/// until somebody decides what a bindable browser act looks like — which today
/// forbids nothing, because the table classifies no act the capability can
/// dispatch (see the limit recorded on the const itself). That is the same trade
/// `execution::restricted_action` already took for `calendar`, in its own
/// words: *"a 'restricted' form would be a fiction that refused every real
/// call. Until the skill models its parameters, an autonomous agent cannot send
/// a calendar invitation."* It is a real cost and it is deliberate: an agent
/// pressing Submit on somebody's application form in the owner's name, with no
/// record that it happened, is the failure the readiness review named.
///
/// Reading, navigating and clicking are untouched, so an autonomous run can
/// still use the web. Only an explicit submission is gated — see the limit
/// recorded on [`BROWSER_SUBMITTING_ACTIONS`], which is the price of that.
const BROWSER_SUBMITTING_ACTIONS: &[&str] = &[
    // The only tokens that MEAN submission. Everything else a browser does —
    // `click`, `press`, `key`, `eval`, `find`, `batch` and the Yutori spellings
    // of a click — is the ordinary vocabulary of reading the web, and a click is
    // how you follow a link far more often than it is how you submit a form.
    //
    // The first cut listed all of them, reasoning that Enter in a focused field
    // submits the form around it and the token does not say which key was
    // pressed. That reasoning is correct and its conclusion was not: applied to
    // a browser, fail-closed-on-every-verb gates ordinary browsing out of
    // existence, and a gate that refuses everything is a gate people route
    // around. See `the_gate_must_not_swallow_ordinary_browsing`.
    //
    // THE HONEST LIMIT, and it is larger than it looks: **the browser has no
    // token that means "submit" today.** A form is submitted by clicking its
    // submit control, so the two entries below match nothing the capability can
    // currently dispatch, and this table therefore sees NO browser submission at
    // all.
    //
    // That is stated plainly rather than hidden, because the alternative was
    // worse in the other direction. Classifying every interaction verb (`click`,
    // `press`, `key`, `eval`, `find`, `batch`) does catch the submission — and
    // also catches following a link, since a classified act meets §4A's
    // bindability refusal and `browser` has no restricted form. That refuses
    // every click an autonomous agent makes, and a gate that refuses everything
    // is one people route around.
    //
    // Neither end of that trade is acceptable as a resting place. Closing it
    // properly needs ONE of:
    //   * an explicit submit action on the browser capability, so the act names
    //     what it is doing and these entries start matching; or
    //   * a restricted form for `browser` that binds the target, so a classified
    //     click has a bindable shape and stops being an automatic refusal.
    // Until one exists, an undeclared submit-by-click reaches the world
    // unrecorded, and NOTHING stands in front of it. Capture is not it: the
    // whole outward block in `execute_action_inner` — the disclosure record,
    // the bindability refusal, the suppression check and the capture branch —
    // sits behind `classify_outward_dispatch`, which answers `None` here, so
    // the capture posture never sees the act to capture. The only control that
    // survives is the grant itself: an agent whose acts speak for somebody is
    // not given `browser` (see the ambassador's `denied_tools`).
    //
    // The entries stay so the seam is visible and the first of those two fixes
    // is a one-line change rather than a rediscovery.
    "submit",
    "submit_form",
];

/// Capabilities that drive somebody else's page, paired with the tokens that
/// can transmit through it.
///
/// Separate from [`OUTWARD_CAPABILITIES`] because the two tables answer
/// different questions. There, the capability's whole purpose is to transmit,
/// so one short list of sending verbs serves all of them and the generic
/// fallbacks (`execute`, an unbounded argv passthrough) correctly read as
/// sends. Here the capability's purpose is to *read the web*, and only a few of
/// its tokens leave anything behind.
///
/// Folding a browser into the other table would classify **every** browser
/// dispatch as outward twice over: through `execute`, which is the token a
/// dispatch with no action parameter falls back to, and through the argv
/// passthrough, because `args` is precisely how a browser subcommand is
/// spelled. That is the over-gating that would make the gate unusable, so the
/// action list here is per-capability and the passthrough rule deliberately
/// does not extend to it.
const SUBMITTING_CAPABILITIES: &[(&str, OutwardClass, &[&str])] = &[
    (
        "browser",
        OutwardClass::FormSubmission,
        BROWSER_SUBMITTING_ACTIONS,
    ),
    (
        "agent-browser",
        OutwardClass::FormSubmission,
        BROWSER_SUBMITTING_ACTIONS,
    ),
];

/// The capability a dispatch name addresses, as the approval gate reads it.
///
/// A compiled pack tool arrives as `pack__action` — `browser__click` is what
/// `ExecutableAction::Pack` carries and what the classifier is handed. Matching
/// the raw name against either table answers "not outward" for every one of
/// them, so the whole gate would be blind to compiled tools.
///
/// Delegates to [`super::approval::pack_tool_for_approval`] rather than
/// splitting on `__` here, for the same reason
/// [`dispatch_action_token`] delegates for the other half of the coordinate:
/// two functions that must agree about "which capability is this" and do not
/// share code will disagree eventually, and the disagreement would mean the
/// outward gate and the approval gate protect different sets.
fn capability_key(capability: &str) -> String {
    super::approval::pack_tool_for_approval(capability.trim())
        .trim()
        .to_ascii_lowercase()
}

/// Whether a capability action sends outward, and as what.
///
/// `None` means "not an outward act", which is the answer for every capability
/// in neither table and for reading actions on ones that are.
pub fn outward_action_class(capability: &str, action: &str) -> Option<OutwardClass> {
    let capability = capability_key(capability);
    let action = action.trim().to_ascii_lowercase();
    if let Some(class) = OUTWARD_CAPABILITIES
        .iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, class)| *class)
    {
        // A send-only capability sends whatever token it was invoked with; a
        // mixed-mode one (gmail reads and sends) must name a sending action.
        return (send_only_capability(&capability) || SENDING_ACTIONS.contains(&action.as_str()))
            .then_some(class);
    }
    // A page-driving capability names its own transmitting tokens: the generic
    // sending verbs say nothing about a browser, and the generic fallbacks
    // would classify every dispatch.
    let (_, class, submitting) = SUBMITTING_CAPABILITIES
        .iter()
        .find(|(name, _, _)| *name == capability)?;
    submitting.contains(&action.as_str()).then_some(*class)
}

/// Whether a capability can transmit to a third party AT ALL, whatever action it
/// is invoked with.
///
/// The question a toolset projection asks: before deciding what shape a tool may
/// take, it needs to know whether the tool reaches anybody. Asked through
/// [`capability_outward_class`] rather than by reading either table directly, so
/// this answer and the dispatch gate's cannot drift — a capability added to
/// either table becomes reachable here on the same edit.
///
/// Deliberately **not** probed with a canonical `send` token. `send` answers for
/// every sending capability and means nothing to a page-driving one, so a probe
/// would report a browser — which can submit a form — as reaching nobody, and a
/// projection that trusted it would hand out the unrestricted shape of the one
/// capability every agent holds.
pub fn capability_can_reach_the_world(capability: &str) -> bool {
    capability_outward_class(capability).is_some()
}

/// Whether opaque arguments themselves classify a capability as outward.
/// Shared by dispatch and catalog projection: action-classified capabilities
/// retain their ordinary argument surface until an outward action is named.
pub fn outward_passthrough_class(capability: &str) -> Option<OutwardClass> {
    let capability = capability_key(capability);
    OUTWARD_CAPABILITIES
        .iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, class)| *class)
}

/// Whether every invocation of a capability is a send.
///
/// Exposed because a caller holding only a capability NAME — a toolset
/// projection, an owner surface listing what an agent can do — cannot otherwise
/// tell `agentmail-send`, where the bare invocation IS the send, from `gmail`,
/// whose bare name stands for its reads as well.
pub fn capability_only_sends(capability: &str) -> bool {
    send_only_capability(&capability.trim().to_ascii_lowercase())
}

/// Capabilities that exist ONLY to transmit, so any invocation is a send. Split
/// out rather than folded into the action list because the distinction is about
/// the capability, not the token: adding a new action to `imessage_send` must
/// not silently create an ungated path.
fn send_only_capability(capability: &str) -> bool {
    matches!(
        capability,
        "imessage_send" | "agentmail-send" | "kapso-whatsapp-send"
    )
}

/// Whether a dispatch on `capability` reaches the world, judged from its
/// ARGUMENTS as well as its action token.
///
/// # The hole this closes
///
/// [`outward_action_class`] matches an action token against a list of sending
/// verbs. The shipped `gmail` and `presto-gmail` skills expose a `raw` action —
/// `fixed_args: [gmail]`, `mappings: [{type: passthrough, parameter: args}]` —
/// whose whole purpose is to run a command nobody modelled. `raw` is not a
/// sending verb, so token matching alone answered "not outward" for
///
/// ```text
/// gmail  action=raw  args=["+send", "--to", "someone@example.com", ...]
/// ```
///
/// which sends an email while bypassing the disclosure record, the capture
/// posture, the restricted-action refusal and every envelope. The skill's own
/// text tells the agent to reach for it: *"Use `raw` only for a Gmail CLI
/// command not modeled as an action yet."*
///
/// # The rule
///
/// If a capability CAN reach the world, and this dispatch carries an unbounded
/// argument passthrough, then it is an outward act whatever its token says. The
/// passthrough test is
/// [`crate::magician_v2::execution::effective_action::is_passthrough_parameter`],
/// the same one the restriction and effective-action primitives use, so there is
/// one answer to "what is an escape hatch" across the whole control plane.
///
/// This deliberately over-gates: a `triage` invocation that happens to use
/// `extra_args` is classified outward and captured. That costs a capture notice
/// on a read. The other direction costs a send nobody recorded.
///
/// # Why the passthrough rule stops at the sending capabilities
///
/// It is scoped to [`OUTWARD_CAPABILITIES`] and does **not** extend to a
/// page-driving capability. There, an argv array is the escape hatch around a
/// modelled action; on a browser an argv array *is* the ordinary calling
/// convention — every `snapshot`, every `get` carries one. Extending the rule
/// would classify reading a page as an outward act, which is the noise that
/// gets a gate switched off rather than a hole that gets one bypassed. A
/// browser's future explicit submission tokens are named in
/// [`BROWSER_SUBMITTING_ACTIONS`] instead. Its current `batch`, `find`, `click`,
/// and key actions are intentionally not classified wholesale; the limitation
/// and the required capability-level fix are documented on that table.
pub fn outward_dispatch_class(
    capability: &str,
    action: &str,
    resolved_params: &HashMap<String, Value>,
) -> Option<OutwardClass> {
    if let Some(class) = outward_action_class(capability, action) {
        return Some(class);
    }

    let capability_key = capability_key(capability);
    let class = outward_passthrough_class(&capability_key)?;

    // These three mixed-mode chat skills retain an opaque diagnostic `run`
    // action for reads. Inspect only the command/method head that the fixed
    // adapter executable will parse, and classify transmitting forms. They
    // then reach the ordinary outward bindability gate, which refuses the
    // opaque form and directs the caller to the typed `send` action. This is a
    // deny rule, not an attempt to extract authority from free text.
    if opaque_chat_run_transmits(&capability_key, action, resolved_params) {
        return Some(class);
    }

    let carries_passthrough = resolved_params
        .keys()
        .any(|key| crate::magician_v2::execution::effective_action::is_passthrough_parameter(key));
    carries_passthrough.then_some(class)
}

fn opaque_chat_run_transmits(
    capability: &str,
    action: &str,
    resolved_params: &HashMap<String, Value>,
) -> bool {
    if !action.trim().eq_ignore_ascii_case("run") {
        return false;
    }
    match capability {
        "telegram" => resolved_params
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .is_some_and(|method| {
                method.starts_with("send")
                    || method.starts_with("forward")
                    || method.starts_with("copy")
            }),
        "whatsapp" => command_starts_with(
            resolved_params.get("command").and_then(Value::as_str),
            &[["messages", "send"], ["media", "send"]],
        ),
        "telegram-self" => command_starts_with(
            resolved_params.get("command").and_then(Value::as_str),
            &[["send", "text"], ["send", "photo"], ["send", "file"]],
        ),
        _ => false,
    }
}

fn command_starts_with<const N: usize>(command: Option<&str>, heads: &[[&str; N]]) -> bool {
    let Some(command) = command else {
        return false;
    };
    let tokens = command.split_whitespace().take(N).collect::<Vec<_>>();
    tokens.len() == N
        && heads.iter().any(|head| {
            tokens
                .iter()
                .zip(head.iter())
                .all(|(token, expected)| token.eq_ignore_ascii_case(expected))
        })
}

/// What the runtime should do with a dispatch, once classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutwardDisposition {
    /// Not an outward act. Dispatch normally; this gate has no opinion.
    NotOutward,
    /// Outward, and capture mode is on: record what WOULD have been sent and do
    /// not send it. This is what makes the rest of the system rehearsable.
    Capture(OutwardClass),
    /// Outward and live. The gate permits it; other gates (approval,
    /// engagement ceiling) still apply and are not weakened by this one.
    Live(OutwardClass),
}

/// Classify a dispatch and decide its disposition under the current capture
/// setting.
///
/// `capture_only` is expected to default to **true**. An operator turns sending
/// on deliberately, once; nobody turns it off by forgetting. That direction is
/// the whole point of the step — the review's own framing is that these are the
/// difference between a design that can be developed and one that can only be
/// launched.
///
/// Argument-aware, via [`outward_dispatch_class`] rather than
/// [`outward_action_class`]: the disposition must agree with whatever
/// classified this dispatch as outward in the first place (including the
/// passthrough-argv case), or a dispatch recorded and captured/refused as
/// outward by the classifier could still read as `NotOutward` here and skip
/// the capture branch entirely.
pub fn outward_disposition(
    capability: &str,
    action: &str,
    resolved_params: &HashMap<String, Value>,
    capture_only: bool,
) -> OutwardDisposition {
    match outward_dispatch_class(capability, action, resolved_params) {
        None => OutwardDisposition::NotOutward,
        Some(class) if capture_only => OutwardDisposition::Capture(class),
        Some(class) => OutwardDisposition::Live(class),
    }
}

/// Every outward capability an agent's grant contains, with its class.
///
/// Readiness review §9 step 7 — *"pause every outward agent individually"* —
/// is only actionable if the set can be named. Per-agent pause already exists
/// (`pause_agent`); what did not exist was any way to answer "which agents are
/// outward?", so the step reduced to remembering, which is not a control.
///
/// Derived from the same table the dispatch gate uses, so the two cannot
/// disagree about what "outward" means. An agent that gains a sending tool
/// becomes outward here automatically — including one added after this was
/// written, which is the case a hand-maintained list gets wrong.
///
/// `denied_tools` is subtracted: a denied capability is not reachable, so an
/// agent denied every sender is not outward however its grant reads.
pub fn agent_outward_capabilities(
    tools: &[String],
    denied_tools: &[String],
) -> Vec<(String, OutwardClass)> {
    let mut found: Vec<(String, OutwardClass)> = Vec::new();
    for tool in tools {
        if denied_tools.iter().any(|denied| denied == tool) {
            continue;
        }
        // A grant names a capability, not an action, so ask whether ANY action
        // on it would be outward. Asked of both tables rather than by probing
        // one canonical verb: `send` answers for every sending capability but
        // means nothing to a browser, and probing with it alone would report an
        // agent that can submit forms as having no outward reach at all — the
        // per-agent pause of §9 step 7 would then not list the agent it most
        // needs to.
        if let Some(class) = capability_outward_class(tool) {
            found.push((tool.clone(), class));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

/// Whether this grant can reach a third party at all.
pub fn agent_is_outward(tools: &[String], denied_tools: &[String]) -> bool {
    !agent_outward_capabilities(tools, denied_tools).is_empty()
}

/// Whether a capability can reach the world through **any** of its actions.
///
/// The capability-level question, which is what a grant asks. Distinct from
/// [`outward_action_class`], which answers about one dispatch: a grant of
/// `gmail` is outward reach even though most gmail actions read, and a grant of
/// `browser` is outward reach even though most browser actions read.
pub fn capability_outward_class(capability: &str) -> Option<OutwardClass> {
    let capability = capability_key(capability);
    if let Some(class) = OUTWARD_CAPABILITIES
        .iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, class)| *class)
    {
        return Some(class);
    }
    SUBMITTING_CAPABILITIES
        .iter()
        .find(|(name, _, _)| *name == capability)
        .map(|(_, class, _)| *class)
}

/// Process-wide outward posture, installed once at boot from config.
///
/// A `OnceLock` rather than a config handle threaded through the executor,
/// matching how `GLOBAL_ENGAGEMENTS` reaches the same dispatch path: the gate
/// has to be readable from deep inside action execution, and a value that can
/// only be set once cannot be flipped mid-run by anything the model does.
static CAPTURE_ONLY: OnceLock<bool> = OnceLock::new();

/// Install the posture at boot. Returns `false` if one was already installed —
/// the first call wins, so a later caller cannot quietly enable sending.
pub fn install_outward_capture_posture(capture_only: bool) -> bool {
    CAPTURE_ONLY.set(capture_only).is_ok()
}

/// The posture in force.
///
/// **Defaults to capture when nothing was installed.** A process that failed to
/// wire its config — a test, a partially-initialised binary, a future entry
/// point nobody remembered to update — must not therefore be able to send. The
/// direction of that default is the entire safety property; inverting it would
/// make every unconfigured path a live sender.
pub fn outward_capture_only() -> bool {
    *CAPTURE_ONLY.get().unwrap_or(&true)
}

/// The text a captured outward action returns to the model.
///
/// It must read as "this did not happen". A capture that returns something
/// resembling success teaches the model it sent the mail, and it will then say
/// so to the owner — which is worse than refusing outright, because the owner
/// believes a message went out.
pub fn captured_dispatch_notice(capability: &str, action: &str, class: OutwardClass) -> String {
    format!(
        "NOT SENT — captured. Outward capture mode is on, so this {} action \
         (`{capability}` / `{action}`) was recorded and deliberately NOT \
         performed. Nothing reached any recipient. Report it as prepared and \
         awaiting a real send; do not state or imply that it was sent.",
        class.as_str()
    )
}

/// The action token for a capability dispatch, matching how the approval gate
/// derives the same coordinate.
///
/// Deliberately delegates to `approval::pack_action_for_approval` rather than
/// re-deriving: two functions that must agree about "which action is this" and
/// do not share code will disagree eventually, and the disagreement would mean
/// the outward gate and the approval gate protect different sets.
pub fn dispatch_action_token(capability: &str, resolved_params: &HashMap<String, Value>) -> String {
    super::approval::pack_action_for_approval(capability, resolved_params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    /// The bypass that token-only classification let through.
    ///
    /// The shipped `gmail` skill exposes `raw` — `fixed_args: [gmail]`, a pure
    /// `args` passthrough — described as *"Run an unmodeled Gmail command using
    /// exact bounded argv tokens."* `raw` is not a sending verb, so the action
    /// token alone said "not outward" for a dispatch that sends an email, and it
    /// bypassed the disclosure record, the capture posture, restriction and every
    /// envelope at once.
    #[test]
    fn a_raw_passthrough_on_a_mail_capability_is_still_an_outward_act() {
        let sending_argv = params(&[(
            "args",
            serde_json::json!(["+send", "--to", "someone@example.com", "--subject", "hi"]),
        )]);

        assert_eq!(
            outward_action_class("gmail", "raw"),
            None,
            "token matching alone cannot see this, which is why the gate must not rely on it"
        );
        assert_eq!(
            outward_dispatch_class("gmail", "raw", &sending_argv),
            Some(OutwardClass::Mail),
            "an unbounded argv passthrough on a mail capability can send, so it is outward"
        );
        // `presto-gmail` ships the same escape action.
        assert_eq!(
            outward_dispatch_class("presto-gmail", "raw", &sending_argv),
            Some(OutwardClass::Mail)
        );
    }

    /// The rule is about capabilities that CAN reach the world, so a passthrough
    /// on ordinary local work stays ungated. Otherwise every tool taking
    /// `extra_args` would be captured and someone would switch the gate off.
    #[test]
    fn a_passthrough_on_a_local_capability_is_not_outward() {
        let argv = params(&[("args", serde_json::json!(["--anything"]))]);
        assert_eq!(outward_dispatch_class("websearch", "raw", &argv), None);
        assert_eq!(outward_dispatch_class("jq", "execute", &argv), None);
        assert_eq!(outward_dispatch_class("duckdb", "query", &argv), None);
    }

    /// Without a passthrough, a non-sending action on a mail capability is still
    /// a read. Over-gating every gmail call would be false precision.
    #[test]
    fn a_plain_read_on_a_mail_capability_stays_ungated() {
        let read = params(&[("query", serde_json::json!("is:unread"))]);
        assert_eq!(
            outward_dispatch_class("gmail", "messages_list", &read),
            None
        );
        assert_eq!(outward_dispatch_class("gmail", "triage", &read), None);
    }

    /// Every passthrough spelling the effective-action primitive knows must also
    /// trip this gate. Two lists of "what is an escape hatch" would drift, and
    /// the drift would be a silent hole rather than a failing test.
    #[test]
    fn every_known_passthrough_spelling_trips_the_gate() {
        for spelling in ["args", "extra_args", "extra_arguments", "raw_args", "argv"] {
            let argv = params(&[(spelling, serde_json::json!(["+send", "--to", "x@y.z"]))]);
            assert_eq!(
                outward_dispatch_class("gmail", "raw", &argv),
                Some(OutwardClass::Mail),
                "`{spelling}` is a passthrough to effective_action but not to this gate"
            );
        }
    }

    #[test]
    fn the_capabilities_that_can_reach_a_third_party_are_classified() {
        assert_eq!(
            outward_action_class("gmail", "send"),
            Some(OutwardClass::Mail)
        );
        assert_eq!(
            outward_action_class("whatsapp", "send_message"),
            Some(OutwardClass::Message)
        );
        assert_eq!(
            outward_action_class("telegram", "sendMessage"),
            Some(OutwardClass::Message),
            "action matching must not be case-sensitive; providers name actions \
             in their own style"
        );
        assert_eq!(
            outward_action_class("calendar", "create_event"),
            Some(OutwardClass::CalendarInvite)
        );
    }

    /// The three chat channels are outward through `send`, in every shape the
    /// classifier is handed one.
    ///
    /// Before an explicit `send` action existed, each of these exposed only a
    /// free-text `run` — so `outward_dispatch_class` answered `None` for every
    /// dispatch they could make, and the entire outward block in
    /// `execute_action_inner` (disclosure record, bindability refusal,
    /// suppression screen, capture) sat behind an answer that never came.
    #[test]
    fn a_chat_channel_is_outward_through_its_explicit_send_action() {
        for capability in ["whatsapp", "telegram", "telegram-self"] {
            assert_eq!(
                outward_action_class(capability, "send"),
                Some(OutwardClass::Message),
                "{capability}/send reaches a named person"
            );
            // The shape `ExecutableAction::Pack` actually carries.
            assert_eq!(
                outward_action_class(&format!("{capability}__send"), "send"),
                Some(OutwardClass::Message),
                "{capability}__send is how a compiled leaf arrives"
            );
        }

        // With the arguments a real WhatsApp send carries, through the
        // argument-aware entry point the gate actually calls.
        let send = params(&[
            ("action", serde_json::json!("send")),
            ("jid", serde_json::json!("919876543210@s.whatsapp.net")),
            ("text", serde_json::json!("on my way")),
        ]);
        assert_eq!(
            outward_dispatch_class("whatsapp", "send", &send),
            Some(OutwardClass::Message)
        );
    }

    /// An opaque `run` send is detected narrowly and refused by the ordinary
    /// bindability gate; reads through the same action stay usable.
    ///
    /// `whatsapp run command="messages send <jid> 'hi' --json"` and
    /// `telegram run method=sendMessage` both reach a real person. Neither
    /// `command` nor `method` is a generic passthrough parameter name, so these
    /// capability-specific heads are the necessary structural deny rule.
    #[test]
    fn a_send_composed_inside_run_is_classified_without_classifying_reads() {
        let whatsapp = params(&[
            ("action", serde_json::json!("run")),
            (
                "command",
                serde_json::json!("messages send 919876543210@s.whatsapp.net 'hi' --json"),
            ),
        ]);
        assert_eq!(
            outward_dispatch_class("whatsapp", "run", &whatsapp),
            Some(OutwardClass::Message)
        );

        let telegram = params(&[
            ("action", serde_json::json!("run")),
            ("method", serde_json::json!("sendMessage")),
            ("data", serde_json::json!(r#"{"chat_id":123,"text":"hi"}"#)),
        ]);
        assert_eq!(
            outward_dispatch_class("telegram", "run", &telegram),
            Some(OutwardClass::Message)
        );

        let telegram_self = params(&[
            ("action", serde_json::json!("run")),
            (
                "command",
                serde_json::json!("send text --to @alice --message 'hi'"),
            ),
        ]);
        assert_eq!(
            outward_dispatch_class("telegram-self", "run", &telegram_self),
            Some(OutwardClass::Message)
        );

        for method in ["sendPhoto", "forwardMessage", "copyMessage"] {
            let telegram_media = params(&[("method", serde_json::json!(method))]);
            assert_eq!(
                outward_dispatch_class("telegram", "run", &telegram_media),
                Some(OutwardClass::Message),
                "{method} also publishes into a third-party chat"
            );
        }
    }

    /// The precision half: reading through these channels stays ungated, which
    /// is the reason `run` is not classified.
    #[test]
    fn reading_through_a_chat_channel_stays_ungated() {
        for (capability, command) in [
            ("whatsapp", "chats list --json"),
            ("whatsapp", "contacts search 'ana' --json"),
            ("telegram-self", "channels"),
        ] {
            let reading = params(&[("command", serde_json::json!(command))]);
            assert_eq!(
                outward_dispatch_class(capability, "run", &reading),
                None,
                "{capability} reading `{command}` is not a transmission"
            );
        }
        // A reaction puts an emoji in somebody's chat and is deliberately not
        // classified — recorded on the table so it is a decision, not an
        // oversight.
        assert_eq!(outward_action_class("whatsapp", "react"), None);
    }

    /// A compiled pack tool arrives as `pack__action`, and matching the raw
    /// name found nothing in the table.
    ///
    /// `ExecutableAction::Pack` carries `gmail__send` and the classifier is
    /// handed that string verbatim, so a lookup on the unsplit name answered
    /// "not outward" for a compiled sender — while the approval gate, which
    /// does split it, saw `gmail`/`send`. The two gates protected different
    /// sets, and this is the one that decides whether anything is recorded.
    #[test]
    fn a_compiled_pack_name_resolves_to_the_capability_it_names() {
        assert_eq!(
            outward_action_class("gmail__send", "send"),
            Some(OutwardClass::Mail)
        );
        assert_eq!(
            outward_action_class("calendar__create_event", "create_event"),
            Some(OutwardClass::CalendarInvite)
        );
        // Splitting must not invent reach: the read counterpart stays a read.
        assert_eq!(
            outward_action_class("gmail__messages_list", "messages_list"),
            None
        );
    }

    /// A send-only capability sends whatever it was invoked with. Pinned
    /// separately because the failure it prevents is silent: adding a new action
    /// token to `imessage_send` must not create a path the gate does not see.
    #[test]
    fn a_send_only_capability_is_outward_whatever_action_it_names() {
        for action in ["execute", "send", "some_new_action_nobody_added_here", ""] {
            assert_eq!(
                outward_action_class("imessage_send", action),
                Some(OutwardClass::Message),
                "imessage_send/{action} escaped the outward gate"
            );
        }
    }

    /// The acknowledged gap: a browser can submit a form, and the classifier
    /// answered `None` for every way of doing it.
    ///
    /// Readiness review §9 step 2 names *"form submission"* as a restricted
    /// outward act alongside email, WhatsApp and calendar. `browser` was in
    /// neither table, so pressing Submit on somebody's application form reached
    /// no disclosure record, no capture posture and no envelope — the same
    /// bypass `gmail action=raw` was, through a capability every agent holds.
    #[test]
    fn submitting_a_form_through_the_browser_is_an_outward_act() {
        for action in ["submit", "submit_form"] {
            assert_eq!(
                outward_action_class("browser", action),
                Some(OutwardClass::FormSubmission),
                "browser/{action} sends a form and escaped the gate"
            );
        }
        // A compiled pack tool arrives as `pack__action`, which is the shape
        // `ExecutableAction::Pack` actually carries for the browser.
        assert_eq!(
            outward_action_class("browser__submit", "submit"),
            Some(OutwardClass::FormSubmission)
        );
    }

    /// The gate must not swallow ordinary browsing.
    ///
    /// The first cut classified `click`, `press`, `key`, `eval`, `find` and
    /// `batch` as outward, reasoning that any of them CAN submit a form. Each
    /// step of that reasoning is true and the conclusion broke the browser: a
    /// classified act meets §4A's bindability refusal, `browser` has no
    /// restricted form, so every click an autonomous agent made would have been
    /// refused — and following a link is a click. A gate that refuses
    /// everything is a gate people route around.
    ///
    /// The honest limit is recorded on `BROWSER_SUBMITTING_ACTIONS`: a click on
    /// a submit control is outward and this table cannot see it. The answer is
    /// for the capability to name what it is doing, not for the gate to guess
    /// from a verb that means both things.
    #[test]
    fn the_gate_must_not_swallow_ordinary_browsing() {
        for action in [
            "click",
            "dblclick",
            "tap",
            "press",
            "key",
            "eval",
            "execute_js",
            "find",
            "batch",
            "snapshot",
            "goto",
            "left_click",
        ] {
            assert_eq!(
                outward_action_class("browser", action),
                None,
                "browser/{action} is how an agent reads the web, not how it submits"
            );
        }
    }

    /// THE KNOWN GAP, pinned so it stays visible: a browser can submit without
    /// naming a submitting token, and this table does not see it.
    ///
    /// `find testid "submit-btn" click` and `batch -- 'fill @e2 x' 'click @e5'`
    /// both press Submit while naming a token that is not an interaction at
    /// all — the `gmail action=raw` shape again. So does a bare `click` on a
    /// submit control.
    ///
    /// An earlier cut closed this by classifying every one of those tokens.
    /// That is where the reasoning has to stop and judgement start: those verbs
    /// are also how an agent follows a link, presses Enter in a search box and
    /// batches two page reads, and a classified act meets §4A's bindability
    /// refusal — so the gate would have refused ordinary browsing entirely.
    /// Gating everything and gating nothing fail the same way, because a gate
    /// that refuses every call gets switched off.
    ///
    /// The gap closes when a browser act NAMES what it is doing — an explicit
    /// submit action, which the form-filling competence is written against — or
    /// when `browser` grows a restricted form that binds the target. Neither is
    /// a table entry, so this test asserts today's truth rather than a wish.
    #[test]
    fn a_command_carrying_token_can_still_submit_unseen() {
        for token in ["find", "batch", "click"] {
            assert_eq!(
                outward_action_class("browser", token),
                None,
                "browser/{token} is unclassified — a submit hidden inside it is the recorded gap"
            );
        }
    }

    /// The precision half. Reading and navigating a page must stay ungated, or
    /// the gate is on for every browser call and somebody switches it off.
    ///
    /// Staging input is in the same set deliberately: `fill` and `type` put a
    /// value into a field and transmit nothing. The act that leaves is the
    /// click on Submit, and that is the one classified.
    #[test]
    fn reading_and_staging_through_a_browser_stay_ungated() {
        for action in [
            "snapshot",
            "get",
            "is",
            "screenshot",
            "pdf",
            "scroll",
            "hover",
            "wait",
            "open",
            "goto",
            "navigate",
            "back",
            "forward",
            "reload",
            "fill",
            "type",
            "check",
            "uncheck",
            "select",
            "focus",
            "upload",
            "execute",
        ] {
            assert_eq!(
                outward_action_class("browser", action),
                None,
                "browser/{action} is not a transmission and gating it would make the gate noise"
            );
        }
    }

    /// A browser's argv is its ordinary calling convention, not an escape
    /// hatch, so the passthrough rule must not reach it.
    ///
    /// `snapshot` carries `args: ["-i"]`. If the passthrough rule that catches
    /// `gmail action=raw` extended to page-driving capabilities, every read of
    /// every page would classify as an outward act and be captured — which is
    /// the failure mode the module warns about, in the opposite direction from
    /// the one it usually guards.
    #[test]
    fn a_browser_argv_alone_does_not_make_a_read_outward() {
        let reading = params(&[("args", serde_json::json!(["-i"]))]);
        assert_eq!(
            outward_dispatch_class("browser", "snapshot", &reading),
            None
        );
        // The same argv on a submitting token is still outward — via the token,
        // not via the passthrough.
        let submitting = params(&[("args", serde_json::json!(["@e5"]))]);
        assert_eq!(
            outward_dispatch_class("browser", "submit", &submitting),
            Some(OutwardClass::FormSubmission)
        );
    }

    /// A submission is not bounded communication, and the difference decides
    /// whether a standing envelope can cover it.
    ///
    /// Bounded communication is the one class a standing envelope may cover. If
    /// a form submission were filed there, a blanket "you may talk to people in
    /// this engagement" consent would also authorise pressing Submit on an
    /// application the owner never saw.
    #[test]
    fn a_form_submission_is_a_submission_not_a_conversation() {
        use crate::magician_v2::agents::consequence_class::{
            consequence_class_for, ConsequenceClass,
        };
        assert_eq!(
            consequence_class_for("browser", "submit"),
            ConsequenceClass::SubmissionOrPublication
        );
        assert!(!ConsequenceClass::SubmissionOrPublication.standing_envelope_may_cover());
        assert!(ConsequenceClass::SubmissionOrPublication.requires_gate());
        // Reading a page is still local work with no gate — the classification
        // must not have turned the browser into a permanently gated capability.
        assert_eq!(
            consequence_class_for("browser", "snapshot"),
            ConsequenceClass::PrivateLocal
        );
    }

    /// An agent holding a browser has outward reach, and §9 step 7 cannot pause
    /// what it cannot name.
    ///
    /// The grant probe used to ask `outward_action_class(tool, "send")`. `send`
    /// means nothing to a browser, so an agent that could submit forms reported
    /// no outward reach at all — the per-agent pause would have skipped exactly
    /// the agent it most needed to list.
    #[test]
    fn a_grant_that_holds_a_browser_reports_outward_reach() {
        let tools: Vec<String> = ["browser", "websearch", "jq"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(
            agent_outward_capabilities(&tools, &[]),
            vec![("browser".to_string(), OutwardClass::FormSubmission)]
        );
        assert!(agent_is_outward(&tools, &[]));
        // Denying it removes the reach, exactly as for a sender.
        let denied: Vec<String> = ["browser".to_string()].into();
        assert!(!agent_is_outward(&tools, &denied));
    }

    /// Reading is not an outward act, and gating it would make the mechanism so
    /// noisy that someone would switch it off — which is how a safety gate
    /// actually fails.
    #[test]
    fn reading_is_not_an_outward_act() {
        assert_eq!(outward_action_class("gmail", "list_messages"), None);
        assert_eq!(outward_action_class("gmail", "read"), None);
        assert_eq!(outward_action_class("agentmail-read", "read"), None);
        assert_eq!(outward_action_class("kapso-whatsapp-read", "list"), None);
        assert_eq!(outward_action_class("websearch", "search"), None);
    }

    /// Capture is the default posture. An operator enables sending once,
    /// deliberately; nobody enables it by forgetting to set a flag.
    #[test]
    fn capture_is_what_happens_unless_sending_was_turned_on() {
        let no_params = HashMap::new();
        assert_eq!(
            outward_disposition("gmail", "send", &no_params, true),
            OutwardDisposition::Capture(OutwardClass::Mail)
        );
        assert_eq!(
            outward_disposition("gmail", "send", &no_params, false),
            OutwardDisposition::Live(OutwardClass::Mail)
        );
        // A non-outward action is untouched in either posture: this gate must
        // not become a general kill switch, or it will be turned off wholesale.
        assert_eq!(
            outward_disposition("websearch", "search", &no_params, true),
            OutwardDisposition::NotOutward
        );
        assert_eq!(
            outward_disposition("websearch", "search", &no_params, false),
            OutwardDisposition::NotOutward
        );
    }

    /// The default posture is capture, and that direction is the safety
    /// property. A process that never wired its config must not be able to send.
    #[test]
    fn an_uninstalled_posture_captures_rather_than_sends() {
        // `outward_capture_only` reads a process-global that this test does not
        // install, so it exercises the uninstalled path directly.
        assert!(
            outward_capture_only(),
            "an unconfigured process could SEND; the default must be capture"
        );
    }

    /// A capture must not read like a success. If the model believes the mail
    /// went out it will tell the owner so, which is worse than a refusal.
    #[test]
    fn a_capture_notice_cannot_be_mistaken_for_a_send() {
        let notice = captured_dispatch_notice("gmail", "send", OutwardClass::Mail);
        assert!(notice.contains("NOT SENT"));
        assert!(notice.to_ascii_lowercase().contains("captured"));
        assert!(
            notice.to_ascii_lowercase().contains("do not state"),
            "the notice must tell the model not to claim it was sent: {notice}"
        );
    }

    /// An agent is outward when its GRANT can reach a third party, and denying
    /// a capability removes it. Both halves matter for §9 step 7: the set has to
    /// be derivable, and it has to reflect denials or `company-assistant` would
    /// be listed as a messaging agent on the strength of tools it cannot call.
    #[test]
    fn an_agents_outward_reach_follows_its_grant_minus_its_denials() {
        let tools: Vec<String> = ["gmail", "websearch", "whatsapp", "jq"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let found = agent_outward_capabilities(&tools, &[]);
        assert_eq!(
            found,
            vec![
                ("gmail".to_string(), OutwardClass::Mail),
                ("whatsapp".to_string(), OutwardClass::Message),
            ]
        );
        assert!(agent_is_outward(&tools, &[]));

        // Denying the senders removes the reach.
        let denied: Vec<String> = ["gmail".to_string(), "whatsapp".to_string()].into();
        assert!(
            agent_outward_capabilities(&tools, &denied).is_empty(),
            "a denied capability is not reachable and must not count as outward"
        );
        assert!(!agent_is_outward(&tools, &denied));

        // A research-only grant is not outward.
        let inert: Vec<String> = ["websearch".to_string(), "jq".to_string()].into();
        assert!(!agent_is_outward(&inert, &[]));
    }

    /// Every capability the shipped `executive-assistant` holds that can reach a
    /// third party must be classified. This is the concrete gap §9 step 2 names:
    /// that agent holds gmail, whatsapp, telegram and calendar today with NO
    /// approval rule, so before this gate existed an autonomous officer could
    /// delegate to it and send with nothing in the way.
    #[test]
    fn the_executive_assistants_sending_grant_is_covered() {
        for (capability, action) in [
            ("gmail", "send"),
            ("whatsapp", "send"),
            ("telegram", "send"),
            ("calendar", "create_event"),
            ("imessage_send", "execute"),
        ] {
            assert!(
                outward_action_class(capability, action).is_some(),
                "{capability}/{action} is grantable today and is not classified \
                 as outward, so the capture gate would let it send"
            );
        }
    }
}
