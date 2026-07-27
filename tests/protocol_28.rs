use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use priest::context_builder::Message;
use priest::{
    built_in_default, run_with_tools, AdapterCallOptions, AdapterResult, AdapterStreamEvent,
    OpaqueReasoningState, OutputSpec, PriestConfig, PriestEngine, PriestError, PriestRequest,
    PriestStreamEvent, Profile, ProfileLoader, ProviderAdapter, ReasoningInfo, ToolCall,
    ToolDefinition, ToolExecutionResult, ToolExecutor,
};
use serde_json::json;

struct StaticLoader;

impl ProfileLoader for StaticLoader {
    fn load(&self, _name: &str) -> Result<Profile, PriestError> {
        Ok(built_in_default())
    }
}

fn reasoning() -> ReasoningInfo {
    ReasoningInfo {
        summary: Some("Checked the constraints.".into()),
        continuation: Some(vec![OpaqueReasoningState {
            format: "test.reasoning.v1".into(),
            value: json!({"opaque": true}),
        }]),
    }
}

struct ReasoningAdapter;

#[async_trait]
impl ProviderAdapter for ReasoningAdapter {
    async fn complete(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        Ok(AdapterResult {
            text: "answer".into(),
            finish_reason: Some("content_filter".into()),
            input_tokens: Some(10),
            output_tokens: Some(8),
            cached_input_tokens: Some(4),
            reasoning_tokens: Some(6),
            tool_calls: None,
            reasoning: Some(reasoning()),
        })
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        Ok(Box::pin(futures::stream::once(async {
            Ok("answer".into())
        })))
    }

    async fn stream_events(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<AdapterStreamEvent, PriestError>>, PriestError> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(AdapterStreamEvent::ReasoningSummaryDelta {
                text: "Checked".into(),
            }),
            Ok(AdapterStreamEvent::TextDelta {
                text: "answer".into(),
            }),
            Ok(AdapterStreamEvent::Usage {
                input_tokens: Some(10),
                output_tokens: Some(8),
                cached_input_tokens: Some(4),
                reasoning_tokens: Some(6),
            }),
            Ok(AdapterStreamEvent::Finish {
                finish_reason: Some("stop".into()),
                reasoning: Some(reasoning()),
            }),
        ])))
    }
}

#[tokio::test]
async fn engine_surfaces_reasoning_usage_and_content_filter() {
    let engine =
        PriestEngine::new(Arc::new(StaticLoader)).register("mock", Box::new(ReasoningAdapter));
    let response = engine
        .run(PriestRequest::new(
            PriestConfig::new("mock", "test-model"),
            "Hi",
        ))
        .await
        .unwrap();

    assert_eq!(
        response.execution.finished_reason.as_deref(),
        Some("content_filter")
    );
    assert_eq!(
        response
            .reasoning
            .as_ref()
            .and_then(|info| info.summary.as_deref()),
        Some("Checked the constraints.")
    );
    let usage = response.usage.unwrap();
    assert_eq!(usage.reasoning_tokens, Some(6));
    assert_eq!(usage.total_tokens, Some(18));
}

#[tokio::test]
async fn engine_forwards_reasoning_summary_stream_events() {
    let engine =
        PriestEngine::new(Arc::new(StaticLoader)).register("mock", Box::new(ReasoningAdapter));
    let events: Vec<PriestStreamEvent> = engine
        .stream_events(PriestRequest::new(
            PriestConfig::new("mock", "test-model"),
            "Hi",
        ))
        .await
        .unwrap()
        .collect()
        .await;

    assert!(matches!(
        &events[0],
        PriestStreamEvent::ReasoningSummaryDelta { text } if text == "Checked"
    ));
    let PriestStreamEvent::Done { response } = events.last().unwrap() else {
        panic!("expected done event");
    };
    assert_eq!(
        response
            .reasoning
            .as_ref()
            .and_then(|info| info.summary.as_deref()),
        Some("Checked the constraints.")
    );
    assert_eq!(response.usage.as_ref().unwrap().total_tokens, Some(18));
}

struct ToolAdapter {
    cursor: Mutex<usize>,
    calls: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl ProviderAdapter for ToolAdapter {
    async fn complete(
        &self,
        messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        self.calls.lock().unwrap().push(messages.to_vec());
        let mut cursor = self.cursor.lock().unwrap();
        let result = if *cursor == 0 {
            AdapterResult {
                text: String::new(),
                finish_reason: Some("tool_calls".into()),
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
                reasoning_tokens: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "lookup".into(),
                    arguments: Default::default(),
                }]),
                reasoning: Some(reasoning()),
            }
        } else {
            AdapterResult {
                text: "done".into(),
                finish_reason: Some("stop".into()),
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
                reasoning_tokens: None,
                tool_calls: None,
                reasoning: None,
            }
        };
        *cursor += 1;
        Ok(result)
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct Executor;

#[async_trait]
impl ToolExecutor for Executor {
    async fn execute(&self, _call: &ToolCall) -> ToolExecutionResult {
        ToolExecutionResult {
            content: "found".into(),
            is_error: false,
        }
    }
}

#[tokio::test]
async fn tool_loop_copies_reasoning_into_assistant_exchange() {
    let calls = Arc::new(Mutex::new(vec![]));
    let adapter = ToolAdapter {
        cursor: Mutex::new(0),
        calls: calls.clone(),
    };
    let engine = PriestEngine::new(Arc::new(StaticLoader)).register("mock", Box::new(adapter));
    let mut request = PriestRequest::new(PriestConfig::new("mock", "test-model"), "Use a tool");
    request.tools = vec![ToolDefinition {
        name: "lookup".into(),
        description: String::new(),
        parameters: None,
    }];

    let result = run_with_tools(&engine, request, &Executor, None)
        .await
        .unwrap();
    let priest::ToolExchangeTurn::Assistant {
        reasoning: exchange_reasoning,
        ..
    } = &result.exchange[0]
    else {
        panic!("expected assistant exchange");
    };
    assert_eq!(
        exchange_reasoning
            .as_ref()
            .and_then(|info| info.summary.as_deref()),
        Some("Checked the constraints.")
    );
    assert_eq!(
        calls.lock().unwrap()[1]
            .iter()
            .find(|message| message.role == "assistant")
            .and_then(|message| message.reasoning.as_ref())
            .and_then(|info| info.summary.as_deref()),
        Some("Checked the constraints.")
    );
}
