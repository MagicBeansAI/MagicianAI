//! A whole plan driven with no subprocess, no terminal and no waiting.

use std::collections::BTreeMap;

use magician_components::{loader, Observed};

use super::*;

/// Records what the engine asked for and answers however the test says.
struct Fake {
    /// Components the probe reports present, mutated by `installs`.
    present: Vec<String>,
    /// Targets that "work": running one makes its components present.
    installs: BTreeMap<String, Vec<String>>,
    /// Targets that fail outright.
    failing: Vec<String>,
    /// Components whose probe cannot answer.
    unknown: Vec<String>,
    confirm_answer: bool,
    /// Whether this pretends to be a checkout. Default true, because that is
    /// what every existing test meant before packages could install anything.
    checkout: bool,
    pub ran: Vec<String>,
    /// Variables the engine offered to collect, in order.
    pub asked: Vec<String>,
    pub opened: Vec<String>,
    pub instructed: Vec<String>,
    pub said: Vec<String>,
    pub pauses: usize,
}

impl Fake {
    fn new(present: &[&str]) -> Self {
        Fake {
            present: present.iter().map(|s| s.to_string()).collect(),
            installs: BTreeMap::new(),
            failing: Vec::new(),
            unknown: Vec::new(),
            confirm_answer: true,
            checkout: true,
            ran: Vec::new(),
            asked: Vec::new(),
            opened: Vec::new(),
            instructed: Vec::new(),
            said: Vec::new(),
            pauses: 0,
        }
    }
    /// A machine that installed from a package: no Makefile, so make targets
    /// are not available and the shipped scripts are all there is.
    fn without_checkout(mut self) -> Self {
        self.checkout = false;
        self
    }
    fn target_provides(mut self, target: &str, ids: &[&str]) -> Self {
        self.installs.insert(
            target.to_string(),
            ids.iter().map(|s| s.to_string()).collect(),
        );
        self
    }
    fn target_fails(mut self, target: &str) -> Self {
        self.failing.push(target.to_string());
        self
    }
    fn unprobeable(mut self, id: &str) -> Self {
        self.unknown.push(id.to_string());
        self
    }
    fn declines_everything(mut self) -> Self {
        self.confirm_answer = false;
        self
    }
}

impl Effects for Fake {
    fn in_source_checkout(&self) -> bool {
        self.checkout
    }
    /// Scripts and make targets land in the same list. What matters to every
    /// test here is *whether the step ran*, and recording them apart would
    /// make each assertion pick a world.
    fn run_script(&mut self, script: &str) -> Result<(), String> {
        self.ran.push(script.to_string());
        if self.failing.iter().any(|t| t == script) {
            return Err("exit 2".into());
        }
        if let Some(ids) = self.installs.get(script) {
            self.present.extend(ids.clone());
        }
        Ok(())
    }
    fn run_make(&mut self, target: &str) -> Result<(), String> {
        self.ran.push(target.to_string());
        if self.failing.iter().any(|t| t == target) {
            return Err("exit 2".into());
        }
        if let Some(ids) = self.installs.get(target) {
            self.present.extend(ids.clone());
        }
        Ok(())
    }
    fn ask_secret(&mut self, _label: &str, variable: &str) -> bool {
        self.asked.push(variable.to_string());
        // Whichever variables the test named as pasteable are accepted, and
        // accepting one makes its component present the way a real write does.
        if let Some(ids) = self.installs.get(variable) {
            self.present.extend(ids.clone());
            return true;
        }
        false
    }
    fn instruct(&mut self, title: &str, _steps: &[String]) {
        self.instructed.push(title.to_string());
    }
    fn open(&mut self, target: &str) {
        self.opened.push(target.to_string());
    }
    fn confirm(&mut self, _question: &str, _default: bool) -> bool {
        self.confirm_answer
    }
    fn probe(&mut self, component_id: &str) -> Observed {
        if self.unknown.iter().any(|i| i == component_id) {
            return Observed::Unknown("cannot be read from a process".into());
        }
        if self.present.iter().any(|i| i == component_id) {
            Observed::Present("up".into())
        } else {
            Observed::Absent("not there".into())
        }
    }
    fn say(&mut self, line: &str) {
        self.said.push(line.to_string());
    }
    fn pause(&mut self) {
        self.pauses += 1;
    }
    fn keep_waiting(&mut self, attempt: usize) -> bool {
        attempt < 2
    }
}

