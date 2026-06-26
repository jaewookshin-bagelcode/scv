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
use crate::message::{AgentEvent, ContentBlock, Message, Role, StopReason, StreamEvent};
use crate::provider::{CompletionRequest, Effort, Provider, ThinkingMode};
use crate::session::Session;
use crate::tool::{PermissionGate, PermissionLevel, ToolContext, ToolRegistry};
use crate::{Error, Result};

/// 루프의 라이프사이클 통지([`AgentEvent`])를 받아 화면 출력 등에 쓰는 관찰자.
/// **관찰 전용**(`()` 반환 → 되먹임 불가, ARCHITECTURE §4.5). TUI 가 구현한다.
#[async_trait]
pub trait Observer: Send + Sync {
    async fn on_event(&self, event: &AgentEvent);
}

/// 아무것도 하지 않는 관찰자(비대화형/테스트용).
#[derive(Debug, Default)]
pub struct NullObserver;

#[async_trait]
impl Observer for NullObserver {
    async fn on_event(&self, _event: &AgentEvent) {}
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
        let cancel = &self.tool_ctx.cancel;

        for iteration in 0..self.max_tool_iterations {
            // 협조적 취소 ①: 이터레이션 진입부 체크포인트.
            if cancel.is_cancelled() {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

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

            // 3. 스트림을 집계해 assistant 메시지 한 통을 만든다. 협조적 취소 ②:
            //    스트림 소비를 취소 신호와 select! 로 경쟁시켜 Ctrl-C 시 즉시 멈춘다.
            let mut assembler = MessageAssembler::default();
            let mut stop_reason = StopReason::EndTurn;
            let interrupted = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break true,
                    next = stream.next() => match next {
                        None => break false,
                        Some(event) => {
                            let event = event?;
                            if let StreamEvent::MessageStop { stop_reason: sr, .. } = &event {
                                stop_reason = *sr;
                            }
                            observer.on_event(&AgentEvent::Stream(event.clone())).await;
                            assembler.apply(event);
                        }
                    }
                }
            };

            // 중단 시에도 모은 부분 텍스트를 세션에 보존한다(빈 메시지는 넣지 않음).
            let assistant = Message::assistant(assembler.finish());
            if !assistant.content.is_empty() {
                session.push(assistant.clone());
            }
            if interrupted {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

            // 4. 더 이상 도구 호출이 없으면 턴 종료.
            if stop_reason != StopReason::ToolUse {
                return Ok(());
            }

            // 협조적 취소 ③: 도구 실행 직전 체크포인트.
            if cancel.is_cancelled() {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

            // 5. 도구 호출 처리 → tool_result 들을 하나의 user 메시지로 모은다.
            tracing::debug!(iteration, "executing tool calls");
            let results = self.execute_tool_calls(&assistant, observer).await?;
            session.push(Message {
                role: Role::User,
                content: results,
            });
        }

        Err(Error::MaxIterations(self.max_tool_iterations))
    }

    /// assistant 메시지의 모든 tool_use 블록을 실행해 tool_result 블록들을 만든다.
    async fn execute_tool_calls(
        &self,
        assistant: &Message,
        observer: &dyn Observer,
    ) -> Result<Vec<ContentBlock>> {
        let mut results = Vec::new();

        // NOTE: parallel_safe 도구들은 join_all 로 병렬 실행하도록 확장할 수 있다(현재 순차).
        for (id, name, input) in assistant.tool_uses() {
            // 협조적 취소: 도구 사이마다 체크포인트.
            if self.tool_ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }

            let Some(tool) = self.tools.get(name) else {
                results.push(ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: format!("unknown tool: {name}"),
                    is_error: true,
                });
                continue;
            };

