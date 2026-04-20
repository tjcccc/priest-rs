use async_trait::async_trait;
use futures::stream::BoxStream;
use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;

#[derive(Debug, Clone)]
pub struct AdapterResult {
    pub text: String,
    pub finish_reason: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<AdapterResult, PriestError>;

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError>;
}
