use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;
use crate::schema::reasoning::ReasoningInfo;
use crate::schema::tools::{ProviderToolDefinition, ToolCall, ToolChoice, ToolDefinition};
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
    /// Provider-reported reasoning tokens (a subset of output tokens).
    pub reasoning_tokens: Option<u32>,
    /// Tool calls requested by the model (spec 2.4.0). None when there are none.
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Safe provider-supplied reasoning information.
    pub reasoning: Option<ReasoningInfo>,
}

/// One structured streaming event from a provider adapter.
#[derive(Debug, Clone)]
pub enum AdapterStreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningSummaryDelta {
        text: String,
    },
    ToolCallStart {
        index: usize,
        id: Option<String>,
        name: Option<String>,
    },
    ToolCallDelta {
        index: usize,
        arguments_delta: String,
    },
    ToolCallEnd {
        index: usize,
        tool_call: ToolCall,
    },
    Usage {
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        cached_input_tokens: Option<u32>,
        reasoning_tokens: Option<u32>,
    },
    Finish {
        finish_reason: Option<String>,
        reasoning: Option<ReasoningInfo>,
    },
}

/// Per-call options threaded from the engine into adapters (spec 2.4.0).
#[derive(Debug, Clone, Default)]
pub struct AdapterCallOptions {
    pub tools: Vec<ToolDefinition>,
    pub provider_tools: Vec<ProviderToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
}

/// Cancellation: Rust maps the spec's cancellation concept to dropping the
/// future / stream — adapters must not block detached threads.
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Whether this adapter can execute the provider-owned tool for the
    /// selected model/configuration. The default supports none.
    fn supports_provider_tool(
        &self,
        _tool: &ProviderToolDefinition,
        _config: &PriestConfig,
    ) -> bool {
        false
    }

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

    /// Yield structured events. Adapters may override this for native
    /// reasoning/tool/usage events; the default wraps the legacy text stream.
    async fn stream_events(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<AdapterStreamEvent, PriestError>>, PriestError> {
        use futures::StreamExt;

        let text = self.stream(messages, config, output_spec, options).await?;
        let events = text
            .map(|item| item.map(|text| AdapterStreamEvent::TextDelta { text }))
            .chain(futures::stream::once(async {
                Ok(AdapterStreamEvent::Finish {
                    finish_reason: Some("stop".into()),
                    reasoning: None,
                })
            }));
        Ok(Box::pin(events))
    }
}
