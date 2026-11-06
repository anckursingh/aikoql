//! Model endpoint configuration (`[chat]`) and, later, the OpenAI-style client.

use serde::Deserialize;

/// Set once at startup when `[chat]` is present; absent = chat disabled.
pub(crate) static CHAT_CONFIG: std::sync::OnceLock<ChatConfig> = std::sync::OnceLock::new();

fn default_timeout() -> u64 {
    60
}

/// `[chat]` table: an OpenAI-compatible chat-completions endpoint.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatConfig {
    pub url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl ChatConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}
