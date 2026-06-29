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
use crate::tool::{PermissionGate, PermissionLevel, Tool, ToolContext, ToolRegistry};
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
        // compaction 트리거 신호: 직전 응답의 입력 토큰 수(MessageStop usage). 첫 턴은 0.
        let mut last_input_tokens: u64 = 0;

        for iteration in 0..self.max_tool_iterations {
            // 협조적 취소 ①: 이터레이션 진입부 체크포인트.
            if cancel.is_cancelled() {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

            // 1. 컨텍스트 준비(임계 초과 시 compaction — 직전 입력 토큰을 신호로 넘긴다).
            let messages = self
                .context
                .prepare(session.messages.clone(), last_input_tokens)
                .await?;

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
                            if let StreamEvent::MessageStop { stop_reason: sr, usage } = &event {
                                stop_reason = *sr;
                                // 다음 이터레이션의 compaction 트리거 신호로 쓴다.
                                last_input_tokens = usage.input_tokens;
                            }
                            observer.on_event(&AgentEvent::Stream(event.clone())).await;
                            assembler.apply(event);
                        }
                    }
                }
            };

            // 중단 시에도 모은 부분 텍스트를 세션에 보존한다(빈 메시지는 넣지 않음).
            let mut assistant = Message::assistant(assembler.finish());
            let has_tool_use = assistant.tool_uses().next().is_some();
            if !interrupted && stop_reason == StopReason::ToolUse && !has_tool_use {
                // 일부 OpenAI-호환 백엔드는 structured tool_use 없이 finish_reason 만
                // tool_calls 로 보내고 reasoning 만 흘린다. 실행할 도구가 없으면 최종 응답으로
                // 처리해 thinking-only fallback 이 사용자에게 보이게 한다.
                tracing::warn!(
                    "stop_reason=ToolUse 인데 tool_use 블록이 없어 end_turn 으로 처리한다"
                );
                stop_reason = StopReason::EndTurn;
            }
            if !assistant.content.is_empty() {
                if !interrupted {
                    promote_final_thinking_to_text(&mut assistant.content, stop_reason);
                }
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
    ///
    /// **권한은 순차로** 해소한다(대화형 `Ask` 프롬프트는 한 번에 하나, 거부 시 턴 중단).
    /// 그 다음 **`parallel_safe` 도구(read/glob/grep 등 부작용 없음)는 `join_all` 로 동시
    /// 실행**하고, 비-parallel 도구(write/edit/bash)는 순차로 실행한다. tool_result 는
    /// tool_use_id 로 매칭되지만, 결정성을 위해 **원래 tool_use 순서**로 모은다.
    async fn execute_tool_calls(
        &self,
        assistant: &Message,
        observer: &dyn Observer,
    ) -> Result<Vec<ContentBlock>> {
        let uses: Vec<(String, String, serde_json::Value)> = assistant
            .tool_uses()
            .map(|(id, name, input)| (id.to_string(), name.to_string(), input.clone()))
            .collect();

        // 1단계: 권한을 순차 해소해 실행 계획을 만든다.
        let mut plans: Vec<ToolPlan> = Vec::with_capacity(uses.len());
        for (_id, name, input) in &uses {
            // 협조적 취소: 권한 해소 사이 체크포인트.
            if self.tool_ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Some(tool) = self.tools.get(name) else {
                plans.push(ToolPlan::Unknown);
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
                        .on_event(&AgentEvent::PermissionAsked { name: name.clone() })
                        .await;
                    self.permissions.decide(name, input).await
                }
            };
            // fail-closed: 명시적 Allow 만 실행한다. 게이트가 끝내 Ask 를 돌려주면
            // (대화형 동의를 못 받은 상태) 실행하지 않고 거부한다 — write/bash 같은 Ask
            // 도구가 모달 없이 무단 실행되는 것을 막는다.
            if level != PermissionLevel::Allow {
                return Err(Error::PermissionDenied(name.clone()));
            }
            let parallel = tool.parallel_safe();
            plans.push(ToolPlan::Run {
                tool: tool.clone(),
                parallel,
            });
        }

        // 2단계: 실행. 결과는 인덱스로 채워 원래 순서를 보존한다.
        let mut results: Vec<Option<ContentBlock>> = (0..uses.len()).map(|_| None).collect();

        // 2a. parallel_safe 그룹을 동시에 실행(join_all). 부작용이 없어 순서 무관·경쟁 안전.
        if self.tool_ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let parallel_futures: Vec<_> = plans
            .iter()
            .enumerate()
            .filter_map(|(i, plan)| match plan {
                ToolPlan::Run {
                    tool,
                    parallel: true,
                } => {
                    let tool = tool.clone();
                    let (id, name, input) = uses[i].clone();
                    Some(async move {
                        observer
                            .on_event(&AgentEvent::ToolStart { name: name.clone() })
                            .await;
                        let output = tool.invoke(input, &self.tool_ctx).await;
                        let content = output.content;
                        let is_error = output.is_error;
                        observer
                            .on_event(&AgentEvent::ToolEnd {
                                name,
                                content: content.clone(),
                                is_error,
                            })
                            .await;
                        (
                            i,
                            ContentBlock::ToolResult {
                                tool_use_id: id,
                                content,
                                is_error,
                            },
                        )
                    })
                }
                _ => None,
            })
            .collect();
        for (i, block) in futures::future::join_all(parallel_futures).await {
            results[i] = Some(block);
        }

        // 2b. 나머지(비-parallel 실행 + unknown 도구)는 순차로.
        for (i, plan) in plans.iter().enumerate() {
            if results[i].is_some() {
                continue;
            }
            let (id, name, input) = &uses[i];
            let block = match plan {
                ToolPlan::Unknown => {
                    let content = format!("unknown tool: {name}");
                    observer
                        .on_event(&AgentEvent::ToolEnd {
                            name: name.clone(),
                            content: content.clone(),
                            is_error: true,
                        })
                        .await;
                    ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content,
                        is_error: true,
                    }
                }
                ToolPlan::Run { tool, .. } => {
                    // 협조적 취소: 비가역 도구 실행 사이 체크포인트.
                    if self.tool_ctx.cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    observer
                        .on_event(&AgentEvent::ToolStart { name: name.clone() })
                        .await;
                    let output = tool.invoke(input.clone(), &self.tool_ctx).await;
                    let content = output.content;
                    let is_error = output.is_error;
                    observer
                        .on_event(&AgentEvent::ToolEnd {
                            name: name.clone(),
                            content: content.clone(),
                            is_error,
                        })
                        .await;
                    ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content,
                        is_error,
                    }
                }
            };
            results[i] = Some(block);
        }

        Ok(results.into_iter().map(|b| b.expect("filled")).collect())
    }
}

