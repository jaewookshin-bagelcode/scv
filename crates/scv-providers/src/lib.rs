//! LLM 프로바이더 어댑터 구현.
//!
//! 각 어댑터는 `scv_core::provider::Provider` 를 구현하고, 프로바이더 중립 타입
//! ([`scv_core::message`])을 자기 와이어 포맷으로 양방향 변환한다.
//!
//! - [`anthropic`] Anthropic Messages API (raw HTTP/SSE — 공식 Rust SDK 없음)
//! - [`openai`]    OpenAI Chat Completions API
//!
//! 설정의 `kind` 값으로 어떤 어댑터를 만들지 [`build`] 가 분기한다.

#![warn(rust_2018_idioms, unreachable_pub)]

pub mod anthropic;
pub mod openai;

use std::sync::Arc;

use scv_core::provider::Provider;

/// `kind` 문자열로 적절한 프로바이더를 생성한다.
///
/// `api_key` 는 호출자가 환경변수에서 읽어 넘긴다(이 크레이트는 비밀을 직접 읽지 않는다).
pub fn build(
    kind: &str,
    model: String,
    api_key: String,
    base_url: Option<String>,
) -> anyhow::Result<Arc<dyn Provider>> {
    match kind {
        "anthropic" => Ok(Arc::new(anthropic::AnthropicProvider::new(model, api_key, base_url))),
        "openai" => Ok(Arc::new(openai::OpenAiProvider::new(model, api_key, base_url))),
        other => anyhow::bail!("unknown provider kind: {other}"),
    }
}
