//! Tier 1: Rule-Based Matching
//!
//! This module implements fast rule-based tool matching using:
//! - Keyword matching with fuzzy tolerance
//! - Pattern matching with regex
//! - Category bonus scoring
//!
//! ## Performance
//! - <100ms typical execution time
//! - Pre-compiled regex patterns for speed
//! - O(n) complexity for n candidate tools
//!
//! ## Scoring
//! - Base weight: 15% of total score
//! - Category bonus: +0.15 for category matches
//! - Keyword matches: +0.10 per keyword (max 0.40)
//! - Pattern matches: +0.08 per pattern (max 0.32)

use regex::Regex;
use tracing::{debug, info};

use super::{config::ToolMatcherConfig, types::ToolCandidate};

/// Rule-based matcher for Tier 1
pub struct RuleMatcher {
    /// Configuration for matching behavior
    config: ToolMatcherConfig,

    /// Pre-compiled common patterns for fast matching
    common_patterns: Vec<CompiledPattern>,
}

/// Pre-compiled regex pattern with metadata
struct CompiledPattern {
    /// Pattern name for debugging
    name: String,
    /// Compiled regex
    regex: Regex,
    /// Score boost if pattern matches
    score_boost: f32,
}

impl RuleMatcher {
    /// Create a new rule-based matcher
    pub fn new(config: ToolMatcherConfig) -> Self {
        let common_patterns = Self::compile_common_patterns();

        info!(
            "[MAGICIAN-V2-RULE] RuleMatcher initialized with {} pre-compiled patterns",
            common_patterns.len()
        );

        Self {
            config,
            common_patterns,
        }
    }

    /// Score candidates using rule-based matching
    ///
    /// Updates each candidate's rule_score based on:
    /// - Keyword matches in task description
    /// - Pattern matches
    /// - Category bonus
    /// - Hierarchical context (parent task/tool for disambiguation)
    ///
    /// # Arguments
    /// * `task` - Task description to match against
    /// * `candidates` - Mutable candidates to score
    /// * `parent_task` - Optional parent task for context
    /// * `parent_tool_name` - Optional parent tool name for context
    pub fn score_candidates(
        &self,
        task: &str,
        candidates: &mut [ToolCandidate],
        parent_task: Option<&str>,
        _parent_tool_name: Option<&str>,
    ) {
        let start_time = std::time::Instant::now();
        let task_lower = task.to_lowercase();

        // Build context string for disambiguation
        let parent_context = parent_task.map(|p| p.to_lowercase());

        if let Some(ref ctx) = parent_context {
            debug!(
                "[MAGICIAN-V2-MATCHER] Tier 1 - Rule Matcher: Scoring {} candidates for task '{}' \
                 with parent context '{}'",
                candidates.len(),
                task,
                ctx
            );
        } else {
            debug!(
                "[MAGICIAN-V2-MATCHER] Tier 1 - Rule Matcher: Scoring {} candidates for task '{}'",
                candidates.len(),
                task
            );
        }

        for candidate in candidates.iter_mut() {
            let mut score = 0.0f32;
            let mut match_details = Vec::new();

            // 1. Keyword matching from tool name
            let name_keywords = self.extract_keywords(&candidate.tool_name);
            let mut keyword_matches = 0;
            for keyword in &name_keywords {
                if task_lower.contains(&keyword.to_lowercase()) {
                    score += 0.10;
                    keyword_matches += 1;
                    match_details.push(format!("keyword:{}", keyword));
                }
            }
            // Cap keyword score at 0.40
            if keyword_matches > 0 {
                score = score.min(0.40);
            }

            // 2. Keyword matching from description
            let desc_keywords = self.extract_keywords(&candidate.tool_info.description);
            for keyword in desc_keywords.iter().take(5) {
                // Limit to top 5 from description
                if task_lower.contains(&keyword.to_lowercase()) {
                    score += 0.05;
                    match_details.push(format!("desc_keyword:{}", keyword));
                }
            }

            // 3. Pattern matching using pre-compiled patterns
            let mut pattern_score = 0.0f32;
            for pattern in &self.common_patterns {
                if pattern.regex.is_match(&task_lower) {
                    pattern_score += pattern.score_boost;
                    match_details.push(format!("pattern:{}", pattern.name));
                }
            }
            // Cap pattern score at 0.32
            score += pattern_score.min(0.32);

            // 4. Category bonus (if tool matched suggested categories)
            if candidate.category_matched {
                let category_bonus = self.config.category_bonuses.rule_category_bonus;
                score += category_bonus;
                match_details.push(format!("category_bonus:+{:.2}", category_bonus));
            }

            // 5. Exact name match boost
            if candidate.tool_name.to_lowercase() == task_lower {
                score += 0.20;
                match_details.push("exact_name_match".to_string());
            }

            // 6. Hierarchical context scoring (parent task awareness)
            if let Some(ref parent_ctx) = parent_context {
                let tool_category_lower = candidate.category.to_lowercase();
                let tool_name_lower = candidate.tool_name.to_lowercase();

                // Define domain keywords for context detection
                let web_keywords = [
                    "google", "search", "web", "internet", "browser", "http", "url",
                ];
                let database_keywords = [
                    "database", "db", "sql", "table", "record", "postgres", "mysql",
                ];
                let file_keywords = ["file", "filesystem", "directory", "folder", "path"];
                let network_keywords = ["network", "ping", "dns", "tcp", "ip", "port"];

                // Detect parent domain
                let parent_is_web = web_keywords.iter().any(|kw| parent_ctx.contains(kw));
                let parent_is_database = database_keywords.iter().any(|kw| parent_ctx.contains(kw));
                let parent_is_file = file_keywords.iter().any(|kw| parent_ctx.contains(kw));
                let parent_is_network = network_keywords.iter().any(|kw| parent_ctx.contains(kw));

                // Context boost: +0.15 for matching domain
                if parent_is_web
                    && (tool_category_lower.contains("web")
                        || tool_category_lower.contains("search")
                        || tool_name_lower.contains("search"))
                {
                    score += 0.15;
                    match_details.push("context_boost:web".to_string());
                } else if parent_is_database && tool_category_lower.contains("database") {
                    score += 0.15;
                    match_details.push("context_boost:database".to_string());
                } else if parent_is_file
                    && (tool_category_lower.contains("file")
                        || tool_category_lower.contains("filesystem"))
                {
                    score += 0.15;
                    match_details.push("context_boost:file".to_string());
                } else if parent_is_network && tool_category_lower.contains("network") {
                    score += 0.15;
                    match_details.push("context_boost:network".to_string());
                }

                // Context penalty: -0.15 for contradictory domain
                // Example: parent="google search" but tool is "database_query_executor"
                if parent_is_web
                    && tool_category_lower.contains("database")
                    && !tool_name_lower.contains("search")
                {
                    score -= 0.15;
                    match_details.push("context_penalty:database_not_web".to_string());
                } else if parent_is_database
                    && (tool_category_lower.contains("web")
                        || tool_category_lower.contains("search"))
                    && !tool_name_lower.contains("database")
                {
                    score -= 0.15;
                    match_details.push("context_penalty:web_not_database".to_string());
                }
            }

            // Update candidate's rule score (ensure non-negative)
            candidate.rule_score = score.max(0.0);

            if score > 0.0 {
                debug!(
                    "[MAGICIAN-V2-MATCHER]   {} → rule_score={:.3} ({})",
                    candidate.tool_name,
                    score,
                    match_details.join(", ")
                );
            }
        }

        let elapsed = start_time.elapsed();
        let avg_score: f32 =
            candidates.iter().map(|c| c.rule_score).sum::<f32>() / candidates.len().max(1) as f32;

        info!(
            "[MAGICIAN-V2-RULE] Tier 1 complete: Scored {} candidates (avg={:.3}) in {:?}",
            candidates.len(),
            avg_score,
            elapsed
        );
    }

