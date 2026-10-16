use super::super::adapter_registry::{
    ChannelAdapter, ChannelCapabilities, ChannelConnectionStatus,
};
use super::super::assist::distill::ContentFetcher;
use super::super::ingest::{ChannelIngestor, IngestContext};
use super::super::ingest_agentmail::{
    AgentMailContentFetcher, AgentMailIngestor, AGENTMAIL_PROVIDER,
};
use super::super::registry::ChannelAccount;

pub struct AgentMailAdapter;

impl ChannelAdapter for AgentMailAdapter {
    fn provider(&self) -> &'static str {
        AGENTMAIL_PROVIDER
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::pull_with_content().with_connection_status()
    }

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        Some(Box::new(AgentMailIngestor))
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        Some(Box::new(AgentMailContentFetcher))
    }

    fn connection_status(&self) -> Option<&dyn ChannelConnectionStatus> {
        Some(self)
    }
}

impl ChannelConnectionStatus for AgentMailAdapter {
    fn account_connected(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool {
        let ingestor = AgentMailIngestor;
        ingestor.account_ready(ctx, account)
    }
}