            // 권한 결정. 도구가 선언한 기준 권한을 게이트가 최종 확정한다.
            //  - Allow(읽기 전용 등 부작용 없음) → 게이트에 묻지 않고 허용.
            //  - Deny(예: workdir 밖 경로) → 즉시 거부.
            //  - Ask(되돌리기 어려운 동작) → 게이트(정적 정책 + 대화형)가 최종 결정.
            let level = match tool.permission(input) {
                PermissionLevel::Allow => PermissionLevel::Allow,
                PermissionLevel::Deny => PermissionLevel::Deny,
                PermissionLevel::Ask => {
                    observer
                        .on_event(&AgentEvent::PermissionAsked {
                            name: name.to_string(),
                        })
                        .await;
                    self.permissions.decide(name, input).await
                }
            };
            // fail-closed: 명시적 Allow 만 실행한다. 게이트가 끝내 Ask 를 돌려주면
            // (대화형 게이트가 없어 동의를 못 받은 상태) 실행하지 않고 거부한다 —
            // write/bash 같은 Ask 도구가 모달 없이 무단 실행되는 것을 막는다.
            if level != PermissionLevel::Allow {
                return Err(Error::PermissionDenied(name.to_string()));
            }

            observer
                .on_event(&AgentEvent::ToolStart {
                    name: name.to_string(),
                })
                .await;
            let output = tool.invoke(input.clone(), &self.tool_ctx).await;
            observer
                .on_event(&AgentEvent::ToolEnd {
                    name: name.to_string(),
                    is_error: output.is_error,
                })
                .await;
            results.push(ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: output.content,
                is_error: output.is_error,
            });
        }

        Ok(results)
    }
}

/// 스트림 이벤트를 받아 하나의 assistant 메시지(콘텐츠 블록 리스트)로 누적한다.
///
/// **순수 변환**(부작용 없음): SSE 바이트 수신은 [`Provider`] 어댑터가, 이벤트 → 메시지
/// 집계는 여기서 한다 — functional core / imperative shell(CODING_RULES §4.1).
///
/// 블록 경계: 새 블록이 시작되면(예: 텍스트 → tool_use) 직전 블록을 **자동으로 닫는다**.
/// 따라서 어댑터가 [`StreamEvent::ContentBlockStop`] 을 매 블록마다 보내지 않아도 순서가
/// 보존된다(OpenAI 는 명시적 블록 종료 이벤트가 없다 — Anthropic 만 보낸다). 마지막으로
/// 열린 블록은 [`Self::finish`] 가 닫는다.
#[derive(Debug, Default)]
struct MessageAssembler {
    blocks: Vec<ContentBlock>,
    /// 아직 증분 누적 중인(닫히지 않은) 블록.
    open: Option<OpenBlock>,
}

/// 누적 중인 콘텐츠 블록의 가변 버퍼. 닫힐 때 [`ContentBlock`] 으로 확정된다.
#[derive(Debug)]
enum OpenBlock {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
}

impl MessageAssembler {
    fn apply(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::TextDelta(t) => match &mut self.open {
                Some(OpenBlock::Text(buf)) => buf.push_str(&t),
                _ => {
                    self.close_open();
                    self.open = Some(OpenBlock::Text(t));
                }
            },
            StreamEvent::ThinkingDelta(t) => match &mut self.open {
                Some(OpenBlock::Thinking { text, .. }) => text.push_str(&t),
                _ => {
                    self.close_open();
                    self.open = Some(OpenBlock::Thinking {
                        text: t,
                        signature: None,
                    });
                }
            },
            StreamEvent::ToolUseStart { id, name } => {
                self.close_open();
                self.open = Some(OpenBlock::ToolUse {
                    id,
                    name,
                    json: String::new(),
                });
            }
            StreamEvent::ToolUseInputDelta { partial_json, .. } => {
                if let Some(OpenBlock::ToolUse { json, .. }) = &mut self.open {
                    json.push_str(&partial_json);
                }
                // open 이 tool_use 가 아니면 무시한다(정상 흐름에선 발생하지 않음).
            }
            StreamEvent::ContentBlockStop => self.close_open(),
            // MessageStart/MessageStop 은 루프가 직접 처리한다(여기선 무시).
            _ => {}
        }
    }

    /// 현재 열린 블록을 완성된 [`ContentBlock`] 으로 확정해 push 한다(빈 텍스트는 버림).
    fn close_open(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        let block = match open {
            OpenBlock::Text(text) => {
                if text.is_empty() {
                    return;
                }
                ContentBlock::Text { text }
            }
            OpenBlock::Thinking { text, signature } => {
                if text.is_empty() {
                    return;
                }
                ContentBlock::Thinking { text, signature }
            }
            OpenBlock::ToolUse { id, name, json } => {
                let input = parse_tool_input(&name, &json);
                ContentBlock::ToolUse { id, name, input }
            }
        };
        self.blocks.push(block);
    }

    fn finish(mut self) -> Vec<ContentBlock> {
        self.close_open();
        self.blocks
    }
}

