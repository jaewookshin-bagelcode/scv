//! Agentic loop — 한 사용자 턴을 끝까지 구동하는 엔진.
//!
//! 흐름:
//! ```text
//!  user 입력
//!     │
//!     ▼
//!  [컨텍스트 준비] ─► [Provider.stream] ─► (스트림 이벤트를 Observer 로 흘리며 assistant 메시지로 집계)
//!     ▲                                          │
//!     │                              stop_reason == tool_use ?
//!     │                                ├── no ──► 턴 종료
//!     │                                └── yes ─► [권한 게이트] ─► [도구 실행(병렬 가능)] ─┐
//!     └──────────────────  tool_result 들을 user 메시지로 push  ◄────────────────────────┘
//! ```
//!
//! 이 엔진은 [`Provider`]/[`Tool`]/[`PermissionGate`] 의 **구체 타입을 모른다** —
//! 전부 trait object 로 받는다. 그래서 어떤 프로바이더·도구 조합과도 동작한다.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;

use crate::context::ContextManager;
use crate::message::{ContentBlock, Message, Role, StopReason, StreamEvent};
use crate::provider::{CompletionRequest, Effort, Provider, ThinkingMode};
use crate::session::Session;
use crate::tool::{PermissionGate, PermissionLevel, ToolContext, ToolRegistry};
use crate::{Error, Result};

/// 스트림 이벤트를 받아 화면 출력 등에 쓰는 관찰자. TUI 가 구현한다.
#[async_trait]
pub trait Observer: Send + Sync {
    async fn on_event(&self, event: &StreamEvent);
}

/// 아무것도 하지 않는 관찰자(비대화형/테스트용).
#[derive(Debug, Default)]
pub struct NullObserver;

#[async_trait]
impl Observer for NullObserver {
    async fn on_event(&self, _event: &StreamEvent) {}
}

/// 에이전트 루프 실행에 필요한 구성요소 묶음.
pub struct Agent {
    pub provider: Arc<dyn Provider>,
    pub tools: ToolRegistry,
    pub permissions: Arc<dyn PermissionGate>,
    pub context: Arc<dyn ContextManager>,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub max_tool_iterations: usize,
    pub tool_ctx: ToolContext,
}

// trait object 필드(Provider/PermissionGate/ContextManager)는 Debug 가 아니므로
// 수동으로 스칼라 필드만 출력한다.
impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("effort", &self.effort)
            .field("max_tool_iterations", &self.max_tool_iterations)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// 사용자 입력 한 건을 처리하고, 턴이 끝나면 세션을 갱신한다.
    ///
    /// `observer` 로 스트림 이벤트가 실시간 전달된다(TUI 출력).
    pub async fn run_turn(
        &self,
        session: &mut Session,
        user_input: String,
        observer: &dyn Observer,
    ) -> Result<()> {
        session.push(Message::user(user_input));

        for iteration in 0..self.max_tool_iterations {
            // 1. 컨텍스트 준비(필요 시 compaction).
            let messages = self.context.prepare(session.messages.clone()).await?;

            // 2. 요청 구성 → 스트리밍 호출.
            let request = CompletionRequest {
                model: self.model.clone(),
                system: Some(self.system_prompt.clone()),
                messages,
                tools: self.tools.schemas(),
                max_tokens: self.max_tokens,
                effort: self.effort,
                thinking: ThinkingMode::Adaptive,
            };
            let mut stream = self.provider.stream(request).await?;

            // 3. 스트림을 집계해 assistant 메시지 한 통을 만든다.
            let mut assembler = MessageAssembler::default();
            let mut stop_reason = StopReason::EndTurn;
            while let Some(event) = stream.next().await {
                let event = event?;
                observer.on_event(&event).await;
                if let StreamEvent::MessageStop { stop_reason: sr, .. } = &event {
                    stop_reason = *sr;
                }
                assembler.apply(event);
            }
            let assistant = Message::assistant(assembler.finish());
            session.push(assistant.clone());

            // 4. 더 이상 도구 호출이 없으면 턴 종료.
            if stop_reason != StopReason::ToolUse {
                return Ok(());
            }

            // 5. 도구 호출 처리 → tool_result 들을 하나의 user 메시지로 모은다.
            let results = self.execute_tool_calls(&assistant).await?;
            session.push(Message { role: Role::User, content: results });
            let _ = iteration; // 반복 카운터(로깅에 사용 가능)
        }

        Err(Error::MaxIterations(self.max_tool_iterations))
    }

    /// assistant 메시지의 모든 tool_use 블록을 실행해 tool_result 블록들을 만든다.
    async fn execute_tool_calls(&self, assistant: &Message) -> Result<Vec<ContentBlock>> {
        let mut results = Vec::new();

        // NOTE: parallel_safe 도구들은 join_all 로 병렬 실행하도록 확장할 수 있다.
        //       여기서는 골격만 — 순차 실행으로 둔다.
        for (id, name, input) in assistant.tool_uses() {
            let Some(tool) = self.tools.get(name) else {
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: format!("unknown tool: {name}"),
                    is_error: true,
                });
                continue;
            };

            // 권한 게이트: 정적 정책과 사용자 응답을 종합한다.
            let level = match tool.permission(input) {
                PermissionLevel::Deny => PermissionLevel::Deny,
                _ => self.permissions.decide(name, input).await,
            };
            if level == PermissionLevel::Deny {
                return Err(Error::PermissionDenied(name.to_string()));
            }

            let output = tool.invoke(input.clone(), &self.tool_ctx).await;
            results.push(ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: output.content,
                is_error: output.is_error,
            });
        }

        Ok(results)
    }
}

/// 스트림 이벤트를 받아 콘텐츠 블록 리스트로 누적한다.
#[derive(Debug, Default)]
struct MessageAssembler {
    blocks: Vec<ContentBlock>,
    text_buf: String,
    // tool_use 누적용(id → (name, json buffer)) 등은 실제 구현에서 채운다.
}

impl MessageAssembler {
    fn apply(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::TextDelta(t) => self.text_buf.push_str(&t),
            StreamEvent::ContentBlockStop => self.flush_text(),
            // TODO: ToolUseStart/ToolUseInputDelta 를 모아 ContentBlock::ToolUse 로 합친다.
            _ => {}
        }
    }

    fn flush_text(&mut self) {
        if !self.text_buf.is_empty() {
            self.blocks.push(ContentBlock::text(std::mem::take(&mut self.text_buf)));
        }
    }

    fn finish(mut self) -> Vec<ContentBlock> {
        self.flush_text();
        self.blocks
    }
}
