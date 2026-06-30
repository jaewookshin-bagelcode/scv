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
use scv_core::{Error, Result};

/// `kind` 문자열로 적절한 프로바이더를 생성한다.
///
/// `api_key` 는 호출자가 환경변수에서 읽어 넘긴다(이 크레이트는 비밀을 직접 읽지 않는다).
/// `auth_style` 은 anthropic kind 에만 의미가 있다(`"bearer"` = aiproxy 게이트웨이 경유,
/// 생략/그 외 = `x-api-key` 직결). 다른 kind 는 무시한다.
/// 라이브러리 경계이므로 `anyhow` 가 아니라 코어의 [`Error`] 를 돌려준다(CODING_RULES §2).
pub fn build(
    kind: &str,
    model: String,
    api_key: String,
    base_url: Option<String>,
    auth_style: Option<&str>,
) -> Result<Arc<dyn Provider>> {
    match kind {
        "anthropic" => Ok(Arc::new(anthropic::AnthropicProvider::new(
            model,
            api_key,
            base_url,
            anthropic::AuthStyle::from_config(auth_style),
        ))),
        // openai: 표준 OpenAI. openai-compat: OpenAI-호환 백엔드(OpenRouter·Gemini 등)용
        // 와이어 호환 모드. ollama: 같은 어댑터를 재사용하되 로컬 기본 base_url 을 주고
        // id 로 자신을 드러낸다(별도 어댑터가 아니라 OpenAI-호환 어댑터 재사용).
        "openai" => Ok(Arc::new(openai::OpenAiProvider::new(
            "openai", model, api_key, base_url, false,
        ))),
        "openai-compat" => Ok(Arc::new(openai::OpenAiProvider::new(
            "openai-compat",
            model,
            api_key,
            base_url,
            true,
        ))),
        "ollama" => Ok(Arc::new(openai::OpenAiProvider::new(
            "ollama",
            model,
            api_key,
            base_url.or_else(|| Some("http://localhost:11434/v1".to_string())),
            true,
        ))),
        other => Err(Error::Provider(format!("unknown provider kind: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_each_known_kind() {
        for kind in ["anthropic", "openai", "openai-compat", "ollama"] {
            let p = build(kind, "m".into(), "k".into(), None, None).expect("known kind builds");
            assert!(!p.id().is_empty());
            assert!(!p.models().is_empty());
        }
        // openai 계열은 id 로 자신의 kind 를 드러낸다.
        assert_eq!(
            build("openai-compat", "m".into(), "k".into(), None, None)
                .unwrap()
                .id(),
            "openai-compat"
        );
    }

    #[test]
    fn anthropic_builds_with_bearer_auth_style() {
        // auth_style="bearer" → aiproxy 게이트웨이 경유 모드로 anthropic 어댑터 생성.
        let p = build(
            "anthropic",
            "claude-sonnet-4-6".into(),
            "aiproxy_xxx".into(),
            Some("https://aiproxy-api.example.com/anthropic".into()),
            Some("bearer"),
        )
        .expect("anthropic builds with bearer");
        assert_eq!(p.id(), "anthropic");
    }

    #[test]
    fn ollama_builds_without_key_or_base_url() {
        // base_url·키 생략 → ollama 는 로컬 기본값을 채워 out-of-box 동작. auth_style 은 무시.
        let p = build("ollama", "qwen".into(), String::new(), None, None).expect("ollama builds");
        assert_eq!(p.id(), "ollama");
    }

    #[test]
    fn unknown_kind_is_error() {
        match build("nope", "m".into(), "k".into(), None, None) {
            Err(Error::Provider(msg)) => assert!(msg.contains("unknown provider kind")),
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("expected error for unknown kind"),
        }
    }
}