fn graph() -> Graph {
    loader::try_load(&loader::Paths {
        data_root: "/tmp/d".into(),
        home: "/tmp/h".into(),
    })
    .expect("graph")
}

fn usable(ids: &[&str]) -> BTreeMap<String, bool> {
    ids.iter().map(|i| (i.to_string(), true)).collect()
}

fn outcome(reports: &[StepReport], id: &str) -> StepOutcome {
    reports
        .iter()
        .find(|r| r.component_id == id)
        .expect(id)
        .outcome
        .clone()
}

#[test]
fn a_component_already_present_is_not_installed_again() {
    let g = graph();
    let mut fake = Fake::new(&["bundled-browser"]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["bundled-browser".into()]);
    assert_eq!(outcome(&reports, "bundled-browser"), StepOutcome::AlreadyThere);
    assert!(
        fake.ran.is_empty(),
        "nothing should have run: {:?}",
        fake.ran
    );
}

#[test]
fn the_probe_decides_success_not_the_exit_code() {
    // The target exits zero and changes nothing. An installer that trusted the
    // exit code would report success onto a machine that still cannot do it.
    let g = graph();
    let mut fake = Fake::new(&[]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["bundled-browser".into()]);
    assert!(fake.ran.contains(&"setup-agent-browser".to_string()));
    match outcome(&reports, "bundled-browser") {
        StepOutcome::NotVerified { .. } => {},
        other => panic!("expected not-verified, got {other:?}"),
    }
}

#[test]
fn a_target_that_works_is_verified_by_the_probe() {
    let g = graph();
    let mut fake = Fake::new(&[]).target_provides("setup-agent-browser", &["bundled-browser"]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["bundled-browser".into()]);
    assert_eq!(outcome(&reports, "bundled-browser"), StepOutcome::Installed);
}

#[test]
fn a_failing_target_is_still_probed_and_does_not_stop_the_run() {
    // A target can fail having already done the part that matters, and one
    // component failing must not cost the ones beside it.
    let g = graph();
    let mut fake = Fake::new(&[])
        .target_fails("setup-ollama-embedding")
        .target_provides("setup-agent-browser", &["bundled-browser"]);
    // The embedder rather than the generation model: it has no dependency of
    // its own, so this stays a test about one failing step not costing the
    // step beside it.
    let reports = Engine::new(&g, &mut fake, BTreeMap::new())
        .run(&["ollama-embedding".into(), "bundled-browser".into()]);
    assert!(matches!(
        outcome(&reports, "ollama-embedding"),
        StepOutcome::NotVerified { .. }
    ));
    assert_eq!(
        outcome(&reports, "bundled-browser"),
        StepOutcome::Installed,
        "the next component must still be attempted"
    );
}

#[test]
fn a_component_whose_dependency_failed_is_blocked_and_names_it() {
    // chrome-extension needs magicutor. With magicutor unusable the step is not
    // attempted, and the report names what actually broke.
    let g = graph();
    let mut fake = Fake::new(&[]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["chrome-extension".into()]);
    match outcome(&reports, "chrome-extension") {
        StepOutcome::Blocked { by } => assert!(by.contains("executor"), "named: {by}"),
        other => panic!("expected blocked, got {other:?}"),
    }
    assert!(
        fake.instructed.is_empty(),
        "a blocked step must not instruct anyone"
    );
}

