use async_trait::async_trait;

use crate::magician_v2::progress_channel_seam::types::{ProgressMessage, Subscription};

#[async_trait]
pub trait ProgressChannel: Send + Sync {
    fn id(&self) -> &str;

    async fn deliver(
        &self,
        subscription: &Subscription,
        message: &ProgressMessage,
    ) -> anyhow::Result<()>;
}
