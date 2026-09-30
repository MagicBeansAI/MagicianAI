use anyhow::Result;
use async_trait::async_trait;

use super::types::{
    ContentDocument, ContentSourceDescriptor, DiscoveryPage, DiscoveryRequest, ReadRequest,
};

#[async_trait]
pub trait DiscoveryAdapter: Send + Sync {
    fn descriptor(&self) -> &ContentSourceDescriptor;

    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage>;
}

#[async_trait]
pub trait ContentReader: Send + Sync {
    fn descriptor(&self) -> &ContentSourceDescriptor;

    async fn read(&self, request: &ReadRequest) -> Result<ContentDocument>;
}
