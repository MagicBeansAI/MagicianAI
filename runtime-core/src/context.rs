use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};

/// Execution context for workflow operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionContext {
    /// Principal/user making the request
    pub principal: String,
    /// Workspace/tenant scope
    pub workspace: String,
    /// Extra metadata for context
    pub metadata: HashMap<String, String>,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self {
            principal: "system".to_string(),
            workspace: "default".to_string(),
            metadata: HashMap::new(),
        }
    }
}

/// User preferences for workflow execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub prefer_speed: f32,       // 0.0-1.0 weight for faster execution
    pub prefer_reliability: f32, // 0.0-1.0 weight for higher confidence
    pub prefer_simplicity: f32,  // 0.0-1.0 weight for fewer steps
    pub confidence_threshold: f32,
    pub max_execution_time: Option<Duration>,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            prefer_speed: 0.3,
            prefer_reliability: 0.5,
            prefer_simplicity: 0.2,
            confidence_threshold: 0.7,
            max_execution_time: Some(Duration::from_secs(300)), // 5 minutes
        }
    }
}
