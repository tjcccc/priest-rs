use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Map, Value};

use super::adapter::{AdapterCallOptions, AdapterResult, AdapterStreamEvent, ProviderAdapter};
use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::reasoning::{OpaqueReasoningState, ReasoningInfo, ReasoningSummaryMode};
use crate::schema::request::OutputSpec;
use crate::schema::tools::{parse_tool_arguments, ToolCall, ToolChoice};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";
const REASONING_FORMAT: &str = "openai.responses.reasoning.v1";

/// First-class OpenAI Responses provider, separate from Chat Completions.
pub struct OpenAIResponsesProvider {
    base_url: String,
    exact_url: Option<String>,
    api_key: Option<String>,
    headers: HashMap<String, String>,
    client: Client,
}

impl OpenAIResponsesProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            exact_url: None,
            api_key: Some(api_key.into()),
            headers: HashMap::new(),
            client: Client::new(),
        }
    }

    pub fn without_api_key() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            exact_url: None,
            api_key: None,
            headers: HashMap::new(),
            client: Client::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_exact_url(mut self, url: impl Into<String>) -> Self {
        self.exact_url = Some(url.into());
        self
    }

    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    fn url(&self) -> String {
        self.exact_url
            .clone()
            .unwrap_or_else(|| format!("{}/v1/responses", self.base_url.trim_end_matches('/')))
    }

    fn request(&self, payload: &Value) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(self.url())
            .header("content-type", "application/json")
            .json(payload);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        request
    }
}

impl Default for OpenAIResponsesProvider {
    fn default() -> Self {
        Self::without_api_key()
    }
}

#[async_trait]
impl ProviderAdapter for OpenAIResponsesProvider {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        let payload = build_payload(messages, config, output_spec, options, false);
        let response = self
            .request(&payload)
            .timeout(Duration::from_secs_f64(config.timeout_seconds))
            .send()
            .await
            .map_err(|error| map_reqwest_error(error, config.timeout_seconds))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| provider_error(error.to_string()))?;
        if !status.is_success() {
            return Err(provider_error(format!("HTTP {status}: {body}")));
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|error| provider_error(error.to_string()))?;
        parse_response(&value)
    }

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let events = self
            .stream_events(messages, config, output_spec, options)
            .await?;
        Ok(Box::pin(events.filter_map(|event| async move {
            match event {
                Ok(AdapterStreamEvent::TextDelta { text }) => Some(Ok(text)),
                Err(error) => Some(Err(error)),
                _ => None,
            }
        })))
    }

    async fn stream_events(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<AdapterStreamEvent, PriestError>>, PriestError> {
        let payload = build_payload(messages, config, output_spec, options, true);
        let response = self
            .request(&payload)
            .timeout(Duration::from_secs_f64(config.timeout_seconds))
            .send()
            .await
            .map_err(|error| map_reqwest_error(error, config.timeout_seconds))?;

        let status = response.status();
        // The Rust SDK's established streaming API buffers provider streams
        // before engine emission. Parsing the complete body still preserves SSE
        // frame boundaries across arbitrary transport chunks and LF/CRLF forms.
        let body = response
            .text()
            .await
            .map_err(|error| provider_error(error.to_string()))?;
        if !status.is_success() {
            return Err(provider_error(format!("HTTP {status}: {body}")));
        }
        let events = parse_sse_events(&body)?;
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

fn build_payload(
    messages: &[Message],
    config: &PriestConfig,
    output_spec: &OutputSpec,
    options: Option<&AdapterCallOptions>,
    stream: bool,
) -> Value {
    let mut payload = json!({"store": false});
    if let Some(max_output_tokens) = config.max_output_tokens {
        payload["max_output_tokens"] = json!(max_output_tokens);
    }
    if let Some(reasoning) = openai_reasoning(config) {
        payload["reasoning"] = reasoning;
    }
    if let Some(schema) = &output_spec.json_schema {
        payload["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": output_spec.json_schema_name,
                "schema": schema,
                "strict": output_spec.json_schema_strict,
            }
        });
    } else if output_spec.provider_format.as_deref() == Some("json") {
        payload["text"] = json!({"format": {"type": "json_object"}});
    }
    if let Some(options) = options.filter(|options| !options.tools.is_empty()) {
        payload["tools"] = Value::Array(
            options
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters.clone().unwrap_or_else(|| json!({})),
                    })
                })
                .collect(),
        );
        if let Some(choice) = &options.tool_choice {
            payload["tool_choice"] = match choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
            };
        }
    }

    for (key, value) in &config.provider_options {
        payload[key] = value.clone();
    }
    // Adapter-owned operation invariants override provider options.
    payload["model"] = json!(config.model);
    payload["input"] = Value::Array(responses_input(messages));
    payload["stream"] = json!(stream);
    payload
}

