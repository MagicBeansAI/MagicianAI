use super::body_template::{extract_body_template_params, extract_body_template_values};
use super::capability::ApiCapability;
use super::correlator::ActionEvent;
use super::router::extract_template_params;
use super::types::NetworkTraceEvent;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const TAKEOVER_READY_SAMPLES: usize = 2;
const RUNTIME_NOW_ISO_UTC: &str = "__magician_runtime:now_iso_utc";
const RUNTIME_TODAY_UTC: &str = "__magician_runtime:today_utc";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionParamBinding {
    pub action_param: String,
    pub capability_param: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionBinding {
    pub action_type: String,
    pub action_signature: String,
    /// Stable semantic signature derived from element attributes
    /// (`tag`, `name`, `data-testid`, `aria-label`, `role`,
    /// visible-text-truncated) rather than the exact CSS selector
    /// (which rotates whenever React re-renders or A/B tests flip).
    ///
    /// Phase 0 Gap 2 (v0.6.515): added as a fallback match key in
    /// `find_action_context_candidates`. The extension's observe
    /// handler is the producer (`observe.js` → element_metadata on
    /// the ActionEvent payload); when that data isn't present this
    /// field stays None and matching falls back to the exact
    /// signature path only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_path_template: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_bindings: Vec<ActionParamBinding>,
    /// Capability parameters learned from the correlated request that are
    /// stable but not supplied by the browser action. This covers backend
    /// constants that the miner parameterized structurally, such as version
    /// path segments (`/{id}/indexes/...` where `id = 1`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub default_params: HashMap<String, String>,
    #[serde(default = "default_binding_sample_count")]
    pub sample_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionContext {
    pub action_type: String,
    pub action_signature: String,
    /// Semantic signature for the current action (see ActionBinding
    /// docs). When the extension's observe handler ships element
    /// metadata, the executor computes this alongside the exact
    /// signature so registry matching can fall back from exact to
    /// semantic when selectors rotate.
    pub semantic_signature: Option<String>,
    pub page_origin: Option<String>,
    pub page_path_template: Option<String>,
    pub param_values: HashMap<String, String>,
    pub user_values: Vec<String>,
}

/// Quality of the match between a stored ActionBinding and a live
/// ActionContext. Used by the registry to rank candidates so an exact
/// signature match always wins over a semantic-only fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionMatchQuality {
    /// Semantic-only fallback (Phase 0 Gap 2). The stored exact
    /// signature differs but the semantic signature matches — likely
    /// the site rotated a CSS class / re-rendered, but the same
    /// semantic element (same `data-testid`, name, role, etc.) is
    /// still there.
    Semantic = 0,
    /// Exact CSS-selector match (historical behavior, highest
    /// confidence).
    Exact = 1,
}

impl ActionBinding {
    /// Best-quality match between this binding and a live context.
    /// `None` means no match. Used by `find_action_context_candidates`
    /// to rank candidates: exact matches outrank semantic-only.
    pub fn match_quality(&self, context: &ActionContext) -> Option<ActionMatchQuality> {
        if self.action_type != context.action_type {
            return None;
        }

        if let Some(expected_origin) = &self.page_origin {
            if context.page_origin.as_deref() != Some(expected_origin.as_str()) {
                return None;
            }
        }

        if let Some(expected_path) = &self.page_path_template {
            if context.page_path_template.as_deref() != Some(expected_path.as_str()) {
                return None;
            }
        }

        // Exact signature wins. Fall back to semantic when the exact
        // signature differs but both sides carry a non-empty semantic
        // signature that agrees.
        if self.action_signature == context.action_signature {
            Some(ActionMatchQuality::Exact)
        } else {
            match (
                self.semantic_signature.as_deref(),
                context.semantic_signature.as_deref(),
            ) {
                (Some(stored), Some(live)) if !stored.is_empty() && stored == live => {
                    Some(ActionMatchQuality::Semantic)
                },
                _ => None,
            }
        }
    }

    pub fn matches(&self, context: &ActionContext) -> bool {
        self.match_quality(context).is_some()
    }

    pub fn is_takeover_ready(&self) -> bool {
        self.sample_count >= TAKEOVER_READY_SAMPLES
    }

    pub fn is_takeover_ready_for(&self, capability: &ApiCapability) -> bool {
        self.is_takeover_ready() && self.can_resolve_capability(capability)
    }

    pub fn can_resolve_url_template(&self, url_template: &str) -> bool {
        self.can_resolve_param_names(&template_param_names(url_template))
    }

    pub fn can_resolve_capability(&self, capability: &ApiCapability) -> bool {
        self.can_resolve_param_names(&capability_required_params(capability))
    }

    fn can_resolve_param_names(&self, required: &HashSet<String>) -> bool {
        if required.is_empty() {
            return true;
        }

        let mut available = self.default_params.keys().cloned().collect::<HashSet<_>>();
        available.extend(
            self.param_bindings
                .iter()
                .map(|binding| binding.capability_param.clone()),
        );

        required.iter().all(|param| available.contains(param))
    }

    fn merge_from(&mut self, learned: &ActionBinding) {
        self.sample_count = self
            .sample_count
            .saturating_add(learned.sample_count.max(1));
        self.last_seen_at = match (self.last_seen_at, learned.last_seen_at) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };

        for binding in &learned.param_bindings {
            self.param_bindings.retain(|existing| {
                existing.action_param != binding.action_param
                    && existing.capability_param != binding.capability_param
            });
            self.param_bindings.push(binding.clone());
        }

        for (key, value) in &learned.default_params {
            self.default_params.insert(key.clone(), value.clone());
        }

        normalize_param_bindings(&mut self.param_bindings);
    }
}

