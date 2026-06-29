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
use scv_core::provider::{CompletionRequest, Provider, ThinkingMode, ToolSchema};
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

fn request_with_tools() -> CompletionRequest {
    CompletionRequest {
        model: "mock-model".into(),
        system: Some("sys".into()),
        // assistant tool_use + tool_result 도 실어 to_wire 의 메시지 분기를 통과시킨다.
        messages: vec![
            Message::user("search please"),
            Message::assistant(vec![scv_core::message::ContentBlock::ToolUse {
                id: "c0".into(),
                name: "grep".into(),
                input: serde_json::json!({ "pattern": "x" }),
            }]),
            Message {
                role: scv_core::message::Role::User,
                content: vec![scv_core::message::ContentBlock::ToolResult {
                    tool_use_id: "c0".into(),
                    content: "match".into(),
                    is_error: false,
                }],
            },
        ],
        tools: vec![ToolSchema {
            name: "grep".into(),
            description: "search".into(),
            input_schema: serde_json::json!({ "type": "object" }),
        }],
        max_tokens: 1000,
        effort: None,
        thinking: ThinkingMode::Disabled,
    }
}

#[tokio::test]
async fn tool_call_stream_decodes_to_tool_use_events() {
    // tool_calls SSE 증분 → ToolUseStart/InputDelta + ToolUse stop. 요청에 tools/도구 메시지가
    // 있어 to_wire 의 tools·assistant tool_calls·role:"tool" 분기까지 실호출로 통과한다.
    let body = "data: {\"model\":\"mock-model\",\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"type\":\"function\",\"function\":{\"name\":\"grep\",\"arguments\":\"\"}}]}}]}\n\n\
                data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"pattern\\\":\\\"y\\\"}\"}}]}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
                data: [DONE]\n\n";
    let port = spawn_mock("HTTP/1.1 200 OK", body);
    let provider = OpenAiProvider::new(
        "openai",
        "mock-model".into(),
        "k".into(),
        Some(format!("http://127.0.0.1:{port}")),
        false,
    );

    let events: Vec<StreamEvent> = provider
        .stream(request_with_tools())
        .await
        .expect("stream opens")
        .map(|e| e.expect("event ok"))
        .collect()
        .await;

    assert!(events.iter().any(|e| matches!(e,
        StreamEvent::ToolUseStart { id, name } if id == "call_9" && name == "grep")));
    assert!(events.iter().any(|e| matches!(e,
        StreamEvent::ToolUseInputDelta { partial_json, .. } if partial_json.contains("pattern"))));
    assert!(
        matches!(events.last(), Some(StreamEvent::MessageStop { stop_reason, .. })
        if *stop_reason == StopReason::ToolUse)
    );
}

#[tokio::test]
async fn compat_provider_streams_text() {
    // compat=true 경로(max_tokens·reasoning_effort 생략)도 실 HTTP 로 한 번 돈다.
    let body = "data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                data: [DONE]\n\n";
    let port = spawn_mock("HTTP/1.1 200 OK", body);
    let provider = OpenAiProvider::new(
        "ollama",
        "m".into(),
        String::new(), // 무인증(Authorization 헤더 생략 경로).
        Some(format!("http://127.0.0.1:{port}")),
        true,
    );
    let text: String = provider
        .stream(request())
        .await
        .expect("stream opens")
        .filter_map(|e| async move {
            match e.ok()? {
                StreamEvent::TextDelta(t) => Some(t),
                _ => None,
            }
        })
        .collect()
        .await;
    assert_eq!(text, "ok");
}

#[tokio::test]
async fn count_tokens_estimates_request_size() {
    // count_tokens 는 로컬 토크나이저 경로(render_for_count + tiktoken) — 공개 Provider 메서드.
    let provider = OpenAiProvider::new("openai", "gpt-5.5".into(), "k".into(), None, false);
    let tools = vec![ToolSchema {
        name: "grep".into(),
        description: "search".into(),
        input_schema: serde_json::json!({ "type": "object" }),
    }];
    let n = provider
        .count_tokens(
            Some("system prompt"),
            &[Message::user("hello world")],
            &tools,
        )
        .await
        .expect("count");
    assert!(n > 0);
}