fn responses_input(messages: &[Message]) -> Vec<Value> {
    let mut input = vec![];
    for message in messages {
        if message.role == "tool" {
            input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id,
                "output": message.content,
            }));
            continue;
        }
        if message.role == "assistant"
            && message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
        {
            for state in message
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.continuation.as_ref())
                .into_iter()
                .flatten()
            {
                if state.format == REASONING_FORMAT {
                    input.push(state.value.clone());
                }
            }
            for call in message.tool_calls.as_ref().unwrap() {
                input.push(json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": Value::Object(call.arguments.clone()).to_string(),
                }));
            }
            continue;
        }
        input.push(json!({
            "role": message.role,
            "content": [{"type": "input_text", "text": message.content}],
        }));
    }
    input
}

fn openai_reasoning(config: &PriestConfig) -> Option<Value> {
    let requested = config.reasoning.as_ref()?;
    let mut reasoning = Map::new();
    if let Some(effort) = requested.effort {
        reasoning.insert("effort".into(), json!(effort.as_str()));
    } else if requested.enabled == Some(false) {
        reasoning.insert("effort".into(), json!("none"));
    }
    if requested.summary == Some(ReasoningSummaryMode::Auto) {
        reasoning.insert("summary".into(), json!("auto"));
    }
    (!reasoning.is_empty()).then_some(Value::Object(reasoning))
}

fn parse_response(data: &Value) -> Result<AdapterResult, PriestError> {
    let status = data["status"].as_str();
    if matches!(status, Some("failed") | Some("cancelled")) || !data["error"].is_null() {
        return Err(response_error(data));
    }

    let mut text = String::new();
    let mut tool_calls = vec![];
    let mut summaries = vec![];
    let mut states = vec![];

    for item in data["output"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    if part["type"].as_str() == Some("output_text") {
                        if let Some(part_text) = part["text"].as_str() {
                            text.push_str(part_text);
                        }
                    }
                }
            }
            Some("function_call") => {
                let Some(name) = item["name"].as_str().filter(|name| !name.is_empty()) else {
                    continue;
                };
                tool_calls.push(ToolCall {
                    id: item["call_id"]
                        .as_str()
                        .or_else(|| item["id"].as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{}", tool_calls.len())),
                    name: name.to_string(),
                    arguments: parse_tool_arguments(item["arguments"].as_str().unwrap_or("")),
                });
            }
            Some("reasoning") => {
                for part in item["summary"].as_array().into_iter().flatten() {
                    if part["type"].as_str() == Some("summary_text") {
                        if let Some(summary) = part["text"].as_str().filter(|text| !text.is_empty())
                        {
                            summaries.push(summary.to_string());
                        }
                    }
                }
                if let Some(state) = safe_reasoning_state(item) {
                    states.push(state);
                }
            }
            _ => {}
        }
    }

    let has_tools = !tool_calls.is_empty();
    let continuation = (has_tools && !states.is_empty()).then_some(states);
    let reasoning = if summaries.is_empty() && continuation.is_none() {
        None
    } else {
        Some(ReasoningInfo {
            summary: (!summaries.is_empty()).then(|| summaries.join("\n\n")),
            continuation,
        })
    };
    let usage = &data["usage"];

    Ok(AdapterResult {
        text,
        finish_reason: Some(response_finish(data, has_tools)),
        input_tokens: usage["input_tokens"].as_u64().map(|value| value as u32),
        output_tokens: usage["output_tokens"].as_u64().map(|value| value as u32),
        cached_input_tokens: usage["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .map(|value| value as u32),
        reasoning_tokens: usage["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .map(|value| value as u32),
        tool_calls: has_tools.then_some(tool_calls),
        reasoning,
    })
}

fn safe_reasoning_state(item: &Value) -> Option<OpaqueReasoningState> {
    if item["content"]
        .as_array()
        .is_some_and(|content| !content.is_empty())
    {
        return None;
    }
    if item["encrypted_content"].is_null() && item["id"].is_null() {
        return None;
    }

    let mut value = Map::new();
    value.insert("type".into(), json!("reasoning"));
    for key in ["id", "status", "summary", "encrypted_content"] {
        if !item[key].is_null() {
            value.insert(key.into(), item[key].clone());
        }
    }
    Some(OpaqueReasoningState {
        format: REASONING_FORMAT.into(),
        value: Value::Object(value),
    })
}

fn response_finish(data: &Value, has_tools: bool) -> String {
    if has_tools {
        return "tool_calls".into();
    }
    if data["status"].as_str() == Some("incomplete") {
        return match data["incomplete_details"]["reason"].as_str() {
            Some("max_output_tokens") => "length",
            Some("content_filter") => "content_filter",
            _ => "unknown",
        }
        .into();
    }
    match data["status"].as_str() {
        None | Some("completed") => "stop".into(),
        _ => "unknown".into(),
    }
}

fn response_error(data: &Value) -> PriestError {
    let code = data["error"]["code"]
        .as_str()
        .map(|code| format!("{code}: "))
        .unwrap_or_default();
    let message = data["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "response status {}",
                data["status"].as_str().unwrap_or("failed")
            )
        });
    provider_error(format!("{code}{message}"))
}