/// 한 tool_use 의 실행 계획(권한 해소 후). `execute_tool_calls` 의 1→2단계 사이 매개.
enum ToolPlan {
    /// 알 수 없는 도구 → tool_result 에 에러로 돌려준다(모델이 복구 시도 가능).
    Unknown,
    /// 실행 허가됨. `parallel` 이면 동시 실행 그룹에 들어간다.
    Run { tool: Arc<dyn Tool>, parallel: bool },
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

/// 일부 OpenAI-호환 백엔드(Ollama 등)는 도구 결과를 받은 뒤 최종 답을 `content`
/// 대신 `reasoning` 으로만 흘리고 `EndTurn` 으로 끝낸다. 중간 tool-use 사고는 그대로
/// 숨기되, 최종 응답이 thinking-only 이면 세션/컨텍스트에 사용자-visible text 로도 남긴다.
fn promote_final_thinking_to_text(content: &mut Vec<ContentBlock>, stop_reason: StopReason) {
    if stop_reason != StopReason::EndTurn {
        return;
    }
    if content.iter().any(|block| match block {
        ContentBlock::Text { text } => !text.trim().is_empty(),
        ContentBlock::ToolUse { .. } => true,
        _ => false,
    }) {
        return;
    }
    let Some(text) = content.iter().rev().find_map(|block| match block {
        ContentBlock::Thinking { text, .. } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    }) else {
        return;
    };
    content.push(ContentBlock::Text { text });
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

    #[test]
    fn assembles_thinking_deltas_into_one_block() {
        let blocks = apply_all(vec![
            StreamEvent::ThinkingDelta("let me ".into()),
            StreamEvent::ThinkingDelta("think".into()),
            StreamEvent::ContentBlockStop,
        ]);
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(&blocks[0], ContentBlock::Thinking { text, .. } if text == "let me think")
        );
    }