pub fn merge_action_bindings(
    existing: &[ActionBinding],
    incoming: &[ActionBinding],
) -> Vec<ActionBinding> {
    let mut merged = existing.to_vec();

    for binding in incoming {
        if let Some(current) = merged.iter_mut().find(|candidate| {
            candidate.action_type == binding.action_type
                && candidate.action_signature == binding.action_signature
                && candidate.page_origin == binding.page_origin
                && candidate.page_path_template == binding.page_path_template
        }) {
            current.merge_from(binding);
        } else {
            let mut normalized = binding.clone();
            normalize_param_bindings(&mut normalized.param_bindings);
            merged.push(normalized);
        }
    }

    merged.sort_by(|left, right| {
        (
            &left.action_type,
            &left.action_signature,
            &left.page_origin,
            &left.page_path_template,
        )
            .cmp(&(
                &right.action_type,
                &right.action_signature,
                &right.page_origin,
                &right.page_path_template,
            ))
    });

    merged
}

pub fn action_context_from_event(event: &ActionEvent) -> Option<ActionContext> {
    let action_signature = event.action_signature.clone()?;
    Some(ActionContext {
        action_type: event.action_type.clone(),
        action_signature,
        // The extension's observe handler ships element metadata via
        // `ActionEvent::element_metadata`; the executor builds the
        // semantic signature from that and stores it on the event so
        // it survives the same trip the action_signature takes.
        semantic_signature: event.semantic_signature.clone(),
        page_origin: event.page_origin.clone(),
        page_path_template: event.page_path_template.clone(),
        param_values: event.action_params.clone(),
        user_values: event.user_values.clone(),
    })
}

