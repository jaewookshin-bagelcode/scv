//! 실제 aiproxy 모델 카탈로그 종단 테스트 — mock 이 아니라 **진짜 aiproxy**의
//! `GET {root}/api/v1/models/anthropic` 를 조회해 `AnthropicProvider::list_models` 가 실시간
//! 카탈로그를 반환하는지 검증한다(`/models` 표시·`/model` 검증의 데이터 출처).
//!
//! 외부 의존(사내 aiproxy + `CODEB_TOKEN`)이 필요하므로 **기본 `#[ignore]` +
//! `SCV_E2E_AIPROXY` 게이트**다(CODING_RULES §10: 실제 의존 테스트는 옵트인). 파일명 `*_live`
//! 는 결정적 자동 게이트와 구분되는 수동/라이브 검증 컨벤션이다.
//!
//! 실행:
//! ```sh
//! SCV_E2E_AIPROXY=1 cargo test -p scv-providers --test aiproxy_live -- --ignored --nocapture
//! #   다른 base_url: SCV_AIPROXY_BASE=https://.../anthropic ...
//! ```

use scv_core::provider::Provider;
use scv_providers::anthropic::{AnthropicProvider, AuthStyle};

const DEFAULT_BASE: &str = "https://aiproxy-api.backoffice.bagelgames.com/anthropic";

#[tokio::test]
#[ignore = "requires CODEB_TOKEN + aiproxy network; run with SCV_E2E_AIPROXY=1 -- --ignored"]
async fn aiproxy_list_models_returns_live_catalog() {
    if std::env::var("SCV_E2E_AIPROXY").is_err() {
        eprintln!("skip: set SCV_E2E_AIPROXY=1 (and CODEB_TOKEN) to enable");
        return;
    }
    let Ok(token) = std::env::var("CODEB_TOKEN") else {
        eprintln!("skip: CODEB_TOKEN 미설정");
        return;
    };
    let base = std::env::var("SCV_AIPROXY_BASE").unwrap_or_else(|_| DEFAULT_BASE.to_string());
    let provider = AnthropicProvider::new(
        "claude-sonnet-4-6".into(),
        token,
        Some(base),
        AuthStyle::Bearer,
        false,
    );

    let models = provider.list_models().await.expect("list_models ok");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();

    // 라이브 카탈로그는 정적 폴백(sonnet+haiku 2개)보다 넓고, 기본 모델을 포함해야 한다.
    assert!(
        ids.contains(&"claude-sonnet-4-6"),
        "카탈로그에 기본 모델이 없음: {ids:?}"
    );
    assert!(
        ids.len() > 2,
        "정적 폴백(2개)만 돌아옴 — 라이브 조회 실패 의심: {ids:?}"
    );
    // 컨텍스트 윈도가 채워졌는지(파싱 검증).
    assert!(
        models.iter().any(|m| m.context_window > 0),
        "contextWindow 파싱 실패: {models:?}"
    );
}
