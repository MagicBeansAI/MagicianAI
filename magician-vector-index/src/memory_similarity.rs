//! Whether two stored memory claims are the same claim worded twice.
//!
//! Used where a near-duplicate must not become a sibling record: the
//! write-time ingress folds one into the other through the lifecycle, a
//! maintenance pass folds copies already stored, and the prompt collapses any
//! that remain. The rule is deliberately strict — anything that might be a
//! refinement or a contradiction is left to semantic review:
//!
//! - the claims' content words (lowercase, stopwords dropped, plural folded)
//!   overlap with a Jaccard of at least [`NEAR_DUPLICATE_TEXT_JACCARD`]
//!   (stricter, [`NEAR_DUPLICATE_TEXT_ONLY_JACCARD`], when either has no key);
//! - the words they do not share include no negation or qualifier (`not`,
//!   `never`, `except`, `unless`, `only`, …) and no number;
//! - their keys, when both have one, share at least
//!   [`NEAR_DUPLICATE_KEY_JACCARD`] of their normalised words (case,
//!   separators, plural and generic words like `preference` folded).

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Content-word Jaccard two keyed claims need to be one claim.
pub const NEAR_DUPLICATE_TEXT_JACCARD: f64 = 0.8;
/// Content-word Jaccard two claims need when either has no key.
pub const NEAR_DUPLICATE_TEXT_ONLY_JACCARD: f64 = 0.9;
/// Normalised key-word Jaccard two keyed claims need.
pub const NEAR_DUPLICATE_KEY_JACCARD: f64 = 0.4;
/// Fewest content words a claim needs before it is compared at all: short
/// claims differ by a word that matters.
const MIN_CONTENT_WORDS: usize = 4;

const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "of",
    "to",
    "for",
    "in",
    "on",
    "with",
    "by",
    "at",
    "from",
    "as",
    "is",
    "are",
    "be",
    "been",
    "it",
    "its",
    "this",
    "that",
    "these",
    "those",
    "their",
    "they",
    "them",
    "user",
    "users",
    "owner",
    "prefers",
    "prefer",
    "preferred",
    "rather",
    "than",
    "over",
    "should",
    "would",
    "will",
    "can",
    "also",
];

/// Words whose presence on one side only can change what a claim means.
const QUALIFIERS: &[&str] = &[
    "not", "no", "never", "nor", "none", "nothing", "dont", "doesnt", "isnt", "wont", "cant",
    "avoid", "without", "except", "unless", "but", "only", "instead", "stop", "longer", "neither",
    "against", "disallow", "forbid",
];

/// Key words too generic to tell two keys apart.
const GENERIC_KEY_WORDS: &[&str] = &[
    "preference",
    "pref",
    "setting",
    "note",
    "item",
    "entry",
    "memory",
    "fact",
    "info",
];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|word| word.replace('\'', "").to_lowercase())
        .filter(|word| !word.is_empty())
        .map(|word| fold_plural(&word))
}