#[test]
fn a_manual_step_opens_the_right_place_and_waits_for_the_person() {
    let g = graph();
    let mut fake = Fake::new(&[]);
    let reports = Engine::new(&g, &mut fake, usable(&["desktop-host-gateway"]))
        .run(&["tcc-automation".into()]);
    assert!(!fake.instructed.is_empty(), "the person needs the steps");
    assert!(
        fake.opened.iter().any(|o| o.contains("Privacy_Automation")),
        "opened: {:?}",
        fake.opened
    );
    assert!(
        fake.pauses > 0,
        "it must actually wait rather than give up at once"
    );
    assert!(matches!(
        outcome(&reports, "tcc-automation"),
        StepOutcome::NotVerified { .. }
    ));
}

#[test]
fn an_unprobeable_permission_falls_back_to_asking_the_person() {
    // Microphone access cannot be read from a process that lacks it, so their
    // word is the only evidence there is. Reporting it as failed would be wrong.
    let g = graph();
    let mut fake = Fake::new(&[]).unprobeable("tcc-microphone");
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["tcc-microphone".into()]);
    assert_eq!(outcome(&reports, "tcc-microphone"), StepOutcome::Installed);
}

#[test]
fn declining_runs_nothing_and_is_not_a_failure() {
    let g = graph();
    let mut fake = Fake::new(&[]).declines_everything();
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["bundled-browser".into()]);
    assert_eq!(outcome(&reports, "bundled-browser"), StepOutcome::Declined);
    assert!(fake.ran.is_empty());
    assert!(!outcome(&reports, "bundled-browser").usable());
}

#[test]
fn a_core_component_is_never_offered_as_something_to_install() {
    let g = graph();
    let mut fake = Fake::new(&[]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["magician-runtime".into()]);
    assert_eq!(
        outcome(&reports, "magician-runtime"),
        StepOutcome::WithRuntime
    );
    assert!(fake.ran.is_empty() && fake.instructed.is_empty());
}

#[test]
fn every_step_reports_a_line_a_person_can_read() {
    let g = graph();
    let mut fake = Fake::new(&["bundled-browser"]);
    Engine::new(&g, &mut fake, BTreeMap::new())
        .run(&["bundled-browser".into(), "magician-runtime".into()]);
    assert_eq!(fake.said.len(), 2, "one line per step: {:?}", fake.said);
    for line in &fake.said {
        assert!(!line.trim().is_empty());
    }
}

#[test]
fn a_step_for_a_component_this_machine_cannot_have_runs_nothing() {
    // The plan already excludes these, so this is the backstop for a step list
    // built elsewhere — `--step <id>` will be exactly that. Refusing before
    // touching the machine is the point: the alternative is a pull that
    // succeeds and a model that never answers.
    let g = graph();
    let mut fake = Fake::new(&[]).target_provides("setup-agent-browser", &["bundled-browser"]);
    let small = magician_components::Host {
        os: "macos".into(),
        arch: "arm64".into(),
        memory_gb: 8,
    };
    let reports = Engine::on_host(&g, &mut fake, BTreeMap::new(), small)
        .run(&["ollama-generation".into(), "bundled-browser".into()]);

    match outcome(&reports, "ollama-generation") {
        StepOutcome::Unsupported { reason } => {
            assert!(
                reason.contains("16"),
                "the reason must say what is missing: {reason}"
            )
        },
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        !fake
            .ran
            .iter()
            .any(|t| t == "setup-local-generation-selected"),
        "nothing should have been run for it: {:?}",
        fake.ran
    );
    // And the step beside it is untouched by the refusal.
    assert!(outcome(&reports, "bundled-browser").usable());
}

