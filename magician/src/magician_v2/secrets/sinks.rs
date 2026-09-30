//! Approved credential sinks (secure HITL plan §5.2, P4 Task 4.2).
//!
//! A reference (`[REF:<key>]` / `[REDACTED:<key>]`) becomes a value only where
//! an adapter delivers that value to its destination and owns what comes back:
//! an HTTP header or body field, a process's stdin, a typed browser fill, a
//! pack parameter the capability declares as a credential sink. Everywhere
//! else — a file, SQL, a sub-goal or delegation context, a handover, a sleep
//! reason, a shell command line or environment, an HTTP URL or query — a
//! reference is refused with the sink named, so the model learns which typed
//! operation to use instead. The decision is made from the *shape* of the
//! action, never from the string the reference sits in.
//!
//! The walker does not decide what a reference becomes: a
//! [`ReferenceResolver`] does (a plain scoped read for a password, a one-time
//! reserve-and-consume for a code, or `None` to leave the marker for a later
//! pass). The walker only guarantees the resolver is asked for approved sinks
//! and never for refused ones.
use crate::magician_v2::execution::capability::ImplementationType;
use crate::magician_v2::execution::{ExecutableAction, FileAction};
use crate::magician_v2::json_traversal::map_json_strings_owned;
use magicvault_core::injection::{REDACTED_PREFIX, REF_PREFIX};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

/// Where a reference may be lowered into a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSink {
    /// An HTTP request header value.
    HttpHeader { name: String },
    /// The HTTP request body (a field inside it; the whole body is one sink).
    HttpBody,
    /// Bytes written to a spawned process's stdin.
    ProcessStdin,
    /// A parameter of a capability pack. The resolver decides whether this
    /// capability owns a credential sink for the parameter (the typed browser
    /// fill, a governed CLI skill with a stdin contract); an undeclared one
    /// is refused there.
    PackParam { capability: String, param: String },
}

impl CredentialSink {
    pub fn label(&self) -> String {
        match self {
            Self::HttpHeader { name } => format!("HTTP header `{name}`"),
            Self::HttpBody => "HTTP body".to_string(),
            Self::ProcessStdin => "process stdin".to_string(),
            Self::PackParam { capability, param } => {
                format!("parameter `{param}` of `{capability}`")
            },
        }
    }
}

/// Where a reference sat that may never carry a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedSink {
    FilePath,
    FileContent,
    Sql,
    DatabasePath,
    ShellCommand,
    ShellEnvironment,
    WorkingDirectory,
    HttpUrl,
    HttpHeaderName,
    HttpContentType,
    PackImplementation,
    SubGoal,
    DelegationContext,
    HandoverContext,
    SleepReason,
}

impl RefusedSink {
    pub fn label(self) -> &'static str {
        match self {
            Self::FilePath => "a file path",
            Self::FileContent => "file content",
            Self::Sql => "a SQL statement",
            Self::DatabasePath => "a database path",
            Self::ShellCommand => "a shell command line",
            Self::ShellEnvironment => "a process environment variable",
            Self::WorkingDirectory => "a working directory",
            Self::HttpUrl => "an HTTP URL or query string",
            Self::HttpHeaderName => "an HTTP header name",
            Self::HttpContentType => "an HTTP content type",
            Self::PackImplementation => "a capability's command or environment",
            Self::SubGoal => "a sub-goal",
            Self::DelegationContext => "a delegation context",
            Self::HandoverContext => "a handover context",
            Self::SleepReason => "a sleep reason",
        }
    }

    /// What to do instead, in the model's vocabulary.
    pub fn advice(self) -> &'static str {
        match self {
            Self::ShellCommand | Self::ShellEnvironment | Self::WorkingDirectory => {
                "pass the value on the process's stdin (the `stdin` field) instead"
            },
            Self::HttpUrl | Self::HttpHeaderName | Self::HttpContentType => {
                "send the value in a request header or the request body instead"
            },
            Self::PackImplementation => "use the capability's declared credential input instead",
            Self::FilePath
            | Self::FileContent
            | Self::Sql
            | Self::DatabasePath
            | Self::SubGoal
            | Self::DelegationContext
            | Self::HandoverContext
            | Self::SleepReason => {
                "a secret is delivered only by the typed operation that needs it"
            },
        }
    }
}

/// Why an action could not be lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoweringError {
    /// A reference sits where a value may never be written.
    UnsupportedSink { sink: RefusedSink, key: String },
    /// A reference sits in a pack parameter the capability does not declare
    /// as a credential input.
    UndeclaredPackSink {
        capability: String,
        param: String,
        key: String,
    },
    /// The material behind a reference cannot be delivered now (expired,
    /// already claimed, spent, missing).
    Material { key: String, reason: String },
}

