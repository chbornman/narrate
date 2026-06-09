//! Language-model connector boundary (spec/RUNTIME.md §4, normative).

use crate::embedder::DecodedImage;
use crate::error::ConnectorResult;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// system/user/assistant.
    pub messages: Vec<ChatMessage>,
    pub max_tokens: u32,
    pub temperature: f32,
    /// When set, decoding is constrained to this JSON Schema (llama.cpp
    /// `response_format: json_schema`; cloud adapters map to their native
    /// structured-output mechanism). Query parsing requires it.
    pub json_schema: Option<serde_json::Value>,
    /// See RUNTIME §9.
    pub priority: Lane,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    Interactive,
    Background,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub model_id: String,
}

pub trait LanguageModel: Send + Sync {
    async fn complete(&self, req: ChatRequest) -> ConnectorResult<ChatResponse>;
    /// Caption for retrieval fuel only (kernel: never user-facing prose).
    /// Local impl: multimodal chat call via the mmproj projector;
    /// always Lane::Background.
    async fn caption_image(&self, img: &DecodedImage, prompt: &str) -> ConnectorResult<String>;
    fn model_id(&self) -> &str;
}
