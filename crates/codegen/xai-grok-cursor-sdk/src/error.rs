use thiserror::Error;

/// Errors from discovering, spawning, or talking to the Cursor SDK Bridge.
#[derive(Debug, Error)]
pub enum CursorSdkError {
    #[error("{0}")]
    Message(String),
    #[error("cursor-sdk-bridge not found: {0}")]
    BridgeNotFound(String),
    #[error("bridge handshake failed: {0}")]
    Handshake(String),
    #[error("connect rpc {method} failed ({code}): {message}")]
    Rpc {
        method: String,
        code: String,
        message: String,
        request_id: Option<String>,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Prost(#[from] prost::DecodeError),
}

impl CursorSdkError {
    pub fn message(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }

    pub fn missing_api_key() -> Self {
        Self::Message(
            "Cursor API key not set. Run `grok login-cursor` or export CURSOR_API_KEY \
             (https://cursor.com/dashboard/api)."
                .into(),
        )
    }
}
