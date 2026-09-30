//! Executing a behavior's reviewed operation recipe.
//!
//! The contract half lives in `manifest.rs`: an ordered `steps` list, each step
//! naming one operation from the behavior's allow-set and optionally guarded by
//! one equality against an earlier step's validated output. This module is the
//! decision that turns that declaration into "which step runs next" — and,
//! importantly, nothing else. It performs no I/O, holds no authority, and
//! cannot dispatch anything.
//!
//! Keeping it pure is the point. Recipe progression is the part a reader has to
//! be able to check by eye, because a guard silently reading the wrong value
//! does not fail: it produces a behavior that quietly stops doing half its job.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::manifest::{AppManifestBehaviorStep, AppManifestBehaviorStepGuard, AppManifestRunner};
use super::models::AppName;

/// Execution support for an admitted, grant-validated behavior. Both scheduler
/// lanes use this gate; the workflow owner still revalidates the exact recipe,
/// operation grants and remaining budget before every model call.
pub(crate) fn behavior_execution_ready(
    runner: AppManifestRunner,
    operations: &[AppName],
    steps: &[AppManifestBehaviorStep],
) -> bool {
    match runner {
        // Publication binds this exact step to the ContextualRound node.
        // Other native recipes are rejected if they declare semantic steps.
        AppManifestRunner::Recipe => {
            (operations.is_empty() && steps.is_empty())
                || (steps.len() == 1
                    && steps[0].when.is_none()
                    && operations == [steps[0].operation.clone()])
        },
        AppManifestRunner::Auto => {
            !steps.is_empty()
                && steps
                    .iter()
                    .all(|step| operations.contains(&step.operation))
        },
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AppBehaviorRecipeError {
    #[error("recipe progress names step `{0}`, which the reviewed recipe does not declare")]
    UnknownStep(String),
    #[error("recipe progress records step `{0}` more than once")]
    DuplicateStep(String),
    #[error("recipe progress records step `{0}` before an earlier step it depends on")]
    OutOfOrder(String),
    // `guarded_on` rather than `source`: thiserror reads a field named
    // `source` as the error's cause and demands it implement `Error`.
    #[error("step `{step}` guards on `{guarded_on}`, which has not produced an output")]
    GuardSourceMissing { step: String, guarded_on: String },
}

/// One completed step: which step, and the output it produced after that
/// output was validated against the step's reviewed schema.
///
/// The output is stored already-validated. A resolver that had to trust
/// unvalidated model bytes to decide what runs next would be deciding
/// authority from an untrusted source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorStepOutcome {
    pub step: AppName,
    pub output: Value,
    /// Output tokens this step actually spent, charged against the behavior's
    /// reviewed PER-RUN ceiling.
    ///
    /// Durable for the same reason the output is: a run can be interrupted and
    /// continued, and a remaining budget held only in the dispatching frame is
    /// re-seeded to the full grant by every continuation — which quietly turns
    /// the reviewed per-run ceiling into a per-continuation one. The recorded
    /// spend of the steps that already ran is the only part of that budget
    /// that survives the gap.
    ///
    /// Deliberately no `serde` default: progress written without a spend
    /// record cannot prove what the run has left, and reading it as zero would
    /// widen exactly the ceiling this field exists to hold. Such a record
    /// fails to deserialize, and the run fails closed.
    pub spent_output_tokens: u32,
}

/// What the recipe should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppBehaviorRecipeNext<'a> {
    /// Run this step. Its operation and output schema are the exact reviewed
    /// material for the model turn.
    Run(&'a AppManifestBehaviorStep),
    /// Every remaining step is guarded off, or none remain. The behavior is
    /// finished; this is a normal outcome, not a failure.
    ///
    /// A gate that said `pass` reaches here, which is exactly right: the turn
    /// happened, it decided not to speak, and no further model call is owed.
    Complete,
}

/// Decide the next step of a reviewed recipe.
///
/// `completed` is the durable record of steps already run, in order. The
/// resolver re-derives the whole decision from it every time rather than
/// keeping a cursor, so a resumed run and a fresh one cannot disagree, and a
/// lost or replayed response cannot advance the recipe by itself.
pub fn next_recipe_step<'a>(
    steps: &'a [AppManifestBehaviorStep],
    completed: &[AppBehaviorStepOutcome],
) -> Result<AppBehaviorRecipeNext<'a>, AppBehaviorRecipeError> {
    let declared: BTreeMap<&AppName, usize> = steps
        .iter()
        .enumerate()
        .map(|(index, step)| (&step.id, index))
        .collect();

    // Validate the progress record before trusting it to skip work. Progress
    // is durable state that survives a restart, and treating a corrupt record
    // as "these steps already ran" would silently skip reviewed model turns.
    let mut seen: BTreeMap<&AppName, &Value> = BTreeMap::new();
    let mut highest = None;
    for outcome in completed {
        let Some(index) = declared.get(&outcome.step) else {
            return Err(AppBehaviorRecipeError::UnknownStep(
                outcome.step.to_string(),
            ));
        };
        if seen.insert(&outcome.step, &outcome.output).is_some() {
            return Err(AppBehaviorRecipeError::DuplicateStep(
                outcome.step.to_string(),
            ));
        }
        if highest.is_some_and(|previous| *index <= previous) {
            return Err(AppBehaviorRecipeError::OutOfOrder(outcome.step.to_string()));
        }
        highest = Some(*index);
    }

    for step in steps {
        if seen.contains_key(&step.id) {
            continue;
        }
        match step.when.as_ref() {
            None => return Ok(AppBehaviorRecipeNext::Run(step)),
            Some(guard) => {
                // The guard's source must already have produced an output.
                // Validation refuses a forward reference at admission, so
                // reaching this means the source step was skipped — which can
                // only happen if IT was guarded off. A step whose guard source
                // never ran is itself unreachable, so skip it rather than
                // erroring: a chain of guards ending in "do nothing" is a
                // legitimate recipe.
                let Some(output) = seen.get(&guard.step) else {
                    if declared.contains_key(&guard.step) {
                        continue;
                    }
                    return Err(AppBehaviorRecipeError::GuardSourceMissing {
                        step: step.id.to_string(),
                        guarded_on: guard.step.to_string(),
                    });
                };
                if guard_matches(guard, output) {
                    return Ok(AppBehaviorRecipeNext::Run(step));
                }
                // Guard did not match: this step does not run. Continue rather
                // than stopping, so a later independent step is still reached.
            },
        }
    }
    Ok(AppBehaviorRecipeNext::Complete)
}

