use actix_test::start;
use actix_web::{web, App};
use actix_web_actors::ws::Frame;
use awc::{ws::Message as AwcMessage, Client};
use futures::{SinkExt, StreamExt};
use magicutor::bridge_protocol::{ExtensionRequest, ExtensionResponse};
use magicutor::{
    server::{bridge::send_over_bridge, configure_routes},
    MagicutorConfig,
};
use serde_json::json;

// End-to-end bridge test with a mocked extension over WebSocket.
#[actix_rt::test]
async fn bridge_roundtrip_request_response() {
    let mut config = MagicutorConfig::default();
    config.bridge.enabled = true;
    config.bridge.auth_token = String::new();
    let config_for_srv = config.clone();

    let srv = start(move || {
        let config = config_for_srv.clone();
        App::new()
            .app_data(web::Data::new(config.clone()))
            .configure(configure_routes)
    });

    // Connect mock extension to bridge.
    let client = Client::new();
    let ws_url = srv.url("/bridge/native");
    let (_resp, mut ws) = client.ws(ws_url).connect().await.expect("ws connect");

    // Spawn handler that reads request and responds
    actix_rt::spawn(async move {
        while let Some(Ok(frame)) = ws.next().await {
            if let Frame::Text(text) = frame {
                let txt = String::from_utf8_lossy(&text);
                let env: serde_json::Value = serde_json::from_str(&txt).unwrap();
                if env["type"] == "request" {
                    let req_id = env["data"]["requestId"].as_str().unwrap().to_string();
                    let resp = ExtensionResponse::success(req_id, json!({"ok": true}));
                    let payload = serde_json::to_string(&serde_json::json!({
                        "type": "response",
                        "data": resp
                    }))
                    .unwrap();
                    let _ = ws.send(AwcMessage::Text(payload.into())).await;
                    break;
                }
            }
        }
    });

    let req = ExtensionRequest {
        request_id: "req-1".to_string(),
        action: "ping".to_string(),
        params: json!({}),
    };

    // The BridgeSession::started() callback registers the sender in the global HUB
    // via an async spawn — there is a brief window where the WS is connected but the
    // HUB sender is not yet set.  Retry until the bridge is ready (up to 2 s).
    let resp = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match send_over_bridge(req.clone()).await {
                Ok(r) => break r,
                Err(_) if std::time::Instant::now() < deadline => {
                    actix_rt::time::sleep(std::time::Duration::from_millis(25)).await;
                },
                Err(e) => panic!("bridge response: {e}"),
            }
        }
    };
    assert!(resp.success);
    assert_eq!(resp.result.unwrap_or_default()["ok"], json!(true));
}
