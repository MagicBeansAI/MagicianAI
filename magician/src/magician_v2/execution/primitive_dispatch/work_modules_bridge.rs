//! Authenticated local transport for the existing work-modules skill.
//! The reviewed route table is shared with the standalone adapter. Tokens are
//! scoped, per-call, host-only and revoked on every completion/cancellation.
use super::{exec_ctx::PrimitiveExecCtx, runner::PrimitiveToolResult};
use crate::magician_v2::auth::bot_tokens::{bot_token_registry, BotGrant};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::OnceLock,
    time::{Duration, Instant},
};

static ENDPOINT: OnceLock<SocketAddr> = OnceLock::new();
type Routes = BTreeMap<String, (String, String, Vec<String>)>;
static ROUTES: OnceLock<Routes> = OnceLock::new();

/// Called by the server host with its actual listener port, never by tool input.
pub fn install(host: &str, port: u16) -> anyhow::Result<()> {
    anyhow::ensure!(
        port != 0,
        "work-module bridge requires a bound listener port"
    );
    let mut address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("work-module listener address unavailable"))?;
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        });
    }
    if let Some(held) = ENDPOINT.get() {
        anyhow::ensure!(*held == address, "work-module bridge endpoint changed");
        return Ok(());
    }
    ENDPOINT
        .set(address)
        .map_err(|_| anyhow::anyhow!("work-module bridge registration raced"))
}

struct TokenLease(String);
impl Drop for TokenLease {
    fn drop(&mut self) {
        bot_token_registry().revoke(&self.0);
    }
}

fn routes() -> &'static Routes {
    ROUTES.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../skillshub/work-modules/routes.json"
        )))
        .expect("reviewed work-module route table")
    })
}

fn request(action: &str, input: Value) -> Result<(reqwest::Method, String, Value), String> {
    let (method, path, fields) = routes().get(action).ok_or("unknown work-module action")?;
    let mut values = input
        .as_object()
        .cloned()
        .ok_or("work-module input must be an object")?;
    let mut path = path.clone();
    for name in ["run_id", "negotiation_id", "obligation_id"] {
        let marker = format!("{{{name}}}");
        if path.contains(&marker) {
            let id = values
                .remove(name)
                .and_then(|x| x.as_str().map(str::to_owned))
                .ok_or("work-module path ID is required")?;
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|x| x.is_ascii_alphanumeric() || matches!(x, b'_' | b'-'))
            {
                return Err("work-module path ID is invalid".into());
            }
            path = path.replace(&marker, &id);
        }
    }
    if !path.starts_with("/work/") || path.contains('{') {
        return Err("invalid reviewed work-module route".into());
    }
    let body = Value::Object(
        values
            .into_iter()
            .filter(|(key, _)| fields.contains(key))
            .collect(),
    );
    let method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        _ => return Err("invalid reviewed work-module method".into()),
    };
    Ok((method, path, body))
}

fn verify_reply_receiver(action: &str, input: &Value, caller: Option<&str>) -> Result<(), String> {
    if action == "bind_offer" {
        if let Some(receiver) = input.get("reply_agent_id").and_then(Value::as_str) {
            if Some(receiver) != caller {
                return Err("a worker can bind only its own reply agent; an owner must configure another receiver".into());
            }
        }
    }
    Ok(())
}

