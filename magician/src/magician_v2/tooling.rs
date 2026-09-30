use std::{collections::HashMap, sync::Arc};

use crate::ToolDiscovery;
use anyhow::Result;
use runtime_core::{
    prelude::async_trait, ExecutionContext, MultipleToolMatchResult, ToolCatalog, ToolInfo,
    ToolMatchResult, ToolMatching,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

/// Temporary adapter that exposes the legacy `ToolDiscovery` interface via the
/// new boundary traits.
///
/// This allows Magician V2 to depend on `ToolCatalog` and `ToolMatching` while
/// we gradually replace direct usages of `ToolDiscovery`.
#[derive(Clone)]
pub struct ToolDiscoveryAdapter {
    inner: Arc<dyn ToolDiscovery>,
}

impl ToolDiscoveryAdapter {
    fn to_legacy_context(context: &ExecutionContext) -> crate::ExecutionContext {
        crate::ExecutionContext {
            principal: context.principal.clone(),
            workspace: context.workspace.clone(),
            metadata: context.metadata.clone(),
        }
    }

    fn convert<T, U>(value: T) -> U
    where
        T: Serialize,
        U: DeserializeOwned,
    {
        let json = serde_json::to_value(value).expect("serialize legacy value");
        serde_json::from_value(json).expect("convert legacy value")
    }

    fn convert_vec<T, U>(values: Vec<T>) -> Vec<U>
    where
        T: Serialize,
        U: DeserializeOwned,
    {
        values.into_iter().map(Self::convert).collect()
    }

    pub fn new(inner: Arc<dyn ToolDiscovery>) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> Arc<dyn ToolDiscovery> {
        Arc::clone(&self.inner)
    }
}

#[async_trait]
impl ToolCatalog for ToolDiscoveryAdapter {
    async fn list_tool_names(&self, context: &ExecutionContext) -> Vec<String> {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner.get_available_tools(&legacy_ctx).await
    }

    async fn available_categories(&self, context: &ExecutionContext) -> Vec<String> {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner.get_available_categories(&legacy_ctx).await
    }

    async fn get_tool_metadata(
        &self,
        tool_name: &str,
        context: &ExecutionContext,
    ) -> Option<HashMap<String, Value>> {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner.get_tool_metadata(tool_name, &legacy_ctx).await
    }

    async fn filtered_tools_by_categories(
        &self,
        categories: &[String],
        context: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>> {
        let legacy_ctx = Self::to_legacy_context(context);
        let legacy = self
            .inner
            .get_filtered_tools_by_categories(categories, &legacy_ctx)
            .await?;
        Ok(Self::convert_vec(legacy))
    }

    async fn all_tools(&self, context: &ExecutionContext) -> Result<Vec<ToolInfo>> {
        let legacy_ctx = Self::to_legacy_context(context);
        let legacy = self.inner.get_all_filtered_tools(&legacy_ctx).await?;
        Ok(Self::convert_vec(legacy))
    }

    async fn category_tool_counts(
        &self,
        context: &ExecutionContext,
    ) -> Result<HashMap<String, usize>> {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner.get_category_tool_counts(&legacy_ctx).await
    }
}

#[async_trait]
impl ToolMatching for ToolDiscoveryAdapter {
    async fn best_match(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> ToolMatchResult {
        let legacy_ctx = Self::to_legacy_context(context);
        let legacy = self
            .inner
            .find_best_match_with_context(task_description, &legacy_ctx)
            .await;
        Self::convert(legacy)
    }

    async fn multiple_matches(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> MultipleToolMatchResult {
        let legacy_ctx = Self::to_legacy_context(context);
        let legacy = self
            .inner
            .find_multiple_matches_with_context(task_description, &legacy_ctx)
            .await;
        Self::convert(legacy)
    }

    async fn is_tool_available(&self, tool_name: &str, context: &ExecutionContext) -> bool {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner.is_tool_available(tool_name, &legacy_ctx).await
    }

    async fn validate_tool_execution(
        &self,
        tool_name: &str,
        parameters: &Value,
        context: &ExecutionContext,
    ) -> bool {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner
            .validate_tool_execution(tool_name, parameters, &legacy_ctx)
            .await
    }

    async fn estimate_execution_cost(
        &self,
        tool_name: &str,
        parameters: &Value,
        context: &ExecutionContext,
    ) -> f64 {
        let legacy_ctx = Self::to_legacy_context(context);
        self.inner
            .estimate_execution_cost(tool_name, parameters, &legacy_ctx)
            .await
    }
}