    #[test]
    fn empty_text_and_thinking_blocks_are_dropped() {
        // 빈 본문으로 열렸다 닫힌 블록은 버려진다(content 에 빈 블록을 넣지 않음).
        let blocks = apply_all(vec![
            StreamEvent::TextDelta(String::new()),
            StreamEvent::ContentBlockStop,
            StreamEvent::ThinkingDelta(String::new()),
            StreamEvent::ContentBlockStop,
        ]);
        assert!(blocks.is_empty(), "got: {blocks:?}");
    }

    #[test]
    fn invalid_tool_input_json_falls_back_to_empty_object() {
        let blocks = apply_all(vec![
            StreamEvent::ToolUseStart {
                id: "c1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c1".into(),
                partial_json: "{not valid json".into(),
            },
            StreamEvent::ContentBlockStop,
        ]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolUse { input, .. } if input == &json!({})));
    }
}

#[cfg(test)]
mod exec_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::context::NoopContextManager;
    use crate::provider::{CompletionRequest, EventStream, ModelInfo, Provider, ToolSchema};
    use crate::tool::{
        CancellationToken, PermissionGate, PermissionLevel, Tool, ToolContext, ToolOutput,
        ToolRegistry,
    };

    /// stream/count_tokens 는 execute_tool_calls 테스트에서 호출되지 않는다.
    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            unreachable!("stream not used in execute_tool_calls tests")
        }
        async fn count_tokens(
            &self,
            _system: Option<&str>,
            _messages: &[Message],
            _tools: &[ToolSchema],
        ) -> Result<u64> {
            Ok(0)
        }
    }

    struct AllowGate;
    #[async_trait]
    impl PermissionGate for AllowGate {
        async fn decide(&self, _tool: &str, _input: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
    }

    /// Allow + 이름만 echo. `parallel` 로 parallel_safe 여부를 정한다.
    struct EchoTool {
        name: String,
        parallel: bool,
    }
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({ "type": "object" })
        }
        fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
        fn parallel_safe(&self) -> bool {
            self.parallel
        }
        async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            ToolOutput::ok(self.name.clone())
        }
    }

    /// parallel_safe 도구. 두 개가 **동시에** 실행돼야만 barrier 를 통과한다 — 순차면 데드락.
    struct BarrierTool {
        name: String,
        barrier: Arc<tokio::sync::Barrier>,
    }
    #[async_trait]
    impl Tool for BarrierTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "barrier"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({ "type": "object" })
        }
        fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
        fn parallel_safe(&self) -> bool {
            true
        }
        async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            self.barrier.wait().await;
            ToolOutput::ok(self.name.clone())
        }
    }

    fn agent_with(tools: ToolRegistry) -> Agent {
        Agent {
            provider: Arc::new(StubProvider),
            tools,
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "m".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 5,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        }
    }

    fn tool_use(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: json!({}),
        }
    }

    #[tokio::test]
    async fn parallel_safe_tools_run_concurrently() {
        // barrier(2): 두 도구가 동시에 실행돼야 둘 다 진행한다. 순차 실행이면 데드락 →
        // timeout 으로 잡아 실패시킨다(= 병렬 실행을 증명).
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(BarrierTool {
            name: "a".into(),
            barrier: barrier.clone(),
        }));
        reg.register(Arc::new(BarrierTool {
            name: "b".into(),
            barrier: barrier.clone(),
        }));
        let agent = agent_with(reg);
        let assistant = Message::assistant(vec![tool_use("1", "a"), tool_use("2", "b")]);

        let results = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            agent.execute_tool_calls(&assistant, &NullObserver),
        )
        .await
        .expect("must not deadlock — proves concurrent execution")
        .expect("ok");
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn results_keep_tool_use_order_across_mixed_and_unknown() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool {
            name: "x".into(),
            parallel: true,
        }));
        reg.register(Arc::new(EchoTool {
            name: "s".into(),
            parallel: false,
        }));
        reg.register(Arc::new(EchoTool {
            name: "y".into(),
            parallel: true,
        }));
        let agent = agent_with(reg);
        // 순서: parallel, sequential, unknown, parallel.
        let assistant = Message::assistant(vec![
            tool_use("1", "x"),
            tool_use("2", "s"),
            tool_use("3", "nope"),
            tool_use("4", "y"),
        ]);

        let results = agent
            .execute_tool_calls(&assistant, &NullObserver)
            .await
            .expect("ok");
        let ids: Vec<&str> = results
            .iter()
            .map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id.as_str(),
                _ => panic!("expected tool_result"),
            })
            .collect();
        // 원래 tool_use 순서가 그대로 보존된다.
        assert_eq!(ids, ["1", "2", "3", "4"]);
        // 미지의 도구(id 3)는 에러 결과.
        assert!(matches!(
            &results[2],
            ContentBlock::ToolResult { is_error: true, content, .. } if content.contains("unknown tool: nope")
        ));
    }

    #[tokio::test]
    async fn denied_ask_tool_aborts_turn() {
        struct DenyGate;
        #[async_trait]
        impl PermissionGate for DenyGate {
            async fn decide(&self, _t: &str, _i: &serde_json::Value) -> PermissionLevel {
                PermissionLevel::Deny
            }
        }
        struct AskTool;
        #[async_trait]
        impl Tool for AskTool {
            fn name(&self) -> &str {
                "danger"
            }
            fn description(&self) -> &str {
                "ask"
            }
            fn input_schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn permission(&self, _i: &serde_json::Value) -> PermissionLevel {
                PermissionLevel::Ask
            }
            async fn invoke(&self, _i: serde_json::Value, _c: &ToolContext) -> ToolOutput {
                ToolOutput::ok("ran")
            }
        }
        // AskTool 의 메타데이터 접근자도 계약대로 동작한다.
        assert_eq!(AskTool.description(), "ask");
        assert_eq!(AskTool.input_schema()["type"], "object");
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(AskTool));
        let mut agent = agent_with(reg);
        agent.permissions = Arc::new(DenyGate);
        let assistant = Message::assistant(vec![tool_use("1", "danger")]);
        let err = agent
            .execute_tool_calls(&assistant, &NullObserver)
            .await
            .expect_err("deny should abort the turn");
        assert!(matches!(err, Error::PermissionDenied(t) if t == "danger"));
    }

    /// 고정 이벤트 시퀀스를 스트리밍하는 프로바이더.
    struct ScriptProvider {
        events: Vec<StreamEvent>,
    }
    #[async_trait]
    impl Provider for ScriptProvider {
        fn id(&self) -> &str {
            "script"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            let evs: Vec<Result<StreamEvent>> = self.events.iter().cloned().map(Ok).collect();
            Ok(Box::pin(futures::stream::iter(evs)))
        }
        async fn count_tokens(
            &self,
            _s: Option<&str>,
            _m: &[Message],
            _t: &[ToolSchema],
        ) -> Result<u64> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn tool_use_stop_without_blocks_ends_turn_not_loop() {
        // stop_reason=ToolUse 인데 tool_use 블록이 없는 비정상 응답 → 빈 결과 무한루프
        // (MaxIterations)로 가지 않고 턴이 정상 종료(Ok)되어야 한다.
        let provider = Arc::new(ScriptProvider {
            events: vec![
                StreamEvent::MessageStart { model: "m".into() },
                StreamEvent::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: crate::message::Usage::default(),
                },
            ],
        });
        let agent = Agent {
            provider,
            tools: ToolRegistry::new(),
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "m".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 5,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        };
        let mut session = Session::new();
        let r = agent
            .run_turn(&mut session, "hi".into(), &NullObserver)
            .await;
        assert!(r.is_ok(), "expected Ok (turn ended), got {r:?}");
    }

    #[tokio::test]
    async fn tool_use_stop_without_blocks_promotes_final_thinking_to_text() {
        // 호환 백엔드가 reasoning 만 흘리고 finish_reason 만 tool_calls 로 잘못 끝내도,
        // 실행할 structured tool_use 가 없으면 최종 답변으로 보존한다.
        let provider = Arc::new(ScriptProvider {
            events: vec![
                StreamEvent::ThinkingDelta("try python3 next".into()),
                StreamEvent::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: crate::message::Usage::default(),
                },
            ],
        });
        let agent = Agent {
            provider,
            tools: ToolRegistry::new(),
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "m".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 5,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        };
        let mut session = Session::new();
        agent
            .run_turn(&mut session, "hi".into(), &NullObserver)
            .await
            .expect("turn should end");
        assert_eq!(session.messages.len(), 2);
        let final_assistant = &session.messages[1];
        assert!(final_assistant
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Thinking { text, .. }
                if text == "try python3 next")));
        assert!(final_assistant
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text }
                if text == "try python3 next")));
    }

    /// 매 호출마다 미리 정해둔 이벤트 시퀀스를 순서대로 흘리는 프로바이더(다중 이터레이션용).
    struct SeqProvider {
        scripts: std::sync::Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
    }
    #[async_trait]
    impl Provider for SeqProvider {
        fn id(&self) -> &str {
            "seq"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            let evs = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
            let evs: Vec<Result<StreamEvent>> = evs.into_iter().map(Ok).collect();
            Ok(Box::pin(futures::stream::iter(evs)))
        }
        async fn count_tokens(
            &self,
            _s: Option<&str>,
            _m: &[Message],
            _t: &[ToolSchema],
        ) -> Result<u64> {
            Ok(0)
        }
    }

    fn agent_with_provider(provider: Arc<dyn Provider>, tools: ToolRegistry) -> Agent {
        Agent {
            provider,
            tools,
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "m".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 3,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        }
    }

    fn stop(reason: StopReason) -> StreamEvent {
        StreamEvent::MessageStop {
            stop_reason: reason,
            usage: crate::message::Usage::default(),
        }
    }

    #[tokio::test]
    async fn run_turn_plain_text_ends_turn() {
        let provider = Arc::new(ScriptProvider {
            events: vec![
                StreamEvent::MessageStart { model: "m".into() },
                StreamEvent::TextDelta("hello".into()),
                stop(StopReason::EndTurn),
            ],
        });
        let agent = agent_with_provider(provider, ToolRegistry::new());
        let mut session = Session::new();
        agent
            .run_turn(&mut session, "hi".into(), &NullObserver)
            .await
            .expect("ok");
        // user + assistant(text) 가 세션에 남는다.
        assert_eq!(session.messages.len(), 2);
        assert!(matches!(
            &session.messages[1].content[0],
            ContentBlock::Text { text } if text == "hello"
        ));
    }

    #[tokio::test]
    async fn run_turn_executes_tool_then_completes() {
        let provider = Arc::new(SeqProvider {
            scripts: std::sync::Mutex::new(std::collections::VecDeque::from(vec![
                // 이터레이션 0: tool_use 요청.
                vec![
                    StreamEvent::MessageStart { model: "m".into() },
                    StreamEvent::ToolUseStart {
                        id: "c1".into(),
                        name: "echo".into(),
                    },
                    StreamEvent::ToolUseInputDelta {
                        id: "c1".into(),
                        partial_json: "{}".into(),
                    },
                    stop(StopReason::ToolUse),
                ],
                // 이터레이션 1: 도구 결과를 본 뒤 텍스트로 마무리.
                vec![
                    StreamEvent::TextDelta("done".into()),
                    stop(StopReason::EndTurn),
                ],
            ])),
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool {
            name: "echo".into(),
            parallel: true,
        }));
        let agent = agent_with_provider(provider, reg);
        let mut session = Session::new();
        agent
            .run_turn(&mut session, "go".into(), &NullObserver)
            .await
            .expect("ok");
        // user, assistant(tool_use), user(tool_result), assistant(text)
        assert_eq!(session.messages.len(), 4);
        assert!(matches!(
            &session.messages[2].content[0],
            ContentBlock::ToolResult { content, .. } if content == "echo"
        ));
    }

    #[tokio::test]
    async fn run_turn_reaches_max_iterations() {
        // 매번 tool_use 만 요청 → 도구는 돌지만 끝나지 않아 상한에서 MaxIterations.
        let provider = Arc::new(ScriptProvider {
            events: vec![
                StreamEvent::ToolUseStart {
                    id: "c1".into(),
                    name: "echo".into(),
                },
                StreamEvent::ToolUseInputDelta {
                    id: "c1".into(),
                    partial_json: "{}".into(),
                },
                stop(StopReason::ToolUse),
            ],
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool {
            name: "echo".into(),
            parallel: false,
        }));
        let mut agent = agent_with_provider(provider, reg);
        agent.max_tool_iterations = 2;
        let mut session = Session::new();
        let err = agent
            .run_turn(&mut session, "go".into(), &NullObserver)
            .await
            .expect_err("should hit the iteration cap");
        assert!(matches!(err, Error::MaxIterations(2)), "got {err:?}");
    }

    #[tokio::test]
    async fn run_turn_cancelled_at_entry_returns_cancelled() {
        let agent = agent_with(ToolRegistry::new());
        agent.tool_ctx.cancel.cancel(); // 진입 전 취소.
        let mut session = Session::new();
        let err = agent
            .run_turn(&mut session, "hi".into(), &NullObserver)
            .await
            .expect_err("cancelled");
        assert!(matches!(err, Error::Cancelled));
    }

    /// 스트림의 첫 이벤트를 내보내며 토큰을 취소해, 스트림 소비 도중 중단을 재현한다.
    struct CancelMidStreamProvider {
        cancel: CancellationToken,
    }
    #[async_trait]
    impl Provider for CancelMidStreamProvider {
        fn id(&self) -> &str {
            "cancel-mid"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            let cancel = self.cancel.clone();
            let s = futures::stream::once(async move {
                cancel.cancel();
                Ok(StreamEvent::TextDelta("partial".into()))
            });
            Ok(Box::pin(s))
        }
        async fn count_tokens(
            &self,
            _s: Option<&str>,
            _m: &[Message],
            _t: &[ToolSchema],
        ) -> Result<u64> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn run_turn_interrupted_mid_stream_preserves_partial() {
        let token = CancellationToken::new();
        let provider = Arc::new(CancelMidStreamProvider {
            cancel: token.clone(),
        });
        let mut agent = agent_with_provider(provider, ToolRegistry::new());
        agent.tool_ctx.cancel = token;
        let mut session = Session::new();
        let err = agent
            .run_turn(&mut session, "hi".into(), &NullObserver)
            .await
            .expect_err("interrupted");
        assert!(matches!(err, Error::Cancelled));
        // 중단돼도 모은 부분 텍스트는 보존된다(user + assistant("partial")).
        assert_eq!(session.messages.len(), 2);
        assert!(matches!(
            &session.messages[1].content[0],
            ContentBlock::Text { text } if text == "partial"
        ));
    }

    #[tokio::test]
    async fn execute_tool_calls_cancelled_before_running() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool {
            name: "echo".into(),
            parallel: true,
        }));
        let agent = agent_with(reg);
        agent.tool_ctx.cancel.cancel();
        let assistant = Message::assistant(vec![tool_use("1", "echo")]);
        let err = agent
            .execute_tool_calls(&assistant, &NullObserver)
            .await
            .expect_err("cancelled");
        assert!(matches!(err, Error::Cancelled));
    }

    #[tokio::test]
    async fn tool_declaring_deny_aborts_turn() {
        struct DenyTool;
        #[async_trait]
        impl Tool for DenyTool {
            fn name(&self) -> &str {
                "forbidden"
            }
            fn description(&self) -> &str {
                "deny"
            }
            fn input_schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn permission(&self, _i: &serde_json::Value) -> PermissionLevel {
                PermissionLevel::Deny
            }
            async fn invoke(&self, _i: serde_json::Value, _c: &ToolContext) -> ToolOutput {
                ToolOutput::ok("should not run")
            }
        }
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DenyTool));
        let agent = agent_with(reg);
        let assistant = Message::assistant(vec![tool_use("1", "forbidden")]);
        let err = agent
            .execute_tool_calls(&assistant, &NullObserver)
            .await
            .expect_err("deny aborts");
        assert!(matches!(err, Error::PermissionDenied(t) if t == "forbidden"));
    }

    #[test]
    fn agent_debug_renders_scalar_fields() {
        let agent = agent_with(ToolRegistry::new());
        let s = format!("{agent:?}");
        assert!(s.contains("Agent"));
        assert!(s.contains("stub")); // provider.id()
        assert!(s.contains("max_tokens"));
    }

    #[tokio::test]
    async fn test_doubles_expose_declared_surface() {
        // 테스트 더블의 trait 표면(루프가 직접 호출하지 않는 접근자)도 계약대로 동작한다.
        assert_eq!(StubProvider.id(), "stub");
        assert!(StubProvider.models().is_empty());
        assert_eq!(StubProvider.count_tokens(None, &[], &[]).await.unwrap(), 0);

        let sp = ScriptProvider { events: vec![] };
        assert_eq!(sp.id(), "script");
        assert!(sp.models().is_empty());
        assert_eq!(sp.count_tokens(None, &[], &[]).await.unwrap(), 0);

        let echo = EchoTool {
            name: "e".into(),
            parallel: false,
        };
        assert_eq!(echo.description(), "echo");
        assert_eq!(echo.input_schema()["type"], "object");

        let barrier = BarrierTool {
            name: "b".into(),
            barrier: Arc::new(tokio::sync::Barrier::new(1)),
        };
        assert_eq!(barrier.description(), "barrier");
        assert_eq!(barrier.input_schema()["type"], "object");
    }
}
