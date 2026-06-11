//! The OpenAI-compatible `LanguageModel` client against the scripted stub
//! HTTP server (spec/RUNTIME.md §3.1/§4.4; packet P6.2). Real loopback
//! sockets — the honest OS boundary — with scripted responses and bounded
//! timeouts; no unbounded waits anywhere.

use std::time::Duration;

use photoproof_connectors::llm::{ChatMessage, ChatRequest, Lane, LanguageModel, Role};
use photoproof_connectors::mock::{StubHttpServer, StubResponse};
use photoproof_connectors::openai::{HealthStatus, OpenAiCompatClient};
use photoproof_connectors::{ConnectorError, DecodedImage};

fn req(text: &str, lane: Lane) -> ChatRequest {
    ChatRequest {
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "you parse queries".into(),
            },
            ChatMessage {
                role: Role::User,
                content: text.into(),
            },
        ],
        max_tokens: 128,
        temperature: 0.0,
        json_schema: None,
        priority: lane,
    }
}

fn client_for(server: &StubHttpServer) -> OpenAiCompatClient {
    OpenAiCompatClient::external(&server.base_url(), "gemma-4-e4b-it-q4_k_m", None)
        .expect("client")
        .with_timeouts(Duration::from_millis(500), Duration::from_millis(2_000))
}

const COMPLETION: &str = r#"{
    "choices": [{ "message": { "role": "assistant", "content": "parsed" } }],
    "usage": { "prompt_tokens": 12, "completion_tokens": 3 },
    "model": "gemma-4-e4b-it-q4_k_m"
}"#;

#[test]
fn non_streaming_completion_round_trips_and_carries_the_request_shape() {
    let server = StubHttpServer::start();
    server.route("/v1/chat/completions", StubResponse::ok_json(COMPLETION));
    let client = client_for(&server);

    let resp = pollster::block_on(client.complete(req("find the harbor shots", Lane::Interactive)))
        .expect("completion");
    assert_eq!(resp.text, "parsed");
    assert_eq!(resp.prompt_tokens, 12);
    assert_eq!(resp.completion_tokens, 3);
    assert_eq!(resp.model_id, "gemma-4-e4b-it-q4_k_m");
    assert_eq!(client.gauge().count(), 0, "gauge released after the call");

    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["model"], "gemma-4-e4b-it-q4_k_m");
    assert_eq!(body["max_tokens"], 128);
    assert_eq!(body["stream"], false, "interactive lane: non-streaming");
    assert_eq!(body["messages"][1]["content"], "find the harbor shots");
    assert!(
        body.get("response_format").is_none(),
        "no schema → no response_format"
    );
}

#[test]
fn json_schema_passes_through_verbatim() {
    let server = StubHttpServer::start();
    server.route("/v1/chat/completions", StubResponse::ok_json(COMPLETION));
    let client = client_for(&server);
    let schema = serde_json::json!({
        "name": "filter_ast",
        "schema": { "type": "object", "properties": { "rating": { "type": "integer" } } }
    });
    let mut r = req("rated 4 and up", Lane::Interactive);
    r.json_schema = Some(schema.clone());
    pollster::block_on(client.complete(r)).expect("completion");

    let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"], schema,
        "schema forwarded verbatim (RUNTIME §4)"
    );
}

#[test]
fn background_lane_streams_and_accumulates_sse_deltas() {
    let server = StubHttpServer::start();
    server.route(
        "/v1/chat/completions",
        StubResponse::Sse {
            events: vec![
                r#"{"choices":[{"delta":{"content":"a quiet "}}]}"#.into(),
                r#"{"choices":[{"delta":{"content":"harbor"}}]}"#.into(),
                r#"{"choices":[{"delta":{}}],"usage":{"prompt_tokens":40,"completion_tokens":2}}"#
                    .into(),
                "[DONE]".into(),
            ],
            cut_after_events: None,
        },
    );
    let client = client_for(&server);
    let resp = pollster::block_on(client.complete(req("caption", Lane::Background)))
        .expect("streamed completion");
    assert_eq!(resp.text, "a quiet harbor");
    assert_eq!(resp.prompt_tokens, 40);
    assert_eq!(resp.completion_tokens, 2);

    let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(
        body["stream"], true,
        "background lane MUST stream (RUNTIME §9 honesty clause)"
    );
}

#[test]
fn mid_stream_cut_is_connection_lost_and_reports_to_the_supervisor_seam() {
    let server = StubHttpServer::start();
    server.route(
        "/v1/chat/completions",
        StubResponse::Sse {
            events: vec![
                r#"{"choices":[{"delta":{"content":"half a "}}]}"#.into(),
                r#"{"choices":[{"delta":{"content":"thought"}}]}"#.into(),
            ],
            cut_after_events: Some(1),
        },
    );
    let client = client_for(&server);
    let lost = client.lost_reports();
    let err = pollster::block_on(client.complete(req("x", Lane::Background)))
        .expect_err("cut stream must error");
    assert!(
        matches!(err, ConnectorError::ConnectionLost(_)),
        "got {err:?}"
    );
    assert_eq!(
        lost.drain(),
        1,
        "the supervisor's immediate-check seam saw the loss (§8.1)"
    );
    assert_eq!(client.gauge().count(), 0);
}

