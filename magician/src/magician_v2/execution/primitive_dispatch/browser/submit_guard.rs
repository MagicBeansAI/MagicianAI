//! Browser form-submission guard.
//!
//! The browser has no command that means "submit": a form is submitted by
//! clicking its submit control or pressing Enter in one of its fields. The
//! outward table (`agents::outward_actions::BROWSER_SUBMITTING_ACTIONS`) can
//! therefore classify no browser submission, so a click that sent somebody's
//! application went out unrecorded and ungated. Classifying every click would
//! gate ordinary browsing out of existence.
//!
//! This guard closes the gap where the act happens. Before an interaction
//! command runs, it inspects the element the command would hit and refuses the
//! command when that is a submission:
//!
//! * a submit control of a form that does not use GET — a GET form is a
//!   search or a filter, i.e. navigation;
//! * any control whose visible label is a final action ("Submit", "Send
//!   application", "Place order", "Pay", "Publish", "Sign up", …) — how
//!   script-driven forms (Typeform-style) submit, with no `<form>` at all;
//! * Enter pressed in a text field of a non-GET form;
//! * a script that calls `form.submit()` / `requestSubmit()`.
//!
//! A held submission is put to the owner as a decision card, with the page and
//! the draft about to be sent: **Approve Once** lets that one act run, and
//! **Deny** (or no answer) stops it. The approval is bound to the act the guard
//! just recognised and is never remembered. Where a run has no approval
//! channel the guard falls back to refusing outright, leaving the form filled
//! for the owner to press Submit.
//!
//! The honest limits: the rules read the DOM, not intent. An element the
//! probe cannot inspect (inside an iframe, off-page, a probe error) is allowed
//! and logged, and a script-driven form whose final control is labelled with
//! a word outside the vocabulary is not recognised.

use base64::Engine as _;
use serde::Deserialize;

use crate::magician_v2::user_requests::{RequestOption, UserRequest};

/// A DOM question the guard must answer before an interaction runs.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SubmitProbe {
    /// A click on this selector or snapshot ref.
    Click(String),
    /// A click at viewport coordinates (coordinate-driven providers).
    ClickAt(f64, f64),
    /// Enter pressed in whatever element has focus.
    EnterInFocused,
    /// Enter typed into this selector or ref (`type`/`fill` text ending in a
    /// newline).
    EnterIn(String),
    /// A locator-based click whose target is known only by these words.
    ClickByLabel(Vec<String>),
    /// Script source run on the page.
    Script(String),
}

/// What the page says about the control a probe would hit.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(super) struct ControlFacts {
    #[serde(default)]
    pub tag: String,
    #[serde(default, rename = "type")]
    pub input_type: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub in_form: bool,
    /// Lower-cased form method; absent method is `get` per HTML.
    #[serde(default)]
    pub form_method: Option<String>,
    #[serde(default)]
    pub iframe: bool,
}

impl ControlFacts {
    fn in_posting_form(&self) -> bool {
        self.in_form && self.form_method.as_deref().unwrap_or("get") != "get"
    }

    fn in_get_form(&self) -> bool {
        self.in_form && self.form_method.as_deref().unwrap_or("get") == "get"
    }
}

/// Labels that name a final, outward action. Matched against the whole
/// normalised label, or as a leading "submit" phrase ("Submit application").
/// Kept narrow on purpose: "Apply", "Next", "Continue", "Book" and "Confirm"
/// label filters and wizard steps far more often than submissions.
const FINAL_ACTION_LABELS: &[&str] = &[
    "submit",
    "send",
    "send application",
    "send message",
    "place order",
    "pay",
    "pay now",
    "confirm and pay",
    "complete purchase",
    "publish",
    "post",
    "sign up",
    "create account",
    "register",
];

