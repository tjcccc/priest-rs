use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::BoxStream;
use futures::StreamExt;

use crate::context_builder::build_messages;
use crate::errors::PriestError;
use crate::profile::loader::ProfileLoader;
use crate::providers::adapter::ProviderAdapter;
use crate::schema::request::PriestRequest;
use crate::schema::response::{ExecutionInfo, PriestErrorModel, PriestResponse, SessionInfo, UsageInfo};
use crate::session::model::Session;
use crate::session::store::SessionStore;

pub const SPEC_VERSION: &str = "2.2.0";

pub struct PriestEngine {
    adapters: HashMap<String, Box<dyn ProviderAdapter>>,
    profile_loader: Arc<dyn ProfileLoader>,
    session_store: Option<Arc<dyn SessionStore>>,
}

impl PriestEngine {
    pub fn new(profile_loader: Arc<dyn ProfileLoader>) -> Self {
        Self { adapters: HashMap::new(), profile_loader, session_store: None }
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
        let adapter = self.adapters.get(&request.config.provider)
            .ok_or_else(|| PriestError::ProviderNotRegistered { provider: request.config.provider.clone() })?;

        let profile = self.profile_loader.load(&request.profile)?;
        let (session, is_new) = self.resolve_session(&request).await?;

        let messages = build_messages(&request, &profile, session.as_ref());

        let start = Instant::now();
        let result = adapter.complete(&messages, &request.config, &request.output).await;
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
                let mut resp = PriestResponse {
                    text: Some(adapter_result.text.clone()),
                    execution: ExecutionInfo {
                        finished_reason: adapter_result.finish_reason.clone(),
                        ..execution
                    },
                    usage: Some(UsageInfo::new(adapter_result.input_tokens, adapter_result.output_tokens)),
                    session: None,
                    error: None,
                    metadata: request.metadata.clone(),
                };

                if let (Some(mut sess), Some(store)) = (session, &self.session_store) {
                    sess.append_turn("user", &request.prompt);
                    sess.append_turn("assistant", &adapter_result.text);
                    store.save(&sess).await?;
                    resp.session = Some(SessionInfo {
                        id: sess.id.clone(),
                        is_new,
                        turn_count: sess.turns.len(),
                    });
                }

                Ok(resp)
            }
            Err(e) => {
                Ok(PriestResponse {
                    text: None,
                    execution: ExecutionInfo {
                        finished_reason: Some("error".into()),
                        ..execution
                    },
                    usage: None,
                    session: None,
                    error: Some(PriestErrorModel::from_priest_error(&e)),
                    metadata: request.metadata.clone(),
                })
            }
        }
    }

    pub async fn stream(
        &self,
        request: PriestRequest,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        let adapter = self.adapters.get(&request.config.provider)
            .ok_or_else(|| PriestError::ProviderNotRegistered { provider: request.config.provider.clone() })?;

        let profile = self.profile_loader.load(&request.profile)?;
        let (session, _is_new) = self.resolve_session(&request).await?;

        let messages = build_messages(&request, &profile, session.as_ref());

        let chunk_stream = adapter.stream(&messages, &request.config, &request.output).await?;

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
                let sess = store.create(&request.profile, Some(&session_ref.id)).await?;
                return Ok((Some(sess), true));
            }
            return Err(PriestError::SessionNotFound { session_id: session_ref.id.clone() });
        }

        let sess = store.create(&request.profile, None).await?;
        Ok((Some(sess), true))
    }
}
