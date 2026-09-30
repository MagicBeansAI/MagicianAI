use super::super::adapter_registry::{
    ChannelAdapter, ChannelCapabilities, ChannelConnectionStatus,
};
use super::super::assist::distill::ContentFetcher;
use super::super::ingest::{ChannelIngestor, IngestContext};
use super::super::ingest_whatsapp::{
    WhatsappContentFetcher, WhatsappWuIngestor, WHATSAPP_PROVIDER,
};
use super::super::registry::ChannelAccount;

pub struct WhatsappWuAdapter;

impl ChannelAdapter for WhatsappWuAdapter {
    fn provider(&self) -> &'static str {
        WHATSAPP_PROVIDER
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::pull_with_content().with_connection_status()
    }

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        Some(Box::new(WhatsappWuIngestor))
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        Some(Box::new(WhatsappContentFetcher))
    }

    fn connection_status(&self) -> Option<&dyn ChannelConnectionStatus> {
        Some(self)
    }
}

impl ChannelConnectionStatus for WhatsappWuAdapter {
    fn account_connected(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool {
        let ingestor = WhatsappWuIngestor;
        ingestor.account_ready(ctx, account)
    }
}
