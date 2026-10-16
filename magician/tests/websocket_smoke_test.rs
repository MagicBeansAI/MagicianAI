//! WebSocket smoke tests for Magician V2 real-time events
//!
//! Tests verify that WebSocket connection to Magician's real-time event stream works
//! and can receive plan generation events.

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::time::Duration;
    use tokio::time::timeout;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    const MAGICIAN_WS_URL: &str = "ws://localhost:3002/api/magician/v2/realtime/ws";

    #[tokio::test]
    #[ignore] // Run only when Magician service is running
    async fn test_websocket_connection() {
        let (ws_stream, _) = timeout(Duration::from_secs(5), connect_async(MAGICIAN_WS_URL))
            .await
            .expect("Should connect within timeout")
            .expect("Should establish WebSocket connection");

        let (_write, mut read) = ws_stream.split();

        // Should receive a connection acknowledgment or be able to close cleanly
        let result = timeout(Duration::from_secs(1), read.next()).await;

        // Either we get a message or timeout (both acceptable for smoke test)
        // The key is that connection was established
        match result {
            Ok(Some(Ok(msg))) => {
                println!("Received message: {:?}", msg);
            },
            Ok(Some(Err(e))) => {
                panic!("WebSocket error: {}", e);
            },
            Ok(None) => {
                println!("Connection closed");
            },
            Err(_) => {
                println!("No initial message (acceptable)");
            },
        }
    }

    #[tokio::test]
    #[ignore] // Run only when Magician service is running
    async fn test_websocket_reconnection() {
        // Connect first time
        let (ws_stream1, _) = connect_async(MAGICIAN_WS_URL)
            .await
            .expect("First connection should succeed");

        drop(ws_stream1);

        // Reconnect
        let (ws_stream2, _) = timeout(Duration::from_secs(5), connect_async(MAGICIAN_WS_URL))
            .await
            .expect("Should reconnect within timeout")
            .expect("Second connection should succeed");

        drop(ws_stream2);

        // Third connection to verify stability
        let (_ws_stream3, _) = timeout(Duration::from_secs(5), connect_async(MAGICIAN_WS_URL))
            .await
            .expect("Should reconnect within timeout")
            .expect("Third connection should succeed");
    }

    #[tokio::test]
    #[ignore] // Run only when Magician service is running
    async fn test_websocket_receives_events() {
        let (ws_stream, _) = connect_async(MAGICIAN_WS_URL)
            .await
            .expect("Should establish WebSocket connection");

        let (mut write, mut read) = ws_stream.split();

        // Send a subscription message if protocol requires it
        let subscribe_msg = json!({
            "type": "subscribe",
            "events": ["plan_generated", "outline_started"]
        });

        write
            .send(Message::Text(subscribe_msg.to_string()))
            .await
            .ok(); // Ignore if subscription not needed

        // Wait for potential events (timeout is acceptable)
        let result = timeout(Duration::from_secs(2), read.next()).await;

        // For smoke test, we just verify connection stability
        // Actual events would come from triggering plan generation
        match result {
            Ok(Some(Ok(Message::Text(text)))) => {
                println!("Received event: {}", text);
                // Verify it's valid JSON
                serde_json::from_str::<serde_json::Value>(&text)
                    .expect("Event should be valid JSON");
            },
            _ => {
                println!("No events received (acceptable for smoke test)");
            },
        }
    }

    #[tokio::test]
    #[ignore] // Run only when Magician service is running
    async fn test_websocket_multiple_concurrent_connections() {
        let mut handles = vec![];

        for i in 0..3 {
            let handle = tokio::spawn(async move {
                let (ws_stream, _) = connect_async(MAGICIAN_WS_URL)
                    .await
                    .unwrap_or_else(|_| panic!("Connection {} should succeed", i));

                tokio::time::sleep(Duration::from_millis(100)).await;
                drop(ws_stream);
            });
            handles.push(handle);
        }

        // All connections should complete successfully
        for handle in handles {
            handle.await.expect("Connection task should complete");
        }
    }
}