/// What is left of a behavior's reviewed per-run output-token ceiling.
///
/// Derived from durable progress rather than from a counter in the dispatching
/// frame, so a continued run sees what its earlier steps already spent instead
/// of a fresh grant. Saturating on both sides: a run that somehow over-spent
/// gets zero, never a wrapped-around budget.
pub fn remaining_run_output_tokens(
    max_tokens_per_run: u64,
    completed: &[AppBehaviorStepOutcome],
) -> u32 {
    let ceiling = u32::try_from(max_tokens_per_run).unwrap_or(u32::MAX);
    let spent = completed.iter().fold(0u32, |total, outcome| {
        total.saturating_add(outcome.spent_output_tokens)
    });
    ceiling.saturating_sub(spent)
}

/// Compare one guard against a step's validated output.
///
/// Absent, null and wrong-typed values do NOT match. A guard is permission for
/// a model call to happen; anything less than an exact match must withhold it.
fn guard_matches(guard: &AppManifestBehaviorStepGuard, output: &Value) -> bool {
    let Some(field) = output.get(guard.field.as_str()) else {
        return false;
    };
    match field {
        Value::String(value) => value == &guard.equals,
        Value::Bool(value) => {
            (guard.equals == "true" && *value) || (guard.equals == "false" && !*value)
        },
        // Manifest validation restricts guards to enum, text and boolean
        // fields, so anything else here is a shape that did not survive
        // validation. Refuse rather than coerce.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::apps::manifest::{AppManifestInputSchema, AppManifestSchemaType};
    use serde_json::json;

    fn schema() -> AppManifestInputSchema {
        AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: Default::default(),
            value_schema: None,
        }
    }

    fn name(value: &str) -> AppName {
        AppName::parse(value).expect("test app name")
    }

    fn step(
        id: &str,
        operation: &str,
        when: Option<(&str, &str, &str)>,
    ) -> AppManifestBehaviorStep {
        AppManifestBehaviorStep {
            id: name(id),
            operation: name(operation),
            output_schema: schema(),
            when: when.map(|(source, field, equals)| AppManifestBehaviorStepGuard {
                step: name(source),
                field: name(field),
                equals: equals.to_owned(),
            }),
        }
    }

    fn done(step: &str, output: Value) -> AppBehaviorStepOutcome {
        spent(step, output, 0)
    }

    fn spent(step: &str, output: Value, spent_output_tokens: u32) -> AppBehaviorStepOutcome {
        AppBehaviorStepOutcome {
            step: name(step),
            output,
            spent_output_tokens,
        }
    }

    /// Town Square's shape: gate, then compose only on `engage`.
    fn town_square() -> Vec<AppManifestBehaviorStep> {
        vec![
            step("gate", "engagement_gate", None),
            step(
                "compose",
                "compose_post",
                Some(("gate", "decision", "engage")),
            ),
        ]
    }

    #[test]
    fn reviewed_operation_recipes_are_executable_but_bare_allow_sets_are_not() {
        let operations = [name("engagement_gate"), name("compose_post")];
        let steps = town_square();
        assert!(behavior_execution_ready(
            AppManifestRunner::Auto,
            &operations,
            &steps
        ));
        assert!(!behavior_execution_ready(
            AppManifestRunner::Auto,
            &operations,
            &[]
        ));
        assert!(!behavior_execution_ready(
            AppManifestRunner::Auto,
            &operations[..1],
            &steps
        ));
        assert!(!behavior_execution_ready(AppManifestRunner::Auto, &[], &[]));
        assert!(!behavior_execution_ready(
            AppManifestRunner::Recipe,
            &operations,
            &steps
        ));
        assert!(behavior_execution_ready(
            AppManifestRunner::Recipe,
            &[],
            &[]
        ));
    }

    #[test]
    fn shipped_town_square_ambient_turn_has_an_execution_binding() {
        let candidate = super::super::package_staging::admit_package_directory(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../magician_data_v3/system/town_square/app")
                .canonicalize()
                .expect("canonical shipped package root"),
        )
        .unwrap();
        let manifest = candidate.manifest().manifest();
        let behavior = &manifest.app.behaviors[0];
        let action = &manifest.app.actions[&behavior.action];
        let workflow = &manifest.app.workflows[&action.workflow];
        assert!(behavior_execution_ready(
            workflow.runner,
            &behavior.operations,
            &behavior.steps
        ));
    }

    #[test]
    fn an_empty_run_starts_at_the_first_unguarded_step() {
        let steps = town_square();
        let next = next_recipe_step(&steps, &[]).expect("resolves");
        assert!(matches!(next, AppBehaviorRecipeNext::Run(step) if step.id.as_str() == "gate"));
    }

    #[test]
    fn a_matching_guard_admits_the_next_step() {
        let steps = town_square();
        let completed = [done("gate", json!({ "decision": "engage" }))];
        let next = next_recipe_step(&steps, &completed).expect("resolves");
        assert!(matches!(next, AppBehaviorRecipeNext::Run(step) if step.id.as_str() == "compose"));
    }

    /// The behaviour the whole guard exists for: a `pass` verdict must not pay
    /// for a compose call, and must not read as a failure either.
    #[test]
    fn a_passed_gate_completes_the_recipe_without_composing() {
        let steps = town_square();
        let completed = [done("gate", json!({ "decision": "pass" }))];
        assert_eq!(
            next_recipe_step(&steps, &completed).expect("resolves"),
            AppBehaviorRecipeNext::Complete
        );
    }

    #[test]
    fn a_finished_recipe_is_complete() {
        let steps = town_square();
        let completed = [
            done("gate", json!({ "decision": "engage" })),
            done("compose", json!({ "body": "hello" })),
        ];
        assert_eq!(
            next_recipe_step(&steps, &completed).expect("resolves"),
            AppBehaviorRecipeNext::Complete
        );
    }

    /// A guard is permission for a model call. Anything short of an exact
    /// match withholds it.
    #[test]
    fn absent_null_and_wrongly_typed_values_never_match() {
        let steps = town_square();
        for output in [
            json!({}),
            json!({ "decision": null }),
            json!({ "decision": 1 }),
            json!({ "decision": ["engage"] }),
            json!({ "decision": "Engage" }),
            json!({ "decision": "engage " }),
            json!({ "other": "engage" }),
        ] {
            let completed = [done("gate", output.clone())];
            assert_eq!(
                next_recipe_step(&steps, &completed).expect("resolves"),
                AppBehaviorRecipeNext::Complete,
                "output {output} must not admit compose"
            );
        }
    }

    #[test]
    fn boolean_guards_compare_by_value_not_by_text() {
        let steps = vec![
            step("check", "gate_op", None),
            step("act", "act_op", Some(("check", "ready", "true"))),
        ];
        let yes = [done("check", json!({ "ready": true }))];
        assert!(matches!(
            next_recipe_step(&steps, &yes).expect("resolves"),
            AppBehaviorRecipeNext::Run(step) if step.id.as_str() == "act"
        ));
        let no = [done("check", json!({ "ready": false }))];
        assert_eq!(
            next_recipe_step(&steps, &no).expect("resolves"),
            AppBehaviorRecipeNext::Complete
        );
    }

    /// A step guarded on a step that was itself skipped is unreachable, not an
    /// error: a chain of guards ending in "do nothing" is a legitimate recipe.
    #[test]
    fn a_step_guarded_on_a_skipped_step_is_skipped_too() {
        let steps = vec![
            step("gate", "gate_op", None),
            step(
                "compose",
                "compose_op",
                Some(("gate", "decision", "engage")),
            ),
            step("polish", "polish_op", Some(("compose", "ok", "yes"))),
        ];
        let completed = [done("gate", json!({ "decision": "pass" }))];
        assert_eq!(
            next_recipe_step(&steps, &completed).expect("resolves"),
            AppBehaviorRecipeNext::Complete
        );
    }

    /// A later independent step is still reached when an earlier guarded one
    /// is skipped.
    #[test]
    fn skipping_a_guarded_step_does_not_end_the_recipe() {
        let steps = vec![
            step("gate", "gate_op", None),
            step(
                "compose",
                "compose_op",
                Some(("gate", "decision", "engage")),
            ),
            step("always", "always_op", None),
        ];
        let completed = [done("gate", json!({ "decision": "pass" }))];
        assert!(matches!(
            next_recipe_step(&steps, &completed).expect("resolves"),
            AppBehaviorRecipeNext::Run(step) if step.id.as_str() == "always"
        ));
    }

    /// Progress is durable state that survives restarts. Trusting a corrupt
    /// record would silently skip reviewed model turns, so it is validated
    /// before it is believed.
    #[test]
    fn corrupt_progress_is_refused_rather_than_skipped() {
        let steps = town_square();
        assert_eq!(
            next_recipe_step(&steps, &[done("ghost", json!({}))]).unwrap_err(),
            AppBehaviorRecipeError::UnknownStep("ghost".to_owned())
        );
        assert_eq!(
            next_recipe_step(
                &steps,
                &[
                    done("gate", json!({ "decision": "engage" })),
                    done("gate", json!({ "decision": "pass" })),
                ]
            )
            .unwrap_err(),
            AppBehaviorRecipeError::DuplicateStep("gate".to_owned())
        );
        assert_eq!(
            next_recipe_step(
                &steps,
                &[
                    done("compose", json!({ "body": "x" })),
                    done("gate", json!({ "decision": "engage" })),
                ]
            )
            .unwrap_err(),
            AppBehaviorRecipeError::OutOfOrder("gate".to_owned())
        );
    }

    #[test]
    fn a_recipe_with_no_steps_is_complete() {
        assert_eq!(
            next_recipe_step(&[], &[]).expect("resolves"),
            AppBehaviorRecipeNext::Complete
        );
    }

    /// The ceiling the owner reviewed is per RUN, and a run survives being
    /// interrupted. A continued run must therefore see the spend its earlier
    /// steps already made, not a fresh full grant.
    #[test]
    fn a_continued_run_keeps_spending_the_same_per_run_ceiling() {
        let completed = [
            spent("gate", json!({ "decision": "engage" }), 400),
            spent("compose", json!({ "body": "hello" }), 350),
        ];
        assert_eq!(remaining_run_output_tokens(1_000, &completed), 250);
        assert_eq!(remaining_run_output_tokens(1_000, &completed[..1]), 600);
        assert_eq!(remaining_run_output_tokens(1_000, &[]), 1_000);
    }

    /// Saturating, not wrapping: an over-spent run has nothing left, and must
    /// not come back around to a full budget.
    #[test]
    fn an_overspent_run_has_no_budget_left() {
        let completed = [spent("gate", json!({ "decision": "engage" }), u32::MAX)];
        assert_eq!(remaining_run_output_tokens(1_000, &completed), 0);
        assert_eq!(
            remaining_run_output_tokens(u64::from(u32::MAX) + 1_000, &completed),
            0
        );
    }

    /// The closed door: progress with no spend record cannot prove what the
    /// run has left, so it must not deserialize into a record claiming zero
    /// spend. Reading it as zero is exactly the widening this field prevents.
    #[test]
    fn progress_without_a_spend_record_does_not_deserialize() {
        let with_spend = json!({
            "step": "gate",
            "output": { "decision": "engage" },
            "spent_output_tokens": 400
        });
        assert_eq!(
            serde_json::from_value::<AppBehaviorStepOutcome>(with_spend)
                .expect("a complete record loads")
                .spent_output_tokens,
            400
        );

        let without_spend = json!({ "step": "gate", "output": { "decision": "engage" } });
        assert!(serde_json::from_value::<AppBehaviorStepOutcome>(without_spend).is_err());
    }
}
