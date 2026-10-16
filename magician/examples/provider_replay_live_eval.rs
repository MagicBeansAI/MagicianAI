//! CLI wrapper for the production-backed cross-provider replay eval.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    magician_surfaces::evals::provider_replay::run_cli().await
}