impl fmt::Display for LoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSink { sink, key } => write!(
                formatter,
                "Action blocked: the secret reference for '{key}' cannot be written to {}; {}.",
                sink.label(),
                sink.advice()
            ),
            Self::UndeclaredPackSink {
                capability,
                param,
                key,
            } => write!(
                formatter,
                "Action blocked: the secret reference for '{key}' cannot be passed as parameter \
                 '{param}' of '{capability}', which declares no credential input; use the typed \
                 operation that delivers the value."
            ),
            Self::Material { key, reason } => write!(
                formatter,
                "Action blocked: the secret for '{key}' cannot be delivered ({reason}). Ask the user \
                 for it again with need_user_input."
            ),
        }
    }
}

impl std::error::Error for LoweringError {}

/// Every name the compiled `http` pack answers to: the pack itself and the
/// per-method aliases the catalog generates for it
/// (`compiled_providers::…tool_infos`). A CLOSED set on purpose — a prefix
/// match would hand a third-party capability that merely starts with `http_`
/// the HTTP adapter's sinks, and a command-backed pack registered under its
/// exact name wins the registry lookup, so the value would ride its argv.
pub(crate) const HTTP_PACK_CAPABILITIES: [&str; 9] = [
    "http",
    "http_get",
    "http_post",
    "http_put",
    "http_patch",
    "http_delete",
    "http_head",
    "http_options",
    "http_request",
];

/// The compiled `http` pack and its per-method variants (`http_get`,
/// `http_post`, …): the HTTP adapter an agentic run's model reaches.
pub fn is_http_pack(capability: &str) -> bool {
    HTTP_PACK_CAPABILITIES.contains(&capability)
}

/// The pack parameter the provider's lowering reads into
/// `HttpAction::carries_credential`, set here when a reference was lowered
/// into the request. A model setting it itself only makes its own request
/// stricter.
pub const HTTP_PACK_CARRIES_CREDENTIAL: &str = "__carries_credential";

/// The sink one parameter of the `http` pack is, if it is one: the body
/// delivers to the request's origin; the URL, the method, the content type
/// and the rest never carry a value. `headers` is a sink too, but not a
/// single one — it is the request's whole header map, and it is lowered by
/// [`lower_http_pack_headers`] so that each value goes into its own header's
/// sink and no name can carry a reference.
pub fn http_pack_sink(capability: &str, param: &str) -> Option<CredentialSink> {
    if !is_http_pack(capability) {
        return None;
    }
    match param {
        "body" => Some(CredentialSink::HttpBody),
        _ => None,
    }
}

/// Lower the `http` pack's `headers` parameter — the adapter's header map,
/// in either shape the pack accepts: the object a model writes, or the JSON
/// string the provider's own parameter coercion turns it into.
///
/// Each **name** is refused exactly as the native arm refuses one
/// (`RefusedSink::HttpHeaderName`): a reference in a header name is not a
/// delivery to a destination, it is a request the person never described,
/// and the adapter would send it as the name. Each **value** lowers under
/// its own header's sink, so the audit line names the header the material
/// went into rather than the parameter it arrived in. A blob that is neither
/// shape cannot be split into names and values, so it is refused whole
/// rather than lowered blind.
fn lower_http_pack_headers<R: ReferenceResolver>(
    value: Value,
    resolver: &mut R,
) -> Result<(Value, bool), LoweringError> {
    let was_string = value.is_string();
    let map = match &value {
        Value::Object(map) => map.clone(),
        Value::String(text) => match serde_json::from_str::<serde_json::Map<String, Value>>(text) {
            Ok(map) => map,
            Err(_) => {
                refuse_value(&value, RefusedSink::HttpHeaderName)?;
                return Ok((value, false));
            },
        },
        _ => {
            refuse_value(&value, RefusedSink::HttpHeaderName)?;
            return Ok((value, false));
        },
    };
    let mut delivered = false;
    let mut lowered = serde_json::Map::with_capacity(map.len());
    for (name, header_value) in map {
        refuse(&name, RefusedSink::HttpHeaderName)?;
        let sink = CredentialSink::HttpHeader { name: name.clone() };
        let mut failure = None;
        let header_value = map_json_strings_owned(
            header_value,
            |text| {
                if failure.is_some() {
                    return text;
                }
                match lower_text(&text, &sink, resolver) {
                    Ok(lowered) => {
                        delivered |= lowered != text;
                        lowered
                    },
                    Err(error) => {
                        failure = Some(error);
                        text
                    },
                }
            },
            |_, _| None,
        );
        if let Some(error) = failure {
            return Err(error);
        }
        lowered.insert(name, header_value);
    }
    let lowered = Value::Object(lowered);
    let lowered = if was_string {
        Value::String(
            serde_json::to_string(&lowered).map_err(|error| LoweringError::Material {
                key: "headers".to_string(),
                reason: format!("the request's headers could not be rebuilt: {error}"),
            })?,
        )
    } else {
        lowered
    };
    Ok((lowered, delivered))
}