pub fn select_ranked_action_binding_candidates<'a>(
    action: &ActionEvent,
    candidates: &[&'a super::correlator::CorrelatedRequest],
) -> Vec<&'a super::correlator::CorrelatedRequest> {
    let Some(action_context) = action_context_from_event(action) else {
        return Vec::new();
    };
    let has_user_values = !action_context.user_values.is_empty();

    let mut eligible = candidates
        .iter()
        .copied()
        .filter(|candidate| {
            let is_xhr_fetch = candidate
                .trace
                .resource_type
                .as_deref()
                .map(|resource_type| resource_type == "XHR" || resource_type == "Fetch")
                .unwrap_or(false);
            if !is_xhr_fetch || candidate.trace.status == 0 {
                return false;
            }

            if has_user_values {
                candidate.signals.value_match && candidate.confidence >= 0.35
            } else {
                candidate.confidence >= 0.55
                    && candidate.signals.timing
                    && candidate.signals.xhr_or_fetch
            }
        })
        .collect::<Vec<_>>();

    if eligible.is_empty() {
        return Vec::new();
    }

    eligible.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                usize::from(right.signals.value_match).cmp(&usize::from(left.signals.value_match))
            })
            .then_with(|| {
                usize::from(right.signals.frame_match).cmp(&usize::from(left.signals.frame_match))
            })
            .then_with(|| {
                right
                    .trace
                    .timing
                    .request_time
                    .partial_cmp(&left.trace.timing.request_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    if !has_user_values && eligible.len() > 1 {
        let top = eligible[0];
        let runner_up = eligible[1];
        if top.confidence - runner_up.confidence < 0.20 {
            return Vec::new();
        }
    }

    eligible
}

pub fn infer_action_binding(
    event: &ActionEvent,
    capability: &ApiCapability,
    trace: &NetworkTraceEvent,
) -> Option<ActionBinding> {
    let context = action_context_from_event(event)?;
    let capability_params = extract_capability_param_values(capability, trace);
    let mut param_bindings = Vec::new();
    let mut used_capability_params = HashSet::new();
    let mut context_params = context.param_values.iter().collect::<Vec<_>>();
    context_params.sort_by(|(left_key, _), (right_key, _)| {
        action_param_priority(left_key)
            .cmp(&action_param_priority(right_key))
            .reverse()
            .then_with(|| left_key.cmp(right_key))
    });

    for (action_param, action_value) in context_params {
        if action_value.is_empty() {
            continue;
        }

        if let Some(capability_param) = select_capability_param_for_action_value(
            action_param,
            action_value,
            &capability_params,
            &used_capability_params,
        ) {
            used_capability_params.insert(capability_param.clone());
            param_bindings.push(ActionParamBinding {
                action_param: action_param.clone(),
                capability_param,
            });
        }
    }

    let default_params = capability_params
        .into_iter()
        .filter(|(param, value)| !used_capability_params.contains(param) && !value.is_empty())
        .map(|(param, value)| {
            let value = dynamic_default_param_value(&param, &value).unwrap_or(value);
            (param, value)
        })
        .collect::<HashMap<_, _>>();

    Some(ActionBinding {
        action_type: context.action_type,
        action_signature: context.action_signature,
        // Inherit the semantic signature the executor computed at
        // observation time (from element metadata supplied by the
        // extension). Falls back to None when the extension hasn't
        // shipped metadata, in which case matching falls back to
        // exact-signature only.
        semantic_signature: context.semantic_signature,
        page_origin: context.page_origin,
        page_path_template: context.page_path_template,
        param_bindings,
        default_params,
        sample_count: 1,
        last_seen_at: Some(chrono::Utc::now().timestamp()),
    })
}

pub fn params_from_action_binding(
    binding: &ActionBinding,
    context: &ActionContext,
) -> Result<HashMap<String, String>, String> {
    let mut params = binding
        .default_params
        .iter()
        .map(|(key, value)| (key.clone(), resolve_runtime_default_param(value)))
        .collect::<HashMap<_, _>>();

    for mapping in &binding.param_bindings {
        let value = context
            .param_values
            .get(&mapping.action_param)
            .ok_or_else(|| {
                format!(
                    "Missing action parameter '{}' for action replay binding",
                    mapping.action_param
                )
            })?;
        params.insert(mapping.capability_param.clone(), value.clone());
    }

    Ok(params)
}

fn dynamic_default_param_value(param_name: &str, value: &str) -> Option<String> {
    let lowered = param_name.to_ascii_lowercase();
    if (lowered.contains("timestamp")
        || lowered.contains("time")
        || lowered.ends_with("_at")
        || lowered.contains("datetime"))
        && looks_like_iso_datetime(value)
    {
        return Some(RUNTIME_NOW_ISO_UTC.to_string());
    }

    if lowered.contains("date") && looks_like_yyyy_mm_dd(value) {
        return Some(RUNTIME_TODAY_UTC.to_string());
    }

    None
}

fn resolve_runtime_default_param(value: &str) -> String {
    match value {
        RUNTIME_NOW_ISO_UTC => {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        },
        RUNTIME_TODAY_UTC => chrono::Utc::now().date_naive().to_string(),
        _ => value.to_string(),
    }
}

fn looks_like_iso_datetime(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() >= 20
        && trimmed.as_bytes().get(4) == Some(&b'-')
        && trimmed.as_bytes().get(7) == Some(&b'-')
        && matches!(trimmed.as_bytes().get(10), Some(b'T') | Some(b' '))
        && trimmed
            .chars()
            .take(10)
            .enumerate()
            .all(|(idx, ch)| matches!(idx, 4 | 7) || ch.is_ascii_digit())
}

fn looks_like_yyyy_mm_dd(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() == 10
        && trimmed.as_bytes().get(4) == Some(&b'-')
        && trimmed.as_bytes().get(7) == Some(&b'-')
        && trimmed
            .chars()
            .enumerate()
            .all(|(idx, ch)| matches!(idx, 4 | 7) || ch.is_ascii_digit())
}

pub fn capability_requires_runtime_params(capability: &ApiCapability) -> bool {
    template_contains_runtime_placeholder(&capability.url_template)
        || capability
            .headers_template
            .values()
            .any(|value| template_contains_runtime_placeholder(value))
        || capability
            .body_template
            .as_deref()
            .map(body_template_contains_runtime_placeholder)
            .unwrap_or(false)
}

fn default_binding_sample_count() -> usize {
    1
}

fn capability_required_params(capability: &ApiCapability) -> HashSet<String> {
    let mut required = template_param_names(&capability.url_template);

    for value in capability.headers_template.values() {
        required.extend(template_param_names(value));
    }

    if let Some(body_template) = capability.body_template.as_deref() {
        required.extend(
            extract_body_template_params(body_template)
                .into_iter()
                .map(|param| param.name),
        );
    }

    required
}

fn template_param_names(value: &str) -> HashSet<String> {
    let mut params = HashSet::new();
    let bytes = value.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }

        if index + 1 < bytes.len() && bytes[index + 1] == b'{' {
            index += 2;
            continue;
        }

        let start = index + 1;
        let Some(relative_end) = value[start..].find('}') else {
            break;
        };
        let end = start + relative_end;
        let name = &value[start..end];
        if !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            params.insert(name.to_string());
        }
        index = end + 1;
    }

    params
}

