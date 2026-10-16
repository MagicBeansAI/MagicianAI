//! Bounded options for the lifecycle fixture runner. Profile names come from
//! runtime configuration; this module never carries a model/provider catalog.
use serde::{Deserialize, Serialize};

pub const LIFECYCLE_LIVE: &str = "test-memory-lifecycle-live-eval";
pub const LIFECYCLE_TESTS: &str = "test-memory-lifecycle";

pub fn has_run_reports(lane: &str) -> bool {
    matches!(lane, LIFECYCLE_LIVE | LIFECYCLE_TESTS)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalRunOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
}

impl EvalRunOptions {
    pub fn validate(&self, lane: &str) -> Result<(), String> {
        if lane != LIFECYCLE_LIVE && self != &Self::default() {
            return Err("this lane does not accept run options".into());
        }
        if self.repeats.is_some_and(|n| !(1..=3).contains(&n)) {
            return Err("repeats must be between 1 and 3".into());
        }
        if self
            .partition
            .as_deref()
            .is_some_and(|p| !matches!(p, "all" | "development" | "validation"))
        {
            return Err("partition must be all, development or validation".into());
        }
        if self.profiles.len() > 3 || self.profiles.iter().any(|p| !safe_token(p)) {
            return Err("select at most three configured profiles; names must use letters, numbers, '.', '_', ':' or '-'".into());
        }
        let mut unique = std::collections::HashSet::new();
        if self.profiles.iter().any(|p| !unique.insert(p)) {
            return Err("comparison profiles must be distinct".into());
        }
        Ok(())
    }

    /// Values have been validated before being included in a shell/make goal.
    pub fn make_arguments(&self, lane: &str, run_id: &str) -> String {
        assert!(self.validate(lane).is_ok());
        if !has_run_reports(lane) {
            return String::new();
        }
        assert!(safe_token(run_id));
        let mut args = format!(" EVAL_RUN_ID={run_id}");
        if lane == LIFECYCLE_LIVE {
            args.push_str(&format!(
                " MEMORY_LIFECYCLE_EVAL_REPEATS={} MEMORY_LIFECYCLE_EVAL_PARTITION={}",
                self.repeats.unwrap_or(3),
                self.partition.as_deref().unwrap_or("all")
            ));
            if !self.profiles.is_empty() {
                args.push_str(&format!(
                    " MEMORY_LIFECYCLE_EVAL_PROFILES={}",
                    self.profiles.join(",")
                ));
            }
        }
        args
    }
}

pub fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(i, c)| {
            c.is_ascii_alphanumeric() || (i > 0 && matches!(c, b'.' | b'_' | b':' | b'-'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_lanes_are_declared_in_the_makefile() {
        let registry = crate::evals::registry::parse_eval_registry(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../Makefile"
        )));
        for id in [LIFECYCLE_LIVE, LIFECYCLE_TESTS] {
            let lane = registry
                .lanes
                .iter()
                .find(|l| l.id == id)
                .expect("lifecycle lane registered");
            assert!(lane.parse_error.is_none());
            assert!(lane.report_dir.is_some());
            assert_eq!(
                lane.requires
                    .contains(&crate::evals::registry::EvalRequirement::ProviderKeys),
                id == LIFECYCLE_LIVE
            );
        }
    }

    #[test]
    fn lifecycle_options_are_bounded_and_never_shell_code() {
        for name in [
            "x$(touch bad)",
            "x`id`",
            "x;exit",
            "../x",
            "x y",
            "x=y",
            "-f",
            "x\ny",
        ] {
            assert!(EvalRunOptions {
                profiles: vec![name.into()],
                ..Default::default()
            }
            .validate(LIFECYCLE_LIVE)
            .is_err());
        }
        for repeats in [0, 4, 255] {
            assert!(EvalRunOptions {
                repeats: Some(repeats),
                ..Default::default()
            }
            .validate(LIFECYCLE_LIVE)
            .is_err());
        }
        let options = EvalRunOptions {
            profiles: vec!["candidate.a".into(), "another:v2".into()],
            repeats: Some(2),
            partition: Some("validation".into()),
        };
        assert!(options.validate(LIFECYCLE_LIVE).is_ok());
        assert!(options.validate(LIFECYCLE_TESTS).is_err());
        assert!(options
            .make_arguments(LIFECYCLE_LIVE, "run-123")
            .contains("MEMORY_LIFECYCLE_EVAL_PROFILES=candidate.a,another:v2"));
        assert!(serde_json::from_str::<EvalRunOptions>(r#"{"command":"echo bad"}"#).is_err());
    }
}
