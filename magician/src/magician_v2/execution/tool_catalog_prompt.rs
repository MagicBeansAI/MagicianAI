//! Shared helpers for classifying tools in the merged-agent-tool list.
//!
//! ## Schema and tool listings live in the function-calling tools array
//!
//! This module used to also render prose `## AVAILABLE CAPABILITIES`
//! and `## CAPABILITY FOR THIS STEP` sections into outer-loop user
//! prompts. Those sections restated what the function-calling tools
//! array already delivers natively (name, description, parameter
//! schema) and were removed — restating the catalog in prose is
//! **wasteful** (pays the token cost twice every turn), **drift-prone**
//! (two sources of truth diverge), and **cache-busting** (per-turn
//! prompt churn defeats the provider's prompt cache).
//!
//! What remains in this module are the small classification helpers
//! that other code still needs to reason about pack ownership and
//! built-in lane membership.

use crate::magician_v2::execution::builtin_action_types::is_reserved_non_pack_capability_name;

pub fn is_builtin_capability_name(name: &str) -> bool {
    is_reserved_non_pack_capability_name(name)
}

pub fn is_direct_pack_tool(tool: &runtime_core::ToolInfo) -> bool {
    tool.providing_agent_id.is_none() && !is_builtin_capability_name(&tool.name)
}

pub fn has_direct_pack_tools(tools: &[runtime_core::ToolInfo]) -> bool {
    tools.iter().any(is_direct_pack_tool)
}

pub fn direct_pack_tool_names(tools: &[runtime_core::ToolInfo]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| is_direct_pack_tool(tool))
        .map(|tool| tool.name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        direct_pack_tool_names, has_direct_pack_tools, is_builtin_capability_name,
        is_direct_pack_tool,
    };

    #[test]
    fn direct_pack_tool_helpers_exclude_builtins_and_delegate_owned_tools() {
        let tools = vec![
            runtime_core::ToolInfo {
                name: "browser".to_string(),
                description: "Browser automation".to_string(),
                category: "browser".to_string(),
                categories: vec!["browser".to_string()],
                parameters: Vec::new(),
                enhanced_description: None,
                keywords: Vec::new(),
                use_cases: Vec::new(),
                composition_category: None,
                providing_agent_id: None,
            },
            runtime_core::ToolInfo {
                name: "websearch".to_string(),
                description: "Search".to_string(),
                category: "search".to_string(),
                categories: vec!["search".to_string()],
                parameters: Vec::new(),
                enhanced_description: None,
                keywords: Vec::new(),
                use_cases: Vec::new(),
                composition_category: None,
                providing_agent_id: None,
            },
            runtime_core::ToolInfo {
                name: "keka_clock".to_string(),
                description: "Clock in".to_string(),
                category: "browser".to_string(),
                categories: vec!["browser".to_string()],
                parameters: Vec::new(),
                enhanced_description: None,
                keywords: Vec::new(),
                use_cases: Vec::new(),
                composition_category: None,
                providing_agent_id: Some("keka-agent".to_string()),
            },
        ];

        assert!(!is_builtin_capability_name("browser"));
        assert!(is_direct_pack_tool(&tools[1]));
        assert!(is_direct_pack_tool(&tools[0]));
        assert!(!is_direct_pack_tool(&tools[2]));
        assert!(has_direct_pack_tools(&tools));
        assert_eq!(direct_pack_tool_names(&tools), vec!["browser", "websearch"]);
    }
}
