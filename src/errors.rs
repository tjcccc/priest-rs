use thiserror::Error;

#[derive(Debug, Error)]
pub enum PriestError {
    #[error("profile not found: {profile}")]
    ProfileNotFound { profile: String },

    #[error("profile invalid: {profile} — {reason}")]
    ProfileInvalid { profile: String, reason: String },

    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: String },

    #[error("session store error: {message}")]
    SessionStoreError { message: String },

    #[error("provider not registered: {provider}")]
    ProviderNotRegistered { provider: String },

    #[error("provider timeout: {provider} after {timeout}s")]
    ProviderTimeout { provider: String, timeout: f64 },

    #[error("provider error: {provider} — {message}")]
    ProviderError { provider: String, message: String },

    #[error("provider rate limited: {provider}")]
    ProviderRateLimited { provider: String, retry_after: Option<f64> },

    #[error("request invalid: {message}")]
    RequestInvalid { message: String },

    #[error("internal error: {message}")]
    InternalError { message: String },
}

impl PriestError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProfileNotFound { .. }    => "PROFILE_NOT_FOUND",
            Self::ProfileInvalid { .. }     => "PROFILE_INVALID",
            Self::SessionNotFound { .. }    => "SESSION_NOT_FOUND",
            Self::SessionStoreError { .. }  => "SESSION_STORE_ERROR",
            Self::ProviderNotRegistered { .. } => "PROVIDER_NOT_REGISTERED",
            Self::ProviderTimeout { .. }    => "PROVIDER_TIMEOUT",
            Self::ProviderError { .. }      => "PROVIDER_ERROR",
            Self::ProviderRateLimited { .. } => "PROVIDER_RATE_LIMITED",
            Self::RequestInvalid { .. }     => "REQUEST_INVALID",
            Self::InternalError { .. }      => "INTERNAL_ERROR",
        }
    }

    pub fn details(&self) -> std::collections::HashMap<String, String> {
        let mut m = std::collections::HashMap::new();
        match self {
            Self::ProfileNotFound { profile } | Self::ProfileInvalid { profile, .. } => {
                m.insert("profile".into(), profile.clone());
            }
            Self::SessionNotFound { session_id } => {
                m.insert("session_id".into(), session_id.clone());
            }
            Self::ProviderNotRegistered { provider }
            | Self::ProviderTimeout { provider, .. }
            | Self::ProviderError { provider, .. }
            | Self::ProviderRateLimited { provider, .. } => {
                m.insert("provider".into(), provider.clone());
                if let Self::ProviderTimeout { timeout, .. } = self {
                    m.insert("timeout".into(), timeout.to_string());
                }
                if let Self::ProviderRateLimited { retry_after: Some(ra), .. } = self {
                    m.insert("retry_after".into(), ra.to_string());
                }
            }
            _ => {}
        }
        m
    }
}