/// tool_use 의 누적 입력(부분 JSON 문자열)을 [`serde_json::Value`] 로 파싱한다.
///
/// 빈 입력은 인자 없는 호출이므로 빈 객체로 본다. 파싱 실패(모델이 잘못된 JSON 을 낸
/// 경우)는 빈 객체로 대체하고 경고만 남긴다 — 도구가 필수 인자 부재로 에러를 돌려주면
/// 모델이 복구할 수 있다. (CODING_RULES §9: tool_use 입력은 문자열 매칭 말고 JSON 파싱.)
fn parse_tool_input(name: &str, json: &str) -> serde_json::Value {
    if json.trim().is_empty() {
        return serde_json::Value::Object(Default::default());
    }
    match serde_json::from_str(json) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(tool = %name, %error, "tool_use input is not valid JSON; using empty object");
            serde_json::Value::Object(Default::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apply_all(events: Vec<StreamEvent>) -> Vec<ContentBlock> {
        let mut asm = MessageAssembler::default();
        for ev in events {
            asm.apply(ev);
        }
        asm.finish()
    }

    #[test]
    fn assembles_streamed_text_into_one_block() {
        let blocks = apply_all(vec![
            StreamEvent::TextDelta("Hello, ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::ContentBlockStop,
        ]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Hello, world"));
    }

    #[test]
    fn aggregates_tool_use_from_split_json_deltas() {
        // 텍스트 한 블록 + tool_use 한 블록. 입력 JSON 은 여러 delta 로 쪼개져 온다.
        let blocks = apply_all(vec![
            StreamEvent::TextDelta("reading".into()),
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "call_1".into(),
                partial_json: "{\"path\":".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "call_1".into(),
                partial_json: "\"a.rs\"}".into(),
            },
            StreamEvent::ContentBlockStop,
        ]);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "reading"));
        match &blocks[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "read");
                assert_eq!(input, &json!({ "path": "a.rs" }));
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_start_auto_closes_previous_block_without_explicit_stop() {
        // ContentBlockStop 없이 텍스트 → tool_use → tool_use 로 전환해도 순서가 보존된다.
        let blocks = apply_all(vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::ToolUseStart {
                id: "c1".into(),
                name: "glob".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c1".into(),
                partial_json: "{}".into(),
            },
            StreamEvent::ToolUseStart {
                id: "c2".into(),
                name: "grep".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c2".into(),
                partial_json: "{\"q\":\"x\"}".into(),
            },
        ]);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], ContentBlock::Text { .. }));
        assert!(
            matches!(&blocks[1], ContentBlock::ToolUse { name, input, .. }
            if name == "glob" && input == &json!({}))
        );
        assert!(
            matches!(&blocks[2], ContentBlock::ToolUse { name, input, .. }
            if name == "grep" && input == &json!({ "q": "x" }))
        );
    }

    #[test]
    fn empty_tool_arguments_become_empty_object() {
        let blocks = apply_all(vec![StreamEvent::ToolUseStart {
            id: "c1".into(),
            name: "list".into(),
        }]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolUse { input, .. } if input == &json!({})));
    }
}