/// Where an `http` pack request delivers: the origin of its `url`, if the
/// parameter is a URL.
pub fn http_pack_destination(resolved_params: &HashMap<String, Value>) -> Option<String> {
    resolved_params
        .get("url")
        .and_then(Value::as_str)
        .and_then(crate::magician_v2::execution::native_executors::http_origin)
}

/// The ephemeral key under which a run records where material collected for
/// a challenge may be delivered (P4). A companion entry beside the material,
/// in the same scope, holding the destination string — value-free, bounded
/// like the material, gone with the scope. Never a placeholder anyone
/// references.
pub fn binding_key(key: &str) -> String {
    format!("__bound__:{key}")
}

/// Where the material under `key` may be delivered, if a challenge bound it.
pub fn bound_destination(
    store: &crate::magician_v2::secrets::SecretStore,
    scope: &str,
    key: &str,
) -> Option<String> {
    store.get_ephemeral_scoped(scope, &binding_key(key))
}

/// Whether material bound to `bound` may be delivered to `destination`: an
/// unbound entry goes anywhere its sink allows; a bound one only to exactly
/// its destination, which a sink with no verifiable destination never is.
pub fn destination_admits(bound: Option<&str>, destination: Option<&str>) -> bool {
    match bound {
        None => true,
        Some(bound) => destination == Some(bound),
    }
}

/// Decides what a reference in an approved sink becomes.
pub trait ReferenceResolver {
    /// `Ok(Some(value))` replaces the marker; `Ok(None)` leaves it in place
    /// for a later pass (a one-time key deferred to dispatch, or a key this
    /// scope does not hold, which the caller's unresolved-reference check
    /// then refuses).
    fn resolve(
        &mut self,
        key: &str,
        sink: &CredentialSink,
    ) -> Result<Option<String>, LoweringError>;
}

/// The markers in `text`, in order: `(marker, key)`.
pub fn placeholders(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text;
    loop {
        let next = [REF_PREFIX, REDACTED_PREFIX]
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|at| (at, *prefix)))
            .min_by_key(|(at, _)| *at);
        let Some((at, prefix)) = next else { break };
        let after_prefix = &rest[at + prefix.len()..];
        let Some(end) = after_prefix.find(']') else {
            break;
        };
        let key = &after_prefix[..end];
        if !key.is_empty() && !key.contains('[') {
            found.push((format!("{prefix}{key}]"), key.to_string()));
        }
        rest = &after_prefix[end + 1..];
    }
    found
}

fn first_key(text: &str) -> Option<String> {
    placeholders(text).into_iter().next().map(|(_, key)| key)
}

fn refuse(text: &str, sink: RefusedSink) -> Result<(), LoweringError> {
    match first_key(text) {
        Some(key) => Err(LoweringError::UnsupportedSink { sink, key }),
        None => Ok(()),
    }
}

fn refuse_path(path: &std::path::Path, sink: RefusedSink) -> Result<(), LoweringError> {
    refuse(&path.to_string_lossy(), sink)
}

fn refuse_value(value: &Value, sink: RefusedSink) -> Result<(), LoweringError> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::String(text) => refuse(text, sink)?,
            Value::Array(values) => pending.extend(values.iter()),
            Value::Object(values) => {
                for (key, value) in values {
                    refuse(key, sink)?;
                    pending.push(value);
                }
            },
            _ => {},
        }
    }
    Ok(())
}

fn lower_text<R: ReferenceResolver>(
    text: &str,
    sink: &CredentialSink,
    resolver: &mut R,
) -> Result<String, LoweringError> {
    let mut lowered = text.to_string();
    for (marker, key) in placeholders(text) {
        if let Some(value) = resolver.resolve(&key, sink)? {
            lowered = lowered.replacen(&marker, &value, 1);
        }
    }
    Ok(lowered)
}