fn normalize_param_bindings(bindings: &mut Vec<ActionParamBinding>) {
    let mut seen_action_params = HashSet::new();
    let mut seen_capability_params = HashSet::new();
    let mut normalized = Vec::with_capacity(bindings.len());

    for binding in bindings.drain(..).rev() {
        if !seen_action_params.insert(binding.action_param.clone()) {
            continue;
        }
        if !seen_capability_params.insert(binding.capability_param.clone()) {
            continue;
        }
        normalized.push(binding);
    }

    normalized.sort_by(|left, right| {
        (&left.action_param, &left.capability_param)
            .cmp(&(&right.action_param, &right.capability_param))
    });
    *bindings = normalized;
}

fn extract_capability_param_values(
    capability: &ApiCapability,
    trace: &NetworkTraceEvent,
) -> HashMap<String, String> {
    let mut values = extract_template_params(&capability.url_template, &trace.url);

    if let (Some(template), Some(body)) = (
        capability.body_template.as_deref(),
        trace.request_body.as_deref(),
    ) {
        if let Ok(body_values) = extract_body_template_values(template, body) {
            values.extend(body_values);
        }
    }

    values
}

fn select_capability_param_for_action_value(
    action_param: &str,
    action_value: &str,
    capability_params: &HashMap<String, String>,
    used_capability_params: &HashSet<String>,
) -> Option<String> {
    let mut candidates = capability_params
        .iter()
        .filter(|(param, value)| *value == action_value && !used_capability_params.contains(*param))
        .map(|(param, _)| param.clone())
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return None;
    }

    if candidates.len() == 1 {
        return candidates.pop();
    }

    candidates.sort_by(|left, right| {
        let left_score = capability_param_similarity_score(action_param, left);
        let right_score = capability_param_similarity_score(action_param, right);
        right_score.cmp(&left_score).then(left.cmp(right))
    });

    candidates.into_iter().next()
}

