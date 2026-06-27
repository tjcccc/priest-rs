use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;
use crate::schema::tools::{ToolCall, ToolChoice, ToolDefinition};
use async_trait::async_trait;
use futures::stream::BoxStream;

#[derive(Debug, Clone)]
pub struct AdapterResult {
    pub text: String,
    pub finish_reason: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Prompt-cache hit count (spec 2.5.0). None when the provider omits it.
    pub cached_input_tokens: Option<u32>,
    /// Tool calls requested by the model (spec 2.4.0). None when there are none.
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Per-call options threaded from the engine into adapters (spec 2.4.0).
#[derive(Debug, Clone, Default)]
pub struct AdapterCallOptions {
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
}

/// Cancellation: Rust maps the spec's cancellation concept to dropping the
/// future / stream — adapters must not block detached threads.
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError>;

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError>;
}
