use anyhow::Result;

use crate::client::config::{AppConfig, ConsumerConfig};
use crate::client::consumer::{ConsumedMessage, Consumer, MessageHandler};

pub struct ConsumerBuilder {
    broker_addr: String,
    config: ConsumerConfig,
}

impl ConsumerBuilder {
    pub fn new(broker_addr: impl Into<String>, topic: impl Into<String>) -> Self {
        let topic = topic.into();
        let mut config = AppConfig::default_consumer(topic, None)
            .consumer
            .expect("default consumer config must exist");
        config.auto_commit = false;
        Self {
            broker_addr: broker_addr.into(),
            config,
        }
    }

    pub fn group_id(mut self, group_id: impl Into<String>) -> Self {
        self.config.group_id = Some(group_id.into());
        self
    }

    pub fn auto_commit(mut self, enabled: bool) -> Self {
        self.config.auto_commit = enabled;
        self
    }

    pub fn latest(mut self) -> Self {
        self.config.offset = -1;
        self
    }

    pub fn earliest(mut self) -> Self {
        self.config.offset = -2;
        self
    }

    pub fn with_config(mut self, config: ConsumerConfig) -> Self {
        self.config = config;
        self
    }

    pub async fn build(self) -> Result<ConsumerRuntime> {
        let inner = Consumer::new(&self.broker_addr, self.config).await?;
        Ok(ConsumerRuntime { inner })
    }
}

pub struct ConsumerRuntime {
    inner: Consumer,
}

impl ConsumerRuntime {
    pub async fn run<H: MessageHandler + 'static>(&mut self, handler: H) -> Result<()> {
        self.inner.start(handler).await
    }

    pub async fn poll(&mut self) -> Result<Vec<ConsumedMessage>> {
        self.inner.poll().await
    }

    pub async fn commit(&self) -> Result<()> {
        self.inner.commit().await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown().await
    }
}
