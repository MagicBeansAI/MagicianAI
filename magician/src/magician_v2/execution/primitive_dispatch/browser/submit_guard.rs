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
//! A refused submission is left for the owner to make: the agent fills the
//! form, reports it, and the person presses Submit. This is deliberately a
//! refusal rather than an approval prompt — a browser act has no bindable
//! form, so even a classified one could not be sent on anyone's authority.
//!
//! The honest limits: the rules read the DOM, not intent. An element the
//! probe cannot inspect (inside an iframe, off-page, a probe error) is allowed
//! and logged, and a script-driven form whose final control is labelled with
//! a word outside the vocabulary is not recognised.

use base64::Engine as _;
use serde::Deserialize;

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

#[cfg(test)]
mod tests {
    use super::*;

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
