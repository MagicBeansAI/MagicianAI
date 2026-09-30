//! Linux iMessage reads cross the private desktop relay; the Mac database stays
//! with its signed owner. The governed provider still owns admission/formatting.
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct HostRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

pub async fn query(sql: &str, timeout_secs: u64) -> Result<HostRows, String> {
    let deadline = timeout_secs.clamp(1, 30);
    let base = crate::magician_v2::media_seam::host_gateway_url_from_env();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(deadline + 5))
        .build()
        .map_err(|_| "Cannot initialize Messages host client")?;
    let mut response = client
        .post(format!(
            "{}/host/imessage/query",
            base.trim_end_matches('/')
        ))
        .json(&json!({"sql": sql, "timeout_secs": deadline}))
        .send()
        .await
        .map_err(|_| {
            "Messages host relay is unavailable; start the desktop managing this container"
        })?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Messages host response failed")?
    {
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err("Messages host result exceeded its limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let error = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("Messages host returned HTTP {status}"));
        return Err(error);
    }
    let result: HostRows =
        serde_json::from_slice(&body).map_err(|_| "Invalid Messages host result")?;
    if result.columns.len() > 128
        || result.rows.len() > 1000
        || result
            .rows
            .iter()
            .any(|row| row.len() != result.columns.len())
    {
        return Err("Invalid Messages host row shape".into());
    }
    Ok(result)
}
