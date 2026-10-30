//! Time the prompt-path LanceDB hybrid search against a memory index copy.
//!
//! Runs the same search a chat lookup runs (vector and full-text legs, fused,
//! with the chat prompt predicate), after embedding each query once, and
//! reports per-query latency. With `--optimize` it then compacts the copy the
//! way `make memory-index-optimize` would and measures again.
//!
//! Point it at a **copy** of `<scope>/memory` (`cp -cR` clones on APFS):
//! `--optimize` rewrites the table. The embedding endpoint comes from the
//! same environment as the server (load the runtime `.env` first).
//!
//! ```text
//! cargo run -p magician-vector-index --example lance_hybrid_search_bench -- \
//!     --memory-root <copy>/memory --agent personal-assistant --rounds 5 --optimize
//! ```

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use magician_vector_index::memory_index::{
    benchmark_embed_prompt_query, benchmark_lancedb_hybrid_search, benchmark_lancedb_table_stats,
    benchmark_optimize_lancedb, MemoryIndexManifest,
};

const QUERIES: [&str; 5] = [
    "what do you remember about how I like my meetings scheduled",
    "my travel preferences",
    "who is Asha and what does she prefer",
    "the invoice for the design work",
    "what did we plan for the quarterly offsite",
];

#[tokio::main]
async fn main() -> Result<()> {
    let mut memory_root = None::<PathBuf>;
    let mut agent = "personal-assistant".to_string();
    let mut rounds = 5usize;
    let mut optimize = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--memory-root" => memory_root = args.next().map(PathBuf::from),
            "--agent" => agent = args.next().context("--agent needs a value")?,
            "--rounds" => rounds = args.next().context("--rounds needs a value")?.parse()?,
            "--optimize" => optimize = true,
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let memory_root = memory_root.context("--memory-root is required")?;
    // The server sets its embedding endpoint from magician-config.yaml
    // (`runtime.ollama.embedding_*`) at startup; mirror the live values so the
    // query embedding matches the index's embedding contract.
    magician_vector_index::ollama_keep_alive::set_default_ollama_embedding_policy(
        Some("http://127.0.0.1:11435".to_string()),
        Some("-1".to_string()),
        Some(1),
        Some(1),
        Some(5_000),
        Some(180_000),
        Some("hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K".to_string()),
        Some(8_192),
        Some(512),
        Some(10),
        Some(2_560),
    );
    let index_dir = memory_root.join("index").join("lancedb");
    let manifest: MemoryIndexManifest = serde_json::from_slice(
        &std::fs::read(memory_root.join("index").join("manifest.json"))
            .context("reading index manifest")?,
    )
    .context("parsing index manifest")?;

    let mut embedded = Vec::new();
    for query in QUERIES {
        let started = Instant::now();
        let (expanded, embedding) = benchmark_embed_prompt_query(&manifest, query).await?;
        println!(
            "embed {:>7.1} ms  {query}",
            started.elapsed().as_secs_f64() * 1_000.0
        );
        embedded.push((expanded, embedding));
    }

    measure("as is", &index_dir, &manifest, &embedded, &agent, rounds).await?;
    if optimize {
        let started = Instant::now();
        benchmark_optimize_lancedb(&index_dir).await?;
        println!("optimize {:.1} s", started.elapsed().as_secs_f64());
        measure(
            "compacted",
            &index_dir,
            &manifest,
            &embedded,
            &agent,
            rounds,
        )
        .await?;
    }
    Ok(())
}

async fn measure(
    label: &str,
    index_dir: &std::path::Path,
    manifest: &MemoryIndexManifest,
    embedded: &[(String, Vec<f32>)],
    agent: &str,
    rounds: usize,
) -> Result<()> {
    if let Some((fragments, rows)) = benchmark_lancedb_table_stats(index_dir).await? {
        println!("== {label}: {fragments} active fragments, {rows} rows");
    }
    let mut samples = Vec::new();
    for round in 0..rounds {
        for (expanded, embedding) in embedded {
            let started = Instant::now();
            let hits =
                benchmark_lancedb_hybrid_search(index_dir, manifest, expanded, embedding, agent)
                    .await?;
            let ms = started.elapsed().as_secs_f64() * 1_000.0;
            // The first round opens tables cold; report it apart.
            if round > 0 {
                samples.push(ms);
            } else {
                println!("   cold {ms:>8.1} ms  hits={hits}");
            }
        }
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    if !samples.is_empty() {
        let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
        println!(
            "   warm n={} p50 {:.1} ms  p90 {:.1} ms  max {:.1} ms",
            samples.len(),
            at(0.5),
            at(0.9),
            at(1.0)
        );
    }
    Ok(())
}