fn refuse_implementation(implementation: &ImplementationType) -> Result<(), LoweringError> {
    let sink = RefusedSink::PackImplementation;
    let strings = |values: &[String]| values.iter().try_for_each(|value| refuse(value, sink));
    let map = |values: &HashMap<String, String>| {
        values
            .iter()
            .try_for_each(|(key, value)| refuse(key, sink).and_then(|()| refuse(value, sink)))
    };
    match implementation {
        ImplementationType::Composite { steps } => steps.iter().try_for_each(|step| {
            refuse(&step.tool, sink)?;
            step.parameters
                .iter()
                .try_for_each(|(key, value)| refuse(key, sink).and_then(|()| refuse(value, sink)))
        }),
        ImplementationType::Compiled { provider_name } => refuse(provider_name, sink),
        ImplementationType::Primitive {
            command,
            cwd,
            env,
            suffix_args,
            ..
        } => {
            command.as_deref().map_or(Ok(()), strings)?;
            cwd.as_deref().map_or(Ok(()), |cwd| refuse(cwd, sink))?;
            map(env)?;
            strings(suffix_args)
        },
        ImplementationType::Command {
            program,
            fixed_args,
            suffix_args,
            env,
            ..
        } => {
            refuse(program, sink)?;
            strings(fixed_args)?;
            strings(suffix_args)?;
            map(env)
        },
    }
}

