//! User-facing Work links. API endpoints and artifact paths are separate contracts.

/// Link to an existing task, optionally focusing an exact execution.
pub fn task_href(task_id: &str, execution_id: Option<&str>) -> String {
    let mut href = format!("/work?selected={}", urlencoding::encode(task_id));
    if let Some(execution_id) = execution_id {
        href.push_str("&execution_id=");
        href.push_str(&urlencoding::encode(execution_id));
    }
    href
}

pub fn task_record_href(
    task: &crate::magician_v2::artifact_v2::models::TaskRecord,
    execution_id: Option<&str>,
) -> String {
    let mut href = task_href(&task.manifest.task_id, execution_id);
    if task.manifest.lifecycle == crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal {
        href.push_str("&type=internal");
    }
    href
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_links_keep_record_and_run_identity_in_encoded_query_values() {
        assert_eq!(
            task_href("task/one +&?#", None),
            "/work?selected=task%2Fone%20%2B%26%3F%23"
        );
        assert_eq!(
            task_href("task%2Fone", Some("exec/two +&?#")),
            "/work?selected=task%252Fone&execution_id=exec%2Ftwo%20%2B%26%3F%23"
        );
    }
}
