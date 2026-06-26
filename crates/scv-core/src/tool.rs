//! 도구(tool) 추상과 권한 모델.
//!
//! 도구는 모델이 호출할 수 있는 "행동"이다(파일 읽기/쓰기, bash 실행, 검색 등).
//! 코어는 [`Tool`] trait 과 [`ToolRegistry`] 만 정의하고, 내장 도구 구현은
//! `scv-tools` 크레이트가 제공한다.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
/// 협조적 취소 토큰. 다른 크레이트가 `tokio-util` 에 직접 의존하지 않도록 코어가 재노출한다.
pub use tokio_util::sync::CancellationToken;

use crate::provider::ToolSchema;

/// 도구가 요구하는 권한 수준.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionLevel {
    /// 자동 허용(읽기 전용·부작용 없음).
    Allow,
    /// 매번 사용자에게 묻는다(파일 수정, bash 등 되돌리기 어려운 동작).
    Ask,
    /// 항상 거부.
    Deny,
}

/// 되돌리기 어려운 동작(`Ask`)에 대해 최종 허용 여부를 확정하는 게이트.
/// (`scv-tools` 의 정적 정책 + TUI 의 대화형 프롬프트가 구현한다.)
///
/// 계약(루프 측): 도구가 `Ask` 를 요구할 때만 이 게이트를 부르고, **`Allow` 일 때만
/// 실행**한다 — 그 외(`Ask`/`Deny`)는 거부(fail-closed). 따라서 사용자 동의를 받을 수
/// 없는 비대화형 게이트는 `Allow` 를 돌려줘선 안 된다. `Ask` 를 그대로 돌려주면
/// "승인 못 받음"으로 간주되어 거부된다.
#[async_trait]
pub trait PermissionGate: Send + Sync {
    /// `tool` 이 `input` 으로 호출되려 할 때 최종 허용 여부를 결정한다.
    async fn decide(&self, tool: &str, input: &serde_json::Value) -> PermissionLevel;
}

/// 도구 실행에 필요한 주변 정보(작업 디렉터리, 취소 신호 등).
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// 도구가 파일을 읽고 쓸 루트(여기 밖으로 나가지 못하게 강제할 것).
    ///
    /// 세션 간 파일시스템 격리의 **유일한 경계**다 — scv 는 세션별 파일 샌드박스를
    /// 만들지 않으므로, 같은 workdir 의 두 세션은 같은 파일을 공유해 충돌할 수 있다.
    /// 같은 repo 다중 세션을 격리하려면 세션마다 다른 workdir(예: per-session git
    /// worktree)를 주입한다(ARCHITECTURE.md §4.2 세션 격리).
    pub workdir: std::path::PathBuf,
    /// 취소 신호(사용자가 Esc/Ctrl-C 로 턴을 중단할 때). 긴 도구는 주기적으로
    /// `cancel.is_cancelled()` 를 확인하거나 await 루프를 `cancel.cancelled()` 와
    /// `tokio::select!` 한다(협조적 취소 — ARCHITECTURE §2·§4.5).
    pub cancel: CancellationToken,
}

/// 도구 실행 결과.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// 모델에게 돌려줄 텍스트(tool_result 의 content 가 된다).
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// 모델이 호출할 수 있는 도구.
///
/// 설계 원칙(아키텍처 문서 §도구 참고):
/// - 부작용이 없는 동작은 [`PermissionLevel::Allow`] 로 두어 병렬 실행을 허용한다.
/// - 되돌리기 어려운 동작(파일 수정, 외부 호출)은 [`PermissionLevel::Ask`] 로 게이팅한다.
#[async_trait]
pub trait Tool: Send + Sync {
    /// 모델에 노출되는 고유 이름(예: "read", "bash").
    fn name(&self) -> &str;

    /// 모델이 "언제 쓸지" 판단하는 근거가 되는 설명. 구체적으로 쓴다.
    fn description(&self) -> &str;

    /// 입력 JSON Schema.
    fn input_schema(&self) -> serde_json::Value;

    /// 이 입력에 대해 요구되는 권한 수준. 입력에 따라 달라질 수 있다
    /// (예: workdir 밖 경로 쓰기는 Deny).
    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Ask
    }

    /// 병렬 실행 가능 여부(읽기 전용 도구는 true). 루프가 스케줄링에 사용.
    fn parallel_safe(&self) -> bool {
        false
    }

    /// 도구를 실행한다.
    async fn invoke(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolOutput;

    /// 프로바이더에 보낼 스키마로 변환(기본 구현 제공).
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// 이름 → 도구 매핑. 에이전트 루프가 도구를 찾고 스키마를 모으는 진입점.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// 등록된 모든 도구의 스키마(프로바이더 요청에 실어 보낼 용도).
    /// 정렬된 BTreeMap 을 쓰므로 순서가 결정적 → 프롬프트 캐시 친화적.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }
        fn description(&self) -> &str {
            "dummy tool"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            ToolOutput::ok("done")
        }
    }

    #[test]
    fn registry_register_get_and_schemas() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DummyTool));
        assert!(reg.get("dummy").is_some());
        assert!(reg.get("nope").is_none());
        let schemas = reg.schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "dummy");
    }

    #[test]
    fn output_helpers_and_default_permission() {
        assert!(!ToolOutput::ok("x").is_error);
        assert!(ToolOutput::error("x").is_error);
        // 기본 권한은 Ask, 기본은 병렬 불가.
        assert_eq!(
            DummyTool.permission(&serde_json::json!({})),
            PermissionLevel::Ask
        );
        assert!(!DummyTool.parallel_safe());
    }

    #[tokio::test]
    async fn invoke_runs_with_context() {
        let ctx = ToolContext {
            workdir: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        };
        let out = DummyTool.invoke(serde_json::json!({}), &ctx).await;
        assert!(!out.is_error);
        assert_eq!(out.content, "done");
    }
}
