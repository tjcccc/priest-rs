use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::BoxStream;
use futures::StreamExt;

use crate::context_builder::build_messages;
use crate::errors::PriestError;
use crate::profile::loader::ProfileLoader;
use crate::providers::adapter::{AdapterCallOptions, ProviderAdapter};
use crate::schema::request::PriestRequest;
use crate::schema::tools::ToolCall;
use crate::schema::response::{
    ExecutionInfo, PriestErrorModel, PriestResponse, SessionInfo, UsageInfo,
};
use crate::session::model::Session;
use crate::session::store::SessionStore;

pub const SPEC_VERSION: &str = "2.4.0";

/// Engine-level structured streaming event (spec 2.4.0). The terminal event
/// is always `Done` carrying the full `PriestResponse`.
///
/// Note: the Rust providers do not yet surface native tool-call deltas while
/// streaming, so tool-call variants are reserved; use `run()` /
/// `run_with_tools()` for tool calling.
#[derive(Debug, Clone)]
pub enum PriestStreamEvent {
    TextDelta { text: String },
    ToolCallStart { index: usize, id: Option<String>, name: Option<String> },
    ToolCallDelta { index: usize, arguments_delta: String },
    ToolCallEnd { index: usize, tool_call: ToolCall },
    Usage { input_tokens: Option<u32>, output_tokens: Option<u32> },
    Done { response: PriestResponse },
}

fn call_options(request: &PriestRequest) -> Option<AdapterCallOptions> {
    if request.tools.is_empty() {
        None
    } else {
        Some(AdapterCallOptions {
            tools: request.tools.clone(),
            tool_choice: request.tool_choice.clone(),
        })
    }
}

pub struct PriestEngine {
    adapters: HashMap<String, Box<dyn ProviderAdapter>>,
    profile_loader: Arc<dyn ProfileLoader>,
    session_store: Option<Arc<dyn SessionStore>>,
}

impl PriestEngine {
    pub fn new(profile_loader: Arc<dyn ProfileLoader>) -> Self {
        Self {
            adapters: HashMap::new(),
            profile_loader,
            session_store: None,
        }
    }

    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    pub fn register(mut self, name: impl Into<String>, adapter: Box<dyn ProviderAdapter>) -> Self {
        self.adapters.insert(name.into(), adapter);
        self
    }

    pub async fn run(&self, request: PriestRequest) -> Result<PriestResponse, PriestError> {
        let adapter = self.adapters.get(&request.config.provider).ok_or_else(|| {
            PriestError::ProviderNotRegistered {
                provider: request.config.provider.clone(),
            }
        })?;

        let profile = self.profile_loader.load(&request.profile)?;
        let (session, is_new) = self.resolve_session(&request).await?;

        let messages = build_messages(&request, &profile, session.as_ref());

        let start = Instant::now();
        let options = call_options(&request);
        let result = adapter
            .complete(&messages, &request.config, &request.output, options.as_ref())
            .await;
        let latency_ms = start.elapsed().as_millis() as i64;

        let execution = ExecutionInfo {
            provider: request.config.provider.clone(),
            model: request.config.model.clone(),
            profile: request.profile.clone(),
            latency_ms: Some(latency_ms),
            finished_reason: None,
        };

        match result {
            Ok(adapter_result) => {
                let tool_calls = adapter_result
                    .tool_calls
                    .clone()
                    .filter(|calls| !calls.is_empty());
                let finished_reason = if tool_calls.is_some() {
                    Some("tool_calls".to_string())
                } else {
                    adapter_result.finish_reason.clone()
                };
                let mut resp = PriestResponse {
                    text: Some(adapter_result.text.clone()),
                    tool_calls: tool_calls.clone(),
                    execution: ExecutionInfo {
                        finished_reason,
                        ..execution
                    },
                    usage: Some(UsageInfo::new(
                        adapter_result.input_tokens,
                        adapter_result.output_tokens,
                    )),
                    session: None,
                    error: None,
                    metadata: request.metadata.clone(),
                };

                if let (Some(mut sess), Some(store)) = (session, &self.session_store) {
                    // Tool-call iterations are turn-local: persist only when the
                    // model produced a final answer (spec behavior/tool-calling.md).
                    if tool_calls.is_none() {
                        sess.append_turn("user", &request.prompt);
                        sess.append_turn("assistant", &adapter_result.text);
                        store.save(&sess).await?;
                    }
                    resp.session = Some(SessionInfo {
                        id: sess.id.clone(),
                        is_new,
                        turn_count: sess.turns.len(),
                    });
                }

                Ok(resp)
            }
            Err(e) => Ok(PriestResponse {
                text: None,
                tool_calls: None,
                execution: ExecutionInfo {
                    finished_reason: Some("error".into()),
                    ..execution
                },
                usage: None,
                session: None,
                error: Some(PriestErrorModel::from_priest_error(&e)),
                metadata: request.metadata.clone(),
            }),
        }
    }