pub(super) async fn dispatch(
    action: &str,
    input: Value,
    ctx: &PrimitiveExecCtx,
    timeout_secs: u32,
) -> Result<PrimitiveToolResult, String> {
    let started = Instant::now();
    let principal = ctx
        .principal
        .as_deref()
        .filter(|x| !x.trim().is_empty())
        .ok_or("work-module runtime principal is required")?;
    let workspace = ctx
        .workspace
        .as_deref()
        .filter(|x| !x.trim().is_empty())
        .ok_or("work-module runtime workspace is required")?;
    let endpoint = ENDPOINT
        .get()
        .ok_or("work-module host bridge is not installed")?;
    verify_reply_receiver(action, &input, ctx.agent_id.as_deref())?;
    let (method, path, mut body) = request(action, input)?;
    if action == "bind_offer" {
        if let Some(task_id) = &ctx.task_id {
            body.as_object_mut()
                .expect("object")
                .insert("source_task_id".into(), Value::String(task_id.clone()));
        }
    }
    let token = TokenLease(bot_token_registry().mint(BotGrant {
        principal: principal.into(),
        workspace: workspace.into(),
        bot_name: format!("work-module-call-{}", uuid::Uuid::new_v4()),
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(u64::from(timeout_secs)))
        .build()
        .map_err(|_| "work-module local client unavailable")?;
    let mut request = client
        .request(
            method.clone(),
            format!("http://{endpoint}/api/magician/v2{path}"),
        )
        .bearer_auth(&token.0);
    if method == reqwest::Method::GET {
        let query: Vec<_> = body
            .as_object()
            .expect("object")
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string()),
                )
            })
            .collect();
        request = request.query(&query);
    } else {
        request = request.json(&body);
    }
    let call = async {
        let mut response = request
            .send()
            .await
            .map_err(|_| "work-module local request failed")?;
        let status_code = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "work-module local response failed")?
        {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err("work-module response exceeds its output limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let response = serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        let result = json!({"status":if (200..300).contains(&status_code) {"ok"} else {"refused"},"status_code":status_code,"response":response});
        Ok(PrimitiveToolResult {
            success: true,
            stdout: result.to_string(),
            parsed_json: Some(result),
            elapsed_ms: started.elapsed().as_millis() as u64,
            ..PrimitiveToolResult::default()
        })
    };
    if let Some(cancellation) = &ctx.cancellation_token {
        tokio::select! { biased; _ = cancellation.cancelled() => Err("work-module call cancelled".into()), result = call => result.map_err(str::to_owned) }
    } else {
        call.await.map_err(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_table_keeps_scope_out_of_caller_input_and_has_no_submit_or_sweep() {
        let (_,path,body) = request("hold",json!({"negotiation_id":"neg_123","principal":"victim","workspace":"other","calendar_event_ref":"event-1","calendar_act_ref":"act_1"})).unwrap();
        assert_eq!(path, "/work/negotiations/neg_123/hold");
        assert!(body.get("principal").is_none() && body.get("workspace").is_none());
        assert!(request("submit", json!({})).is_err());
        assert!(request("sweep", json!({})).is_err());
        assert!(request("hold", json!({"negotiation_id":"../other"})).is_err());
    }
    #[test]
    fn a_worker_cannot_use_reply_binding_to_invoke_another_agent() {
        assert!(verify_reply_receiver(
            "bind_offer",
            &json!({"reply_agent_id":"outward-worker"}),
            Some("outward-worker")
        )
        .is_ok());
        assert!(verify_reply_receiver(
            "bind_offer",
            &json!({"reply_agent_id":"owner-officer"}),
            Some("outward-worker")
        )
        .is_err());
        assert!(verify_reply_receiver(
            "bind_offer",
            &json!({"reply_agent_id":"outward-worker"}),
            None
        )
        .is_err());
        assert!(verify_reply_receiver("bind_offer", &json!({}), Some("outward-worker")).is_ok());
    }
    #[test]
    fn token_cleanup_revokes_only_the_exact_call() {
        let registry = bot_token_registry();
        let one = registry.mint(BotGrant {
            principal: "alice".into(),
            workspace: "work".into(),
            bot_name: "work-module-test-1".into(),
        });
        let two = registry.mint(BotGrant {
            principal: "alice".into(),
            workspace: "work".into(),
            bot_name: "work-module-test-2".into(),
        });
        drop(TokenLease(one.clone()));
        assert!(registry.resolve(&one).is_none());
        assert!(registry.resolve(&two).is_some());
        registry.revoke(&two);
    }
}
