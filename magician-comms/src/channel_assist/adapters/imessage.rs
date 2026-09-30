use super::super::adapter_registry::{
    ChannelActionAdapter, ChannelAdapter, ChannelCapabilities, ChannelConnectionStatus,
};
use super::super::assist::distill::ContentFetcher;
use super::super::ingest::{ChannelIngestor, IngestContext};
use super::super::ingest_imessage::{
    ImessageContentFetcher, ImessageWuIngestor, IMESSAGE_PROVIDER,
};
use super::super::registry::ChannelAccount;
use super::imessage_actions::IMESSAGE_ACTION_ADAPTER;

pub struct ImessageAdapter;

impl ChannelAdapter for ImessageAdapter {
    fn provider(&self) -> &'static str {
        IMESSAGE_PROVIDER
    }

    fn capabilities(&self) -> ChannelCapabilities {
        // Phase 1 = pull + content + connection status. Phase 2 flips outbound
        // send + draft creation on (the reply action adapter below).
        ChannelCapabilities::pull_with_content()
            .with_connection_status()
            .with_outbound_send_and_draft()
    }

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        Some(Box::new(ImessageWuIngestor))
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        Some(Box::new(ImessageContentFetcher))
    }

    fn connection_status(&self) -> Option<&dyn ChannelConnectionStatus> {
        Some(self)
    }

    fn action_adapter(&self) -> Option<&dyn ChannelActionAdapter> {
        Some(&IMESSAGE_ACTION_ADAPTER)
    }
}

impl ChannelConnectionStatus for ImessageAdapter {
    fn account_connected(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool {
        let ingestor = ImessageWuIngestor;
        ingestor.account_ready(ctx, account)
    }
}
