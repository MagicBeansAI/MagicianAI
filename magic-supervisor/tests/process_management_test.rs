//! Process management tests for Magic Supervisor.
//!
//! Tests verify that the supervisor can manage both Magician and Magicutor
//! processes independently with proper lifecycle management.

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use std::time::Duration;

    const MAGICIAN_URL: &str = "http://localhost:3002";
    const MAGICUTOR_URL: &str = "http://localhost:3003";

    /// Helper to check if a service is running
    async fn is_service_running(url: &str) -> bool {
        let client = Client::new();
        client
            .get(format!("{}/health", url))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    #[tokio::test]
    #[ignore] // Run only when supervisor is running
    async fn test_supervisor_starts_managed_services() {
        // Give services time to start after supervisor launch
        tokio::time::sleep(Duration::from_secs(2)).await;

        let magician_running = is_service_running(MAGICIAN_URL).await;
        let magicutor_running = is_service_running(MAGICUTOR_URL).await;

        assert!(magician_running, "Supervisor should have started Magician");
        assert!(
            magicutor_running,
            "Supervisor should have started Magicutor"
        );
    }

    #[tokio::test]
    #[ignore] // Run only when supervisor is running
    async fn test_services_run_independently() {
        // Verify both services respond concurrently
        let (magician_result, magicutor_result) = tokio::join!(
            is_service_running(MAGICIAN_URL),
            is_service_running(MAGICUTOR_URL)
        );

        assert!(
            magician_result,
            "Magician should respond during concurrent access"
        );
        assert!(
            magicutor_result,
            "Magicutor should respond during concurrent access"
        );

        // Verify they can be accessed multiple times
        for _ in 0..3 {
            let magician_health = is_service_running(MAGICIAN_URL).await;
            let magicutor_health = is_service_running(MAGICUTOR_URL).await;

            assert!(
                magician_health && magicutor_health,
                "Services should remain healthy"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    #[ignore] // Run only when supervisor is running
    async fn test_supervisor_health_monitoring() {
        // This test verifies that supervisor is monitoring both services
        // by checking that both services remain accessible over time

        let mut magician_checks = 0;
        let mut magicutor_checks = 0;

        for _ in 0..5 {
            if is_service_running(MAGICIAN_URL).await {
                magician_checks += 1;
            }
            if is_service_running(MAGICUTOR_URL).await {
                magicutor_checks += 1;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Services should be consistently available
        assert!(
            magician_checks >= 4,
            "Magician should be consistently healthy (got {}/5)",
            magician_checks
        );
        assert!(
            magicutor_checks >= 4,
            "Magicutor should be consistently healthy (got {}/5)",
            magicutor_checks
        );
    }

    #[tokio::test]
    #[ignore] // Run only when supervisor is running
    async fn test_process_isolation() {
        // Verify that services are truly independent processes
        // by checking they can handle load independently

        let magician_tasks: Vec<_> = (0..5)
            .map(|_| tokio::spawn(async move { is_service_running(MAGICIAN_URL).await }))
            .collect();

        let magicutor_tasks: Vec<_> = (0..5)
            .map(|_| tokio::spawn(async move { is_service_running(MAGICUTOR_URL).await }))
            .collect();

        // All tasks should complete successfully
        let mut magician_successes = 0;
        for task in magician_tasks {
            if task.await.unwrap_or(false) {
                magician_successes += 1;
            }
        }

        let mut magicutor_successes = 0;
        for task in magicutor_tasks {
            if task.await.unwrap_or(false) {
                magicutor_successes += 1;
            }
        }

        assert!(
            magician_successes >= 4,
            "Magician should handle concurrent requests"
        );
        assert!(
            magicutor_successes >= 4,
            "Magicutor should handle concurrent requests"
        );
    }
}
