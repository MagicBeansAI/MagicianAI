//! Phase 3 parameter resolver: turn a `ParamSource` into the concrete string
//! value for the HTTP request. Four sources:
//!
//! - `Literal { value }` → returns `value` verbatim.
//! - `DataFlow { data_flow_id }` → resolves to the JSONPath extraction of
//!   the referenced DataFlow's `source_step` response.
//! - `UserInput { input_key }` → looks up `input_key` in the caller-supplied
//!   `user_inputs` map.
//! - `SessionAuth { auth_scheme }` → looks up `auth_scheme` in the caller-
//!   supplied `session_values` map.

use crate::magician_v2::api_mining::workflow::ParamSource;
use crate::magician_v2::api_mining::workflow_replay::jsonpath;
use crate::magician_v2::api_mining::workflow_replay::types::ReplayError;
use serde_json::Value;
use std::collections::HashMap;

/// Subset of `DataFlow` needed for parameter resolution; carrying the full
/// struct would couple this module to workflow.rs unnecessarily.
#[derive(Debug, Clone)]
pub struct DataFlowLookup {
    pub source_step: String,
    pub source_path: String,
}

pub fn resolve_param_source(
    source: &ParamSource,
    param_name: &str,
    step_id: &str,
    data_flows: &HashMap<String, DataFlowLookup>,
    prior_responses: &HashMap<String, Value>,
    user_inputs: &HashMap<String, String>,
    session_values: &HashMap<String, String>,
) -> Result<String, ReplayError> {
    match source {
        ParamSource::Literal { value } => Ok(value.clone()),
        ParamSource::DataFlow { data_flow_id } => {
            let lookup =
                data_flows
                    .get(data_flow_id)
                    .ok_or_else(|| ReplayError::ParamResolutionFailed {
                        step_id: step_id.to_string(),
                        param: param_name.to_string(),
                        reason: format!("unknown data_flow_id: {data_flow_id}"),
                    })?;
            let source_body = prior_responses.get(&lookup.source_step).ok_or_else(|| {
                ReplayError::ParamResolutionFailed {
                    step_id: step_id.to_string(),
                    param: param_name.to_string(),
                    reason: format!(
                        "source step {} has not run / produced no response",
                        lookup.source_step
                    ),
                }
            })?;
            jsonpath::extract_jsonpath(source_body, &lookup.source_path).ok_or_else(|| {
                ReplayError::JsonPathMiss {
                    step_id: step_id.to_string(),
                    path: lookup.source_path.clone(),
                    source_step: lookup.source_step.clone(),
                }
            })
        },
        ParamSource::UserInput { input_key } => {
            user_inputs
                .get(input_key)
                .cloned()
                .ok_or_else(|| ReplayError::ParamResolutionFailed {
                    step_id: step_id.to_string(),
                    param: param_name.to_string(),
                    reason: format!("user_input '{input_key}' not supplied"),
                })
        },
        ParamSource::SessionAuth { auth_scheme } => session_values
            .get(auth_scheme)
            .cloned()
            .ok_or_else(|| ReplayError::AuthMissing {
                step_id: step_id.to_string(),
                scheme: auth_scheme.clone(),
            }),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::workflow::ParamSource;
    use serde_json::json;

    fn empty_prior() -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
    fn empty_inputs() -> HashMap<String, String> {
        HashMap::new()
    }
    fn empty_session() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn literal_returns_stored_value() {
        let src = ParamSource::Literal {
            value: "csv".to_string(),
        };
        let out = resolve_param_source(
            &src,
            "format",
            "step_0",
            &HashMap::new(),
            &empty_prior(),
            &empty_inputs(),
            &empty_session(),
        )
        .unwrap();
        assert_eq!(out, "csv");
    }

    #[test]
    fn data_flow_extracts_from_prior_response() {
        let src = ParamSource::DataFlow {
            data_flow_id: "df_1".to_string(),
        };
        let mut flows = HashMap::new();
        flows.insert(
            "df_1".to_string(),
            DataFlowLookup {
                source_step: "step_0".to_string(),
                source_path: "$.token".to_string(),
            },
        );
        let mut prior = HashMap::new();
        prior.insert("step_0".to_string(), json!({"token":"sk_xyz"}));
        let out = resolve_param_source(
            &src,
            "auth",
            "step_1",
            &flows,
            &prior,
            &empty_inputs(),
            &empty_session(),
        )
        .unwrap();
        assert_eq!(out, "sk_xyz");
    }

    #[test]
    fn data_flow_extracts_array_index_path_from_prior_response() {
        let src = ParamSource::DataFlow {
            data_flow_id: "df_1".to_string(),
        };
        let mut flows = HashMap::new();
        flows.insert(
            "df_1".to_string(),
            DataFlowLookup {
                source_step: "step_0".to_string(),
                source_path: "$.hits[0].objectID".to_string(),
            },
        );
        let mut prior = HashMap::new();
        prior.insert(
            "step_0".to_string(),
            json!({"hits":[{"objectID":"22238335"}]}),
        );
        let out = resolve_param_source(
            &src,
            "item_id",
            "step_1",
            &flows,
            &prior,
            &empty_inputs(),
            &empty_session(),
        )
        .unwrap();
        assert_eq!(out, "22238335");
    }

    #[test]
    fn data_flow_returns_error_when_source_missing() {
        let src = ParamSource::DataFlow {
            data_flow_id: "df_1".to_string(),
        };
        let mut flows = HashMap::new();
        flows.insert(
            "df_1".to_string(),
            DataFlowLookup {
                source_step: "step_never_ran".to_string(),
                source_path: "$.token".to_string(),
            },
        );
        let result = resolve_param_source(
            &src,
            "auth",
            "step_1",
            &flows,
            &empty_prior(),
            &empty_inputs(),
            &empty_session(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn data_flow_returns_error_when_data_flow_id_unknown() {
        let src = ParamSource::DataFlow {
            data_flow_id: "df_unknown".to_string(),
        };
        let result = resolve_param_source(
            &src,
            "auth",
            "step_1",
            &HashMap::new(),
            &empty_prior(),
            &empty_inputs(),
            &empty_session(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn user_input_pulled_from_inputs_map() {
        let src = ParamSource::UserInput {
            input_key: "report_name".to_string(),
        };
        let mut inputs = HashMap::new();
        inputs.insert("report_name".to_string(), "Weekly Q3".to_string());
        let out = resolve_param_source(
            &src,
            "name",
            "step_0",
            &HashMap::new(),
            &empty_prior(),
            &inputs,
            &empty_session(),
        )
        .unwrap();
        assert_eq!(out, "Weekly Q3");
    }

    #[test]
    fn user_input_missing_returns_error() {
        let src = ParamSource::UserInput {
            input_key: "missing_key".to_string(),
        };
        let result = resolve_param_source(
            &src,
            "name",
            "step_0",
            &HashMap::new(),
            &empty_prior(),
            &empty_inputs(),
            &empty_session(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn session_auth_pulled_from_session_map() {
        let src = ParamSource::SessionAuth {
            auth_scheme: "bearer".to_string(),
        };
        let mut session = HashMap::new();
        session.insert("bearer".to_string(), "tok_abc".to_string());
        let out = resolve_param_source(
            &src,
            "Authorization",
            "step_0",
            &HashMap::new(),
            &empty_prior(),
            &empty_inputs(),
            &session,
        )
        .unwrap();
        assert_eq!(out, "tok_abc");
    }

    #[test]
    fn session_auth_missing_returns_auth_missing_error() {
        let src = ParamSource::SessionAuth {
            auth_scheme: "bearer".to_string(),
        };
        let result = resolve_param_source(
            &src,
            "Authorization",
            "step_0",
            &HashMap::new(),
            &empty_prior(),
            &empty_inputs(),
            &empty_session(),
        );
        match result.unwrap_err() {
            ReplayError::AuthMissing { scheme, .. } => assert_eq!(scheme, "bearer"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn data_flow_jsonpath_miss_returns_jsonpath_miss_error() {
        let src = ParamSource::DataFlow {
            data_flow_id: "df_1".to_string(),
        };
        let mut flows = HashMap::new();
        flows.insert(
            "df_1".to_string(),
            DataFlowLookup {
                source_step: "step_0".to_string(),
                source_path: "$.missing_field".to_string(),
            },
        );
        let mut prior = HashMap::new();
        prior.insert("step_0".to_string(), json!({"other":"x"}));
        let result = resolve_param_source(
            &src,
            "auth",
            "step_1",
            &flows,
            &prior,
            &empty_inputs(),
            &empty_session(),
        );
        match result.unwrap_err() {
            ReplayError::JsonPathMiss {
                path, source_step, ..
            } => {
                assert_eq!(path, "$.missing_field");
                assert_eq!(source_step, "step_0");
            },
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