/// Lower every reference that sits in an approved sink through `resolver`;
/// refuse the action if any reference sits where a value may never go.
pub fn lower_references<R: ReferenceResolver>(
    action: ExecutableAction,
    resolver: &mut R,
) -> Result<ExecutableAction, LoweringError> {
    Ok(match action {
        ExecutableAction::File(action) => {
            match &action {
                FileAction::Read { path, .. }
                | FileAction::Delete { path, .. }
                | FileAction::Exists { path }
                | FileAction::CreateDir { path }
                | FileAction::List { path, .. } => refuse_path(path, RefusedSink::FilePath)?,
                FileAction::Write { path, content, .. } | FileAction::Append { path, content } => {
                    refuse_path(path, RefusedSink::FilePath)?;
                    refuse(content, RefusedSink::FileContent)?;
                },
                FileAction::Copy {
                    source,
                    destination,
                }
                | FileAction::Move {
                    source,
                    destination,
                } => {
                    refuse_path(source, RefusedSink::FilePath)?;
                    refuse_path(destination, RefusedSink::FilePath)?;
                },
            }
            ExecutableAction::File(action)
        },
        ExecutableAction::Http(mut http) => {
            refuse(&http.url, RefusedSink::HttpUrl)?;
            if let Some(content_type) = http.content_type.as_deref() {
                refuse(content_type, RefusedSink::HttpContentType)?;
            }
            let mut headers = HashMap::with_capacity(http.headers.len());
            let mut delivered = false;
            for (name, value) in http.headers {
                refuse(&name, RefusedSink::HttpHeaderName)?;
                let sink = CredentialSink::HttpHeader { name: name.clone() };
                let lowered = lower_text(&value, &sink, resolver)?;
                delivered |= lowered != value;
                headers.insert(name, lowered);
            }
            http.headers = headers;
            http.body = match http.body {
                Some(body) => {
                    let lowered = lower_text(&body, &CredentialSink::HttpBody, resolver)?;
                    delivered |= lowered != body;
                    Some(lowered)
                },
                None => None,
            };
            // The adapter reads this: a request carrying a credential never
            // follows a redirect, so the value reaches its bound origin only.
            http.carries_credential |= delivered;
            ExecutableAction::Http(http)
        },
        ExecutableAction::Bash(mut bash) => {
            refuse(&bash.command, RefusedSink::ShellCommand)?;
            for (key, value) in &bash.env {
                refuse(key, RefusedSink::ShellEnvironment)?;
                refuse(value, RefusedSink::ShellEnvironment)?;
            }
            if let Some(dir) = bash.working_dir.as_deref() {
                refuse_path(dir, RefusedSink::WorkingDirectory)?;
            }
            bash.stdin = match bash.stdin {
                Some(stdin) => Some(lower_text(&stdin, &CredentialSink::ProcessStdin, resolver)?),
                None => None,
            };
            ExecutableAction::Bash(bash)
        },
        ExecutableAction::DuckDb(duckdb) => {
            refuse(&duckdb.sql, RefusedSink::Sql)?;
            if let Some(database) = duckdb.database.as_deref() {
                refuse(database, RefusedSink::DatabasePath)?;
            }
            ExecutableAction::DuckDb(duckdb)
        },
        ExecutableAction::Pack {
            capability_name,
            implementation,
            resolved_params,
        } => {
            refuse_implementation(&implementation)?;
            let http_pack = is_http_pack(&capability_name);
            let mut lowered = HashMap::with_capacity(resolved_params.len());
            let mut delivered = false;
            for (param, value) in resolved_params {
                // The request's header map is lowered per header, so a
                // reference can never become a header NAME — the refusal the
                // native arm below makes, which a single map-wide sink would
                // have dropped.
                if http_pack && param == "headers" {
                    let (headers, any) = lower_http_pack_headers(value, resolver)?;
                    delivered |= any;
                    lowered.insert(param, headers);
                    continue;
                }
                // The compiled `http` pack is the HTTP adapter the model
                // reaches (the native lane is a plan-step lowering of the
                // same request): its body is the body sink, and its URL is a
                // URL.
                let sink = match http_pack_sink(&capability_name, &param) {
                    Some(sink) => sink,
                    // The URL and the content type are the two parameters
                    // with a refusal of their own — the same two the native
                    // arm above names — because a reference written there is
                    // a specific mistake the model can correct (send it in a
                    // header or the body). The pack's remaining parameters
                    // are simply not declared credential inputs and are
                    // refused as such.
                    None if http_pack && matches!(param.as_str(), "url" | "content_type") => {
                        let refused = if param == "url" {
                            RefusedSink::HttpUrl
                        } else {
                            RefusedSink::HttpContentType
                        };
                        refuse_value(&value, refused)?;
                        lowered.insert(param, value);
                        continue;
                    },
                    None => CredentialSink::PackParam {
                        capability: capability_name.clone(),
                        param: param.clone(),
                    },
                };
                let mut failure = None;
                let value = map_json_strings_owned(
                    value,
                    |text| {
                        if failure.is_some() {
                            return text;
                        }
                        match lower_text(&text, &sink, resolver) {
                            Ok(lowered) => {
                                delivered |= lowered != text;
                                lowered
                            },
                            Err(error) => {
                                failure = Some(error);
                                text
                            },
                        }
                    },
                    |_, _| None,
                );
                if let Some(error) = failure {
                    return Err(error);
                }
                lowered.insert(param, value);
            }
            if http_pack && delivered {
                // The provider's lowering reads this into
                // `HttpAction::carries_credential`: a request carrying a
                // credential never follows a redirect off its origin.
                lowered.insert(HTTP_PACK_CARRIES_CREDENTIAL.to_string(), Value::Bool(true));
            }
            ExecutableAction::Pack {
                capability_name,
                implementation,
                resolved_params: lowered,
            }
        },
        ExecutableAction::SpawnSubGoal { goal, budget } => {
            refuse(&goal, RefusedSink::SubGoal)?;
            ExecutableAction::SpawnSubGoal { goal, budget }
        },
        ExecutableAction::DelegateToAgent { targets } => {
            for target in &targets {
                refuse(&target.context, RefusedSink::DelegationContext)?;
                if let Some(data) = target.input_data.as_ref() {
                    refuse_value(data, RefusedSink::DelegationContext)?;
                }
            }
            ExecutableAction::DelegateToAgent { targets }
        },
        ExecutableAction::HandoverToAgent {
            target_agent_id,
            context,
        } => {
            refuse(&context, RefusedSink::HandoverContext)?;
            ExecutableAction::HandoverToAgent {
                target_agent_id,
                context,
            }
        },
        ExecutableAction::SleepUntil { wake_at, reason } => {
            if let Some(reason) = reason.as_deref() {
                refuse(reason, RefusedSink::SleepReason)?;
            }
            ExecutableAction::SleepUntil { wake_at, reason }
        },
    })
}

