use async_trait::async_trait;
use futures::stream::BoxStream;
use priest::context_builder::Message;
use priest::errors::PriestError;
use priest::providers::adapter::{AdapterResult, ProviderAdapter};
use priest::schema::config::PriestConfig;
use priest::schema::request::OutputSpec;

pub struct MockAdapter {
    pub response_text: String,
    pub error: Option<PriestError>,
}

impl MockAdapter {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { response_text: text.into(), error: None }
    }

    pub fn failing(e: PriestError) -> Self {
        Self { response_text: String::new(), error: Some(e) }
    }
}

#[async_trait]
impl ProviderAdapter for MockAdapter {
    async fn complete(
        &self,
        _messages: &[Message],
        config: &PriestConfig,
        _output_spec: &OutputSpec,
    ) -> Result<AdapterResult, PriestError> {
        if let Some(ref e) = self.error {
            return Err(PriestError::ProviderError {
                provider: config.provider.clone(),
                message: e.to_string(),
            });
        }
        Ok(AdapterResult {
            text: self.response_text.clone(),
            finish_reason: Some("stop".into()),
            input_tokens: Some(10),
            output_tokens: Some(5),
        })
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        if let Some(ref e) = self.error {
            let msg = e.to_string();
            return Ok(Box::pin(futures::stream::once(async move {
                Err(PriestError::ProviderError { provider: "mock".into(), message: msg })
            })));
        }
        // Yield whole text as a single chunk so reassembly matches exactly
        let text = self.response_text.clone();
        Ok(Box::pin(futures::stream::once(async move { Ok(text) })))
    }
}
