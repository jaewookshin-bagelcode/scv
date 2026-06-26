//! core 계층의 에러 타입.
//!
//! 규칙(코딩 규칙 문서 §에러 처리 참고):
//! - 라이브러리 경계에서는 `thiserror` 로 의미 있는 enum 을 노출한다.
//! - 바이너리(`scv-cli`) 최상단에서만 `anyhow` 로 흡수한다.

use thiserror::Error;

/// core 전반에서 쓰는 결과 타입 별칭.
pub type Result<T> = std::result::Result<T, Error>;

/// 에이전트 코어에서 발생할 수 있는 오류.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// LLM 프로바이더 호출 실패(네트워크/HTTP/응답 파싱 등).
    #[error("provider error: {0}")]
    Provider(String),

    /// 도구 실행 실패.
    #[error("tool `{name}` failed: {source}")]
    Tool {
        name: String,
        #[source]
        source: anyhow::Error,
    },

    /// 사용자가 권한 요청을 거부함.
    #[error("permission denied for tool `{0}`")]
    PermissionDenied(String),

    /// 스킬을 찾지 못하거나 로드 실패.
    #[error("skill error: {0}")]
    Skill(String),

    /// 세션 영속화(읽기/쓰기) 실패.
    #[error("session store error: {0}")]
    SessionStore(#[source] std::io::Error),

    /// 한 턴의 도구 호출 반복 상한 초과(무한 루프 방지).
    #[error("exceeded max tool iterations ({0})")]
    MaxIterations(usize),

    /// 사용자가 턴을 중단함(Ctrl-C 등). 크래시가 아니라 정상적인 협조적 취소 —
    /// 모은 부분 결과는 세션에 보존된다(ARCHITECTURE §2).
    #[error("turn cancelled")]
    Cancelled,

    /// 직렬화/역직렬화 오류.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// 입출력 오류(터미널 raw mode·렌더 등 라이브러리 경계의 IO). `context` 로 어디서
    /// 났는지 짚는다 — TUI 런타임(`scv-tui`)이 crossterm/ratatui IO 를 이 변형으로 감싼다.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}
