use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use crate::context_builder::Message;
use crate::errors::PriestError;
use crate::schema::config::PriestConfig;
use crate::schema::request::OutputSpec;
use super::adapter::{AdapterResult, ProviderAdapter};

pub struct OpenAICompatProvider {
    base_url: String,
    api_key: String,
    client: Client,
}

impl OpenAICompatProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), api_key: api_key.into(), client: Client::new() }
    }
}

#[derive(Deserialize)]
struct OAIResponse {
    choices: Vec<OAIChoice>,
    usage: Option<OAIUsage>,
}

#[derive(Deserialize)]
struct OAIChoice {
    message: OAIMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OAIMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct OAIUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

fn map_finish(r: Option<&str>) -> Option<String> {
    Some(match r? {
        "stop"           => "stop",
        "length"         => "length",
        "content_filter" => "unknown",
        _                => "unknown",
    }.to_string())
}

fn build_payload(messages: &[Message], config: &PriestConfig, output_spec: &OutputSpec, stream: bool) -> Value {
    let msgs: Vec<Value> = messages.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
    let mut payload = json!({ "model": config.model, "messages": msgs });
    if stream { payload["stream"] = json!(true); }
    if let Some(max_t) = config.max_output_tokens { payload["max_tokens"] = json!(max_t); }
    if let Some(ref schema) = output_spec.json_schema {
        payload["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name":   output_spec.json_schema_name,
                "schema": schema,
                "strict": output_spec.json_schema_strict,
            },
        });
    } else if output_spec.provider_format.as_deref() == Some("json") {
        payload["response_format"] = json!({"type": "json_object"});
    }
    for (k, v) in &config.provider_options {
        payload[k] = v.clone();
    }
    payload
}

fn provider_error(config: &PriestConfig, msg: impl Into<String>) -> PriestError {
    PriestError::ProviderError { provider: config.provider.clone(), message: msg.into() }
}

#[async_trait]
impl ProviderAdapter for OpenAICompatProvider {
    async fn complete(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<AdapterResult, PriestError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let payload = build_payload(messages, config, output_spec, false);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);

        let resp = self.client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    PriestError::ProviderTimeout { provider: config.provider.clone(), timeout: config.timeout_seconds }
                } else {
                    provider_error(config, e.to_string())
                }
            })?;

        if resp.status() == 429 {
            let retry = resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            return Err(PriestError::ProviderRateLimited { provider: config.provider.clone(), retry_after: retry });
        }
        if !resp.status().is_success() {
            return Err(provider_error(config, format!("HTTP {}", resp.status())));
        }

        let data: OAIResponse = resp.json().await.map_err(|e| provider_error(config, e.to_string()))?;
        let choice = data.choices.into_iter().next()
            .ok_or_else(|| provider_error(config, "empty choices"))?;
        Ok(AdapterResult {
            text: choice.message.content.unwrap_or_default(),
            finish_reason: map_finish(choice.finish_reason.as_deref()),
            input_tokens: data.usage.as_ref().and_then(|u| u.prompt_tokens),
            output_tokens: data.usage.as_ref().and_then(|u| u.completion_tokens),
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        config: &PriestConfig,
        output_spec: &OutputSpec,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let payload = build_payload(messages, config, output_spec, true);
        let timeout = Duration::from_secs_f64(config.timeout_seconds);
        let provider = config.provider.clone();

        let resp = self.client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    PriestError::ProviderTimeout { provider: provider.clone(), timeout: config.timeout_seconds }
                } else {
                    PriestError::ProviderError { provider: provider.clone(), message: e.to_string() }
                }
            })?;

        if !resp.status().is_success() {
            return Err(PriestError::ProviderError { provider, message: format!("HTTP {}", resp.status()) });
        }

        let lines = resp.bytes_stream();
        let provider2 = provider.clone();

        let stream = lines.flat_map(move |chunk: Result<Bytes, reqwest::Error>| {
            let prov = provider2.clone();
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => return futures::stream::iter(vec![Err(
                    PriestError::ProviderError { provider: prov, message: e.to_string() }
                )]),
            };
            let text = String::from_utf8_lossy(&bytes).to_string();
            let mut items = vec![];
            for line in text.lines() {
                let line = line.trim();
                if line == "data: [DONE]" { break; }
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        if let Some(content) = v["choices"][0]["delta"]["content"].as_str() {
                            if !content.is_empty() {
                                items.push(Ok(content.to_string()));
                            }
                        }
                    }
                }
            }
            futures::stream::iter(items)
        });

        Ok(Box::pin(stream))
    }
}
