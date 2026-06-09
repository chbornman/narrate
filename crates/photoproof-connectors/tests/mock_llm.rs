//! The scripted mock LanguageModel: FIFO responses, scripted errors (the
//! RUNTIME §13.2 retry-once shape), recorded requests, deterministic
//! fallback, and captions (background-lane retrieval fuel).

use photoproof_connectors::mock::MockLanguageModel;
use photoproof_connectors::{
    ChatMessage, ChatRequest, ConnectorError, DecodedImage, Lane, LanguageModel, Role,
};
use pollster::block_on;

fn request(content: &str) -> ChatRequest {
    ChatRequest {
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "translate a photographer's search".to_owned(),
            },
            ChatMessage {
                role: Role::User,
                content: content.to_owned(),
            },
        ],
        max_tokens: 256,
        temperature: 0.0,
        json_schema: None,
        priority: Lane::Interactive,
    }
}

#[test]
fn scripted_responses_consume_in_fifo_order() {
    let llm = MockLanguageModel::new("mock-llm");
    llm.push_response("{\"filters\":[],\"semantic\":null,\"visual\":false}");
    llm.push_response("second");

    let first = block_on(llm.complete(request("q1"))).unwrap();
    let second = block_on(llm.complete(request("q2"))).unwrap();
    assert_eq!(
        first.text,
        "{\"filters\":[],\"semantic\":null,\"visual\":false}"
    );
    assert_eq!(second.text, "second");
    assert_eq!(first.model_id, "mock-llm");
    // Token counts are derived deterministically.
    assert_eq!(first.prompt_tokens, 5);
    assert_eq!(second.completion_tokens, 1);
}

#[test]
fn scripted_error_then_success_supports_retry_once_tests() {
    // RUNTIME §13.2: mid-call crash → ConnectorError::ConnectionLost, the
    // request retries exactly once after Ready and succeeds.
    let llm = MockLanguageModel::new("mock-llm");
    llm.push_error(ConnectorError::ConnectionLost(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "killed mid-call",
    )));
    llm.push_response("recovered");

    let first = block_on(llm.complete(request("parse this")));
    assert!(matches!(first, Err(ConnectorError::ConnectionLost(_))));
    let retry = block_on(llm.complete(request("parse this"))).unwrap();
    assert_eq!(retry.text, "recovered");
}

#[test]
fn requests_are_recorded_for_assertions() {
    let llm = MockLanguageModel::new("mock-llm");
    llm.push_response("ok");
    let mut req = request("melancholic series last winter");
    req.json_schema = Some(serde_json::json!({ "type": "object" }));
    req.priority = Lane::Background;
    block_on(llm.complete(req.clone())).unwrap();

    let seen = llm.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0], req);
}

#[test]
fn unscripted_complete_falls_back_to_deterministic_echo() {
    let llm = MockLanguageModel::new("mock-llm");
    let a = block_on(llm.complete(request("fog barn"))).unwrap();
    let b = block_on(llm.complete(request("fog barn"))).unwrap();
    assert_eq!(a.text, "echo: fog barn");
    assert_eq!(a, b);
}

#[test]
fn captions_are_scripted_recorded_and_deterministic_by_default() {
    let llm = MockLanguageModel::new("mock-llm");
    let img = DecodedImage {
        rgb8: vec![0; 4 * 3 * 3],
        width: 4,
        height: 3,
    };

    llm.push_caption("a barn in fog");
    let scripted = block_on(llm.caption_image(&img, "describe for retrieval")).unwrap();
    assert_eq!(scripted, "a barn in fog");

    let default = block_on(llm.caption_image(&img, "describe for retrieval")).unwrap();
    assert_eq!(default, "mock caption of 4x3 image");

    llm.push_caption_error(ConnectorError::Cancelled);
    let cancelled = block_on(llm.caption_image(&img, "describe for retrieval"));
    assert!(matches!(cancelled, Err(ConnectorError::Cancelled)));

    let calls = llm.caption_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].prompt, "describe for retrieval");
    assert_eq!((calls[0].width, calls[0].height), (4, 3));
}
