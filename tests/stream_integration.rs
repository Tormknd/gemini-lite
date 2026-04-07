use gemini_lite::api::{self, Content, Part, UiEvent};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user_msg(text: &str) -> Content {
    Content {
        role: "user".to_string(),
        parts: vec![Part {
            text: text.to_string(),
        }],
    }
}

fn sse_body(events: &[&str]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!("data: {event}\n\n"));
    }
    body
}

fn gemini_chunk(text: &str) -> String {
    format!(r#"{{"candidates":[{{"content":{{"parts":[{{"text":"{text}"}}]}}}}]}}"#)
}

fn gemini_chunk_with_tokens(text: &str, tokens: u32) -> String {
    format!(
        r#"{{"candidates":[{{"content":{{"parts":[{{"text":"{text}"}}]}}}}],"usageMetadata":{{"totalTokenCount":{tokens}}}}}"#
    )
}

#[tokio::test]
async fn stream_complete_response() {
    let server = MockServer::start().await;

    let body = sse_body(&[
        &gemini_chunk("Hello "),
        &gemini_chunk_with_tokens("world!", 42),
        "[DONE]",
    ]);

    Mock::given(method("POST"))
        .and(path_regex(r"/.*:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let (tx, rx) = async_channel::unbounded::<UiEvent>();
    let history = vec![user_msg("hi")];

    let result = api::stream_gemini(
        &client,
        &server.uri(),
        "fake-key",
        "gemini-test",
        &history,
        &tx,
    )
    .await;

    let (text, tokens) = result.expect("stream should succeed");
    assert_eq!(text, "Hello world!");
    assert_eq!(tokens, 42);

    let mut deltas = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let UiEvent::Delta(d) = event {
            deltas.push(d);
        }
    }
    assert!(!deltas.is_empty(), "should have received Delta events");
}

#[tokio::test]
async fn stream_handles_api_error_429() {
    let server = MockServer::start().await;

    let error_body = r#"{"error":{"message":"Rate limit exceeded","code":429}}"#;

    Mock::given(method("POST"))
        .and(path_regex(r"/.*:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(429).set_body_string(error_body))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let (tx, _rx) = async_channel::unbounded::<UiEvent>();
    let history = vec![user_msg("hi")];

    let result = api::stream_gemini(
        &client,
        &server.uri(),
        "fake-key",
        "gemini-test",
        &history,
        &tx,
    )
    .await;

    let err = result.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("429") || msg.contains("Rate limit"),
        "error should mention 429: {msg}"
    );
}

#[tokio::test]
async fn stream_handles_api_error_500() {
    let server = MockServer::start().await;

    let error_body = r#"{"error":{"message":"Internal server error","code":500}}"#;

    Mock::given(method("POST"))
        .and(path_regex(r"/.*:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(500).set_body_string(error_body))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let (tx, _rx) = async_channel::unbounded::<UiEvent>();
    let history = vec![user_msg("hi")];

    let result = api::stream_gemini(
        &client,
        &server.uri(),
        "fake-key",
        "gemini-test",
        &history,
        &tx,
    )
    .await;

    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(msg.contains("500"), "error should mention 500: {msg}");
}

#[tokio::test]
async fn stream_empty_response_is_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/.*:streamGenerateContent"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let (tx, _rx) = async_channel::unbounded::<UiEvent>();
    let history = vec![user_msg("hi")];

    let result = api::stream_gemini(
        &client,
        &server.uri(),
        "fake-key",
        "gemini-test",
        &history,
        &tx,
    )
    .await;

    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("without model text"),
        "should report empty stream: {msg}"
    );
}

#[tokio::test]
async fn stream_token_count_from_last_event() {
    let server = MockServer::start().await;

    let body = sse_body(&[
        &gemini_chunk_with_tokens("first", 10),
        &gemini_chunk_with_tokens("second", 25),
    ]);

    Mock::given(method("POST"))
        .and(path_regex(r"/.*:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let (tx, _rx) = async_channel::unbounded::<UiEvent>();
    let history = vec![user_msg("hi")];

    let (_, tokens) = api::stream_gemini(
        &client,
        &server.uri(),
        "fake-key",
        "gemini-test",
        &history,
        &tx,
    )
    .await
    .expect("stream should succeed");

    assert_eq!(tokens, 25, "should take the last token count");
}