    pub async fn stream(
        &self,
        request: PriestRequest,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let adapter = self.adapters.get(&request.config.provider).ok_or_else(|| {
            PriestError::ProviderNotRegistered {
                provider: request.config.provider.clone(),
            }
        })?;

        let profile = self.profile_loader.load(&request.profile)?;
        let (session, _is_new) = self.resolve_session(&request).await?;

        let messages = build_messages(&request, &profile, session.as_ref());

        let chunk_stream = adapter
            .stream(&messages, &request.config, &request.output, call_options(&request).as_ref())
            .await?;

        let store = self.session_store.clone();
        let prompt = request.prompt.clone();

        let stream = chunk_stream.collect::<Vec<_>>().await;
        let mut chunks = vec![];
        for item in stream {
            match item {
                Ok(chunk) => chunks.push(chunk),
                Err(e) => return Ok(Box::pin(futures::stream::once(async move { Err(e) }))),
            }
        }

        let full_text = chunks.join("");
        if let (Some(sess), Some(st)) = (session, store) {
            let mut s = sess.clone();
            s.append_turn("user", &prompt);
            s.append_turn("assistant", &full_text);
            let _ = st.save(&s).await;
        }

        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }

    /// Yield structured streaming events (spec 2.4.0): text deltas followed by
    /// a terminal `Done` event carrying the full `PriestResponse`. Like the
    /// existing `stream()`, the underlying provider stream is currently
    /// collected before emission. Provider errors surface in
    /// `Done.response.error` rather than as stream errors, matching `run()`.
    pub async fn stream_events(
        &self,
        request: PriestRequest,
    ) -> Result<BoxStream<'static, PriestStreamEvent>, PriestError> {
        let start = Instant::now();
        let adapter = self.adapters.get(&request.config.provider).ok_or_else(|| {
            PriestError::ProviderNotRegistered {
                provider: request.config.provider.clone(),
            }
        })?;

        let profile = self.profile_loader.load(&request.profile)?;
        let (session, is_new) = self.resolve_session(&request).await?;
        let messages = build_messages(&request, &profile, session.as_ref());

        let mut chunks: Vec<String> = vec![];
        let mut error: Option<PriestError> = None;
        match adapter
            .stream(&messages, &request.config, &request.output, call_options(&request).as_ref())
            .await
        {
            Ok(chunk_stream) => {
                let collected = chunk_stream.collect::<Vec<_>>().await;
                for item in collected {
                    match item {
                        Ok(chunk) => chunks.push(chunk),
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                }
            }
            Err(e) => error = Some(e),
        }

        let text = if chunks.is_empty() { None } else { Some(chunks.join("")) };
        let mut session_info = None;
        if let (Some(sess), Some(store)) = (session, &self.session_store) {
            if error.is_none() {
                if let Some(ref full_text) = text {
                    let mut s = sess.clone();
                    s.append_turn("user", &request.prompt);
                    s.append_turn("assistant", full_text);
                    let _ = store.save(&s).await;
                    session_info = Some(SessionInfo {
                        id: s.id.clone(),
                        is_new,
                        turn_count: s.turns.len(),
                    });
                } else {
                    session_info = Some(SessionInfo {
                        id: sess.id.clone(),
                        is_new,
                        turn_count: sess.turns.len(),
                    });
                }
            }
        }

        let response = PriestResponse {
            text,
            tool_calls: None,
            execution: ExecutionInfo {
                provider: request.config.provider.clone(),
                model: request.config.model.clone(),
                profile: request.profile.clone(),
                latency_ms: Some(start.elapsed().as_millis() as i64),
                finished_reason: Some(if error.is_some() { "error".into() } else { "stop".to_string() }),
            },
            usage: None,
            session: session_info,
            error: error.as_ref().map(PriestErrorModel::from_priest_error),
            metadata: request.metadata.clone(),
        };

        let events: Vec<PriestStreamEvent> = chunks
            .into_iter()
            .map(|text| PriestStreamEvent::TextDelta { text })
            .chain(std::iter::once(PriestStreamEvent::Done { response }))
            .collect();
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn resolve_session(
        &self,
        request: &PriestRequest,
    ) -> Result<(Option<Session>, bool), PriestError> {
        let (Some(session_ref), Some(store)) = (&request.session, &self.session_store) else {
            return Ok((None, false));
        };

        if session_ref.continue_existing {
            let existing = store.get(&session_ref.id).await?;
            if let Some(sess) = existing {
                return Ok((Some(sess), false));
            }
            if session_ref.create_if_missing {
                let sess = store
                    .create(&request.profile, Some(&session_ref.id))
                    .await?;
                return Ok((Some(sess), true));
            }
            return Err(PriestError::SessionNotFound {
                session_id: session_ref.id.clone(),
            });
        }

        let sess = store.create(&request.profile, None).await?;
        Ok((Some(sess), true))
    }
}