fn sse_data(body: &str) -> Vec<String> {
    body.replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(|frame| {
            let lines: Vec<&str> = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        })
        .collect()
}

#[derive(Default)]
struct PartialCall {
    event_index: usize,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    ended: bool,
}

fn parse_sse_events(body: &str) -> Result<Vec<AdapterStreamEvent>, PriestError> {
    let mut events = vec![];
    let mut partials: HashMap<u64, PartialCall> = HashMap::new();
    let mut emitted_ids = HashSet::new();
    let mut next_index = 0usize;

    fn ensure_partial<'a>(
        partials: &'a mut HashMap<u64, PartialCall>,
        output_index: u64,
        item: Option<&Value>,
        next_index: &mut usize,
    ) -> &'a mut PartialCall {
        let partial = partials.entry(output_index).or_insert_with(|| {
            let event_index = *next_index;
            *next_index += 1;
            PartialCall {
                event_index,
                ..Default::default()
            }
        });
        if let Some(item) = item {
            partial.call_id = item["call_id"]
                .as_str()
                .map(str::to_string)
                .or_else(|| partial.call_id.clone());
            partial.name = item["name"]
                .as_str()
                .map(str::to_string)
                .or_else(|| partial.name.clone());
            if let Some(arguments) = item["arguments"].as_str() {
                partial.arguments = arguments.to_string();
            }
        }
        partial
    }

    fn finish_partial(
        partial: &mut PartialCall,
        emitted_ids: &mut HashSet<String>,
    ) -> Option<AdapterStreamEvent> {
        if partial.ended {
            return None;
        }
        partial.ended = true;
        let id = partial
            .call_id
            .clone()
            .unwrap_or_else(|| format!("call_{}", partial.event_index));
        emitted_ids.insert(id.clone());
        Some(AdapterStreamEvent::ToolCallEnd {
            index: partial.event_index,
            tool_call: ToolCall {
                id,
                name: partial.name.clone().unwrap_or_default(),
                arguments: parse_tool_arguments(&partial.arguments),
            },
        })
    }

    for data in sse_data(body) {
        if data == "[DONE]" {
            break;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        match event["type"].as_str() {
            Some("response.output_text.delta") => {
                if let Some(text) = event["delta"].as_str().filter(|text| !text.is_empty()) {
                    events.push(AdapterStreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(text) = event["delta"].as_str().filter(|text| !text.is_empty()) {
                    events.push(AdapterStreamEvent::ReasoningSummaryDelta {
                        text: text.to_string(),
                    });
                }
            }
            Some("response.output_item.added")
                if event["item"]["type"].as_str() == Some("function_call") =>
            {
                let output_index = event["output_index"]
                    .as_u64()
                    .unwrap_or(partials.len() as u64);
                let partial = ensure_partial(
                    &mut partials,
                    output_index,
                    Some(&event["item"]),
                    &mut next_index,
                );
                events.push(AdapterStreamEvent::ToolCallStart {
                    index: partial.event_index,
                    id: partial.call_id.clone(),
                    name: partial.name.clone(),
                });
            }
            Some("response.function_call_arguments.delta") => {
                let partial = ensure_partial(
                    &mut partials,
                    event["output_index"].as_u64().unwrap_or(0),
                    None,
                    &mut next_index,
                );
                if let Some(delta) = event["delta"].as_str().filter(|delta| !delta.is_empty()) {
                    partial.arguments.push_str(delta);
                    events.push(AdapterStreamEvent::ToolCallDelta {
                        index: partial.event_index,
                        arguments_delta: delta.to_string(),
                    });
                }
            }
            Some("response.function_call_arguments.done") => {
                let partial = ensure_partial(
                    &mut partials,
                    event["output_index"].as_u64().unwrap_or(0),
                    None,
                    &mut next_index,
                );
                if let Some(arguments) = event["arguments"].as_str() {
                    partial.arguments = arguments.to_string();
                }
                if let Some(name) = event["name"].as_str() {
                    partial.name = Some(name.to_string());
                }
                if let Some(finished) = finish_partial(partial, &mut emitted_ids) {
                    events.push(finished);
                }
            }
            Some("response.output_item.done")
                if event["item"]["type"].as_str() == Some("function_call") =>
            {
                let partial = ensure_partial(
                    &mut partials,
                    event["output_index"].as_u64().unwrap_or(0),
                    Some(&event["item"]),
                    &mut next_index,
                );
                if let Some(finished) = finish_partial(partial, &mut emitted_ids) {
                    events.push(finished);
                }
            }
            Some("response.completed") => {
                let parsed = parse_response(&event["response"])?;
                let mut indexes: Vec<u64> = partials.keys().copied().collect();
                indexes.sort_unstable();
                for index in indexes {
                    if let Some(finished) =
                        finish_partial(partials.get_mut(&index).unwrap(), &mut emitted_ids)
                    {
                        events.push(finished);
                    }
                }
                for call in parsed.tool_calls.clone().unwrap_or_default() {
                    if !emitted_ids.insert(call.id.clone()) {
                        continue;
                    }
                    let index = next_index;
                    next_index += 1;
                    events.push(AdapterStreamEvent::ToolCallStart {
                        index,
                        id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                    });
                    events.push(AdapterStreamEvent::ToolCallEnd {
                        index,
                        tool_call: call,
                    });
                }
                if parsed.input_tokens.is_some()
                    || parsed.output_tokens.is_some()
                    || parsed.cached_input_tokens.is_some()
                    || parsed.reasoning_tokens.is_some()
                {
                    events.push(AdapterStreamEvent::Usage {
                        input_tokens: parsed.input_tokens,
                        output_tokens: parsed.output_tokens,
                        cached_input_tokens: parsed.cached_input_tokens,
                        reasoning_tokens: parsed.reasoning_tokens,
                    });
                }
                events.push(AdapterStreamEvent::Finish {
                    finish_reason: parsed.finish_reason,
                    reasoning: parsed.reasoning,
                });
                return Ok(events);
            }
            Some("response.failed") | Some("response.cancelled") => {
                return Err(response_error(&event["response"]));
            }
            Some("error") => {
                return Err(provider_error(
                    event["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| event.to_string()),
                ));
            }
            _ => {}
        }
    }

    let mut indexes: Vec<u64> = partials.keys().copied().collect();
    indexes.sort_unstable();
    for index in indexes {
        if let Some(finished) = finish_partial(partials.get_mut(&index).unwrap(), &mut emitted_ids)
        {
            events.push(finished);
        }
    }
    events.push(AdapterStreamEvent::Finish {
        finish_reason: Some(if partials.is_empty() {
            "stop".into()
        } else {
            "tool_calls".into()
        }),
        reasoning: None,
    });
    Ok(events)
}

fn map_reqwest_error(error: reqwest::Error, timeout: f64) -> PriestError {
    if error.is_timeout() {
        PriestError::ProviderTimeout {
            provider: "openai-responses".into(),
            timeout,
        }
    } else {
        provider_error(error.to_string())
    }
}

fn provider_error(message: impl Into<String>) -> PriestError {
    PriestError::ProviderError {
        provider: "openai-responses".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::reasoning::{ReasoningConfig, ReasoningEffort};
    use crate::schema::tools::{ToolDefinition, ToolExchangeTurn};

    fn config() -> PriestConfig {
        PriestConfig::new("responses", "gpt-test")
    }

    #[test]
    fn builds_reasoning_schema_tools_and_protects_invariants() {
        let mut config = config();
        config.max_output_tokens = Some(200);
        config.reasoning = Some(ReasoningConfig {
            enabled: Some(true),
            effort: Some(ReasoningEffort::Medium),
            summary: Some(ReasoningSummaryMode::Auto),
        });
        config
            .provider_options
            .insert("model".into(), json!("wrong"));
        config.provider_options.insert("stream".into(), json!(true));
        config.provider_options.insert("store".into(), json!(true));
        let output = OutputSpec {
            json_schema: Some(json!({"type": "object"})),
            json_schema_name: "classification".into(),
            json_schema_strict: true,
            ..Default::default()
        };
        let options = AdapterCallOptions {
            tools: vec![ToolDefinition {
                name: "lookup".into(),
                description: "Look up a label.".into(),
                parameters: Some(json!({"type": "object"})),
            }],
            tool_choice: None,
        };
        let body = build_payload(
            &[Message::user("Classify.")],
            &config,
            &output,
            Some(&options),
            false,
        );

        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["stream"], false);
        assert_eq!(body["store"], true);
        assert_eq!(
            body["reasoning"],
            json!({"effort":"medium","summary":"auto"})
        );
        assert_eq!(body["text"]["format"]["name"], "classification");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[test]
    fn replays_reasoning_before_function_call_and_result() {
        let request = ToolExchangeTurn::Assistant {
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: serde_json::from_value(json!({"id":"42"})).unwrap(),
            }],
            reasoning: Some(ReasoningInfo {
                summary: None,
                continuation: Some(vec![OpaqueReasoningState {
                    format: REASONING_FORMAT.into(),
                    value: json!({"type":"reasoning","id":"rs_1","encrypted_content":"opaque"}),
                }]),
            }),
        };
        let ToolExchangeTurn::Assistant {
            text,
            tool_calls,
            reasoning,
        } = request
        else {
            unreachable!()
        };
        let messages = vec![
            Message {
                role: "assistant".into(),
                content: text.unwrap_or_default(),
                tool_calls: Some(tool_calls),
                reasoning,
                ..Default::default()
            },
            Message {
                role: "tool".into(),
                content: "found".into(),
                tool_call_id: Some("call_1".into()),
                ..Default::default()
            },
        ];
        let input = responses_input(&messages);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
    }

    #[test]
    fn parses_safe_reasoning_usage_and_rejects_raw_trace() {
        let result = parse_response(&json!({
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{"type":"summary_text","text":"Checked two options."}],
                    "encrypted_content": "opaque"
                },
                {"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"id\":\"42\"}"}
            ],
            "usage": {
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens":80},
                "output_tokens":25,
                "output_tokens_details":{"reasoning_tokens":20}
            }
        }))
        .unwrap();
        assert_eq!(result.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            result
                .reasoning
                .as_ref()
                .and_then(|value| value.summary.as_deref()),
            Some("Checked two options.")
        );
        assert_eq!(result.reasoning_tokens, Some(20));

        let raw = parse_response(&json!({
            "status":"completed",
            "output":[
                {"type":"reasoning","id":"rs_raw","content":[{"type":"reasoning_text","text":"private"}]},
                {"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{}"}
            ]
        }))
        .unwrap();
        assert!(raw.reasoning.is_none());
    }

    #[test]
    fn parses_crlf_semantic_stream_without_duplicate_tool_end() {
        let body = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Checking\"}\r\n\r\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"\"}}\r\n\r\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"id\\\":\\\"42\\\"}\"}\r\n\r\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"arguments\":\"{\\\"id\\\":\\\"42\\\"}\",\"name\":\"lookup\"}\r\n\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"lookup\",\"arguments\":\"{\\\"id\\\":\\\"42\\\"}\"}],\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\r\n\r\n"
        );
        let events = parse_sse_events(body).unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterStreamEvent::ReasoningSummaryDelta { text } if text == "Checking"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AdapterStreamEvent::ToolCallEnd { .. }))
                .count(),
            1
        );
    }
}
