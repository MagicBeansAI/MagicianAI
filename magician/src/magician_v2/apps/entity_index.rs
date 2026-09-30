//! Canonical typed-index projection shared by app reads and mutations.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use super::query_semantics::AppQueryScalarKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppScalarIndexProjection {
    pub value_kind: &'static str,
    pub text_value: Option<String>,
    pub integer_value: Option<i64>,
}

pub fn project_scalar_index(
    kind: AppQueryScalarKind,
    value: &Value,
) -> Result<AppScalarIndexProjection, AppEntityIndexError> {
    if value.is_null() {
        return Ok(AppScalarIndexProjection {
            value_kind: "null",
            text_value: None,
            integer_value: None,
        });
    }
    let (value_kind, text_value, integer_value) = match kind {
        AppQueryScalarKind::Text | AppQueryScalarKind::Markdown => (
            "text",
            Some(normalize_text(
                value.as_str().ok_or(AppEntityIndexError::TypeMismatch)?,
            )),
            None,
        ),
        AppQueryScalarKind::Enum => (
            "enum",
            Some(normalize_text(
                value.as_str().ok_or(AppEntityIndexError::TypeMismatch)?,
            )),
            None,
        ),
        AppQueryScalarKind::Reference => (
            "reference",
            Some(normalize_text(
                value.as_str().ok_or(AppEntityIndexError::TypeMismatch)?,
            )),
            None,
        ),
        AppQueryScalarKind::Integer => {
            if let Some(value) = value.as_i64() {
                ("integer", None, Some(value))
            } else {
                (
                    "integer",
                    Some(
                        value
                            .as_u64()
                            .ok_or(AppEntityIndexError::TypeMismatch)?
                            .to_string(),
                    ),
                    None,
                )
            }
        },
        AppQueryScalarKind::Decimal => (
            "decimal",
            Some(
                Decimal::from_str(&value.to_string())
                    .map_err(|_| AppEntityIndexError::TypeMismatch)?
                    .normalize()
                    .to_string(),
            ),
            None,
        ),
        AppQueryScalarKind::Boolean => (
            "boolean",
            None,
            Some(
                if value.as_bool().ok_or(AppEntityIndexError::TypeMismatch)? {
                    1
                } else {
                    0
                },
            ),
        ),
        AppQueryScalarKind::Timestamp => (
            "timestamp",
            Some(
                DateTime::parse_from_rfc3339(
                    value.as_str().ok_or(AppEntityIndexError::TypeMismatch)?,
                )
                .map_err(|_| AppEntityIndexError::TypeMismatch)?
                .with_timezone(&Utc)
                .to_rfc3339(),
            ),
            None,
        ),
    };
    Ok(AppScalarIndexProjection {
        value_kind,
        text_value,
        integer_value,
    })
}

pub fn normalized_search_text(
    kind: AppQueryScalarKind,
    value: &Value,
) -> Result<Option<String>, AppEntityIndexError> {
    if value.is_null() {
        return Ok(None);
    }
    if !matches!(
        kind,
        AppQueryScalarKind::Text | AppQueryScalarKind::Markdown
    ) {
        return Err(AppEntityIndexError::TypeMismatch);
    }
    Ok(Some(normalize_text(
        value.as_str().ok_or(AppEntityIndexError::TypeMismatch)?,
    )))
}

fn normalize_text(value: &str) -> String {
    value.nfkc().collect()
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AppEntityIndexError {
    #[error("app index value does not match its compiled scalar kind")]
    TypeMismatch,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn index_projection_is_exact_normalized_and_float_free() {
        assert_eq!(
            project_scalar_index(AppQueryScalarKind::Decimal, &json!(1.2500)).unwrap(),
            AppScalarIndexProjection {
                value_kind: "decimal",
                text_value: Some("1.25".to_owned()),
                integer_value: None,
            }
        );
        assert_eq!(
            project_scalar_index(AppQueryScalarKind::Integer, &json!(u64::MAX)).unwrap(),
            AppScalarIndexProjection {
                value_kind: "integer",
                text_value: Some(u64::MAX.to_string()),
                integer_value: None,
            }
        );
        assert_eq!(
            normalized_search_text(AppQueryScalarKind::Text, &json!("A\u{030a}"))
                .unwrap()
                .unwrap(),
            "Å"
        );
    }
}
