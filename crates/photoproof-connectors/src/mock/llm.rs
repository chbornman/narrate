//! Scripted mock `LanguageModel`.
//!
//! Responses and captions are FIFO queues of scripted texts or errors; an
//! empty queue falls back to a deterministic echo so unscripted calls
//! still behave reproducibly. Every call is recorded for assertions.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::embedder::DecodedImage;
use crate::error::{ConnectorError, ConnectorResult};
use crate::llm::{ChatRequest, ChatResponse, LanguageModel, Role};

enum Scripted<T> {
    Ok(T),
    Err(ConnectorError),
}

/// One recorded `caption_image` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionCall {
    pub prompt: String,
    pub width: u32,
    pub height: u32,
}

pub struct MockLanguageModel {
    model_id: String,
    responses: Mutex<VecDeque<Scripted<String>>>,
    captions: Mutex<VecDeque<Scripted<String>>>,
    seen_requests: Mutex<Vec<ChatRequest>>,
    seen_captions: Mutex<Vec<CaptionCall>>,
}

impl MockLanguageModel {
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            responses: Mutex::new(VecDeque::new()),
            captions: Mutex::new(VecDeque::new()),
            seen_requests: Mutex::new(Vec::new()),
            seen_captions: Mutex::new(Vec::new()),
        }
    }

    /// Queue the next `complete` response text (token counts derived
    /// deterministically at call time).
    pub fn push_response(&self, text: impl Into<String>) {
        self.responses
            .lock()
            .unwrap()
            .push_back(Scripted::Ok(text.into()));
    }

    /// Queue the next `complete` outcome as an error (e.g. a scripted
    /// `ConnectionLost` for the retry-exactly-once supervision test).
    pub fn push_error(&self, error: ConnectorError) {
        self.responses
            .lock()
            .unwrap()
            .push_back(Scripted::Err(error));
    }

    /// Queue the next `caption_image` text.
    pub fn push_caption(&self, text: impl Into<String>) {
        self.captions
            .lock()
            .unwrap()
            .push_back(Scripted::Ok(text.into()));
    }

    pub fn push_caption_error(&self, error: ConnectorError) {
        self.captions
            .lock()
            .unwrap()
            .push_back(Scripted::Err(error));
    }

    /// Every `ChatRequest` received so far, in call order.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.seen_requests.lock().unwrap().clone()
    }

    /// Every `caption_image` call received so far, in call order.
    pub fn caption_calls(&self) -> Vec<CaptionCall> {
        self.seen_captions.lock().unwrap().clone()
    }
}

impl LanguageModel for MockLanguageModel {
    async fn complete(&self, req: ChatRequest) -> ConnectorResult<ChatResponse> {
        let prompt_tokens: u32 = req
            .messages
            .iter()
            .map(|m| m.content.split_whitespace().count() as u32)
            .sum();
        self.seen_requests.lock().unwrap().push(req.clone());
        let text = match self.responses.lock().unwrap().pop_front() {
            Some(Scripted::Ok(text)) => text,
            Some(Scripted::Err(error)) => return Err(error),
            None => {
                // Deterministic fallback: echo the last user message.
                let last_user = req
                    .messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, Role::User))
                    .map(|m| m.content.as_str())
                    .unwrap_or("");
                format!("echo: {last_user}")
            }
        };
        Ok(ChatResponse {
            prompt_tokens,
            completion_tokens: text.split_whitespace().count() as u32,
            text,
            model_id: self.model_id.clone(),
        })
    }

    async fn caption_image(&self, img: &DecodedImage, prompt: &str) -> ConnectorResult<String> {
        self.seen_captions.lock().unwrap().push(CaptionCall {
            prompt: prompt.to_owned(),
            width: img.width,
            height: img.height,
        });
        match self.captions.lock().unwrap().pop_front() {
            Some(Scripted::Ok(text)) => Ok(text),
            Some(Scripted::Err(error)) => Err(error),
            None => Ok(format!(
                "mock caption of {}x{} image",
                img.width, img.height
            )),
        }
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}
