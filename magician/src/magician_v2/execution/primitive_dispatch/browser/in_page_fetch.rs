//! Browser-context transport for recipe reads that plain HTTP cannot satisfy.

use super::AgentBrowserSession;
use crate::magician_v2::api_mining::recipe::Transport;
use crate::magician_v2::api_mining::recipe_runner::{
    StepTransport, TransportRequest, TransportResponse,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub struct InPageFetchTransport {
    session: Arc<AgentBrowserSession>,
}

impl InPageFetchTransport {
    pub fn new(session: Arc<AgentBrowserSession>) -> Self {
        Self { session }
    }
}

#[async_trait::async_trait]
impl StepTransport for InPageFetchTransport {
    fn kind(&self) -> Transport {
        Transport::InPageFetch
    }

    async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
        // Browsers own these headers and reject attempts to set them. Cookies
        // flow through `credentials: include`; explicit auth/custom headers are
        // preserved.
        let headers: HashMap<_, _> = request
            .headers
            .iter()
            .filter(|(name, _)| {
                !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "cookie" | "host" | "content-length" | "origin" | "referer" | "user-agent"
                )
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let url = serde_json::to_string(&request.url).map_err(|error| error.to_string())?;
        let method = serde_json::to_string(&request.method).map_err(|error| error.to_string())?;
        let headers = serde_json::to_string(&headers).map_err(|error| error.to_string())?;
        let body = serde_json::to_string(&request.body).map_err(|error| error.to_string())?;
        let timeout_ms = request.timeout.as_millis().max(1);
        let script = format!(
            "(async()=>{{const c=new AbortController();const t=setTimeout(()=>c.abort(),{timeout_ms});try{{const r=await fetch({url},{{method:{method},headers:{headers},body:{body},credentials:'include',redirect:'manual',signal:c.signal}});const h={{}};r.headers.forEach((v,k)=>h[k]=v);const d=new TextDecoder();let text='',n=0;if(r.body){{const reader=r.body.getReader();while(true){{const chunk=await reader.read();if(chunk.done)break;n+=chunk.value.byteLength;if(n>8388608){{await reader.cancel();throw new Error('Task Recipe response limit exceeded');}}text+=d.decode(chunk.value,{{stream:true}});}}text+=d.decode();}}return JSON.stringify({{status:r.status,headers:h,body:text}});}}finally{{clearTimeout(t);}}}})()"
        );
        let timeout_secs = request.timeout.as_secs().saturating_add(1);
        let result = self
            .session
            .run_command_with_options(&["eval", &script], timeout_secs, &[])
            .await
            .map_err(|error| format!("in-page fetch command failed: {error}"))?;
        if !result.success {
            return Err(format!(
                "in-page fetch failed: {}",
                result.stderr.trim().chars().take(512).collect::<String>()
            ));
        }
        let value = decode_eval_result(result.parsed_json.as_ref(), &result.stdout)
            .ok_or_else(|| "in-page fetch returned an invalid response envelope".to_string())?;
        let status = value
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .ok_or_else(|| "in-page fetch response omitted status".to_string())?;
        let headers = value
            .get("headers")
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.to_ascii_lowercase(), value.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let body = value
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(TransportResponse {
            status,
            headers,
            body,
        })
    }
}

fn decode_eval_result(parsed: Option<&Value>, stdout: &str) -> Option<Value> {
    let candidate = parsed
        .cloned()
        .or_else(|| serde_json::from_str(stdout.trim()).ok())?;
    decode_value(candidate)
}

fn decode_value(value: Value) -> Option<Value> {
    match value {
        Value::String(encoded) => serde_json::from_str(&encoded).ok(),
        Value::Object(mut object) => {
            if object.contains_key("status") {
                Some(Value::Object(object))
            } else {
                object
                    .remove("data")
                    .or_else(|| object.remove("result"))
                    .and_then(decode_value)
            }
        },
        _ => None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_string_and_wrapped_eval_results() {
        let body = json!({"status": 200, "headers": {"x-a": "b"}, "body": "ok"});
        assert_eq!(
            decode_value(Value::String(body.to_string())),
            Some(body.clone())
        );
        assert_eq!(
            decode_value(serde_json::json!({"result": body.to_string()})),
            Some(body.clone())
        );
        assert_eq!(
            decode_value(json!({"success": true, "data": {"result": body.to_string()}})),
            Some(body)
        );
    }
}
