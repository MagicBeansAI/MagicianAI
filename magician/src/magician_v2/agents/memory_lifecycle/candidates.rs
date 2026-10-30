//! Which existing records the reviewer weighs an incoming one against.
//!
//! The reviewer can only call a record a duplicate of one it is shown, and
//! the decision engine answers the shown records one at a time, so the list
//! must be short and right. Records are ranked by meaning (cosine over local
//! embeddings, cached per wording so a record is embedded once per process),
//! then by shared words; the conflict being resolved always comes first.
//! When the embedding model is unavailable the ranking is by shared words
//! alone, as before. Ranking only chooses what the reviewer reads: the
//! verdict stays the reviewer's.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use super::{text, Conflict, Source};

pub type EmbedFuture = Pin<Box<dyn Future<Output = anyhow::Result<Vec<Vec<f32>>>> + Send>>;
pub type Embedder = Arc<dyn Fn(Vec<String>) -> EmbedFuture + Send + Sync>;

/// Embeddings kept per process (by wording digest). Past this the cache is
/// cleared and refilled: a few thousand short records, never unbounded.
const CACHE_ENTRIES: usize = 8_192;
const EMBED_BATCH: usize = 64;

/// The local embedding model, on the background lane.
pub fn local_embedder() -> Embedder {
    Arc::new(|texts| {
        Box::pin(crate::magician_v2::agents::user_memory_bounds::dedupe::local_embed(texts))
    })
}

fn cache() -> &'static Mutex<HashMap<String, Arc<Vec<f32>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<f32>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One ranking embeds at a time. After a restart the cache is empty and the
/// first ranking embeds every record (minutes on the local model); the sweep
/// and the resurfacing worker used to do that same work side by side. Now the
/// second waits and finds the first one's vectors cached.
fn embed_turn() -> &'static tokio::sync::Mutex<()> {
    static TURN: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    TURN.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Vectors for `texts`, embedding only those not cached. Each batch is cached
/// as soon as it is embedded, so a pass that fails or is dropped part way
/// keeps what it already paid for.
async fn embed_cached(embed: &Embedder, texts: &[String]) -> anyhow::Result<Vec<Arc<Vec<f32>>>> {
    let keys: Vec<String> = texts
        .iter()
        .map(|text| super::digest(&serde_json::json!(text)))
        .collect();
    let _turn = embed_turn().lock().await;
    // Vectors this ranking uses, held here so a cache refill cannot evict them.
    let mut have: HashMap<String, Arc<Vec<f32>>> = {
        let cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        keys.iter()
            .filter_map(|key| cache.get(key).map(|vector| (key.clone(), vector.clone())))
            .collect()
    };
    let mut queued = std::collections::HashSet::new();
    let missing: Vec<usize> = (0..texts.len())
        .filter(|&i| !have.contains_key(&keys[i]) && queued.insert(&keys[i]))
        .collect();
    for batch in missing.chunks(EMBED_BATCH) {
        let vectors = embed(batch.iter().map(|&i| texts[i].clone()).collect()).await?;
        anyhow::ensure!(
            vectors.len() == batch.len(),
            "the embedding model returned {} vectors for {} texts",
            vectors.len(),
            batch.len()
        );
        let fresh: Vec<(String, Arc<Vec<f32>>)> = batch
            .iter()
            .zip(vectors)
            .map(|(&i, vector)| (keys[i].clone(), Arc::new(vector)))
            .collect();
        {
            let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() + fresh.len() > CACHE_ENTRIES {
                cache.clear();
            }
            cache.extend(fresh.iter().cloned());
        }
        have.extend(fresh);
    }
    keys.iter()
        .map(|key| {
            have.get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("embedding missing after ranking"))
        })
        .collect()
}

/// The `max` records of `offered` the reviewer should see for `incoming`.
pub async fn choose(
    incoming: &Source,
    mut offered: Vec<Source>,
    resolving: Option<&Conflict>,
    max: usize,
    embed: Option<&Embedder>,
) -> Vec<Source> {
    let words: BTreeSet<String> = text(&incoming.item)
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let overlap = |source: &Source| {
        text(&source.item)
            .split_whitespace()
            .filter(|word| words.contains(&word.to_lowercase()))
            .count()
    };
    let in_conflict = |source: &Source| {
        resolving.is_some_and(|c| source.id == c.incoming.id || source.id == c.existing.id)
    };
    let similarity: Option<Vec<f32>> = match embed {
        Some(embed) => {
            let mut texts = vec![embed_text(&incoming.item)];
            texts.extend(offered.iter().map(|source| embed_text(&source.item)));
            match embed_cached(embed, &texts).await {
                Ok(vectors) => Some(
                    vectors[1..]
                        .iter()
                        .map(|vector| cosine(&vectors[0], vector))
                        .collect(),
                ),
                Err(error) => {
                    tracing::debug!(
                        %error,
                        "memory review: embeddings unavailable; ranking by shared words"
                    );
                    None
                },
            }
        },
        None => None,
    };
    let mut ranked: Vec<(bool, f32, usize, Source)> = offered
        .drain(..)
        .enumerate()
        .map(|(at, source)| {
            let score = similarity.as_ref().map_or(0.0, |s| s[at]);
            (in_conflict(&source), score, overlap(&source), source)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)).then(b.2.cmp(&a.2)));
    ranked.truncate(max);
    ranked.into_iter().map(|(_, _, _, source)| source).collect()
}