fn normalise_label(label: &str) -> String {
    label
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(super) fn label_is_final_action(label: &str) -> bool {
    let label = normalise_label(label);
    if label.is_empty() {
        return false;
    }
    FINAL_ACTION_LABELS.contains(&label.as_str()) || label.starts_with("submit ")
}

/// Why a click on this control would submit, if it would.
pub(super) fn click_submission_reason(facts: &ControlFacts) -> Option<String> {
    if facts.iframe {
        return None;
    }
    let tag = facts.tag.to_ascii_lowercase();
    let input_type = facts.input_type.to_ascii_lowercase();
    let submit_control = (tag == "input" && matches!(input_type.as_str(), "submit" | "image"))
        || (tag == "button" && matches!(input_type.as_str(), "" | "submit"));
    if submit_control && facts.in_posting_form() {
        return Some("the submit control of a form that posts".to_string());
    }
    if label_is_final_action(&facts.label) && !facts.in_get_form() {
        return Some(format!("a \"{}\" control", facts.label.trim()));
    }
    None
}

/// Why pressing Enter in this element would submit, if it would.
pub(super) fn enter_submission_reason(facts: &ControlFacts) -> Option<String> {
    if facts.iframe {
        return None;
    }
    let tag = facts.tag.to_ascii_lowercase();
    let input_type = facts.input_type.to_ascii_lowercase();
    let text_entry = tag == "input"
        && !matches!(
            input_type.as_str(),
            "button" | "checkbox" | "radio" | "reset" | "file" | "hidden"
        );
    (text_entry && facts.in_posting_form())
        .then(|| "Enter in a field of a form that posts".to_string())
}

/// Why this script would submit a form, if it would.
pub(super) fn script_submission_reason(script: &str) -> Option<String> {
    let compact: String = script.chars().filter(|c| !c.is_whitespace()).collect();
    (compact.contains(".submit(") || compact.contains(".requestSubmit("))
        .then(|| "a script that submits a form".to_string())
}

fn key_is_enter(key: &str) -> bool {
    let last = key.rsplit('+').next().unwrap_or(key);
    matches!(
        last.to_ascii_lowercase().as_str(),
        "enter" | "numpadenter" | "return"
    )
}

fn positional(command: &[String]) -> Vec<&str> {
    command
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|arg| !arg.starts_with("--"))
        .collect()
}

fn decode_eval_script(command: &[String], stdin: Option<&str>) -> Option<String> {
    let args: Vec<&str> = command.iter().skip(1).map(String::as_str).collect();
    if args.contains(&"--stdin") {
        return stdin.map(str::to_string);
    }
    let base64 = args.iter().any(|arg| matches!(*arg, "-b" | "--base64"));
    let script = args
        .iter()
        .rev()
        .find(|arg| !arg.starts_with('-'))
        .copied()?;
    if base64 {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(script.trim())
            .ok()?;
        return String::from_utf8(bytes).ok();
    }
    Some(script.to_string())
}

