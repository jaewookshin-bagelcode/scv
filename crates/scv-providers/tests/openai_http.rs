//! OpenAI 어댑터 HTTP/SSE 통합 테스트(integration 티어).
//!
//! 로컬 mock 서버로 **실제 `reqwest` 전송 → `eventsource` 파싱 → `StreamEvent` 디코드**
//! 전 경로를 검증한다 — 외부 네트워크나 API 키 없이 `OpenAiProvider::stream` 의 실호출
//! 경로(요청 빌드/HTTP/SSE/`drive_stream`)를 종단으로 돌린다. (실제 모델 응답 형상은
//! 수동 테스트로, 여기선 와이어 프로토콜 경로를 본다.)

use std::io::{Read, Write};
use std::net::TcpListener;

use futures::StreamExt;
use scv_core::message::{Message, StopReason, StreamEvent};
use scv_core::provider::{CompletionRequest, Provider, ThinkingMode};
use scv_providers::openai::OpenAiProvider;

/// 한 번의 연결을 받아 고정된 HTTP 응답을 돌려주는 1회용 mock 서버. 포트를 반환한다.
fn spawn_mock(status_line: &'static str, body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf); // 요청은 읽고 버린다.
            let resp = format!(
                "{status_line}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    port
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "mock-model".into(),
        system: Some("sys".into()),
        messages: vec![Message::user("hi")],
        tools: vec![],
        max_tokens: 1000,
        effort: None,
        thinking: ThinkingMode::Disabled,
    }
}

#[tokio::test]
async fn streams_sse_into_normalized_events() {
    let body = "data: {\"model\":\"mock-model\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n\
                data: [DONE]\n\n";
    let port = spawn_mock("HTTP/1.1 200 OK", body);
    let provider = OpenAiProvider::new(
        "openai",
        "mock-model".into(),
        "test-key".into(),
        Some(format!("http://127.0.0.1:{port}")),
        false,
    );

    let events: Vec<StreamEvent> = provider
        .stream(request())
        .await
        .expect("stream opens")
        .map(|e| e.expect("event ok"))
        .collect()
        .await;

    assert!(
        matches!(events.first(), Some(StreamEvent::MessageStart { model }) if model == "mock-model")
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello world");
    match events.last().expect("has events") {
        StreamEvent::MessageStop { stop_reason, usage } => {
            assert_eq!(*stop_reason, StopReason::EndTurn);
            assert_eq!(usage.input_tokens, 5);
            assert_eq!(usage.output_tokens, 2);
        }
        other => panic!("expected MessageStop last, got {other:?}"),
    }
}

#[tokio::test]
async fn http_error_status_is_reported_with_body() {
    let port = spawn_mock(
        "HTTP/1.1 400 Bad Request",
        "{\"error\":{\"message\":\"bad model\"}}",
    );
    let provider = OpenAiProvider::new(
        "openai",
        "x".into(),
        "k".into(),
        Some(format!("http://127.0.0.1:{port}")),
        false,
    );

    let err = provider
        .stream(request())
        .await
        .err()
        .expect("non-2xx should error");
    let msg = format!("{err}");
    assert!(msg.contains("400"), "msg = {msg}");
    assert!(msg.contains("bad model"), "msg = {msg}");
}