fn capability_param_similarity_score(action_param: &str, capability_param: &str) -> usize {
    let mut score = 0usize;
    let action_tokens = token_set(action_param);
    let capability_tokens = token_set(capability_param);

    for token in &action_tokens {
        if capability_tokens.contains(token) {
            score += 3;
        }
    }

    if action_param == "text"
        && capability_tokens
            .iter()
            .any(|token| ["q", "query", "search", "term"].contains(&token.as_str()))
    {
        score += 4;
    }

    if action_param == "value"
        && capability_tokens
            .iter()
            .any(|token| ["value", "id", "status", "option", "selected"].contains(&token.as_str()))
    {
        score += 4;
    }

    if action_param == "checked"
        && capability_tokens
            .iter()
            .any(|token| ["checked", "enabled", "selected", "active"].contains(&token.as_str()))
    {
        score += 4;
    }

    score
}

fn action_param_priority(action_param: &str) -> usize {
    if action_param.starts_with("page_query_") {
        1
    } else if action_param.starts_with("page_") {
        2
    } else {
        3
    }
}

fn token_set(raw: &str) -> HashSet<String> {
    let sanitized = sanitize_token(raw);
    sanitized
        .split('_')
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect()
}

fn sanitize_token(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn template_contains_runtime_placeholder(value: &str) -> bool {
    value.split('{').skip(1).any(|segment| {
        segment
            .split('}')
            .next()
            .is_some_and(|token| !token.is_empty())
    })
}

fn body_template_contains_runtime_placeholder(value: &str) -> bool {
    value.contains("{{") && value.contains("}}")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::ApiCapability;
    use crate::magician_v2::api_mining::types::{
        NetworkTraceEvent, RequestInitiator, RequestTiming,
    };

    fn make_trace(url: &str, request_body: Option<&str>) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: "req-1".to_string(),
            method: "POST".to_string(),
            url: url.to_string(),
            resource_type: Some("XHR".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]),
            request_body: request_body.map(|body| body.to_string()),
            response_headers: HashMap::new(),
            response_body: None,
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 0.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: None,
            },
            timestamp: 0,
            request_size: 0,
            response_size: 0,
            capture_source: None,
        }
    }

    #[test]
    fn test_infer_action_binding_maps_text_to_query_param() {
        let event = ActionEvent {
            action_id: "action-1".to_string(),
            action_type: "Type".to_string(),
            timestamp_ms: 0,
            frame_id: None,
            user_values: vec!["latest report".to_string()],
            page_url: Some("https://app.example.com/projects/123/search".to_string()),
            action_signature: Some(
                "selector=input%5Bname%3D%27q%27%5D|submit=true|iframe=".to_string(),
            ),
            semantic_signature: None,
            element_metadata: HashMap::new(),
            action_params: HashMap::from([("text".to_string(), "latest report".to_string())]),
            page_origin: Some("https://app.example.com".to_string()),
            page_path_template: Some("/projects/{id}/search".to_string()),
        };

        let mut capability = ApiCapability::new(
            "search".to_string(),
            "https://api.example.com".to_string(),
            "GET".to_string(),
            "https://api.example.com/search?q={q}".to_string(),
        );
        capability.add_sample("req-2".to_string());
        capability.add_sample("req-3".to_string());

        let trace = make_trace("https://api.example.com/search?q=latest%20report", None);
        let binding = infer_action_binding(&event, &capability, &trace).expect("binding");
        assert_eq!(binding.param_bindings.len(), 1);
        assert_eq!(binding.param_bindings[0].action_param, "text");
        assert_eq!(binding.param_bindings[0].capability_param, "q");
    }

    #[test]
    fn test_infer_action_binding_maps_page_context_param() {
        let event = ActionEvent {
            action_id: "action-2".to_string(),
            action_type: "Click".to_string(),
            timestamp_ms: 0,
            frame_id: None,
            user_values: Vec::new(),
            page_url: Some("https://app.example.com/projects/123/details".to_string()),
            action_signature: Some(
                "selector=button%5Bdata-action%3D%27refresh%27%5D|button=Left|count=1|iframe="
                    .to_string(),
            ),
            semantic_signature: None,
            element_metadata: HashMap::new(),
            action_params: HashMap::from([("page_project_id".to_string(), "123".to_string())]),
            page_origin: Some("https://app.example.com".to_string()),
            page_path_template: Some("/projects/{id}/details".to_string()),
        };

        let mut capability = ApiCapability::new(
            "project_summary".to_string(),
            "https://api.example.com".to_string(),
            "GET".to_string(),
            "https://api.example.com/projects/{project_id}/summary".to_string(),
        );
        capability.add_sample("req-2".to_string());
        capability.add_sample("req-3".to_string());

        let trace = make_trace("https://api.example.com/projects/123/summary", None);
        let binding = infer_action_binding(&event, &capability, &trace).expect("binding");
        assert_eq!(binding.param_bindings.len(), 1);
        assert_eq!(binding.param_bindings[0].action_param, "page_project_id");
        assert_eq!(binding.param_bindings[0].capability_param, "project_id");
    }

    #[test]
    fn test_infer_action_binding_maps_page_query_to_body_template_param() {
        let event = ActionEvent {
            action_id: "action-3".to_string(),
            action_type: "PageLoad".to_string(),
            timestamp_ms: 0,
            frame_id: None,
            user_values: vec!["rust".to_string()],
            page_url: Some("https://hn.algolia.com/?q=rust".to_string()),
            action_signature: Some("https://hn.algolia.com/|query_keys=q".to_string()),
            semantic_signature: Some("page_load:https://hn.algolia.com/?q".to_string()),
            element_metadata: HashMap::new(),
            action_params: HashMap::from([
                ("page_query_q".to_string(), "rust".to_string()),
                ("query".to_string(), "rust".to_string()),
            ]),
            page_origin: Some("https://hn.algolia.com".to_string()),
            page_path_template: Some("/".to_string()),
        };

        let mut capability = ApiCapability::new(
            "hn_search".to_string(),
            "https://uj5wyc0l7x-dsn.algolia.net".to_string(),
            "POST".to_string(),
            "https://uj5wyc0l7x-dsn.algolia.net/{id}/indexes/Item_dev/query".to_string(),
        );
        capability.body_template = Some(
            r#"{"query":{{string:body_payload_query}},"page":0,"hitsPerPage":30}"#.to_string(),
        );
        capability.add_sample("req-2".to_string());
        capability.add_sample("req-3".to_string());

        let trace = make_trace(
            "https://uj5wyc0l7x-dsn.algolia.net/1/indexes/Item_dev/query",
            Some(r#"{"query":"rust","page":0,"hitsPerPage":30}"#),
        );
        let binding = infer_action_binding(&event, &capability, &trace).expect("binding");

        assert_eq!(binding.param_bindings.len(), 1);
        assert_eq!(binding.param_bindings[0].action_param, "query");
        assert_eq!(
            binding.param_bindings[0].capability_param,
            "body_payload_query"
        );
        assert_eq!(binding.default_params.get("id"), Some(&"1".to_string()));
    }

    #[test]
    fn test_capability_requires_runtime_params_detects_url_and_body_templates() {
        let mut capability = ApiCapability::new(
            "project_summary".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/projects/{project_id}/summary".to_string(),
        );
        assert!(capability_requires_runtime_params(&capability));

        capability.url_template = "https://api.example.com/projects/summary".to_string();
        capability.body_template = Some(
            r#"{"query":"query","variables":{"id":"{{string:body_variables_id}}"}}"#.to_string(),
        );
        assert!(capability_requires_runtime_params(&capability));
    }

    #[test]
    fn test_params_from_action_binding_builds_placeholder_map() {
        let binding = ActionBinding {
            action_type: "Type".to_string(),
            action_signature: "selector=input|submit=false|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_bindings: vec![ActionParamBinding {
                action_param: "text".to_string(),
                capability_param: "q".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(0),
        };
        let context = ActionContext {
            action_type: "Type".to_string(),
            action_signature: binding.action_signature.clone(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_values: HashMap::from([("text".to_string(), "foo".to_string())]),
            user_values: vec!["foo".to_string()],
        };

        let params = params_from_action_binding(&binding, &context).expect("params");
        assert_eq!(params.get("q"), Some(&"foo".to_string()));
    }

    #[test]
    fn test_params_from_action_binding_includes_default_params() {
        let binding = ActionBinding {
            action_type: "PageLoad".to_string(),
            action_signature: "https://example.com/search|query_keys=q".to_string(),
            semantic_signature: None,
            page_origin: Some("https://example.com".to_string()),
            page_path_template: Some("/search".to_string()),
            param_bindings: vec![ActionParamBinding {
                action_param: "query".to_string(),
                capability_param: "body_query".to_string(),
            }],
            default_params: HashMap::from([("id".to_string(), "1".to_string())]),
            sample_count: 2,
            last_seen_at: Some(0),
        };
        let context = ActionContext {
            action_type: "PageLoad".to_string(),
            action_signature: binding.action_signature.clone(),
            semantic_signature: None,
            page_origin: binding.page_origin.clone(),
            page_path_template: binding.page_path_template.clone(),
            param_values: HashMap::from([("query".to_string(), "rust".to_string())]),
            user_values: vec!["rust".to_string()],
        };

        let params = params_from_action_binding(&binding, &context).expect("params");
        assert_eq!(params.get("id").map(String::as_str), Some("1"));
        assert_eq!(params.get("body_query").map(String::as_str), Some("rust"));
    }

    #[test]
    fn test_infer_action_binding_marks_timestamp_default_as_runtime_now() {
        let event = ActionEvent {
            action_id: "action-clock".to_string(),
            action_type: "Click".to_string(),
            timestamp_ms: 0,
            frame_id: None,
            user_values: vec![],
            page_url: Some("https://app.example.com/dashboard".to_string()),
            action_signature: Some("eval_click|label=Clock In|iframe=".to_string()),
            semantic_signature: Some("click:text:clock in".to_string()),
            element_metadata: HashMap::new(),
            action_params: HashMap::from([("target".to_string(), "Clock In".to_string())]),
            page_origin: Some("https://app.example.com".to_string()),
            page_path_template: Some("/dashboard".to_string()),
        };

        let mut capability = ApiCapability::new(
            "clock_in".to_string(),
            "https://app.example.com".to_string(),
            "POST".to_string(),
            "https://app.example.com/api/clock-in".to_string(),
        );
        capability.body_template =
            Some(r#"{"timestamp":{{string:body_payload_timestamp}}}"#.to_string());
        let trace = make_trace(
            "https://app.example.com/api/clock-in",
            Some(r#"{"timestamp":"2026-06-16T19:56:34.298Z"}"#),
        );

        let binding = infer_action_binding(&event, &capability, &trace).expect("binding");
        assert_eq!(
            binding
                .default_params
                .get("body_payload_timestamp")
                .map(String::as_str),
            Some(RUNTIME_NOW_ISO_UTC)
        );

        let context = action_context_from_event(&event).unwrap();
        let params = params_from_action_binding(&binding, &context).expect("params");
        let resolved = params.get("body_payload_timestamp").unwrap();
        assert_ne!(resolved, RUNTIME_NOW_ISO_UTC);
        assert!(looks_like_iso_datetime(resolved));
    }

    #[test]
    fn test_takeover_ready_requires_all_capability_template_params() {
        let mut capability = ApiCapability::new(
            "search".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/orgs/{org_id}/search?q={query}".to_string(),
        );
        capability.headers_template.insert(
            "x-workspace".to_string(),
            "workspace-{workspace_id}".to_string(),
        );
        capability.body_template =
            Some(r#"{"query":"static","limit":{{number:body_limit}}}"#.to_string());

        let incomplete = ActionBinding {
            action_type: "Type".to_string(),
            action_signature: "selector=input|submit=false|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_bindings: vec![ActionParamBinding {
                action_param: "text".to_string(),
                capability_param: "query".to_string(),
            }],
            default_params: HashMap::from([
                ("org_id".to_string(), "acme".to_string()),
                ("workspace_id".to_string(), "main".to_string()),
            ]),
            sample_count: 2,
            last_seen_at: Some(0),
        };
        assert!(!incomplete.is_takeover_ready_for(&capability));

        let complete = ActionBinding {
            default_params: HashMap::from([
                ("org_id".to_string(), "acme".to_string()),
                ("workspace_id".to_string(), "main".to_string()),
                ("body_limit".to_string(), "10".to_string()),
            ]),
            ..incomplete
        };
        assert!(complete.is_takeover_ready_for(&capability));
    }

    #[test]
    fn test_merge_action_bindings_replaces_conflicting_action_param_mapping() {
        let existing = ActionBinding {
            action_type: "Type".to_string(),
            action_signature: "selector=input|submit=false|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_bindings: vec![ActionParamBinding {
                action_param: "text".to_string(),
                capability_param: "id".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(1),
        };
        let incoming = ActionBinding {
            action_type: existing.action_type.clone(),
            action_signature: existing.action_signature.clone(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_bindings: vec![ActionParamBinding {
                action_param: "text".to_string(),
                capability_param: "q".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 1,
            last_seen_at: Some(2),
        };

        let merged = merge_action_bindings(&[existing], &[incoming]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].sample_count, 3);
        assert_eq!(merged[0].param_bindings.len(), 1);
        assert_eq!(merged[0].param_bindings[0].action_param, "text");
        assert_eq!(merged[0].param_bindings[0].capability_param, "q");
    }

    #[test]
    fn test_merge_action_bindings_replaces_conflicting_capability_param_mapping() {
        let existing = ActionBinding {
            action_type: "Click".to_string(),
            action_signature: "selector=button|button=Left|count=1|iframe=".to_string(),
            semantic_signature: None,
            page_origin: Some("https://app.example.com".to_string()),
            page_path_template: Some("/projects/{id}".to_string()),
            param_bindings: vec![ActionParamBinding {
                action_param: "page_project_id".to_string(),
                capability_param: "entity_id".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(1),
        };
        let incoming = ActionBinding {
            action_type: existing.action_type.clone(),
            action_signature: existing.action_signature.clone(),
            semantic_signature: None,
            page_origin: existing.page_origin.clone(),
            page_path_template: existing.page_path_template.clone(),
            param_bindings: vec![ActionParamBinding {
                action_param: "page_org_id".to_string(),
                capability_param: "entity_id".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 1,
            last_seen_at: Some(2),
        };

        let merged = merge_action_bindings(&[existing], &[incoming]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].param_bindings.len(), 1);
        assert_eq!(merged[0].param_bindings[0].action_param, "page_org_id");
        assert_eq!(merged[0].param_bindings[0].capability_param, "entity_id");
    }
}