/// The DOM questions an invocation raises, in command order.
///
/// `commands` are the argv vectors the invocation will run (a batch lists
/// each step); `stdin` is the invocation's stdin, which carries an
/// `eval --stdin` script. A coordinate click is recognised from the
/// `mouse move x y` that precedes its `mouse down`.
pub(super) fn submit_probes(commands: &[Vec<String>], stdin: Option<&str>) -> Vec<SubmitProbe> {
    let mut probes = Vec::new();
    let mut pointer: Option<(f64, f64)> = None;
    // Enter lands in whatever the same invocation last targeted: in a batch
    // `fill @e3 …` then `press Enter`, focus has not moved yet when the probe
    // runs, so the focused element would be the wrong one to ask about.
    let mut last_target: Option<String> = None;
    let enter_probe = |last_target: &Option<String>| match last_target {
        Some(target) => SubmitProbe::EnterIn(target.clone()),
        None => SubmitProbe::EnterInFocused,
    };
    for command in commands {
        let Some(head) = command.first().map(|head| head.to_ascii_lowercase()) else {
            continue;
        };
        let args = positional(command);
        match head.as_str() {
            "click" | "dblclick" => {
                if let Some(target) = args.first() {
                    probes.push(SubmitProbe::Click((*target).to_string()));
                    last_target = Some((*target).to_string());
                }
            },
            "focus" | "check" | "uncheck" | "select" => {
                last_target = args.first().map(|target| (*target).to_string());
            },
            "press" | "key" => {
                if args.iter().any(|key| key_is_enter(key)) {
                    probes.push(enter_probe(&last_target));
                }
            },
            "keyboard" => {
                if args
                    .iter()
                    .skip(1)
                    .any(|text| text.contains('\n') || text.contains('\r'))
                {
                    probes.push(enter_probe(&last_target));
                }
            },
            "type" | "fill" => {
                if let Some(target) = args.first() {
                    last_target = Some((*target).to_string());
                    if args
                        .get(1)
                        .is_some_and(|text| text.ends_with('\n') || text.ends_with('\r'))
                    {
                        probes.push(SubmitProbe::EnterIn((*target).to_string()));
                    }
                }
            },
            "open" | "goto" | "navigate" | "back" | "forward" | "reload" => {
                last_target = None;
            },
            "find" => {
                if args
                    .iter()
                    .any(|arg| matches!(arg.to_ascii_lowercase().as_str(), "click" | "dblclick"))
                {
                    probes.push(SubmitProbe::ClickByLabel(
                        command.iter().skip(1).cloned().collect(),
                    ));
                }
            },
            "mouse" => match args.first().map(|sub| sub.to_ascii_lowercase()) {
                Some(sub) if sub == "move" => {
                    let x = args.get(1).and_then(|v| v.parse::<f64>().ok());
                    let y = args.get(2).and_then(|v| v.parse::<f64>().ok());
                    pointer = x.zip(y);
                },
                Some(sub) if sub == "down" => {
                    if let Some((x, y)) = pointer {
                        probes.push(SubmitProbe::ClickAt(x, y));
                    }
                },
                _ => {},
            },
            "eval" => {
                if let Some(script) = decode_eval_script(command, stdin) {
                    probes.push(SubmitProbe::Script(script));
                }
            },
            _ => {},
        }
    }
    probes
}

/// Page script: facts about the control at a viewport point.
pub(super) fn facts_at_point_script(x: f64, y: f64) -> String {
    format!(
        "(()=>{{const el=document.elementFromPoint({x},{y});if(!el)return null;{FACTS_BODY}}})()"
    )
}

/// Page script: facts about the focused element.
pub(super) const FOCUSED_FACTS_SCRIPT: &str =
    "(()=>{const el=document.activeElement;if(!el||el===document.body)return null;\
     const ctl=el;const form=ctl.form||(ctl.closest&&ctl.closest('form'));\
     return {tag:ctl.tagName.toLowerCase(),type:(ctl.getAttribute('type')||'').toLowerCase(),\
     label:'',in_form:!!form,form_method:form?(form.getAttribute('method')||'get').toLowerCase():null,\
     iframe:ctl.tagName==='IFRAME'};})()";

const FACTS_BODY: &str = "const ctl=el.closest('button,input,[role=button],a')||el;\
     const form=ctl.form||ctl.closest('form');\
     const label=(ctl.innerText||ctl.value||ctl.getAttribute('aria-label')||'').trim().slice(0,80);\
     return {tag:ctl.tagName.toLowerCase(),type:(ctl.getAttribute('type')||'').toLowerCase(),\
     label:label,in_form:!!form,form_method:form?(form.getAttribute('method')||'get').toLowerCase():null,\
     iframe:el.tagName==='IFRAME'};";

/// The owner-facing refusal the agent sees.
pub(super) fn refusal_note(reason: &str) -> String {
    format!(
        "refused: this would submit a form ({reason}). Submitting sends something to the world \
         in the owner's or company's name, so the browser never does it. Stop here: leave the \
         form filled, report what you entered and where, and ask the owner to press Submit."
    )
}

/// The owner's answers to a pending submission.
pub(super) const APPROVE_ONCE: &str = "approve_once";
pub(super) const APPROVE_ALWAYS: &str = "approve_always";
pub(super) const DENY: &str = "deny";

/// Long enough for an owner away from the screen; an unanswered request denies.
const APPROVAL_TIMEOUT_SECS: u64 = 1800;
const PREVIEW_MAX_CHARS: usize = 1500;

