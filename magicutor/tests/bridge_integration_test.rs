use actix_web::{web, App, HttpServer};
use futures::{SinkExt, StreamExt};
use magicutor::bridge_protocol::{ExtensionRequest, ExtensionResponse};
use magicutor::{
    server::{bridge::send_over_bridge, configure_routes},
    MagicutorConfig,
};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct StopOnDrop(actix_web::dev::ServerHandle);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        drop(self.0.stop(false));
    }
}

// End-to-end bridge test with a mocked extension over WebSocket.
#[actix_rt::test]
async fn bridge_roundtrip_request_response() {
    // Keep the bridge's accepted-browser store out of the real data root.
    std::env::set_var(
        "MAGICUTOR_BROWSER_STORE_PATH",
        std::env::temp_dir()
            .join(format!("magicutor-it-{}", std::process::id()))
            .join("bridge_browsers.json"),
    );
    let mut config = MagicutorConfig::default();
    config.bridge.enabled = true;
    config.bridge.auth_token = String::new();
    let config_for_srv = config.clone();

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = HttpServer::new(move || {
        let config = config_for_srv.clone();
        App::new()
            .app_data(web::Data::new(config.clone()))
            .configure(configure_routes)
    })
    .workers(1)
    .disable_signals()
    .listen(listener)
    .unwrap()
    .run();
    let stop = StopOnDrop(server.handle());
    actix_rt::spawn(server);

    // Connect mock extension to bridge.
    let ws_url = format!("ws://{address}/bridge/native");
    let (mut ws, _resp) =
        tokio::time::timeout(std::time::Duration::from_secs(5), connect_async(ws_url))
            .await
            .expect("WebSocket handshake deadline")
            .expect("ws connect");

    // Spawn handler that reads request and responds
    actix_rt::spawn(async move {
        while let Some(Ok(frame)) = ws.next().await {
            if let Message::Text(text) = frame {
                let env: serde_json::Value = serde_json::from_str(&text).unwrap();
                if env["type"] == "request" {
                    let req_id = env["data"]["requestId"].as_str().unwrap().to_string();
                    let resp = ExtensionResponse::success(req_id, json!({"ok": true}));
                    let payload = serde_json::to_string(&serde_json::json!({
                        "type": "response",
                        "data": resp
                    }))
                    .unwrap();
                    let _ = ws.send(Message::Text(payload)).await;
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
    stop.0.stop(false).await;
}
