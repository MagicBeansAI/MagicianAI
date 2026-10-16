//! Restated rather than imported from the magician-chunking crate: the batch
//! consolidator (lib) persists this bounded plan before the first write, so
//! the durable checkpoint boundary cannot depend on the satellite crate.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;

/// Cap on episodes per logical archive root.
pub const MAX_ARCHIVE_GROUP_EPISODES: usize = 6;

/// Return the adapter's exact deterministic archive-group membership without
/// rendering model prompts. The batch consolidator persists this bounded plan
/// before the first write, then commits one complete logical root at a time.
pub fn archive_checkpoint_groups(episodes: &[V3EpisodeRecord]) -> Vec<Vec<String>> {
    let mut ordered = episodes.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| episode_order(left, right));
    let mut grouped = BTreeMap::<String, Vec<&V3EpisodeRecord>>::new();
    for episode in ordered {
        grouped
            .entry(archive_group_key(episode))
            .or_default()
            .push(episode);
    }
    let mut groups = grouped.into_iter().collect::<Vec<_>>();
    groups.sort_by(|(left_key, left), (right_key, right)| {
        left.first()
            .and_then(|episode| parse_timestamp_value(&episode.completed_at))
            .cmp(
                &right
                    .first()
                    .and_then(|episode| parse_timestamp_value(&episode.completed_at)),
            )
            .then_with(|| left_key.cmp(right_key))
    });
    groups
        .into_iter()
        .flat_map(|(_, group)| {
            group
                .chunks(MAX_ARCHIVE_GROUP_EPISODES)
                .map(|segment| {
                    segment
                        .iter()
                        .map(|episode| episode.episode_id.clone())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn archive_group_key(episode: &V3EpisodeRecord) -> String {
    let workflow = episode
        .task_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| (!episode.goal_key.trim().is_empty()).then_some(episode.goal_key.as_str()))
        .unwrap_or(episode.consolidation_key.as_str());
    let session = episode
        .ui_thread_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            episode
                .root_execution_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| episode.completed_at.split('T').next().unwrap_or("unknown"));
    format!(
        "workflow:{}|session:{}",
        normalize_key(workflow),
        normalize_key(session)
    )
}

fn episode_order(left: &V3EpisodeRecord, right: &V3EpisodeRecord) -> std::cmp::Ordering {
    episode_completed_time(left)
        .cmp(&episode_completed_time(right))
        .then_with(|| left.trigger_seq.cmp(&right.trigger_seq))
        .then_with(|| left.episode_id.cmp(&right.episode_id))
}

fn episode_completed_time(episode: &V3EpisodeRecord) -> Option<DateTime<Utc>> {
    parse_timestamp_value(&episode.completed_at)
}

fn parse_timestamp_value(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn normalize_key(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}
