use super::super::adapter_registry::{ChannelAdapter, ChannelCapabilities};
use super::super::assist::distill::ContentFetcher;
use super::super::ingest::ChannelIngestor;
use super::super::ingest_telegram::{
    TelegramChatContentFetcher, TelegramChatIngestor, TELEGRAM_PROVIDER,
};

pub struct TelegramAdapter;

impl ChannelAdapter for TelegramAdapter {
    fn provider(&self) -> &'static str {
        TELEGRAM_PROVIDER
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities::pull_with_content()
    }

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        Some(Box::new(TelegramChatIngestor))
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        Some(Box::new(TelegramChatContentFetcher))
    }
}
