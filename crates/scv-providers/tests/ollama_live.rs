//! 실제 로컬 모델(Ollama) 종단 테스트 — fake/mock 이 아니라 **진짜 모델**로 OpenAI-호환
//! (compat) 경로를 검증한다. 기본 모델 `qwen3.5:9b` 같은 로컬 모델로 `OpenAiProvider`(compat)
//! 가 실제 토큰을 스트리밍하는지 본다.
//!
//! 외부 의존(실행 중인 Ollama)이 필요하므로 **기본 `#[ignore]` + `SCV_E2E_OLLAMA` 게이트**다
//! (CODING_RULES §10: 실제 의존 테스트는 옵트인). 파일명 `*_live` 는 결정적 자동 게이트
//! (fake provider e2e·mock SSE 통합)와 구분되는 **수동/라이브 검증** 컨벤션이다.
//!
//! 실행:
//! ```sh
//! ollama serve && ollama pull qwen3.5:9b   # 기본 모델
//! SCV_E2E_OLLAMA=1 cargo test -p scv-providers --test ollama_live -- --ignored --nocapture
//! #   다른 모델: SCV_OLLAMA_MODEL=<model> ...
//! ```

use futures::StreamExt;
use scv_core::message::{Message, StreamEvent};
use scv_core::provider::{CompletionRequest, Provider, ThinkingMode};
use scv_providers::openai::OpenAiProvider;

fn request(model: String) -> CompletionRequest {
    CompletionRequest {
        model,
        system: Some("You are concise.".into()),
        messages: vec![Message::user("Reply with exactly one word: hello")],
        tools: vec![],
        max_tokens: 1024,
        effort: None,
        thinking: ThinkingMode::Disabled,
    }
}

#[tokio::test]
#[ignore = "requires a running local Ollama; run with SCV_E2E_OLLAMA=1 -- --ignored"]
async fn ollama_streams_real_completion() {
    if std::env::var("SCV_E2E_OLLAMA").is_err() {
        eprintln!("skip: set SCV_E2E_OLLAMA=1 and run a local Ollama (ollama serve) to enable");
        return;
    }
    let model = std::env::var("SCV_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3.5:9b".to_string());
    // compat=true: Ollama 는 max_tokens + no stream_options/reasoning_effort 를 기대한다.
    let provider = OpenAiProvider::new(
        "ollama",
        model.clone(),
        "ollama".to_string(), // Ollama 는 키를 무시
        Some("http://localhost:11434/v1".to_string()),
        true,
    );

    let events: Vec<StreamEvent> = provider
        .stream(request(model))
        .await
        .expect("stream opens — is `ollama serve` running and the model pulled?")
        .map(|e| e.expect("event ok"))
        .collect()
        .await;

    // 실제 모델 출력은 비결정적이라 **형태만** 검증한다: 텍스트나 추론(thinking)이 흐르고
    // 정상 종료하는지. (gemma 등 reasoning 모델은 답을 reasoning 으로 흘릴 수 있다.)
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta(t) | StreamEvent::ThinkingDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !streamed.trim().is_empty(),
        "expected streamed text or reasoning; events = {events:?}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::MessageStop { .. })),
        "expected MessageStop as the final event; events = {events:?}"
    );
}