#[test]
fn a_package_install_runs_the_shipped_script_instead_of_a_make_target() {
    // The gap that kept a downloaded release from being a real install: with
    // no Makefile, every capability step was unreachable. The graph names the
    // script each target runs, so the same step works from either world.
    let g = graph();
    let mut fake = Fake::new(&[])
        .without_checkout()
        .target_provides("setup-ollama-embedding.sh", &["ollama-embedding"]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["ollama-embedding".into()]);

    assert!(
        outcome(&reports, "ollama-embedding").usable(),
        "{reports:?}"
    );
    assert!(
        fake.ran.iter().any(|r| r == "setup-ollama-embedding.sh"),
        "it should have run the shipped script: {:?}",
        fake.ran
    );
    assert!(
        !fake.ran.iter().any(|r| r == "setup-ollama-embedding"),
        "there is no make here: {:?}",
        fake.ran
    );
}

#[test]
fn a_checkout_still_runs_the_make_target() {
    // The other half of the same rule: where make exists it stays the way in,
    // because the target may do more than the one script it calls.
    let g = graph();
    let mut fake = Fake::new(&[]).target_provides("setup-ollama-embedding", &["ollama-embedding"]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["ollama-embedding".into()]);

    assert!(outcome(&reports, "ollama-embedding").usable());
    assert!(
        fake.ran.iter().any(|r| r == "setup-ollama-embedding"),
        "{:?}",
        fake.ran
    );
}

#[test]
fn a_step_that_needs_a_checkout_says_so_before_asking() {
    // setup-agent-browser delegates to skillshub's own Makefile, so no package
    // can carry it. Discovering that after someone agreed to install it wastes
    // the one answer they gave.
    let g = graph();
    let mut fake = Fake::new(&[]).without_checkout();
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["bundled-browser".into()]);

    match outcome(&reports, "bundled-browser") {
        StepOutcome::Unsupported { reason } => {
            assert!(reason.contains("checkout"), "{reason}")
        },
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        fake.ran.is_empty(),
        "nothing should have run: {:?}",
        fake.ran
    );
}

#[test]
fn a_key_pasted_at_the_prompt_finishes_the_step_and_stops_the_asking() {
    // The remote-models component offers six providers. Pasting one satisfies
    // an any-of probe, so asking about the other five afterwards is noise —
    // and the person has already told you which one they have.
    let g = graph();
    let mut fake = Fake::new(&[]).target_provides("OPENAI_API_KEY", &["remote-models"]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["remote-models".into()]);

    assert!(outcome(&reports, "remote-models").usable(), "{reports:?}");
    assert_eq!(
        fake.asked.first().map(String::as_str),
        Some("OPENAI_API_KEY"),
        "asked in declared order: {:?}",
        fake.asked
    );
    assert_eq!(
        fake.asked.len(),
        1,
        "stopped after one landed: {:?}",
        fake.asked
    );
}

#[test]
fn skipping_every_prompt_leaves_the_step_unfinished_rather_than_failed() {
    // Skipping is ordinary — a key can always be pasted into the env file by
    // hand — so it must not read as a failure, and every provider gets offered.
    let g = graph();
    let mut fake = Fake::new(&[]);
    let reports = Engine::new(&g, &mut fake, BTreeMap::new()).run(&["remote-models".into()]);

    assert!(
        fake.asked.len() > 1,
        "every provider offered: {:?}",
        fake.asked
    );
    assert!(
        matches!(
            outcome(&reports, "remote-models"),
            StepOutcome::NotVerified { .. }
        ),
        "{reports:?}"
    );
}

#[test]
fn a_component_that_wants_a_file_is_never_asked_for_a_secret() {
    // workspace-oauth needs client_secret.json downloaded and placed. A text
    // field cannot supply a file, and prompting for one would be theatre.
    let g = graph();
    let mut fake = Fake::new(&[]);
    Engine::new(&g, &mut fake, BTreeMap::new()).run(&["workspace-oauth".into()]);
    assert!(
        fake.asked.is_empty(),
        "nothing to paste here: {:?}",
        fake.asked
    );
}