/// The keys of every reference still in the action's approved sinks, in
/// order of first appearance. (A reference in a refused sink never gets this
/// far: [`lower_references`] refuses the action first.)
pub fn remaining_reference_keys(action: &ExecutableAction) -> Vec<String> {
    struct Collect(Vec<String>);
    impl ReferenceResolver for Collect {
        fn resolve(
            &mut self,
            key: &str,
            _: &CredentialSink,
        ) -> Result<Option<String>, LoweringError> {
            if !self.0.iter().any(|seen| seen == key) {
                self.0.push(key.to_string());
            }
            Ok(None)
        }
    }
    let mut collect = Collect(Vec::new());
    // The walk only reads through the resolver; a refusal cannot happen for
    // an action that was already lowered, and an un-lowered one reports the
    // keys it can see before the refusal.
    let _ = lower_references(action.clone(), &mut collect);
    collect.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::{BashAction, HttpAction, HttpMethod};

    struct Table(HashMap<&'static str, &'static str>, Vec<(String, String)>);
    impl ReferenceResolver for Table {
        fn resolve(
            &mut self,
            key: &str,
            sink: &CredentialSink,
        ) -> Result<Option<String>, LoweringError> {
            self.1.push((key.to_string(), sink.label()));
            Ok(self.0.get(key).map(|value| value.to_string()))
        }
    }

    fn table() -> Table {
        Table(
            HashMap::from([("password", "pw-canary"), ("otp", "042917")]),
            Vec::new(),
        )
    }

    fn http_action(url: &str, headers: &[(&str, &str)], body: Option<&str>) -> ExecutableAction {
        ExecutableAction::Http(HttpAction {
            method: HttpMethod::Post,
            url: url.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.map(str::to_string),
            content_type: Some("application/json".into()),
            timeout_secs: None,
            follow_redirects: true,
            carries_credential: false,
        })
    }

    fn bash(command: &str, env: &[(&str, &str)], stdin: Option<&str>) -> ExecutableAction {
        ExecutableAction::Bash(BashAction {
            command: command.into(),
            working_dir: None,
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            timeout_secs: None,
            capture_output: true,
            stdin: stdin.map(str::to_string),
        })
    }

    #[test]
    fn markers_are_found_with_their_keys() {
        assert_eq!(
            placeholders("x=[REF:password] y=[REDACTED:otp] z=[REF:]"),
            vec![
                ("[REF:password]".to_string(), "password".to_string()),
                ("[REDACTED:otp]".to_string(), "otp".to_string()),
            ]
        );
        assert!(placeholders("no markers here").is_empty());
    }

    #[test]
    fn an_http_header_and_body_lower_and_the_url_does_not() {
        let lowered = lower_references(
            http_action(
                "https://api.example.test/login",
                &[("Authorization", "Bearer [REF:password]")],
                Some(r#"{"code":"[REF:otp]"}"#),
            ),
            &mut table(),
        )
        .unwrap();
        let ExecutableAction::Http(http) = lowered else {
            panic!()
        };
        assert_eq!(http.headers["Authorization"], "Bearer pw-canary");
        assert_eq!(http.body.as_deref(), Some(r#"{"code":"042917"}"#));
        assert!(
            http.carries_credential,
            "the adapter must know a credential rides this request"
        );
        let ExecutableAction::Http(plain) = lower_references(
            http_action("https://api.example.test/", &[("Accept", "*/*")], None),
            &mut table(),
        )
        .unwrap() else {
            panic!()
        };
        assert!(!plain.carries_credential);

        for (url, sink) in [
            (
                "https://api.example.test/login?code=[REF:otp]",
                RefusedSink::HttpUrl,
            ),
            (
                "https://[REF:password]@api.example.test/",
                RefusedSink::HttpUrl,
            ),
        ] {
            let error = lower_references(http_action(url, &[], None), &mut table()).unwrap_err();
            assert!(
                matches!(error, LoweringError::UnsupportedSink { sink: s, .. } if s == sink),
                "{error}"
            );
        }
        let error = lower_references(
            http_action(
                "https://api.example.test/",
                &[("X-[REF:password]", "x")],
                None,
            ),
            &mut table(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LoweringError::UnsupportedSink {
                sink: RefusedSink::HttpHeaderName,
                ..
            }
        ));
    }

    #[test]
    fn a_process_takes_a_secret_on_stdin_only() {
        let lowered = lower_references(
            bash("gh auth login --with-token", &[], Some("[REF:password]\n")),
            &mut table(),
        )
        .unwrap();
        let ExecutableAction::Bash(bash_action) = lowered else {
            panic!()
        };
        assert_eq!(bash_action.stdin.as_deref(), Some("pw-canary\n"));

        let error = lower_references(bash("curl -u me:[REF:password] x", &[], None), &mut table())
            .unwrap_err();
        assert!(matches!(
            error,
            LoweringError::UnsupportedSink {
                sink: RefusedSink::ShellCommand,
                ..
            }
        ));
        assert!(error.to_string().contains("stdin"), "{error}");
        let error = lower_references(
            bash("run", &[("TOKEN", "[REF:password]")], None),
            &mut table(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LoweringError::UnsupportedSink {
                sink: RefusedSink::ShellEnvironment,
                ..
            }
        ));
    }

    #[test]
    fn every_other_sink_is_refused_before_any_resolution() {
        use crate::magician_v2::execution::actions::DelegationTargetRequest;
        use crate::magician_v2::execution::DuckDbAction;
        let cases: Vec<(ExecutableAction, RefusedSink)> = vec![
            (
                ExecutableAction::File(FileAction::Write {
                    path: "/tmp/creds".into(),
                    content: "pw=[REF:password]".into(),
                    create_dirs: false,
                }),
                RefusedSink::FileContent,
            ),
            (
                ExecutableAction::File(FileAction::Read {
                    path: "/tmp/[REF:password]".into(),
                    encoding: None,
                }),
                RefusedSink::FilePath,
            ),
            (
                ExecutableAction::DuckDb(DuckDbAction {
                    sql: "insert into t values ('[REF:password]')".into(),
                    database: None,
                    output_format: "json".into(),
                    timeout_secs: None,
                }),
                RefusedSink::Sql,
            ),
            (
                ExecutableAction::SpawnSubGoal {
                    goal: "log in with [REF:password]".into(),
                    budget: 1,
                },
                RefusedSink::SubGoal,
            ),
            (
                ExecutableAction::DelegateToAgent {
                    targets: vec![DelegationTargetRequest {
                        target_agent_id: "helper".into(),
                        context: "use [REF:otp]".into(),
                        input_artifact_ids: vec![],
                        input_data: None,
                        depth: None,
                        timeout_secs: None,
                        spend_token_ids: vec![],
                        required_capability: None,
                        expected_artifacts: Vec::new(),
                    }],
                },
                RefusedSink::DelegationContext,
            ),
            (
                ExecutableAction::HandoverToAgent {
                    target_agent_id: "helper".into(),
                    context: "[REF:password]".into(),
                },
                RefusedSink::HandoverContext,
            ),
            (
                ExecutableAction::SleepUntil {
                    wake_at: chrono::Utc::now(),
                    reason: Some("[REF:otp]".into()),
                },
                RefusedSink::SleepReason,
            ),
        ];
        for (action, sink) in cases {
            let mut resolver = table();
            let error = lower_references(action, &mut resolver).unwrap_err();
            assert!(
                matches!(error, LoweringError::UnsupportedSink { sink: s, .. } if s == sink),
                "{error}"
            );
            assert!(
                resolver.1.is_empty(),
                "a refused sink must not consult the resolver"
            );
        }
    }

    #[test]
    fn a_pack_parameter_is_offered_to_the_resolver_with_its_capability() {
        let action = ExecutableAction::Pack {
            capability_name: "browser__secure_prompt_fill".into(),
            implementation: ImplementationType::Compiled {
                provider_name: "browser".into(),
            },
            resolved_params: HashMap::from([(
                "fields".to_string(),
                serde_json::json!([{ "field_name": "code", "value": "[REF:otp]" }]),
            )]),
        };
        let mut resolver = table();
        let lowered = lower_references(action, &mut resolver).unwrap();
        let ExecutableAction::Pack {
            resolved_params, ..
        } = lowered
        else {
            panic!()
        };
        assert_eq!(resolved_params["fields"][0]["value"], "042917");
        assert_eq!(
            resolver.1,
            vec![(
                "otp".to_string(),
                "parameter `fields` of `browser__secure_prompt_fill`".to_string()
            )]
        );
    }

    /// P7 qualification found the model's `http` tool call is the compiled
    /// `http` pack, not the plan-step `HttpAction`: its body and headers are
    /// the HTTP sinks, its URL is a URL, and a delivered reference marks the
    /// request as carrying a credential for the provider's lowering.
    #[test]
    fn the_http_pack_is_the_http_adapter_body_and_headers_deliver_and_the_url_never_does() {
        let pack = |params: serde_json::Value| ExecutableAction::Pack {
            capability_name: "http_post".into(),
            implementation: ImplementationType::Compiled {
                provider_name: "http".into(),
            },
            resolved_params: params
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        let mut resolver = table();
        let lowered = lower_references(
            pack(serde_json::json!({
                "url": "https://api.example.test/login",
                "method": "POST",
                "headers": {"Authorization": "Bearer [REF:otp]"},
                "body": "{\"password\":\"[REF:password]\"}",
            })),
            &mut resolver,
        )
        .unwrap();
        let ExecutableAction::Pack {
            resolved_params, ..
        } = lowered
        else {
            panic!()
        };
        assert_eq!(resolved_params["body"], "{\"password\":\"pw-canary\"}");
        assert_eq!(resolved_params["headers"]["Authorization"], "Bearer 042917");
        assert_eq!(resolved_params[HTTP_PACK_CARRIES_CREDENTIAL], true);
        assert_eq!(
            http_pack_destination(&resolved_params).as_deref(),
            Some("https://api.example.test")
        );
        // Parameter order is a map's, so the pair is compared as a set. Each
        // header lowers under its OWN header's sink, which is what the audit
        // line names.
        let mut sinks: Vec<String> = resolver.1.iter().map(|(_, sink)| sink.clone()).collect();
        sinks.sort();
        assert_eq!(sinks, vec!["HTTP body", "HTTP header `Authorization`"]);
        // A reference in a header NAME is refused exactly as the native arm
        // refuses it — the pack door must not turn the map into one sink.
        for headers in [
            serde_json::json!({"X-[REF:password]": "x"}),
            serde_json::json!("{\"X-[REF:password]\": \"x\"}"),
        ] {
            let mut resolver = table();
            let error = lower_references(
                pack(serde_json::json!({"url": "https://api.example.test/", "method": "GET", "headers": headers})),
                &mut resolver,
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    LoweringError::UnsupportedSink {
                        sink: RefusedSink::HttpHeaderName,
                        ..
                    }
                ),
                "{error}"
            );
            assert!(
                resolver.1.is_empty(),
                "nothing is read for a refused header name"
            );
        }
        // A headers blob that is neither shape cannot be split into names and
        // values, so it is refused whole rather than lowered blind.
        let mut resolver = table();
        let error = lower_references(
            pack(serde_json::json!({"url": "https://api.example.test/", "method": "GET", "headers": "Authorization: Basic [REF:password]"})),
            &mut resolver,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                LoweringError::UnsupportedSink {
                    sink: RefusedSink::HttpHeaderName,
                    ..
                }
            ),
            "{error}"
        );
        // The content type is refused on the pack door too, the way the
        // native arm refuses it.
        let mut resolver = table();
        let error = lower_references(
            pack(serde_json::json!({"url": "https://api.example.test/", "method": "POST", "content_type": "application/[REF:password]"})),
            &mut resolver,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                LoweringError::UnsupportedSink {
                    sink: RefusedSink::HttpContentType,
                    ..
                }
            ),
            "{error}"
        );
        // A reference in the URL is refused with the sink named; nothing is read.
        let mut resolver = table();
        let err = lower_references(pack(serde_json::json!({"url": "https://api.example.test/?code=[REF:otp]", "method": "GET"})), &mut resolver).unwrap_err();
        assert!(
            matches!(
                err,
                LoweringError::UnsupportedSink {
                    sink: RefusedSink::HttpUrl,
                    ..
                }
            ),
            "{err}"
        );
        assert!(resolver.1.is_empty());
        // Any other parameter of the pack is offered to the resolver as an
        // undeclared pack parameter — the sink whose own refusal the
        // preparation resolver raises — not as the URL.
        let mut resolver = table();
        let _ = lower_references(
            pack(serde_json::json!({"url": "https://api.example.test/", "method": "POST", "token": "[REF:password]"})),
            &mut resolver,
        )
        .expect("the test resolver answers every sink");
        assert_eq!(
            resolver
                .1
                .iter()
                .map(|(_, sink)| sink.clone())
                .collect::<Vec<_>>(),
            vec!["parameter `token` of `http_post`"],
            "an undeclared http-pack parameter is offered as a pack parameter"
        );
        // The flag the adapter reads must survive the dispatch strips that
        // remove the model's own `__*` fields — without it the request follows
        // redirects with the credential on board.
        assert!(
            crate::magician_v2::execution::flat_loop::dispatch::model_visible_params(
                &serde_json::json!({"url": "https://api.example.test/", HTTP_PACK_CARRIES_CREDENTIAL: true})
            )
            .contains_key(HTTP_PACK_CARRIES_CREDENTIAL),
            "the credential flag was stripped before dispatch"
        );
        // A request without a reference is not marked.
        let plain = lower_references(
            pack(serde_json::json!({"url": "https://api.example.test/", "method": "GET"})),
            &mut table(),
        )
        .unwrap();
        let ExecutableAction::Pack {
            resolved_params, ..
        } = plain
        else {
            panic!()
        };
        assert!(!resolved_params.contains_key(HTTP_PACK_CARRIES_CREDENTIAL));
        assert!(
            !is_http_pack("browser__secure_prompt_fill")
                && is_http_pack("http")
                && is_http_pack("http_get")
        );
    }

    #[test]
    fn remaining_keys_report_what_a_later_pass_still_owes() {
        let action = http_action(
            "https://api.example.test/",
            &[("X-Code", "[REF:otp]")],
            Some("[REF:password] [REF:otp]"),
        );
        assert_eq!(
            remaining_reference_keys(&action),
            vec!["otp".to_string(), "password".to_string()]
        );
        let lowered = lower_references(action, &mut table()).unwrap();
        assert!(remaining_reference_keys(&lowered).is_empty());
    }
}