fn fold_plural(word: &str) -> String {
    if word.chars().count() > 3
        && word.ends_with('s')
        && !word.ends_with("ss")
        && !word.ends_with("us")
        && !word.ends_with("is")
    {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// The content words of a claim: lowercase, stopwords dropped, plurals folded.
pub fn content_words(text: &str) -> BTreeSet<String> {
    words(text)
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect()
}

/// The words of a key, normalised: case and separators (`_`, `-`, `.`,
/// spaces) folded, plurals folded, generic words dropped.
pub fn normalized_key_words(key: &str) -> BTreeSet<String> {
    words(key)
        .filter(|word| !GENERIC_KEY_WORDS.contains(&word.as_str()))
        .collect()
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        return 0.0;
    }
    left.intersection(right).count() as f64 / union as f64
}

/// Whether two claims (each a key, when it has one, and its text) are the
/// same claim worded twice. See the module documentation for the rule.
pub fn near_duplicate_claims(
    left_key: Option<&str>,
    left_text: &str,
    right_key: Option<&str>,
    right_text: &str,
) -> bool {
    PreparedMemoryClaim::new(left_key, left_text)
        .matches(&PreparedMemoryClaim::new(right_key, right_text))
}

/// Normalized once per claim, rather than again for every comparison.
#[derive(Debug, Clone)]
pub struct PreparedMemoryClaim {
    words: BTreeSet<String>,
    key_words: Option<BTreeSet<String>>,
    protected_words: BTreeSet<String>,
}

impl PreparedMemoryClaim {
    pub fn new(key: Option<&str>, text: &str) -> Self {
        let words = content_words(text);
        let protected_words = words
            .iter()
            .filter(|word| {
                QUALIFIERS.contains(&word.as_str()) || word.chars().any(|c| c.is_ascii_digit())
            })
            .cloned()
            .collect();
        Self {
            words,
            key_words: key
                .map(normalized_key_words)
                .filter(|words| !words.is_empty()),
            protected_words,
        }
    }

    fn matches(&self, other: &Self) -> bool {
        if self.words.len() < MIN_CONTENT_WORDS
            || other.words.len() < MIN_CONTENT_WORDS
            || self.protected_words != other.protected_words
        {
            return false;
        }
        match self.key_words.as_ref().zip(other.key_words.as_ref()) {
            Some((left, right)) => {
                jaccard(left, right) >= NEAR_DUPLICATE_KEY_JACCARD
                    && jaccard(&self.words, &other.words) >= NEAR_DUPLICATE_TEXT_JACCARD
            },
            None => jaccard(&self.words, &other.words) >= NEAR_DUPLICATE_TEXT_ONLY_JACCARD,
        }
    }
}

/// Per-render lookup of retained claims in one semantic tier. Positions are
/// caller-owned; the earliest matching position preserves first-match order.
#[derive(Debug, Default)]
pub struct MemoryClaimSimilarityIndex {
    claims: BTreeMap<usize, PreparedMemoryClaim>,
    postings: HashMap<String, BTreeSet<usize>>,
}

impl MemoryClaimSimilarityIndex {
    fn possible_matches(&self, claim: &PreparedMemoryClaim) -> Vec<usize> {
        if claim.words.len() < MIN_CONTENT_WORDS {
            return Vec::new();
        }
        let mut overlaps = HashMap::<usize, usize>::new();
        for word in &claim.words {
            if let Some(positions) = self.postings.get(word) {
                for position in positions {
                    *overlaps.entry(*position).or_default() += 1;
                }
            }
        }
        overlaps
            .into_iter()
            .filter_map(|(position, overlap)| {
                let other = self.claims.get(&position)?;
                let union = claim.words.len() + other.words.len() - overlap;
                // Every matching pair needs at least the weaker text threshold.
                // This prefilter cannot add a match or exclude a valid one.
                (other.words.len() >= MIN_CONTENT_WORDS
                    && other.protected_words == claim.protected_words
                    && overlap as f64 / union as f64 >= NEAR_DUPLICATE_TEXT_JACCARD)
                    .then_some(position)
            })
            .collect()
    }

    pub fn first_match(&self, claim: &PreparedMemoryClaim) -> Option<usize> {
        self.possible_matches(claim)
            .into_iter()
            .filter(|position| {
                self.claims
                    .get(position)
                    .is_some_and(|other| other.matches(claim))
            })
            .min()
    }

    /// A stronger representative can replace an earlier one. Refresh its
    /// postings so subsequent claims compare against the current winner.
    pub fn insert(&mut self, position: usize, claim: PreparedMemoryClaim) {
        if let Some(previous) = self.claims.remove(&position) {
            for word in previous.words {
                if let Some(positions) = self.postings.get_mut(&word) {
                    positions.remove(&position);
                    if positions.is_empty() {
                        self.postings.remove(&word);
                    }
                }
            }
        }
        if claim.words.len() < MIN_CONTENT_WORDS {
            return;
        }
        for word in &claim.words {
            self.postings
                .entry(word.clone())
                .or_default()
                .insert(position);
        }
        self.claims.insert(position, claim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "For audio-editing requests, the user prefers a finished, usable artifact \
                        over advice or a substitute, with requested export formats, technical \
                        validation, and concise edit notes.";

    #[test]
    fn key_variants_of_one_claim_are_near_duplicates() {
        let reworded = "For audio-editing requests, the user prefers finished, usable artifacts \
                        rather than advice or a substitute, with requested export formats, \
                        technical validation, and concise edit notes.";
        for (left, right) in [
            (
                "audio_editing_deliverable_preference",
                "audio_editing_deliverable_preferences",
            ),
            (
                "audio_editing_deliverable_preference",
                "Audio-Editing Deliverables",
            ),
            (
                "audio_editing_artifact_preference",
                "audio_editing_deliverables",
            ),
        ] {
            assert!(
                near_duplicate_claims(Some(left), BASE, Some(right), reworded),
                "{left} / {right}"
            );
        }
        assert_eq!(
            normalized_key_words("audio_editing_deliverable_preferences"),
            normalized_key_words("Audio-Editing Deliverable")
        );
    }

    #[test]
    fn distinct_or_qualified_claims_stay_separate() {
        let key = Some("audio_editing_deliverable_preference");
        for other in [
            "For audio-editing requests, the user does not want a finished artifact, with \
             requested export formats, technical validation, and concise edit notes.",
            "For audio-editing requests, the user prefers a finished, usable artifact over \
             advice, with 2 export formats, technical validation, and concise edit notes.",
            "For audio-editing requests, the user prefers a finished, usable artifact over \
             advice or a substitute, except for podcasts, with export formats and edit notes.",
            "Prefers concise markdown summaries with headings for every weekly report.",
        ] {
            assert!(!near_duplicate_claims(key, BASE, key, other), "{other}");
        }
        assert!(!near_duplicate_claims(
            Some("audio_editing_deliverable_preference"),
            BASE,
            Some("weekly_report_format"),
            BASE
        ));
        assert!(!near_duplicate_claims(None, "likes tea", None, "likes tea"));
    }

    // Independent reference for the pre-index rules, including missing/generic
    // keys, negation, numbers and replacement of a retained representative.
    fn original_match(left: (Option<&str>, &str), right: (Option<&str>, &str)) -> bool {
        let a = content_words(left.1);
        let b = content_words(right.1);
        if a.len() < MIN_CONTENT_WORDS || b.len() < MIN_CONTENT_WORDS {
            return false;
        }
        if a.symmetric_difference(&b).any(|word| {
            QUALIFIERS.contains(&word.as_str()) || word.chars().any(|c| c.is_ascii_digit())
        }) {
            return false;
        }
        match left
            .0
            .map(normalized_key_words)
            .filter(|k| !k.is_empty())
            .zip(right.0.map(normalized_key_words).filter(|k| !k.is_empty()))
        {
            Some((a_key, b_key)) => {
                jaccard(&a_key, &b_key) >= NEAR_DUPLICATE_KEY_JACCARD
                    && jaccard(&a, &b) >= NEAR_DUPLICATE_TEXT_JACCARD
            },
            None => jaccard(&a, &b) >= NEAR_DUPLICATE_TEXT_ONLY_JACCARD,
        }
    }

    #[test]
    fn indexed_matching_preserves_original_first_match_and_replacements() {
        let texts = [
            BASE,
            "For audio-editing requests, prefer finished usable artifacts over advice or a substitute, with requested export formats, technical validation and concise edit notes.",
            "For audio-editing requests, do not prefer finished usable artifacts over advice or a substitute, with requested export formats, technical validation and concise edit notes.",
            "For audio-editing requests, prefer 2 finished usable artifacts over advice or a substitute, with requested export formats, technical validation and concise edit notes.",
            "For audio-editing requests, prefer 3 finished usable artifacts over advice or a substitute, with requested export formats, technical validation and concise edit notes.",
            "Weekly reports use concise markdown summaries and readable headings.",
            "Weekly report uses concise markdown summary and readable heading.",
            "Weekly reports use concise markdown summaries and readable headings only.",
            "Résumé notes preserve café names and Unicode punctuation.",
            "likes tea",
        ];
        let keys = [
            None,
            Some("preference"),
            Some("audio_editing_deliverables"),
            Some("weekly_report_format"),
        ];
        let cases: Vec<_> = keys
            .into_iter()
            .flat_map(|key| texts.iter().map(move |text| (key, *text)))
            .collect();
        for left in &cases {
            for right in &cases {
                assert_eq!(
                    near_duplicate_claims(left.0, left.1, right.0, right.1),
                    original_match(*left, *right)
                );
            }
        }
        let mut index = MemoryClaimSimilarityIndex::default();
        let mut retained = Vec::new();
        let mut seed = 42u32;
        for turn in 0..400 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let claim = cases[seed as usize % cases.len()];
            let prepared = PreparedMemoryClaim::new(claim.0, claim.1);
            let expected = retained
                .iter()
                .position(|other| original_match(*other, claim));
            assert_eq!(index.first_match(&prepared), expected);
            match expected {
                Some(position) if turn % 3 == 0 => {
                    index.insert(position, prepared);
                    retained[position] = claim;
                },
                Some(_) => {},
                None => {
                    index.insert(retained.len(), prepared);
                    retained.push(claim);
                },
            }
        }
    }

    #[test]
    fn indexed_matching_uses_earliest_position_and_refreshes_winner_words() {
        let key = Some("content guidance");
        let a = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let b = "alpha bravo charlie delta echo foxtrot golf hotel kilo lima";
        let bridge = format!("{a} kilo lima");
        let mut index = MemoryClaimSimilarityIndex::default();
        index.insert(20, PreparedMemoryClaim::new(key, a));
        index.insert(4, PreparedMemoryClaim::new(key, b));
        let bridge = PreparedMemoryClaim::new(key, &bridge);
        assert_eq!(index.first_match(&bridge), Some(4));
        index.insert(
            4,
            PreparedMemoryClaim::new(key, "november oscar papa quebec romeo"),
        );
        assert_eq!(index.first_match(&bridge), Some(20));
        assert!(!index.postings["alpha"].contains(&4));
    }

    #[test]
    fn index_prunes_distinct_claims_before_pairwise_matching() {
        let mut index = MemoryClaimSimilarityIndex::default();
        for position in 0..1000 {
            index.insert(
                position,
                PreparedMemoryClaim::new(None, &format!("alpha bravo charlie topic{position}")),
            );
        }
        let claim = PreparedMemoryClaim::new(None, "alpha bravo charlie another-topic");
        assert!(index.possible_matches(&claim).is_empty());
        assert_eq!(index.first_match(&claim), None);
    }
}
