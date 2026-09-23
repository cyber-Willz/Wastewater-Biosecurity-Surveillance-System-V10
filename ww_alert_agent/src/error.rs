use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Failed to spawn bridge subprocess: {0}")]
    BridgeSpawn(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Anthropic API returned an error: status={status} body={body}")]
    ApiResponse { status: u16, body: String },

    #[error("API response contained no usable content block")]
    EmptyResponse,

    #[error("Tool-use block missing 'decisions' key")]
    MalformedToolInput,

    #[error("Alert review failed: {0}")]
    ReviewFailed(String),
}