/// What the page shows at the moment a submission is about to be made.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(super) struct PageFacts {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// The text of the first editable field (the draft about to be sent).
    #[serde(default)]
    pub text: String,
    /// Digest computed in the browser; form values (including secrets) stay there.
    #[serde(default)]
    pub state_fingerprint: Option<String>,
}

impl PageFacts {
    /// Bind the actual draft before redaction/truncation for the owner card.
    /// Only a digest remains beside that card; secret values are not exposed.
    pub(super) fn submission_fingerprint(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update((self.url.len() as u64).to_le_bytes());
        digest.update(self.url.as_bytes());
        digest.update((self.text.len() as u64).to_le_bytes());
        digest.update(self.text.as_bytes());
        digest.update(
            self.state_fingerprint
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        digest.finalize().into()
    }
}

/// Script that reads [`PageFacts`] from the live page.
pub(super) const PAGE_FACTS_SCRIPT: &str = "(async()=>{const sel=\
     '[role=textbox],textarea,[contenteditable=true],input:not([type]),input[type=text],input[type=email]';\
     const a=document.activeElement;\
     const f=(a&&a.matches&&a.matches(sel))?a:document.querySelector(sel);\
     let state_fingerprint=null;\
     if(globalThis.crypto?.subtle){\
       const root=f?.closest('form')||document;\
       const state=Array.from(root.querySelectorAll('input,textarea,select,[contenteditable=true]')).map(e=>\
         [e.tagName,e.type,e.name,e.id,e.value??e.innerText??'',!!e.checked,\
          Array.from(e.files||[],v=>[v.name,v.size,v.lastModified]),\
          Array.from(e.selectedOptions||[],v=>v.value)]);\
       const bytes=new TextEncoder().encode(JSON.stringify(state));\
       const hash=await crypto.subtle.digest('SHA-256',bytes);\
       state_fingerprint=Array.from(new Uint8Array(hash),v=>v.toString(16).padStart(2,'0')).join('');\
     }\
     return {url:location.href,title:document.title,\
     text:f?String(f.innerText||f.value||''):'',state_fingerprint}})()";

/// One-time approval binds the observed URL/draft/form; an explicit site-wide
/// grant permits later drafts, but never a different origin.
pub(super) fn approval_matches_page(
    reviewed: &PageFacts,
    reviewed_fingerprint: [u8; 32],
    current: &PageFacts,
    current_fingerprint: [u8; 32],
    always: bool,
) -> bool {
    super::submission_grants::origin_of(&reviewed.url)
        == super::submission_grants::origin_of(&current.url)
        && (always || reviewed_fingerprint == current_fingerprint)
}

/// Whether any step of an invocation can leave the page it was probed on:
/// a navigation, or a switch of tab, frame or window.
pub(super) fn changes_destination(commands: &[Vec<String>]) -> bool {
    commands.iter().any(|command| {
        command.first().is_some_and(|head| {
            matches!(
                head.to_ascii_lowercase().as_str(),
                "open"
                    | "goto"
                    | "navigate"
                    | "back"
                    | "forward"
                    | "reload"
                    | "tab"
                    | "frame"
                    | "window"
            )
        })
    })
}

/// Who is asking, for the approval card.
#[derive(Debug, Clone, Default)]
pub(super) struct ApprovalAsker {
    pub principal: String,
    pub workspace: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Origin and path only: a query or fragment can carry tokens or identifiers.
fn page_location(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// The decision card shown to the owner once a form is filled and the next
/// act would send it.
pub(super) fn approval_request(
    reason: &str,
    page: &PageFacts,
    asker: &ApprovalAsker,
    offer_always: bool,
    destination_unconfirmed: bool,
) -> UserRequest {
    let location = page_location(&page.url);
    let draft: String = page.text.trim().chars().take(PREVIEW_MAX_CHARS).collect();
    let always_hint = if offer_always {
        ", or approve always to stop asking for this site until you revoke it"
    } else {
        ""
    };
    let mut options = vec![RequestOption {
        id: APPROVE_ONCE.to_owned(),
        label: "Approve Once".to_owned(),
        requires_input: false,
    }];
    if offer_always {
        options.push(RequestOption {
            id: APPROVE_ALWAYS.to_owned(),
            label: "Approve Always".to_owned(),
            requires_input: false,
        });
    }
    options.push(RequestOption {
        id: DENY.to_owned(),
        label: "Deny".to_owned(),
        requires_input: false,
    });
    let destination_note = if destination_unconfirmed {
        "\n\nThis step also navigates or switches tab, so the page where it will submit \
         cannot be confirmed from here."
    } else {
        ""
    };
    let draft_block = if draft.is_empty() {
        String::new()
    } else {
        format!("\n\nWhat will be sent:\n{draft}")
    };
    UserRequest {
        id: String::new(),
        request_type: "browser_submission_approval".to_owned(),
        question: format!(
            "Everything is filled in on {location}. Press {reason} now?{draft_block}{destination_note}\n\n\
             Approve to send it once{always_hint}, or deny to leave the page as it is."
        ),
        options,
        principal: asker.principal.clone(),
        workspace: asker.workspace.clone(),
        context: serde_json::json!({
            "kind": "browser_submission_approval",
            "reason": reason,
            "page": location,
            "page_title": page.title,
            "draft": draft,
            "owner_agent_id": asker.agent_id,
        }),
        source: "browser".to_owned(),
        execution_id: asker.execution_id.clone(),
        task_id: asker.task_id.clone(),
        timeout_secs: APPROVAL_TIMEOUT_SECS,
        default_on_timeout: DENY.to_owned(),
        created_at: 0,
        sensitive: None,
    }
}

/// What the agent is told when the owner declined or did not answer.
pub(super) fn declined_note(reason: &str) -> String {
    format!(
        "not submitted: the owner did not approve this submission ({reason}). Do not retry it \
         or look for another way to send it. Leave the page as it is, report what is ready, and \
         wait for the owner."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asker() -> ApprovalAsker {
        ApprovalAsker {
            principal: "anonymous".into(),
            workspace: "default".into(),
            execution_id: Some("exec-1".into()),
            task_id: Some("task-1".into()),
            agent_id: Some("personal-assistant".into()),
        }
    }

    #[test]
    fn the_approval_card_offers_always_only_when_the_site_is_known() {
        let page = PageFacts {
            url: "https://x.com/compose/post?token=secret#frag".into(),
            title: "Compose".into(),
            text: "Hello world".into(),
            ..Default::default()
        };
        let request = approval_request("a \"Post\" control", &page, &asker(), true, false);
        let ids: Vec<_> = request.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, [APPROVE_ONCE, APPROVE_ALWAYS, DENY]);
        let once_only = approval_request("a \"Post\" control", &page, &asker(), false, false);
        let ids: Vec<_> = once_only.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, [APPROVE_ONCE, DENY]);
        assert_eq!(request.default_on_timeout, DENY);
        assert_eq!(request.execution_id.as_deref(), Some("exec-1"));
        assert!(request.question.contains("https://x.com/compose/post"));
        assert!(request.question.contains("Hello world"));
        let shown = serde_json::to_string(&request.context).unwrap();
        assert!(!shown.contains("secret") && !shown.contains("frag"));
    }

    #[test]
    fn once_approval_refuses_changed_form_values_even_after_redaction() {
        let original = PageFacts {
            url: "https://example.com/compose".into(),
            text: "draft".into(),
            state_fingerprint: Some("original-form".into()),
            ..Default::default()
        };
        let pinned = original.submission_fingerprint();
        let mut current = original.clone();
        current.state_fingerprint = Some("changed-recipient-or-password".into());
        let changed = current.submission_fingerprint();
        // Both display cards can omit/redact the value that changed.
        current.state_fingerprint = original.state_fingerprint.clone();
        assert!(!approval_matches_page(
            &original, pinned, &current, changed, false
        ));
        assert!(approval_matches_page(
            &original, pinned, &current, changed, true
        ));
        current.url = "https://other.example/compose".into();
        assert!(!approval_matches_page(
            &original, pinned, &current, changed, true
        ));
        current = original.clone();
        current.url.push_str("?recipient=another");
        assert!(!approval_matches_page(
            &original,
            pinned,
            &current,
            current.submission_fingerprint(),
            false
        ));
        current = original.clone();
        current.title = "A notification changed the title".into();
        assert!(approval_matches_page(
            &original,
            pinned,
            &current,
            current.submission_fingerprint(),
            false
        ));
    }

    #[test]
    fn a_long_draft_is_cut_and_an_empty_one_is_left_out() {
        let long = PageFacts {
            text: "é".repeat(PREVIEW_MAX_CHARS + 50),
            ..PageFacts::default()
        };
        let request = approval_request("a \"Post\" control", &long, &asker(), true, false);
        assert_eq!(
            request.context["draft"].as_str().unwrap().chars().count(),
            PREVIEW_MAX_CHARS
        );
        let empty = approval_request(
            "a \"Post\" control",
            &PageFacts::default(),
            &asker(),
            true,
            false,
        );
        assert!(!empty.question.contains("What will be sent"));
    }

    #[test]
    fn a_step_that_moves_the_page_is_flagged_and_never_offers_always() {
        let command = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert!(changes_destination(&[
            command(&["open", "https://linkedin.com/feed"]),
            command(&["find", "role", "button", "click"]),
        ]));
        assert!(changes_destination(&[command(&[
            "tab",
            "new",
            "https://x.com"
        ])]));
        assert!(!changes_destination(&[
            command(&["fill", "@e3", "hello"]),
            command(&["click", "@e9"]),
        ]));
        let request = approval_request(
            "a \"Post\" control",
            &PageFacts::default(),
            &asker(),
            false,
            true,
        );
        assert!(request.question.contains("cannot be confirmed"));
        assert!(request.options.iter().all(|o| o.id != APPROVE_ALWAYS));
    }

    #[test]
    fn a_declined_submission_tells_the_agent_to_stop() {
        let note = declined_note("a \"Post\" control");
        assert!(note.contains("Do not retry"));
    }

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    fn facts(tag: &str, kind: &str, label: &str, method: Option<&str>) -> ControlFacts {
        ControlFacts {
            tag: tag.to_string(),
            input_type: kind.to_string(),
            label: label.to_string(),
            in_form: method.is_some(),
            form_method: method.map(str::to_string),
            iframe: false,
        }
    }

    #[test]
    fn submit_controls_of_posting_forms_are_submissions() {
        assert!(click_submission_reason(&facts("button", "", "Continue", Some("post"))).is_some());
        assert!(click_submission_reason(&facts("button", "submit", "Go", Some("post"))).is_some());
        assert!(click_submission_reason(&facts("input", "submit", "", Some("post"))).is_some());
        assert!(click_submission_reason(&facts("input", "image", "", Some("put"))).is_some());
    }

    #[test]
    fn ordinary_browsing_is_not_a_submission() {
        // A search form is navigation.
        assert!(click_submission_reason(&facts("button", "", "Search", Some("get"))).is_none());
        assert!(
            click_submission_reason(&facts("button", "submit", "Submit", Some("get"))).is_none()
        );
        // A typeless button outside any form is not a submit control.
        assert!(click_submission_reason(&facts("button", "", "Menu", None)).is_none());
        assert!(
            click_submission_reason(&facts("button", "button", "Next", Some("post"))).is_none()
        );
        assert!(click_submission_reason(&facts("a", "", "Pricing", None)).is_none());
        // Wizard and filter words stay free.
        for label in [
            "Apply",
            "Next",
            "Continue",
            "Book",
            "Confirm",
            "Apply filters",
        ] {
            assert!(
                click_submission_reason(&facts("div", "", label, None)).is_none(),
                "{label}"
            );
        }
    }

    #[test]
    fn final_action_labels_catch_script_driven_forms() {
        assert!(click_submission_reason(&facts("div", "", "Submit", None)).is_some());
        assert!(click_submission_reason(&facts("div", "", "  Pay now ", None)).is_some());
        assert!(
            click_submission_reason(&facts("button", "button", "Submit application →", None))
                .is_some()
        );
        assert!(click_submission_reason(&facts("a", "", "Sign up", None)).is_some());
        assert!(click_submission_reason(&facts("span", "", "Publish", None)).is_some());
    }

    #[test]
    fn iframes_are_not_inspectable_and_are_allowed() {
        let mut inside = facts("iframe", "", "Submit", Some("post"));
        inside.iframe = true;
        assert!(click_submission_reason(&inside).is_none());
    }

    #[test]
    fn enter_submits_only_text_fields_of_posting_forms() {
        assert!(enter_submission_reason(&facts("input", "email", "", Some("post"))).is_some());
        assert!(enter_submission_reason(&facts("input", "", "", Some("post"))).is_some());
        assert!(enter_submission_reason(&facts("input", "search", "", Some("get"))).is_none());
        assert!(enter_submission_reason(&facts("textarea", "", "", Some("post"))).is_none());
        assert!(enter_submission_reason(&facts("input", "checkbox", "", Some("post"))).is_none());
        assert!(enter_submission_reason(&facts("input", "text", "", None)).is_none());
    }

    #[test]
    fn scripts_that_submit_are_recognised() {
        assert!(script_submission_reason("document.forms[0].submit()").is_some());
        assert!(script_submission_reason("f.requestSubmit ( )").is_some());
        assert!(script_submission_reason("document.title").is_none());
    }

    #[test]
    fn probes_are_planned_from_every_interaction_command() {
        let probes = submit_probes(
            &[
                cmd(&["open", "https://example.com"]),
                cmd(&["fill", "@e3", "Ada"]),
                cmd(&["click", "@e4"]),
                cmd(&["press", "Enter"]),
                cmd(&["keyboard", "type", "hello\n"]),
                cmd(&["type", "#q", "rust\n"]),
                cmd(&["find", "text", "Submit", "click"]),
                cmd(&["mouse", "move", "100", "200"]),
                cmd(&["mouse", "down"]),
                cmd(&["mouse", "up"]),
                cmd(&["eval", "document.forms[0].submit()"]),
                cmd(&["snapshot", "-i"]),
            ],
            None,
        );
        assert_eq!(
            probes,
            vec![
                SubmitProbe::Click("@e4".into()),
                SubmitProbe::EnterIn("@e4".into()),
                SubmitProbe::EnterIn("@e4".into()),
                SubmitProbe::EnterIn("#q".into()),
                SubmitProbe::ClickByLabel(vec!["text".into(), "Submit".into(), "click".into()]),
                SubmitProbe::ClickAt(100.0, 200.0),
                SubmitProbe::Script("document.forms[0].submit()".into()),
            ]
        );
    }

    #[test]
    fn eval_scripts_are_read_from_base64_and_stdin() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("form.requestSubmit()");
        let probes = submit_probes(&[cmd(&["eval", "-b", &encoded])], None);
        assert_eq!(
            probes,
            vec![SubmitProbe::Script("form.requestSubmit()".into())]
        );
        let probes = submit_probes(&[cmd(&["eval", "--stdin"])], Some("x.submit()"));
        assert_eq!(probes, vec![SubmitProbe::Script("x.submit()".into())]);
    }

    #[test]
    fn non_enter_keys_and_plain_typing_raise_no_probe() {
        assert!(submit_probes(
            &[
                cmd(&["press", "Tab"]),
                cmd(&["press", "Control+a"]),
                cmd(&["fill", "@e3", "Ada Lovelace"]),
                cmd(&["keyboard", "type", "no newline"]),
            ],
            None,
        )
        .is_empty());
        assert_eq!(
            submit_probes(&[cmd(&["press", "Shift+Enter"])], None),
            vec![SubmitProbe::EnterInFocused]
        );
        assert_eq!(
            submit_probes(
                &[cmd(&["fill", "@e3", "Ada"]), cmd(&["press", "Enter"])],
                None
            ),
            vec![SubmitProbe::EnterIn("@e3".into())]
        );
    }
}