#[test]
fn backend_errors_and_refused_connections_map_to_the_connector_vocabulary() {
    let server = StubHttpServer::start();
    server.route("/v1/chat/completions", StubResponse::status(503));
    let client = client_for(&server);
    let err = pollster::block_on(client.complete(req("x", Lane::Interactive))).unwrap_err();
    assert!(matches!(err, ConnectorError::Backend { status: 503, .. }));

    // Nothing listening: NotReady, the caller treats the feature as
    // unavailable (never a retry-loop).
    let dead = OpenAiCompatClient::external("http://127.0.0.1:1", "m", None)
        .unwrap()
        .with_timeouts(Duration::from_millis(300), Duration::from_millis(300));
    let err = pollster::block_on(dead.complete(req("x", Lane::Interactive))).unwrap_err();
    assert!(matches!(err, ConnectorError::NotReady(_)), "got {err:?}");
}

#[test]
fn health_maps_200_503_timeout_and_refused() {
    let server = StubHttpServer::start();
    let route = server.route("/health", StubResponse::status(503));
    StubHttpServer::push(&route, StubResponse::status(200));
    StubHttpServer::push(&route, StubResponse::Hang);
    let client = client_for(&server);
    assert_eq!(
        client.health(Duration::from_millis(500)),
        HealthStatus::Loading,
        "llama returns 503 while loading (§8.1)"
    );
    assert_eq!(
        client.health(Duration::from_millis(500)),
        HealthStatus::Ready
    );
    assert_eq!(
        client.health(Duration::from_millis(200)),
        HealthStatus::TimedOut,
        "a hanging /health is a timeout, never a panic"
    );
    let dead = OpenAiCompatClient::external("http://127.0.0.1:1", "m", None).unwrap();
    assert_eq!(
        dead.health(Duration::from_millis(200)),
        HealthStatus::Refused
    );
}

#[test]
fn caption_rides_the_background_lane_with_an_image_content_part() {
    let server = StubHttpServer::start();
    server.route(
        "/v1/chat/completions",
        StubResponse::Sse {
            events: vec![
                r#"{"choices":[{"delta":{"content":"red kayak at dusk"}}]}"#.into(),
                "[DONE]".into(),
            ],
            cut_after_events: None,
        },
    );
    let client = client_for(&server);
    let img = DecodedImage {
        rgb8: vec![200, 30, 30, 10, 10, 40],
        width: 2,
        height: 1,
    };
    let caption =
        pollster::block_on(client.caption_image(&img, "describe for retrieval")).expect("caption");
    assert_eq!(caption, "red kayak at dusk");

    let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(body["stream"], true, "caption = background = streaming");
    let parts = body["messages"][0]["content"].as_array().expect("parts");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "describe for retrieval");
    assert_eq!(parts[1]["type"], "image_url");
    let url = parts[1]["image_url"]["url"].as_str().unwrap();
    assert!(
        url.starts_with("data:image/png;base64,"),
        "image as a data URL content part"
    );
}

#[test]
fn disconnect_cancels_a_streaming_request_from_another_thread() {
    let server = StubHttpServer::start();
    server.route(
        "/v1/chat/completions",
        StubResponse::SseHang {
            events: vec![r#"{"choices":[{"delta":{"content":"stuck "}}]}"#.into()],
        },
    );
    let client = client_for(&server);
    let (tx, rx) = std::sync::mpsc::channel();
    let result = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            client.complete_transport(
                &req("never finishes", Lane::Background),
                true,
                &mut |disconnect| {
                    let _ = tx.send(disconnect);
                },
            )
        });
        let disconnect = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("disconnect handle delivered at open");
        // The §9 cancellation primitive: sever the socket mid-generation.
        std::thread::sleep(Duration::from_millis(100));
        disconnect.disconnect();
        handle.join().expect("worker join")
    });
    let err = result.expect_err("severed stream must not succeed");
    assert!(
        matches!(
            err,
            ConnectorError::ConnectionLost(_) | ConnectorError::Timeout(_)
        ),
        "got {err:?}"
    );
    assert_eq!(client.gauge().count(), 0, "gauge released after cancel");
}

#[test]
fn bearer_key_is_sent_when_resolved_and_absent_otherwise() {
    let server = StubHttpServer::start();
    server.route("/v1/chat/completions", StubResponse::ok_json(COMPLETION));
    let with_key =
        OpenAiCompatClient::external(&server.base_url(), "m", Some("resolved-secret".into()))
            .unwrap()
            .with_timeouts(Duration::from_millis(500), Duration::from_millis(1_000));
    pollster::block_on(with_key.complete(req("x", Lane::Interactive))).unwrap();
    assert_eq!(
        server.requests()[0].header("Authorization"),
        Some("Bearer resolved-secret")
    );

    let without = client_for(&server);
    pollster::block_on(without.complete(req("x", Lane::Interactive))).unwrap();
    assert_eq!(server.requests()[1].header("Authorization"), None);
}
