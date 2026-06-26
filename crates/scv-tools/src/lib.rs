//! 내장 도구 모음과 권한 정책.
//!
//! 모든 도구는 `scv_core::tool::Tool` 을 구현한다. 설계 가이드:
//! - 읽기 전용 도구(`read`/`glob`/`grep`)는 `Allow` + `parallel_safe = true`.
//! - 파일 수정·`bash` 같은 비가역 동작은 `Ask` 로 게이팅한다.
//! - **모든 경로 입력은 `ctx.workdir` 안으로 제한**한다(경로 탈출 방지).

#![warn(rust_2018_idioms, unreachable_pub)]

mod bash;
mod edit;
mod glob;
mod grep;
mod path;
mod read;
mod write;
// TODO(계획): mod web_fetch;       // HTTP GET. egress → 권한 Ask/도메인 allowlist, parallel_safe.
//             mod transcript_search; // 세션 JSONL·파일 정확 일치 검색(정밀 추출). reqwest 의존 추가.

use std::sync::Arc;

use async_trait::async_trait;
use scv_core::tool::{PermissionGate, PermissionLevel, ToolRegistry};

pub use bash::BashTool;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

/// 내장 도구를 모두 등록한 레지스트리를 만든다.
pub fn default_registry() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    // 읽기 전용(Allow + parallel_safe).
    reg.register(Arc::new(ReadTool));
    reg.register(Arc::new(GlobTool));
    reg.register(Arc::new(GrepTool));
    // 비가역(Ask 게이팅 — fail-closed: 대화형 모달/명시적 Allow 없이는 거부됨).
    reg.register(Arc::new(WriteTool));
    reg.register(Arc::new(EditTool));
    reg.register(Arc::new(BashTool));
    // 계획 도구:
    // reg.register(Arc::new(WebFetchTool));        // HTTP GET (egress 권한 게이팅)
    // reg.register(Arc::new(TranscriptSearchTool)); // 세션/파일 정밀 추출
    reg
}

/// 설정 기반 정적 권한 정책(비대화형). 대화형 동의가 필요하면 TUI 게이트와 합성한다.
///
/// 비대화형이라 사용자에게 물을 수 없다 → 루프는 `Allow` 만 실행하므로(fail-closed),
/// `Ask` 도구를 TUI 없이 허용하려면 해당 도구에 명시적 `Allow` 오버라이드를 준다
/// (`with_override`). 기본값 `Ask` 로 둔 도구는 모달이 붙기 전까지 거부된다.
#[derive(Debug, Clone)]
pub struct StaticPermissionGate {
    pub default: PermissionLevel,
    pub overrides: std::collections::BTreeMap<String, PermissionLevel>,
}

impl StaticPermissionGate {
    pub fn new(default: PermissionLevel) -> Self {
        Self {
            default,
            overrides: std::collections::BTreeMap::new(),
        }
    }

    pub fn with_override(mut self, tool: impl Into<String>, level: PermissionLevel) -> Self {
        self.overrides.insert(tool.into(), level);
        self
    }
}

#[async_trait]
impl PermissionGate for StaticPermissionGate {
    async fn decide(&self, tool: &str, _input: &serde_json::Value) -> PermissionLevel {
        self.overrides.get(tool).copied().unwrap_or(self.default)
    }
}