fn embed_text(item: &serde_json::Value) -> String {
    crate::magician_v2::agents::user_memory_bounds::dedupe::embed_text(item)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    crate::magician_v2::agents::user_memory_bounds::dedupe::cosine(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(id: &str, key: &str, value: &str) -> Source {
        serde_json::from_value(json!({
            "id": id, "pointer": format!("/preferences/{id}"), "revision": id,
            "item": {"key": key, "value": value}
        }))
        .unwrap()
    }

    /// Texts about drafts point one way, the rest another.
    fn stub() -> Embedder {
        Arc::new(|texts: Vec<String>| {
            Box::pin(async move {
                Ok(texts
                    .iter()
                    .map(|t| {
                        if t.contains("draft") {
                            vec![1.0f32, 0.0]
                        } else {
                            vec![0.0f32, 1.0]
                        }
                    })
                    .collect())
            }) as EmbedFuture
        })
    }

    #[tokio::test]
    async fn the_same_meaning_beats_shared_words_and_the_list_is_short() {
        let incoming = source(
            "in",
            "draft_only",
            "Return only the draft text for writing requests",
        );
        let offered = vec![
            source(
                "a",
                "writing_requests_log",
                "Return the text for writing requests only on weekdays",
            ),
            source("b", "output_format", "draft text alone, no labels"),
            source("c", "tone", "Warm"),
        ];
        let chosen = choose(&incoming, offered.clone(), None, 2, Some(&stub())).await;
        assert_eq!(chosen.len(), 2);
        assert_eq!(chosen[0].id, "b", "the paraphrase comes first");
        // Without embeddings the old word-overlap order holds.
        let chosen = choose(&incoming, offered, None, 1, None).await;
        assert_eq!(chosen[0].id, "a");
    }

    /// An embedder that counts the texts it is asked for (each call yields so
    /// concurrent rankings interleave) and fails from call `fail_from` on.
    fn counting(count: Arc<std::sync::atomic::AtomicUsize>, fail_from: usize) -> Embedder {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Arc::new(move |texts: Vec<String>| {
            let count = count.clone();
            let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                anyhow::ensure!(call < fail_from, "offline");
                count.fetch_add(texts.len(), std::sync::atomic::Ordering::SeqCst);
                Ok(texts.iter().map(|_| vec![1.0f32, 0.0]).collect())
            }) as EmbedFuture
        })
    }

    /// `n` records, more than one embedding batch, worded uniquely per `tag`
    /// so no other test's cache entries count.
    fn many(tag: &str, n: usize) -> (Source, Vec<Source>) {
        let incoming = source(&format!("{tag}-in"), "k", &format!("{tag} incoming claim"));
        let offered = (0..n)
            .map(|i| {
                source(
                    &format!("{tag}-{i}"),
                    "k",
                    &format!("{tag} claim number {i}"),
                )
            })
            .collect();
        (incoming, offered)
    }

    #[tokio::test]
    async fn concurrent_rankings_embed_each_record_once() {
        let (incoming, offered) = many("warmup-concurrent", 70);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let embed = counting(count.clone(), usize::MAX);
        let (a, b) = tokio::join!(
            choose(&incoming, offered.clone(), None, 3, Some(&embed)),
            choose(&incoming, offered.clone(), None, 3, Some(&embed)),
        );
        assert_eq!((a.len(), b.len()), (3, 3));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 71);
    }

    #[tokio::test]
    async fn a_ranking_that_fails_part_way_keeps_the_batches_it_embedded() {
        let (incoming, offered) = many("warmup-partial", 70);
        let first = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // The first batch (64 texts) succeeds, the second fails: the ranking
        // falls back to shared words, but the 64 vectors stay cached.
        choose(
            &incoming,
            offered.clone(),
            None,
            3,
            Some(&counting(first.clone(), 1)),
        )
        .await;
        assert_eq!(first.load(std::sync::atomic::Ordering::SeqCst), EMBED_BATCH);
        let second = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        choose(
            &incoming,
            offered,
            None,
            3,
            Some(&counting(second.clone(), usize::MAX)),
        )
        .await;
        assert_eq!(
            second.load(std::sync::atomic::Ordering::SeqCst),
            71 - EMBED_BATCH
        );
    }

    #[tokio::test]
    async fn a_down_embedding_model_falls_back_to_shared_words() {
        let down: Embedder =
            Arc::new(|_| Box::pin(async { anyhow::bail!("offline") }) as EmbedFuture);
        let incoming = source("in2", "k", "metric units everywhere please");
        let offered = vec![
            source("x", "a", "tone warm"),
            source("y", "b", "metric units"),
        ];
        let chosen = choose(&incoming, offered, None, 1, Some(&down)).await;
        assert_eq!(chosen[0].id, "y");
    }
}
