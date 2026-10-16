use super::super::adapter_registry::{
    ChannelAdapter, ChannelCapabilities, ChannelConnectionStatus,
};
use super::super::assist::distill::ContentFetcher;
use super::super::ingest::{ChannelIngestor, IngestContext};
use super::super::ingest_kapso::{KapsoContentFetcher, WhatsappKapsoIngestor, KAPSO_PROVIDER};
use super::super::registry::ChannelAccount;

pub struct WhatsappKapsoAdapter;

impl ChannelAdapter for WhatsappKapsoAdapter {
    fn provider(&self) -> &'static str {
        KAPSO_PROVIDER
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::pull_with_content().with_connection_status()
    }

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        Some(Box::new(WhatsappKapsoIngestor))
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        Some(Box::new(KapsoContentFetcher))
    }

    fn connection_status(&self) -> Option<&dyn ChannelConnectionStatus> {
        Some(self)
    }
}

impl ChannelConnectionStatus for WhatsappKapsoAdapter {
    fn account_connected(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool {
        let ingestor = WhatsappKapsoIngestor;
        ingestor.account_ready(ctx, account)
    }
}