    /// Extract meaningful keywords from text
    ///
    /// Filters out common words and extracts significant terms
    fn extract_keywords(&self, text: &str) -> Vec<String> {
        // Common stop words to filter out
        const STOP_WORDS: &[&str] = &[
            "the", "a", "an", "and", "or", "but", "is", "are", "was", "were", "to", "from", "with",
            "for", "of", "in", "on", "at", "by", "this", "that", "these", "those", "tool", "using",
            "use",
        ];

        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter_map(|word| {
                let word_lower = word.to_lowercase();
                if word_lower.len() >= 3
                    && !STOP_WORDS.contains(&word_lower.as_str())
                    && !word_lower.chars().all(|c| c.is_numeric())
                {
                    Some(word.to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Compile common patterns for fast matching
    fn compile_common_patterns() -> Vec<CompiledPattern> {
        vec![
            // Network patterns
            CompiledPattern {
                name: "ping".to_string(),
                regex: Regex::new(r"\b(ping|check|test)\s+(connect|network|host|server)").unwrap(),
                score_boost: 0.08,
            },
            CompiledPattern {
                name: "http_request".to_string(),
                regex: Regex::new(r"\b(http|https|get|post|put|delete)\s+(request|call|api)")
                    .unwrap(),
                score_boost: 0.08,
            },
            CompiledPattern {
                name: "dns_lookup".to_string(),
                regex: Regex::new(r"\b(dns|lookup|resolve|nslookup|dig)").unwrap(),
                score_boost: 0.08,
            },
            // File operations
            CompiledPattern {
                name: "file_read".to_string(),
                regex: Regex::new(r"\b(read|cat|view|show)\s+(file|content)").unwrap(),
                score_boost: 0.08,
            },
            CompiledPattern {
                name: "file_write".to_string(),
                regex: Regex::new(r"\b(write|save|create)\s+(file|content)").unwrap(),
                score_boost: 0.08,
            },
            CompiledPattern {
                name: "file_move".to_string(),
                regex: Regex::new(r"\b(move|rename|copy)\s+file").unwrap(),
                score_boost: 0.08,
            },
            // Database operations
            CompiledPattern {
                name: "db_query".to_string(),
                regex: Regex::new(r"\b(query|select|find|search)\s+(database|db|table|record)")
                    .unwrap(),
                score_boost: 0.08,
            },
            CompiledPattern {
                name: "db_update".to_string(),
                regex: Regex::new(r"\b(update|insert|delete|modify)\s+(database|db|table|record)")
                    .unwrap(),
                score_boost: 0.08,
            },
            // System operations
            CompiledPattern {
                name: "process_list".to_string(),
                regex: Regex::new(r"\b(list|show|view)\s+(process|service|task)").unwrap(),
                score_boost: 0.08,
            },
            CompiledPattern {
                name: "system_info".to_string(),
                regex: Regex::new(r"\b(system|server|machine)\s+(info|status|health|metrics)")
                    .unwrap(),
                score_boost: 0.08,
            },
        ]
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::{magician_v2::tool_matcher::config::ToolMatcherConfig, ToolInfo};

    fn create_test_tool(
        name: &str,
        description: &str,
        category: &str,
        category_matched: bool,
    ) -> ToolCandidate {
        let tool_info = ToolInfo {
            name: name.to_string(),
            description: description.to_string(),
            category: category.to_string(),
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            categories: vec![category.to_string()], // Populate with primary category
            composition_category: None,
            providing_agent_id: None,
        };
        ToolCandidate::new(tool_info, category_matched)
    }

    #[test]
    fn test_keyword_matching() {
        let config = ToolMatcherConfig::default();
        let matcher = RuleMatcher::new(config);

        let mut candidates = vec![
            create_test_tool(
                "ping_network",
                "Ping host to check network connectivity",
                "network",
                false,
            ),
            create_test_tool("file_reader", "Read file contents", "file", false),
        ];

        matcher.score_candidates("ping google.com", &mut candidates, None, None);

        // "ping_network" should score higher due to keyword match
        assert!(candidates[0].rule_score > candidates[1].rule_score);
        assert!(candidates[0].rule_score > 0.0);
    }

    #[test]
    fn test_category_bonus() {
        let mut config = ToolMatcherConfig::default();
        config.category_bonuses.rule_category_bonus = 0.15;
        let matcher = RuleMatcher::new(config);

        let mut candidates = vec![
            create_test_tool("network_tool", "Network operations", "network", true), /* category matched */
            create_test_tool("network_tool2", "Network operations", "network", false), /* no category match */
        ];

        matcher.score_candidates("network test", &mut candidates, None, None);

        // First tool should have category bonus
        assert!(candidates[0].rule_score > candidates[1].rule_score);
        // Category bonus should be applied
        assert!((candidates[0].rule_score - candidates[1].rule_score - 0.15).abs() < 0.01);
    }

    #[test]
    fn test_pattern_matching() {
        let config = ToolMatcherConfig::default();
        let matcher = RuleMatcher::new(config);

        let mut candidates = vec![
            create_test_tool("network_ping", "Ping network hosts", "network", false),
            create_test_tool("random_tool", "Random operations", "other", false),
        ];

        matcher.score_candidates("ping the server", &mut candidates, None, None);

        // Pattern "ping.*server" should match first tool
        assert!(candidates[0].rule_score > 0.0);
        assert!(candidates[0].rule_score > candidates[1].rule_score);
    }

    #[test]
    fn test_exact_name_match() {
        let config = ToolMatcherConfig::default();
        let matcher = RuleMatcher::new(config);

        let mut candidates = vec![
            create_test_tool("ping", "Ping tool", "network", false),
            create_test_tool("network_ping", "Network ping", "network", false),
        ];

        matcher.score_candidates("ping", &mut candidates, None, None);

        // Exact name match should get boost
        assert!(candidates[0].rule_score > candidates[1].rule_score);
    }

    #[test]
    fn test_keyword_extraction() {
        let config = ToolMatcherConfig::default();
        let matcher = RuleMatcher::new(config);

        let keywords = matcher.extract_keywords("Read file content from disk");

        // Should extract: "Read", "file", "content", "disk"
        // Should filter out: "from" (stop word)
        assert!(keywords.contains(&"Read".to_string()));
        assert!(keywords.contains(&"file".to_string()));
        assert!(keywords.contains(&"content".to_string()));
        assert!(keywords.contains(&"disk".to_string()));
        assert!(!keywords.contains(&"from".to_string()));
    }
}
